#include "loa/ui/session/decisions/unattended.hpp"

#include <algorithm>
#include <limits>

#include "loa/ui/session/decisions/card.hpp"

namespace loa::ui::session::decisions {

std::string Duration::to_string() const {
    if (secs >= 60 && secs % 60 == 0) return std::to_string(secs / 60) + " min";
    return std::to_string(secs) + " s";
}

bool Unattended::enabled() const {
    return std::ranges::any_of(KINDS, [&](DecisionKind k) { return view.for_kind(k).has_value(); });
}

std::optional<std::int64_t> Unattended::timeout_ms(DecisionKind kind) const {
    const auto& d = view.for_kind(kind);
    if (!d) return std::nullopt;
    constexpr auto max = static_cast<std::uint64_t>(std::numeric_limits<std::int64_t>::max());
    const std::uint64_t ms = d->timeout_secs > std::numeric_limits<std::uint64_t>::max() / 1000 ? max : d->timeout_secs * 1000;
    return static_cast<std::int64_t>(std::min(ms, max));
}

std::optional<loa::contracts::Answer> Unattended::answer(const Decision& decision, std::string_view by) const {
    const auto& def = view.for_kind(decision.kind);
    if (!def) return std::nullopt;
    auto policy = kind_for(decision.kind).policy(decision, def->answer);
    if (!policy) return std::nullopt;
    auto [value, refuses] = std::move(*policy);
    std::optional<std::string> reason = def->reason;
    if (!reason && refuses) reason = "Nobody answered within " + Duration{def->timeout_secs}.to_string();
    return loa::contracts::Answer{{std::move(value)}, std::move(reason), std::string(by)};
}

void Unattended::update(const unattended::Message& message) {
    std::uint64_t timeout = DEFAULT_TIMEOUT;
    for (auto k : KINDS)
        if (const auto& d = view.for_kind(k)) {
            timeout = d->timeout_secs;
            break;
        }
    const auto fresh = [&](std::string_view answer) { return DecisionDefaultView{timeout, std::string(answer), std::nullopt}; };
    if (const auto* e = std::get_if<unattended::Enabled>(&message)) {
        if (!e->on) {
            view = UnattendedView{};
            return;
        }
        for (auto [kind, answer] : {std::pair{DecisionKind::Approval, loa::core::answer::DENY},
                                    std::pair{DecisionKind::Plan, loa::core::answer::REJECT}}) {
            auto& slot = view.for_kind(kind);
            if (!slot) slot = fresh(answer);
        }
    } else if (const auto* a = std::get_if<unattended::Answer>(&message)) {
        auto& slot = view.for_kind(a->kind);
        if (!a->answer) {
            slot = std::nullopt;
        } else if (!slot) {
            slot = fresh(*a->answer);
        } else if (slot->answer != *a->answer) {
            slot->answer = *a->answer;
            slot->reason = std::nullopt;
        }
    } else if (const auto* t = std::get_if<unattended::Timeout>(&message)) {
        if (auto& slot = view.for_kind(t->kind)) slot->timeout_secs = std::max<std::uint64_t>(t->timeout.secs, 1);
    }
}

std::vector<SettingsRow> settings_rows(const Unattended& policy) {
    std::vector<SettingsRow> rows;
    for (auto kind : KINDS) {
        const auto& def = policy.view.for_kind(kind);
        const auto& strategy = kind_for(kind);
        SettingsRow row{kind, strategy.settings_label(), {}, std::nullopt, {}, {}, std::nullopt};
        const auto presets = strategy.presets();
        if (presets.empty()) {
            if (def) row.typed = def->answer;
        } else {
            row.choices.push_back({std::nullopt, "Wait for someone"});
            for (auto [answer, label] : presets) row.choices.push_back({std::string(answer), std::string(label)});
            const std::optional<std::string> selected = def ? std::optional(def->answer) : std::nullopt;
            const auto it = std::ranges::find_if(row.choices, [&](const Preset& p) { return p.answer == selected; });
            row.current = it != row.choices.end() ? *it : Preset{selected, "Value from the config file"};
        }
        if (def) {
            for (auto t : TIMEOUTS) row.timeouts.push_back({t});
            if (std::ranges::find(TIMEOUTS, def->timeout_secs) == TIMEOUTS.end()) row.timeouts.push_back({def->timeout_secs});
            row.timeout = Duration{def->timeout_secs};
        }
        rows.push_back(std::move(row));
    }
    return rows;
}

} // namespace loa::ui::session::decisions
