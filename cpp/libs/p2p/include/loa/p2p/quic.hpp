#pragma once

// The iroh QUIC adapter, with an explicit relay policy (Rust `transport::quic`).

#include <array>
#include <cstdint>
#include <memory>
#include <string>

#include "loa/p2p/transport.hpp"

namespace loa::p2p {

// The default UDP port of a node.
inline constexpr std::uint16_t DEFAULT_PORT = 31415;

// Only explicitly configured relays may carry peer traffic.
struct RelayConfig {
    // Empty = `Off`; otherwise `SelfHosted(url)`.
    std::string self_hosted;

    static RelayConfig off() { return {}; }
    static RelayConfig self_hosted_at(std::string url) { return {std::move(url)}; }
    friend bool operator==(const RelayConfig&, const RelayConfig&) = default;
};

// One UDP endpoint serving every label of its router.
class Quic final : public Transport {
  public:
    // `seed` is the node secret; `bind` an IPv4 "ip:port" (port `DEFAULT_PORT` in the app).
    static Result<std::shared_ptr<Quic>> bind(const std::array<std::uint8_t, 32>& seed, const SocketAddr& bind,
                                              const RelayConfig& relay, Router router);
    ~Quic() override;

    Result<Connection> dial(const NodeId& peer, std::span<const SocketAddr> hints, std::string_view alpn) override;
    // Unsupported: QUIC needs the node's key.
    Result<Connection> dial_any(const SocketAddr& addr, std::string_view alpn) override;
    [[nodiscard]] std::vector<SocketAddr> local_addrs() const override;
    void close() override;

    struct Impl;

  private:
    explicit Quic(std::unique_ptr<Impl> impl);
    std::unique_ptr<Impl> impl_;
};

} // namespace loa::p2p
