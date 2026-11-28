#include "loa/harness/omp/rpc/client.hpp"

#include <asio.hpp>
#include <asio/experimental/awaitable_operators.hpp>
#include <asio/experimental/concurrent_channel.hpp>

#include <algorithm>
#include <mutex>

#include "loa/runtime/exec.hpp"

namespace loa::harness::omp::rpc {

namespace json = loa::runtime::json;
namespace exec = loa::runtime::exec;
namespace kit = loa::harness::kit;

namespace detail {
using EventChannel = asio::experimental::concurrent_channel<void(asio::error_code, Incoming)>;
using Signal = asio::experimental::concurrent_channel<void(asio::error_code)>;

struct EventQueue {
    EventQueue(const asio::any_io_executor& executor, std::size_t capacity)
        : channel(executor, std::max<std::size_t>(capacity, 1)) {}

    EventChannel channel;
    std::atomic<bool> listening{true};
};

class OmpInbound final : public kit::Inbound {
  public:
    OmpInbound(const asio::any_io_executor& executor, std::shared_ptr<EventQueue> queue,
               std::shared_ptr<HostHandler> host)
        : events_(std::move(queue)), ready_signal_(executor, 1), host_(std::move(host)) {}

    [[nodiscard]] Awaitable<RpcResult<Json>> on_request(std::string method, Json params) const override {
        if (method != "host_tool_call")
            co_return std::unexpected(RpcError::Protocol{"this host serves no URI schemes"});
        HostToolOutput output;
        if (host_) {
            auto call = decode_as<HostToolCall>(params);
            if (!call) co_return std::unexpected(std::move(call).error());
            output = co_await host_->tool_call(std::move(*call));
        } else {
            output = HostToolOutput::error("this host serves no tools");
        }
        const auto* id = params.find("id");
        co_return output.result_json(id && id->as_string() ? *id->as_string() : std::string{});
    }

    [[nodiscard]] Awaitable<void> on_notification(std::string method, Json params) const override {
        if (method == "ready") {
            auto parsed = decode_as<Ready>(params);
            {
                std::scoped_lock lock(ready_mutex_);
                if (!ready_done_) {
                    ready_value_ = parsed ? std::move(*parsed) : Ready{};
                    ready_done_ = true;
                    ready_signal_.try_send(asio::error_code{});
                }
            }
            co_return;
        }
        if (!events_->listening.load()) co_return;
        Incoming incoming;
        if (method == "response") incoming = rpc::incoming::Event{event::Unknown{std::move(params)}};
        else {
            Frame frame = frame_from_json(std::move(params));
            if (auto* event = std::get_if<Event>(&frame)) incoming = rpc::incoming::Event{std::move(*event)};
            else incoming = rpc::incoming::Ui{std::move(std::get<UiRequest>(frame))};
        }
        (void)co_await events_->channel.async_send(asio::error_code{}, std::move(incoming), asio::as_tuple(asio::use_awaitable));
    }

    [[nodiscard]] Awaitable<void> on_closed(std::optional<ExitStatus> status, std::string stderr) const override {
        {
            std::scoped_lock lock(ready_mutex_);
            if (!ready_done_) {
                ready_done_ = true;
                ready_signal_.try_send(asio::error_code{});
            }
        }
        if (events_->listening.load()) {
            (void)co_await events_->channel.async_send(asio::error_code{}, rpc::incoming::Exited{std::move(status), std::move(stderr)},
                                                        asio::as_tuple(asio::use_awaitable));
        }
    }

    // Called once, by the coroutine that spawned the child. `on_closed` also wakes it so
    // early exit returns the child's status and stderr rather than timing out.
    [[nodiscard]] Awaitable<std::optional<Ready>> wait_ready(std::chrono::milliseconds timeout) const {
        using namespace asio::experimental::awaitable_operators;
        auto executor = co_await asio::this_coro::executor;
        asio::steady_timer timer(executor, timeout);
        auto woke = co_await (ready_signal_.async_receive(asio::as_tuple(asio::use_awaitable)) ||
                              timer.async_wait(asio::as_tuple(asio::use_awaitable)));
        if (woke.index() == 1) co_return std::nullopt;
        std::scoped_lock lock(ready_mutex_);
        co_return ready_value_.value_or(Ready{});
    }

    [[nodiscard]] bool has_ready() const {
        std::scoped_lock lock(ready_mutex_);
        return ready_value_.has_value();
    }

  private:
    std::shared_ptr<EventQueue> events_;
    mutable Signal ready_signal_;
    mutable std::mutex ready_mutex_;
    mutable bool ready_done_ = false;
    mutable std::optional<Ready> ready_value_;
    std::shared_ptr<HostHandler> host_;
};

struct Inner {
    kit::StdioRpc<OmpFrames> rpc;
    Ready ready;
    std::atomic<std::uint32_t> protocol{1};

