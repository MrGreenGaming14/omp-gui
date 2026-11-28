#include "loa/app_runtime/harnesses.hpp"

#include <algorithm>
#include <cassert>

namespace loa::app_runtime {

void Harnesses::add(std::shared_ptr<contracts::Adapter> adapter, Harness harness) {
    assert(adapter->harness() == harness.id && "a harness is listed under its adapter's id");
    registry.insert(harness.id, std::move(adapter));
    auto listed = std::ranges::find(list, harness.id, &Harness::id);
    if (listed != list.end())
        *listed = std::move(harness);
    else
        list.push_back(std::move(harness));
}

void Harnesses::set_availability(std::string_view id, contracts::Availability availability) {
    auto harness = std::ranges::find(list, id, &Harness::id);
    if (harness == list.end()) return;
    if (availability.status != contracts::AvailabilityStatus::Unknown) {
        harness->available = availability.is_ready();
        harness->executable = availability.executable;
    }
    harness->detail = availability.detail;
    harness->availability = std::move(availability);
}

std::shared_ptr<contracts::Adapter> Harnesses::runnable(std::string_view id) const {
    const Harness* harness = get(id);
    if (!harness || !harness->available) return nullptr;
    return registry.get(id);
}

const Harness* Harnesses::get(std::string_view id) const {
    auto harness = std::ranges::find(list, id, &Harness::id);
    return harness == list.end() ? nullptr : &*harness;
}

contracts::HarnessTraits Harnesses::traits(std::string_view id) const {
    const Harness* harness = get(id);
    return harness ? harness->traits : contracts::HarnessTraits{};
}

std::string_view Harnesses::label(std::string_view id) const {
    const Harness* harness = get(id);
    return harness ? std::string_view(harness->label) : id;
}

std::string_view Harnesses::roles_id() const {
    auto harness = std::ranges::find_if(list, [](const Harness& h) { return h.traits.has_roles; });
    return harness == list.end() ? std::string_view() : std::string_view(harness->id);
}

std::optional<std::string> Harnesses::default_id() const {
    auto roles = std::ranges::find_if(list, [](const Harness& h) { return h.available && h.traits.has_roles; });
    if (roles != list.end()) return roles->id;
    auto any = std::ranges::find_if(list, &Harness::available);
    if (any != list.end()) return any->id;
    return std::nullopt;
}

} // namespace loa::app_runtime
