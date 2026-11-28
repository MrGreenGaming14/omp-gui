#include "loa/harness/acp/mapper.hpp"

#include <algorithm>
#include <format>
#include <utility>

#include "loa/core/schema.hpp"

namespace loa::harness::acp {

namespace {

using namespace loa::core;
using loa::contracts::AdapterError;
using loa::runtime::json::Object;
namespace json = loa::runtime::json;

constexpr std::string_view HARNESS = "acp";

AdapterError commit_error(CommitError e) { return AdapterError(AdapterError::Commit{std::move(e)}); }

const std::string* str_of(const Json& v, std::string_view key) {
    const Json* item = v.find(key);
    return item ? item->as_string() : nullptr;
}

bool is_str(const Json* v, std::string_view s) { return v && v->as_string() && *v->as_string() == s; }

// `value` at a `/a/b` path of object keys.
const Json* pointer(const Json& value, std::initializer_list<std::string_view> path) {
    const Json* at = &value;
    for (auto key : path) {
        at = at->find(key);
        if (at == nullptr) return nullptr;
    }
    return at;
}

reply::Respond cancelled_reply(Json id) {
    return reply::Respond{std::move(id), Json(Object{{"outcome", Object{{"outcome", "cancelled"}}}})};
}

#define LOA_OUT(expr)                                         \
    do {                                                      \
        if (auto r_ = (expr); !r_) return std::unexpected(std::move(r_).error()); \
    } while (0)

// Writes the `<result>` of a finished call; content without a text form (diffs, terminals,
// resources) goes to `<ext>` children.
std::expected<void, CommitError> finish_call(const DomWriter& w, NodeId node, CallStatus status, std::vector<Json> content,
                                             std::optional<Json> raw_output) {
    std::vector<Json> other;
    std::string text;
    bool first = true;
    for (auto& item : content) {
        const Json* t = pointer(item, {"content", "text"});
        if (is_str(item.find("type"), "content") && t && t->as_string()) {
            if (!first) text += '\n';
            text += *t->as_string();
            first = false;
        } else {
            other.push_back(std::move(item));
        }
    }
    Attrs attrs{{std::string(attr::IS_ERROR), Value(status == CallStatus::Error)}};
    if (raw_output && text.empty()) {
        if (raw_output->as_string()) {
            text = *raw_output->as_string();
        } else {
            attrs.insert_or_assign(std::string(attr::FORMAT), Value("json"));
            text = json::dump(*raw_output);
        }
    }
    LOA_OUT(w.insert(node, ElementKind::Result, std::move(attrs), std::move(text)));
    for (auto& item : other) {
        sort_all_objects(item);
        LOA_OUT(w.ext(node, HARNESS, "tool_call_content", json::dump(item)));
    }
    return w.close(node, status);
}

// Canonical approval answers advertised for the offered ACP permission options.
std::vector<std::string> canonical_options(const std::vector<wire::PermissionOption>& options) {
    using K = wire::PermissionOptionKind;
    auto has = [&](auto pred) { return std::ranges::any_of(options, [&](const auto& o) { return pred(o.kind); }); };
    std::vector<std::string> out;
    if (has([](K k) { return k == K::AllowOnce; })) out.emplace_back(answer::ALLOW_ONCE);
    if (has([](K k) { return k == K::AllowAlways; })) out.emplace_back(answer::ALLOW_SESSION);
    if (has([](K k) { return k == K::RejectOnce || k == K::RejectAlways; })) out.emplace_back(answer::DENY);
    return out;
}

} // namespace

void sort_all_objects(Json& value) {
    if (auto* array = value.as_array()) {
        for (auto& item : *array) sort_all_objects(item);
    } else if (auto* object = value.as_object()) {
        for (auto& [key, item] : *object) sort_all_objects(item);
        std::ranges::stable_sort(*object, {}, [](const auto& field) -> const std::string& { return field.first; });
    }
}

const wire::PermissionOption* pick_option(const std::vector<wire::PermissionOption>& options, std::string_view value) {
    using K = wire::PermissionOptionKind;
    for (const auto& o : options)
        if (o.option_id == value) return &o;
    for (const auto& o : options)
        if (o.name == value) return &o;
    std::vector<K> kinds;
    if (value == answer::ALLOW_ONCE) kinds = {K::AllowOnce};
    else if (value == answer::ALLOW_SESSION) kinds = {K::AllowAlways};
    else if (value == answer::DENY) kinds = {K::RejectOnce, K::RejectAlways};
    for (K kind : kinds)
        for (const auto& o : options)
            if (o.kind == kind) return &o;
    return nullptr;
}

Input decode_fixture(Json raw) {
    auto text = [&](std::string_view key) -> std::optional<std::string> {
        if (const std::string* s = str_of(raw, key)) return *s;
        return std::nullopt;
    };
    auto take = [&](std::string_view key) -> Json {
        const Json* item = raw.find(key);
        return item ? *item : Json();
    };
    const std::string* kind = str_of(raw, "$");
    const std::string_view k = kind ? std::string_view(*kind) : std::string_view();
    if (kind && k == "send")
        return input::Prompt{"1", contracts::Prompt{text("text").value_or(""), {}, "owner"}};
    if (kind && k == "result") {
        if (auto error = text("error")) return input::Response{"1", std::unexpected(std::move(*error))};
        return input::Response{"1", take("result")};
    }
    if (kind && k == "interrupt") return input::Interrupt{};
    if (kind && k == "closed") return input::Closed{false};
    if (kind && k == "load") return input::Load{"99"};
    if (kind && k == "loaded") return input::Response{"99", Json()};
    if (kind && k == "answer") {
        std::vector<std::string> values;
        if (const Json* list = raw.find("values"); list && list->as_array())
            for (const auto& v : *list->as_array())
                if (v.as_string()) values.push_back(*v.as_string());
        contracts::Answer answer{std::move(values), text("reason"), text("by").value_or("owner")};
        return input::Answer{take("request"), std::move(answer)};
    }
    if (is_str(raw.find("method"), "session/update")) return input::Update{take("params")};
    return input::Permission{take("id"), take("params")};
}

AcpMapper::Call* AcpMapper::Turn::call(std::string_view id) {
    for (auto& [key, call] : calls)
        if (key == id) return &call;
    return nullptr;
}

AcpMapper::AcpMapper(NodeId root, std::optional<std::string> model) : ledger_(root), model_(std::move(model)) {}

std::optional<NodeId> AcpMapper::turn_node() const {
    if (const auto* turn = ledger_.turn()) return turn->node;
    return std::nullopt;
}

std::optional<std::string_view> AcpMapper::mode_id() const {
    if (mode_id_) return std::string_view(*mode_id_);
    return std::nullopt;
}

contracts::Result<std::vector<Reply>> AcpMapper::apply(Input in, const DomWriter& w) {
    auto lift = [](auto r) -> contracts::Result<std::vector<Reply>> {
        if (!r) return std::unexpected(commit_error(std::move(r).error()));
        if constexpr (std::is_same_v<std::remove_cvref_t<decltype(*r)>, std::vector<Reply>>) return std::move(*r);
        else return std::vector<Reply>{};
    };
    if (auto* prompt = std::get_if<input::Prompt>(&in)) {
        auto turn = ledger_.begin(w, prompt->prompt);
        if (!turn) return std::unexpected(commit_error(std::move(turn).error()));
        if (auto r = w.set(*turn, attr::STARTED, Value(w.now())); !r) return std::unexpected(commit_error(std::move(r).error()));
        turn_ = Turn{};
        turn_.prompt = std::move(prompt->id);
        return std::vector<Reply>{};
    }
    if (auto* response = std::get_if<input::Response>(&in)) {
        if (replay_ == response->id) replay_.reset();
        if (turn_.prompt == response->id && ledger_.turn()) {
            TurnEnd end = [&]() -> TurnEnd {
                if (!response->result) return Failed{std::move(response->result).error()};
                auto decoded = wire::decode_prompt_response(*response->result);
                if (!decoded) return Failed{"malformed session/prompt response: " + decoded.error()};
                return Stop{decoded->stop_reason};
            }();
            return lift(end_turn(w, std::move(end)));
        }
        return std::vector<Reply>{};
    }
    if (auto* load = std::get_if<input::Load>(&in)) {
        replay_ = std::move(load->id);
        return std::vector<Reply>{};
    }
    if (std::holds_alternative<input::Interrupt>(in)) {
        if (!ledger_.turn()) return std::vector<Reply>{};
        turn_.interrupted = true;
        auto replies = cancel_decisions(w);
        if (!replies) return std::unexpected(commit_error(std::move(replies).error()));
        replies->emplace_back(reply::Cancel{});
        return std::move(*replies);
    }
    if (auto* closed = std::get_if<input::Closed>(&in)) {
        auto replies = end_turn(w, Gone{closed->shutdown});
        if (!replies) return std::unexpected(commit_error(std::move(replies).error()));
        ledger_.close();
        return std::move(*replies);
    }
    if (auto* update = std::get_if<input::Update>(&in)) {
        bool ours = true;
        if (const std::string* theirs = str_of(update->params, "sessionId"); native_ && theirs) ours = *native_ == *theirs;
        const Json* body = update->params.find("update");
        if (ours && !replay_ && body) return lift(this->update(w, *body));
        return std::vector<Reply>{};
    }
    if (auto* permission = std::get_if<input::Permission>(&in)) {
        auto request = wire::decode_permission(permission->params);
        if (!request) return std::vector<Reply>{reply::Reject{std::move(permission->id), std::move(request).error()}};
        return lift(this->permission(w, std::move(permission->id), std::move(*request)));
    }
    auto& answered = std::get<input::Answer>(in);
    std::optional<NodeId> node;
    for (const auto& [id, pending] : decisions_)
        if (pending.rpc_id == answered.request) { node = id; break; }
    if (!node)
        return std::unexpected(AdapterError(AdapterError::Protocol{"no permission request " + json::dump(answered.request)}));
    auto reply = answer(*node, answered.answer, w);
    if (!reply) return std::unexpected(std::move(reply).error());
    std::vector<Reply> out;
    if (*reply) out.push_back(std::move(**reply));
    return out;
}

contracts::Result<std::optional<Reply>> AcpMapper::answer(NodeId decision, const contracts::Answer& answer, const DomWriter& w) {
    auto it = decisions_.find(decision);
    if (it == decisions_.end()) return std::unexpected(AdapterError(AdapterError::UnknownDecision{decision}));
    if (answer.values.empty()) return std::unexpected(AdapterError(AdapterError::Protocol{"the answer is empty"}));
    const std::string& value = answer.values.front();
    const auto* picked = pick_option(it->second.options, value);
    if (!picked)
        return std::unexpected(AdapterError(AdapterError::Protocol{std::format("`{}` matches none of the offered options", value)}));
    wire::PermissionOption option = *picked;
    Json rpc_id = std::move(it->second.rpc_id);
    decisions_.erase(it);
    std::string_view canonical = answer::DENY;
    if (option.kind == wire::PermissionOptionKind::AllowOnce) canonical = answer::ALLOW_ONCE;
    else if (option.kind == wire::PermissionOptionKind::AllowAlways) canonical = answer::ALLOW_SESSION;
    auto write = [&]() -> std::expected<void, CommitError> {
        LOA_OUT(w.set(decision, attr::ANSWER, Value(std::vector<std::string>{std::string(canonical)})));
        LOA_OUT(w.set(decision, X_OPTION, Value(option.option_id)));
        if (!answer.by.empty()) LOA_OUT(w.set(decision, attr::BY, Value(answer.by)));
        if (answer.reason) LOA_OUT(w.set(decision, attr::REASON, Value(*answer.reason)));
        LOA_OUT(w.close(decision, Value(DecisionStatus::Answered)));
        ledger_.answered(decision);
        if (const auto* turn = ledger_.turn(); turn && decisions_.empty())
            LOA_OUT(w.set(turn->node, attr::STATUS, Value(TurnStatus::Running)));
        return {};
    };
    if (auto r = write(); !r) return std::unexpected(commit_error(std::move(r).error()));
    Json result(Object{{"outcome", Object{{"optionId", option.option_id}, {"outcome", "selected"}}}});
    return Reply(reply::Respond{std::move(rpc_id), std::move(result)});
}

void AcpMapper::adopt(const Dom& dom) {
    const NodeId r = root();
    todos_.clear();
    for (const Node* todo : dom.children_of_kind(r, ElementKind::Todo)) {
        auto state = todo->str_attr(attr::STATE);
        auto parsed = state ? parse<TodoState>(*state) : std::nullopt;
        auto priority = todo->str_attr(X_PRIORITY);
        todos_.push_back({todo->id, todo->text, parsed.value_or(TodoState::Pending),
                          priority ? std::optional<std::string>(*priority) : std::nullopt});
    }
    const Node* node = dom.get(r);
    auto mode = node ? node->str_attr(X_MODE) : std::nullopt;
    mode_id_ = mode ? std::optional<std::string>(*mode) : std::nullopt;
}

std::expected<void, CommitError> AcpMapper::set_native(const DomWriter& w, std::string_view id) {
    native_ = std::string(id);
    return w.set(root(), attr::NATIVE_ID, Value(id));
}

std::expected<void, CommitError> AcpMapper::model_changed(const DomWriter& w, std::string_view model) {
    if (model_ == model) return {};
    model_ = std::string(model);
    return w.set(root(), attr::MODEL, Value(model));
}

std::expected<void, CommitError> AcpMapper::mode_changed(const DomWriter& w, std::string_view mode_id) {
    if (mode_id_ == mode_id) return {};
    mode_id_ = std::string(mode_id);
    LOA_OUT(w.set(root(), X_MODE, Value(mode_id)));
    return w.set(root(), attr::MODE, Value(mode_id == "plan" ? SessionMode::Plan : SessionMode::Normal));
}

std::expected<std::vector<Reply>, CommitError> AcpMapper::end_turn(const DomWriter& w, TurnEnd end) {
    auto node = turn_node();
    if (!node) return std::vector<Reply>{};
    Turn turn = std::exchange(turn_, Turn{});
    const auto* stop = std::get_if<Stop>(&end);
    const auto* gone = std::get_if<Gone>(&end);
    const bool cancelled = turn.interrupted || (stop && stop->reason == wire::StopReason::Cancelled) || (gone && gone->shutdown);
    TurnStatus turn_status;
    MessageStatus message_status;
    if (cancelled) {
        turn_status = TurnStatus::Cancelled;
        message_status = MessageStatus::Cancelled;
    } else if (stop && stop->reason == wire::StopReason::EndTurn) {
        turn_status = TurnStatus::Done;
        message_status = MessageStatus::Done;
    } else if (stop) {
        // Truncated output is still a complete message; the turn records why it stopped.
        turn_status = TurnStatus::Error;
        message_status = MessageStatus::Done;
    } else {
        turn_status = TurnStatus::Error;
        message_status = MessageStatus::Error;
    }
    if (turn.message) LOA_OUT(w.set(std::get<0>(*turn.message), attr::STATUS, Value(message_status)));
    for (const auto& [id, call] : turn.calls)
        if (call.status == CallStatus::Pending || call.status == CallStatus::Running)
            LOA_OUT(w.close(call.node, Value(CallStatus::Cancelled)));
    auto replies = cancel_decisions(w);
    if (!replies) return replies;
    if (stop && stop->reason != wire::StopReason::EndTurn) {
        LOA_OUT(w.set(*node, X_STOP_REASON, Value(wire::as_str(stop->reason))));
    } else if (const auto* failed = std::get_if<Failed>(&end)) {
        LOA_OUT(w.ext(*node, HARNESS, "error", failed->message));
    } else if (gone && !gone->shutdown) {
        LOA_OUT(w.ext(*node, HARNESS, "error", "the agent exited"));
    }
    LOA_OUT(ledger_.end_turn(w, turn_status));
    return replies;
}

std::expected<std::vector<Reply>, CommitError> AcpMapper::cancel_decisions(const DomWriter& w) {
    std::vector<Reply> replies;
    replies.reserve(decisions_.size());
    auto drained = std::exchange(decisions_, {});
    for (auto& [node, pending] : drained) {
        LOA_OUT(w.close(node, Value(DecisionStatus::Cancelled)));
        ledger_.answered(node);
        replies.emplace_back(cancelled_reply(std::move(pending.rpc_id)));
    }
    if (const auto* turn = ledger_.turn(); turn && !replies.empty())
        LOA_OUT(w.set(turn->node, attr::STATUS, Value(TurnStatus::Running)));
    return replies;
}

std::expected<void, CommitError> AcpMapper::ext_here(const DomWriter& w, std::string_view kind, std::string raw) const {
    LOA_OUT(w.ext(turn_node().value_or(root()), HARNESS, kind, std::move(raw)));
    return {};
}

std::expected<void, CommitError> AcpMapper::update(const DomWriter& w, Json update) {
    const std::string* kind_ptr = str_of(update, "sessionUpdate");
    if (!kind_ptr) return ext_here(w, "malformed", json::dump(update));
    const std::string kind = *kind_ptr;
    auto kept = [&](const std::string& e) { return e + ": " + json::dump(update); };
    if (kind == "agent_message_chunk") return chunk(w, MessageKind::Text, kind, update);
    if (kind == "agent_thought_chunk") return chunk(w, MessageKind::Thinking, kind, update);
    if (kind == "tool_call" || kind == "tool_call_update") {
        auto fields = wire::decode_tool_call(update);
        // Malformed, or outside any turn: keep it raw.
        if (fields && turn_node()) {
            auto r = tool_call(w, std::move(*fields));
            if (!r) return std::unexpected(std::move(r).error());
            return {};
        }
        return ext_here(w, kind, json::dump(update));
    }
    if (kind == "plan") {
        auto plan = wire::decode_plan(update);
        if (!plan) return ext_here(w, kind, kept(plan.error()));
        return this->plan(w, std::move(*plan));
    }
    if (kind == "current_mode_update") {
        auto mode = wire::decode_current_mode(update);
        if (!mode) return ext_here(w, kind, kept(mode.error()));
        return mode_changed(w, mode->current_mode_id);
    }
    if (kind == "session_info_update") {
        auto info = wire::decode_session_info(update);
        if (info && info->title) return w.set(root(), attr::TITLE, Value(*info->title));
        return ext_here(w, kind, json::dump(update));
    }
    if (auto* object = update.as_object())
        std::erase_if(*object, [](const auto& field) { return field.first == "sessionUpdate"; });
    return ext_here(w, kind, json::dump(update));
}

std::expected<void, CommitError> AcpMapper::chunk(const DomWriter& w, MessageKind kind, std::string_view update_kind,
                                                  const Json& update) {
    std::optional<std::string> message_id;
    if (const std::string* id = str_of(update, "messageId")) message_id = *id;
    const Json* content = update.find("content");
    const std::string* text = content && is_str(content->find("type"), "text") ? str_of(*content, "text") : nullptr;
    if (!text) return ext_here(w, update_kind, json::dump(update));
    auto turn = turn_node();
    if (!turn) {
        LOA_OUT(w.ext(root(), HARNESS, update_kind, *text));
        return {};
    }
    if (turn_.message) {
        const auto& [node, open_kind, open_id] = *turn_.message;
        if (open_kind == kind && (!message_id || !open_id || *open_id == *message_id)) return w.append(node, *text);
    }
    if (turn_.message) {
        LOA_OUT(w.set(std::get<0>(*turn_.message), attr::STATUS, Value(MessageStatus::Done)));
        turn_.message.reset();
    }
    Attrs attrs{{std::string(attr::ROLE), Value(Role::Assistant)}, {std::string(attr::STATUS), Value(MessageStatus::Streaming)}};
    if (kind == MessageKind::Thinking) attrs.insert_or_assign(std::string(attr::KIND), Value(kind));
    if (model_) attrs.insert_or_assign(std::string(attr::MODEL), Value(*model_));
    if (message_id) attrs.insert_or_assign(std::string(attr::ID), Value(*message_id));
    auto node = w.insert(*turn, ElementKind::Message, std::move(attrs), *text);
    if (!node) return std::unexpected(std::move(node).error());
    turn_.message.emplace(*node, kind, std::move(message_id));
    return {};
}

std::expected<std::optional<NodeId>, CommitError> AcpMapper::tool_call(const DomWriter& w, wire::ToolCallFields fields) {
    auto turn = turn_node();
    if (!turn) return std::nullopt;
    const std::string id = fields.tool_call_id;
    std::optional<std::string> tool = fields.name ? fields.name : fields.kind;
    NodeId node;
    if (Call* call = turn_.call(id)) {
        node = call->node;
        if (fields.title) LOA_OUT(w.set(node, attr::TITLE, Value(*fields.title)));
        if (tool) LOA_OUT(w.set(node, attr::TOOL, Value(*tool)));
    } else {
        if (turn_.message) {
            LOA_OUT(w.set(std::get<0>(*turn_.message), attr::STATUS, Value(MessageStatus::Done)));
            turn_.message.reset();
        }
        const std::string& title = fields.title ? *fields.title : id;
        Attrs attrs{{std::string(attr::TOOL), Value(tool.value_or("other"))},
                    {std::string(attr::CALL_ID), Value(id)},
                    {std::string(attr::TITLE), Value(title)},
                    {std::string(attr::STATUS), Value(CallStatus::Pending)},
                    {std::string(attr::STARTED), Value(w.now())}};
        auto inserted = w.insert(*turn, ElementKind::Call, std::move(attrs), "");
        if (!inserted) return std::unexpected(std::move(inserted).error());
        node = *inserted;
        turn_.calls.emplace_back(id, Call{node, std::nullopt, CallStatus::Pending, {}, std::nullopt});
    }
    Call& call = *turn_.call(id);
    if (fields.raw_input && !fields.raw_input->is_null()) {
        std::string input = json::dump(*fields.raw_input);
        if (call.input) {
            LOA_OUT(w.set_text(*call.input, std::move(input)));
        } else {
            auto inserted = w.insert(node, ElementKind::Input, Attrs{{std::string(attr::FORMAT), Value("json")}}, std::move(input));
            if (!inserted) return std::unexpected(std::move(inserted).error());
            call.input = *inserted;
        }
    }
    if (fields.content) call.content = std::move(*fields.content);
    if (fields.raw_output && !fields.raw_output->is_null()) call.raw_output = std::move(*fields.raw_output);
    if (!fields.status || *fields.status == wire::ToolCallStatus::Pending) return node;
    CallStatus status = CallStatus::Error;
    switch (*fields.status) {
    case wire::ToolCallStatus::InProgress: status = CallStatus::Running; break;
    case wire::ToolCallStatus::Completed: status = CallStatus::Done; break;
    default: break;
    }
    if (call.status == status || call.status == CallStatus::Done || call.status == CallStatus::Error) return node;
    call.status = status;
    if (status == CallStatus::Running) {
        LOA_OUT(w.set(node, attr::STATUS, Value(status)));
    } else {
        auto content = std::exchange(call.content, {});
        auto raw_output = std::exchange(call.raw_output, std::nullopt);
        LOA_OUT(finish_call(w, node, status, std::move(content), std::move(raw_output)));
    }
    return node;
}

std::expected<void, CommitError> AcpMapper::plan(const DomWriter& w, wire::Plan plan) {
    const std::size_t kept = plan.entries.size();
    for (std::size_t index = 0; index < plan.entries.size(); ++index) {
        auto& entry = plan.entries[index];
        TodoState state = TodoState::Pending;
        if (entry.status == wire::PlanEntryStatus::InProgress) state = TodoState::InProgress;
        else if (entry.status == wire::PlanEntryStatus::Completed) state = TodoState::Done;
        if (index >= todos_.size()) {
            Attrs attrs{{std::string(attr::STATE), Value(state)}};
            if (entry.priority) attrs.insert_or_assign(std::string(X_PRIORITY), Value(*entry.priority));
            auto node = w.insert(root(), ElementKind::Todo, std::move(attrs), entry.content);
            if (!node) return std::unexpected(std::move(node).error());
            todos_.push_back({*node, std::move(entry.content), state, std::move(entry.priority)});
            continue;
        }
        Todo& todo = todos_[index];
        if (todo.text != entry.content) LOA_OUT(w.set_text(todo.node, entry.content));
        if (todo.state != state) LOA_OUT(w.set(todo.node, attr::STATE, Value(state)));
        if (todo.priority != entry.priority) {
            if (entry.priority) LOA_OUT(w.set(todo.node, X_PRIORITY, Value(*entry.priority)));
            else LOA_OUT(w.clear(todo.node, X_PRIORITY));
        }
        todo = Todo{todo.node, std::move(entry.content), state, std::move(entry.priority)};
    }
    const std::size_t keep = std::min(kept, todos_.size());
    for (std::size_t i = keep; i < todos_.size(); ++i) LOA_OUT(w.remove(todos_[i].node));
    todos_.erase(todos_.begin() + static_cast<std::ptrdiff_t>(keep), todos_.end());
    return {};
}

std::expected<void, CommitError> AcpMapper::permission(const DomWriter& w, Json rpc_id, wire::RequestPermissionRequest request) {
    auto title = request.tool_call.title;
    std::optional<std::string> input;
    if (request.tool_call.raw_input && !request.tool_call.raw_input->is_null()) input = json::dump(*request.tool_call.raw_input);
    auto content = request.tool_call.content.value_or(std::vector<Json>{});
    // The request's tool call doubles as an update: it may be the first the client hears of it.
    std::optional<NodeId> call;
    if (Call* known = turn_.call(request.tool_call.tool_call_id)) {
        call = known->node;
    } else {
        auto inserted = tool_call(w, std::move(request.tool_call));
        if (!inserted) return std::unexpected(std::move(inserted).error());
        call = *inserted;
    }
    const NodeId parent = call ? *call : turn_node().value_or(root());
    std::vector<std::string> ids, kinds;
    for (const auto& o : request.options) {
        ids.push_back(o.option_id);
        kinds.emplace_back(wire::as_str(o.kind));
    }
    const std::string request_id = rpc_id.as_string() ? *rpc_id.as_string() : json::dump(rpc_id);
    Attrs attrs{{std::string(attr::KIND), Value(DecisionKind::Approval)},
                {std::string(attr::STATUS), Value(DecisionStatus::Waiting)},
                {std::string(attr::OPTIONS), Value(canonical_options(request.options))},
                {std::string(X_OPTION_IDS), Value(ids)},
                {std::string(X_OPTION_KINDS), Value(kinds)},
                {std::string(attr::REQUEST_ID), Value(request_id)},
                {std::string(attr::CREATED), Value(w.now())}};
    if (call) attrs.insert_or_assign(std::string(attr::CALL), Value(*call));
    if (title) attrs.insert_or_assign(std::string(attr::TITLE), Value(*title));
    auto node = w.insert(parent, ElementKind::Decision, std::move(attrs), input.value_or(""));
    if (!node) return std::unexpected(std::move(node).error());
    for (auto& item : content) {
        sort_all_objects(item);
        LOA_OUT(w.ext(*node, HARNESS, "tool_call_content", json::dump(item)));
    }
    if (auto turn = turn_node()) LOA_OUT(w.set(*turn, attr::STATUS, Value(TurnStatus::Waiting)));
    ledger_.wait(*node);
    decisions_.insert_or_assign(*node, Pending{std::move(rpc_id), std::move(request.options)});
    return {};
}

#undef LOA_OUT

} // namespace loa::harness::acp
