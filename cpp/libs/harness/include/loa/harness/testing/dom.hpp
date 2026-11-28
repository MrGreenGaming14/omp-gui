#pragma once

// Adapter-test checks of the DOM conventions in `docs/ARCHITECTURE.md` (port of
// `testing/dom.rs`).

#include <optional>
#include <span>
#include <string>
#include <string_view>
#include <vector>

#include "loa/core/dom.hpp"
#include "loa/core/patch.hpp"

namespace loa::harness::testing {

// The convention a `Violation` breaks.
enum class Rule {
    Root,
    MissingStatus,
    NotTerminal,
    OpenTurn,
    TurnShape,
    CallInput,
    CallResult,
    Waiting,
    DecisionAnswer,
    DanglingRef,
    Transition,
    CloseOrder,
    Replay,
};

// The Rust `Debug` name of `rule` (`"NotTerminal"`).
[[nodiscard]] std::string_view to_string(Rule rule) noexcept;

// One broken rule and the node it was found on.
struct Violation {
    Rule rule;
    std::optional<loa::core::NodeId> node;
    std::string detail;
    friend bool operator==(const Violation&, const Violation&) = default;
};

// `Settled`: everything has ended. `Live`: the harness may be mid event.
enum class Phase { Settled, Live };

// Checks a settled DOM; empty when every convention holds.
[[nodiscard]] std::vector<Violation> check_dom(const loa::core::Dom& dom);
// Checks `dom` as seen at `phase`.
[[nodiscard]] std::vector<Violation> check_dom_in(const loa::core::Dom& dom, Phase phase);
// Replays `patches` onto an empty DOM, checking status transitions; stops at the first failure.
[[nodiscard]] std::vector<Violation> check_history(std::span<const loa::core::Patch> patches);

} // namespace loa::harness::testing
