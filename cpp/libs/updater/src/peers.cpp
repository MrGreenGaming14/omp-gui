#include "loa/updater/peers.hpp"

#include <algorithm>
#include <map>
#include <set>
#include <tuple>

namespace loa::updater::peers {

using loa::settings::PeerStanding;
using loa::settings::ProtocolStatus;
using loa::settings::Standing;

namespace {

// Families a node must share with a peer to talk at all.
constexpr std::string_view REQUIRED_FAMILIES[] = {"control", "session"};

using Families = std::map<std::string, std::set<std::pair<std::uint32_t, std::string>>, std::less<>>;

Families families(const std::vector<std::string>& labels) {
    Families out;
    for (const auto& label : labels)
        if (auto parsed = parse_alpn(label)) out[std::string(parsed->first)].emplace(parsed->second, label);
    return out;
}

std::vector<ProtocolStatus> protocols(const Families& local, const Families& remote) {
    std::set<std::string> names;
    for (const auto& [f, _] : local) names.insert(f);
    for (const auto& [f, _] : remote) names.insert(f);
    std::vector<ProtocolStatus> out;
    for (const auto& family : names) {
        const auto ours = local.find(family);
        const auto theirs = remote.find(family);
        ProtocolStatus status{family, std::nullopt, std::nullopt, std::nullopt};
        if (ours != local.end() && theirs != remote.end()) {
            for (auto it = ours->second.rbegin(); it != ours->second.rend(); ++it) {
                if (theirs->second.contains(*it)) {
                    status.shared = it->first;
                    break;
                }
            }
        }
        if (ours != local.end() && !ours->second.empty()) status.local = ours->second.rbegin()->first;
        if (theirs != remote.end() && !theirs->second.empty()) status.peer = theirs->second.rbegin()->first;
        out.push_back(std::move(status));
    }
    return out;
}

Standing standing(const Version& local, const std::optional<Version>& peer) {
    if (!peer) return {Standing::Kind::Unknown};
    const auto order = peer->cmp_precedence(local);
    if (order == std::strong_ordering::equal) return {Standing::Kind::Current};
    if (order == std::strong_ordering::greater) return {Standing::Kind::Ahead};
    // The first differing component of the peer is the smaller one, so nothing wraps.
    if (peer->major != local.major) return {Standing::Kind::Behind, local.major - peer->major, 0, 0};
    if (peer->minor != local.minor) return {Standing::Kind::Behind, 0, local.minor - peer->minor, 0};
    return {Standing::Kind::Behind, 0, 0, local.patch - peer->patch};
}

// Lower sorts first: the most behind, then unknown, current, ahead.
std::tuple<int, std::uint64_t, std::uint64_t, std::uint64_t> rank(const Standing& s) {
    using K = Standing::Kind;
    switch (s.kind) {
        // Reverse: more behind sorts first.
        case K::Behind: return {0, ~s.majors, ~s.minors, ~s.patches};
        case K::Unknown: return {1, 0, 0, 0};
        case K::Current: return {2, 0, 0, 0};
        case K::Ahead: return {3, 0, 0, 0};
    }
    return {4, 0, 0, 0};
}

} // namespace

std::optional<std::pair<std::string_view, std::uint32_t>> parse_alpn(std::string_view label) {
    if (!label.starts_with("omp/")) return std::nullopt;
    label.remove_prefix(4);
    const auto slash = label.find('/');
    if (slash == std::string_view::npos) return std::nullopt;
    const auto family = label.substr(0, slash);
    const auto digits = label.substr(slash + 1);
    if (family.empty() || digits.empty() || !std::ranges::all_of(digits, [](char c) { return c >= '0' && c <= '9'; })) return std::nullopt;
    std::uint64_t value = 0;
    for (const char c : digits) {
        value = value * 10 + static_cast<std::uint64_t>(c - '0');
        if (value > 0xffffffffull) return std::nullopt;
    }
    return std::pair{family, static_cast<std::uint32_t>(value)};
}

std::vector<PeerStanding> classify(const Version& local_version, const std::vector<std::string>& local_alpns,
                                   const std::vector<loa::settings::PeerInfo>& peers) {
    const auto local = families(local_alpns);
    std::vector<PeerStanding> rows;
    for (const auto& peer : peers) {
        auto protos = protocols(local, families(peer.alpns));
        const auto shares = [&](std::string_view family) {
            return std::ranges::any_of(protos, [&](const ProtocolStatus& p) { return p.family == family && p.shared.has_value(); });
        };
        const bool can_talk = std::ranges::all_of(REQUIRED_FAMILIES, shares) &&
                              std::ranges::all_of(local, [&](const auto& entry) { return shares(entry.first); });
        rows.push_back(PeerStanding{peer.node, peer.name, peer.app_version, standing(local_version, peer.app_version), std::move(protos), can_talk});
    }
    std::ranges::stable_sort(rows, [](const PeerStanding& a, const PeerStanding& b) {
        const auto ra = rank(a.standing);
        const auto rb = rank(b.standing);
        return std::tie(a.can_talk, ra, a.node) < std::tie(b.can_talk, rb, b.node);
    });
    return rows;
}

} // namespace loa::updater::peers
