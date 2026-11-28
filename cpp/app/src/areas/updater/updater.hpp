#pragma once

// Updates of the app, omp and the harnesses (port of `loa-app/src/areas/updater/mod.rs`): the
// Updates screen (palette "Updates", and the sidebar's Updates row while something waits for
// the user), feed checks at startup and every `[updater].check_interval_secs`, staging, gate
// checks, hold choices, swaps between turns, rollback and the peers that are behind.

#include <chrono>
#include <expected>
#include <filesystem>
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
#include "loa/settings/updates.hpp"
#include "loa/ui/settings/updates/state.hpp"
#include "loa/updater/service.hpp"

namespace loa::p2p {
struct PeerInfo;
}

namespace loa::app::areas::updater {

namespace screen = loa::ui::settings::updates;
using loa::runtime::semver::Version;
using loa::settings::InstalledStatus;
using loa::settings::Overview;
using loa::settings::PeerInfo;
using loa::settings::PeerStanding;
using loa::settings::ProductKind;
using loa::updater::SwapOutcome;
using loa::updater::Updater;

// The app version the updater compares against (the Rust workspace's `CARGO_PKG_VERSION`).
inline constexpr std::string_view APP_VERSION = "0.1.0";

namespace msg {
// Show the Updates screen (palette, sidebar row).
using Open = to::updater::Open;
struct Screen {
    screen::Message message;
};
// A step finished for this product (`nullopt`: every product), with a notice to show.
struct Done {
    std::optional<std::string> product;
    std::optional<std::string> note;
};
// The check interval passed.
struct CheckDue {};
} // namespace msg

using Message = std::variant<msg::Open, msg::Screen, msg::Done, msg::CheckDue>;

class State final : public TypedArea<State> {
  public:
    static constexpr std::string_view ID = area_id::UPDATER;
    using Message = updater::Message;

    // Opens the updater under `<data>/updates` and checks every feed.
    [[nodiscard]] static std::pair<std::unique_ptr<State>, Task> make(Ctx& ctx);
    [[nodiscard]] static std::span<const AreaCommand<State>> COMMANDS();

    State(std::expected<Updater, std::string> updater, Version version, std::chrono::seconds interval);

    Task update_typed(Message message, Ctx& ctx);
    [[nodiscard]] QWidget* view_screen(const Ctx& ctx, const Emit& send) const override;
    void contribute(contracts::SessionConfig& config, const Target& target, const Ctx& ctx) const override;
    void before_send(const SessionId& session, contracts::Prompt& prompt, Ctx& ctx) override;
    Task on_session_event(const SessionEvent& event, Ctx& ctx) override;
    [[nodiscard]] Subscriptions subscription() const override;

    // Checks every feed, as the screen's "Check all" does.
    Task check_all(Ctx& ctx);

    [[nodiscard]] const std::expected<Updater, std::string>& service() const noexcept { return updater_; }
    [[nodiscard]] const screen::State& screen() const noexcept { return screen_; }
    [[nodiscard]] std::size_t running() const noexcept { return running_; }
    // Executables the updater installed and made current, by product id.
    [[nodiscard]] const std::map<std::string, std::filesystem::path, std::less<>>& managed() const noexcept { return managed_; }

  private:
    // Re-reads the updater's snapshot into the screen, the sidebar row and the executables new
    // sessions start.
    void refresh(Ctx& ctx);
    // Runs one request of the screen on the updater.
    Task run(screen::Intent intent, Ctx& ctx);
    // Writes the updater's pins to `[updater].pins` of the central config.
    void save_pins(const Updater& updater, Ctx& ctx) const;
    // Records how many sessions are mid turn; once none is, swaps in the versions cleared meanwhile.
    void set_running(std::size_t running, Ctx& ctx);

    std::expected<Updater, std::string> updater_;
    // Running turns, for swaps between turns only.
    std::shared_ptr<loa::updater::TurnGate> turns_;
    // Sessions mid turn.
    std::size_t running_ = 0;
    screen::State screen_;
    std::map<std::string, std::filesystem::path, std::less<>> managed_;
    // This app's version.
    Version version_;
    std::chrono::seconds interval_;
};

// Installed products with something for the user: an offer, a held or pending update, or a restart.
[[nodiscard]] std::size_t waiting(const Overview& overview);

// The peers whose hello this node has seen, against an app at `version`.
[[nodiscard]] std::vector<PeerStanding> standings(const Version& version, const std::vector<loa::p2p::PeerInfo>& known);

// The notice for a swap or rollback of `id`; errors show on the product instead.
[[nodiscard]] std::optional<std::string> swap_note(const Updater& updater, std::string_view id,
                                                   const loa::updater::Result<SwapOutcome>& outcome);

} // namespace loa::app::areas::updater
