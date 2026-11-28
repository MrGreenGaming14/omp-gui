#include <algorithm>
#include <cctype>
#include <format>

#include <arpa/inet.h>
#include <netdb.h>
#include <sys/socket.h>

#include "loa/p2p/discovery.hpp"
#include "loa/runtime/exec.hpp"
#include "loa/runtime/json.hpp"

namespace loa::p2p {
namespace {

namespace json = loa::runtime::json;

std::optional<std::string> dns_name(std::string_view raw) {
    std::string_view name = raw.ends_with('.') ? raw.substr(0, raw.size() - 1) : raw;
    auto label_ok = [](std::string_view l) {
        return !l.empty() && l.size() <= 63 && !l.starts_with('-') && !l.ends_with('-') &&
               std::ranges::all_of(l, [](char c) { return std::isalnum(static_cast<unsigned char>(c)) || c == '-'; });
    };
    if (name.size() > 253 || name.find('.') == std::string_view::npos) return std::nullopt;
    for (std::size_t start = 0;;) {
        const auto dot = name.find('.', start);
        if (!label_ok(name.substr(start, dot == std::string_view::npos ? std::string_view::npos : dot - start)))
            return std::nullopt;
        if (dot == std::string_view::npos) break;
        start = dot + 1;
    }
    std::string out(name);
    for (auto& c : out) c = static_cast<char>(std::tolower(static_cast<unsigned char>(c)));
    return out;
}

// Every resolved address of `host` at `port`, in resolver order (Rust `lookup_host`).
std::vector<SocketAddr> lookup_host(const std::string& host, std::uint16_t port) {
    addrinfo hints{};
    hints.ai_family = AF_UNSPEC;
    hints.ai_socktype = SOCK_STREAM;
    addrinfo* found = nullptr;
    std::vector<SocketAddr> out;
    if (getaddrinfo(host.c_str(), std::to_string(port).c_str(), &hints, &found) != 0) return out;
    for (auto* ai = found; ai; ai = ai->ai_next) {
        char ip[INET6_ADDRSTRLEN] = {};
        if (ai->ai_family == AF_INET) {
            inet_ntop(AF_INET, &reinterpret_cast<sockaddr_in*>(ai->ai_addr)->sin_addr, ip, sizeof ip);
            out.push_back(std::format("{}:{}", ip, port));
        } else if (ai->ai_family == AF_INET6) {
            inet_ntop(AF_INET6, &reinterpret_cast<sockaddr_in6*>(ai->ai_addr)->sin6_addr, ip, sizeof ip);
            out.push_back(std::format("[{}]:{}", ip, port));
        }
    }
    freeaddrinfo(found);
    return out;
}

std::string_view trimmed(std::string_view s) {
    const auto first = s.find_first_not_of(" \t\r\n");
    if (first == std::string_view::npos) return {};
    return s.substr(first, s.find_last_not_of(" \t\r\n") - first + 1);
}

} // namespace

Result<std::vector<std::string>> parse_tailscale_status(std::string_view text) {
    auto root = json::parse(text);
    if (!root) return fail(NodeError::Kind::net, "invalid tailscale status: " + root.error().message());
    const auto* peer_value = root->find("Peer");
    const auto* peers = peer_value ? peer_value->as_object() : nullptr;
    if (!peers) return fail(NodeError::Kind::net, "tailscale status has no Peer object");
    std::vector<std::string> names;
    for (const auto& [_, peer] : *peers) {
        const auto* online = peer.find("Online");
        if (online && online->as_bool() && !*online->as_bool()) continue;
        const auto* dns = peer.find("DNSName");
        if (!dns || !dns->as_string()) continue;
        if (auto name = dns_name(*dns->as_string())) names.push_back(std::move(*name));
    }
    std::ranges::sort(names);
    names.erase(std::unique(names.begin(), names.end()), names.end());
    return names;
}

std::vector<NearbyPeer>
admit_candidates(const std::vector<std::pair<std::string, std::vector<SocketAddr>>>& candidates, NodeProbe& probe) {
    std::vector<NearbyPeer> found;
    for (const auto& [name, addresses] : candidates) {
        for (const auto& address : addresses) {
            if (auto key = probe.probe(address)) {
                std::string shortname = name.substr(0, name.find('.'));
                found.push_back(NearbyPeer::make(*key, NearbySource::tailscale, std::move(shortname), {address}));
                break;
            }
        }
    }
    return found;
}

Tailnet::Tailnet(std::filesystem::path executable, std::uint16_t port, std::shared_ptr<NodeProbe> probe)
    : executable_(std::move(executable)), port_(port), probe_(std::move(probe)) {}

Result<std::vector<NearbyPeer>> Tailnet::scan() const {
    namespace exec = loa::runtime::exec;
    auto output = exec::output(exec::command(executable_, {"status", "--json"}));
    if (!output) return fail(NodeError::Kind::io, output.error().message());
    if (!output->status.success())
        return fail(NodeError::Kind::net, std::format("tailscale status exited with {}: {}", output->status.to_string(),
                                                      trimmed(output->err)));
    auto names = parse_tailscale_status(output->out);
    if (!names) return std::unexpected(names.error());
    std::vector<std::pair<std::string, std::vector<SocketAddr>>> candidates;
    for (auto& name : *names) {
        auto addresses = lookup_host(name, port_);
        candidates.emplace_back(std::move(name), std::move(addresses));
    }
    return admit_candidates(candidates, *probe_);
}

std::optional<NearbyPeer> Tailnet::next() {
    if (!found_) found_ = scan();
    if (!*found_ || (*found_)->empty()) return std::nullopt;
    auto peer = std::move((*found_)->back());
    (*found_)->pop_back();
    return peer;
}

Availability Tailnet::availability() const {
    if (found_ && !*found_) return {Availability::State::failed, found_->error().to_string()};
    return {Availability::State::active, "tailnet scanned"};
}

} // namespace loa::p2p
