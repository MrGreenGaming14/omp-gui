#include "loa/remote/hosts.hpp"

#include <mutex>

namespace loa::remote {

std::vector<Endpoint> Host::hops() const {
    auto out = jumps;
    out.push_back(target);
    return out;
}

void HostRegistry::insert(Host host) {
    std::unique_lock lock(mu_);
    auto name = host.name;
    hosts_.insert_or_assign(std::move(name), std::move(host));
}

void HostRegistry::replace_all(std::vector<Host> hosts) {
    std::map<std::string, Host> next;
    for (auto& h : hosts) {
        auto name = h.name;
        next.insert_or_assign(std::move(name), std::move(h));
    }
    std::unique_lock lock(mu_);
    hosts_ = std::move(next);
}

std::optional<Host> HostRegistry::get(const std::string& name) const {
    std::shared_lock lock(mu_);
    const auto it = hosts_.find(name);
    if (it == hosts_.end()) return std::nullopt;
    return it->second;
}

} // namespace loa::remote
