#pragma once

// Internal: fork/exec with piped stdio, an optional cleared environment and an optional new
// process group (what the Rust code gets from `std::process::Command`).

#include <chrono>
#include <filesystem>
#include <optional>
#include <string>
#include <string_view>
#include <utility>
#include <vector>

#include "loa/sandbox/channel.hpp"

namespace loa::sandbox::detail {

enum class Stdio { null, piped, inherit };

struct Spawn {
    std::filesystem::path program;
    std::vector<std::string> args;
    std::optional<std::filesystem::path> cwd;
    // Start from an empty environment instead of this process's.
    bool clear_env = false;
    // Added on top (replacing variables of the same name).
    std::vector<std::pair<std::string, std::string>> env;
    Stdio in = Stdio::null;
    Stdio out = Stdio::null;
    Stdio err = Stdio::null;
    // The child leads a new process group (`process_group(0)`).
    bool new_group = false;
    // Killed with SIGKILL and reaped when the `Process` is destroyed unwaited (`kill_on_drop`).
    bool kill_on_drop = true;
};

// A spawned child.
class Process {
  public:
    [[nodiscard]] static Result<Process> spawn(const Spawn& spec);
    // Takes over a forked child and the parent's pipe ends.
    [[nodiscard]] static Process adopt(int pid, int in, int out, int err, bool kill_on_drop) noexcept {
        return Process(pid, in, out, err, kill_on_drop);
    }

    Process(Process&& other) noexcept;
    Process& operator=(Process&& other) noexcept;
    Process(const Process&) = delete;
    Process& operator=(const Process&) = delete;
    ~Process();

    [[nodiscard]] int pid() const noexcept { return pid_; }
    // The parent's pipe ends; -1 when not piped or already taken. The caller owns them.
    [[nodiscard]] int take_stdin() noexcept { return std::exchange(in_, -1); }
    [[nodiscard]] int take_stdout() noexcept { return std::exchange(out_, -1); }
    [[nodiscard]] int take_stderr() noexcept { return std::exchange(err_, -1); }

    // Blocks for the exit; the raw wait status.
    [[nodiscard]] Result<int> wait();
    // The raw wait status if it exited already.
    [[nodiscard]] Result<std::optional<int>> try_wait();
    // SIGKILL to the child (no-op once reaped).
    void kill() noexcept;
    [[nodiscard]] bool reaped() const noexcept { return status_.has_value(); }

  private:
    Process(int pid, int in, int out, int err, bool kill_on_drop) noexcept
        : pid_(pid), in_(in), out_(out), err_(err), kill_on_drop_(kill_on_drop) {}
    void reset() noexcept;

    int pid_ = -1;
    int in_ = -1;
    int out_ = -1;
    int err_ = -1;
    bool kill_on_drop_ = true;
    std::optional<int> status_;
};

// What a finished command printed.
struct Captured {
    int status = 0;
    std::string out;
    std::string err;
    [[nodiscard]] bool success() const noexcept;
};

// Runs `spec` with stdout and stderr captured and `input` on its stdin. With a timeout, the
// child is killed once it passes and the run fails with `ErrorCode::timeout`.
[[nodiscard]] Result<Captured> run(Spawn spec, std::string_view input = {},
                                   std::optional<std::chrono::milliseconds> timeout = std::nullopt);
// Rust `ExitStatus` display of a raw wait status ("exit status: 3", "signal: 9 (SIGKILL)").
[[nodiscard]] std::string status_string(int raw);
// Exit code of a raw wait status, none when killed by a signal.
[[nodiscard]] std::optional<int> exit_code(int raw) noexcept;
[[nodiscard]] std::optional<int> exit_signal(int raw) noexcept;

// Rust `String::from_utf8_lossy(s).trim()`.
[[nodiscard]] std::string lossy_trim(std::string_view s);
// Rust `String::from_utf8_lossy`.
[[nodiscard]] std::string lossy(std::string_view s);
[[nodiscard]] bool valid_utf8(std::string_view s);

} // namespace loa::sandbox::detail
