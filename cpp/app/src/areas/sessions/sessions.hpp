#pragma once

// Session features around the chat: the Agents and Session tree tabs, branching, plan mode,
// rules and the advisor, model roles, import and export (port of
// `loa-app/src/areas/sessions/`). One area, its parts split over `branch.cpp`, `hooks.cpp`,
// `plan.cpp`, `subagents.cpp`, `transfer.cpp` and `tree.cpp` as the Rust modules are.

#include <chrono>
#include <cstdint>
#include <expected>
#include <filesystem>
#include <map>
#include <memory>
#include <mutex>
#include <optional>
#include <set>
#include <span>
#include <string>
#include <string_view>
#include <utility>
#include <variant>
#include <vector>

#include "loa/app/area.hpp"
#include "loa/app/routes.hpp"
#include "loa/contracts/harness.hpp"
#include "loa/core/dom.hpp"
#include "loa/core/patch.hpp"
#include "loa/harness/kit/native.hpp"
#include "loa/projects/digester.hpp"
#include "loa/session/hooks.hpp"
#include "loa/session/plan.hpp"
#include "loa/session/session.hpp"
#include "loa/ui/panel/agents.hpp"
#include "loa/ui/panel/session_tree.hpp"
#include "loa/ui/session/transcript/state.hpp"

namespace loa::app::areas::sessions {

namespace agents = loa::ui::panel::agents;
namespace session_tree = loa::ui::panel::session_tree;
namespace chat = loa::ui::session::transcript;
// The projects library, not the projects area beside this one.
namespace projects = loa::projects;
using core::NodeId;

// Model or omp role of the app's small-model calls: compaction summaries and advisor notes.
inline constexpr std::string_view SMALL_MODEL = "@smol";
// How often the Agents tab advances elapsed times while a job runs.
inline constexpr std::chrono::milliseconds TICK{1000};

// omp's model roles with their candidate models in order.
using Roles = std::vector<std::pair<std::string, std::vector<std::string>>>;

// Reads omp's model roles, candidates in order, from `config.yml` alone.
[[nodiscard]] std::expected<Roles, std::string> model_roles(const std::filesystem::path& path);

// ---- plan ----------------------------------------------------------------------------------

// A plan permit carried by a message: copies share it, the first `take` gets it.
class Held {
  public:
    Held() = default;
    explicit Held(session::PermitPtr permit) : slot_(std::make_shared<Slot>()) { slot_->permit = std::move(permit); }
    [[nodiscard]] session::PermitPtr take() const {
        if (!slot_) return nullptr;
        std::lock_guard lock(slot_->mutex);
        return std::move(slot_->permit);
    }

  private:
    struct Slot {
        std::mutex mutex;
        session::PermitPtr permit;
    };
    std::shared_ptr<Slot> slot_;
};

// What follows a plan review.
namespace plan_next {
struct Execute {
    std::string prompt;
};
struct Revise {
    std::string prompt;
    Held permit;
    NodeId step;
};
} // namespace plan_next
using PlanNext = std::variant<plan_next::Execute, plan_next::Revise>;

// A session in app plan mode.
struct Plan {
    // Keeps the read-only gate on.
    session::PermitPtr permit;
    // The open plan step; `nullopt` while a plan waits for review.
    std::optional<NodeId> step;
};

// ---- subagents -----------------------------------------------------------------------------

// Outcome error of `HarnessSession::message_subagent`, keeping addressability distinct.
struct SendError {
    enum class Kind : std::uint8_t { NotAddressable, Other };
    Kind kind = Kind::Other;
    std::string message;
};

// One open child subtab of a parent conversation.
struct ChildTab {
    ChildTab(std::string id, std::uint64_t generation, std::string title, std::optional<core::JobStatus> status);

    // Whether the subagent may still take messages: the harness said so, or its job still runs
    // (a first read that failed or hasn't arrived yet).
    [[nodiscard]] bool live() const { return addressable || status == core::JobStatus::Running; }

