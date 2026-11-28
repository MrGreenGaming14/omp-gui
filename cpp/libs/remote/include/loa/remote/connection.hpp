#pragma once

// SSH connections over libssh: ProxyJump chains, host key checks through `KnownHosts` and the
// `HostKeyDecider` port, agent authentication, and one pooled connection per host.

#include <chrono>
#include <condition_variable>
#include <filesystem>
#include <map>
#include <memory>
#include <mutex>
#include <optional>
#include <string>
#include <thread>

#include "loa/remote/cancel.hpp"
#include "loa/remote/error.hpp"
#include "loa/remote/hosts.hpp"
#include "loa/remote/known_hosts.hpp"
#include "loa/remote/ports.hpp"

namespace loa::remote {

namespace detail {
struct Session;
struct PoolSlot;
} // namespace detail

// Connection settings the app passes in.
struct ConnectorConfig {
    // Agent socket for hops without `IdentityAgent` (normally `SSH_AUTH_SOCK`).
    std::optional<std::filesystem::path> default_agent;
    std::chrono::milliseconds connect_timeout{20'000};
    // Keepalive interval; a connection that misses a keepalive is redialled on next use.
    std::chrono::milliseconds keepalive{30'000};

    // Defaults with the agent socket taken from `SSH_AUTH_SOCK` (on Windows falling back to the
    // OpenSSH agent pipe).
    [[nodiscard]] static ConnectorConfig from_env();
    [[nodiscard]] static ConnectorConfig with_agent(std::optional<std::filesystem::path> default_agent);
};

// Dials hosts and keeps one connection per host for reuse. Thread-safe.
class Connector {
  public:
    Connector(std::shared_ptr<KnownHosts> known, std::shared_ptr<HostKeyDecider> decider, ConnectorConfig config);
    ~Connector();
    Connector(const Connector&) = delete;
    Connector& operator=(const Connector&) = delete;

    // Closes the pooled connection to `host`, if any.
    void disconnect(const std::string& host);
    // A live connection to `host`: the pooled one unless it closed or the host's hops changed.
    // `cancel` reaches the host key decider.
    [[nodiscard]] Result<std::shared_ptr<detail::Session>> session(const Host& host, const CancelToken& cancel);

  private:
    void keepalive_loop(std::stop_token stop);

    std::shared_ptr<KnownHosts> known_;
    std::shared_ptr<HostKeyDecider> decider_;
    ConnectorConfig config_;
    std::mutex mu_;
    std::map<std::string, std::shared_ptr<detail::PoolSlot>> pool_;
    std::condition_variable_any wake_;
    std::jthread keepalive_;
};

} // namespace loa::remote
