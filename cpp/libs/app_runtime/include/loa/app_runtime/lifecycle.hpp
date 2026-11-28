#pragma once

// The session lifecycle without a UI: starting, resuming, sending to,
// interrupting, closing and feeding sessions (port of `loa_runtime::lifecycle`).
//
// The app hands each `Command` to `Runtime::dispatch` and gets a `Transition`
// back: effects to run, each resolving to a `Completion` the app dispatches as
// `command::Completed`, and the events to react to, in order. Feeds use
// `Runtime::begin_feed` and `Runtime::advance_feed` so consumers react to each
// patch before the next one changes the DOM.
//
// Starting, resuming and spawning a session is a handshake: the runtime builds
// the base `SessionConfig` and emits `runtime_event::PrepareSession`; the app
// adds what its areas contribute and dispatches `command::Prepared`, which
// launches the harness exactly once.

#include <array>
#include <cstddef>
#include <cstdint>
#include <expected>
#include <filesystem>
#include <functional>
#include <map>
#include <memory>
#include <optional>
#include <string>
#include <string_view>
#include <tuple>
#include <variant>
#include <vector>

#include "loa/app_runtime/session.hpp"
#include "loa/contracts/harness.hpp"

namespace loa::app_runtime {

// The permission picker's choices: omp's approval modes, as id, label and detail.
struct AccessMode {
    std::string_view id;
    std::string_view label;
    std::string_view detail;
};
inline constexpr std::array<AccessMode, 4> ACCESS{{
    {"always-ask", "Ask first", "Reads run; edits and commands ask"},
    {"always-ask-cached", "Ask and remember", "Asks before file reads and writes, remembers answers"},
    {"write", "Auto-edit", "Reads and edits run; commands ask"},
    {"yolo", "Full access", "Everything runs without asking"},
}};
inline constexpr std::string_view ACCESS_DEFAULT = "always-ask";

// Label of approval mode `id`; empty for one `ACCESS` doesn't list.
[[nodiscard]] std::string_view access_label(std::string_view id);

// Why a session couldn't be resumed.
struct ResumeError {
    enum class Kind : std::uint8_t {
        // The harness can't resume it; a new session takes the prompt.
        Unsupported,
        Failed,
    };
    Kind kind = Kind::Failed;
    std::string message;

    // `Unsupported` and `Format` adapter errors are unsupported; the rest failed.
    [[nodiscard]] static ResumeError from(const contracts::AdapterError& error);