    std::string id;
    // Distinguishes this tab from an earlier one of the same subagent that was closed, so a
    // read or send started by that one never lands here.
    std::uint64_t generation = 0;
    std::string title;
    std::string tab_label;
    std::optional<core::JobStatus> status;
    std::optional<core::Dom> dom;
    chat::State transcript;
    bool addressable = false;
    // A send result arrived while a read was in flight: that read started before it, so its
    // `addressable` is out of date and the follow-up read decides.
    bool addressable_stale = false;
    bool loading = false;
    bool refetch_needed = false;
    std::optional<std::string> fetch_error;
    std::string draft;
    bool sending = false;
    std::optional<std::string> send_error;
};

// Open child subtabs and the selection of one parent session.
struct ParentTabs {
    std::vector<std::string> order;
    std::map<std::string, ChildTab, std::less<>> children;
    std::optional<std::string> selected;
};

// Child subtabs keyed by parent session id.
class SubagentTabs {
  public:
    void open(std::string_view parent, const std::string& child, std::string title, std::optional<core::JobStatus> status);
    void select(std::string_view parent, std::optional<std::string> child);
    void close(std::string_view parent, std::string_view child);
    void remove_parent(std::string_view parent) {
        if (auto it = parents.find(parent); it != parents.end()) parents.erase(it);
    }
    [[nodiscard]] std::optional<std::string_view> selected_of(std::string_view parent) const;
    // The selected child of `parent` when it may still take messages, so it is worth polling.
    [[nodiscard]] std::optional<std::string> selected_live(std::string_view parent) const;
    // The tab of `child` under `parent`; null when it isn't open.
    [[nodiscard]] ChildTab* tab(std::string_view parent, std::string_view child);
    [[nodiscard]] const ChildTab* tab(std::string_view parent, std::string_view child) const;
    // The open tab of `child` under `parent`, if it is still the tab of `generation`.
    [[nodiscard]] ChildTab* current(std::string_view parent, std::string_view child, std::uint64_t generation);

    std::map<std::string, ParentTabs, std::less<>> parents;

  private:
    std::uint64_t next_generation_ = 0;
};

// ---- session tree --------------------------------------------------------------------------

namespace tree {

// Where a session branched off.
struct Link {
    std::optional<std::string> parent;
    std::optional<std::uint64_t> forked_at;
    [[nodiscard]] static Link of(const core::Dom& dom);
    friend bool operator==(const Link&, const Link&) = default;
};

using Seqs = std::vector<std::pair<NodeId, std::uint64_t>>;

// Turn rows of one session, as read from its journal.
struct Turns {
    // The journal sequence that started each turn, in journal order.
    Seqs seqs;
    // First line of each turn; kept only for a session closed at read time.
    std::optional<std::map<NodeId, std::string>> lines;
};

// What one background read of the journals found.
struct Loaded {
    bool scanned = false;
    std::map<std::string, Link, std::less<>> links;
    std::map<std::string, Turns, std::less<>> turns;
};

// The Session tree tab's data: the fork family of the active session, from the
// `parent_session` / `forked_at` links on each session's root and the turn rows of the journal.
// Journal reads run off the UI thread; the tab shows what it knows and refreshes when one lands.
class Tree {
  public:
    session_tree::State view;

    // Drops a deleted session.
    void forget(std::string_view id);
    // Notes a turn that session `id` just gained, so the tab shows it before the next journal read.
    void record_turn(std::string_view id, NodeId turn, std::uint64_t seq);
    // Rebuilds the tab for the active session's family. Turns the tab lacks are read off the UI
    // thread; the result arrives as `msg::TreeLoaded`.
    [[nodiscard]] Task refresh(const Ctx& ctx);
    // Applies a background read. Turns recorded while it ran are kept.
    void merge(Loaded loaded);

  private:
    [[nodiscard]] bool needs_read(const Ctx& ctx, std::string_view id) const;
    [[nodiscard]] std::vector<loa::view::panel::TurnInfo> turns_of(const Ctx& ctx, std::string_view id) const;

