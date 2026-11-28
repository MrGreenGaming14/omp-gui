#pragma once

// The app side of the session lifecycle: hands lifecycle commands to the runtime, runs its
// harness calls as tasks, adds every area's part to new session configs and shows the active
// session in the chat (port of `loa-app/src/app/lifecycle.rs`).

#include <filesystem>
#include <map>
#include <optional>
#include <string>
#include <string_view>
#include <variant>
#include <vector>

#include "loa/app/area.hpp"
#include "loa/app/message.hpp"
#include "loa/app/task.hpp"
#include "loa/app_runtime/lifecycle.hpp"
#include "loa/ui/session/composer/state.hpp"
#include "loa/ui/session/transcript/state.hpp"

namespace loa::app {

class Registry;

namespace lifecycle {

// What the app does after a lifecycle call, with the parts it owns.
namespace after {
// The center shows a session in `cwd` (or none): the composer chip and terminal follow.
struct Present {
    std::filesystem::path cwd;
};
// Reread the working tree diff.
struct Diff {};
struct RefreshFiles {};
// The active session's turn ended: a queued composer message goes out.
struct TurnEnded {};
// Continue a session in a new one, in `folder` when it still exists.
struct Continue {
    std::optional<std::filesystem::path> folder;
    contracts::Prompt prompt;
};
} // namespace after
using After = std::variant<after::Present, after::Diff, after::RefreshFiles, after::TurnEnded, after::Continue>;

} // namespace lifecycle

// A read-only transcript shown in its own detached window.
struct DetachedTranscript {
    core::Dom dom;
    ui::session::transcript::State chat;
    std::string title;
};

// Drives the runtime's session lifecycle and shows the active session in the chat.
class SessionController {
  public:
    SessionController();

    ui::session::transcript::State chat;
    // Transcripts of detached read-only windows (e.g. subagent session files), keyed by source path.
    std::map<std::string, DetachedTranscript, std::less<>> detached_transcripts;
    std::vector<lifecycle::After> after;
    // The unsent composer content of the sessions not shown (and of the center without one).
    std::map<std::optional<SessionId>, ui::session::composer::Draft> drafts;

    // Hands `command` to the runtime, then does what its transition asks of the app.
    Task run(Ctx& ctx, Registry& areas, app_runtime::Command command);
    // Runs the harness calls of `transition` and reacts to its events, in order.
    Task apply(Ctx& ctx, Registry& areas, app_runtime::Transition transition);
    // Sends `prompt` to the active session; every area may rewrite it first when a harness
    // runs the session. Without one the runtime resumes it, and the prompt comes back as
    // `runtime_event::SendRequested`.
    Task send(Ctx& ctx, Registry& areas, contracts::Prompt prompt);
    // Handles a lifecycle message.
    Task update(Ctx& ctx, Registry& areas, lifecycle::Message message);
    // The app's own reaction to a session event, then every area's. A streamed append only
    // reaches the transcripts and the sessions area's Agents tab.
    Task dispatch(Ctx& ctx, Registry& areas, const SessionEvent& event);
    // The sessions read from disk at startup. Empty sessions that are not open are deleted;
    // open sessions show their live summary. Rows other areas listed meanwhile (peers'
    // sessions) stay.
    void recents_loaded(Ctx& ctx, std::vector<loa::view::shell::SessionSummary> summaries,
                        std::vector<std::string> problems, std::vector<std::string> empty);
    // Shows the permission picker for harnesses with approval modes, with the current level.
    void sync_access(Ctx& ctx) const;
    // A permission level was picked: it is saved, new sessions start on it and the open one
    // restarts on it.
    Task set_access(Ctx& ctx, Registry& areas, std::string id);

    // The session the composer shows, `nullopt` for the center without one.
    [[nodiscard]] const std::optional<SessionId>& shown() const noexcept { return shown_; }

  private:
    Task react(Ctx& ctx, Registry& areas, app_runtime::RuntimeEvent event);
    void present(Ctx& ctx);
    void follow(ui::session::composer::State& composer, std::optional<SessionId> shown, bool busy, bool starting);
    void hand_over(ui::session::composer::State& composer, std::optional<SessionId> to,
                   ui::session::composer::Draft draft);
    void apply_to_chats(const Ctx& ctx, const SessionId& id, const core::Patch& patch);

    std::optional<SessionId> shown_;
    // The unsent composer content of a session replaced by the next one that starts, until it does.
    std::optional<ui::session::composer::Draft> carried_;
};

// Whether session `id` is actively running a turn: busy, and driven by a live or starting
// harness. Disk recents, peer sessions and reopened crash DOMs without a harness are not.
[[nodiscard]] bool session_running(const app_runtime::Sessions& sessions, const SessionId& id);
// The top bar status of the session: its latest turn's state unless it finished normally.
[[nodiscard]] std::optional<std::string_view> turn_status(const core::Dom& dom);

} // namespace loa::app
