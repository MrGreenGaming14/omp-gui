#pragma once

// Live sessions: the runtime's copy of each open session's DOM, the feed that
// keeps it in step with the session writer, and the Recents listing (port of
// `loa_runtime::session`).

#include <chrono>
#include <cstdint>
#include <expected>
#include <filesystem>
#include <map>
#include <memory>
#include <optional>
#include <string>
#include <string_view>
#include <tuple>
#include <utility>
#include <variant>
#include <vector>

#include "loa/app_runtime/source.hpp"
#include "loa/bus/queue.hpp"
#include "loa/bus/store.hpp"
#include "loa/bus/writer.hpp"
#include "loa/contracts/harness.hpp"
#include "loa/contracts/session.hpp"
#include "loa/core/dom.hpp"
#include "loa/core/patch.hpp"
#include "loa/session/library.hpp"

namespace loa::app_runtime {

using bus::SessionId;
using HarnessPtr = std::shared_ptr<contracts::HarnessSession>;
using PatchPtr = std::shared_ptr<const core::Patch>;

// How long a feed gathers the events that arrive while it waits: one frame, so a streamed
// burst reaches the app as one update.
inline constexpr std::chrono::milliseconds FRAME{16};

// What happened to a session, as every app area sees it.
namespace session_event {
// This session became the one the chat shows; `nullopt` when none is.
struct Activated {
    std::optional<SessionId> id;
};
// The session's DOM was replaced wholesale: started, opened from its journal, or resynced
// after its feed fell behind.
struct Loaded {
    SessionId id;
};
// One patch was applied.
struct Patch {
    SessionId id;
    PatchPtr patch;
};
// A `<turn>` was inserted.
struct TurnStarted {
    SessionId session;
    core::NodeId turn;
};
// A turn ended `done`, `error` or `cancelled`.
struct TurnEnded {
    SessionId session;
    core::NodeId turn;
    core::TurnStatus status;
};
// A `<decision>` waits for an answer.
struct DecisionWaiting {
    SessionId session;
    core::NodeId decision;
};
// A `<decision>` was answered.
struct DecisionAnswered {
    SessionId session;
    core::NodeId decision;
};
// A `<compaction>` was inserted.
struct Compaction {
    SessionId session;
    core::NodeId node;
};
// The session was closed and its harness stopped.
struct Closed {
    SessionId id;
};
} // namespace session_event

using SessionEvent =
    std::variant<session_event::Activated, session_event::Loaded, session_event::Patch, session_event::TurnStarted,
                 session_event::TurnEnded, session_event::DecisionWaiting, session_event::DecisionAnswered,
                 session_event::Compaction, session_event::Closed>;

// What a session writer's feed delivers: the session's feed in the app, a peer's mirror in
// the peers area.
namespace feed {
// The writer's DOM when the feed (re)subscribed; events follow it.
struct Synced {
    std::shared_ptr<const core::Dom> dom;
};
// The events that arrived within one frame, oldest first.
struct Events {
    std::vector<bus::Event> events;
};
// The writer closed; nothing follows.
struct Ended {};
} // namespace feed

using Feed = std::variant<feed::Synced, feed::Events, feed::Ended>;

// The DOM of `writer`, then the events that arrive within each frame as one batch;
// resubscribes after falling behind and ends with `feed::Ended` once the writer closes.
class WriterFeed final : public Source<Feed> {
  public:
    WriterFeed(std::shared_ptr<bus::SessionWriter> writer, bus::Policy policy);
    ~WriterFeed() override;
    WriterFeed(WriterFeed&&) noexcept;
    WriterFeed& operator=(WriterFeed&&) noexcept;

    contracts::Awaitable<std::optional<Feed>> next() override;

  private:
    std::shared_ptr<bus::SessionWriter> writer_;
    bus::Policy policy_;
    std::optional<bus::Subscription> subscription_;
    bool ended_ = false;
};

// One open session.
class Session {
  public:
    Session(Session&&) noexcept = default;
    Session& operator=(Session&&) noexcept = default;

    [[nodiscard]] const SessionId& id() const noexcept { return id_; }
    // The session as the UI last saw it.
    [[nodiscard]] const core::Dom& dom() const noexcept { return dom_; }
    // The session's single writer: the `PatchSink` adapters write through.
    [[nodiscard]] const std::shared_ptr<bus::SessionWriter>& writer() const noexcept { return writer_; }
    // The session's journal, for forks, rewinds and reads of past states.
    [[nodiscard]] const std::shared_ptr<bus::JournalWriter>& journal() const noexcept { return journal_; }
    // The running harness; null until the session is started or resumed.
    [[nodiscard]] const HarnessPtr& harness() const noexcept { return harness_; }
    // Id of the harness the session runs on: the running one, else the `harness` attribute
    // of its `<session>`.
    [[nodiscard]] std::optional<std::string_view> harness_id() const;
    // A turn runs or waits, or a prompt was sent and its turn hasn't started.
    [[nodiscard]] bool is_busy() const noexcept { return busy_; }
    [[nodiscard]] bool has_turns() const;
    // The sidebar entry of the session, kept in step with the DOM.
    [[nodiscard]] const std::optional<contracts::SessionSummary>& summary() const noexcept { return summary_; }

