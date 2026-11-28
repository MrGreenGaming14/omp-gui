#pragma once

// Harness switching and availability (port of `loa-app/src/areas/harness/mod.rs`): the
// composer's harness picker, the palette's "Switch harness" commands and the Harnesses tab,
// which shows what each adapter's probe found and lets the user point a harness at another
// executable.

#include <cstdint>
#include <map>
#include <memory>
#include <optional>
#include <span>
#include <string>
#include <string_view>
#include <utility>
#include <variant>
#include <vector>

#include "loa/app/area.hpp"
#include "loa/app/routes.hpp"
#include "loa/contracts/availability.hpp"
#include "loa/settings/app/schema.hpp"

namespace loa::app::areas::harness {

// Id of the Harnesses panel tab.
inline constexpr std::string_view TAB = "harnesses";
// Palette command id that probes every harness again; the others switch to the harness their
// id names.
inline constexpr std::string_view RECHECK = "harness.recheck";

namespace msg {
// Route new sessions to the harness with this id.
using Pick = to::harness::Pick;
// Probe every harness again.
struct Recheck {};
// Harness `id`'s probe number `probe` answered.
struct Probed {
    std::string id;
    std::uint64_t probe = 0;
    contracts::Availability availability;
};
// The executable field of harness `id` changed.
struct EditExecutable {
    std::string id;
    std::string value;
};
// Run harness `id` on its executable field from now on; empty restores the default.
struct SaveExecutable {
    std::string id;
};
} // namespace msg

using Message = std::variant<msg::Pick, msg::Recheck, msg::Probed, msg::EditExecutable, msg::SaveExecutable>;

// What the Harnesses tab says about a probe result; `nullopt` while probing.
[[nodiscard]] std::string_view status_word(std::optional<contracts::AvailabilityStatus> status);

class State final : public TypedArea<State> {
  public:
    static constexpr std::string_view ID = area_id::HARNESS;
    using Message = harness::Message;

    // Applies `[harness]` from the central config (executables, then the default harness) and
    // probes every harness.
    [[nodiscard]] static std::pair<std::unique_ptr<State>, Task> make(Ctx& ctx);
    [[nodiscard]] static std::span<const AreaCommand<State>> COMMANDS();

    Task update_typed(Message message, Ctx& ctx);
    [[nodiscard]] std::vector<PanelTab> panel_tabs() const override;
    [[nodiscard]] QWidget* view_tab(std::string_view id, const Ctx& ctx, const Emit& send) const override;
    Task on_session_event(const SessionEvent& event, Ctx& ctx) override;

    // Probes every registered harness.
    Task probe_all(const Ctx& ctx);
    // Routes new sessions to harness `id` and makes it the default.
    Task switch_to(std::string id, Ctx& ctx);

    // Harnesses whose latest probe hasn't answered.
    [[nodiscard]] std::span<const std::string> probing() const noexcept { return probing_; }
    // The executable field of harness `id`.
    [[nodiscard]] std::string_view draft(std::string_view id) const;
    // `[harness] default`: the harness new sessions start on.
    [[nodiscard]] const std::optional<std::string>& default_harness() const noexcept { return default_; }

  private:
    void apply(const settings::app::HarnessSettings& settings, Ctx& ctx);
    Task probe(std::string id, const Ctx& ctx);
    void probed(const std::string& id, std::uint64_t probe, contracts::Availability availability, Ctx& ctx);
    Task save_executable(std::string id, Ctx& ctx);
    [[nodiscard]] QWidget* harness_row(const app_runtime::Harness& harness, std::optional<std::string_view> selected,
                                       const Emit& send) const;

    // Number of the latest probe per harness; older answers are dropped.
    std::map<std::string, std::uint64_t, std::less<>> probes_;
    std::vector<std::string> probing_;
    std::uint64_t next_probe_ = 0;
    std::map<std::string, std::string, std::less<>> drafts_;
    std::optional<std::string> default_;
};

} // namespace loa::app::areas::harness
