#pragma once

// The left sidebar: machine switcher, New session, Search, Projects (the highlighted one lists
// its sessions), Pinned, Recents and a footer. Port of `loa-ui-shell/src/sidebar.rs`.

#include <cstddef>
#include <optional>
#include <set>
#include <string>
#include <string_view>
#include <utility>
#include <variant>
#include <vector>

#include "loa/ui/shell/data.hpp"
#include "loa/ui/shell/task.hpp"
#include "loa/view/shell.hpp"

namespace loa::ui::shell::sidebar {

using loa::view::shell::NodeFilter;
using loa::view::shell::NodeStatus;
using loa::view::shell::SessionSummary;
using loa::view::shell::SidebarIntent;
using loa::view::shell::UsageMeter;

inline constexpr std::string_view SEARCH_ID = "loa-sidebar-search";
// Recents rows drawn at first, and added by each "Show more".
inline constexpr std::size_t RECENTS_PAGE = 50;
// Sessions listed under the highlighted project; the rest are on its page.
inline constexpr std::size_t PROJECT_SESSIONS = 8;

namespace msg {
struct Emit {
    SidebarIntent intent;
    friend bool operator==(const Emit&, const Emit&) = default;
};
struct Search {
    std::string query;
    friend bool operator==(const Search&, const Search&) = default;
};
struct OpenPeers {
    friend bool operator==(const OpenPeers&, const OpenPeers&) = default;
};
struct ToggleMachines {
    friend bool operator==(const ToggleMachines&, const ToggleMachines&) = default;
};
struct PickMachine {
    NodeFilter filter;
    friend bool operator==(const PickMachine&, const PickMachine&) = default;
};
struct OpenSession {
    std::string id;
    friend bool operator==(const OpenSession&, const OpenSession&) = default;
};
struct TogglePin {
    std::string id;
    friend bool operator==(const TogglePin&, const TogglePin&) = default;
};
struct ToggleProjectPin {
    std::string project;
    friend bool operator==(const ToggleProjectPin&, const ToggleProjectPin&) = default;
};
// The row under Recents that lists the next page of sessions.
struct ShowMoreRecents {
    friend bool operator==(const ShowMoreRecents&, const ShowMoreRecents&) = default;
};
// The folder button on a session row: opens or closes its project list.
struct ToggleMoveMenu {
    std::string id;
    friend bool operator==(const ToggleMoveMenu&, const ToggleMoveMenu&) = default;
};
// A project picked in a session's list; `nullopt` is "No project".
struct MoveTo {
    std::string id;
    std::optional<std::string> project;
    friend bool operator==(const MoveTo&, const MoveTo&) = default;
};
} // namespace msg

using Message = std::variant<msg::Emit, msg::Search, msg::OpenPeers, msg::ToggleMachines, msg::PickMachine, msg::OpenSession,
                             msg::TogglePin, msg::ToggleProjectPin, msg::ShowMoreRecents, msg::ToggleMoveMenu, msg::MoveTo>;
using Event = SidebarIntent;

class State {
  public:
    void set_sessions(std::vector<SessionSummary> sessions);
    void upsert_session(SessionSummary summary);
    // `upsert_session` for many sessions, sorting once.
    void upsert_sessions(std::vector<SessionSummary> summaries);
    void set_pins(std::vector<std::string> pins);
    [[nodiscard]] const std::set<std::string, std::less<>>& pins() const { return pins_; }
    void remove_session(std::string_view id);
    // Marks whether session `id` is actively running a turn, separate from its DOM summary.
    void set_running(std::string_view id, bool running);
    [[nodiscard]] bool is_running(std::string_view id) const { return running_.contains(id); }
    // Whether session `id` displays the running indicator; a waiting decision takes precedence.
    [[nodiscard]] bool shows_running(std::string_view id) const;
    [[nodiscard]] bool row_running(const SessionSummary& s) const { return !s.waiting && is_running(s.id); }
    [[nodiscard]] const SessionSummary* session(std::string_view id) const;
    void set_pinned_projects(std::vector<std::string> pinned);
    [[nodiscard]] const std::set<std::string, std::less<>>& pinned_projects() const { return pinned_projects_; }
    void set_projects(std::vector<std::string> projects);
    [[nodiscard]] const std::vector<std::string>& all_projects() const { return projects_; }
    [[nodiscard]] const Nodes& nodes() const { return nodes_; }
    void set_nodes(std::vector<NodeStatus> nodes);
    void set_active(std::optional<std::string> id) { active_ = std::move(id); }
    [[nodiscard]] const std::optional<std::string>& active() const { return active_; }
    void set_updates(std::size_t count) { updates_ = count; }
    [[nodiscard]] std::size_t updates() const { return updates_; }
    void set_usage(std::optional<UsageMeter> usage) { usage_ = std::move(usage); }
    [[nodiscard]] const std::optional<UsageMeter>& usage() const { return usage_; }
    // Pinned sessions on the listed machine that match the search, most recently updated first.
    [[nodiscard]] std::vector<const SessionSummary*> pinned() const;
    // Unpinned sessions on the listed machine that match the search, most recently updated
    // first; the view draws the first pages of them.
    [[nodiscard]] std::vector<const SessionSummary*> recents() const;
    // Projects whose name matches the search: pinned ones by name, then the rest in the order given.
    [[nodiscard]] std::vector<std::string_view> projects() const;
    // How many Recents rows the view draws.
    [[nodiscard]] std::size_t recents_limit() const { return RECENTS_PAGE * (recents_pages_ + 1); }
    // The sessions under `project`'s row: pinned first then newest, at most `PROJECT_SESSIONS`,
    // and how many more there are.
    [[nodiscard]] std::pair<std::vector<const SessionSummary*>, std::size_t> project_sessions(std::string_view project) const;
    // The project whose row lists its sessions: the shown one, else the active one, else the active session's.
    [[nodiscard]] std::optional<std::string_view> highlighted_project() const;
    // Name, sub line and whether the dot shows online, for a machine choice.
    struct Machine {
        std::string name, sub;
        bool online = false;
    };
    [[nodiscard]] Machine describe(const NodeFilter& filter) const;
    // Sessions and those waiting for the user on the machines `filter` lists.
    [[nodiscard]] std::pair<std::size_t, std::size_t> tally(const NodeFilter& filter) const;

