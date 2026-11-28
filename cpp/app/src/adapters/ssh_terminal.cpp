#include "loa/app/adapters/ssh_terminal.hpp"

#include "loa/remote/pty.hpp"
#include "loa/remote/service.hpp"
#include "offload.hpp"

namespace loa::app::adapters {

namespace port = view::terminal::port;

namespace {

port::Error remote_error(const std::string& host, const remote::Error& error) {
    return port::Error::remote(host, error.message());
}

port::Result<> checked(const std::string& host, const remote::Result<void>& result) {
    if (!result) return std::unexpected(remote_error(host, result.error()));
    return {};
}

// The shell calls block, so each runs on its own thread.
class SshReader final : public port::ChannelReader {
  public:
    explicit SshReader(remote::ShellReader reader) : reader_(std::move(reader)) {}

    contracts::Awaitable<port::Result<port::ChannelRead>> read() override {
        auto read = co_await detail::offload([reader = reader_]() mutable { return reader.read(); });
        if (read.kind == remote::ShellRead::Kind::closed) co_return port::channel_read::Closed{read.exit_status};
        co_return port::channel_read::Data{std::vector<std::uint8_t>(read.data.begin(), read.data.end())};
    }

  private:
    remote::ShellReader reader_;
};

class SshWriter final : public port::ChannelWriter {
  public:
    explicit SshWriter(remote::ShellWriter writer) : writer_(std::move(writer)) {}

    contracts::Awaitable<port::Result<>> write(std::vector<std::uint8_t> bytes) override {
        co_return co_await detail::offload([this, bytes = std::move(bytes)] {
            return checked(writer_.host(), writer_.write({reinterpret_cast<const char*>(bytes.data()), bytes.size()}));
        });
    }

    contracts::Awaitable<port::Result<>> resize(port::GridSize size) override {
        co_return co_await detail::offload([this, size] { return checked(writer_.host(), writer_.resize(size.cols, size.rows)); });
    }

    contracts::Awaitable<port::Result<>> close() override {
        co_return co_await detail::offload([this] { return checked(writer_.host(), writer_.close()); });
    }

  private:
    remote::ShellWriter writer_;
};

} // namespace

contracts::Awaitable<port::Result<port::PtyChannel>> SshPty::open_pty(std::string host, port::GridSize size) const {
    auto shell = co_await detail::offload(
        [service = service_, host, size] { return service->open_shell(host, size.cols, size.rows); });
    if (!shell) co_return std::unexpected(remote_error(host, shell.error()));
    co_return port::PtyChannel{std::make_unique<SshReader>(std::move(shell->reader)),
                               std::make_unique<SshWriter>(std::move(shell->writer))};
}

} // namespace loa::app::adapters
