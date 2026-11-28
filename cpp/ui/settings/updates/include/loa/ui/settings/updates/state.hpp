#pragma once

// Updates: what each watched product runs and what its feed offers, staged versions, the gate
// report of the last check with its choices (update anyway, pin, wait), swaps between sessions,
// rollback, app signatures and the peers that are behind, drawn straight from the updater's
// status. The app runs the `Intent`s and calls `State::finish` when a step is done.
// Port of `crates/loa-ui-settings/src/updates.rs`.

#include <array>
#include <cstdint>
#include <map>
#include <optional>
#include <string>
#include <string_view>
#include <utility>
#include <variant>
#include <vector>

#include "loa/core/value.hpp"
#include "loa/ui/kit/style.hpp"
#include "loa/ui/kit/widgets.hpp"
#include "loa/view/settings_updates.hpp"

namespace loa::ui::settings::updates {

namespace vu = loa::view::settings::updates;
using loa::ui::kit::Tone;
using vu::Data;
using vu::HoldChoice;
using vu::Intent;
using vu::PeerStanding;
using vu::Product;

// A status line in its tone when the rule holds for a product, given whether a turn runs.
struct Rule {
    Tone tone;
    std::optional<std::string> (*line)(const Product&, bool turn_running);
};

// A product's status line: the first rule with a line wins; the last always matches.
extern const std::array<Rule, 12> STATUS_RULES;

// The status line of `p` and its tone.
[[nodiscard]] std::pair<Tone, std::string> status(const Product& p, bool turn_running);

// A peer's standing: it can't talk without a shared protocol, else its version against this app.
[[nodiscard]] std::pair<Tone, std::string> standing(const PeerStanding& p);

// A step the app runs for a product, named by its busy label.
enum class Step : std::uint8_t { Checking, Downloading, RunningChecks, Verifying, Swapping };

namespace local {
// Shows or hides a product's details.
struct Toggle {
    std::string id;
    friend bool operator==(const Toggle&, const Toggle&) = default;
};
// Starts a step (or every check), marking its products busy.
struct Start {
    Intent intent;
    friend bool operator==(const Start&, const Start&) = default;
};
} // namespace local
using Local = std::variant<local::Toggle, local::Start>;
using Message = loa::ui::kit::widgets::Msg<Local, Intent>;

struct State : loa::ui::kit::widgets::Screen<Local, Intent> {
    Data data;
    // The product whose details are open.
    std::optional<std::string> selected;

    State() = default;
    explicit State(Data d) : data(std::move(d)) {}

    // Replaces the snapshot, keeping the open product and running steps.
    void set_data(Data d);
    [[nodiscard]] std::optional<Step> busy(std::string_view id) const;
    // Marks `id`'s step done.
    void finish(std::string_view id);
    // Marks every step done (after checking every product).
    void finish_all();
    // Whether any listed product has no step running.
    [[nodiscard]] bool any_idle() const;

    // A product with a step running takes no further requests.
    std::optional<Intent> reduce(Local m) override;
    // Instant edits of a listed, idle product.
    [[nodiscard]] bool guard(const Intent& e) const override;

  private:
    [[nodiscard]] bool idle(std::string_view id) const;
    std::map<std::string, Step, std::less<>> busy_;
};

} // namespace loa::ui::settings::updates

namespace loa::core {
template <>
struct StrEnum<loa::ui::settings::updates::Step> {
    static constexpr std::array<std::string_view, 5> values{"Checking feed\u2026", "Downloading\u2026", "Running gate checks\u2026",
                                                           "Checking signature\u2026", "Swapping\u2026"};
};
} // namespace loa::core
