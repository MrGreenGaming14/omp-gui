#include "browser/cdp.hpp"

#include <asio/as_tuple.hpp>
#include <asio/co_spawn.hpp>
#include <asio/connect.hpp>
#include <asio/detached.hpp>
#include <asio/experimental/awaitable_operators.hpp>
#include <asio/experimental/concurrent_channel.hpp>
#include <asio/ip/tcp.hpp>
#include <asio/post.hpp>
#include <asio/read.hpp>
#include <asio/steady_timer.hpp>
#include <asio/strand.hpp>
#include <asio/use_awaitable.hpp>
#include <asio/write.hpp>

#include <openssl/rand.h>

#include <array>
#include <atomic>
#include <map>
#include <mutex>

#include "loa/core/base64.hpp"
#include "loa/runtime/log.hpp"

namespace loa::surfaces::browser::cdp {

namespace json = loa::runtime::json;
using asio::ip::tcp;
using namespace asio::experimental::awaitable_operators;

namespace {

constexpr auto use_tuple = asio::as_tuple(asio::use_awaitable);

// Largest message accepted from Chrome (screenshots of big pages run to tens of MB).
constexpr std::uint64_t MAX_MESSAGE = 512ULL << 20;

struct Reply {
    explicit Reply(const asio::any_io_executor& ex) : signal(ex, 1) {}
    std::optional<Result<Value>> result;
    asio::experimental::concurrent_channel<void(asio::error_code)> signal;
};

struct Subscription {
    std::uint64_t token;
    std::string session;
    std::string method;
    std::shared_ptr<EventHandler> handler;
};

std::string error_text(const Value& error) {
    const auto* message = error.find("message");
    std::string text = message && message->as_string() ? *message->as_string() : json::dump(error);
    if (const auto* data = error.find("data"); data && data->as_string()) text += ": " + *data->as_string();
    return text;
}

} // namespace

struct Connection::Impl : std::enable_shared_from_this<Connection::Impl> {
    explicit Impl(const asio::any_io_executor& ex)
        : strand(asio::make_strand(ex)), socket(strand), outgoing(ex, 4096) {}

    asio::strand<asio::any_io_executor> strand;
    tcp::socket socket;
    std::string pending_bytes; // read past the HTTP response
    asio::experimental::concurrent_channel<void(asio::error_code, std::string)> outgoing;

    mutable std::mutex mutex;
    std::map<std::int64_t, std::shared_ptr<Reply>> pending;
    std::vector<Subscription> subscriptions;
    std::atomic<std::int64_t> next_id{0};
    std::atomic<std::uint64_t> next_token{0};
    std::atomic<bool> closed{false};

    // Reads exactly `n` bytes, first from what the handshake left over.
    asio::awaitable<bool> read_exact(std::string& out, std::size_t n) {
        out.clear();
        const auto take = std::min(n, pending_bytes.size());
        out.append(pending_bytes, 0, take);
        pending_bytes.erase(0, take);
        if (out.size() < n) {
            const auto have = out.size();
            out.resize(n);
            auto [ec, got] = co_await asio::async_read(socket, asio::buffer(out.data() + have, n - have), use_tuple);
            if (ec) co_return false;
        }
        co_return true;
    }

    void enqueue_frame(std::uint8_t opcode, std::string_view payload) {
        std::string frame;
        frame.push_back(char(0x80 | opcode));
        const auto len = payload.size();
        if (len < 126) {
            frame.push_back(char(0x80 | len));
        } else if (len <= 0xffff) {
            frame.push_back(char(0x80 | 126));
            frame.push_back(char(len >> 8));
            frame.push_back(char(len & 0xff));
        } else {
            frame.push_back(char(0x80 | 127));
            for (int shift = 56; shift >= 0; shift -= 8) frame.push_back(char((std::uint64_t(len) >> shift) & 0xff));
        }
        std::array<unsigned char, 4> mask{};
        RAND_bytes(mask.data(), int(mask.size()));
        frame.append(reinterpret_cast<const char*>(mask.data()), mask.size());
        const auto start = frame.size();
        frame.append(payload);
        for (std::size_t i = 0; i < len; ++i) frame[start + i] = char(frame[start + i] ^ char(mask[i % 4]));
        if (!outgoing.try_send(asio::error_code{}, std::move(frame))) {
            loa::runtime::log::warn("loa_surfaces::browser", "devtools send queue full or closed");
        }
    }

