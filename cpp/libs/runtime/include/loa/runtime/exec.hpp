#pragma once

// Finding, writing and running executables. Children forked by other threads inherit
// writable descriptors, so fresh files can hit `ETXTBSY`; these helpers avoid or retry it.

#include <chrono>
#include <cstddef>
#include <expected>
#include <filesystem>
#include <memory>
#include <optional>
#include <string>
#include <string_view>
#include <system_error>
#include <thread>
#include <utility>
#include <vector>

#include "loa/runtime/error.hpp"

namespace loa::runtime::exec {

using Path = std::filesystem::path;

// Calls `spawn` again after a doubling backoff (2 ms up to 50 ms, 12 tries, about 0.4 s
// in total) while it fails with `ETXTBSY`. `spawn` returns `std::expected<T, std::error_code>`.
template <typename F>
auto retry_busy(F&& spawn) -> decltype(spawn()) {
    auto delay = std::chrono::milliseconds(2);
    for (int attempt = 1; attempt < 12; ++attempt) {
        auto result = spawn();
        if (result || result.error() != std::errc::text_file_busy) return result;
        std::this_thread::sleep_for(delay);
        delay = std::min(delay * 2, std::chrono::milliseconds(50));
    }
    return spawn();
}

// Writes `contents` to `path` (created or truncated) with mode 0755, never holding it
// open for writing in this process (`/bin/sh -c 'cat > "$1" && chmod 755 "$1"'`).
[[nodiscard]] std::expected<void, Error> write_executable(const Path& path, std::string_view contents);

// Whether `file` names an executable file: a regular file with any execute bit set.
[[nodiscard]] bool is_executable(const Path& file);

// Testable core of `which` with the `PATH` and `PATHEXT` values passed in. A bare name is
// searched in every non-empty `path_var` entry; a path with separators is checked as given.
// `pathext` only matters on Windows.
[[nodiscard]] std::optional<Path> which_in(const Path& program, std::optional<std::string_view> path_var,
                                           std::optional<std::string_view> pathext = std::nullopt);
// `which_in` with the process `PATH` and `PATHEXT`.
[[nodiscard]] std::optional<Path> which(const Path& program);
// What `which` resolves, else `program` unchanged so a spawn fails with its usual error.
[[nodiscard]] Path resolve(const Path& program);
// `resolve` searching `path_var` (a child's own `PATH`) instead of the process `PATH` when given.
[[nodiscard]] Path resolve_in(const Path& program, std::optional<std::string_view> path_var);

// Whether the environment variable `name` is `PATH`: case-insensitive on Windows, exact elsewhere.
[[nodiscard]] bool is_path_var(std::string_view name) noexcept;

// A program to run: `env` goes on top of the app's environment.
struct Command {
    Path program;
    std::vector<std::string> args;
    std::vector<std::pair<std::string, std::string>> env;
    std::optional<Path> current_dir;
};

// A command running `program` as `resolve` finds it.
[[nodiscard]] Command command(const Path& program, std::vector<std::string> args = {},
                              std::vector<std::pair<std::string, std::string>> env = {}, std::optional<Path> current_dir = {});

// A child's wait status.
struct ExitStatus {
    int raw = 0;
    [[nodiscard]] bool success() const noexcept;
    // The exit code, none when killed by a signal.
    [[nodiscard]] std::optional<int> code() const noexcept;
    // "exit status: 3" or "signal: 9 (SIGKILL)", as Rust's `ExitStatus` display.
    [[nodiscard]] std::string to_string() const;
};

struct Output {
    ExitStatus status;
    std::string out;
    std::string err;
};

// A running child with stdin on /dev/null and stdout/stderr piped. Killed (SIGKILL) and
// reaped on destruction unless already waited for.
class Child {
  public:
    // Spawns `cmd`, retrying `ETXTBSY` with `retry_busy`.
    [[nodiscard]] static std::expected<Child, Error> spawn(const Command& cmd);

    Child(Child&& other) noexcept;
    Child& operator=(Child&& other) noexcept;
    Child(const Child&) = delete;
    Child& operator=(const Child&) = delete;
    ~Child();

    [[nodiscard]] int pid() const noexcept { return pid_; }

    // Reads both streams to their end and reaps the child. With a timeout, gives up with
    // `ErrorCode::timeout` when it passes (the child is then killed on destruction).
    [[nodiscard]] std::expected<Output, Error> wait_with_output(std::optional<std::chrono::milliseconds> timeout = {});

  private:
    Child(int pid, int out, int err) noexcept : pid_(pid), out_(out), err_(err) {}
    void reset() noexcept;

    int pid_ = -1;
    int out_ = -1;
    int err_ = -1;
};

// Runs a command with stdin closed, capturing both output streams.
[[nodiscard]] std::expected<Output, Error> output(const Command& cmd, std::optional<std::chrono::milliseconds> timeout = {});

// Runs `exe args` and returns its stdout; errors name the command and its first stderr line.
[[nodiscard]] std::expected<std::string, Error> probe_cli(const Path& exe, const std::vector<std::string>& args,
                                                          std::chrono::milliseconds timeout);

// Runs `cmd` to completion: stdout on success, else the exit status and first stderr line.
[[nodiscard]] std::expected<std::string, std::string> run_checked(const Command& cmd);

// The last lines a child wrote to a stream, kept in a bounded ring by a reader thread.
// Copies share the ring.
class StderrTail {
  public:
    // Takes ownership of `fd` and drains it on a thread, keeping its last `lines` lines
    // (invalid UTF-8 replaced by U+FFFD, trailing whitespace trimmed).
    [[nodiscard]] static StderrTail spawn(int fd, std::size_t lines);

    [[nodiscard]] std::string joined() const;
    // Waits until the stream reached its end; false when `timeout` passed first.
    [[nodiscard]] bool wait_closed(std::chrono::milliseconds timeout) const;

    struct State;

  private:
    explicit StderrTail(std::shared_ptr<State> state) : state_(std::move(state)) {}
    std::shared_ptr<State> state_;
};

// `s` single-quoted for a POSIX shell.
[[nodiscard]] std::string shell_quote(std::string_view s);

// The first dotted version in `text`, optionally after `v`, with any `-pre`/`+build` suffix.
[[nodiscard]] std::optional<std::string> parse_version(std::string_view text);

// Whether this process may read and write `/dev/kvm`.
[[nodiscard]] bool kvm_available() noexcept;

} // namespace loa::runtime::exec
