#pragma once

// The hosts the agent can reach, as the app's config resolved them.

#include <cstdint>
#include <filesystem>
#include <map>
#include <optional>
#include <shared_mutex>
#include <string>
#include <vector>

#include "loa/remote/limits.hpp"

namespace loa::remote {

// Which `ssh-agent` socket authenticates a hop.
struct AgentSocket {
    enum class Kind {
        // The socket the app was given (normally `SSH_AUTH_SOCK`).
        default_socket,
        path,
        // `IdentityAgent none`: no way to authenticate.
        disabled,
    };
    Kind kind = Kind::default_socket;
    std::filesystem::path path;

    [[nodiscard]] static AgentSocket at(std::filesystem::path p) { return {Kind::path, std::move(p)}; }
    [[nodiscard]] static AgentSocket none() { return {Kind::disabled, {}}; }
    friend bool operator==(const AgentSocket&, const AgentSocket&) = default;
};

// One SSH hop: where to connect and as whom.
struct Endpoint {
    std::string alias;
    std::string hostname;
    std::uint16_t port = 22;
    std::string user;
    AgentSocket agent;
    friend bool operator==(const Endpoint&, const Endpoint&) = default;
};

// A host the agent can reach.
struct Host {
    std::string name;
    Endpoint target;
    // ProxyJump hops in connection order.
    std::vector<Endpoint> jumps;
    HostLimits limits;

    // Every hop, jumps first, the target last.
    [[nodiscard]] std::vector<Endpoint> hops() const;
    friend bool operator==(const Host&, const Host&) = default;
};

// The resolved Remote hosts, by name. Thread-safe.
class HostRegistry {
  public:
    void insert(Host host);
    // Replaces the whole list, e.g. after the config changed.
    void replace_all(std::vector<Host> hosts);
    [[nodiscard]] std::optional<Host> get(const std::string& name) const;

  private:
    mutable std::shared_mutex mu_;
    std::map<std::string, Host> hosts_;
};

} // namespace loa::remote
