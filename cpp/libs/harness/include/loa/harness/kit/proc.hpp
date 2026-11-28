#pragma once

// Harness processes: piped spawn that survives `ETXTBSY`, a stderr tail, and the error
// mapping every adapter shares.

#include <cstddef>
#include <optional>
#include <string>
#include <string_view>
#include <system_error>
#include <utility>

#include "loa/harness/kit/rpc.hpp"
#include "loa/runtime/exec.hpp"

namespace loa::harness::kit {

// Lines of stderr kept for error reports.
inline constexpr std::size_t STDERR_LINES = 64;

// An owned file descriptor, closed on destruction.
class Fd {
  public:
    Fd() = default;
    explicit Fd(int fd) noexcept : fd_(fd) {}
    Fd(Fd&& other) noexcept : fd_(std::exchange(other.fd_, -1)) {}
    Fd& operator=(Fd&& other) noexcept;
    Fd(const Fd&) = delete;
    Fd& operator=(const Fd&) = delete;
    ~Fd() { reset(); }

    [[nodiscard]] int get() const noexcept { return fd_; }
    [[nodiscard]] int release() noexcept { return std::exchange(fd_, -1); }
    void reset() noexcept;

  private:
    int fd_ = -1;
};

// A spawned child: SIGKILLed and reaped on destruction unless released or reaped.
class ChildProcess {
  public:
    ChildProcess() = default;
    explicit ChildProcess(int pid) noexcept : pid_(pid) {}
    ChildProcess(ChildProcess&& other) noexcept : pid_(std::exchange(other.pid_, -1)) {}
    ChildProcess& operator=(ChildProcess&& other) noexcept;
    ChildProcess(const ChildProcess&) = delete;
    ChildProcess& operator=(const ChildProcess&) = delete;
    ~ChildProcess();

    [[nodiscard]] int pid() const noexcept { return pid_; }
    // The exit status once the child exited, without blocking; reaps it.
    [[nodiscard]] std::optional<ExitStatus> try_wait();
    // Sends SIGKILL (a no-op once reaped).
    void kill() noexcept;
    // Blocks until the child exits and reaps it; `nullopt` when it can't be waited for.
    [[nodiscard]] std::optional<ExitStatus> wait();

  private:
    int pid_ = -1;
};

// A spawned child with its pipes taken.
struct Piped {
    ChildProcess child;
    Fd stdin_fd;
    Fd stdout_fd;
    loa::runtime::exec::StderrTail stderr;
};

// Spawns `cmd` with piped stdio, killed on destruction. A freshly written executable (a
// staged update, a test fake) can still be open for writing in a child another thread
// forked; that clears once it execs, so a busy executable is retried briefly.
[[nodiscard]] RpcResult<Piped> spawn_piped(const loa::runtime::exec::Command& cmd);

// A missing executable is `RpcError::NotFound`; anything else is i/o.
[[nodiscard]] RpcError spawn_error(std::error_code err, const loa::runtime::exec::Command& cmd);

// Bytes a process wrote, cut from the front to stay under `limit`, never inside a UTF-8 sequence.
class TailBuffer {
  public:
    explicit TailBuffer(std::optional<std::size_t> limit = std::nullopt) : limit_(limit) {}

    void push(std::string_view chunk);
    // The bytes, invalid UTF-8 replaced by U+FFFD.
    [[nodiscard]] std::string text() const;
    [[nodiscard]] const std::string& bytes() const noexcept { return bytes_; }
    // Whether bytes were cut.
    [[nodiscard]] bool truncated() const noexcept { return truncated_; }

  private:
    std::string bytes_;
    std::optional<std::size_t> limit_;
    bool truncated_ = false;
};

} // namespace loa::harness::kit
