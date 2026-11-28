#pragma once

// App-provided session features on any harness (port of `loa-session`'s
// `lib.rs`): fork, rewind and compaction over the `SessionStore` port, plan
// submission, role selection and the transcripts that seed fresh sessions.

#include <cstdint>
#include <memory>
#include <optional>
#include <string>
#include <string_view>
#include <utility>
#include <vector>

#include "loa/contracts/async.hpp"
#include "loa/contracts/harness.hpp"
#include "loa/core/dom.hpp"
#include "loa/core/sink.hpp"
#include "loa/session/error.hpp"

namespace loa::session {

template <typename T>
using Awaitable = contracts::Awaitable<T>;

// Creates sessions: `fork` forks the journal at `seq` and continues its DOM;
// `fresh` starts an empty one.
class SessionStore {
  public:
    virtual ~SessionStore() = default;
    [[nodiscard]] virtual Awaitable<Result<std::pair<core::Dom, std::shared_ptr<core::PatchSink>>>> fork(
        std::string source_id, std::uint64_t seq, std::string new_id) const = 0;
    [[nodiscard]] virtual Awaitable<Result<std::shared_ptr<core::PatchSink>>> fresh(std::string new_id) const = 0;
};

// Held while plan mode is on; destroying it restores normal tool permissions.
class PlanPermit {
  public:
    virtual ~PlanPermit() = default;
};

using PermitPtr = std::unique_ptr<PlanPermit>;

// The visible history as text (compactions replace the turns they cover), to
// seed a fresh session.
[[nodiscard]] Result<std::string> translated_history(const core::Dom& dom);

// The visible text of one `<turn>`: messages (no thinking) and tool calls
// with their input and result.
[[nodiscard]] std::string turn_transcript(const core::Dom& dom, core::NodeId turn);

// Branches `source` into `config.session_id`: a journal fork when the harness
// forks natively, else a fresh session seeded with the translated history.
[[nodiscard]] Awaitable<Result<contracts::SessionPtr>> fork(const contracts::Adapter& adapter,
                                                             const SessionStore& store, const core::Dom& source,
                                                             contracts::SessionConfig config);

// Branches at the earlier state `target` of `current`; the old timeline is
// left untouched.
[[nodiscard]] Awaitable<Result<contracts::SessionPtr>> rewind(const contracts::Adapter& adapter,
                                                               const SessionStore& store, const core::Dom& current,
                                                               const core::Dom& target,
                                                               contracts::SessionConfig config);

// The live session being compacted and the configuration for its successor.
struct Compaction {
    const contracts::HarnessSession& running;
    const core::Dom& source;
    core::PatchSink& source_sink;
    contracts::SessionConfig next;
    // Recorded as `<compaction model>`.
    std::string smol_model;
    std::optional<std::string> instructions;
};

// What the small model is asked to write when the app compacts a session.
[[nodiscard]] std::string compaction_prompt(std::string_view history,
                                            std::optional<std::string_view> instructions);

// Compacts natively when the harness can (`nullopt`); otherwise `summarizer`
// writes a summary, a `<compaction>` records the turns it covers and a fresh
// session continues from it.
[[nodiscard]] Awaitable<Result<std::optional<contracts::SessionPtr>>> compact(
    const contracts::Adapter& adapter, const SessionStore& store, const contracts::Summarizer& summarizer,
    Compaction request);

// Submits a plan for review: the plan step is done and a
// `<decision kind="plan">` waits under the session.
[[nodiscard]] Result<core::NodeId> submit_plan(core::PatchSink& sink, core::NodeId session, core::NodeId step,
                                               std::string_view plan);

// What `select_role` switched to.
struct RoleSelected {
    std::string model;
    // Why the candidate's thinking level was not applied; the model and role were.
    std::optional<std::string> effort_error;
    friend bool operator==(const RoleSelected&, const RoleSelected&) = default;
};

// Switches to `role`, trying its `candidates` in order; returns the model picked.
[[nodiscard]] Awaitable<Result<RoleSelected>> select_role(const contracts::HarnessSession& session,
                                                           const contracts::Capabilities& capabilities,
                                                           core::PatchSink& sink, core::NodeId root, std::string role,
                                                           std::vector<std::string> candidates);

} // namespace loa::session
