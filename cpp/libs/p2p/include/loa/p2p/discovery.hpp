#pragma once

// Finding nodes: the `Discoverer` port with mDNS and tailnet adapters (Rust
// `loa_p2p::discovery`). A discovery is a hint; the transport proves the key
// on every connection.

#include <cstdint>
#include <filesystem>
#include <memory>
#include <optional>
#include <span>
#include <string>
#include <utility>
#include <vector>

#include "loa/p2p/error.hpp"
#include "loa/p2p/transport.hpp"

namespace loa::p2p {

// Whether an optional discovery source works here.
struct Availability {
    // `off`: turned off, or its tool isn't installed.
    enum class State { off, active, failed };
    State state = State::off;
    std::string detail;
    friend bool operator==(const Availability&, const Availability&) = default;
};

// How an unsaved node became known.
enum class NearbySource { mdns, tailscale, incoming };

// A node seen but not saved as a peer.
struct NearbyPeer {
    NodeId key;
    std::optional<std::string> name;
    std::vector<SocketAddr> addresses;
    NearbySource source = NearbySource::mdns;
    std::int64_t seen_ms = 0;
    std::optional<std::string> app_version;
    // The protocol labels it announced, when it connected or answered a probe.
    std::vector<std::string> protocols;

    // Seen now.
    static NearbyPeer make(NodeId key, NearbySource source, std::optional<std::string> name,
                           std::vector<SocketAddr> addresses);
    // Folds a later sighting in: mDNS replaces addresses, the others add to them.
    void merge(NearbyPeer seen);

    friend bool operator==(const NearbyPeer&, const NearbyPeer&) = default;
};

// `found` first, then the `old` ones it lacks unless `replace`; `old` when `found` is empty.
[[nodiscard]] std::vector<SocketAddr> merge_addresses(std::span<const SocketAddr> old, std::vector<SocketAddr> found,
                                                      bool replace);

// Port: a source of nearby nodes.
class Discoverer {
  public:
    virtual ~Discoverer() = default;
    // Blocks for the next node found; `nullopt` once the source is done.
    virtual std::optional<NearbyPeer> next() = 0;
    [[nodiscard]] virtual Availability availability() const = 0;
};

// ---- mDNS ----

inline constexpr std::string_view MDNS_SERVICE_TYPE = "_loa._udp.local.";

// One DNS-SD service instance: what is announced and what a browse resolves.
struct ServiceRecord {
    std::string ty_domain;  // "_loa._udp.local."
    std::string fullname;   // "<instance>.<ty_domain>"
    std::string hostname;   // "<instance>.local."
    std::uint16_t port = 0; // SRV port (UDP)
    std::vector<std::pair<std::string, std::string>> txt;
    // Resolved IPs in display form ("1.2.3.4", "fe80::1").
    std::vector<std::string> addresses;

    [[nodiscard]] const std::string* property(std::string_view key) const;
};

// What a node announces about itself.
struct Announce {
    NodeId node;
    std::uint16_t udp_port = 0;
    std::uint16_t tcp_port = 0;
    std::string name;

    // The service record, without starting multicast; zero ports are refused.
    [[nodiscard]] Result<ServiceRecord> info() const;
    friend bool operator==(const Announce&, const Announce&) = default;
};

// A resolved announcement as a nearby node; `nullopt` when incomplete.
[[nodiscard]] std::optional<NearbyPeer> parse_service(const ServiceRecord& service);

// Announces this node and browses for others on the LAN (mjansson/mdns).
// Destruction unregisters (goodbye) and stops the socket thread.
class Mdns final : public Discoverer {
  public:
    static Result<std::unique_ptr<Mdns>> start(const Announce& announce);
    ~Mdns() override;

    std::optional<NearbyPeer> next() override;
    [[nodiscard]] Availability availability() const override;
    // Stops browsing: a blocked `next` returns `nullopt` soon after. The goodbye goes out on destruction.
    void stop();

    struct Impl;

  private:
    explicit Mdns(std::unique_ptr<Impl> impl);
    std::unique_ptr<Impl> impl_;
};

// ---- tailnet ----

// Port: proves which node answers at an address; a bare TCP connect never does.
class NodeProbe {
  public:
    virtual ~NodeProbe() = default;
    virtual std::optional<NodeId> probe(const SocketAddr& address) = 0;
};

// The DNS names of the peers not known to be offline; malformed names are skipped.
[[nodiscard]] Result<std::vector<std::string>> parse_tailscale_status(std::string_view json);

// The first address of each candidate that the probe proves, as a nearby node.
[[nodiscard]] std::vector<NearbyPeer>
admit_candidates(const std::vector<std::pair<std::string, std::vector<SocketAddr>>>& candidates, NodeProbe& probe);

// One scan of the tailnet (`tailscale status --json`), yielding the machines that answered.
class Tailnet final : public Discoverer {
  public:
    Tailnet(std::filesystem::path executable, std::uint16_t port, std::shared_ptr<NodeProbe> probe);

    std::optional<NearbyPeer> next() override;
    [[nodiscard]] Availability availability() const override;

  private:
    Result<std::vector<NearbyPeer>> scan() const;

    std::filesystem::path executable_;
    std::uint16_t port_;
    std::shared_ptr<NodeProbe> probe_;
    std::optional<Result<std::vector<NearbyPeer>>> found_;
};

} // namespace loa::p2p