    // Links of every stored session; read once, then kept up to date.
    std::map<std::string, Link, std::less<>> links_;
    bool scanned_ = false;
    // Turn rows per session, in journal order; kept up to date by `record_turn`.
    std::map<std::string, Seqs, std::less<>> seqs_;
    // First lines of sessions that were closed when their turns were read.
    std::map<std::string, std::map<NodeId, std::string>, std::less<>> closed_lines_;
    // Turns recorded while a read was in flight; merged into its result.
    std::map<std::string, Seqs, std::less<>> pending_;
    bool loading_ = false;
};

// First line of the user's prompt in `turn`.
[[nodiscard]] std::string first_line(const core::Dom& dom, NodeId turn);

} // namespace tree

// ---- import and export ---------------------------------------------------------------------

namespace transfer {

// Discovered sessions listed at once; older ones are picked by path.
inline constexpr std::size_t LISTED = 200;

namespace msg {
// The import path field changed.
struct Path {
    std::string path;
};
// Import the file in the path field.
struct ImportPath {};
// Import this discovered file.
struct Import {
    std::filesystem::path path;
};
// Look for native sessions again.
struct Refresh {};
struct ExportPath {
    std::string path;
};
// Export the active session: as XML, or in its harness's own format.
struct Export {
    bool xml = false;
};
struct Close {};
} // namespace msg
using Message = std::variant<msg::Path, msg::ImportPath, msg::Import, msg::Refresh, msg::ExportPath, msg::Export, msg::Close>;

struct State {
    std::vector<loa::harness::kit::NativeSession> found;
    std::string path;
    // Where the next export goes; the default path when empty.
    std::string export_path;
    // What the last import or export did.
    std::optional<std::string> status;
};

// The import and export screen; `active` is the active session's id and harness.
[[nodiscard]] QWidget* view(const State& state, std::optional<std::pair<std::string_view, std::string_view>> active,
                            std::int64_t now, const std::function<void(Message)>& send);

} // namespace transfer

// ---- messages ------------------------------------------------------------------------------

namespace msg {
struct Agents {
    agents::Message message;
};
struct Jobs {
    agents::Message message;
};
struct SubagentSelect {
    std::optional<std::string> child;
};
struct SubagentClose {
    std::string child;
};
struct SubagentChat {
    std::string parent, child;
    chat::Message message;
};
struct SubagentDraft {
    std::string parent, child, text;
};
struct SubagentSend {
    std::string parent, child;
};
// A child tab's transcript read finished; `generation` names the tab that asked.
struct SubagentTranscriptLoaded {
    std::string parent, child;
    std::uint64_t generation = 0;
    std::expected<contracts::SubagentTranscript, std::string> result;
};
// A message to a subagent was delivered or refused; `generation` names the tab that sent it.
struct SubagentMessageSent {
    std::string parent, child;
    std::uint64_t generation = 0;
    std::string text;
    std::expected<void, SendError> result;
};
struct Tree {
    session_tree::Message message;
};
// Something the user did in the transcript.
using Chat = to::sessions::Chat;
// The user picked a mode in the composer.
using Mode = to::sessions::Mode;
// Delete a session and its journal.
using Delete = to::sessions::Delete;
// Answer to a plan review the app raised, from the decision card.
using AnswerPlan = to::sessions::AnswerPlan;
struct PlanAnswered {
    SessionId session;
    NodeId decision;
    std::expected<PlanNext, std::string> result;
};
// The advisor reviewed `turn`.
struct Advised {
    SessionId session;
    NodeId turn;
    std::expected<std::optional<std::string>, std::string> result;
};
// A role switch finished: the role and what it switched to.
struct RoleSet {
    std::string session;
    std::expected<std::pair<std::string, session::RoleSelected>, std::string> result;
};
// Where omp's `config.yml` is, for the model roles `/role` and the palette offer.
struct ConfigLocated {
    std::expected<std::filesystem::path, std::string> result;
};
struct Transfer {
    transfer::Message message;
};
// Current time, unix ms, for the Agents tab.
struct Tick {
    std::int64_t now = 0;
};
// A harness call returned.
struct Done {
    std::expected<void, std::string> result;
};
// A Session tree read off the UI thread finished.
struct TreeLoaded {
    std::shared_ptr<const tree::Loaded> loaded;
};
} // namespace msg

using Message =
    std::variant<msg::Agents, msg::Jobs, msg::SubagentSelect, msg::SubagentClose, msg::SubagentChat, msg::SubagentDraft,
                 msg::SubagentSend, msg::SubagentTranscriptLoaded, msg::SubagentMessageSent, msg::Tree, msg::Chat,
                 msg::Mode, msg::Delete, msg::AnswerPlan, msg::PlanAnswered, msg::Advised, msg::RoleSet,
                 msg::ConfigLocated, msg::Transfer, msg::Tick, msg::Done, msg::TreeLoaded>;

// How a new session branches off its source.
namespace branch {
// A copy of the source up to its target.
struct Fork {
    core::Dom target;
};
// The source `current` taken back to `target`.
struct Rewind {
    core::Dom current;
    core::Dom target;
};
// A summary of `source` written by the small model, continued on `running`'s harness.
struct Compact {
    app_runtime::HarnessPtr running;
    core::Dom source;
    std::shared_ptr<bus::SessionWriter> writer;
    std::shared_ptr<const projects::Summarizers> small;
    std::optional<std::string> instructions;
};
} // namespace branch
using Branch = std::variant<branch::Fork, branch::Rewind, branch::Compact>;

// ---- the area ------------------------------------------------------------------------------

class State final : public TypedArea<State>, public SessionsPort {
  public:
    static constexpr std::string_view ID = area_id::SESSIONS;
    using Message = sessions::Message;