    asio::awaitable<void> writer() {
        auto self = shared_from_this();
        for (;;) {
            auto [ec, frame] = co_await outgoing.async_receive(use_tuple);
            if (ec) break;
            auto [wec, n] = co_await asio::async_write(socket, asio::buffer(frame), use_tuple);
            if (wec) break;
        }
        shutdown();
    }

    asio::awaitable<void> reader() {
        auto self = shared_from_this();
        std::string header;
        std::string chunk;
        std::string message;
        for (;;) {
            if (!co_await read_exact(header, 2)) break;
            const auto b0 = static_cast<std::uint8_t>(header[0]);
            const auto b1 = static_cast<std::uint8_t>(header[1]);
            const bool fin = (b0 & 0x80) != 0;
            const std::uint8_t opcode = b0 & 0x0f;
            const bool masked = (b1 & 0x80) != 0;
            std::uint64_t len = b1 & 0x7f;
            if (len == 126) {
                if (!co_await read_exact(header, 2)) break;
                len = std::uint64_t(std::uint8_t(header[0])) << 8 | std::uint8_t(header[1]);
            } else if (len == 127) {
                if (!co_await read_exact(header, 8)) break;
                len = 0;
                for (char c : header) len = len << 8 | std::uint8_t(c);
            }
            if (len > MAX_MESSAGE) break;
            std::array<std::uint8_t, 4> mask{};
            if (masked) {
                if (!co_await read_exact(header, 4)) break;
                std::memcpy(mask.data(), header.data(), 4);
            }
            if (!co_await read_exact(chunk, std::size_t(len))) break;
            if (masked) {
                for (std::size_t i = 0; i < chunk.size(); ++i) chunk[i] = char(chunk[i] ^ char(mask[i % 4]));
            }
            if (opcode == 0x8) break; // close
            if (opcode == 0x9) {      // ping
                enqueue_frame(0xA, chunk);
                continue;
            }
            if (opcode == 0xA) continue; // pong
            if (opcode == 0x1 || opcode == 0x2) {
                message = std::move(chunk);
            } else if (opcode == 0x0) {
                message += chunk;
            } else {
                break;
            }
            if (message.size() > MAX_MESSAGE) break;
            if (fin) {
                dispatch(message);
                message.clear();
            }
        }
        shutdown();
    }

    void dispatch(const std::string& text) {
        auto parsed = json::parse(text);
        if (!parsed) {
            loa::runtime::log::debug("loa_surfaces::browser", "devtools message is not JSON");
            return;
        }
        const Value& msg = *parsed;
        if (const auto* id = msg.find("id"); id && id->as_i64()) {
            std::shared_ptr<Reply> reply;
            {
                std::lock_guard lock(mutex);
                auto it = pending.find(*id->as_i64());
                if (it == pending.end()) return;
                reply = it->second;
                pending.erase(it);
                if (const auto* error = msg.find("error")) {
                    reply->result = std::unexpected(BrowserError::cdp(error_text(*error)));
                } else if (const auto* result = msg.find("result")) {
                    reply->result = *result;
                } else {
                    reply->result = Value(json::Object{});
                }
            }
            reply->signal.try_send(asio::error_code{});
            return;
        }
        const auto* method = msg.find("method");
        if (!method || !method->as_string()) return;
        const auto* session = msg.find("sessionId");
        const std::string session_id = session && session->as_string() ? *session->as_string() : std::string();
        std::vector<std::shared_ptr<EventHandler>> handlers;
        {
            std::lock_guard lock(mutex);
            for (const auto& s : subscriptions) {
                if (s.session == session_id && s.method == *method->as_string()) handlers.push_back(s.handler);
            }
        }
        if (handlers.empty()) return;
        static const Value empty = json::Object{};
        const auto* params = msg.find("params");
        for (const auto& h : handlers) (*h)(params ? *params : empty);
    }

