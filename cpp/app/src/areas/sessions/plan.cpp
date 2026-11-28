// App plan mode for harnesses without their own: a read-only gate until the user approves a
// plan (port of `loa-app/src/areas/sessions/plan.rs`).

#include "sessions.hpp"

#include "loa/app/perform.hpp"
#include "loa/core/schema.hpp"
#include "loa/core/sink.hpp"

namespace loa::app::areas::sessions {

namespace {

namespace plan_mode = session::plan;

contracts::Awaitable<std::expected<void, std::string>> set_mode(app_runtime::HarnessPtr harness, core::SessionMode mode) {
    auto result = co_await harness->set_mode(mode);
    if (!result) co_return std::unexpected(result.error().message());
    co_return std::expected<void, std::string>{};
}

// The permit travels in a shared slot: coroutine frames and messages copy what they carry.
contracts::Awaitable<std::expected<PlanNext, std::string>> answer(app_runtime::HarnessPtr harness, core::Dom dom,
                                                                  std::shared_ptr<bus::SessionWriter> writer,
                                                                  NodeId decision, std::string by,
                                                                  plan_mode::Verdict verdict, Held permit) {
    auto outcome = co_await plan_mode::answer_plan(*harness, dom, *writer, decision, std::move(by), std::move(verdict),
                                                   permit.take());
    if (!outcome) co_return std::unexpected(outcome.error().to_string());
    if (auto* execute = std::get_if<plan_mode::outcome::Execute>(&*outcome))
        co_return PlanNext{plan_next::Execute{std::move(execute->prompt)}};
    auto& revise = std::get<plan_mode::outcome::Revise>(*outcome);
    co_return PlanNext{plan_next::Revise{std::move(revise.prompt), Held(std::move(revise.permit)), revise.step}};
}

// Reopens the plan review card `decision` after its answer failed; the decisions area shows `error`.
void plan_failed(Ctx& ctx, const SessionId& session, NodeId decision, std::string error) {
    to::decisions::Answered reopen{session.str(), decision, std::unexpected(std::move(error))};
    ctx.request(request::Forward{area_message(area_id::DECISIONS, std::move(reopen))});
}

} // namespace

void State::restore_plan(Ctx& ctx) {
    const auto* session = ctx.runtime.sessions().active();
    if (!session) return;
    const std::string id = session->id().str();
    const auto harness = session->harness_id().value_or("");
    if (detail::asks_first(ctx, harness)) gate_->enforce_through_approvals(id);
    if (detail::mode(session->dom()) != core::SessionMode::Plan || plans_.contains(id) ||
        detail::capabilities(ctx, harness).has(contracts::Feature::PlanMode))
        return;
    auto step = plan_mode::open_step(session->dom());
    auto permit = gate_->permit(id);
    if (!permit) {
        ctx.notify("Plan mode can't be enforced here: " + permit.error().to_string());
        return;
    }
    plans_.insert_or_assign(id, Plan{std::move(*permit), step});
}

Task State::pick_mode(core::SessionMode mode, Ctx& ctx) {
    ctx.composer.set_mode(mode);
    const auto* session = ctx.runtime.sessions().active();
    if (!session) return Task::none();
    const std::string id = session->id().str();
    const std::string harness_id(session->harness_id().value_or(""));
    auto writer = session->writer();
    if (detail::capabilities(ctx, harness_id).has(contracts::Feature::PlanMode)) {
        // The harness has its own plan mode.
        auto harness = session->harness();
        if (!harness) {
            const auto current = detail::mode(session->dom());
            ctx.notify("The mode changes once the session runs: send a message first");
            ctx.composer.set_mode(current);
            return Task::none();
        }
        return perform([harness, mode] { return set_mode(harness, mode); },
                       [](std::expected<void, std::string> result) -> loa::app::Message {
                           return message(msg::Done{std::move(result)});
                       });
    }
    if (mode == core::SessionMode::Plan && !plans_.contains(id)) {
        if (session->is_busy()) {
            ctx.notify("Plan mode starts between turns: wait for this one to end");
            ctx.composer.set_mode(core::SessionMode::Normal);
            return Task::none();
        }
        if (detail::asks_first(ctx, harness_id)) gate_->enforce_through_approvals(id);
        auto permit = gate_->permit(id);
        // `begin` commits through the writer, so it reads a copy: committing under `with_dom`
        // would deadlock.
        std::expected<NodeId, std::string> step = std::unexpected(std::string());
        if (permit) {
            auto copy = writer->with_dom([](const core::Dom& dom) { return dom; });
            auto begun = plan_mode::begin(copy, *writer);
            step = begun ? std::expected<NodeId, std::string>(*begun) : std::unexpected(begun.error().to_string());
        } else {
            step = std::unexpected(permit.error().to_string());
        }
        if (step) {
            plans_.insert_or_assign(id, Plan{std::move(*permit), *step});
            ctx.notify("Plan mode: tools that change files or run commands are blocked until you approve a plan");
        } else {
            ctx.notify("Can't enter plan mode on " + std::string(ctx.runtime.harnesses().label(harness_id)) + ": " +
                       step.error());
            ctx.composer.set_mode(core::SessionMode::Normal);
        }
    } else if (mode == core::SessionMode::Normal) {
        const auto* root = session->dom().root();
        if (auto it = plans_.find(id); it != plans_.end()) {
            plans_.erase(it);
            if (root)
                if (auto committed = writer->commit(core::Draft::set(root->id, std::string(core::attr::MODE),
                                                                     core::SessionMode::Normal));
                    !committed)
                    ctx.notify(core::to_string(committed.error()));
        }
    }
    return Task::none();
}

bool State::in_plan(const Ctx& ctx) const {
    const auto* session = ctx.runtime.sessions().active();
    return session && plans_.contains(session->id().as_str());
}

void State::submit_plan(Ctx& ctx, const SessionId& session, NodeId turn) {
    auto it = plans_.find(session.as_str());
    if (it == plans_.end()) return;
    auto& plan = it->second;
    const auto* open = ctx.runtime.sessions().get(session.as_str());
    if (!plan.step || !open) return;
    const auto& dom = open->dom();
    const auto* root = dom.root();
    auto text = plan_mode::plan_text(dom, turn);
    if (!root || !text) return;
    auto submitted = session::submit_plan(*open->writer(), root->id, *plan.step, *text);
    if (!submitted) {
        ctx.notify("Can't raise the plan review: " + submitted.error().to_string());
        return;
    }
    plan.step.reset();
}

Task State::answer_plan(Ctx& ctx, const SessionId& session, NodeId decision, contracts::Answer reply) {
    const auto* open = ctx.runtime.sessions().get(session.as_str());
    if (!open) return Task::none();
    auto harness = open->harness();
    if (!harness) {
        plan_failed(ctx, session, decision,
                    "The session isn't running: send a message to resume it, then answer the plan");
        return Task::none();
    }
    auto it = plans_.find(session.as_str());
    if (it == plans_.end()) {
        plan_failed(ctx, session, decision, "This session isn't in plan mode anymore");
        return Task::none();
    }
    Held permit(std::move(it->second.permit));
    plans_.erase(it);
    auto reason = reply.reason.value_or("");
    plan_mode::Verdict verdict = plan_mode::verdict::Reject{reason};
    if (!reply.values.empty()) {
        if (reply.values.front() == core::answer::APPROVE) verdict = plan_mode::verdict::Approve{};
        else if (reply.values.front() == core::answer::EDIT) verdict = plan_mode::verdict::Edit{reason};
    }
    auto by = reply.by.empty() ? ctx.services.node.key : std::move(reply.by);
    return perform(
        [harness, dom = open->dom(), writer = open->writer(), decision, by, verdict, permit] {
            return answer(harness, dom, writer, decision, by, verdict, permit);
        },
        [session, decision](std::expected<PlanNext, std::string> result) -> loa::app::Message {
            return message(msg::PlanAnswered{session, decision, std::move(result)});
        });
}

void State::plan_answered(Ctx& ctx, const SessionId& session, NodeId decision,
                          std::expected<PlanNext, std::string> result) {
    if (!result) {
        plan_failed(ctx, session, decision, "Can't answer the plan: " + result.error());
        restore_plan(ctx);
        return;
    }
    std::string prompt;
    if (auto* execute = std::get_if<plan_next::Execute>(&*result)) {
        prompt = std::move(execute->prompt);
    } else {
        auto& revise = std::get<plan_next::Revise>(*result);
        if (auto permit = revise.permit.take()) plans_.insert_or_assign(session.str(), Plan{std::move(permit), revise.step});
        prompt = std::move(revise.prompt);
    }
    if (ctx.runtime.sessions().is_active(session.as_str()))
        ctx.request(request::Prompt{contracts::Prompt{std::move(prompt), {}, ctx.services.node.key}});
}

} // namespace loa::app::areas::sessions