    State(std::filesystem::path home, std::shared_ptr<const projects::Summarizers> small,
          std::shared_ptr<const contracts::UrlOpener> opener);

    [[nodiscard]] static std::pair<std::unique_ptr<State>, Task> make(Ctx& ctx);
    [[nodiscard]] static std::span<const AreaCommand<State>> COMMANDS();

    Task update_typed(Message message, Ctx& ctx);
    [[nodiscard]] std::vector<PanelTab> panel_tabs() const override;
    [[nodiscard]] QWidget* view_tab(std::string_view id, const Ctx& ctx, const Emit& send) const override;
    [[nodiscard]] QWidget* view_screen(const Ctx& ctx, const Emit& send) const override;
    void contribute(contracts::SessionConfig& config, const Target& target, const Ctx& ctx) const override;
    void before_send(const SessionId& session, contracts::Prompt& prompt, Ctx& ctx) override;
    Task on_session_event(const SessionEvent& event, Ctx& ctx) override;
    [[nodiscard]] Subscriptions subscription() const override;

    // SessionsPort
    bool hold(std::string_view id) override;
    void on_append(const SessionId& id, const core::Patch& patch, Ctx& ctx) override;
    [[nodiscard]] std::optional<std::pair<QWidget*, QWidget*>> view_child_center(const Ctx& ctx,
                                                                                 const Emit& send) const override;

    [[nodiscard]] bool role_switch_pending(std::string_view id) const;
    // Whether the active session is in app plan mode.
    [[nodiscard]] bool in_plan(const Ctx& ctx) const;
    // omp's model roles, read again when `config.yml` changed; `nullopt` until it is located.
    [[nodiscard]] std::optional<std::expected<Roles, std::string>> roles() const;

    // The commands' entry points.
    Task role(Ctx& ctx, std::string role);
    Task pick_mode(core::SessionMode mode, Ctx& ctx);
    Task compact(Ctx& ctx, std::string_view instructions);
    Task remove(Ctx& ctx, const std::string& id);
    void fork_active(Ctx& ctx);
    void show_import(Ctx& ctx);
    void export_session(Ctx& ctx, bool xml, std::optional<std::filesystem::path> path);

    agents::State agents = agents::State::agents();
    agents::State jobs = agents::State::background_jobs();
    SubagentTabs subagents;
    tree::Tree tree;
    transfer::State transfer;
    // Compacted successor id -> the session it replaces.
    std::map<std::string, std::string, std::less<>> compacted;
    // The small model; null when omp, which runs it, isn't installed.
    std::shared_ptr<const projects::Summarizers> small;

  private:
    Task reload(Ctx& ctx);
    // The role switches of `id` are over, whatever their outcome: the prompts held for them go out.
    void release(Ctx& ctx, const std::string& id);
    Task switch_role(Ctx& ctx, std::string id, std::string role);
    void open_link(Ctx& ctx, std::string_view link) const;
    Task agents_event(std::optional<agents::Event> event, bool jobs_pane, Ctx& ctx);
    Task interrupt_job(Ctx& ctx) const;

    // branch.cpp
    void spawn_branch(Ctx& ctx, const std::string& source, Branch branch);
    void fork_at(Ctx& ctx, const std::string& id, NodeId turn);
    void rewind(Ctx& ctx, const std::string& id, NodeId node);

    // hooks.cpp
    [[nodiscard]] std::optional<std::string> rules(const std::filesystem::path& cwd) const;
    void start_hooks(contracts::SessionConfig& config, const Target& target) const;
    void hook_prompt(const Ctx& ctx, const SessionId& session, contracts::Prompt& prompt);
    [[nodiscard]] Task advise(const Ctx& ctx, const SessionId& session, NodeId turn) const;
    void advised(Ctx& ctx, const SessionId& session, NodeId turn, std::expected<std::optional<std::string>, std::string> result);

