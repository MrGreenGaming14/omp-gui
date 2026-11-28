#include "loa/sandbox/client.hpp"

#include <condition_variable>
#include <deque>
#include <format>
#include <future>
#include <map>
#include <optional>
#include <thread>

#include "offload.hpp"
#include "process.hpp"

namespace loa::sandbox {

namespace {

// Answers to one request; `closed` once the stub disconnected.
struct Route {
    std::mutex mutex;
    std::condition_variable ready;
    std::deque<proto::Response> queue;
    bool closed = false;

    void push(proto::Response response) {
        {
            std::scoped_lock lock(mutex);
            queue.push_back(std::move(response));
        }
        ready.notify_all();
    }
    void close() {
        {
            std::scoped_lock lock(mutex);
            closed = true;
        }
        ready.notify_all();
    }
    std::optional<proto::Response> pop() {
        std::unique_lock lock(mutex);
        ready.wait(lock, [&] { return !queue.empty() || closed; });
        if (queue.empty()) return std::nullopt;
        auto front = std::move(queue.front());
        queue.pop_front();
        return front;
    }
};

// The error for an answer that isn't the one asked for.
loa::Error refused(const proto::Response& response) {
    if (const auto* e = std::get_if<proto::response::Error>(&response)) return error::stub(e->message);
    return error::stub("unexpected answer " + proto::debug(response));
}

} // namespace

struct StubClient::State {
    std::mutex mutex;
    // None once the stub disconnected.
    std::optional<std::map<std::uint32_t, std::shared_ptr<Route>>> routes{std::in_place};

    void disconnect() {
        std::optional<std::map<std::uint32_t, std::shared_ptr<Route>>> routes_left;
        {
            std::scoped_lock lock(mutex);
            routes_left.swap(routes);
        }
        if (routes_left) {
            for (auto& [_, route] : *routes_left) route->close();
        }
    }
};

namespace {

// Reads the hello, reports it on `ready`, then hands each answer to its request until the
// stream ends.
void route(std::unique_ptr<ByteReader> reader, std::shared_ptr<StubClient::State> state, std::promise<Result<>> ready) {
    proto::Bytes buf;
    Result<> hello;
    auto magic = proto::skip_to_magic(*reader);
    if (!magic) {
        hello = std::unexpected(magic.error());
    } else {
        auto frame = proto::read_frame(*reader, buf);
        if (!frame) hello = std::unexpected(frame.error());
        else if (!*magic || !*frame) hello = std::unexpected(error::disconnected());
        else if (auto response = proto::decode_response(buf); !response) hello = std::unexpected(response.error());
        else if (const auto* r = std::get_if<proto::response::Ready>(&*response); !r) hello = std::unexpected(refused(*response));
        else if (r->version != proto::VERSION)
            hello = std::unexpected(error::stub(std::format("stub speaks protocol {}, the host speaks {}", r->version, proto::VERSION)));
    }
    const bool connected = hello.has_value();
    ready.set_value(std::move(hello));
    while (connected) {
        auto frame = proto::read_frame(*reader, buf);
        if (!frame || !*frame) break;
        auto response = proto::decode_response(buf);
        if (!response) break;
        const auto id = proto::response_id(*response);
        if (!id) continue;
        const bool done = !std::holds_alternative<proto::response::Stdout>(*response) &&
                          !std::holds_alternative<proto::response::Stderr>(*response);
        std::shared_ptr<Route> target;
        {
            std::scoped_lock lock(state->mutex);
            if (!state->routes) break;
            auto it = state->routes->find(*id);
            if (it != state->routes->end()) {
                target = it->second;
                if (done) state->routes->erase(it);
            }
        }
        // A missing route only means the caller stopped listening.
        if (target) {
            target->push(std::move(*response));
            if (done) target->close();
        }
    }
    // Closing every route wakes waiting callers with `Disconnected`.
    state->disconnect();
}

} // namespace

StubClient::StubClient(std::unique_ptr<ByteWriter> writer, std::shared_ptr<State> state)
    : writer_(std::move(writer)), state_(std::move(state)) {}

StubClient::~StubClient() = default;

Result<std::shared_ptr<StubClient>> StubClient::connect_blocking(Channel channel) {
    auto state = std::make_shared<State>();
    std::promise<Result<>> ready;
    auto hello = ready.get_future();
    std::thread(route, std::move(channel.reader), state, std::move(ready)).detach();
    auto connected = hello.get();
    if (!connected) return std::unexpected(connected.error());
    return std::make_shared<StubClient>(std::move(channel.writer), std::move(state));
}

contracts::Awaitable<Result<std::shared_ptr<StubClient>>> StubClient::connect(Channel channel) {
    auto shared = std::make_shared<Channel>(std::move(channel));
    co_return co_await detail::offload([shared] { return connect_blocking(std::move(*shared)); });
}

Result<> StubClient::send(const proto::Request& request) const {
    std::scoped_lock lock(writer_mutex_);
    if (!writer_->write_all(proto::frame(request))) return std::unexpected(error::disconnected());
    return {};
}

Result<ExecOutput> StubClient::exec_blocking(proto::ExecRequest exec) const {
    const auto id = next_id_.fetch_add(1, std::memory_order_relaxed);
    auto route = std::make_shared<Route>();
    {
        std::scoped_lock lock(state_->mutex);
        if (!state_->routes) return std::unexpected(error::disconnected());
        state_->routes->insert_or_assign(id, route);
    }
    if (auto sent = send(proto::request::Exec{id, std::move(exec)}); !sent) return std::unexpected(sent.error());
    ExecOutput out;
    for (;;) {
        auto response = route->pop();
        if (!response) return std::unexpected(error::disconnected());
        if (auto* o = std::get_if<proto::response::Stdout>(&*response)) out.stdout_data += o->data;
        else if (auto* e = std::get_if<proto::response::Stderr>(&*response)) out.stderr_data += e->data;
        else if (auto* x = std::get_if<proto::response::Exit>(&*response)) {
            out.status = x->status;
            return out;
        } else return std::unexpected(refused(*response));
    }
}

Result<proto::Bytes> StubClient::run_blocking(proto::ExecRequest exec) const {
    auto out = exec_blocking(std::move(exec));
    if (!out) return std::unexpected(out.error());
    if (out->status.success()) return std::move(out->stdout_data);
    const auto code = out->status.code ? std::format("Some({})", *out->status.code) : std::string("None");
    return std::unexpected(error::stub(std::format("exit status {}: {}", code, detail::lossy_trim(out->stderr_data))));
}

Result<> StubClient::shutdown_blocking() const { return send(proto::request::Shutdown{}); }

contracts::Awaitable<Result<ExecOutput>> StubClient::exec(proto::ExecRequest exec) const {
    co_return co_await detail::offload([this, exec = std::move(exec)]() mutable { return exec_blocking(std::move(exec)); });
}

contracts::Awaitable<Result<proto::Bytes>> StubClient::run(proto::ExecRequest exec) const {
    co_return co_await detail::offload([this, exec = std::move(exec)]() mutable { return run_blocking(std::move(exec)); });
}

contracts::Awaitable<Result<>> StubClient::shutdown() const {
    co_return co_await detail::offload([this] { return shutdown_blocking(); });
}

} // namespace loa::sandbox
