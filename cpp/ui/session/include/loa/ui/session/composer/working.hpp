#pragma once

// The working indicator above the editor while the shown session runs a turn (port of
// `composer/working.rs`): a spinning accent loader, a label saying what the agent does drawn in
// a shimmer, and once the turn runs its elapsed time with the interrupt hint.

#include <chrono>
#include <cstddef>
#include <cstdint>
#include <optional>
#include <string>
#include <string_view>
#include <vector>

#include "loa/core/schema.hpp"

namespace loa::core {
class Dom;
}

namespace loa::ui::session::composer {

namespace working {
// The label while the turn runs with no call or thinking in progress.
inline constexpr std::string_view WORKING = "Working\u2026";
inline constexpr std::string_view THINKING = "Thinking\u2026";
// The label while a send starts the session, before its turn runs.
inline constexpr std::string_view STARTING = "Starting session\u2026";
// The trailer's key hint: Esc interrupts while the composer is busy.
inline constexpr std::string_view INTERRUPT_HINT = "esc to interrupt";
// Most characters of a call title the label shows.
inline constexpr std::size_t MAX_TITLE = 60;
// Graphemes from the middle of the shimmer band to its ends; its middle third is brightest.
inline constexpr float BAND = 3.0F;
// Spinner turns per shimmer cycle.
inline constexpr float SPINS = 2.0F;
// How often the indicator ticks while the window has focus.
inline constexpr std::chrono::milliseconds FRAME{90};
// How often it ticks without focus: the elapsed time keeps counting, the animation rests.
inline constexpr std::chrono::milliseconds UNFOCUSED{1000};

// `what` cut to `MAX_TITLE` characters, ending in one `…`.
[[nodiscard]] std::string ongoing(std::string_view what);
// `12s`, `1m 05s` or `1h 02m` for a turn that has run `ms` milliseconds.
[[nodiscard]] std::string elapsed(std::int64_t ms);
// The trailer of a running turn: the time since `started` at `now`, when known, and the hint.
[[nodiscard]] std::string trailer(std::optional<std::int64_t> started, std::int64_t now);

// A run of one shimmer level: 0 off the band, 1 on its edges, 2 in its middle.
struct Run {
    std::size_t begin = 0, end = 0;
    std::size_t level = 0;
    friend bool operator==(const Run&, const Run&) = default;
};
// `label` in runs of one shimmer level for a band centred on grapheme `center`. Runs break only
// between graphemes, so a colour change never splits a letter from its combining marks.
[[nodiscard]] std::vector<Run> runs(std::string_view label, float center);
// The band's centre at `progress` (`0..1`) for `label`.
[[nodiscard]] float band_center(std::string_view label, float progress);
} // namespace working

// What the shown session's latest turn holds, as the activity view-model input: its status and
// start, and its children oldest first. The app builds it from the session's DOM
// (`ActivityModel::of`) or keeps it in step from patches.
struct ActivityModel {
    struct Call {
        std::optional<core::CallStatus> status;
        std::optional<std::string> title;
        std::optional<std::string> tool;
        friend bool operator==(const Call&, const Call&) = default;
    };
    struct Message {
        std::optional<core::MessageStatus> status;
        std::optional<core::MessageKind> kind;
        friend bool operator==(const Message&, const Message&) = default;
    };
    struct Item {
        enum class Kind : std::uint8_t { Call, Message, Other };
        Kind kind = Kind::Other;
        Call call;
        Message message;
        friend bool operator==(const Item&, const Item&) = default;
    };

    // Unset: the session has no turn yet, or its status is unknown.
    std::optional<core::TurnStatus> status;
    std::optional<std::int64_t> started;
    std::vector<Item> items;

    // The model of `dom`'s latest turn.
    [[nodiscard]] static ActivityModel of(const core::Dom& dom);
    friend bool operator==(const ActivityModel&, const ActivityModel&) = default;
};

// What the shown session's agent is doing, read from its latest turn.
struct Activity {
    enum class Kind : std::uint8_t {
        Idle,    // No turn runs, or the session has none yet.
        Waiting, // The turn waits on a decision, whose card already asks the user.
        Working, // The turn runs: what it does now, and when it started (Unix milliseconds).
    };
    Kind kind = Kind::Idle;
    std::string label;
    std::optional<std::int64_t> started;

    [[nodiscard]] static Activity idle() { return {}; }
    [[nodiscard]] static Activity waiting() { return {Kind::Waiting, {}, {}}; }
    [[nodiscard]] static Activity working(std::string label, std::optional<std::int64_t> started) {
        return {Kind::Working, std::move(label), started};
    }

    // The activity of the latest turn. A running turn shows its newest running call (its title,
    // else its tool) or streaming message ("Thinking…" for thinking), else "Working…".
    [[nodiscard]] static Activity of(const ActivityModel& model);
    [[nodiscard]] static Activity of(const core::Dom& dom) { return of(ActivityModel::of(dom)); }

    friend bool operator==(const Activity&, const Activity&) = default;
};

} // namespace loa::ui::session::composer