    [[nodiscard]] const std::string& query() const { return query_; }
    [[nodiscard]] const NodeFilter& filter() const { return filter_; }
    [[nodiscard]] bool machines_open() const { return machines_open_; }
    [[nodiscard]] const std::optional<std::string>& move_menu() const { return move_menu_; }

    std::optional<std::string> active_project;
    std::optional<std::string> shown_project;

    std::pair<Task, std::optional<Event>> update(Message message);

  private:
    struct Entry {
        SessionSummary summary;
        // Lowercase search text, rebuilt when sessions or node names change.
        std::string haystack;
    };
    void put(SessionSummary summary);
    Entry entry(SessionSummary s);
    void reorder();
    void refilter_projects();

    std::vector<Entry> sessions_;
    std::vector<std::string> projects_;
    Nodes nodes_;
    std::set<std::string, std::less<>> pins_;
    std::set<std::string, std::less<>> pinned_projects_;
    // Live local sessions currently running a turn; kept separate from DOM-derived summaries.
    std::set<std::string, std::less<>> running_;
    std::string query_;
    std::string lower_query_;
    std::optional<std::string> active_;
    std::size_t updates_ = 0;
    // `nullopt` lists this computer.
    std::optional<NodeFilter> machine_;
    // The machine whose sessions are listed: `machine_` resolved against the nodes.
    NodeFilter filter_;
    // Indices into `sessions_` of the listed pinned and unpinned sessions, newest first.
    std::vector<std::size_t> pinned_order_;
    std::vector<std::size_t> recent_order_;
    // Indices into `projects_` of the listed projects, pinned ones first.
    std::vector<std::size_t> visible_projects_;
    // "Show more" pages of Recents opened beyond the first.
    std::size_t recents_pages_ = 0;
    bool machines_open_ = false;
    std::optional<UsageMeter> usage_;
    // The session whose "Move to project" list is open.
    std::optional<std::string> move_menu_;
};

} // namespace loa::ui::shell::sidebar