    // plan.cpp
    void restore_plan(Ctx& ctx);
    void submit_plan(Ctx& ctx, const SessionId& session, NodeId turn);
    Task answer_plan(Ctx& ctx, const SessionId& session, NodeId decision, contracts::Answer answer);
    void plan_answered(Ctx& ctx, const SessionId& session, NodeId decision, std::expected<PlanNext, std::string> result);

    // subagents.cpp
    void sync_active_parent(const Ctx& ctx);
    [[nodiscard]] bool should_poll_subagent() const;
    Task refresh_selected_subagent_on_activate(const Ctx& ctx);
    Task open_child_subtab(const Ctx& ctx, std::string child);
    Task select_child_subtab(const Ctx& ctx, std::optional<std::string> child);
    void close_child_subtab(const Ctx& ctx, const std::string& child);
    Task poll_selected_subagent(const Ctx& ctx);
    Task fetch_child_transcript(const Ctx& ctx, const std::string& parent, const std::string& child);
    Task on_transcript_loaded(const Ctx& ctx, msg::SubagentTranscriptLoaded loaded);
    void set_child_draft(const Ctx& ctx, const std::string& parent, const std::string& child, std::string text);
    Task send_child_message(const Ctx& ctx, const std::string& parent, const std::string& child);
    Task on_message_sent(const Ctx& ctx, msg::SubagentMessageSent sent);
    Task update_child_chat(Ctx& ctx, const std::string& parent, const std::string& child, chat::Message message);
    void detach_child_transcript(Ctx& ctx, const std::string& reference);
    Task on_job_patch(const Ctx& ctx, std::string_view session, NodeId node);
    [[nodiscard]] bool can_message_child(const Ctx& ctx, std::string_view parent, std::string_view child) const;
    [[nodiscard]] QWidget* view_child_subtab(const Ctx& ctx, const app_runtime::Session& parent, const ChildTab& child,
                                             const Emit& send) const;

    // transfer.cpp
    void transfer_update(Ctx& ctx, transfer::Message message);
    void import(Ctx& ctx, const std::filesystem::path& path);

    std::optional<std::string> active_parent_;
    bool active_subagent_messages_ = false;
    // Read-only tool gate of app plan mode.
    std::shared_ptr<session::plan::PlanGate> gate_ = session::plan::PlanGate::make();
    std::map<std::string, Plan, std::less<>> plans_;
    // Rules and advisor state per session whose harness lacks them.
    mutable std::mutex hooks_mutex_;
    mutable std::map<std::string, session::hooks::TurnHooks, std::less<>> hooks_;
    // Home directory: native sessions, omp's rules and config.
    std::filesystem::path home_;
    // Opens links clicked in the transcript.
    std::shared_ptr<const contracts::UrlOpener> opener_;
    // The fork or rewind being started, to show the Session tab once it is active.
    std::optional<std::string> branching_;
    // Roles picked while the session ran a turn or switched roles: applied once it is idle.
    std::map<std::string, std::string, std::less<>> pending_roles_;
    std::set<std::string, std::less<>> switching_roles_;
    // Sessions whose composer holds its queued prompts until their role switch ends.
    std::set<std::string, std::less<>> held_;
    // The role each session's next prompt announces.
    std::map<std::string, std::string, std::less<>> role_notes_;
    // omp's `config.yml`, or why it couldn't be located; `nullopt` until asked.
    std::optional<std::expected<std::filesystem::path, std::string>> config_;
    // Its model roles, cached by its modified time.
    mutable std::optional<std::pair<std::filesystem::file_time_type, Roles>> roles_cache_;
};

// Shared helpers of the area's parts.
namespace detail {
// The session's mode, `normal` unless the DOM says otherwise.
[[nodiscard]] core::SessionMode mode(const core::Dom& dom);
// The session's title as the sidebar shows it.
[[nodiscard]] std::string title_of(const core::Dom& dom);
[[nodiscard]] contracts::Capabilities capabilities(const Ctx& ctx, std::string_view harness);
// Whether `harness` runs on an approval mode that asks before tools that write or execute.
[[nodiscard]] bool asks_first(const Ctx& ctx, std::string_view harness);
// The composer's `/` commands: the app's, then the harness's own from the latest list its
// adapter recorded in `dom`.
[[nodiscard]] std::vector<loa::view::session::composer::Command> composer_commands(
    const std::vector<loa::view::session::composer::Command>& app, const core::Dom* dom);
} // namespace detail

} // namespace loa::app::areas::sessions
