#pragma once

// `omp/control/1` (Rust `protocols::control`): both sides send a hello, then
// the dialer sends one request (presence, grant delivery, renewal or
// revocation) and the listener answers it. Any node may open it; a connection
// never makes it a peer.

#include <chrono>
#include <memory>
#include <optional>

#include "loa/p2p/discovery.hpp"
#include "loa/p2p/protocols/host.hpp"
#include "loa/trust/renew.hpp"

namespace loa::p2p::control {

// Whether a peer's hello is usable: an app version, and named protocols with versions above 0.
[[nodiscard]] bool valid_hello(const wire::Hello& hello);

// The dialer's side of one request on an open connection.
Result<wire::Control> call(NodeHost& host, const Connection& conn, wire::Control request);
// Dials `peer` on `omp/control/1`, makes one request and closes.
Result<wire::Control> call_peer(NodeHost& host, const NodeId& peer, wire::Control request);

// This node is online now.
[[nodiscard]] wire::Control presence();

// The listener's side of one control stream.
Result<> serve(NodeHost& host, const Connection& conn, Stream stream);

// A tailnet probe: the control handshake over TCP proves the node key.
class Probe final : public NodeProbe {
  public:
    explicit Probe(std::shared_ptr<NodeHost> host, std::chrono::milliseconds wait = std::chrono::seconds(3))
        : host_(std::move(host)), wait_(wait) {}
    std::optional<NodeId> probe(const SocketAddr& address) override;

  private:
    std::shared_ptr<NodeHost> host_;
    std::chrono::milliseconds wait_;
};

// Renews held grants by asking their issuer over control.
class RenewVia final : public trust::RenewTransport {
  public:
    explicit RenewVia(std::shared_ptr<NodeHost> host) : host_(std::move(host)) {}
    [[nodiscard]] trust::Awaitable<std::expected<std::vector<std::uint8_t>, trust::RenewFailure>>
    renew(trust::PublicKey issuer, trust::GrantId grant) const override;

  private:
    std::shared_ptr<NodeHost> host_;
};

} // namespace loa::p2p::control
