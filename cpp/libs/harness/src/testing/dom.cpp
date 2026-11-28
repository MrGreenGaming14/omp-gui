#include "loa/harness/testing/dom.hpp"

#include <algorithm>
#include <array>
#include <format>

#include "loa/runtime/json.hpp"

namespace loa::harness::testing {

using namespace loa::core;

std::string_view to_string(Rule rule) noexcept {
    static constexpr std::array<std::string_view, 13> names{"Root",        "MissingStatus", "NotTerminal", "OpenTurn",
                                                            "TurnShape",   "CallInput",     "CallResult",  "Waiting",
                                                            "DecisionAnswer", "DanglingRef", "Transition", "CloseOrder",
                                                            "Replay"};
    return names[static_cast<std::size_t>(rule)];
}

namespace {

void push(std::vector<Violation>& out, Rule rule, std::optional<NodeId> node, std::string detail) {
    out.push_back(Violation{rule, node, std::move(detail)});
}

std::string tag(const Node& n) { return std::string(as_str(n.kind)); }

// Rust `{:?}` of an `Option<&str>`.
std::string debug(std::optional<std::string_view> s) { return s ? std::format("Some(\"{}\")", *s) : "None"; }

// Rust `{:?}` of a `Value`.
std::string debug(const Value& v) {
    return std::visit(
        [](const auto& x) -> std::string {
            using T = std::decay_t<decltype(x)>;
            if constexpr (std::is_same_v<T, bool>) return std::format("Bool({})", x);
            else if constexpr (std::is_same_v<T, std::int64_t>) return std::format("Int({})", x);
            else if constexpr (std::is_same_v<T, double>) return std::format("Float({})", x);
            else if constexpr (std::is_same_v<T, std::string>) return std::format("Str(\"{}\")", x);
            else if constexpr (std::is_same_v<T, Value::Bytes>) {
                std::string out = "Bytes([";
                for (std::size_t i = 0; i < x.size(); ++i) out += (i ? ", " : "") + std::to_string(x[i]);
                return out + "])";
            } else {
                std::string out = "List([";
                for (std::size_t i = 0; i < x.size(); ++i) out += (i ? ", " : "") + debug(x[i]);
                return out + "])";
            }
        },
        v.variant());
}

template <typename E>
std::optional<E> parsed(std::optional<std::string_view> s) {
    return s ? parse<E>(*s) : std::nullopt;
}

bool is_any(std::optional<TurnStatus> s, std::initializer_list<TurnStatus> set) { return s && std::ranges::find(set, *s) != set.end(); }
bool is_any(std::optional<CallStatus> s, std::initializer_list<CallStatus> set) { return s && std::ranges::find(set, *s) != set.end(); }

bool status_valid(const Node& node) {
    const auto status = node.str_attr(attr::STATUS);
    switch (node.kind) {
    case ElementKind::Turn: return parsed<TurnStatus>(status).has_value();
    case ElementKind::Message: return parsed<MessageStatus>(status).has_value();
    case ElementKind::Call: return parsed<CallStatus>(status).has_value();
    case ElementKind::Decision: return parsed<DecisionStatus>(status).has_value();
    case ElementKind::Job: return parsed<JobStatus>(status).has_value();
    default: return true;
    }
}

// A turn, message, call or decision whose status isn't terminal.
bool is_open(const Node& node) {
    const auto status = node.str_attr(attr::STATUS);
    switch (node.kind) {
    case ElementKind::Turn: return is_any(parsed<TurnStatus>(status), {TurnStatus::Running, TurnStatus::Waiting});
    case ElementKind::Message: return status == as_str(MessageStatus::Streaming);
    case ElementKind::Call: return is_any(parsed<CallStatus>(status), {CallStatus::Pending, CallStatus::Running});
    case ElementKind::Decision: return status == as_str(DecisionStatus::Waiting);
    default: return false;
    }
}

std::expected<void, std::string> resolves(const Dom& dom, const Value& value, ElementKind want) {
    const auto v = value.as_int();
    if (!v || *v < 0) return std::unexpected(debug(value) + " isn't a node id");
    const auto id = static_cast<std::uint64_t>(*v);
    const Node* n = dom.get(NodeId{id});
    if (n == nullptr) return std::unexpected(std::format("node {} doesn't exist", id));
    if (n->kind != want) return std::unexpected(std::format("node {} is <{}>, not <{}>", id, tag(*n), as_str(want)));
    return {};
}

void check_ref(const Dom& dom, const Node& node, std::string_view name, ElementKind want, std::vector<Violation>& out) {
    if (const Value* value = node.attr(name)) {
        if (auto r = resolves(dom, *value, want); !r) push(out, Rule::DanglingRef, node.id, std::format("`{}`: {}", name, r.error()));
    }
}

void check_turn(const Dom& dom, const Node& turn, bool settled, std::vector<Violation>& out) {
    const Node* first = nullptr;
    for (const Node* n : dom.children(turn.id))
        if (n->kind == ElementKind::Message || n->kind == ElementKind::Call) {
            first = n;
            break;
        }
    if (first == nullptr) {
        push(out, Rule::TurnShape, turn.id, "the turn has no user message");
    } else if (first->kind == ElementKind::Message && first->str_attr(attr::ROLE) == as_str(Role::User)) {
        if (first->str_attr(attr::STATUS) != as_str(MessageStatus::Done))
            push(out, Rule::TurnShape, first->id, "the user message isn't status=\"done\"");
    } else {
        push(out, Rule::TurnShape, first->id, std::format("the turn starts with <{}>", tag(*first)));
    }
    const auto status = parsed<TurnStatus>(turn.str_attr(attr::STATUS));
    if (!settled && is_any(status, {TurnStatus::Done, TurnStatus::Error, TurnStatus::Cancelled})) {
        auto all = dom.descendants(turn.id);
        (void)all.next();
        while (const Node* n = all.next())
            if (is_open(*n)) push(out, Rule::NotTerminal, n->id, std::format("<{}> is open under an ended turn", tag(*n)));
    }
    if (settled) {
        bool waiting = false;
        for (const Node& n : dom.descendants(turn.id))
            if (n.kind == ElementKind::Decision && n.str_attr(attr::STATUS) == as_str(DecisionStatus::Waiting)) waiting = true;
        if ((status == TurnStatus::Waiting) != waiting) push(out, Rule::Waiting, turn.id, "turn and decision disagree on waiting");
    }
}

void check_call(const Dom& dom, const Node& call, bool settled, std::vector<Violation>& out) {
    const auto status = parsed<CallStatus>(call.str_attr(attr::STATUS));
    const bool open = is_any(status, {CallStatus::Pending, CallStatus::Running});
    if (settled || !open) {
        const auto inputs = dom.children_of_kind(call.id, ElementKind::Input);
        if (inputs.size() == 1) {
            const Node& input = *inputs.front();
            const auto format = input.str_attr(attr::FORMAT);
            if (!format || *format == "json") {
                if (auto parsed_json = loa::runtime::json::parse(input.text); !parsed_json)
                    push(out, Rule::CallInput, input.id, "the arguments aren't JSON: " + parsed_json.error().message());
            }
        } else {
            push(out, Rule::CallInput, call.id, std::format("the call has {} <input>s", inputs.size()));
        }
    }
    const auto results = dom.children_of_kind(call.id, ElementKind::Result);
    const auto is_error = results.empty() ? std::nullopt : results.front()->bool_attr(attr::IS_ERROR);
    const char* bad = nullptr;
    if (results.size() > 1) bad = "the call has several <result>s";
    else if (is_any(status, {CallStatus::Done, CallStatus::Error}) && results.empty()) bad = "the call ended without a <result>";
    else if (status == CallStatus::Done && is_error == true) bad = "status=\"done\" with an is_error result";
    else if (status == CallStatus::Error && is_error == false) bad = "status=\"error\" with a result that isn't is_error";
    if (bad) push(out, Rule::CallResult, call.id, bad);
}

void check_decision(const Dom& dom, const Node& node, bool settled, std::vector<Violation>& out) {
    check_ref(dom, node, attr::CALL, ElementKind::Call, out);
    const auto call = node.int_attr(attr::CALL);
    const Node* parent = dom.get(node.parent);
    if (call && parent && parent->kind == ElementKind::Call && (*call < 0 || static_cast<std::uint64_t>(*call) != parent->id.value))
        push(out, Rule::DanglingRef, node.id, std::format("names call {} but sits under {}", *call, loa::core::to_string(parent->id)));
    const auto status = parsed<DecisionStatus>(node.str_attr(attr::STATUS));
    auto waiting_turn = [&] {
        for (const Node* a : dom.ancestors(node.id))
            if (a->kind == ElementKind::Turn) return a->str_attr(attr::STATUS) == as_str(TurnStatus::Waiting);
        return false;
    };
    if (settled && status == DecisionStatus::Waiting && !waiting_turn())
        push(out, Rule::Waiting, node.id, "a waiting decision has no waiting turn");
    const bool has_answer = node.attr(attr::ANSWER) != nullptr;
    if (status == DecisionStatus::Waiting && has_answer)
        push(out, Rule::DecisionAnswer, node.id, "a waiting decision already has an answer");
    else if (status == DecisionStatus::Answered && !has_answer)
        push(out, Rule::DecisionAnswer, node.id, "an answered decision has no answer");
}

// Whether a status machine allows `from` to become `to`.
bool transition_allowed(ElementKind kind, std::string_view from, std::string_view to) {
    if (from == to) return true;
    switch (kind) {
    case ElementKind::Turn: {
        const auto f = parse<TurnStatus>(from), t = parse<TurnStatus>(to);
        return (f == TurnStatus::Running && is_any(t, {TurnStatus::Waiting, TurnStatus::Done, TurnStatus::Error, TurnStatus::Cancelled})) ||
               (f == TurnStatus::Waiting && is_any(t, {TurnStatus::Running, TurnStatus::Error, TurnStatus::Cancelled}));
    }
    case ElementKind::Call: {
        const auto f = parse<CallStatus>(from), t = parse<CallStatus>(to);
        return (f == CallStatus::Pending && is_any(t, {CallStatus::Running, CallStatus::Done, CallStatus::Error, CallStatus::Cancelled})) ||
               (f == CallStatus::Running && is_any(t, {CallStatus::Done, CallStatus::Error, CallStatus::Cancelled}));
    }
    case ElementKind::Message: return from == as_str(MessageStatus::Streaming);
    case ElementKind::Decision: return from == as_str(DecisionStatus::Waiting);
    case ElementKind::Job: return from == as_str(JobStatus::Running);
    default: return true;
    }
}

void check_set(const Dom& dom, const Node& node, std::optional<std::string_view> to, std::uint64_t seq, std::vector<Violation>& out) {
    if (!to) {
        static constexpr std::array machine{ElementKind::Turn, ElementKind::Message, ElementKind::Call, ElementKind::Decision, ElementKind::Job};
        if (std::ranges::find(machine, node.kind) != machine.end())
            push(out, Rule::Transition, node.id, std::format("patch {} clears the <{}> status", seq, tag(node)));
        return;
    }
    if (const auto from = node.str_attr(attr::STATUS); from && !transition_allowed(node.kind, *from, *to))
        push(out, Rule::Transition, node.id, std::format("patch {} moves <{}> from {} to {}", seq, tag(node), *from, *to));
    const bool ends_turn = is_any(parse<TurnStatus>(*to), {TurnStatus::Done, TurnStatus::Error, TurnStatus::Cancelled});
    if (node.kind == ElementKind::Turn && ends_turn) {
        auto all = dom.descendants(node.id);
        (void)all.next();
        while (const Node* open = all.next())
            if (is_open(*open)) push(out, Rule::CloseOrder, open->id, std::format("patch {} ends the turn while this is open", seq));
    }
    if (node.kind == ElementKind::Decision && *to == as_str(DecisionStatus::Answered) && node.attr(attr::ANSWER) == nullptr)
        push(out, Rule::CloseOrder, node.id, std::format("patch {} answers before the answer is set", seq));
}

} // namespace

std::vector<Violation> check_dom(const Dom& dom) { return check_dom_in(dom, Phase::Settled); }

std::vector<Violation> check_dom_in(const Dom& dom, Phase phase) {
    std::vector<Violation> out;
    const Node* root = dom.root();
    if (root == nullptr || root->kind != ElementKind::Session) {
        push(out, Rule::Root, std::nullopt, "the root isn't a <session>");
        return out;
    }
    const bool settled = phase == Phase::Settled;
    const auto turns = dom.children_of_kind(root->id, ElementKind::Turn);
    for (const Node* turn : turns)
        if (is_open(*turn) && !settled && turn->id != turns.back()->id)
            push(out, Rule::OpenTurn, turn->id, "an open turn isn't the session's last turn");
    for (const Node& node : dom.iter()) {
        if (settled && is_open(node)) push(out, Rule::NotTerminal, node.id, std::format("<{}> is open in a settled DOM", tag(node)));
        if (!status_valid(node))
            push(out, Rule::MissingStatus, node.id, std::format("<{}> status {}", tag(node), debug(node.str_attr(attr::STATUS))));
        switch (node.kind) {
        case ElementKind::Turn: check_turn(dom, node, settled, out); break;
        case ElementKind::Call: check_call(dom, node, settled, out); break;
        case ElementKind::Decision: check_decision(dom, node, settled, out); break;
        case ElementKind::Job: check_ref(dom, node, attr::CALL, ElementKind::Call, out); break;
        case ElementKind::Compaction:
        case ElementKind::Digest: {
            const std::string_view name = node.kind == ElementKind::Compaction ? attr::COVERS : attr::POINTERS;
            if (const Value* list = node.attr(name)) {
                if (auto items = list->as_list()) {
                    for (const Value& value : *items)
                        if (auto r = resolves(dom, value, ElementKind::Turn); !r)
                            push(out, Rule::DanglingRef, node.id, std::format("`{}`: {}", name, r.error()));
                }
            }
            break;
        }
        default: break;
        }
    }
    return out;
}

std::vector<Violation> check_history(std::span<const Patch> patches) {
    std::vector<Violation> out;
    Dom dom;
    for (const Patch& patch : patches) {
        if (const auto* set = std::get_if<op::Set>(&patch.op); set && set->name == attr::STATUS) {
            if (const Node* node = dom.get(patch.node)) {
                std::optional<std::string_view> to;
                if (set->value) to = set->value->as_str();
                check_set(dom, *node, to, patch.seq, out);
            }
        }
        if (auto applied = dom.apply(patch); !applied) {
            push(out, Rule::Replay, patch.node, std::format("patch {} doesn't apply: {}", patch.seq, loa::core::to_string(applied.error())));
            break;
        }
    }
    return out;
}

} // namespace loa::harness::testing
