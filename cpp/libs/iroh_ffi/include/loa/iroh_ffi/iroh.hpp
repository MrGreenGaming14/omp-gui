// RAII wrappers over the vendored iroh-c-ffi fork (irohnet.h).
// Every call blocks the calling thread; transport policy lives in loa::net.
#pragma once

#include <array>
#include <chrono>
#include <cstddef>
#include <cstdint>
#include <expected>
#include <span>
#include <string>
#include <string_view>
#include <vector>

namespace loa::iroh_ffi {

struct Error {
    std::uint8_t code = 0;  // irohnet EndpointResult_t, 0 when not applicable
    std::string message;
};

template <class T>
using Result = std::expected<T, Error>;

using Seed = std::array<std::uint8_t, 32>;
using EndpointId = std::array<std::uint8_t, 32>;

inline constexpr std::uint16_t kDefaultPort = 31415;

/// Bind options; mirrors `loa_p2p::transport::quic::Quic::bind`.
struct EndpointOptions {
    Seed seed{};
    std::vector<std::string> alpns;
    /// Empty = relays disabled; otherwise RelayMode::custom with these URLs.
    std::vector<std::string> relay_urls;
    bool clear_ip_transports = true;
    /// UDP IPv4 bind address, "ip:port".
    std::string ipv4_bind = "0.0.0.0:31415";
};

/// Public endpoint id derived from a 32-byte seed.
EndpointId endpoint_id_from_seed(const Seed& seed);
std::string endpoint_id_to_string(const EndpointId& id);

/// One bidirectional stream (send + recv halves).
class Stream {
public:
    Stream(Stream&& other) noexcept;
    Stream& operator=(Stream&& other) noexcept;
    Stream(const Stream&) = delete;
    Stream& operator=(const Stream&) = delete;
    ~Stream();

    Result<void> write(std::span<const std::uint8_t> data);
    Result<void> write(std::string_view data);
    /// Finishes the send half; later writes fail.
    Result<void> finish();
    /// Reads up to `buf.size()` bytes; 0 means the peer finished.
    Result<std::size_t> read(std::span<std::uint8_t> buf);
    /// Reads until the peer finishes, at most `limit` bytes.
    Result<std::vector<std::uint8_t>> read_to_end(std::size_t limit, std::chrono::milliseconds timeout);

private:
    friend class Connection;
    Stream(void* send, void* recv) noexcept : send_(send), recv_(recv) {}
    void* send_ = nullptr;
    void* recv_ = nullptr;
};

class Connection {
public:
    Connection(Connection&& other) noexcept;
    Connection& operator=(Connection&& other) noexcept;
    Connection(const Connection&) = delete;
    Connection& operator=(const Connection&) = delete;
    ~Connection();

    EndpointId remote_id() const;
    std::vector<std::uint8_t> alpn() const;
    /// True if any selected path goes through a relay.
    bool selected_path_is_relay() const;
    Result<Stream> open_bi();
    Result<Stream> accept_bi();
    void close(std::uint32_t code, std::string_view reason);
    /// Blocks until closed; returns the close reason text.
    std::string closed();

private:
    friend class Endpoint;
    explicit Connection(void* c) noexcept : conn_(c) {}
    void* conn_ = nullptr;
};

class Endpoint {
public:
    static Result<Endpoint> bind(const EndpointOptions& options);

    Endpoint(Endpoint&& other) noexcept;
    Endpoint& operator=(Endpoint&& other) noexcept;
    Endpoint(const Endpoint&) = delete;
    Endpoint& operator=(const Endpoint&) = delete;
    ~Endpoint();

    EndpointId id() const { return id_; }
    /// Direct socket addresses ("ip:port") this endpoint advertises.
    std::vector<std::string> ip_addrs() const;
    /// Fails with ENDPOINT_RESULT_TIMEOUT (11) after `timeout`.
    Result<Connection> connect(const EndpointId& peer, std::span<const std::string> hints, std::string_view alpn,
                               std::chrono::milliseconds timeout = std::chrono::seconds(30));
    /// Accepts the next incoming connection on any configured ALPN.
    Result<Connection> accept();
    /// Gracefully closes; the endpoint is unusable afterwards.
    void close();

private:
    Endpoint(void* ep, EndpointId id) noexcept : ep_(ep), id_(id) {}
    void* ep_ = nullptr;
    EndpointId id_{};
};

}  // namespace loa::iroh_ffi