    Inner(kit::StdioRpc<OmpFrames> rpc, Ready ready) : rpc(std::move(rpc)), ready(std::move(ready)) {}
    ~Inner() { rpc.kill(); }
};
} // namespace detail

exec::Command SpawnOptions::command() const {
    auto cmd = exec::command(executable);
    cmd.args = {"--mode", mode == Mode::RpcUi ? "rpc-ui" : "rpc"};
    if (no_ui) cmd.args.emplace_back("--no-ui");
    cmd.args.insert(cmd.args.end(), args.begin(), args.end());
    cmd.env = env;
    cmd.env_remove = env_remove;
    cmd.current_dir = cwd;
    return cmd;
}

Events& Events::operator=(Events&& other) noexcept {
    if (this != &other) {
        if (queue_) {
            queue_->listening = false;
            queue_->channel.close();
        }
        queue_ = std::move(other.queue_);
    }
    return *this;
}
Events::~Events() {
    if (queue_) {
        queue_->listening = false;
        queue_->channel.close();
    }
}
Awaitable<std::optional<Incoming>> Events::recv() {
    if (!queue_) co_return std::nullopt;
    auto [ec, frame] = co_await queue_->channel.async_receive(asio::as_tuple(asio::use_awaitable));
    if (ec) co_return std::nullopt;
    if (std::holds_alternative<incoming::Exited>(frame)) queue_->channel.close();
    co_return std::move(frame);
}

RpcResult<void> UiResponder::respond(const UiResponse& response) const { return writer_.send(to_json(response)); }

std::optional<RpcClient> WeakRpcClient::upgrade() const {
    if (auto inner = inner_.lock()) return RpcClient(std::move(inner));
    return std::nullopt;
}

Awaitable<RpcResult<Spawned>> RpcClient::spawn(SpawnOptions opts) {
    auto executor = co_await asio::this_coro::executor;
    auto events = std::make_shared<detail::EventQueue>(executor, opts.event_capacity);
    auto inbound = std::make_shared<detail::OmpInbound>(executor, events, opts.host);
    auto rpc = kit::StdioRpc<OmpFrames>::spawn(executor, opts.command(), OmpFrames{}, inbound);
    if (!rpc) co_return std::unexpected(std::move(rpc).error());
    auto ready = co_await inbound->wait_ready(opts.ready_timeout);
    if (!ready) {
        rpc->kill();
        co_return std::unexpected(RpcError::StartTimeout{opts.ready_timeout});
    }
    if (!inbound->has_ready()) co_return std::unexpected(co_await rpc->closed());

    RpcClient client(std::make_shared<detail::Inner>(std::move(*rpc), std::move(*ready)));
    auto offered = client.ready().offered_versions();
    std::uint32_t chosen = 0;
    for (auto version : offered) {
        if (version >= MIN_PROTOCOL && version <= MAX_PROTOCOL) chosen = std::max(chosen, version);
    }
    if (!chosen) {
        client.kill();
        co_return std::unexpected(RpcError::Version{std::move(offered), MIN_PROTOCOL, MAX_PROTOCOL});
    }
    if (chosen >= 2 && !opts.negotiate) chosen = 1;
    if (chosen >= 2) {
        auto negotiated = co_await client.request(command::NegotiateProtocol{chosen});
        if (!negotiated) co_return std::unexpected(std::move(negotiated).error());
    }
    client.inner_->protocol.store(chosen, std::memory_order_release);
    co_return Spawned{std::move(client), Events(std::move(events))};
}

const Ready& RpcClient::ready() const { return inner_->ready; }
std::uint32_t RpcClient::protocol_version() const { return inner_->protocol.load(std::memory_order_acquire); }
bool RpcClient::has_exited() const { return inner_->rpc.has_exited(); }
std::string RpcClient::reserve_id() const { return "c" + std::to_string(inner_->rpc.next_id()); }

Awaitable<RpcResult<Json>> RpcClient::request_as(std::string id, Command command) const {
    auto frame = to_json(command);
    const auto* type = frame.find("type");
    auto result = co_await inner_->rpc.request_as(Json(std::move(id)), type && type->as_string() ? *type->as_string() : std::string{}, std::move(frame));
    if (!result) co_return std::unexpected(std::move(result).error());
    co_return kit::decode(std::move(*result));
}
Awaitable<RpcResult<Json>> RpcClient::request(Command command) const {
    co_return co_await request_as(reserve_id(), std::move(command));
}
RpcResult<void> RpcClient::respond_ui(const UiResponse& response) const { return inner_->rpc.send(to_json(response)); }
UiResponder RpcClient::ui_responder() const { return UiResponder(inner_->rpc.writer()); }
WeakRpcClient RpcClient::downgrade() const { return WeakRpcClient(inner_); }
void RpcClient::kill() const { inner_->rpc.kill(); }
Awaitable<RpcResult<std::optional<ExitStatus>>> RpcClient::shutdown(std::chrono::milliseconds grace) const {
    co_return co_await inner_->rpc.shutdown(grace);
}

RpcResult<std::string> omp_version(const std::filesystem::path& executable) {
    auto result = exec::probe_cli(exec::resolve(executable), {"--version"}, std::chrono::seconds(30));
    if (!result) return std::unexpected(RpcError::Io{std::move(result).error()});
    auto& text = *result;
    const auto start = text.find_first_not_of(" \t\r\n");
    if (start == std::string::npos) return std::string{};
    const auto end = text.find_last_not_of(" \t\r\n");
    return text.substr(start, end - start + 1);
}

} // namespace loa::harness::omp::rpc
