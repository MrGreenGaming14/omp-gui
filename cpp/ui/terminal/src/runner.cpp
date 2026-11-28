#include "loa/ui/terminal/runner.hpp"

#include <atomic>
#include <chrono>

#include <asio.hpp>

namespace loa::ui::terminal {

namespace {

using asio::awaitable;
using asio::use_awaitable;

std::expected<ChannelRead, std::string> describe(port::Result<ChannelRead> r) {
    if (r) return std::move(*r);
    return std::unexpected(r.error().message());
}

// Reads until the program ends or the channel fails; stops reporting once the tab is gone.
awaitable<void> read_loop(TabId id, std::shared_ptr<PtyChannel> channel, std::shared_ptr<InputQueue> queue, Runner::Post post) {
    auto reader = std::move(channel->reader);
    channel.reset();
    for (;;) {
        auto read = co_await reader->read();
        const bool more = read && std::holds_alternative<port::channel_read::Data>(*read);
        if (queue->closed()) co_return;
        post(msg::Output{id, describe(std::move(read))});
        if (!more) co_return;
    }
}

// Forwards input until the tab closes its queue, then hangs up.
awaitable<void> write_loop(TabId id, std::shared_ptr<PtyChannel> channel, std::shared_ptr<InputQueue> queue, Runner::Post post) {
    auto writer = std::move(channel->writer);
    channel.reset();
    const auto executor = co_await asio::this_coro::executor;
    auto timer = std::make_shared<asio::steady_timer>(executor, asio::steady_timer::time_point::max());
    auto signaled = std::make_shared<std::atomic<bool>>(false);
    queue->set_waker([timer, signaled, executor] {
        signaled->store(true);
        asio::post(executor, [timer] { timer->cancel(); });
    });
    port::Result<> result;
    for (bool done = false; !done && result;) {
        done = queue->closed();
        while (auto item = queue->try_pop()) {
            if (auto* b = std::get_if<inputs::Bytes>(&*item)) result = co_await writer->write(std::move(b->bytes));
            else result = co_await writer->resize(std::get<inputs::Resize>(*item).size);
            if (!result) break;
        }
        if (done || !result || signaled->exchange(false)) continue;
        timer->expires_at(asio::steady_timer::time_point::max());
        asio::error_code ec;
        co_await timer->async_wait(asio::redirect_error(use_awaitable, ec));
    }
    queue->set_waker({});
    if (result) result = co_await writer->close();
    writer.reset();
    if (result) post(msg::Output{id, ChannelRead{port::channel_read::Closed{std::nullopt}}});
    else post(msg::Output{id, std::unexpected(result.error().message())});
}

} // namespace

struct Runner::Impl {
    asio::any_io_executor executor;
    Post post;
};

Runner::Runner(const asio::any_io_executor& executor, Post post) : impl_(std::make_shared<Impl>(Impl{executor, std::move(post)})) {}
Runner::~Runner() = default;

void Runner::run(const Task& task, std::shared_ptr<const PtyBackend> backend) const {
    for (const auto& o : task.ops) {
        if (const auto* open = std::get_if<op::Open>(&o)) {
            asio::co_spawn(
                impl_->executor,
                [backend, open = *open, post = impl_->post]() -> awaitable<void> {
                    auto channel = co_await backend->open(open.target, open.size);
                    if (channel) post(msg::Opened{open.tab, std::make_shared<PtyChannel>(std::move(*channel))});
                    else post(msg::Output{open.tab, std::unexpected(channel.error().message())});
                },
                asio::detached);
        } else if (const auto* drive = std::get_if<op::Drive>(&o)) {
            asio::co_spawn(impl_->executor, read_loop(drive->tab, drive->channel, drive->input, impl_->post), asio::detached);
            asio::co_spawn(impl_->executor, write_loop(drive->tab, drive->channel, drive->input, impl_->post), asio::detached);
        }
    }
}

// ---- Backends -----------------------------------------------------------------------------------

awaitable<port::Result<PtyChannel>> LocalPtyBackend::open(Target target, GridSize size) const {
    const auto* shell = std::get_if<LocalShell>(&target);
    if (!shell) co_return std::unexpected(port::Error::no_remote());
    const auto executor = co_await asio::this_coro::executor;
    co_return spawn(*shell, size, executor);
}

awaitable<port::Result<PtyChannel>> RemotePtyBackend::open(Target target, GridSize size) const {
    const auto* remote = std::get_if<Remote>(&target);
    if (!remote) co_return co_await fallback_->open(std::move(target), size);
    co_return co_await remote_->open_pty(remote->host, size);
}

} // namespace loa::ui::terminal
