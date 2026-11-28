#pragma once

// The TCP adapter (Rust `transport::tcp`): mutually authenticated TLS 1.3 with
// the node's Ed25519 key as a self-signed `localhost` certificate, and yamux
// streams. Every call blocks and is thread-safe.

#include <array>
#include <cstddef>
#include <cstdint>
#include <memory>
#include <span>

#include "loa/p2p/transport.hpp"

namespace loa::p2p {

// The first byte of every yamux stream: an application stream (1 was datagrams).
inline constexpr std::uint8_t STREAM_FRAME = 0;
// Accepted streams waiting for `accept_stream`.
inline constexpr std::size_t TCP_QUEUE = 64;
// The DER SubjectPublicKeyInfo of an Ed25519 key, before its 32 key bytes.
inline constexpr std::array<std::uint8_t, 12> ED25519_SPKI_PREFIX{0x30, 0x2a, 0x30, 0x05, 0x06, 0x03,
                                                                  0x2b, 0x65, 0x70, 0x03, 0x21, 0x00};

// The DER self-signed `localhost` certificate of the node key `seed`.
[[nodiscard]] Result<Bytes> node_certificate(const std::array<std::uint8_t, 32>& seed);
// The node key of a DER Ed25519 leaf certificate (`invalid_key` otherwise).
[[nodiscard]] Result<NodeId> certificate_id(std::span<const std::uint8_t> der);

// A TCP listener and dialer keyed by the same seed as the QUIC endpoint.
class Tcp final : public Transport {
  public:
    // Listens on `bind` ("ip:port"; port `DEFAULT_PORT` in the app) and serves every label of `router`.
    static Result<std::shared_ptr<Tcp>> bind(const std::array<std::uint8_t, 32>& seed, const SocketAddr& bind,
                                             Router router);
    ~Tcp() override;

    Result<Connection> dial(const NodeId& peer, std::span<const SocketAddr> hints, std::string_view alpn) override;
    Result<Connection> dial_any(const SocketAddr& addr, std::string_view alpn) override;
    [[nodiscard]] std::vector<SocketAddr> local_addrs() const override;
    void close() override;

    struct Impl;

  private:
    explicit Tcp(std::shared_ptr<Impl> impl);
    std::shared_ptr<Impl> impl_;
};

} // namespace loa::p2p
