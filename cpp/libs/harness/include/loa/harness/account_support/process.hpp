#pragma once

// Process runner for harness CLIs (port of `account_support/process.rs`).

#include <cstddef>
#include <expected>
#include <memory>
#include <optional>
#include <span>
#include <string>
#include <string_view>

#include "loa/contracts/account.hpp"
#include "loa/contracts/async.hpp"
#include "loa/runtime/error.hpp"
#include "loa/settings/models/error.hpp"

namespace loa::harness::account_support {

using loa::contracts::Awaitable;
using loa::contracts::CommandSpec;
using loa::settings::models::Error;
using loa::settings::models::Result;

// What a finished process printed and how it exited.
struct ProcessOutput {
    // Exit code; `nullopt` when the process was ended by a signal.
    std::optional<int> code;
    // Standard output, decoded as UTF-8 with invalid bytes replaced.
    std::string out;
    // Standard error, decoded as UTF-8 with invalid bytes replaced.
    std::string err;
    friend bool operator==(const ProcessOutput&, const ProcessOutput&) = default;
};

// Readable end of a child's stdout or stderr pipe.
class ProcessReader {
  public:
    virtual ~ProcessReader() = default;
    // Reads at least one byte into `buf`; 0 at end of stream.
    [[nodiscard]] virtual Awaitable<std::expected<std::size_t, loa::Error>> read_some(std::span<char> buf) = 0;
    // Reads until end of stream.
    [[nodiscard]] Awaitable<std::expected<std::string, loa::Error>> read_to_end();
};

// Writable end of a child's stdin pipe; closed on destruction.
class ProcessWriter {
  public:
    virtual ~ProcessWriter() = default;
    [[nodiscard]] virtual Awaitable<std::expected<void, loa::Error>> write_all(std::string bytes) = 0;
};

// A child process; stdout and stderr should be read while it runs to avoid blocking.
class RunningProcess {
  public:
    virtual ~RunningProcess() = default;
    // Waits for the process to exit and returns its exit code (`nullopt` when it was ended
    // by a signal). Closes stdin first unless it was taken.
    [[nodiscard]] virtual Awaitable<Result<std::optional<int>>> wait() = 0;
    // Kills the process and waits for it to exit.
    [[nodiscard]] virtual Awaitable<Result<>> kill() = 0;
    // Takes the child's stdin; null after the first call.
    [[nodiscard]] virtual std::unique_ptr<ProcessWriter> take_stdin() = 0;
    // Takes the child's stdout; null after the first call.
    [[nodiscard]] virtual std::unique_ptr<ProcessReader> take_stdout() = 0;
    // Takes the child's stderr; null after the first call.
    [[nodiscard]] virtual std::unique_ptr<ProcessReader> take_stderr() = 0;
};

// Runs external programs.
class ProcessRunner {
  public:
    virtual ~ProcessRunner() = default;
    // Runs `spec` to completion with stdin closed and returns its output. A program that
    // cannot be started is an `Error::Kind::Process`.
    [[nodiscard]] virtual Awaitable<Result<ProcessOutput>> output(CommandSpec spec) const = 0;
    // Starts `spec` with stdin, stdout and stderr piped so the app can show the process in
    // its terminal view and forward what the user types. A program that cannot be started
    // is an `Error::Kind::Process`.
    [[nodiscard]] virtual Awaitable<Result<std::unique_ptr<RunningProcess>>> spawn(CommandSpec spec) const = 0;
};

// `ProcessRunner` over Asio pipes on the calling coroutine's executor (the Rust
// `TokioProcessRunner`). Spawned processes are killed when their `RunningProcess` is destroyed.
class AsioProcessRunner final : public ProcessRunner {
  public:
    [[nodiscard]] Awaitable<Result<ProcessOutput>> output(CommandSpec spec) const override;
    [[nodiscard]] Awaitable<Result<std::unique_ptr<RunningProcess>>> spawn(CommandSpec spec) const override;
};

} // namespace loa::harness::account_support
