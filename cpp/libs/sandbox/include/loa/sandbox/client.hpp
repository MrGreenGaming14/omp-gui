#pragma once

// The host side of the stub protocol (Rust `loa_sandbox::client`).

#include <atomic>
#include <memory>
#include <mutex>

#include "loa/contracts/async.hpp"
#include "loa/sandbox/channel.hpp"
#include "loa/sandbox/proto.hpp"

namespace loa::sandbox {

// Everything a command printed and how it ended.
struct ExecOutput {
    proto::Bytes stdout_data;
    proto::Bytes stderr_data;
    proto::ExitStatus status;
    friend bool operator==(const ExecOutput&, const ExecOutput&) = default;
};

// A connection to one stub. Requests may run concurrently; answers are routed back by request
// id on a reader thread. The `_blocking` calls block the calling thread; the coroutine calls
// run them off the executor.
class StubClient {
  public:
    struct State;

    // Waits for the stub's hello on `channel`, skipping anything printed before it, and starts
    // routing its answers.
    [[nodiscard]] static Result<std::shared_ptr<StubClient>> connect_blocking(Channel channel);
    [[nodiscard]] static contracts::Awaitable<Result<std::shared_ptr<StubClient>>> connect(Channel channel);

    // Runs a command to the end, collecting its output.
    [[nodiscard]] Result<ExecOutput> exec_blocking(proto::ExecRequest exec) const;
    [[nodiscard]] contracts::Awaitable<Result<ExecOutput>> exec(proto::ExecRequest exec) const;

    // Runs a command that must succeed and returns its stdout; a failure carries its stderr.
    [[nodiscard]] Result<proto::Bytes> run_blocking(proto::ExecRequest exec) const;
    [[nodiscard]] contracts::Awaitable<Result<proto::Bytes>> run(proto::ExecRequest exec) const;

    // Asks the stub to kill its commands and exit.
    [[nodiscard]] Result<> shutdown_blocking() const;
    [[nodiscard]] contracts::Awaitable<Result<>> shutdown() const;

    StubClient(std::unique_ptr<ByteWriter> writer, std::shared_ptr<State> state);
    ~StubClient();

  private:
    [[nodiscard]] Result<> send(const proto::Request& request) const;

    mutable std::mutex writer_mutex_;
    std::unique_ptr<ByteWriter> writer_;
    std::shared_ptr<State> state_;
    mutable std::atomic<std::uint32_t> next_id_{1};
};

} // namespace loa::sandbox
