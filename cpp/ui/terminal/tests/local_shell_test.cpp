// A real shell on a local PTY (`/bin/sh`, or `cmd.exe` on ConPTY): cwd, input, resize, exit status, and the cwd
// check. Port of `tests/local_shell.rs`.

#include <doctest/doctest.h>

#include <chrono>
#include <filesystem>
#include <string>

#include <asio.hpp>

#include "loa/runtime/tempfile.hpp"
#include "loa/ui/terminal/state.hpp"

using namespace loa::ui::terminal;
namespace fs = std::filesystem;

namespace {

// `path` as the shell prints its working directory: no `\\?\` prefix on Windows.
std::string printed(const fs::path& path) {
    std::string text = path.string();
#if defined(_WIN32)
    if (text.starts_with(R"(\\?\)")) text.erase(0, 4);
#endif
    return text;
}

// The platform shell in `cwd`.
LocalShell shell(const fs::path& cwd) {
    auto s = LocalShell::login_shell(printed(cwd));
#if defined(_WIN32)
    s.program = "cmd.exe";
    s.args = {"/D", "/Q"};
#else
    s.program = "/bin/sh";
#endif
    return s;
}

// Prints the working directory, the window size and `$LOA_MARK`, then exits with 3. The mark is expanded only in
// the output, so the echoed command line can't match however ConPTY redraws the screen.
#if defined(_WIN32)
constexpr std::string_view SCRIPT =
    "cd&powershell.exe -NoProfile -Command \"'{0} {1}' -f [Console]::WindowHeight,[Console]::WindowWidth\"&echo %LOA_MARK%&exit 3\r";
#else
constexpr std::string_view SCRIPT = "pwd; stty size; echo $LOA_MARK; exit 3\n";
#endif

template <typename T>
port::Result<T> block_on(asio::io_context& io, loa::contracts::Awaitable<port::Result<T>> work) {
    std::optional<port::Result<T>> out;
    asio::co_spawn(io, std::move(work), [&](std::exception_ptr, port::Result<T> r) { out = std::move(r); });
    io.restart();
    while (!out && io.run_one() > 0) {}
    REQUIRE(out);
    return std::move(*out);
}

} // namespace

TEST_CASE("sh runs in its cwd, resizes and reports its exit status") {
    auto dir = loa::runtime::TempDir::create();
    REQUIRE(dir);
    const auto cwd = fs::canonical(dir->path());
    auto sh = shell(cwd);
    sh.env = {{"PS1", "$ "}, {"LOA_MARK", "done-42"}};
    asio::io_context io;
    const LocalPtyBackend backend;
    auto ch = block_on(io, backend.open(Target{sh}, GridSize{80, 24}));
    REQUIRE(ch);
    REQUIRE(block_on(io, ch->writer->resize(GridSize{132, 43})));
    REQUIRE(block_on(io, ch->writer->write(Bytes(SCRIPT.begin(), SCRIPT.end()))));

    std::string out;
    std::optional<std::optional<std::uint32_t>> exit;
    const auto deadline = std::chrono::steady_clock::now() + std::chrono::seconds(10);
    while (!exit && std::chrono::steady_clock::now() < deadline) {
        auto read = block_on(io, ch->reader->read());
        REQUIRE_MESSAGE(read, "read");
        if (const auto* data = std::get_if<port::channel_read::Data>(&*read)) {
            const std::string chunk(data->bytes.begin(), data->bytes.end());
            // ConPTY asks for the cursor position at startup and waits for the terminal to answer.
            if (chunk.find("\x1b[6n") != std::string::npos) REQUIRE(block_on(io, ch->writer->write(Bytes{'\x1b', '[', '1', ';', '1', 'R'})));
            out += chunk;
        } else {
            exit = std::get<port::channel_read::Closed>(*read).exit_code;
        }
    }
    INFO("output: " << out);
    REQUIRE_MESSAGE(exit, "exits in time");
    CHECK(*exit == std::optional<std::uint32_t>(3));
    CHECK(out.find(printed(cwd)) != std::string::npos);
    CHECK(out.find("43 132") != std::string::npos);
    CHECK(out.find("done-42") != std::string::npos);

    sh.cwd = cwd / "missing";
    const auto missing = block_on(io, backend.open(Target{sh}, GridSize{}));
    REQUIRE_FALSE(missing);
    CHECK(missing.error().kind() == port::Error::Kind::NotADirectory);
    const auto remote = block_on(io, backend.open(Target{Remote{"h"}}, GridSize{}));
    REQUIRE_FALSE(remote);
    CHECK(remote.error().kind() == port::Error::Kind::NoRemote);
}
