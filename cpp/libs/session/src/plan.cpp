#include "loa/session/plan.hpp"

#include <asio.hpp>

#include <algorithm>

#include "detail.hpp"
#include "loa/core/schema.hpp"

namespace loa::session::plan {

using core::Draft;
using core::ElementKind;
using core::NodeId;
using core::Value;
namespace attr = core::attr;

std::string wrap_prompt(std::string_view text) {
    std::string out(text);
    out += "\n\n";
    out += PLAN_STEP;
    return out;
}

class PlanGate::Permit final : public PlanPermit {
  public:
    Permit(std::shared_ptr<PlanGate> gate, std::string session) : gate_(std::move(gate)), session_(std::move(session)) {}
    ~Permit() override { gate_->release(session_); }
    Permit(const Permit&) = delete;
    Permit& operator=(const Permit&) = delete;

  private:
    std::shared_ptr<PlanGate> gate_;
    std::string session_;
};

std::shared_ptr<PlanGate> PlanGate::make() { return std::shared_ptr<PlanGate>(new PlanGate()); }

void PlanGate::enforce_through_approvals(std::string_view session_id) {
    std::scoped_lock lock(mutex_);
    auto it = sessions_.find(session_id);
    if (it == sessions_.end()) it = sessions_.emplace(std::string(session_id), Entry{}).first;
    it->second.enforced = true;
}

bool PlanGate::enforceable(std::string_view session_id) const {
    std::scoped_lock lock(mutex_);
    auto it = sessions_.find(session_id);
    return it != sessions_.end() && it->second.enforced;
}

bool PlanGate::active(std::string_view session_id) const {
    std::scoped_lock lock(mutex_);
    auto it = sessions_.find(session_id);
    return it != sessions_.end() && it->second.permits > 0;
}

std::optional<contracts::Answer> PlanGate::verdict(std::string_view session_id, const core::Node& decision) const {
    const bool approval = decision.kind == ElementKind::Decision &&
                          decision.str_attr(attr::KIND) == core::as_str(core::DecisionKind::Approval) &&
                          decision.str_attr(attr::STATUS) == core::as_str(core::DecisionStatus::Waiting);
    if (!approval || !active(session_id)) return std::nullopt;
    return contracts::Answer{{std::string(core::answer::DENY)}, std::string(DENY_REASON), "plan-mode"};
}

Result<PermitPtr> PlanGate::permit(std::string_view session_id) {
    {
        std::scoped_lock lock(mutex_);
        auto it = sessions_.find(session_id);
        if (it == sessions_.end() || !it->second.enforced)
            return std::unexpected(Error::port("this harness can't be limited to read-only tools"));
        ++it->second.permits;
    }
    return PermitPtr(std::make_unique<Permit>(shared_from_this(), std::string(session_id)));
}

void PlanGate::release(const std::string& session) {
    std::scoped_lock lock(mutex_);
    if (auto it = sessions_.find(session); it != sessions_.end() && it->second.permits > 0) --it->second.permits;
}

Result<NodeId> begin(const core::Dom& dom, core::PatchSink& sink) {
    const auto* root = dom.root();
    if (root == nullptr) return std::unexpected(Error::no_session());
    if (auto r = detail::commit(sink, Draft::set(root->id, std::string(attr::MODE), Value(core::SessionMode::Plan)));
        !r)
        return std::unexpected(r.error());
    auto patch = detail::commit(sink, Draft::insert(root->id, ElementKind::Todo,
                                                    detail::attrs({{attr::STATE, Value(core::TodoState::InProgress)}}),
                                                    "Write and review a plan"));
    if (!patch) return std::unexpected(patch.error());
    return patch->node;
}

bool is_app_plan(const core::Dom& dom, NodeId decision) {
    const auto* node = dom.get(decision);
    if (node == nullptr) return false;
    const auto* root = dom.root();
    return node->kind == ElementKind::Decision &&
           node->str_attr(attr::KIND) == core::as_str(core::DecisionKind::Plan) && root != nullptr &&
           std::ranges::find(root->children, decision) != root->children.end();
}

std::optional<std::string_view> plan_text(const core::Dom& dom, NodeId turn) {
    std::optional<std::string_view> last;
    for (const auto* m : dom.children_of_kind(turn, ElementKind::Message)) {
        if (m->str_attr(attr::ROLE) != core::as_str(core::Role::Assistant)) continue;
        auto kind = m->str_attr(attr::KIND);
        if (kind && *kind != core::as_str(core::MessageKind::Text)) continue;
        auto text = detail::trim(m->text);
        if (!text.empty()) last = text;
    }
    return last;
}

std::optional<NodeId> open_step(const core::Dom& dom) {
    const auto* root = dom.root();
    if (root == nullptr) return std::nullopt;
    std::optional<NodeId> last;
    for (const auto* t : dom.children_of_kind(root->id, ElementKind::Todo))
        if (t->str_attr(attr::STATE) == core::as_str(core::TodoState::InProgress)) last = t->id;
    return last;
}

Awaitable<Result<Outcome>> answer_plan(const contracts::HarnessSession& session, const core::Dom& dom,
                                       core::PatchSink& sink, NodeId decision, std::string by, Verdict verdict,
                                       PermitPtr permit) {
    if (!is_app_plan(dom, decision))
        co_return std::unexpected(Error::port(core::to_string(decision) + " is not a plan review the app raised"));
    const auto* root_node = dom.root();
    if (root_node == nullptr) co_return std::unexpected(Error::no_session());
    const NodeId root = root_node->id;
    std::string plan;
    if (const auto* d = dom.get(decision)) plan = d->text;

    std::string_view value;
    std::optional<std::string> reason;
    if (std::holds_alternative<verdict::Approve>(verdict)) {
        value = core::answer::APPROVE;
    } else if (const auto* edit = std::get_if<verdict::Edit>(&verdict)) {
        value = core::answer::EDIT;
        reason = edit->plan;
    } else {
        value = core::answer::REJECT;
        reason = std::get<verdict::Reject>(verdict).feedback;
    }
    if (auto r = detail::commit(sink, Draft::set(decision, std::string(attr::ANSWER), Value(Value::List{Value(value)})));
        !r)
        co_return std::unexpected(r.error());
    if (reason)
        if (auto r = detail::commit(sink, Draft::set(decision, std::string(attr::REASON), Value(*reason))); !r)
            co_return std::unexpected(r.error());
    if (auto r = detail::commit(sink, Draft::set(decision, std::string(attr::BY), Value(by))); !r)
        co_return std::unexpected(r.error());
    if (auto r = detail::commit(sink, Draft::set(decision, std::string(attr::STATUS),
                                                 Value(core::DecisionStatus::Answered)));
        !r)
        co_return std::unexpected(r.error());
    if (auto r = detail::commit(sink, Draft::set(decision, std::string(attr::ENDED), Value(core::now_ms()))); !r)
        co_return std::unexpected(r.error());

    if (const auto* reject = std::get_if<verdict::Reject>(&verdict)) {
        auto step = detail::commit(sink, Draft::insert(root, ElementKind::Todo,
                                                       detail::attrs({{attr::STATE, Value(core::TodoState::InProgress)}}),
                                                       "Revise the plan"));
        if (!step) co_return std::unexpected(step.error());
        auto prompt = wrap_prompt("The user rejected the plan. Feedback: " + reject->feedback + "\nRevise the plan.");
        co_return Outcome(outcome::Revise{std::move(prompt), std::move(permit), step->node});
    }
    if (const auto* edit = std::get_if<verdict::Edit>(&verdict)) plan = edit->plan;
    auto mode = co_await session.set_mode(core::SessionMode::Normal);
    if (!mode && mode.error().get<contracts::AdapterError::Unsupported>() == nullptr)
        co_return std::unexpected(Error::adapter(std::move(mode.error())));
    if (auto r = detail::commit(sink, Draft::set(root, std::string(attr::MODE), Value(core::SessionMode::Normal))); !r)
        co_return std::unexpected(r.error());
    permit.reset();
    co_return Outcome(outcome::Execute{"The user approved this plan. Carry it out now.\n\n" + plan});
}

} // namespace loa::session::plan
