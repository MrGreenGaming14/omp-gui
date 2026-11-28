#pragma once

// Authenticated connections: the `Transport` port, the ALPN `Router` and the
// `FallbackDialer` (Rust `loa_p2p::transport`).
//
// The iroh FFI blocks the calling thread, so every call here blocks too and
// is thread-safe; async callers run them off the executor (e.g. `asio::post`
// to a blocking pool) the way the Rust code awaits them.

#include <chrono>
#include <cstddef>
#include <cstdint>
#include <map>
#include <memory>
#include <span>
#include <string>
#include <string_view>
#include <utility>
#include <vector>

#include "loa/p2p/error.hpp"
#include "loa/trust/key.hpp"

namespace loa::p2p {

// The public ed25519 node key, the network address.
using NodeId = loa::trust::PublicKey;
// A socket address in Rust `SocketAddr` display form: `1.2.3.4:5` or `[::1]:5`.
using SocketAddr = std::string;
using Bytes = std::vector<std::uint8_t>;

// What carries a connection.
enum class TransportKind { quic, relay, tcp };

// The writing half of a stream; `shutdown` ends the peer's reads.
class SendHalf {
  public:
    virtual ~SendHalf() = default;
    virtual Result<> write_all(std::span<const std::uint8_t> data) = 0;
    virtual Result<> shutdown() = 0;
};

// The reading half of a stream.
class RecvHalf {
  public:
    virtual ~RecvHalf() = default;
    // Up to `buf.size()` bytes; 0 only at a clean end of stream.
    virtual Result<std::size_t> read(std::span<std::uint8_t> buf) = 0;
    // Exactly `buf.size()` bytes; an early end is an `io` error.
    Result<> read_exact(std::span<std::uint8_t> buf);
};

// One ordered, reliable bidirectional stream.
struct Stream {
    std::unique_ptr<SendHalf> send;
    std::unique_ptr<RecvHalf> recv;
};

// Two connected in-memory streams (Rust `tokio::io::duplex`), each direction
// buffering at most `capacity` bytes.
[[nodiscard]] std::pair<Stream, Stream> duplex(std::size_t capacity);

// A connection to one peer on one protocol label.
class PeerConnection {
  public:
    virtual ~PeerConnection() = default;
    // The peer's key, proven during the handshake.
    [[nodiscard]] virtual NodeId remote() const = 0;
    [[nodiscard]] virtual Bytes alpn() const = 0;
    [[nodiscard]] virtual TransportKind kind() const = 0;
    virtual Result<Stream> open_stream() = 0;
    virtual Result<Stream> accept_stream() = 0;
    virtual void close(std::uint32_t code, std::string_view reason) = 0;
    // Blocks until either side closed; the reason.
    virtual std::string closed() = 0;
};

using Connection = std::shared_ptr<PeerConnection>;

// Port: one way to reach peers.
class Transport {
  public:
    virtual ~Transport() = default;
    // Dials `peer` on its address hints; the handshake must prove its key.
    virtual Result<Connection> dial(const NodeId& peer, std::span<const SocketAddr> hints, std::string_view alpn) = 0;
    // Dials whatever node answers at `addr`, for probing addresses learned without a key.
    virtual Result<Connection> dial_any(const SocketAddr& addr, std::string_view alpn) = 0;
    [[nodiscard]] virtual std::vector<SocketAddr> local_addrs() const = 0;
    // Stops accepting connections.
    virtual void close() = 0;
};

// Serves one protocol on accepted connections, for the connection's life.
class ProtocolHandler {
  public:
    virtual ~ProtocolHandler() = default;
    virtual void handle(Connection conn) = 0;
};

// Close code when the dialed node proved a different key.
inline constexpr std::uint32_t CLOSE_WRONG_PEER = 0x403;
// Close code when no handler serves the negotiated label.
inline constexpr std::uint32_t CLOSE_NO_HANDLER = 0x404;

// Handlers keyed by exact protocol label.
class Router {
  public:
    Router& handler(std::string_view alpn, std::shared_ptr<ProtocolHandler> handler);
    // The labels in byte order.
    [[nodiscard]] std::vector<std::string> alpns() const;
    // Runs the label's handler, else closes with `CLOSE_NO_HANDLER`.
    void dispatch(const Connection& conn) const;

  private:
    std::map<std::string, std::shared_ptr<ProtocolHandler>, std::less<>> handlers_;
};

// Fails unless the negotiated label is the one asked for.
Result<> check_alpn(const NodeId& peer, std::string_view asked, const Bytes* got);

// Tries each transport in order; the first gets `first_timeout`.
class FallbackDialer {
  public:
    std::vector<std::shared_ptr<Transport>> transports;
    std::chrono::milliseconds first_timeout{0};

    Result<Connection> dial(const NodeId& peer, std::span<const SocketAddr> hints, std::string_view alpn) const;
    Result<Connection> dial_any(const SocketAddr& addr, std::string_view alpn) const;
    void close() const;
};

} // namespace loa::p2p
