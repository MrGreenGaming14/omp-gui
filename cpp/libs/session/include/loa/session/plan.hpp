#pragma once

// Plan mode on harnesses without it (port of `loa_session::plan`): a
// read-only tool gate enforced through the harness's own tool approvals, the
// plan step added to each prompt, and the plan review that ends the mode.

#include <cstddef>
#include <map>
#include <memory>
#include <mutex>
#include <optional>
#include <string>
#include <string_view>
#include <variant>

#include "loa/contracts/harness.hpp"
#include "loa/core/dom.hpp"
#include "loa/core/sink.hpp"
#include "loa/session/session.hpp"

namespace loa::session::plan {

// The plan step, put before every prompt sent while a session is in plan mode.
inline constexpr std::string_view PLAN_STEP =
    "[Plan mode] Tools that change files or run commands are blocked until the user approves a plan. Investigate "
    "with read-only tools only, then answer with a concise numbered plan for the user to review. Do not make any "
    "changes yet.";

// Deny reason given to tool approvals while plan mode is on.
inline constexpr std::string_view DENY_REASON =
    "Plan mode is read-only: write a plan for the user to review instead.";

// What a prompt becomes while the session is in plan mode: the user's text first.
[[nodiscard]] std::string wrap_prompt(std::string_view text);

// Per session read-only tool gate.
class PlanGate : public std::enable_shared_from_this<PlanGate> {
  public:
    [[nodiscard]] static std::shared_ptr<PlanGate> make();

    // Declares that `session_id`'s harness asks the app before every tool that
    // writes or executes, so denying those approvals makes it read-only.
    void enforce_through_approvals(std::string_view session_id);
    // Whether plan mode can be enforced for `session_id`.
    [[nodiscard]] bool enforceable(std::string_view session_id) const;
    // Whether `session_id` is in plan mode.
    [[nodiscard]] bool active(std::string_view session_id) const;
    // A deny for every waiting tool approval while plan mode is on, `nullopt` otherwise.
    [[nodiscard]] std::optional<contracts::Answer> verdict(std::string_view session_id,
                                                           const core::Node& decision) const;
    // A permit that keeps `session_id` in plan mode while held. Fails closed
    // for a session not registered with `enforce_through_approvals`.
    [[nodiscard]] Result<PermitPtr> permit(std::string_view session_id);

  private:
    struct Entry {
        bool enforced = false;
        std::size_t permits = 0;
    };
    class Permit;

    PlanGate() = default;
    void release(const std::string& session);

    mutable std::mutex mutex_;
    std::map<std::string, Entry, std::less<>> sessions_;
};

// Puts the session in `dom` in plan mode: sets its `mode` and adds the plan
// step, a `<todo>` in progress. Returns the step.
[[nodiscard]] Result<core::NodeId> begin(const core::Dom& dom, core::PatchSink& sink);

// Whether `decision` is a plan review the app raised (directly under `<session>`).
[[nodiscard]] bool is_app_plan(const core::Dom& dom, core::NodeId decision);

// The plan an agent wrote in `turn`: its last assistant text message.
[[nodiscard]] std::optional<std::string_view> plan_text(const core::Dom& dom, core::NodeId turn);

// The plan step of plan mode: the `<todo>` `begin` inserted, still in progress.
[[nodiscard]] std::optional<core::NodeId> open_step(const core::Dom& dom);

// How the user answered a plan review.
namespace verdict {
struct Approve {
    friend bool operator==(const Approve&, const Approve&) = default;
};
// Approve this edited plan instead.
struct Edit {
    std::string plan;
    friend bool operator==(const Edit&, const Edit&) = default;
};
// Reject with feedback; the session stays in plan mode.
struct Reject {
    std::string feedback;
    friend bool operator==(const Reject&, const Reject&) = default;
};
} // namespace verdict
using Verdict = std::variant<verdict::Approve, verdict::Edit, verdict::Reject>;

// What follows a plan review.
namespace outcome {
// Plan mode is over and the gate released. Send `prompt` to carry out the plan.
struct Execute {
    std::string prompt;
};
// Still in plan mode. Send `prompt` for a revised plan; `step` is the new plan step.
struct Revise {
    std::string prompt;
    PermitPtr permit;
    core::NodeId step;
};
} // namespace outcome
using Outcome = std::variant<outcome::Execute, outcome::Revise>;

// Records the user's answer on an app plan review and ends or continues plan
// mode accordingly.
[[nodiscard]] Awaitable<Result<Outcome>> answer_plan(const contracts::HarnessSession& session, const core::Dom& dom,
                                                     core::PatchSink& sink, core::NodeId decision, std::string by,
                                                     Verdict verdict, PermitPtr permit);

} // namespace loa::session::plan
