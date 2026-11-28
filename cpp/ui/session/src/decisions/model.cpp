#include "loa/ui/session/decisions/model.hpp"

#include <algorithm>

#include "loa/ui/session/decisions/card.hpp"

namespace loa::ui::session::decisions {

namespace attr = loa::core::attr;
using loa::core::ElementKind;

std::optional<Decision> Decision::from_dom(const loa::core::Dom& dom, NodeId id) {
    const auto* node = dom.get(id);
    if (!node) return std::nullopt;
    return from_node(dom, *node);
}

std::optional<Decision> Decision::from_node(const loa::core::Dom& dom, const loa::core::Node& node) {
    if (node.kind != ElementKind::Decision) return std::nullopt;
    const auto kind = node.str_attr(attr::KIND).and_then([](std::string_view s) { return loa::core::parse<DecisionKind>(s); });
    if (!kind) return std::nullopt;
    const auto strings = [&](std::string_view name) {
        std::vector<std::string> out;
        if (const auto* v = node.attr(name))
            for (auto s : v->str_items()) out.emplace_back(s);
        return out;
    };
    const auto text = [&](std::string_view name) -> std::optional<std::string> {
        if (auto s = node.str_attr(name)) return std::string(*s);
        return std::nullopt;
    };
    const loa::core::Node* call = nullptr;
    if (auto id = node.int_attr(attr::CALL); id && *id >= 0)
        if (const auto* named = dom.get(NodeId{static_cast<std::uint64_t>(*id)}); named && named->kind == ElementKind::Call)
            call = named;
    const auto ancestors = dom.ancestors(node.id);
    if (!call)
        for (const auto* a : ancestors)
            if (a->kind == ElementKind::Call) {
                call = a;
                break;
            }
    Decision d;
    d.id = node.id;
    d.kind = *kind;
    d.status = node.str_attr(attr::STATUS)
                   .and_then([](std::string_view s) { return loa::core::parse<DecisionStatus>(s); })
                   .value_or(DecisionStatus::Waiting);
    d.title = text(attr::TITLE);
    d.body = node.text;
    d.options = strings(attr::OPTIONS);
    d.multi = node.bool_attr(attr::MULTI).value_or(false);
    d.answer = strings(attr::ANSWER);
    d.by = text(attr::BY);
    d.reason = text(attr::REASON);
    d.fields = text(attr::FIELDS);
    d.defaults = strings(attr::DEFAULT);
    d.timeout_ms = node.int_attr(attr::TIMEOUT_MS);
    d.created = node.int_attr(attr::CREATED);
    if (call)
        if (auto t = call->str_attr(attr::TOOL)) d.tool = std::string(*t);
    d.harness = text(attr::HARNESS);
    if (!d.harness)
        for (const auto* a : ancestors)
            if (auto h = a->str_attr(attr::HARNESS)) {
                d.harness = std::string(*h);
                break;
            }
    return d;
}

std::vector<Decision> Decision::all(const loa::core::Dom& dom) {
    std::vector<Decision> out;
    for (const auto& node : dom.iter())
        if (auto d = from_node(dom, node)) out.push_back(std::move(*d));
    return out;
}

bool Decision::allows_session() const { return std::ranges::find(options, loa::core::answer::ALLOW_SESSION) != options.end(); }

std::span<const ApprovalChoice> Decision::approval_choices() const {
    static constexpr std::array WITH_SESSION{ApprovalChoice::AllowOnce, ApprovalChoice::AllowSession, ApprovalChoice::Deny};
    static constexpr std::array WITHOUT_SESSION{ApprovalChoice::AllowOnce, ApprovalChoice::Deny};
    if (allows_session()) return WITH_SESSION;
    return WITHOUT_SESSION;
}

std::optional<Answer> Decision::default_answer(std::string_view by) const {
    auto values = kind_for(kind).default_values(*this);
    if (!values) return std::nullopt;
    return Answer{std::move(*values), std::nullopt, std::string(by)};
}

std::string answer_summary(DecisionKind kind, std::span<const std::string> values) {
    if (values.empty()) return "Skipped";
    if (values.size() == 1) {
        if (kind == DecisionKind::Approval)
            if (auto c = from_answer<ApprovalChoice>(values[0])) return std::string(past_of(*c));
        if (kind == DecisionKind::Plan)
            if (auto c = from_answer<PlanChoice>(values[0])) return std::string(past_of(*c));
    }
    std::string out;
    for (const auto& v : values) {
        if (!out.empty()) out += ", ";
        out += v;
    }
    return out;
}

} // namespace loa::ui::session::decisions
