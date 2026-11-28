#include "loa/updater/decision.hpp"

namespace loa::updater::decision {

using loa::settings::HoldChoice;

namespace {

template <typename M>
auto find(M& m, std::string_view key) {
    return m.find(std::string(key));
}

} // namespace

bool record(UpdaterState& state, const loa::settings::GateReport& report, std::uint64_t now_unix) {
    const auto& product = report.product;
    if (report.is_clean()) {
        state.held.erase(product);
        state.pending_swap.insert_or_assign(product, report.to);
        return true;
    }
    if (auto it = state.pending_swap.find(product); it != state.pending_swap.end() && it->second == report.to) state.pending_swap.erase(it);
    state.held.insert_or_assign(product, loa::settings::HeldUpdate{report.to, report, now_unix});
    return false;
}

std::optional<Version> apply_choice(UpdaterState& state, std::string_view product_view, const Version& current, HoldChoice choice) {
    const std::string product(product_view);
    if (choice == HoldChoice::PinCurrent) {
        state.held.erase(product);
        state.pending_swap.erase(product);
        state.pins.insert_or_assign(product, current);
        return std::nullopt;
    }
    auto it = state.held.find(product);
    if (it == state.held.end()) return std::nullopt;
    const auto held = std::move(it->second);
    state.held.erase(it);
    if (choice == HoldChoice::UpdateAnyway) {
        // An earlier pin would otherwise keep offering only the old version.
        state.pins.erase(product);
        state.pending_swap.insert_or_assign(product, held.version);
        return held.version;
    }
    if (auto p = state.pending_swap.find(product); p != state.pending_swap.end() && p->second == held.version) state.pending_swap.erase(p);
    if (auto w = state.waits.find(product); w == state.waits.end() || held.version > w->second) state.waits.insert_or_assign(product, held.version);
    return std::nullopt;
}

bool offer_allowed(const UpdaterState& state, std::string_view product, const Version& version) {
    const auto pin = find(state.pins, product);
    const auto wait = find(state.waits, product);
    return (pin == state.pins.end() || pin->second == version) && (wait == state.waits.end() || version > wait->second);
}

} // namespace loa::updater::decision
