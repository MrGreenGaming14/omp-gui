#include "loa/iroh_ffi/iroh.hpp"

#include <irohnet.h>

#include <algorithm>
#include <cstring>
#include <memory>
#include <utility>

namespace loa::iroh_ffi {
namespace {

Error fail(EndpointResult_t code, std::string message) {
    return Error{static_cast<std::uint8_t>(code), std::move(message)};
}

slice_ref_uint8_t bytes(std::string_view s) {
    return {reinterpret_cast<const std::uint8_t*>(s.data()), s.size()};
}

slice_ref_uint8_t bytes(std::span<const std::uint8_t> s) { return {s.data(), s.size()}; }

EndpointId to_id(const PublicKey_t& key) {
    EndpointId id;
    std::memcpy(id.data(), key.key.idx, id.size());
    return id;
}

PublicKey_t to_key(const EndpointId& id) {
    PublicKey_t key;
    std::memcpy(key.key.idx, id.data(), id.size());
    return key;
}

std::string take_string(char* s) {
    std::string out = s ? s : "";
    if (s) rust_free_string(s);
    return out;
}

SecretKey_t* secret_from_seed(const Seed& seed) {
    return secret_key_from_bytes({seed.data(), seed.size()});
}

// Handles are stored as void* in the public header to keep irohnet.h private.
template <class T>
T** h(void*& p) { return reinterpret_cast<T**>(&p); }
template <class T>
T* const* h(void* const& p) { return reinterpret_cast<T* const*>(&p); }

}  // namespace

EndpointId endpoint_id_from_seed(const Seed& seed) {
    SecretKey_t* secret = secret_from_seed(seed);
    EndpointId id = to_id(secret_key_public(secret));
    secret_key_free(secret);
    return id;
}

std::string endpoint_id_to_string(const EndpointId& id) {
    PublicKey_t key = to_key(id);
    return take_string(public_key_as_base32(&key));
}

// ---- Stream ----

Stream::Stream(Stream&& o) noexcept
    : send_(std::exchange(o.send_, nullptr)), recv_(std::exchange(o.recv_, nullptr)) {}

Stream& Stream::operator=(Stream&& o) noexcept {
    if (this != &o) {
        this->~Stream();
        send_ = std::exchange(o.send_, nullptr);
        recv_ = std::exchange(o.recv_, nullptr);
    }
    return *this;
}

Stream::~Stream() {
    if (send_) send_stream_free(static_cast<SendStream_t*>(std::exchange(send_, nullptr)));
    if (recv_) recv_stream_free(static_cast<RecvStream_t*>(std::exchange(recv_, nullptr)));
}

Result<void> Stream::write(std::span<const std::uint8_t> data) {
    if (!send_) return std::unexpected(fail(ENDPOINT_RESULT_SEND_ERROR, "send half finished"));
    if (auto r = send_stream_write(h<SendStream_t>(send_), bytes(data)); r != ENDPOINT_RESULT_OK)
        return std::unexpected(fail(r, "stream write failed"));
    return {};
}

Result<void> Stream::write(std::string_view data) {
    return write(std::span(reinterpret_cast<const std::uint8_t*>(data.data()), data.size()));
}

Result<void> Stream::finish() {
    if (!send_) return std::unexpected(fail(ENDPOINT_RESULT_SEND_ERROR, "send half finished"));
    if (auto r = send_stream_finish(static_cast<SendStream_t*>(std::exchange(send_, nullptr))); r != ENDPOINT_RESULT_OK)
        return std::unexpected(fail(r, "stream finish failed"));
    return {};
}

Result<std::size_t> Stream::read(std::span<std::uint8_t> buf) {
    std::int64_t n = recv_stream_read(h<RecvStream_t>(recv_), {buf.data(), buf.size()});
    if (n < 0) return std::unexpected(fail(ENDPOINT_RESULT_READ_ERROR, "stream read failed"));
    return static_cast<std::size_t>(n);
}

Result<std::vector<std::uint8_t>> Stream::read_to_end(std::size_t limit, std::chrono::milliseconds timeout) {
    Vec_uint8_t buf = rust_buffer_alloc(0);
    auto r = recv_stream_read_to_end_timeout(h<RecvStream_t>(recv_), &buf, limit, static_cast<std::uint64_t>(timeout.count()));
    std::vector<std::uint8_t> out(buf.ptr, buf.ptr + buf.len);
    rust_buffer_free(buf);
    if (r != ENDPOINT_RESULT_OK) return std::unexpected(fail(r, "stream read_to_end failed"));
    return out;
}

// ---- Connection ----

Connection::Connection(Connection&& o) noexcept : conn_(std::exchange(o.conn_, nullptr)) {}

Connection& Connection::operator=(Connection&& o) noexcept {
    if (this != &o) {
        this->~Connection();
        conn_ = std::exchange(o.conn_, nullptr);
    }
    return *this;
}

Connection::~Connection() {
    if (conn_) connection_free(static_cast<Connection_t*>(std::exchange(conn_, nullptr)));
}

EndpointId Connection::remote_id() const { return to_id(connection_remote_id(h<Connection_t>(conn_))); }

std::vector<std::uint8_t> Connection::alpn() const {
    Vec_uint8_t v = connection_alpn(h<Connection_t>(conn_));
    std::vector<std::uint8_t> out(v.ptr, v.ptr + v.len);
    rust_buffer_free(v);
    return out;
}

bool Connection::selected_path_is_relay() const { return connection_selected_path_is_relay(h<Connection_t>(conn_)); }

Result<Stream> Connection::open_bi() {
    Stream s(send_stream_default(), recv_stream_default());
    if (auto r = connection_open_bi(h<Connection_t>(conn_), h<SendStream_t>(s.send_), h<RecvStream_t>(s.recv_)); r != ENDPOINT_RESULT_OK)
        return std::unexpected(fail(r, "open_bi failed"));
    return s;
}

Result<Stream> Connection::accept_bi() {
    Stream s(send_stream_default(), recv_stream_default());
    if (auto r = connection_accept_bi(h<Connection_t>(conn_), h<SendStream_t>(s.send_), h<RecvStream_t>(s.recv_)); r != ENDPOINT_RESULT_OK)
        return std::unexpected(fail(r, "accept_bi failed"));
    return s;
}

void Connection::close(std::uint32_t code, std::string_view reason) {
    connection_close_with(h<Connection_t>(conn_), code, bytes(reason));
}

std::string Connection::closed() { return take_string(connection_closed_wait(h<Connection_t>(conn_))); }

// ---- Endpoint ----

Result<Endpoint> Endpoint::bind(const EndpointOptions& options) {
    EndpointConfig_t config = endpoint_config_default();
    endpoint_config_add_secret_key(&config, secret_from_seed(options.seed));
    for (const auto& alpn : options.alpns) endpoint_config_add_alpn(&config, bytes(alpn));
    for (const auto& url : options.relay_urls) {
        if (!endpoint_config_add_relay_url(&config, url.c_str())) {
            endpoint_config_free(config);
            return std::unexpected(fail(ENDPOINT_RESULT_BIND_ERROR, "invalid relay url: " + url));
        }
    }
    endpoint_config_clear_ip_transports(&config, options.clear_ip_transports);

    SocketAddrV4_t* v4 = socket_addr_v4_default();
    if (socket_addr_v4_from_string(options.ipv4_bind.c_str(), &v4) != ADDR_RESULT_OK) {
        socket_addr_v4_free(v4);
        endpoint_config_free(config);
        return std::unexpected(fail(ENDPOINT_RESULT_BIND_ERROR, "invalid ipv4 bind address: " + options.ipv4_bind));
    }

    Endpoint_t* ep = endpoint_default();
    auto r = endpoint_bind(&config, v4, nullptr, &ep);
    EndpointId id = to_id(secret_key_public(config.secret_key));
    endpoint_config_free(config);
    if (r != ENDPOINT_RESULT_OK) {
        endpoint_free(ep);
        return std::unexpected(fail(r, "endpoint bind failed"));
    }
    return Endpoint(ep, id);
}

Endpoint::Endpoint(Endpoint&& o) noexcept : ep_(std::exchange(o.ep_, nullptr)), id_(o.id_) {}

Endpoint& Endpoint::operator=(Endpoint&& o) noexcept {
    if (this != &o) {
        this->~Endpoint();
        ep_ = std::exchange(o.ep_, nullptr);
        id_ = o.id_;
    }
    return *this;
}

Endpoint::~Endpoint() {
    if (ep_) endpoint_free(static_cast<Endpoint_t*>(std::exchange(ep_, nullptr)));
}

std::vector<std::string> Endpoint::ip_addrs() const {
    std::vector<std::string> out;
    EndpointAddr_t addr = endpoint_addr_default();
    if (endpoint_addr(h<Endpoint_t>(ep_), &addr) == ENDPOINT_RESULT_OK) {
        for (std::size_t i = 0; i < addr.ip_addrs.len; ++i)
            out.push_back(take_string(socket_addr_as_str(endpoint_addr_ip_addrs_nth(&addr, i))));
    }
    endpoint_addr_free(addr);
    return out;
}

Result<Connection> Endpoint::connect(const EndpointId& peer, std::span<const std::string> hints, std::string_view alpn,
                                     std::chrono::milliseconds timeout) {
    EndpointAddr_t addr = endpoint_addr_new(to_key(peer));
    for (const auto& hint : hints) {
        SocketAddr_t* sa = socket_addr_default();
        if (socket_addr_from_string(hint.c_str(), &sa) != ADDR_RESULT_OK) {
            socket_addr_free(sa);
            endpoint_addr_free(addr);
            return std::unexpected(fail(ENDPOINT_RESULT_CONNECT_ERROR, "invalid address hint: " + hint));
        }
        endpoint_addr_add_ip_addrs(&addr, sa);
    }
    Connection conn(connection_default());
    // endpoint_connect_timeout consumes `addr`.
    if (auto r = endpoint_connect_timeout(h<Endpoint_t>(ep_), bytes(alpn), addr,
                                          static_cast<std::uint64_t>(timeout.count()), h<Connection_t>(conn.conn_)); r != ENDPOINT_RESULT_OK)
        return std::unexpected(fail(r, "connect failed"));
    return conn;
}

Result<Connection> Endpoint::accept() {
    Connection conn(connection_default());
    Vec_uint8_t alpn = rust_buffer_alloc(0);
    auto r = endpoint_accept_any(h<Endpoint_t>(ep_), &alpn, h<Connection_t>(conn.conn_));
    rust_buffer_free(alpn);
    if (r != ENDPOINT_RESULT_OK) return std::unexpected(fail(r, "accept failed"));
    return conn;
}

void Endpoint::close() {
    if (ep_) endpoint_close(static_cast<Endpoint_t*>(std::exchange(ep_, nullptr)));
}

}  // namespace loa::iroh_ffi
