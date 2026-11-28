#pragma once

// Unattended runs: a waiting decision nobody answers within its kind's timeout gets the default
// answer set for that kind. The policy is the central config's `unattended` table, as the app
// projects it into an `UnattendedView`; a kind without an entry waits, and a decision carrying
// its own default keeps it. Port of `decisions/unattended.rs` and the settings rows of
// `decisions/settings.rs`.

#include <array>
#include <cstdint>
#include <optional>
#include <string>
#include <string_view>
#include <variant>
#include <vector>

#include "loa/ui/session/decisions/model.hpp"
#include "loa/view/session.hpp"

namespace loa::ui::session::decisions {

using loa::view::session::decisions::DecisionDefaultView;
using loa::view::session::decisions::UnattendedView;

// Timeouts the settings offer, in seconds.
inline constexpr std::array<std::uint64_t, 6> TIMEOUTS{30, 60, 120, 300, 900, 1800};
// Timeout of a kind turned on while no other kind has one.
inline constexpr std::uint64_t DEFAULT_TIMEOUT = 300;
// The kinds the policy covers, in settings order.
inline constexpr std::array KINDS{DecisionKind::Approval, DecisionKind::Question, DecisionKind::Plan, DecisionKind::Form};

// A timeout shown as "30 s", "2 min".
struct Duration {
    std::uint64_t secs = 0;
    [[nodiscard]] std::string to_string() const;
    friend bool operator==(const Duration&, const Duration&) = default;
};

// Settings edits.
namespace unattended {
// On: approvals are denied and plans rejected after five minutes (kinds already set keep their
// answers). Off: every kind waits.
struct Enabled {
    bool on = false;
    friend bool operator==(const Enabled&, const Enabled&) = default;
};
// The default answer of a kind; `nullopt` waits for someone.
struct Answer {
    DecisionKind kind = DecisionKind::Approval;
    std::optional<std::string> answer;
    friend bool operator==(const Answer&, const Answer&) = default;
};
struct Timeout {
    DecisionKind kind = DecisionKind::Approval;
    Duration timeout;
    friend bool operator==(const Timeout&, const Timeout&) = default;
};
using Message = std::variant<Enabled, Answer, Timeout>;
} // namespace unattended

// The unattended policy over the central config's `unattended` table.
struct Unattended {
    UnattendedView view;

    // Whether any kind is answered after a timeout.
    [[nodiscard]] bool enabled() const;
    // Timeout in ms of decisions of `kind`, when the policy answers them.
    [[nodiscard]] std::optional<std::int64_t> timeout_ms(DecisionKind kind) const;
    // The default answer this policy gives `decision`, recorded with `by`; `nullopt` when its
    // kind waits or the configured answer doesn't fit (a hand-edited value).
    [[nodiscard]] std::optional<loa::contracts::Answer> answer(const Decision& decision, std::string_view by) const;
    void update(const unattended::Message& message);

    friend bool operator==(const Unattended&, const Unattended&) = default;
};

// A default answer with its label; `nullopt` waits.
struct Preset {
    std::optional<std::string> answer;
    std::string label;
    friend bool operator==(const Preset&, const Preset&) = default;
};

// One row of the unattended settings: the kind's label, then either a picker over `choices`
// with `current` selected or (no presets) a text input holding `typed`, then the timeout picker
// over `timeouts` with `timeout` selected when the kind has a default.
struct SettingsRow {
    DecisionKind kind = DecisionKind::Approval;
    std::string_view label;
    std::vector<Preset> choices;
    std::optional<Preset> current;
    std::string typed;
    std::vector<Duration> timeouts;
    std::optional<Duration> timeout;
};

inline constexpr std::string_view SETTINGS_TOGGLE = "Answer waiting prompts after a timeout";
inline constexpr std::string_view SETTINGS_NOTE =
    "For unattended runs: a prompt nobody answers in time gets the default set for its kind, recorded as answered "
    "by timeout. A kind set to wait is never answered for you.";
inline constexpr std::string_view SETTINGS_TYPED_PLACEHOLDER = "Option or typed answer; empty waits";

// The rows of every kind, in `KINDS` order.
[[nodiscard]] std::vector<SettingsRow> settings_rows(const Unattended& policy);

} // namespace loa::ui::session::decisions
