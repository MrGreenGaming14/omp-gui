#include "loa/updater/state.hpp"

#include "util.hpp"

namespace loa::updater {

using namespace detail;

namespace {

template <typename V, typename F>
Json map_json(const std::map<std::string, V>& m, F&& f) {
    Object out;
    for (const auto& [k, v] : m) out.emplace_back(k, f(v));
    return out;
}

template <typename V, typename F>
void map_value(const Object& o, std::string_view key, std::map<std::string, V>& out, F&& f) {
    const auto* v = field(o, key);
    if (!v) return;
    for (const auto& [k, item] : object(*v, "a map")) out.insert_or_assign(k, f(item));
}

} // namespace

Json to_json(const UpdaterState& v) {
    const auto ver = [](const Version& x) { return Json(x.to_string()); };
    return Object{
        {"pins", map_json(v.pins, ver)},
        {"held", map_json(v.held, [](const auto& h) { return loa::settings::to_json(h); })},
        {"waits", map_json(v.waits, ver)},
        {"last_check_unix", map_json(v.last_check_unix, [](std::uint64_t n) { return Json(n); })},
        {"pending_swap", map_json(v.pending_swap, ver)},
        {"reports", map_json(v.reports, [](const auto& r) { return loa::settings::to_json(r); })},
    };
}

Decoded<UpdaterState> updater_state_from_json(const Json& v) {
    return decoding([&] {
        const auto& o = object(v, "struct UpdaterState");
        UpdaterState s;
        map_value(o, "pins", s.pins, version);
        map_value(o, "held", s.held, settings_value<loa::settings::HeldUpdate>);
        map_value(o, "waits", s.waits, version);
        map_value(o, "last_check_unix", s.last_check_unix, u64);
        map_value(o, "pending_swap", s.pending_swap, version);
        map_value(o, "reports", s.reports, settings_value<loa::settings::GateReport>);
        return s;
    });
}

Result<UpdaterState> UpdaterState::load(const Path& root) {
    auto r = read_json<UpdaterState>(root / "state.json", updater_state_from_json);
    if (!r) return std::unexpected(r.error());
    return r->value_or(UpdaterState{});
}

Result<> UpdaterState::save(const Path& root) const { return write_json(root / "state.json", to_json(*this)); }

} // namespace loa::updater
