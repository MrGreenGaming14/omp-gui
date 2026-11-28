#include "loa/p2p/transport.hpp"

#include <algorithm>
#include <condition_variable>
#include <deque>
#include <format>
#include <future>
#include <mutex>
#include <thread>

namespace loa::p2p {

std::string NodeError::to_string() const {
    switch (kind_) {
    case Kind::io: return "io: " + message_;
    case Kind::protocol: return "protocol: " + message_;
    case Kind::decode: return "decode: " + message_;
    case Kind::refused: return "the peer refused: " + message_;
    case Kind::timeout: return message_ + " timed out";
    default: return message_;
    }
}

Result<> RecvHalf::read_exact(std::span<std::uint8_t> buf) {
    while (!buf.empty()) {
        auto n = read(buf);
        if (!n) return std::unexpected(n.error());
        if (*n == 0) return fail(NodeError::Kind::io, "early eof");
        buf = buf.subspan(*n);
    }
    return {};
}

// ---- in-memory duplex ----

namespace {

struct Pipe {
    std::mutex mutex;
    std::condition_variable changed;
    std::deque<std::uint8_t> data;
    std::size_t capacity;
    bool writer_done = false;
    bool reader_gone = false;
    explicit Pipe(std::size_t cap) : capacity(cap) {}
};

class PipeSend final : public SendHalf {
  public:
    explicit PipeSend(std::shared_ptr<Pipe> pipe) : pipe_(std::move(pipe)) {}
    ~PipeSend() override { (void)shutdown(); }

    Result<> write_all(std::span<const std::uint8_t> data) override {
        std::unique_lock lock(pipe_->mutex);
        while (!data.empty()) {
            pipe_->changed.wait(lock, [&] { return pipe_->reader_gone || pipe_->data.size() < pipe_->capacity; });
            if (pipe_->reader_gone) return fail(NodeError::Kind::io, "broken pipe");
            if (pipe_->writer_done) return fail(NodeError::Kind::io, "write after shutdown");
            const auto n = std::min(data.size(), pipe_->capacity - pipe_->data.size());
            pipe_->data.insert(pipe_->data.end(), data.begin(), data.begin() + static_cast<std::ptrdiff_t>(n));
            data = data.subspan(n);
            pipe_->changed.notify_all();
        }
        return {};
    }

    Result<> shutdown() override {
        std::lock_guard lock(pipe_->mutex);
        pipe_->writer_done = true;
        pipe_->changed.notify_all();
        return {};
    }

  private:
    std::shared_ptr<Pipe> pipe_;
};

class PipeRecv final : public RecvHalf {
  public:
    explicit PipeRecv(std::shared_ptr<Pipe> pipe) : pipe_(std::move(pipe)) {}
    ~PipeRecv() override {
        std::lock_guard lock(pipe_->mutex);
        pipe_->reader_gone = true;
        pipe_->changed.notify_all();
    }

    Result<std::size_t> read(std::span<std::uint8_t> buf) override {
        if (buf.empty()) return std::size_t{0};
        std::unique_lock lock(pipe_->mutex);
        pipe_->changed.wait(lock, [&] { return pipe_->writer_done || !pipe_->data.empty(); });
        const auto n = std::min(buf.size(), pipe_->data.size());
        std::copy_n(pipe_->data.begin(), n, buf.begin());
        pipe_->data.erase(pipe_->data.begin(), pipe_->data.begin() + static_cast<std::ptrdiff_t>(n));
        pipe_->changed.notify_all();
        return n;
    }

  private:
    std::shared_ptr<Pipe> pipe_;
};

} // namespace

std::pair<Stream, Stream> duplex(std::size_t capacity) {
    auto ab = std::make_shared<Pipe>(capacity);
    auto ba = std::make_shared<Pipe>(capacity);
    Stream a{std::make_unique<PipeSend>(ab), std::make_unique<PipeRecv>(ba)};
    Stream b{std::make_unique<PipeSend>(ba), std::make_unique<PipeRecv>(ab)};
    return {std::move(a), std::move(b)};
}

// ---- router ----

Router& Router::handler(std::string_view alpn, std::shared_ptr<ProtocolHandler> handler) {
    handlers_.insert_or_assign(std::string(alpn), std::move(handler));
    return *this;
}

std::vector<std::string> Router::alpns() const {
    std::vector<std::string> out;
    out.reserve(handlers_.size());
    for (const auto& [alpn, _] : handlers_) out.push_back(alpn);
    return out;
}

void Router::dispatch(const Connection& conn) const {
    const auto alpn = conn->alpn();
    if (auto it = handlers_.find(std::string_view(reinterpret_cast<const char*>(alpn.data()), alpn.size()));
        it != handlers_.end())
        it->second->handle(conn);
    else
        conn->close(CLOSE_NO_HANDLER, "no handler");
}

Result<> check_alpn(const NodeId& peer, std::string_view asked, const Bytes* got) {
    if (got && std::ranges::equal(*got, asked, [](std::uint8_t a, char b) { return a == static_cast<std::uint8_t>(b); }))
        return {};
    return fail(NodeError::Kind::connect, std::format("connecting to {}: the peer doesn't serve {}", peer.to_string(), asked));
}

// ---- fallback ----

namespace {

using Attempt = std::function<Result<Connection>(Transport&)>;

Result<Connection> each(const FallbackDialer& dialer, const std::string& target, const Attempt& attempt) {
    std::optional<NodeError> last;
    for (std::size_t i = 0; i < dialer.transports.size(); ++i) {
        const auto& transport = dialer.transports[i];
        Result<Connection> result = fail(NodeError::Kind::timeout, "the first transport");
        if (i == 0) {
            // The attempt keeps running detached past the deadline; it owns what it touches.
            auto done = std::make_shared<std::promise<Result<Connection>>>();
            auto future = done->get_future();
            std::thread([transport, attempt, done] { done->set_value(attempt(*transport)); }).detach();
            if (future.wait_for(dialer.first_timeout) == std::future_status::ready) result = future.get();
        } else {
            result = attempt(*transport);
        }
        if (result) return result;
        last = result.error();
    }
    if (last) return std::unexpected(*last);
    return fail(NodeError::Kind::unavailable, "no transport reaches " + target);
}

} // namespace

Result<Connection> FallbackDialer::dial(const NodeId& peer, std::span<const SocketAddr> hints,
                                        std::string_view alpn) const {
    std::vector<SocketAddr> owned(hints.begin(), hints.end());
    return each(*this, "Some(" + peer.to_string() + ")",
                [peer, owned, a = std::string(alpn)](Transport& t) { return t.dial(peer, owned, a); });
}

Result<Connection> FallbackDialer::dial_any(const SocketAddr& addr, std::string_view alpn) const {
    return each(*this, "None", [addr, a = std::string(alpn)](Transport& t) { return t.dial_any(addr, a); });
}

void FallbackDialer::close() const {
    for (const auto& transport : transports) transport->close();
}

} // namespace loa::p2p
