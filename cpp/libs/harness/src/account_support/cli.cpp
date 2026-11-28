#include "loa/harness/account_support/cli.hpp"

#include <asio.hpp>
#include <asio/experimental/awaitable_operators.hpp>
#include <asio/experimental/channel.hpp>

#include <array>
#include <format>

#include "../kit/utf8.hpp"
#include "loa/contracts/login_channels.hpp"
#include "unicode.hpp"

namespace loa::harness::account_support {

namespace login = loa::contracts::login;
using loa::contracts::LoginChannels;
using loa::contracts::LoginUpdate;

namespace cli {

std::optional<std::string_view> first_url(std::string_view text) {
    auto start = text.find("https://");
    if (start == std::string_view::npos) start = text.find("http://");
    if (start == std::string_view::npos) return std::nullopt;
    const std::string_view rest = text.substr(start);
    std::size_t end = rest.size();
    for (std::size_t i = 0; i < rest.size();) {
        const std::size_t at = i;
        const char32_t c = detail::next_char(rest, i);
        if (detail::is_whitespace(c) || c == '"' || c == '\'' || c == '<' || c == '>' || c == ')' || c == ']') {
            end = at;
            break;
        }
    }
    std::string_view url = rest.substr(0, end);
    while (!url.empty() && (url.back() == '.' || url.back() == ',' || url.back() == ';')) url.remove_suffix(1);
    return url;
}

std::string clean(std::string_view text) {
    std::string out;
    out.reserve(text.size());
    std::size_t i = 0;
    while (i < text.size()) {
        const std::size_t at = i;
        const char32_t c = detail::next_char(text, i);
        if (c == 0x1B) {
            if (i >= text.size()) continue;
            if (text[i] == '[') {
                ++i;
                while (i < text.size()) {
                    const char32_t d = detail::next_char(text, i);
                    if ((d >= 'a' && d <= 'z') || (d >= 'A' && d <= 'Z') || d == '~') break;
                }
            } else if (text[i] == ']') {
                // Operating system command: ends at BEL or ESC \.
                ++i;
                while (i < text.size()) {
                    const char32_t d = detail::next_char(text, i);
                    if (d == 0x07) break;
                    if (d == 0x1B) {
                        if (i < text.size() && text[i] == '\\') ++i;
                        // Rust stops only at `ESC \`; a lone ESC keeps scanning.
                        else continue;
                        break;
                    }
                }
            } else {
                (void)detail::next_char(text, i);
            }
            continue;
        }
        if (c == '\r') continue;
        if (detail::is_control(c) && c != '\t' && c != '\n') continue;
        out.append(text.substr(at, i - at));
    }
    return std::string(detail::trim(out));
}

std::vector<Line> LineSplitter::push(std::string_view chunk) {
    partial_.append(chunk);
    std::vector<Line> out;
    for (auto end = partial_.find('\n'); end != std::string::npos; end = partial_.find('\n')) {
        std::string line = clean(std::string_view(partial_).substr(0, end + 1));
        partial_.erase(0, end + 1);
        if (!line.empty()) out.emplace_back(Text{std::move(line)});
    }
    std::string pending = clean(partial_);
    if (!pending.empty() && (pending.back() == ':' || pending.back() == '?' || pending.back() == '>')) {
        partial_.clear();
        out.emplace_back(Prompt{std::move(pending)});
    }
    return out;
}

std::vector<Line> LineSplitter::finish() {
    std::string rest = clean(partial_);
    partial_.clear();
    if (rest.empty()) return {};
    return {Text{std::move(rest)}};
}

} // namespace cli

Awaitable<void> CliLoginCommand::run(std::shared_ptr<LoginChannels> channels) {
    auto process = co_await runner_->spawn(command_);
    if (!process) {
        (void)co_await channels->updates.async_send(asio::error_code{}, LoginUpdate{login::Failed{process.error().to_string()}},
                                                    asio::as_tuple(asio::use_awaitable));
        co_return;
    }
    CliLogin login(std::move(*process));
    co_await login.run(std::move(channels));
}

namespace {

// One stdout/stderr chunk; `nullopt` marks the end of one stream.
struct Chunk {
    std::optional<std::string> text;
};
// What the login loop waits on: process output and the sign-in screen's controls, merged so
// no wake-up is lost between two pending receives.
using Event = std::variant<Chunk, loa::contracts::LoginControl>;
using EventChannel = asio::experimental::channel<void(asio::error_code, Event)>;

Awaitable<void> pump(std::unique_ptr<ProcessReader> reader, std::shared_ptr<EventChannel> out) {
    std::array<char, 4096> buf{};
    for (;;) {
        auto n = co_await reader->read_some(buf);
        if (!n || *n == 0) break;
        auto [ec] = co_await out->async_send(asio::error_code{}, Event{Chunk{kit::detail::utf8_lossy(std::string_view(buf.data(), *n))}},
                                             asio::as_tuple(asio::use_awaitable));
        if (ec) co_return;
    }
    (void)co_await out->async_send(asio::error_code{}, Event{Chunk{std::nullopt}}, asio::as_tuple(asio::use_awaitable));
}

// Forwards controls until the control channel closes, the merged channel closes, or `stop` fires.
Awaitable<void> forward(std::shared_ptr<LoginChannels> channels, std::shared_ptr<EventChannel> out,
                        std::shared_ptr<asio::cancellation_signal> stop) {
    for (;;) {
        auto [ec, control] = co_await channels->control.async_receive(
            asio::bind_cancellation_slot(stop->slot(), asio::as_tuple(asio::use_awaitable)));
        if (ec) co_return;
        auto [sent] = co_await out->async_send(asio::error_code{}, Event{std::move(control)}, asio::as_tuple(asio::use_awaitable));
        if (sent) co_return;
    }
}

struct Publisher {
    std::shared_ptr<LoginChannels> channels;
    bool url_sent = false;
    std::optional<std::string> last;

