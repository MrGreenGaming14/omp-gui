#pragma once

// In-house yamux, wire-compatible with Rust `yamux` 0.14 under its default
// `Config`: 12-byte big-endian frame headers, per-stream credit windows of
// 256 KiB, ping answers and go-away. Every call blocks and is thread-safe.

#include <array>
#include <cstddef>
#include <cstdint>
#include <memory>
#include <span>
#include <string>

#include "loa/p2p/transport.hpp"

namespace loa::p2p::yamux {

enum class Tag : std::uint8_t { data = 0, window_update = 1, ping = 2, go_away = 3 };

inline constexpr std::uint16_t SYN = 1;
inline constexpr std::uint16_t ACK = 2;
inline constexpr std::uint16_t FIN = 4;
inline constexpr std::uint16_t RST = 8;

inline constexpr std::size_t HEADER_SIZE = 12;
// The initial window of every stream, per the yamux specification.
inline constexpr std::uint32_t DEFAULT_CREDIT = 256 * 1024;
// The largest data body a peer may send in one frame.
inline constexpr std::uint32_t MAX_FRAME_BODY = 1024 * 1024;

// Go-away codes.
inline constexpr std::uint32_t GO_AWAY_NORMAL = 0;
inline constexpr std::uint32_t GO_AWAY_PROTOCOL_ERROR = 1;
inline constexpr std::uint32_t GO_AWAY_INTERNAL_ERROR = 2;

struct Header {
    std::uint8_t version = 0;
    Tag tag = Tag::data;
    std::uint16_t flags = 0;
    std::uint32_t stream_id = 0;
    // Body length for data; credit for window updates; opaque id for pings; code for go-away.
    std::uint32_t length = 0;

    static Header data(std::uint32_t id, std::uint32_t len, std::uint16_t flags = 0) { return {0, Tag::data, flags, id, len}; }
    static Header window_update(std::uint32_t id, std::uint32_t credit, std::uint16_t flags = 0) {
        return {0, Tag::window_update, flags, id, credit};
    }
    static Header ping(std::uint32_t id, std::uint16_t flags) { return {0, Tag::ping, flags, 0, id}; }
    static Header go_away(std::uint32_t code) { return {0, Tag::go_away, 0, 0, code}; }

    friend bool operator==(const Header&, const Header&) = default;
};

[[nodiscard]] std::array<std::uint8_t, HEADER_SIZE> encode(const Header& header);
// Refuses versions other than 0 and unknown tags (`decode` errors).
[[nodiscard]] Result<Header> decode(std::span<const std::uint8_t, HEADER_SIZE> bytes);

enum class Mode { client, server };

// Rust `yamux::Config::default()`.
struct Config {
    std::size_t max_num_streams = 512;
    std::size_t split_send_size = 16 * 1024;
};

// The byte stream the mux runs over.
class Socket {
  public:
    virtual ~Socket() = default;
    // Up to `buf.size()` bytes; 0 at end of stream.
    virtual Result<std::size_t> read(std::span<std::uint8_t> buf) = 0;
    virtual Result<> write_all(std::span<const std::uint8_t> data) = 0;
    // Ends both directions and wakes a blocked `read`.
    virtual void shutdown() = 0;
};

// One multiplexed connection. Client streams have odd ids, server streams even.
class Session {
  public:
    static std::shared_ptr<Session> start(std::shared_ptr<Socket> socket, Mode mode, Config config = {});
    ~Session();

    // Opens an outbound stream (announced with SYN right away).
    Result<Stream> open();
    // The next stream the peer opened; fails once the connection ended.
    Result<Stream> accept();
    // Sends go-away, then shuts the socket; `reason` becomes `closed()`.
    void close(std::string reason);
    // Blocks until the connection ended; why.
    std::string closed();

    struct Impl;

  private:
    explicit Session(std::shared_ptr<Impl> impl);
    std::shared_ptr<Impl> impl_;
};

} // namespace loa::p2p::yamux
