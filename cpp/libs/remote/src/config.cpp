#include "loa/remote/config.hpp"

#include <algorithm>
#include <cctype>
#include <charconv>

#include "loa/runtime/fs.hpp"

namespace loa::remote {

namespace {

std::string lower(std::string_view s) {
    std::string out(s);
    for (auto& c : out) c = static_cast<char>(std::tolower(static_cast<unsigned char>(c)));
    return out;
}

bool iequals(std::string_view a, std::string_view b) { return lower(a) == lower(b); }

std::string_view trim(std::string_view s) {
    while (!s.empty() && std::isspace(static_cast<unsigned char>(s.front()))) s.remove_prefix(1);
    while (!s.empty() && std::isspace(static_cast<unsigned char>(s.back()))) s.remove_suffix(1);
    return s;
}

using Chain = std::pair<Endpoint, std::vector<Endpoint>>;

// Resolves `host` and its ProxyJump list: the first jump recursively, the others as given.
Result<Chain> chain(const SshResolver& resolver, const std::string& host, std::optional<std::string_view> user,
                    std::optional<std::uint16_t> port, std::optional<std::string_view> jump, std::vector<std::string>& stack) {
    const auto key = lower(host);
    if (std::ranges::find(stack, key) != stack.end()) return std::unexpected(Error::proxy_jump_cycle(stack.front()));
    if (stack.size() > MAX_JUMPS) return std::unexpected(Error::proxy_jump_too_deep(stack.front()));
    auto resolved = resolver.lookup(host, user, port);
    if (!resolved) return std::unexpected(resolved.error());
    std::optional<std::string> list = jump ? std::optional<std::string>(std::string(*jump)) : resolved->proxy_jump;
    if (list && iequals(*list, "none")) list.reset();
    std::vector<Endpoint> jumps;
    if (list) {
        std::size_t start = 0;
        for (std::size_t i = 0;; ++i) {
            const auto end = std::min(list->find(',', start), list->size());
            const auto spec = trim(std::string_view(*list).substr(start, end - start));
            auto parsed = parse_jump(spec);
            if (!parsed) return std::unexpected(Error::resolve(host, parsed.error()));
            auto& [h, u, p] = *parsed;
            const auto uv = u ? std::optional<std::string_view>(*u) : std::nullopt;
            if (i == 0) {
                stack.push_back(key);
                auto first = chain(resolver, h, uv, p, std::nullopt, stack);
                if (!first) return std::unexpected(first.error());
                stack.pop_back();
                for (auto& e : first->second) jumps.push_back(std::move(e));
                jumps.push_back(std::move(first->first));
            } else {
                auto r = resolver.lookup(h, uv, p);
                if (!r) return std::unexpected(r.error());
                jumps.push_back(endpoint(h, std::move(*r)));
            }
            if (end >= list->size()) break;
            start = end + 1;
        }
    }
    if (jumps.size() > MAX_JUMPS) return std::unexpected(Error::proxy_jump_too_deep(host));
    return Chain{endpoint(host, std::move(*resolved)), std::move(jumps)};
}

} // namespace

HostLimits limits(const RemoteHost& entry) {
    HostLimits l;
    l.allowed_paths = entry.allowed_paths;
    l.read_only = entry.read_only;
    l.commands = entry.commands;
    l.auto_approve_writes = entry.auto_approve;
    l.auto_approve_commands = entry.auto_approve;
    return l;
}

Result<Host> host(std::string_view name, const RemoteHost& entry, const SshResolver& resolver) {
    std::vector<std::string> stack;
    const auto user = entry.user ? std::optional<std::string_view>(*entry.user) : std::nullopt;
    const auto jump = entry.proxy_jump ? std::optional<std::string_view>(*entry.proxy_jump) : std::nullopt;
    auto c = chain(resolver, entry.host, user, entry.port, jump, stack);
    if (!c) return std::unexpected(c.error());
    return Host{std::string(name), std::move(c->first), std::move(c->second), limits(entry)};
}

std::pair<std::vector<Host>, std::vector<std::pair<std::string, Error>>> load(const std::map<std::string, RemoteHost>& entries,
                                                                              const SshResolver& resolver) {
    std::pair<std::vector<Host>, std::vector<std::pair<std::string, Error>>> out;
    for (const auto& [name, entry] : entries) {
        if (auto h = host(name, entry, resolver)) out.first.push_back(std::move(*h));
        else out.second.emplace_back(name, h.error());
    }
    return out;
}

RemoteHost entry_for_alias(std::string_view alias) {
    RemoteHost e;
    e.host = alias;
    e.read_only = true;
    e.commands = false;
    e.auto_approve = false;
    return e;
}

std::vector<std::string> import_aliases(std::map<std::string, RemoteHost>& entries, const SshResolver& resolver) {
    std::vector<std::string> added;
    for (auto& alias : resolver.aliases()) {
        if (!entries.contains(alias)) added.push_back(std::move(alias));
    }
    for (const auto& alias : added) entries.emplace(alias, entry_for_alias(alias));
    return added;
}

std::expected<std::tuple<std::string, std::optional<std::string>, std::optional<std::uint16_t>>, std::string> parse_jump(
    std::string_view spec) {
    const auto bad = [&](std::string_view what) { return std::unexpected("ProxyJump `" + std::string(spec) + "`: " + std::string(what)); };
    auto s = spec;
    if (s.starts_with("ssh://")) s.remove_prefix(6);
    std::optional<std::string> user;
    auto hostport = s;
    if (const auto at = s.rfind('@'); at != std::string_view::npos) {
        user = std::string(s.substr(0, at));
        hostport = s.substr(at + 1);
    }
    std::string_view host = hostport;
    std::optional<std::string_view> port;
    if (hostport.starts_with('[')) {
        const auto rest = hostport.substr(1);
        const auto close = rest.find(']');
        if (close == std::string_view::npos) return bad("missing `]`");
        host = rest.substr(0, close);
        const auto after = rest.substr(close + 1);
        if (!after.empty()) {
            if (!after.starts_with(':')) return bad("junk after `]`");
            port = after.substr(1);
        }
    } else if (const auto colon = hostport.find(':'); colon != std::string_view::npos) {
        const auto p = hostport.substr(colon + 1);
        if (p.find(':') == std::string_view::npos) {
            host = hostport.substr(0, colon);
            port = p;
        }
    }
    if (host.empty()) return bad("empty host");
    std::optional<std::uint16_t> number;
    if (port) {
        auto digits = *port;
        if (digits.starts_with('+')) digits.remove_prefix(1);
        std::uint16_t value = 0;
        const auto [end, ec] = std::from_chars(digits.data(), digits.data() + digits.size(), value);
        if (digits.empty() || ec != std::errc{} || end != digits.data() + digits.size() || value == 0) return bad("bad port");
        number = value;
    }
    return std::tuple{std::string(host), std::move(user), number};
}

Endpoint endpoint(std::string_view alias, ResolvedHost r) {
    AgentSocket agent;
    if (const auto& v = r.identity_agent; v && *v != "SSH_AUTH_SOCK" && *v != "$SSH_AUTH_SOCK") {
        if (iequals(*v, "none")) {
            agent = AgentSocket::none();
        } else {
            std::optional<std::string_view> rest;
            if (v->starts_with("~/")) rest = std::string_view(*v).substr(2);
#if defined(_WIN32)
            else if (v->starts_with("~\\")) rest = std::string_view(*v).substr(2);
#endif
            agent = AgentSocket::at(rest ? loa::runtime::fs::home().value_or(std::filesystem::path()) / *rest : std::filesystem::path(*v));
        }
    }
    return Endpoint{std::string(alias), std::move(r.hostname), r.port, std::move(r.user), std::move(agent)};
}

} // namespace loa::remote
