#include "loa/p2p/discovery.hpp"

#include <algorithm>

#include "loa/runtime/time.hpp"

namespace loa::p2p {

NearbyPeer NearbyPeer::make(NodeId key, NearbySource source, std::optional<std::string> name,
                            std::vector<SocketAddr> addresses) {
    NearbyPeer peer;
    peer.key = key;
    peer.name = std::move(name);
    peer.addresses = std::move(addresses);
    peer.source = source;
    peer.seen_ms = loa::runtime::time::now_ms();
    return peer;
}

void NearbyPeer::merge(NearbyPeer seen) {
    if (seen.source != NearbySource::incoming) source = seen.source;
    seen_ms = seen.seen_ms;
    if (seen.name) name = std::move(seen.name);
    if (seen.app_version) app_version = std::move(seen.app_version);
    if (!seen.protocols.empty()) protocols = std::move(seen.protocols);
    addresses = merge_addresses(addresses, std::move(seen.addresses), seen.source == NearbySource::mdns);
}

std::vector<SocketAddr> merge_addresses(std::span<const SocketAddr> old, std::vector<SocketAddr> found, bool replace) {
    if (found.empty()) return {old.begin(), old.end()};
    if (!replace) {
        std::vector<SocketAddr> missing;
        for (const auto& a : old)
            if (std::ranges::find(found, a) == found.end()) missing.push_back(a);
        found.insert(found.end(), missing.begin(), missing.end());
    }
    return found;
}

} // namespace loa::p2p