    // Fails every pending call and closes the socket. Idempotent.
    void shutdown() {
        if (closed.exchange(true)) return;
        outgoing.close();
        asio::post(strand, [self = shared_from_this()] {
            asio::error_code ignored;
            self->socket.shutdown(tcp::socket::shutdown_both, ignored);
            self->socket.close(ignored);
        });
        std::map<std::int64_t, std::shared_ptr<Reply>> failed;
        {
            std::lock_guard lock(mutex);
            failed.swap(pending);
            for (auto& [_, reply] : failed) reply->result = std::unexpected(BrowserError::cdp("connection closed"));
        }
        for (auto& [_, reply] : failed) reply->signal.try_send(asio::error_code{});
    }

    std::int64_t enqueue_call(const std::string& method, Value params, const std::string& session,
                              std::shared_ptr<Reply> reply) {
        const auto id = next_id.fetch_add(1) + 1;
        Value msg = json::Object{};
        msg.set("id", id);
        msg.set("method", method);
        msg.set("params", std::move(params));
        if (!session.empty()) msg.set("sessionId", session);
        if (reply) {
            std::lock_guard lock(mutex);
            if (closed) {
                reply->result = std::unexpected(BrowserError::cdp("connection closed"));
                reply->signal.try_send(asio::error_code{});
                return id;
            }
            pending.emplace(id, reply);
        }
        enqueue_frame(0x1, json::dump(msg));
        return id;
    }
};

Connection::~Connection() { close(); }

asio::awaitable<Result<std::shared_ptr<Connection>>> Connection::connect(asio::any_io_executor executor, std::string url) {
    constexpr std::string_view scheme = "ws://";
    if (!url.starts_with(scheme)) co_return std::unexpected(BrowserError::cdp("not a ws:// url: " + url));
    const auto rest = std::string_view(url).substr(scheme.size());
    const auto slash = rest.find('/');
    const auto authority = rest.substr(0, slash);
    const std::string path = slash == std::string_view::npos ? "/" : std::string(rest.substr(slash));
    const auto colon = authority.rfind(':');
    if (colon == std::string_view::npos) co_return std::unexpected(BrowserError::cdp("no port in " + url));
    const std::string host(authority.substr(0, colon));
    const std::string port(authority.substr(colon + 1));

    auto impl = std::make_shared<Impl>(executor);
    tcp::resolver resolver(executor);
    auto [rec, endpoints] = co_await resolver.async_resolve(host, port, use_tuple);
    if (rec) co_return std::unexpected(BrowserError::cdp("resolving " + host + ": " + rec.message()));
    auto [cec, ep] = co_await asio::async_connect(impl->socket, endpoints, use_tuple);
    if (cec) co_return std::unexpected(BrowserError::cdp("connecting to " + url + ": " + cec.message()));

    std::array<std::uint8_t, 16> nonce{};
    RAND_bytes(nonce.data(), int(nonce.size()));
    const std::string request = "GET " + path + " HTTP/1.1\r\nHost: " + std::string(authority) +
                                "\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: " +
                                loa::core::base64::encode(nonce) + "\r\nSec-WebSocket-Version: 13\r\n\r\n";
    auto [wec, wn] = co_await asio::async_write(impl->socket, asio::buffer(request), use_tuple);
    if (wec) co_return std::unexpected(BrowserError::cdp("websocket handshake: " + wec.message()));
    std::string response;
    std::array<char, 4096> buf{};
    std::size_t end = std::string::npos;
    while ((end = response.find("\r\n\r\n")) == std::string::npos) {
        auto [ec, n] = co_await impl->socket.async_read_some(asio::buffer(buf), use_tuple);
        if (ec) co_return std::unexpected(BrowserError::cdp("websocket handshake: " + ec.message()));
        response.append(buf.data(), n);
        if (response.size() > 64 * 1024) co_return std::unexpected(BrowserError::cdp("websocket handshake too long"));
    }
    const auto status_end = response.find("\r\n");
    const auto status = response.substr(0, status_end);
    if (status.find(" 101") == std::string::npos) {
        co_return std::unexpected(BrowserError::cdp("websocket handshake refused: " + status));
    }
    impl->pending_bytes = response.substr(end + 4);

    asio::co_spawn(impl->strand, impl->reader(), asio::detached);
    asio::co_spawn(impl->strand, impl->writer(), asio::detached);
    co_return std::shared_ptr<Connection>(new Connection(std::move(impl)));
}

asio::awaitable<Result<Value>> Connection::call(std::string method, Value params, std::string session,
                                                std::chrono::milliseconds timeout) {
    auto executor = co_await asio::this_coro::executor;
    auto reply = std::make_shared<Reply>(executor);
    const auto id = impl_->enqueue_call(method, std::move(params), session, reply);
    asio::steady_timer timer(executor, timeout);
    auto done = co_await (reply->signal.async_receive(use_tuple) || timer.async_wait(use_tuple));
    if (done.index() == 1) {
        std::lock_guard lock(impl_->mutex);
        impl_->pending.erase(id);
        if (!reply->result) co_return std::unexpected(BrowserError::cdp("request timed out: " + method));
    }
    std::lock_guard lock(impl_->mutex);
    if (!reply->result) co_return std::unexpected(BrowserError::cdp("no reply to " + method));
    co_return std::move(*reply->result);
}

void Connection::send(std::string method, Value params, std::string session) {
    impl_->enqueue_call(method, std::move(params), session, nullptr);
}

std::uint64_t Connection::subscribe(std::string session, std::string method, EventHandler handler) {
    const auto token = impl_->next_token.fetch_add(1) + 1;
    std::lock_guard lock(impl_->mutex);
    impl_->subscriptions.push_back(
        {token, std::move(session), std::move(method), std::make_shared<EventHandler>(std::move(handler))});
    return token;
}

void Connection::unsubscribe(std::uint64_t token) {
    std::lock_guard lock(impl_->mutex);
    std::erase_if(impl_->subscriptions, [&](const Subscription& s) { return s.token == token; });
}

bool Connection::closed() const { return impl_->closed.load(); }

void Connection::close() { impl_->shutdown(); }

std::optional<std::vector<std::uint8_t>> base64_decode(std::string_view text) {
    auto value = [](char c) -> int {
        if (c >= 'A' && c <= 'Z') return c - 'A';
        if (c >= 'a' && c <= 'z') return c - 'a' + 26;
        if (c >= '0' && c <= '9') return c - '0' + 52;
        if (c == '+') return 62;
        if (c == '/') return 63;
        return -1;
    };
    while (!text.empty() && text.back() == '=') text.remove_suffix(1);
    if (text.size() % 4 == 1) return std::nullopt;
    std::vector<std::uint8_t> out;
    out.reserve(text.size() * 3 / 4);
    std::uint32_t acc = 0;
    int bits = 0;
    for (char c : text) {
        const int v = value(c);
        if (v < 0) return std::nullopt;
        acc = (acc << 6) | std::uint32_t(v);
        bits += 6;
        if (bits >= 8) {
            bits -= 8;
            out.push_back(std::uint8_t((acc >> bits) & 0xff));
        }
    }
    return out;
}

std::optional<std::pair<std::uint32_t, std::uint32_t>> jpeg_size(std::span<const std::uint8_t> jpeg) {
    if (jpeg.size() < 4 || jpeg[0] != 0xff || jpeg[1] != 0xd8) return std::nullopt;
    std::size_t i = 2;
    while (i + 4 <= jpeg.size()) {
        if (jpeg[i] != 0xff) return std::nullopt;
        const auto marker = jpeg[i + 1];
        if (marker == 0xff) {
            ++i;
            continue;
        }
        if (marker == 0xd8 || marker == 0x01 || (marker >= 0xd0 && marker <= 0xd7)) {
            i += 2;
            continue;
        }
        const std::size_t len = std::size_t(jpeg[i + 2]) << 8 | jpeg[i + 3];
        const bool sof = marker >= 0xc0 && marker <= 0xcf && marker != 0xc4 && marker != 0xc8 && marker != 0xcc;
        if (sof) {
            if (i + 9 > jpeg.size()) return std::nullopt;
            const std::uint32_t height = std::uint32_t(jpeg[i + 5]) << 8 | jpeg[i + 6];
            const std::uint32_t width = std::uint32_t(jpeg[i + 7]) << 8 | jpeg[i + 8];
            return std::pair{width, height};
        }
        if (marker == 0xd9 || marker == 0xda) return std::nullopt;
        i += 2 + len;
    }
    return std::nullopt;
}

} // namespace loa::surfaces::browser::cdp
