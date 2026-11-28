#pragma once

// The decisions of every open session behind one component: cards per session, waiting badges,
// notifications through a `Notifier` while the window is in the background, and unattended
// timeouts. The app syncs it with each session's DOM, forwards messages, passes
// `DeskIntent::Answer`s to the harness and persists `PolicyChanged`. Port of
// `crates/loa-ui-session/src/decisions/desk.rs`.

#include <cstdint>
#include <expected>
#include <map>
#include <memory>
#include <optional>
#include <string>
#include <string_view>
#include <unordered_map>
#include <variant>
#include <vector>

#include "loa/ui/session/decisions/state.hpp"
#include "loa/ui/session/decisions/unattended.hpp"
#include "loa/view/session.hpp"

namespace loa::ui::session::decisions {

using loa::view::session::decisions::DeskIntent;
using loa::view::session::port::Notice;
using loa::view::session::port::Notifier;
using loa::view::session::port::NotifyError;
using loa::view::session::port::NotifyFuture;

// How often pending unattended timeouts are checked, in ms.
inline constexpr std::int64_t TICK_MS = 500;

// An unattended timeout that passed: hand `answer` to the session's harness.
struct Expired {
    std::string session;
    NodeId decision;
    loa::contracts::Answer answer;
    friend bool operator==(const Expired&, const Expired&) = default;
};

// Waiting decisions of every open session: badges, notices, unattended timeouts. Pure: time is
// passed in.
class Tracker {
  public:
    bool background = false;
    Unattended policy;

    // Reads the waiting decisions of the session in `dom` at `now_ms`. Returns a notice per
    // decision first seen waiting while in the background.
    std::vector<Notice> observe(const loa::core::Dom& dom, std::int64_t now_ms);
    void forget(std::string_view session);
    [[nodiscard]] std::size_t badge(std::string_view session) const;
    [[nodiscard]] std::size_t total() const;
    // Default answers whose timeout passed at `now_ms`, each decision once.
    std::vector<Expired> tick(std::int64_t now_ms);
    // The earliest pending timeout.
    [[nodiscard]] std::optional<std::int64_t> next_deadline() const;

  private:
    struct Waiting {
        Decision decision;
        // Unix ms the wait started: `created`, else when first seen.
        std::int64_t since = 0;
        // The decision's own valid default and deadline; wins over the policy.
        std::optional<std::pair<std::int64_t, loa::contracts::Answer>> own;
        bool fired = false;
    };
    [[nodiscard]] std::optional<std::int64_t> deadline(const Waiting& w) const;

    std::map<std::string, std::map<NodeId, Waiting>, std::less<>> sessions_;
};

namespace desk_msg {
// A card message of one session.
struct Card {
    std::string session;
    Message message;
};
// Number key or Enter for the active session's focused card.
struct Key {
    Shortcut shortcut;
};
// The app window gained (`true`) or lost focus.
struct Focused {
    bool focused = false;
};
// Check unattended timeouts at this unix ms.
struct Tick {
    std::int64_t now_ms = 0;
};
struct Notified {
    std::expected<void, NotifyError> result;
};
struct Settings {
    unattended::Message edit;
};
} // namespace desk_msg

using DeskMessage =
    std::variant<desk_msg::Card, desk_msg::Key, desk_msg::Focused, desk_msg::Tick, desk_msg::Notified, desk_msg::Settings>;

class Desk {
  public:
    // `by` is recorded on every answer given here.
    Desk(std::string by, UnattendedView policy, std::shared_ptr<const Notifier> notifier);

    // The session shown in the center; it takes the keyboard shortcuts.
    void set_active(std::optional<std::string> session) { active_ = std::move(session); }
    [[nodiscard]] const std::optional<std::string>& active() const noexcept { return active_; }
    // The local node's host name, shown on its answers instead of its key.
    void set_local_name(std::string name);
    // A peer's session this node may only watch: its cards show but can't be answered.
    void set_view_only(std::string_view session, bool view_only);

    // Brings the cards and badges of the session in `dom` in line at `now_ms`; returns the
    // notices to raise with `notify`.
    std::vector<Notice> sync(const loa::core::Dom& dom, std::int64_t now_ms);
    // One notification per notice; the app runs each off the UI thread and reports its result
    // as `desk_msg::Notified`.
    [[nodiscard]] std::vector<NotifyFuture> notify(const std::vector<Notice>& notices) const;
    // Drops a closed session.
    void forget(std::string_view session);
    // Reopens a card whose answer the harness refused.
    void answer_failed(std::string_view session, NodeId decision);

    [[nodiscard]] const Unattended& policy() const noexcept { return tracker_.policy; }
    [[nodiscard]] const Tracker& tracker() const noexcept { return tracker_; }
    std::vector<DeskIntent> update(DeskMessage message);

    // The cards of one session.
    [[nodiscard]] const State* session(std::string_view session) const;
    // The card taking shortcuts in the active session.
    [[nodiscard]] std::optional<NodeId> focused() const;
    // Waiting decisions of `session`, for its sidebar row.
    [[nodiscard]] std::size_t badge(std::string_view session) const { return tracker_.badge(session); }
    // Waiting decisions across every session.
    [[nodiscard]] std::size_t total() const { return tracker_.total(); }
    // Number keys and Enter go to the desk while the active session has a card to answer (Esc
    // is left to the app, which closes screens first).
    [[nodiscard]] bool wants_keys() const { return focused().has_value(); }
    // A `TICK_MS` clock runs while a timeout is pending.
    [[nodiscard]] bool wants_clock() const { return tracker_.next_deadline().has_value(); }

  private:
    State& state(std::string_view session);
    std::vector<DeskIntent> card(const std::string& session, Message message);

    std::string local_;
    std::optional<std::string> local_name_;
    std::map<std::string, State, std::less<>> sessions_;
    std::optional<std::string> active_;
    Tracker tracker_;
    std::shared_ptr<const Notifier> notifier_;
};

} // namespace loa::ui::session::decisions
