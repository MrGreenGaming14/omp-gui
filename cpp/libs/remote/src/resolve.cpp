#include "loa/remote/resolve.hpp"

#include <system_error>

namespace loa::remote {

std::vector<std::string> OpenSshCli::aliases() const { return loa::runtime::ssh::aliases(config); }

Result<ResolvedHost> OpenSshCli::lookup(std::string_view host, std::optional<std::string_view> user,
                                        std::optional<std::uint16_t> port) const {
    std::error_code ec;
    const auto cfg = std::filesystem::is_regular_file(config, ec) ? config.string() : std::string("none");
    std::vector<std::string> args{"-F", cfg};
    if (user) {
        args.emplace_back("-l");
        args.emplace_back(*user);
    }
    if (port) {
        args.emplace_back("-p");
        args.push_back(std::to_string(*port));
    }
    auto resolved = loa::runtime::ssh::resolve_with(program, args, host);
    if (!resolved) return std::unexpected(Error::resolve(std::string(host), resolved.error().to_string()));
    return std::move(*resolved);
}

Static& Static::with(std::string_view alias, std::string_view hostname, std::uint16_t port, std::optional<std::string_view> proxy_jump) {
    ResolvedHost h;
    h.hostname = hostname;
    h.user = user;
    h.port = port;
    if (proxy_jump) h.proxy_jump = std::string(*proxy_jump);
    hosts.insert_or_assign(std::string(alias), std::move(h));
    return *this;
}

std::vector<std::string> Static::aliases() const {
    std::vector<std::string> out;
    for (const auto& [alias, _] : hosts) out.push_back(alias);
    return out;
}

Result<ResolvedHost> Static::lookup(std::string_view host, std::optional<std::string_view> u, std::optional<std::uint16_t> p) const {
    ResolvedHost resolved;
    if (const auto it = hosts.find(std::string(host)); it != hosts.end()) {
        resolved = it->second;
    } else {
        resolved.hostname = host;
        resolved.user = user;
        resolved.port = 22;
    }
    if (u) resolved.user = *u;
    if (p) resolved.port = *p;
    return resolved;
}

} // namespace loa::remote
