// Port of the state half of `crates/loa-ui-settings/src/updates.rs`.

#include "loa/ui/settings/updates/state.hpp"

#include <algorithm>
#include <format>

namespace loa::ui::settings::updates {

namespace in = vu::intent;
using Opt = std::optional<std::string>;

const std::array<Rule, 12> STATUS_RULES{{
    {Tone::Warn,
     [](const Product& p, bool) -> Opt {
         if (!p.restart_required) return std::nullopt;
         return std::format("Installed{}; restart the app to use it", p.active ? " " + p.active->current : std::string());
     }},
    {Tone::Danger,
     [](const Product& p, bool) -> Opt {
         if (!p.held) return std::nullopt;
         const auto n = p.held->report.harmful().size();
         if (n == 1) return std::format("{} held: 1 harmful check. Update anyway, pin or wait.", p.held->version);
         return std::format("{} held: {} harmful checks. Update anyway, pin or wait.", p.held->version, n);
     }},
    {Tone::Ok,
     [](const Product& p, bool running) -> Opt {
         if (!p.pending_swap || !running) return std::nullopt;
         return std::format("Checks passed; {} swaps in when the running turn ends", *p.pending_swap);
     }},
    {Tone::Ok,
     [](const Product& p, bool) -> Opt {
         if (!p.pending_swap) return std::nullopt;
         return std::format("Checks passed; ready to swap to {}", *p.pending_swap);
     }},
    {Tone::Muted,
     [](const Product& p, bool) -> Opt {
         if (p.current || !std::holds_alternative<vu::installed::Unknown>(p.installed)) return std::nullopt;
         return "Looking for the installed version\u2026";
     }},
    {Tone::Muted,
     [](const Product& p, bool) -> Opt {
         if (p.current) return std::nullopt;
         return p.newest ? std::format("Not installed; latest is {}", *p.newest) : std::string("Not installed");
     }},
    {Tone::Warn,
     [](const Product& p, bool) -> Opt {
         if (!p.offer || std::ranges::find(p.staged, *p.offer) == p.staged.end()) return std::nullopt;
         return std::format("{} downloaded; run the gate checks", *p.offer);
     }},
    {Tone::Warn,
     [](const Product& p, bool) -> Opt {
         if (!p.offer) return std::nullopt;
         return std::format("Update available: {}", *p.offer);
     }},
    {Tone::Muted,
     [](const Product& p, bool) -> Opt {
         if (!p.pin) return std::nullopt;
         return std::format("Pinned to {}", *p.pin);
     }},
    {Tone::Muted,
     [](const Product& p, bool) -> Opt {
         if (!p.waiting_past) return std::nullopt;
         return std::format("Waiting for a version after {}", *p.waiting_past);
     }},
    {Tone::Muted,
     [](const Product& p, bool) -> Opt {
         if (p.last_check_unix) return std::nullopt;
         return "Not checked yet";
     }},
    {Tone::Ok, [](const Product&, bool) -> Opt { return "Up to date"; }},
}};

std::pair<Tone, std::string> status(const Product& p, bool turn_running) {
    for (const auto& rule : STATUS_RULES)
        if (auto line = rule.line(p, turn_running)) return {rule.tone, std::move(*line)};
    return {Tone::Ok, "Up to date"}; // unreachable: the last rule always matches
}

std::pair<Tone, std::string> standing(const PeerStanding& p) {
    if (!p.can_talk) return {Tone::Danger, "can't talk: no shared protocol"};
    return std::visit(
        []<class S>(const S& s) -> std::pair<Tone, std::string> {
            namespace st = vu::standing;
            if constexpr (std::is_same_v<S, st::Current>) return {Tone::Ok, "current"};
            else if constexpr (std::is_same_v<S, st::Ahead>) return {Tone::Muted, "ahead of this app"};
            else if constexpr (std::is_same_v<S, st::Unknown>) return {Tone::Muted, "version unknown"};
            else {
                const auto [n, one, many] = s.majors > 0   ? std::tuple{s.majors, "major", "majors"}
                                            : s.minors > 0 ? std::tuple{s.minors, "minor", "minors"}
                                                           : std::tuple{s.patches, "patch", "patches"};
                if (n == 0) return {Tone::Warn, "behind (prerelease)"};
                if (n == 1) return {Tone::Warn, std::format("behind by 1 {}", one)};
                return {Tone::Warn, std::format("behind by {} {}", n, many)};
            }
        },
        p.standing);
}

namespace {

// The product and the step `intent` runs; choices, unpins and CheckAll are instant.
std::optional<std::pair<std::string, Step>> step(const Intent& intent) {
    return std::visit(
        []<class T>(const T& v) -> std::optional<std::pair<std::string, Step>> {
            if constexpr (std::is_same_v<T, in::Check>) return std::pair{v.product, Step::Checking};
            else if constexpr (std::is_same_v<T, in::Stage>) return std::pair{v.product, Step::Downloading};
            else if constexpr (std::is_same_v<T, in::Gate>) return std::pair{v.product, Step::RunningChecks};
            else if constexpr (std::is_same_v<T, in::Verify>) return std::pair{v.product, Step::Verifying};
            else if constexpr (std::is_same_v<T, in::Swap> || std::is_same_v<T, in::Rollback>) return std::pair{v.product, Step::Swapping};
            else return std::nullopt;
        },
        intent);
}

} // namespace

void State::set_data(Data d) {
    if (selected && !std::ranges::any_of(d.products, [&](const Product& p) { return p.id == *selected; })) selected.reset();
    data = std::move(d);
}

std::optional<Step> State::busy(std::string_view id) const {
    const auto it = busy_.find(id);
    if (it == busy_.end()) return std::nullopt;
    return it->second;
}

void State::finish(std::string_view id) {
    if (const auto it = busy_.find(id); it != busy_.end()) busy_.erase(it);
}

void State::finish_all() { busy_.clear(); }

bool State::idle(std::string_view id) const { return !busy_.contains(id); }

bool State::any_idle() const {
    return std::ranges::any_of(data.products, [this](const Product& p) { return idle(p.id); });
}

std::optional<Intent> State::reduce(Local m) {
    if (auto* t = std::get_if<local::Toggle>(&m)) {
        if (selected == t->id) selected.reset();
        else selected = std::move(t->id);
        return std::nullopt;
    }
    auto& e = std::get<local::Start>(m).intent;
    if (std::holds_alternative<in::CheckAll>(e)) {
        bool any = false;
        for (const auto& p : data.products)
            if (idle(p.id)) {
                busy_.emplace(p.id, Step::Checking);
                any = true;
            }
        if (!any) return std::nullopt;
        return in::CheckAll{};
    }
    auto s = step(e);
    if (!s || busy_.contains(s->first)) return std::nullopt;
    busy_.emplace(std::move(s->first), s->second);
    return std::move(e);
}

bool State::guard(const Intent& e) const {
    const std::string* id = nullptr;
    if (const auto* c = std::get_if<in::Choose>(&e)) id = &c->product;
    else if (const auto* u = std::get_if<in::Unpin>(&e)) id = &u->product;
    if (!id) return false;
    return std::ranges::any_of(data.products, [&](const Product& p) { return p.id == *id && idle(p.id); });
}

} // namespace loa::ui::settings::updates
