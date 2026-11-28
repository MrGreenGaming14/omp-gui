#include "loa/harness/omp/oneshot.hpp"

#include <asio/experimental/awaitable_operators.hpp>
#include <asio/steady_timer.hpp>
#include <asio/this_coro.hpp>
#include <asio/use_awaitable.hpp>

#include <variant>

#include "loa/harness/account_support/process.hpp"
#include "loa/runtime/exec.hpp"
#include "loa/runtime/tempfile.hpp"
#include "loa/runtime/text.hpp"

namespace loa::harness::omp {

namespace {

using Answer = std::expected<std::string, std::string>;

std::string_view trim(std::string_view s) {
    while (!s.empty() && std::string_view(" \t\r\n").contains(s.front())) s.remove_prefix(1);
    while (!s.empty() && std::string_view(" \t\r\n").contains(s.back())) s.remove_suffix(1);
    return s;
}

// Strict UTF-8 check (no overlongs, surrogates or code points above U+10FFFF).
bool valid_utf8(std::string_view s) {
    std::size_t i = 0;
    while (i < s.size()) {
        const auto c = static_cast<unsigned char>(s[i]);
        std::size_t need = 0;
        unsigned char lo = 0x80, hi = 0xBF;
        if (c < 0x80) need = 0;
        else if (c >= 0xC2 && c <= 0xDF) need = 1;
        else if (c >= 0xE0 && c <= 0xEF) { need = 2; if (c == 0xE0) lo = 0xA0; if (c == 0xED) hi = 0x9F; }
        else if (c >= 0xF0 && c <= 0xF4) { need = 3; if (c == 0xF0) lo = 0x90; if (c == 0xF4) hi = 0x8F; }
        else return false;
        if (i + need >= s.size() && need > 0) return false;
        for (std::size_t k = 1; k <= need; ++k) {
            const auto b = static_cast<unsigned char>(s[i + k]);
            if (k == 1 ? (b < lo || b > hi) : (b & 0xC0) != 0x80) return false;
        }
        i += need + 1;
    }
    return true;
}

// "exit status: N" or "signal", as Rust's `ExitStatus` display.
std::string status_text(const std::optional<int>& code) {
    return code ? "exit status: " + std::to_string(*code) : std::string("signal");
}

struct Ran {
    std::optional<int> code;
    std::string out, err;
    std::expected<void, std::string> fed;
};

} // namespace

std::string_view OneShot::pick(std::optional<std::string_view> model) const {
    if (model) {
        auto picked = trim(*model);
        if (!picked.empty()) return picked;
    }
    return model_;
}

std::vector<std::string> OneShot::args(std::string_view model, const std::filesystem::path& overlay) const {
    return {"-p", "--mode", "text", "--no-session", "--no-tools", "--no-title", "--no-context-files", "--no-rules",
            "--no-skills", "--no-extensions", "--no-lsp", "--model", std::string(model), "--config", overlay.string(),
            "--cwd", cwd_.string()};
}

loa::contracts::Awaitable<Answer> OneShot::ask(std::string model, std::string prompt) const {
    using namespace asio::experimental::awaitable_operators;
    auto overlay = loa::runtime::TempFile::create();
    if (!overlay) co_return std::unexpected(overlay.error().message());
    if (auto written = overlay->write_all(MEMORY_OFF); !written) co_return std::unexpected(written.error().message());
    const auto shown = executable_.string();
    loa::contracts::CommandSpec spec{loa::runtime::exec::resolve(executable_), args(model, overlay->path()), {}, cwd_};
    account_support::AsioProcessRunner runner;
    auto spawned = co_await runner.spawn(std::move(spec));
    if (!spawned) co_return std::unexpected("can't run " + shown + ": " + spawned.error().message);
    std::shared_ptr<account_support::RunningProcess> child = std::move(*spawned);

    auto run = [](std::shared_ptr<account_support::RunningProcess> child, std::string prompt) -> loa::contracts::Awaitable<Ran> {
        auto in = child->take_stdin();
        auto out = child->take_stdout();
        auto err = child->take_stderr();
        auto feed = [](std::unique_ptr<account_support::ProcessWriter> in, std::string prompt)
            -> loa::contracts::Awaitable<std::expected<void, std::string>> {
            auto written = co_await in->write_all(std::move(prompt));
            if (!written) co_return std::unexpected(written.error().message());
            co_return std::expected<void, std::string>{};
        };
        auto [fed, stdout_text, stderr_text] =
            co_await (feed(std::move(in), std::move(prompt)) && out->read_to_end() && err->read_to_end());
        auto code = co_await child->wait();
        co_return Ran{code ? *code : std::nullopt, stdout_text.value_or(""), stderr_text.value_or(""), std::move(fed)};
    };
    asio::steady_timer timer(co_await asio::this_coro::executor, timeout_);
    auto raced = co_await (run(child, std::move(prompt)) || timer.async_wait(asio::use_awaitable));
    if (raced.index() == 1) {
        (void)co_await child->kill();
        co_return std::unexpected("omp gave no answer within " + std::to_string(timeout_.count()) + "ms");
    }
    auto& ran = std::get<0>(raced);
    if (ran.code != 0) {
        co_return std::unexpected("omp exited with " + status_text(ran.code) + ": " +
                                  std::string(loa::runtime::text::clip(trim(ran.err), 2000)));
    }
    if (!ran.fed) co_return std::unexpected("writing the prompt to omp: " + ran.fed.error());
    if (!valid_utf8(ran.out)) co_return std::unexpected(std::string("omp printed invalid UTF-8"));
    co_return std::move(ran.out);
}

loa::contracts::Awaitable<Answer> OneShot::summarize(std::string instructions, std::string input) const {
    co_return co_await ask(model_, instructions + input);
}

} // namespace loa::harness::omp
