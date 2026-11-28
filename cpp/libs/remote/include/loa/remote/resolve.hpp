#pragma once

// The `SshResolver` port: how a host name becomes hostname, user, port, ProxyJump and agent,
// with the OpenSSH client (`OpenSshCli`, `ssh -G`) and a fixed table (`Static`) as adapters.

#include <cstdint>
#include <filesystem>
#include <map>
#include <optional>
#include <string>
#include <string_view>
#include <vector>

#include "loa/remote/error.hpp"
#include "loa/runtime/ssh.hpp"

namespace loa::remote {

using ResolvedHost = loa::runtime::ssh::ResolvedHost;

// Resolves one hop the way `ssh [-l user] [-p port] host` would, and lists the config's aliases.
class SshResolver {
  public:
    virtual ~SshResolver() = default;
    [[nodiscard]] virtual std::vector<std::string> aliases() const = 0;
    [[nodiscard]] virtual Result<ResolvedHost> lookup(std::string_view host, std::optional<std::string_view> user,
                                                      std::optional<std::uint16_t> port) const = 0;
};

// `ssh -G -F <config>`: the user's OpenSSH client evaluates the config, `Match` blocks included.
class OpenSshCli final : public SshResolver {
  public:
    explicit OpenSshCli(std::filesystem::path config) : program("ssh"), config(std::move(config)) {}

    std::filesystem::path program;
    std::filesystem::path config;

    [[nodiscard]] std::vector<std::string> aliases() const override;
    [[nodiscard]] Result<ResolvedHost> lookup(std::string_view host, std::optional<std::string_view> user,
                                              std::optional<std::uint16_t> port) const override;
};

// A fixed table; unknown hosts resolve to themselves on port 22 as `user`.
class Static final : public SshResolver {
  public:
    explicit Static(std::string user) : user(std::move(user)) {}

    std::string user;
    std::map<std::string, ResolvedHost> hosts;

    Static& with(std::string_view alias, std::string_view hostname, std::uint16_t port,
                 std::optional<std::string_view> proxy_jump = std::nullopt);

    [[nodiscard]] std::vector<std::string> aliases() const override;
    [[nodiscard]] Result<ResolvedHost> lookup(std::string_view host, std::optional<std::string_view> user,
                                              std::optional<std::uint16_t> port) const override;
};

} // namespace loa::remote