    void set_busy(bool busy) noexcept { busy_ = busy; }
    // Attaches the harness that now drives the session and catches the DOM up with what it
    // wrote while starting.
    void attach(HarnessPtr harness);
    // Takes the running harness off the session; the next send resumes it on a fresh harness.
    HarnessPtr detach();

  private:
    friend class Sessions;
    Session(SessionId id, session::Live live, core::Dom dom);
    void reload(core::Dom dom);
    // Applies one feed event; `nullptr` when it carries nothing new.
    std::expected<PatchPtr, core::ApplyError> apply(bus::Event event);

    SessionId id_;
    std::shared_ptr<bus::SessionWriter> writer_;
    std::shared_ptr<bus::JournalWriter> journal_;
    core::Dom dom_;
    std::optional<contracts::SessionSummary> summary_;
    HarnessPtr harness_;
    bool busy_ = false;
};

// What `Sessions::sync` and `Sessions::apply` did.
namespace change {
struct None {};
struct Loaded {};
struct Patch {
    PatchPtr patch;
    std::optional<SessionEvent> derived;
    // The session's sidebar entry changed.
    bool summary_changed = false;
    // The patch changed a turn's status, so the busy flag may have changed.
    bool turn_changed = false;
};
} // namespace change

using Change = std::variant<change::None, change::Loaded, change::Patch>;

// The open sessions, the active one and the harness new ones start on.
class Sessions {
  public:
    using Map = std::map<SessionId, Session, std::less<>>;

    // Harness new sessions start on.
    std::optional<std::string> harness;

    [[nodiscard]] const Session* get(std::string_view id) const;
    [[nodiscard]] Session* get_mut(std::string_view id);
    // The session the chat shows.
    [[nodiscard]] const Session* active() const;
    [[nodiscard]] Session* active_mut();
    [[nodiscard]] const std::optional<SessionId>& active_id() const noexcept { return active_; }
    [[nodiscard]] bool is_active(std::string_view id) const;
    // The session whose harness is starting or resuming.
    [[nodiscard]] const std::optional<SessionId>& starting() const noexcept { return starting_; }
    // Every open session, by id.
    [[nodiscard]] const Map& iter() const noexcept { return open_; }

    void set_active(std::optional<SessionId> id) { active_ = std::move(id); }
    void set_starting(std::optional<SessionId> id) { starting_ = std::move(id); }

    // Creates a new session's journal in `library` and its writer.
    [[nodiscard]] std::expected<std::pair<SessionId, std::shared_ptr<bus::SessionWriter>>, std::string> create(
        const session::Library& library);
    // Opens a session from its journal in `library` unless it is open already.
    [[nodiscard]] std::expected<void, std::string> load(const session::Library& library, std::string_view id);
    // Closes a session's writer and releases its journal in `library`, deleting the journal
    // too when `delete_`. Returns the session if it was open and the error closing it.
    std::pair<std::optional<Session>, std::optional<std::string>> close(const session::Library& library,
                                                                        std::string_view id, bool delete_);
    // Replaces the runtime's copy with the writer's DOM when that is newer.
    Change sync(std::string_view id, std::shared_ptr<const core::Dom> dom);
    // Applies one event from a session's feed to the runtime's copy.
    Change apply(std::string_view id, bus::Event event);

  private:
    Map open_;
    std::optional<SessionId> active_;
    std::optional<SessionId> starting_;
};

// What `recents` found: summaries newest first, a note per unreadable session, and the
// empty sessions to delete.
struct Recents {
    std::vector<contracts::SessionSummary> summaries;
    std::vector<std::string> problems;
    std::vector<std::string> empty;
};

// Lists the library's sessions for Recents, newest first, with titles from their DOMs.
// Sessions without a turn hold nothing to keep and come back in `empty` for the caller to
// delete (`Runtime::delete_empty`), unless they continue another session. Reads every
// journal: run it off the UI thread.
[[nodiscard]] Recents recents(const session::Library& library);

// The `<turn>` children of the session root, oldest first.
[[nodiscard]] std::vector<const core::Node*> dom_turns(const core::Dom& dom);

// The folder a session works in, from its DOM.
[[nodiscard]] std::optional<std::filesystem::path> session_cwd(const core::Dom& dom);

// A unique session id: creation time, process and a counter.
[[nodiscard]] std::string new_session_id();

} // namespace loa::app_runtime