    friend bool operator==(const ResumeError&, const ResumeError&) = default;
};

// One start, resume or spawn, from `runtime_event::PrepareSession` to its `Completion`.
using OperationId = std::uint64_t;

namespace completion {
// A new session's harness started; `prompt` is sent once it runs.
struct Started {
    OperationId operation = 0;
    SessionId id;
    std::expected<HarnessPtr, std::string> result;
    std::optional<contracts::Prompt> prompt;
};
// A session's harness resumed; `prompt` is sent once it runs.
struct Resumed {
    OperationId operation = 0;
    SessionId id;
    std::expected<HarnessPtr, ResumeError> result;
    contracts::Prompt prompt;
};
// A session an area spawned started.
struct Spawned {
    OperationId operation = 0;
    SessionId id;
    std::expected<HarnessPtr, std::string> result;
    std::optional<contracts::Prompt> prompt;
};
// A harness call on the active session returned: sending, interrupting, or stopping it to
// restart on a new approval mode.
struct Done {
    std::expected<void, std::string> result;
};
// The harness of a closed or discarded session stopped; no turn of the active session
// depends on it.
struct ShutDown {
    std::expected<void, std::string> result;
};
} // namespace completion

// What an `Effect` resolves to.
using Completion =
    std::variant<completion::Started, completion::Resumed, completion::Spawned, completion::Done, completion::ShutDown>;

// Harness work to run off the caller: call it once and `co_await` the coroutine on the
// `loa::runtime` executor; its result goes back as `command::Completed`.
using Effect = std::move_only_function<contracts::Awaitable<Completion>()>;

// Starts a session whose journal the closure creates: it gets the session's full config
// (every area's contribution, keyed by `Spawn::id`) and returns the running harness.
using SpawnStart = std::function<contracts::Awaitable<std::expected<HarnessPtr, std::string>>(contracts::SessionConfig)>;

// A session an area creates itself (fork, rewind, compaction); it becomes the active session.
struct Spawn {
    // Id of the new session; `start` must create it in the library.
    SessionId id;
    // Harness it runs on.
    std::string harness;
    SpawnStart start;
    // Sent once it runs.
    std::optional<contracts::Prompt> prompt;
};

// What the app reacts to.
namespace runtime_event {
// Add every area's part to `config` and dispatch `command::Prepared`. `resume` when the
// session continues on a fresh harness.
struct PrepareSession {
    OperationId operation = 0;
    contracts::SessionConfig config;
    std::string harness;
    bool resume = false;
};
struct Session {
    SessionEvent event;
};
struct Notice {
    std::string text;
};
// A new session's harness failed to start; the session is gone.
struct StartFailed {
    SessionId id;
};
// A resume failed for good; the prompt is dropped.
struct ResumeFailed {
    SessionId id;
};
// Continue the active session in a new one, in `folder` when it is set.
struct ContinueRequested {
    std::optional<std::filesystem::path> folder;
    contracts::Prompt prompt;
};
// A started, spawned or resumed session runs: send `prompt` to it, as any other prompt,
// with `command::Send`.
struct SendRequested {
    SessionId id;
    contracts::Prompt prompt;
};
// A started or spawned session runs and is the active one.
struct SessionStarted {
    SessionId id;
};
// The active session started a turn.
struct TurnStarted {
    SessionId id;
};
// The active session's turn ended.
struct TurnEnded {
    SessionId id;
};
// A session's sidebar entry changed (`Session::summary`).
struct SummaryChanged {
    SessionId id;
};
// A session's turn changed status: the sidebar follows, and the top bar when it is active.
struct StatusChanged {
    SessionId id;
};
// A prompt that couldn't be sent goes back to the composer, which stops waiting for its turn.
struct PromptReturned {
    contracts::Prompt prompt;
};
} // namespace runtime_event

using RuntimeEvent =
    std::variant<runtime_event::PrepareSession, runtime_event::Session, runtime_event::Notice,
                 runtime_event::StartFailed, runtime_event::ResumeFailed, runtime_event::ContinueRequested,
                 runtime_event::SendRequested, runtime_event::SessionStarted, runtime_event::TurnStarted,
                 runtime_event::TurnEnded, runtime_event::SummaryChanged, runtime_event::StatusChanged,
                 runtime_event::PromptReturned>;

// What a `Command` did: work to run and events to react to, in order.
struct Transition {
    std::vector<Effect> effects;
    std::vector<RuntimeEvent> events;
};

// What the runtime is asked to do.
namespace command {
// Start a session on the selected harness; `prompt` is sent once it runs.
struct Start {
    std::optional<contracts::Prompt> prompt;
};
// Show session `id`, loading it from its journal when needed.
struct Open {
    SessionId id;
};
// Send to the active session, resuming it first when no harness runs it.
struct Send {
    contracts::Prompt prompt;
};
// Start a session an area creates itself.
using Spawn = app_runtime::Spawn;
// Stop the active session's running turn.
struct Interrupt {};
// Close a session, deleting its journal too when `delete_` is set.
struct Close {
    SessionId id;
    bool delete_ = false;
};
// Run new sessions, and the open one, on this approval mode.
struct SetAccess {
    std::string id;
};
// The full config of a prepared operation: launch it.
struct Prepared {
    OperationId operation = 0;
    contracts::SessionConfig config;
};
// An `Effect` finished.
struct Completed {
    Completion completion;
};
} // namespace command

using Command = std::variant<command::Start, command::Open, command::Send, command::Spawn, command::Interrupt,
                             command::Close, command::SetAccess, command::Prepared, command::Completed>;

// Launches a prepared operation with its full config.
using Launch = std::move_only_function<Effect(OperationId, contracts::SessionConfig)>;

// An operation between `runtime_event::PrepareSession` and its `Completion`.
struct Pending {
    SessionId session;
    // Empty once launched.
    Launch launch;
};

// The operations in flight and the id of the next one.
struct Operations {
    std::map<OperationId, Pending> pending;
    OperationId next = 0;
};

// A frame of feed events, applied one at a time so reactions see the post-patch DOM. Drain
// with `Runtime::advance_feed` before processing another feed.
class FeedBatch {
  private:
    friend class Runtime;
    FeedBatch() = default;

    SessionId id_;
    std::shared_ptr<const core::Dom> synced_;
    std::vector<bus::Event> events_;
    std::size_t index_ = 0;
    bool active_ = false;
    bool was_busy_ = false;
    bool finished_ = false;
};

} // namespace loa::app_runtime