    Awaitable<bool> send(LoginUpdate update) {
        auto [ec] = co_await channels->updates.async_send(asio::error_code{}, std::move(update), asio::as_tuple(asio::use_awaitable));
        co_return !ec;
    }

    Awaitable<bool> publish(cli::Line line) {
        if (auto* text = std::get_if<cli::Text>(&line)) {
            auto url = cli::first_url(text->text);
            if (url && !url_sent) {
                url_sent = true;
                co_return co_await send(login::Url{std::string(*url), std::nullopt, std::nullopt});
            }
            last = text->text;
            co_return co_await send(login::Progress{std::move(text->text)});
        }
        co_return co_await send(login::Prompt{std::move(std::get<cli::Prompt>(line).message), std::nullopt});
    }
};

} // namespace

Awaitable<void> CliLogin::run(std::shared_ptr<LoginChannels> channels) {
    auto executor = co_await asio::this_coro::executor;
    auto stdin_pipe = process_->take_stdin();
    auto events = std::make_shared<EventChannel>(executor, 64);
    auto stop = std::make_shared<asio::cancellation_signal>();
    int streams = 0;
    std::array<std::unique_ptr<ProcessReader>, 2> readers{process_->take_stdout(), process_->take_stderr()};
    for (auto& reader : readers) {
        if (!reader) continue;
        ++streams;
        asio::co_spawn(executor, pump(std::move(reader), events), asio::detached);
    }
    asio::co_spawn(executor, forward(channels, events, stop), asio::detached);
    cli::LineSplitter lines;
    Publisher out{channels};
    // Stops the pumps and the control forwarder.
    auto detach = [&] {
        events->close();
        stop->emit(asio::cancellation_type::all);
    };
    // Ends the login early: also kills the process.
    auto abandon = [&] {
        detach();
        process_.reset();
    };
    while (streams > 0) {
        auto [ec, event] = co_await events->async_receive(asio::as_tuple(asio::use_awaitable));
        if (ec) break;
        if (auto* chunk = std::get_if<Chunk>(&event)) {
            if (!chunk->text) {
                --streams;
                continue;
            }
            for (auto& line : lines.push(*chunk->text)) {
                if (!co_await out.publish(std::move(line))) {
                    abandon();
                    co_return;
                }
            }
        } else if (auto* input = std::get_if<loa::contracts::SubmitInput>(&std::get<loa::contracts::LoginControl>(event))) {
            if (stdin_pipe) {
                std::string_view typed = input->text;
                while (!typed.empty() && (typed.back() == '\r' || typed.back() == '\n')) typed.remove_suffix(1);
                if (!co_await stdin_pipe->write_all(std::string(typed) + "\n")) stdin_pipe.reset();
            }
        } else {
            auto killed = co_await process_->kill();
            LoginUpdate update = killed ? LoginUpdate{login::Cancelled{}} : LoginUpdate{login::Failed{killed.error().to_string()}};
            (void)co_await out.send(std::move(update));
            abandon();
            co_return;
        }
    }
    detach();
    for (auto& line : lines.finish()) {
        if (!co_await out.publish(std::move(line))) {
            abandon();
            co_return;
        }
    }
    stdin_pipe.reset();
    auto exit = co_await process_->wait();
    LoginUpdate update = login::Failed{"login was stopped"};
    if (!exit) update = login::Failed{exit.error().to_string()};
    else if (*exit == 0) update = login::Succeeded{std::nullopt};
    else if (*exit) {
        update = login::Failed{out.last ? std::format("login exited with code {}: {}", **exit, *out.last)
                                        : std::format("login exited with code {}", **exit)};
    }
    (void)co_await out.send(std::move(update));
    process_.reset();
}

} // namespace loa::harness::account_support
