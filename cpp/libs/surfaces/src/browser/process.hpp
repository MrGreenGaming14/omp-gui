#pragma once

// A child process with piped output, killed and reaped on destruction (tokio's
// `Child` with `kill_on_drop`). Private to the browser module: Chrome and Xvfb.

#include <asio/any_io_executor.hpp>
#include <asio/awaitable.hpp>

#include <chrono>
#include <filesystem>
#include <optional>
#include <string>
#include <utility>
#include <vector>

#include "loa/surfaces/browser/error.hpp"

namespace loa::surfaces::browser::detail {

class Process {
  public:
    // Runs `program args` with stdin on /dev/null, stdout and stderr piped
    // (or on /dev/null when `pipe_stdout` is false), `env` set on top of this
    // process's environment.
    [[nodiscard]] static Result<Process> spawn(const std::filesystem::path& program, const std::vector<std::string>& args,
                                               const std::vector<std::pair<std::string, std::string>>& env,
                                               bool pipe_stdout);

    Process(Process&& other) noexcept;
    Process& operator=(Process&& other) noexcept;
    Process(const Process&) = delete;
    Process& operator=(const Process&) = delete;
    ~Process();

    [[nodiscard]] int pid() const noexcept { return pid_; }
    // The read ends; the caller takes ownership (-1 when taken or not piped).
    [[nodiscard]] int take_stdout() noexcept { return std::exchange(out_, -1); }
    [[nodiscard]] int take_stderr() noexcept { return std::exchange(err_, -1); }

    // Sends SIGKILL unless the child was reaped.
    void kill() noexcept;
    // Reaps the child if it exited. True once it is gone.
    bool try_wait() noexcept;
    // Waits up to `timeout` for the child to exit, polling. True when it did.
    [[nodiscard]] asio::awaitable<bool> wait(std::chrono::milliseconds timeout);

  private:
    Process(int pid, int out, int err) noexcept : pid_(pid), out_(out), err_(err) {}
    void reset() noexcept;

    int pid_ = -1;
    bool reaped_ = false;
    int out_ = -1;
    int err_ = -1;
};

// Reads `fd` to its end on a detached thread, handing each line to the log at
// debug level under `target`. Takes ownership of `fd`.
void drain_lines(int fd, std::string target);

// Reads one line from `fd` (without the newline) until `deadline`, polling a
// non-blocking read so no executor thread blocks. Empty on timeout, end of
// stream or error. Leaves `fd` open and blocking.
[[nodiscard]] asio::awaitable<std::optional<std::string>> read_line(int fd, std::chrono::steady_clock::time_point deadline);

} // namespace loa::surfaces::browser::detail
