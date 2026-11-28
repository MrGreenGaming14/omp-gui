#include "loa/harness/omp2/mapper.hpp"

#include <algorithm>
#include <limits>
#include <string_view>

#include "loa/harness/kit/rpc.hpp"

namespace loa::harness::omp2 {
namespace {
using namespace core;
using namespace contracts;
using Json = wire::Json;

AdapterError to_adapter(AdapterError e) { return e; }
AdapterError to_adapter(CommitError e) { return AdapterError::Commit{std::move(e)}; }
AdapterError bad(std::string_view what) { return AdapterError::Protocol{"malformed omp2 event: " + std::string(what)}; }
AdapterError unknown(wire::Handle h) {
    return AdapterError::Protocol{"omp2 named node " + std::to_string(h.value) + ", which it never inserted"};
}
std::string x(std::string_view prop) { return "x-" + std::string(prop); }
Value handle_value(wire::Handle h) {
    return Value(static_cast<std::int64_t>(std::min<std::uint64_t>(h.value, std::numeric_limits<std::int64_t>::max())));
}
std::optional<Value> to_value(const Json& v) {
    if (v.is_null()) return std::nullopt;
    if (auto p = v.as_bool()) return Value(*p);
    if (v.is_number()) {
        if (auto n = v.as_i64()) return Value(*n);
        return Value(v.as_f64().value_or(std::numeric_limits<double>::quiet_NaN()));
    }
    if (auto p = v.as_string()) return Value(*p);
    return Value(runtime::json::dump(v));
}
std::string_view stop_status(std::string_view reason) {
    if (reason == "aborted" || reason == "cancelled") return "cancelled";
    return reason == "error" ? "error" : "done";
}
} // namespace

#define OMP_TRY(expr) do { auto omp_result = (expr); if (!omp_result) return std::unexpected(to_adapter(std::move(omp_result).error())); } while (false)

Omp2Mapper::Node::Node(wire::NodeSpec spec, std::optional<Handle> p)
    : tag(std::move(spec.tag)), parent(p), kids(std::move(spec.kids)), content(std::move(spec.content)) {
    for (auto& pair : spec.props) if (pair.first != "order") props.push_back(std::move(pair));
}
const Json* Omp2Mapper::Node::prop(std::string_view name) const {
    for (const auto& [key, value] : props) if (key == name) return &value;
    return nullptr;
}

ElementKind Omp2Mapper::kind(Element e) {
    switch (e) {
    case Element::Turn: return ElementKind::Turn;
    case Element::User: case Element::Block: return ElementKind::Message;
    case Element::Call: return ElementKind::Call;
    case Element::Input: return ElementKind::Input;
    case Element::Result: return ElementKind::Result;
    case Element::Usage: return ElementKind::Usage;
    case Element::Todo: return ElementKind::Todo;
    case Element::Job: case Element::Subagent: return ElementKind::Job;
    case Element::Compaction: return ElementKind::Compaction;
    }
    return ElementKind::Ext;
}
Omp2Mapper::ResolvedRule Omp2Mapper::rule(const wire::Tag& tag, const wire::Tag& parent_tag,
                                          std::optional<ElementKind> parent) {
    if (!parent) return {Rule::Ext, {}};
    if (tag.custom) {
        if (*parent == ElementKind::Turn && parent_tag.is("assistant") && tag.name == "assistant-content")
            return {Rule::Element, Element::Block};
        if (*parent == ElementKind::Turn && parent_tag.is("turn")) return {Rule::Element, Element::Call};
        return {Rule::Ext, {}};
    }
    if (tag.name == "session") return {Rule::Root, {}};
    if (tag.name == "meta" || tag.name == "body" || tag.name == "todo" || tag.name == "jobs")
        return {Rule::Flatten, {}};
    if (tag.name == "assistant") return {Rule::Assistant, {}};
    constexpr std::pair<std::string_view, Element> elements[] = {
        {"turn", Element::Turn}, {"user", Element::User}, {"item", Element::Todo}, {"job", Element::Job},
        {"subagent", Element::Subagent}, {"compaction", Element::Compaction}, {"input", Element::Input},
        {"result", Element::Result}, {"usage", Element::Usage}};
    for (auto [name, e] : elements) if (tag.name == name) return {Rule::Element, e};
    return {Rule::Ext, {}};
}
std::optional<std::string_view> Omp2Mapper::dest(Element e, std::string_view prop) {
    using namespace attr;
    if (prop == "id") {
        if (e == Element::Turn || e == Element::User || e == Element::Job || e == Element::Subagent) return ID;
        if (e == Element::Call) return CALL_ID;
    }
    if (prop == "text" && (e == Element::User || e == Element::Block || e == Element::Input || e == Element::Result)) return TEXT;
    if (e == Element::User && prop == "author") return BY;
    if (e == Element::Block && prop == "kind") return KIND;
    if ((e == Element::Call || e == Element::Job || e == Element::Subagent) && prop == "status") return STATUS;
    if (e == Element::Call && prop == "i") return TITLE;
    if (e == Element::Result && prop == "truncated") return TRUNCATED;
    if (e == Element::Usage) {
        if (prop == "tokens-in") return INPUT_TOKENS;
        if (prop == "tokens-out") return OUTPUT_TOKENS;
        if (prop == "cache-read") return CACHE_READ_TOKENS;
        if (prop == "cache-write") return CACHE_WRITE_TOKENS;
        if (prop == "cost-nano-usd") return COST;
        if (prop == "duration-ms") return DURATION_MS;
        if (prop == "model") return MODEL;
    }
    if (e == Element::Todo) {
        if (prop == "label") return TEXT;
        if (prop == "status") return STATE;
        if (prop == "phase") return PHASE;
    }
    if (e == Element::Compaction) {
        if (prop == "summary") return TEXT;
        if (prop == "tokens-before") return TOKENS_BEFORE;
    }
    if (e == Element::Job || e == Element::Subagent) {
        if (prop == "label") return TITLE;
        if (prop == "agent") return AGENT;
        if (prop == "exit") return EXIT_CODE;
    }
    return std::nullopt;
}
std::optional<std::string_view> Omp2Mapper::enum_value(Element e, std::string_view name, std::string_view raw) {
    using namespace attr;
    if (e == Element::Call && name == STATUS) {
        if (raw == "arguments") return "pending";
        if (raw == "running") return "running";
        if (raw == "ok") return "done";
        if (raw == "error") return "error";
        if (raw == "cancelled" || raw == "aborted") return "cancelled";
    }
    if (e == Element::Todo && name == STATE) {
        if (raw == "pending" || raw == "blocked") return "pending";
        if (raw == "in_progress") return "in_progress";
        if (raw == "completed") return "done";
        if (raw == "abandoned") return "cancelled";
    }
    if ((e == Element::Job || e == Element::Subagent) && name == STATUS) {
        if (raw == "running") return "running";
        if (raw == "completed" || raw == "done" || raw == "ok") return "done";
        if (raw == "failed" || raw == "error") return "error";
        if (raw == "cancelled" || raw == "killed" || raw == "aborted") return "cancelled";
    }
    if (e == Element::Block && name == KIND && (raw == "text" || raw == "thinking")) return raw;
    return std::nullopt;
}
Attrs Omp2Mapper::fixed_attrs(Element e, const wire::Tag& tag) {
    Attrs attrs;
    using namespace attr;
    switch (e) {
    case Element::Turn: attrs.emplace(STATUS, "running"); break;
    case Element::User: attrs.emplace(ROLE, "user"); attrs.emplace(STATUS, "done"); break;
    case Element::Block: attrs.emplace(ROLE, "assistant"); attrs.emplace(STATUS, "streaming"); break;
    case Element::Call: attrs.emplace(TOOL, tag.name); attrs.emplace(STATUS, "running"); break;
    case Element::Input: attrs.emplace(FORMAT, "json"); break;
    case Element::Job: attrs.emplace(KIND, "shell"); break;
    case Element::Subagent: attrs.emplace(KIND, "subagent"); break;
    default: break;
    }
    return attrs;
}
std::pair<std::string, std::optional<Value>> Omp2Mapper::element_write(Element e, std::string_view prop, const Json& v) {
    auto mapped = dest(e, prop);
    if (!mapped) return {x(prop), to_value(v)};
    if (v.is_null()) return {std::string(*mapped), std::nullopt};
    if (*mapped == TEXT) {
        if (auto text = v.as_string()) return {std::string(TEXT), Value(*text)};
    } else if (*mapped == attr::COST) {
        if (auto nano = v.as_f64()) return {std::string(attr::COST), Value(*nano / 1e9)};
    } else if (const auto* s = spec(kind(e)).attr(*mapped)) {
        if (!s->values.empty()) {
            if (auto raw = v.as_string()) if (auto value = enum_value(e, *mapped, *raw)) return {std::string(*mapped), Value(*value)};
        } else if ((s->ty == ValueType::Bool && v.is_bool()) ||
                   (s->ty == ValueType::Str && (v.is_string() || v.is_array() || v.is_object()))) {
            return {std::string(*mapped), to_value(v)};
        } else if (s->ty == ValueType::Int) {
            if (auto n = v.as_i64()) return {std::string(*mapped), Value(*n)};
        } else if (s->ty == ValueType::Float) {
            if (auto n = v.as_f64()) return {std::string(*mapped), Value(*n)};
        }
    }
    return {x(prop), to_value(v)};
}

Omp2Mapper::Omp2Mapper(Id root) : ledger_(root) {}
Omp2Mapper::Omp2Mapper(kit::TurnLedger<std::string> ledger) : ledger_(std::move(ledger)) {}
Omp2Mapper Omp2Mapper::from_dom(const Dom& dom) {
    Omp2Mapper mapper(kit::TurnLedger<std::string>::from_dom(dom));
    for (const auto& node : dom.iter()) {
        if (auto handle = node.int_attr(HANDLE_ATTR); handle && *handle >= 0)
            mapper.bind(Handle{static_cast<std::uint64_t>(*handle)}, node.id, node.parent);
        if (node.kind == ElementKind::Call) if (auto id = node.str_attr(attr::CALL_ID)) mapper.calls_.emplace(*id, node.id);
    }
    if (const auto* root = dom.root()) {
        auto turns = dom.children_of_kind(root->id, ElementKind::Turn);
        if (!turns.empty()) mapper.turn_ = turns.back()->id;
    }
    return mapper;
}
contracts::Result<Omp2Mapper::Id> Omp2Mapper::begin(const DomWriter& w, const contracts::Prompt& prompt) {
    Attrs attrs{{std::string(attr::STATUS), "running"}, {std::string(attr::STARTED), w.now()}};
    auto turn = w.commit(Draft{Draft::Insert{ledger_.session, turn_index(), ElementKind::Turn, std::move(attrs), {}}});
    if (!turn) return std::unexpected(to_adapter(std::move(turn).error()));
    Attrs user{{std::string(attr::ROLE), "user"}, {std::string(attr::STATUS), "done"}, {std::string(attr::BY), prompt.by}};
    auto message = w.insert(*turn, ElementKind::Message, std::move(user), prompt.text);
    if (!message) return std::unexpected(to_adapter(std::move(message).error()));
    pending_ = std::pair{*turn, std::optional<Id>{*message}};
    turn_ = *turn;
    ledger_.adopt(*turn);
    return *turn;
}
Omp2Mapper::Result Omp2Mapper::finish(const DomWriter& w, TurnStatus status) {
    pending_.reset();
    if (!running_turn()) return {};
    const auto* message = status == TurnStatus::Done ? "done" : status == TurnStatus::Error ? "error" : "cancelled";
    const auto* call = status == TurnStatus::Error ? "error" : "cancelled";
    auto open = std::move(open_);
    open_.clear();
    for (auto [id, is_call] : open) {
        if (is_call) OMP_TRY(w.close(id, call));
        else OMP_TRY(w.set(id, attr::STATUS, message));
    }
    OMP_TRY(ledger_.end_turn(w, status));
    return {};
}
std::optional<Omp2Mapper::Id> Omp2Mapper::running_turn() {
    if (!ledger_.turn() && turn_) ledger_.adopt(*turn_);
    return turn_;
}
void Omp2Mapper::turn_seen(Id id) {
    turn_ = id;
    if (ledger_.turn() && ledger_.turn()->node != id) ledger_.adopt(id);
}
Omp2Mapper::Result Omp2Mapper::frame(const DomWriter& w, wire::Frame frame) {
    if (auto* end = std::get_if<wire::frame::AgentEnd>(&frame)) {
        auto decisions = std::move(decisions_);
        decisions_.clear();
        for (const auto& [decision, ignored] : decisions) {
            OMP_TRY(w.close(decision, DecisionStatus::Cancelled));
            ledger_.answered(decision);
        }
        if (end->error && turn_) OMP_TRY(w.ext(*turn_, "omp2", "error", *end->error));
        return finish(w, end->error ? TurnStatus::Error : end->cancelled ? TurnStatus::Cancelled : TurnStatus::Done);
    }
    if (auto* req = std::get_if<wire::frame::ToolApprovalRequest>(&frame)) {
        std::optional<Id> call;
        if (req->tool_call_id) if (auto it = calls_.find(*req->tool_call_id); it != calls_.end()) call = it->second;
        bool allow_session = !req->scopes || std::ranges::find(*req->scopes, "session") != req->scopes->end();
        Value::List options{Value(answer::ALLOW_ONCE)};
        if (allow_session) options.emplace_back(answer::ALLOW_SESSION);
        options.emplace_back(answer::DENY);
        Attrs attrs{{std::string(attr::KIND), DecisionKind::Approval},
                    {std::string(attr::OPTIONS), Value(std::move(options))},
                    {std::string(attr::REQUEST_ID), req->prompt_id}};
        if (call) attrs.emplace(attr::CALL, *call);
        if (req->title) attrs.emplace(attr::TITLE, *req->title);
        if (req->timeout_ms) attrs.emplace(attr::TIMEOUT_MS, *req->timeout_ms);
        return decide(w, call, std::move(attrs), req->body.value_or(""), Approval{req->prompt_id, allow_session});
    }
    if (auto* req = std::get_if<wire::frame::ExtensionUiRequest>(&frame); req && req->method == "select") {
        Attrs attrs{{std::string(attr::KIND), DecisionKind::Question},
                    {std::string(attr::REQUEST_ID), kit::id_key(req->id)},
                    {std::string(attr::OPTIONS), Value(req->options)},
                    {std::string(attr::MULTI), req->multi}};
        return decide(w, {}, std::move(attrs), req->title.value_or(""), Select{req->id, req->options});
    }
    return {};
}
Omp2Mapper::Result Omp2Mapper::decide(const DomWriter& w, std::optional<Id> call, Attrs attrs,
                                       std::string text, Pending pending) {
    auto turn = running_turn();
    attrs.emplace(attr::STATUS, DecisionStatus::Waiting);
    attrs.emplace(attr::CREATED, w.now());
    auto decision = w.insert(call.value_or(turn.value_or(ledger_.session)), ElementKind::Decision, std::move(attrs), std::move(text));
    if (!decision) return std::unexpected(to_adapter(std::move(decision).error()));
    decisions_.emplace(*decision, std::move(pending));
    if (turn) {
        OMP_TRY(w.set(*turn, attr::STATUS, TurnStatus::Waiting));
        ledger_.wait(*decision);
    }
    return {};
}

Omp2Mapper::Result Omp2Mapper::dom(const DomWriter& w, wire::DomEvent event) {
    if (tree_.empty() && !std::holds_alternative<wire::dom_event::Snapshot>(event)) {
        if (!ids_.empty()) return std::unexpected(bad("a resumed session must start with a snapshot"));
        wire::Snapshot snapshot;
        snapshot.high_water = 4;
        for (std::uint64_t h = 1; h <= 4; ++h) {
            wire::NodeSpec spec;
            spec.tag = wire::Tag::from_wire(h == 1 ? "session" : h == 2 ? "meta" : h == 3 ? "body" : "queues");
            if (h == 1) spec.kids = {Handle{2}, Handle{3}, Handle{4}};
            snapshot.nodes.push_back({Handle{h}, h == 1 ? std::nullopt : std::optional<Handle>{wire::ROOT}, std::move(spec)});
        }
        OMP_TRY(reset(w, std::move(snapshot)));
    }
    if (auto* patch = std::get_if<wire::dom_event::Patch>(&event)) {
        for (auto& operation : patch->data.ops) OMP_TRY(op(w, std::move(operation)));
    } else if (auto* s = std::get_if<wire::dom_event::Stream>(&event)) {
        std::optional<std::pair<Handle, std::string>> bound;
        if (s->node && s->prop) bound.emplace(*s->node, *s->prop);
        OMP_TRY(stream(w, s->sid, s->op, std::move(bound), std::move(s->text)));
    } else {
        OMP_TRY(reset(w, std::move(std::get<wire::dom_event::Snapshot>(event).data)));
    }
    return {};
}
contracts::Result<const Omp2Mapper::Node*> Omp2Mapper::node(Handle h) const {
    if (auto it = tree_.find(h); it != tree_.end()) return &it->second;
    return std::unexpected(unknown(h));
}
const std::vector<Omp2Mapper::Handle>& Omp2Mapper::kids(Handle h) const {
    static const std::vector<Handle> empty;
    auto it = tree_.find(h);
    return it == tree_.end() ? empty : it->second.kids;
}
std::optional<Omp2Mapper::Target> Omp2Mapper::target(Handle h) const {
    auto it = tree_.find(h);
    return it == tree_.end() ? std::nullopt : std::optional<Target>{it->second.target};
}
bool Omp2Mapper::flat(Handle h) const {
    auto t = target(h);
    return t && (t->kind == TargetKind::Flat || t->kind == TargetKind::Assistant);
}
void Omp2Mapper::set_target(Handle h, Target t) {
    if (auto it = tree_.find(h); it != tree_.end()) it->second.target = t;
}
void Omp2Mapper::bind(Handle h, Id id, Id parent) {
    ids_.insert_or_assign(h, id);
    back_.insert_or_assign(id, std::pair{h, parent});
}
contracts::Result<std::pair<Omp2Mapper::Id, std::optional<ElementKind>>> Omp2Mapper::app_parent_of(Handle at) const {
    while (true) {
        auto n = node(at);
        if (!n) return std::unexpected(n.error());
        switch ((*n)->target.kind) {
        case TargetKind::Root: return std::pair{ledger_.session, std::optional<ElementKind>{ElementKind::Session}};
        case TargetKind::Node:
            return std::pair{(*n)->target.id, (*n)->target.element.transform([](Element e) { return kind(e); })};
        case TargetKind::Flat: case TargetKind::Assistant:
            if (!(*n)->parent) return std::unexpected(bad("a container without a parent"));
            at = *(*n)->parent;
            break;
        case TargetKind::Unset: return std::unexpected(unknown(at));
        }
    }
}
std::optional<std::uint32_t> Omp2Mapper::app_index(Handle h) const {
    std::optional<Handle> origin;
    if (auto it = tree_.find(h); it != tree_.end()) origin = it->second.parent;
    while (origin && flat(*origin)) origin = tree_.at(*origin).parent;
    auto& siblings = kids(origin.value_or(wire::ROOT));
    std::vector<Handle> stack(siblings.rbegin(), siblings.rend());
    std::uint32_t count = 0;
    bool seen = false;
    while (!stack.empty()) {
        auto at = stack.back();
        stack.pop_back();
        auto t = target(at);
        if (at == h) seen = true;
        else if (t && t->kind == TargetKind::Node) {
            if (seen) return count;
            ++count;
        } else if (flat(at)) {
            auto& children = kids(at);
            stack.insert(stack.end(), children.rbegin(), children.rend());
        }
    }
    return std::nullopt;
}
void Omp2Mapper::app_nodes(Handle h, std::vector<Id>& out) const {
    auto t = target(h);
    if (!t) return;
    if (t->kind == TargetKind::Node) out.push_back(t->id);
    else if (t->kind == TargetKind::Flat || t->kind == TargetKind::Assistant || t->kind == TargetKind::Root)
        for (auto child : kids(h)) app_nodes(child, out);
}
std::optional<std::uint32_t> Omp2Mapper::turn_index() const {
    auto& root = kids(wire::ROOT);
    auto body = std::ranges::find(root, wire::BODY);
    if (body == root.end()) return {};
    auto count = [this](auto first, auto last) {
        std::size_t n = 0;
        for (; first != last; ++first) {
            std::vector<Id> out;
            app_nodes(*first, out);
            n += out.size();
        }
        return n;
    };
    auto before = count(root.begin(), body + 1);
    if (!count(body + 1, root.end()) || before > std::numeric_limits<std::uint32_t>::max()) return {};
    return static_cast<std::uint32_t>(before);
}
bool Omp2Mapper::under(Id id, const std::unordered_set<Id>& set) const {
    while (true) {
        if (set.contains(id)) return true;
        auto it = back_.find(id);
        if (it == back_.end()) return false;
        id = it->second.second;
    }
}
void Omp2Mapper::forget(const std::unordered_set<Id>& gone) {
    for (auto it = open_.begin(); it != open_.end(); ) {
        if (gone.contains(it->first)) it = open_.erase(it); else ++it;
    }
    for (auto it = calls_.begin(); it != calls_.end(); ) {
        if (gone.contains(it->second)) it = calls_.erase(it); else ++it;
    }
    for (auto it = streams_.begin(); it != streams_.end(); ) {
        if (auto* s = std::get_if<TextStream>(&it->second); s && gone.contains(s->id)) it = streams_.erase(it);
        else ++it;
    }
    if (turn_ && gone.contains(*turn_)) turn_.reset();
}
Omp2Mapper::Result Omp2Mapper::detach(const DomWriter& w, Handle h) {
    std::vector<Id> doomed;
    app_nodes(h, doomed);
    for (auto id : doomed) OMP_TRY(w.remove(id));
    std::vector<Handle> stack{h};
    std::unordered_set<Id> gone;
    while (!stack.empty()) {
        auto at = stack.back();
        stack.pop_back();
        if (auto it = ids_.find(at); it != ids_.end()) {
            gone.insert(it->second);
            back_.erase(it->second);
            ids_.erase(it);
        }
        if (auto it = tree_.find(at); it != tree_.end()) {
            if (it->second.target.kind == TargetKind::Node) gone.insert(it->second.target.id);
            it->second.target = {};
            stack.insert(stack.end(), it->second.kids.begin(), it->second.kids.end());
        }
    }
    forget(gone);
    auto n = node(h);
    if (!n) return std::unexpected(n.error());
    if ((*n)->parent) if (auto p = tree_.find(*(*n)->parent); p != tree_.end())
        std::erase(p->second.kids, h);
    return {};
}
Omp2Mapper::Result Omp2Mapper::link(Handle h, Handle parent, std::optional<Handle> after) {
    auto it = tree_.find(parent);
    if (it == tree_.end()) return std::unexpected(unknown(parent));
    auto& children = it->second.kids;
    auto anchor = after ? std::ranges::find(children, *after) : children.end();
    children.insert(anchor == children.end() ? children.begin() : anchor + 1, h);
    return {};
}
Omp2Mapper::Result Omp2Mapper::op(const DomWriter& w, wire::Op operation) {
    if (auto* insert = std::get_if<wire::op::Ins>(&operation)) {
        OMP_TRY(node(insert->parent));
        Handle h{++high_water_};
        tree_.emplace(h, Node(std::move(insert->node), insert->parent));
        OMP_TRY(link(h, insert->parent, insert->after));
        return materialize(w, h, true);
    }
    if (auto* rm = std::get_if<wire::op::Rm>(&operation)) {
        OMP_TRY(node(rm->node));
        OMP_TRY(detach(w, rm->node));
        std::vector<Handle> stack{rm->node};
        while (!stack.empty()) {
            auto at = stack.back();
            stack.pop_back();
            auto it = tree_.find(at);
            if (it == tree_.end()) continue;
            stack.insert(stack.end(), it->second.kids.begin(), it->second.kids.end());
            tree_.erase(it);
        }
        return {};
    }
    if (auto* set = std::get_if<wire::op::Set>(&operation))
        return set_prop(w, set->node, std::move(set->prop), std::move(set->value));
    auto mv = std::get<wire::op::Mv>(operation);
    OMP_TRY(node(mv.node));
    OMP_TRY(node(mv.parent));
    OMP_TRY(detach(w, mv.node));
    tree_.at(mv.node).parent = mv.parent;
    OMP_TRY(link(mv.node, mv.parent, mv.after));
    return materialize(w, mv.node, false);
}
Omp2Mapper::Result Omp2Mapper::set_prop(const DomWriter& w, Handle h, std::string prop, Json value) {
    auto it = tree_.find(h);
    if (it == tree_.end()) return std::unexpected(unknown(h));
    if (prop == "order") return {};
    auto& props = it->second.props;
    auto existing = std::ranges::find_if(props, [&](const auto& p) { return p.first == prop; });
    if (existing != props.end()) existing->second = value;
    else props.emplace_back(prop, value);
    return on_set(w, h, prop, value);
}
contracts::Result<Omp2Mapper::ResolvedRule> Omp2Mapper::rule_of(Handle h) const {
    auto n = node(h);
    if (!n) return std::unexpected(n.error());
    if (!(*n)->parent) {
        if (h == wire::ROOT) return ResolvedRule{Rule::Root, {}};
        return std::unexpected(bad("a node without a parent"));
    }
    auto app = app_parent_of(*(*n)->parent);
    if (!app) return std::unexpected(app.error());
    auto p = node(*(*n)->parent);
    if (!p) return std::unexpected(p.error());
    auto r = rule((*n)->tag, (*p)->tag, app->second);
    if (r.rule == Rule::Root) return ResolvedRule{Rule::Ext, {}};
    if (r.rule == Rule::Element && r.element) {
        const auto parents = spec(kind(*r.element)).parents;
        if (!app->second || std::ranges::find(parents, *app->second) == parents.end()) return ResolvedRule{Rule::Ext, {}};
    }
    return r;
}

Omp2Mapper::Result Omp2Mapper::materialize(const DomWriter& w, Handle h, bool live) {
    auto resolved = rule_of(h);
    if (!resolved) return std::unexpected(resolved.error());
    switch (resolved->rule) {
    case Rule::Root: case Rule::Flatten: {
        set_target(h, {resolved->rule == Rule::Root ? TargetKind::Root : TargetKind::Flat, {}, {}});
        auto props = tree_.at(h).props;
        for (const auto& [prop, value] : props) OMP_TRY(on_set(w, h, prop, value));
        break;
    }
    case Rule::Assistant: set_target(h, {TargetKind::Assistant, {}, {}}); break;
    case Rule::Element: OMP_TRY(insert_element(w, h, *resolved->element, live)); break;
    case Rule::Ext: OMP_TRY(insert_ext(w, h)); break;
    }
    auto children = tree_.at(h).kids;
    for (auto child : children) OMP_TRY(reconcile(w, child));
    return {};
}
Omp2Mapper::Result Omp2Mapper::insert_element(const DomWriter& w, Handle h, Element e, bool live) {
    const auto& node = tree_.at(h);
    if (!node.parent) return std::unexpected(bad("an element without a parent"));
    auto app = app_parent_of(*node.parent);
    if (!app) return std::unexpected(app.error());
    auto bound = bind_pending(w, h, e, app->first);
    if (!bound) return std::unexpected(bound.error());
    if (*bound) {
        set_target(h, {TargetKind::Node, **bound, e});
        return {};
    }
    Attrs attrs = fixed_attrs(e, node.tag);
    if (e == Element::Turn || e == Element::Call) attrs.insert_or_assign(std::string(attr::STARTED), w.now());
    if (e == Element::Block) {
        const auto& assistant = tree_.at(*node.parent);
        for (auto [prop, name] : {std::pair<std::string_view, std::string_view>{"model", attr::MODEL},
                                  {"provider", "x-provider"}, {"route", "x-route"}}) {
            if (auto v = assistant.prop(prop)) if (auto converted = to_value(*v)) attrs.insert_or_assign(std::string(name), *converted);
        }
        if (!live && !seed_.contains({h, "text"})) {
            auto reason = assistant.prop("stop-reason");
            attrs.insert_or_assign(std::string(attr::STATUS),
                                   reason && reason->as_string() ? stop_status(*reason->as_string()) : "done");
        }
    }
    std::string text = node.content.value_or("");
    for (const auto& [prop, value] : node.props) {
        auto [name, mapped] = element_write(e, prop, value);
        if (name == TEXT) {
            if (mapped && mapped->as_str()) text = *mapped->as_str();
        } else if (mapped) attrs.insert_or_assign(std::move(name), *mapped);
        if (dest(e, prop) == TEXT) if (auto seeded = seed_.find({h, prop}); seeded != seed_.end()) text = seeded->second;
    }
    attrs.insert_or_assign(std::string(HANDLE_ATTR), handle_value(h));
    if (!validate_insert(app->second, kind(e), attrs)) return insert_ext(w, h);
    std::optional<std::string> status;
    if (auto it = attrs.find(attr::STATUS); it != attrs.end())
        if (auto s = it->second.as_str()) status = *s;
    std::optional<std::string> call_id;
    if (auto it = attrs.find(attr::CALL_ID); it != attrs.end())
        if (auto s = it->second.as_str()) call_id = *s;
    auto id = w.commit(Draft{Draft::Insert{app->first, app_index(h), kind(e), std::move(attrs), std::move(text)}});
    if (!id) return std::unexpected(to_adapter(std::move(id).error()));
    bind(h, *id, app->first);
    set_target(h, {TargetKind::Node, *id, e});
    if (e == Element::Turn) turn_seen(*id);
    else if ((e == Element::Block && status == "streaming") ||
             (e == Element::Call && (status == "pending" || status == "running")))
        open_.insert_or_assign(*id, e == Element::Call);
    if (e == Element::Call && call_id) calls_.insert_or_assign(std::move(*call_id), *id);
    return {};
}
contracts::Result<std::optional<Omp2Mapper::Id>> Omp2Mapper::bind_pending(const DomWriter& w, Handle h,
                                                                            Element e, Id parent) {
    if (!pending_) return std::optional<Id>{};
    Id id;
    if (e == Element::Turn && parent == ledger_.session) id = pending_->first;
    else if (e == Element::User && pending_->second && parent == pending_->first) {
        id = *pending_->second;
        pending_->second.reset();
    } else return std::optional<Id>{};
    if (back_.contains(id)) return std::optional<Id>{};
    bind(h, id, parent);
    OMP_TRY(w.set(id, HANDLE_ATTR, handle_value(h)));
    auto props = tree_.at(h).props;
    for (const auto& [prop, value] : props) {
        auto mapped = dest(e, prop);
        if (mapped != TEXT && mapped != attr::BY && mapped != attr::STATUS) OMP_TRY(write(w, id, e, prop, value));
    }
    return std::optional<Id>{id};
}
Omp2Mapper::Result Omp2Mapper::insert_ext(const DomWriter& w, Handle h) {
    const auto& n = tree_.at(h);
    if (!n.parent) return std::unexpected(bad("an element without a parent"));
    auto app = app_parent_of(*n.parent);
    if (!app) return std::unexpected(app.error());
    Attrs attrs{{std::string(attr::HARNESS), "omp2"}, {std::string(attr::TYPE), n.tag.name},
                {std::string(HANDLE_ATTR), handle_value(h)}};
    for (const auto& [prop, value] : n.props) if (auto converted = to_value(value)) attrs.insert_or_assign(x(prop), *converted);
    auto id = w.commit(Draft{Draft::Insert{app->first, app_index(h), ElementKind::Ext, std::move(attrs), n.content.value_or("")}});
    if (!id) return std::unexpected(to_adapter(std::move(id).error()));
    bind(h, *id, app->first);
    set_target(h, {TargetKind::Node, *id, {}});
    return {};
}
Omp2Mapper::Result Omp2Mapper::write(const DomWriter& w, Id id, Element e, std::string_view prop, const Json& v) {
    auto [name, value] = element_write(e, prop, v);
    if (!validate_set(kind(e), name, value ? &*value : nullptr)) {
        name = x(prop);
        value = to_value(v);
    }
    OMP_TRY(w.commit(Draft{Draft::Set{id, name, value}}));
    if (name != attr::STATUS || !value || !value->as_str()) return {};
    const auto status = *value->as_str();
    const bool terminal = status == "done" || status == "error" || status == "cancelled";
    if (e == Element::Call && terminal) {
        open_.erase(id);
        OMP_TRY(w.set(id, attr::ENDED, w.now()));
        if (status == "error") if (auto result = result_of(id)) OMP_TRY(w.set(*result, attr::IS_ERROR, true));
    } else if (e == Element::Call) open_.insert_or_assign(id, true);
    else if ((e == Element::Job || e == Element::Subagent) && terminal) OMP_TRY(w.set(id, attr::ENDED, w.now()));
    return {};
}
std::optional<Omp2Mapper::Id> Omp2Mapper::result_of(Id call) const {
    auto it = back_.find(call);
    if (it == back_.end()) return {};
    for (auto child : kids(it->second.first)) {
        auto t = target(child);
        if (t && t->kind == TargetKind::Node && t->element == Element::Result) return t->id;
    }
    return {};
}
Omp2Mapper::Result Omp2Mapper::on_set(const DomWriter& w, Handle h, std::string_view prop, const Json& value) {
    auto n = node(h);
    if (!n) return std::unexpected(n.error());
    Id id;
    std::string name;
    switch ((*n)->target.kind) {
    case TargetKind::Unset: return std::unexpected(unknown(h));
    case TargetKind::Root:
        id = ledger_.session;
        name = prop == "name" && (value.is_string() || value.is_null()) ? std::string(attr::TITLE) : x(prop);
        break;
    case TargetKind::Flat: {
        if (!(*n)->parent) return std::unexpected(bad("a container without a parent"));
        auto app = app_parent_of(*(*n)->parent);
        if (!app) return std::unexpected(app.error());
        id = app->first;
        name = x((*n)->tag.name + "-" + std::string(prop));
        break;
    }
    case TargetKind::Assistant: return assistant_set(w, h, prop, value);
    case TargetKind::Node:
        id = (*n)->target.id;
        if ((*n)->target.element) return write(w, id, *(*n)->target.element, prop, value);
        name = x(prop);
        break;
    }
    OMP_TRY(w.commit(Draft{Draft::Set{id, std::move(name), to_value(value)}}));
    return {};
}
Omp2Mapper::Result Omp2Mapper::assistant_set(const DomWriter& w, Handle h, std::string_view prop, const Json& value) {
    if (prop == "text" || prop == "thinking") return {};
    std::vector<Id> blocks;
    for (auto child : kids(h)) {
        auto t = target(child);
        if (t && t->kind == TargetKind::Node && t->element == Element::Block) blocks.push_back(t->id);
    }
    auto name = prop == "model" ? std::string(attr::MODEL) : x(prop);
    auto status = value.as_string() ? stop_status(*value.as_string()) : "done";
    for (auto id : blocks) {
        if (prop == "stop-reason" && (open_.erase(id) != 0 || status != "done")) OMP_TRY(w.set(id, attr::STATUS, status));
        OMP_TRY(w.commit(Draft{Draft::Set{id, name, to_value(value)}}));
    }
    return {};
}

Omp2Mapper::Result Omp2Mapper::stream(const DomWriter& w, std::uint32_t sid, wire::StreamOp operation,
                                       std::optional<std::pair<Handle, std::string>> bound,
                                       std::optional<std::string> text) {
    auto missing = [sid] {
        return AdapterError::Protocol{"omp2 used stream " + std::to_string(sid) + ", which isn't open"};
    };
    if (operation == wire::StreamOp::Open) {
        if (!bound) return std::unexpected(bad("stream open without node and prop"));
        auto result = stream_target(bound->first, bound->second, {});
        if (!result) return std::unexpected(result.error());
        streams_.insert_or_assign(sid, std::move(*result));
    } else if (operation == wire::StreamOp::Append) {
        auto it = streams_.find(sid);
        if (it == streams_.end()) return std::unexpected(missing());
        if (!text) return std::unexpected(bad("stream append without text"));
        if (auto* s = std::get_if<TextStream>(&it->second)) OMP_TRY(w.append(s->id, std::move(*text)));
        else if (auto* s = std::get_if<PropStream>(&it->second)) s->text += *text;
    } else {
        auto it = streams_.find(sid);
        if (it == streams_.end()) return std::unexpected(missing());
        auto stream = std::move(it->second);
        streams_.erase(it);
        if (auto* s = std::get_if<TextStream>(&stream)) {
            if (open_.erase(s->id)) OMP_TRY(w.set(s->id, attr::STATUS, "done"));
        } else if (auto* s = std::get_if<PropStream>(&stream)) {
            if (tree_.contains(s->handle)) OMP_TRY(set_prop(w, s->handle, std::move(s->prop), Json(std::move(s->text))));
        }
    }
    return {};
}
contracts::Result<Omp2Mapper::Stream> Omp2Mapper::stream_target(Handle h, std::string_view prop,
                                                                 std::optional<std::string_view> text) const {
    auto n = node(h);
    if (!n) return std::unexpected(n.error());
    const auto& target = (*n)->target;
    if (target.kind == TargetKind::Unset) return std::unexpected(unknown(h));
    if (target.kind != TargetKind::Node) return Stream{IgnoredStream{}};
    if (target.element && dest(*target.element, prop) == TEXT) return Stream{TextStream{target.id}};
    const auto* current = (*n)->prop(prop);
    return Stream{PropStream{h, std::string(prop),
                            std::string(text.value_or(current && current->as_string() ? *current->as_string() : ""))}};
}
Omp2Mapper::Result Omp2Mapper::reset(const DomWriter& w, wire::Snapshot snapshot) {
    high_water_ = snapshot.high_water;
    tree_.clear();
    for (auto& n : snapshot.nodes) tree_.emplace(n.handle, Node(std::move(n.node), n.parent));
    if (!tree_.contains(wire::ROOT)) return std::unexpected(bad("a snapshot without the session root"));
    seed_.clear();
    for (const auto& stream : snapshot.streams) seed_.insert_or_assign({stream.node, stream.prop}, stream.text);
    streams_.clear();
    for (auto it = open_.begin(); it != open_.end(); ) {
        if (!it->second) it = open_.erase(it);
        else ++it;
    }
    std::unordered_set<Id> stale;
    for (auto [h, id] : ids_) if (!tree_.contains(h)) stale.insert(id);
    for (auto id : stale) {
        auto it = back_.find(id);
        if (it == back_.end() || !under(it->second.second, stale)) OMP_TRY(w.remove(id));
    }
    std::unordered_set<Id> gone;
    for (auto [id, pair] : back_) if (under(id, stale)) gone.insert(id);
    for (auto id : gone) {
        auto it = back_.find(id);
        if (it != back_.end()) {
            ids_.erase(it->second.first);
            back_.erase(it);
        }
    }
    forget(gone);
    OMP_TRY(reconcile(w, wire::ROOT));
    for (const auto& s : snapshot.streams) {
        auto t = stream_target(s.node, s.prop, s.text);
        if (!t) return std::unexpected(t.error());
        if (auto* text = std::get_if<TextStream>(&*t)) {
            auto target = this->target(s.node);
            if (target && target->kind == TargetKind::Node && target->element == Element::Block)
                open_.insert_or_assign(text->id, false);
        }
        streams_.insert_or_assign(s.sid, std::move(*t));
    }
    seed_.clear();
    return {};
}
Omp2Mapper::Result Omp2Mapper::reconcile(const DomWriter& w, Handle h) {
    Target target;
    if (auto it = ids_.find(h); it == ids_.end()) {
        if (h != wire::ROOT) return materialize(w, h, false);
        target = {TargetKind::Root, {}, {}};
    } else {
        auto resolved = rule_of(h);
        if (!resolved) return std::unexpected(resolved.error());
        if (resolved->rule == Rule::Element) target = {TargetKind::Node, it->second, resolved->element};
        else if (resolved->rule == Rule::Assistant) target = {TargetKind::Assistant, {}, {}};
        else target = {TargetKind::Node, it->second, {}};
    }
    set_target(h, target);
    auto props = tree_.at(h).props;
    auto children = tree_.at(h).kids;
    for (const auto& [prop, value] : props) OMP_TRY(on_set(w, h, prop, value));
    if (target.kind == TargetKind::Node && target.element) {
        auto e = *target.element;
        if (e == Element::Turn) turn_seen(target.id);
        for (const auto& [prop, value] : props) {
            if (dest(e, prop) == TEXT) {
                if (auto it = seed_.find({h, prop}); it != seed_.end()) OMP_TRY(w.set_text(target.id, it->second));
            } else if (dest(e, prop) == attr::CALL_ID && value.as_string())
                calls_.insert_or_assign(*value.as_string(), target.id);
        }
    }
    for (auto child : children) OMP_TRY(reconcile(w, child));
    return {};
}
contracts::Result<std::vector<wire::Reply>> Omp2Mapper::apply(wire::Input input, const DomWriter& w) {
    if (auto* event = std::get_if<wire::DomEvent>(&input)) OMP_TRY(dom(w, std::move(*event)));
    else OMP_TRY(frame(w, std::move(std::get<wire::Frame>(input))));
    return std::vector<wire::Reply>{};
}
contracts::Result<std::optional<wire::Reply>> Omp2Mapper::answer(Id decision, const contracts::Answer& answer,
                                                                   const DomWriter& w) {
    auto it = decisions_.find(decision);
    if (it == decisions_.end()) return std::unexpected(AdapterError::UnknownDecision{decision});
    Pending pending = std::move(it->second);
    decisions_.erase(it);
    wire::Reply reply;
    if (auto* approval = std::get_if<Approval>(&pending)) {
        auto choice = answer.values.empty() ? std::string_view{} : std::string_view(answer.values.front());
        bool approved = choice == core::answer::ALLOW_ONCE || choice == core::answer::ALLOW_SESSION;
        std::string_view scope = choice == core::answer::ALLOW_SESSION && approval->allow_session ? "session" : "once";
        reply = wire::reply::Approve{std::move(approval->prompt_id), approved, scope, answer.reason};
    } else {
        auto& select = std::get<Select>(pending);
        std::vector<std::string> values, typed;
        for (const auto& v : answer.values) {
            if (std::ranges::find(select.options, v) != select.options.end()) values.push_back(v);
            else typed.push_back(v);
        }
        std::optional<std::string> custom;
        if (!typed.empty()) {
            custom = typed.front();
            for (std::size_t i = 1; i < typed.size(); ++i) { *custom += '\n'; *custom += typed[i]; }
        }
        reply = wire::reply::ExtensionUiResponse{std::move(select.id), std::move(values), std::move(custom)};
    }
    OMP_TRY(w.set(decision, attr::ANSWER, answer.values));
    OMP_TRY(w.set(decision, attr::BY, answer.by));
    if (answer.reason) OMP_TRY(w.set(decision, attr::REASON, *answer.reason));
    OMP_TRY(w.close(decision, DecisionStatus::Answered));
    ledger_.answered(decision);
    if (decisions_.empty() && turn_) OMP_TRY(w.set(*turn_, attr::STATUS, TurnStatus::Running));
    return std::optional<wire::Reply>{std::move(reply)};
}

#undef OMP_TRY
} // namespace loa::harness::omp2
