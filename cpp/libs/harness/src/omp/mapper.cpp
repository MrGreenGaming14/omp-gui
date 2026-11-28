#include "loa/harness/omp/mapper.hpp"

#include <algorithm>
#include <limits>
#include <sstream>
#include <type_traits>
#include <utility>

#include "loa/core/schema.hpp"
#include "loa/harness/omp/native.hpp"

namespace loa::harness::omp {
namespace {
using namespace loa::core;
using namespace loa::contracts;
using J = rpc::Json;
using Object = loa::runtime::json::Object;
using Array = loa::runtime::json::Array;
#define COMMIT(expr) do { auto result = (expr); if (!result) return std::unexpected(AdapterError::Commit{result.error()}); } while (false)
#define ADAPT(expr) do { auto result = (expr); if (!result) return std::unexpected(result.error()); } while (false)
const J* field(const J& j, std::string_view key) { return j.find(key); }
const std::string* str(const J* j) { return j ? j->as_string() : nullptr; }
std::string text(const J& j) { return j.as_string() ? *j.as_string() : loa::runtime::json::dump(j); }
std::string result_text(const J& j) { return field(j, "content") ? native::content_text(field(j, "content")) : text(j); }
std::string json(const J& j) { return loa::runtime::json::dump(j); }
J obj(Object entries) { return J(std::move(entries)); }
J nullable(const std::optional<std::string>& s) { return s ? J(*s) : J(); }
void sort_json(J& j) {
    if (auto* o = j.as_object()) { for (auto& [_, v] : *o) sort_json(v); std::sort(o->begin(), o->end(), [](const auto& a, const auto& b) { return a.first < b.first; }); }
    if (auto* a = j.as_array()) for (auto& v : *a) sort_json(v);
}
bool blank(std::string_view s) { return s.find_first_not_of(" \t\r\n") == std::string_view::npos; }
std::string join(const std::vector<std::string>& parts, std::string_view sep) {
    std::string out;
    for (const auto& s : parts) { if (!out.empty()) out += sep; out += s; }
    return out;
}
std::vector<std::string> labels(const rpc::AskQuestion& q) {
    std::vector<std::string> out; out.reserve(q.options.size());
    for (const auto& o : q.options) out.push_back(o.label);
    return out;
}
J form_fields(const std::vector<rpc::AskQuestion>& qs) {
    Object properties;
    Array required;
    for (const auto& q : qs) {
        auto names = labels(q);
        J f = obj({{"title", q.question}});
        if (q.header) f.set("description", *q.header);
        Array options; for (const auto& name : names) options.emplace_back(name);
        if (q.multi) { f.set("type", "array"); f.set("items", obj({{"type", "string"}, {"enum", J(std::move(options))}})); }
        else {
            f.set("type", "string");
            if (q.recommended && *q.recommended < names.size()) f.set("default", names[*q.recommended]);
            if (!names.empty()) f.set("enum", J(std::move(options)));
        }
        properties.emplace_back(q.id, std::move(f)); required.emplace_back(q.id);
    }
    J schema = obj({{"type", "object"}, {"properties", J(std::move(properties))}, {"required", J(std::move(required))}});
    sort_json(schema); return schema;
}
std::optional<std::vector<rpc::AskAnswer>> ask_answers(const std::vector<rpc::AskQuestion>& qs, const std::vector<std::string>& values) {
    if (qs.size() == 1) {
        if (values.empty()) return std::nullopt;
        auto names = labels(qs.front());
        bool picked = std::all_of(values.begin(), values.end(), [&](const auto& v) { return std::find(names.begin(), names.end(), v) != names.end(); });
        return std::vector<rpc::AskAnswer>{{qs.front().id, picked ? values : std::vector<std::string>{}, picked ? std::nullopt : std::optional<std::string>(join(values, "\n"))}};
    }
    if (values.size() != 1) return std::nullopt;
    auto parsed = loa::runtime::json::parse(values.front());
    if (!parsed || !parsed->is_object()) return std::nullopt;
    std::vector<rpc::AskAnswer> out;
    for (const auto& q : qs) {
        rpc::AskAnswer a{q.id, {}, std::nullopt};
        if (auto* v = parsed->find(q.id)) {
            if (auto* s = v->as_string()) {
                auto names = labels(q);
                if (std::find(names.begin(), names.end(), *s) != names.end()) a.selected_options.push_back(*s);
                else a.custom_input = *s;
            } else if (auto* arr = v->as_array()) for (const auto& item : *arr) if (auto* s = item.as_string()) a.selected_options.push_back(*s);
        }
        out.push_back(std::move(a));
    }
    return out;
}
struct Approval {
    std::string_view once, session, deny;
    const std::vector<std::string>* options;
    static std::optional<Approval> of(const std::vector<std::string>& options) {
        if (options.empty() || options.back() != "Deny") return std::nullopt;
        Approval a{{}, {}, options.back(), &options};
        for (const auto& o : options) {
            if (o == "Approve Once" || (o == "Approve" && a.once.empty())) a.once = o;
            if (o.starts_with("Approve ") && o.ends_with(" for this session")) a.session = o;
        }
        if (a.once.empty()) return std::nullopt;
        return a;
    }
    std::vector<std::string> answers() const {
        std::vector<std::string> out{std::string(answer::ALLOW_ONCE)};
        if (!session.empty()) out.emplace_back(answer::ALLOW_SESSION);
        out.emplace_back(answer::DENY); return out;
    }
    std::optional<std::string> label(std::string_view value) const {
        if (value == answer::ALLOW_ONCE) return std::string(once);
        if (value == answer::ALLOW_SESSION && !session.empty()) return std::string(session);
        if (value == answer::DENY) return std::string(deny);
        for (const auto& o : *options) if (o == value) return o;
        return std::nullopt;
    }
};
constexpr std::string_view OTHER = "Other (type your own)";
JobStatus job_status(rpc::SubagentStatus s) {
    switch (s) { case rpc::SubagentStatus::Started: return JobStatus::Running; case rpc::SubagentStatus::Completed: return JobStatus::Done;
        case rpc::SubagentStatus::Failed: return JobStatus::Error; case rpc::SubagentStatus::Aborted: return JobStatus::Cancelled; }
    return JobStatus::Error;
}
TodoState todo_state(rpc::TodoStatus s) {
    switch (s) { case rpc::TodoStatus::Pending: return TodoState::Pending; case rpc::TodoStatus::InProgress: return TodoState::InProgress;
        case rpc::TodoStatus::Completed: return TodoState::Done; case rpc::TodoStatus::Abandoned: return TodoState::Cancelled; }
    return TodoState::Cancelled;
}
} // namespace

Mapper::Mapper(NodeId session) : ledger_(session) {}
Mapper& Mapper::titled(bool value) { titled_ = value; return *this; }
std::optional<NodeId> Mapper::turn() const { return ledger_.turn() ? std::optional<NodeId>(ledger_.turn()->node) : std::nullopt; }
SessionState Mapper::state() const { return ledger_.state() == SessionState::Idle && !queued_.empty() ? SessionState::Running : ledger_.state(); }
bool Mapper::take_rename() {
    bool due = rename_ == Rename::Due && !titled_ && state() == SessionState::Idle;
    if (due) rename_ = Rename::Sent;
    return due;
}
void Mapper::rename_failed() { if (rename_ == Rename::Sent) rename_ = Rename::Done; }
void Mapper::withdraw(std::string_view id) { std::erase_if(queued_, [&](const auto& q) { return q.first == id; }); }
Result<NodeId> Mapper::open_turn(const Writer& w) {
    ADAPT(finish(w, TurnStatus::Done));
    std::optional<std::pair<std::string, Prompt>> p;
    if (!queued_.empty()) { p = std::move(queued_.front()); queued_.pop_front(); }
    auto t = ledger_.begin_with(w, p ? &p->second : nullptr);
    if (!t) return std::unexpected(AdapterError::Commit{t.error()});
    COMMIT(w.set(*t, attr::STARTED, w.now()));
    turns_.push_back(*t); open_.echo = p.has_value();
    open_.prompt = p ? std::optional<std::string>(std::move(p->first)) : std::nullopt;
    return *t;
}
Result<NodeId> Mapper::ensure_turn(const Writer& w) { return turn() ? loa::contracts::Result<NodeId>(*turn()) : open_turn(w); }
Mapper::Result Mapper::close(const Writer& w, NodeId node, Value status) { open_.closed.insert(node); COMMIT(w.set(node, attr::STATUS, std::move(status))); return {}; }
Mapper::Result Mapper::finish(const Writer& w, TurnStatus status) {
    if (!turn()) return {};
    auto node = *turn();
    auto m = status == TurnStatus::Cancelled ? MessageStatus::Cancelled : status == TurnStatus::Error ? MessageStatus::Error : MessageStatus::Done;
    Open open = std::move(open_); open_ = {};
    for (const auto& [_, id] : open.messages) if (!open.closed.contains(id)) COMMIT(w.set(id, attr::STATUS, m));
    if (status == TurnStatus::Cancelled) {
        for (const auto& [_, id] : open.calls) if (!open.closed.contains(id)) COMMIT(w.set(id, attr::STATUS, CallStatus::Cancelled));
        for (const auto* jobs : {&open.jobs, &open.subagents}) for (const auto& [_, id] : *jobs)
            if (!open.closed.contains(id)) COMMIT(w.set(id, attr::STATUS, JobStatus::Cancelled));
    }
    for (const auto& [id, _] : open.requests) COMMIT(w.set(id, attr::STATUS, DecisionStatus::Cancelled));
    COMMIT(w.close(node, status));
    ledger_ = kit::TurnLedger<std::string>(session());
    return {};
}
Mapper::Result Mapper::ext(const Writer& w, std::string_view kind, const J& raw) {
    J sorted = raw; sort_json(sorted);
    COMMIT(w.ext(turn().value_or(session()), "omp", kind, json(sorted))); return {};
}
Mapper::Result Mapper::unknown(const Writer& w, const J& raw) {
    auto* kind = str(field(raw, "type")); if (!kind) return {};
    if (*kind == COMMANDS) {
        J cmds = field(raw, "commands") ? *field(raw, "commands") : J();
        if (commands_ != cmds) {
            commands_ = std::move(cmds);
            J sorted = raw; sort_json(sorted);
            COMMIT(w.ext(session(), "omp", COMMANDS, json(sorted)));
        }
    } else if (*kind == "command_output") {
        std::string value = str(field(raw, "text")) ? *str(field(raw, "text")) : "";
        auto parent = turn(); if (!parent && !turns_.empty()) parent = turns_.back();
        if (parent) COMMIT(w.insert(*parent, ElementKind::Message,
            Attrs{{std::string(attr::ROLE), Role::Assistant}, {std::string(attr::KIND), MessageKind::Text}, {std::string(attr::STATUS), MessageStatus::Done}}, value));
        else ADAPT(ext(w, "command_output", raw));
    }
    return {};
}
Mapper::Result Mapper::todos(const Writer& w, const std::vector<rpc::TodoItem>& items) {
    for (std::size_t i = 0; i < items.size(); ++i) {
        auto key = std::to_string(i); auto state = todo_state(items[i].status);
        if (auto found = todos_.find(key); found != todos_.end()) {
            COMMIT(w.set(found->second, attr::STATE, state)); COMMIT(w.set_text(found->second, items[i].content));
        } else {
            auto node = w.insert(session(), ElementKind::Todo, Attrs{{std::string(attr::ID), key}, {std::string(attr::STATE), state}}, items[i].content);
            if (!node) return std::unexpected(AdapterError::Commit{node.error()});
            todos_.emplace(std::move(key), *node);
        }
    }
    for (const auto& [key, node] : todos_) if (std::stoull(key) >= items.size()) COMMIT(w.set(node, attr::STATE, TodoState::Cancelled));
    return {};
}

Mapper::Result Mapper::assistant(const Writer& w, const rpc::AssistantEvent& ev, const std::optional<std::string>& id) {
    std::string prefix = id ? *id : current_message_.value_or("assistant");
    return std::visit([&](const auto& e) -> Result {
        using T = std::decay_t<decltype(e)>;
        if constexpr (std::is_same_v<T, rpc::assistant::TextStart> || std::is_same_v<T, rpc::assistant::ThinkingStart>) {
            if (open_.blocks.contains(e.content_index)) return {};
            auto key = prefix + ":" + std::to_string(e.content_index);
            open_.blocks.emplace(e.content_index, key);
            auto t = ensure_turn(w); if (!t) return std::unexpected(t.error());
            auto node = w.insert(*t, ElementKind::Message, Attrs{{std::string(attr::ROLE), Role::Assistant},
                {std::string(attr::KIND), std::is_same_v<T, rpc::assistant::TextStart> ? MessageKind::Text : MessageKind::Thinking},
                {std::string(attr::STATUS), MessageStatus::Streaming}, {std::string(attr::ID), prefix}});
            if (!node) return std::unexpected(AdapterError::Commit{node.error()});
            open_.messages.emplace(std::move(key), *node);
        } else if constexpr (std::is_same_v<T, rpc::assistant::TextDelta> || std::is_same_v<T, rpc::assistant::ThinkingDelta>) {
            if (auto block = open_.blocks.find(e.content_index); block != open_.blocks.end()) {
                open_.text[block->second] += e.delta;
                if (auto node = open_.messages.find(block->second); node != open_.messages.end()) COMMIT(w.append(node->second, e.delta));
            }
        } else if constexpr (std::is_same_v<T, rpc::assistant::TextEnd> || std::is_same_v<T, rpc::assistant::ThinkingEnd>) {
            auto block = open_.blocks.find(e.content_index); if (block == open_.blocks.end()) return {};
            auto key = std::move(block->second); open_.blocks.erase(block);
            auto old = open_.text.find(key); bool changed = old == open_.text.end() || old->second != e.content;
            if (old != open_.text.end()) open_.text.erase(old);
            if (auto node = open_.messages.find(key); node != open_.messages.end()) {
                if (changed) COMMIT(w.set_text(node->second, e.content));
                ADAPT(close(w, node->second, MessageStatus::Done));
            }
        } else if constexpr (std::is_same_v<T, rpc::assistant::ToolcallStart>) {
            const J* block = e.block ? &*e.block : nullptr;
            if (!block && e.partial) if (const J* content = field(*e.partial, "content")) if (auto* array = content->as_array())
                if (e.content_index < array->size()) block = &(*array)[e.content_index];
            if (block) if (auto* key = str(field(*block, "id"))) open_.blocks[e.content_index] = *key;
        } else if constexpr (std::is_same_v<T, rpc::assistant::ToolcallDelta>) {
            if (auto block = open_.blocks.find(e.content_index); block != open_.blocks.end())
                if (auto node = open_.inputs.find(block->second); node != open_.inputs.end()) COMMIT(w.append(node->second, e.delta));
        } else if constexpr (std::is_same_v<T, rpc::assistant::ToolcallEnd>) {
            open_.blocks.erase(e.content_index);
            if (auto* key = str(field(e.tool_call, "id"))) if (auto node = open_.inputs.find(*key); node != open_.inputs.end())
                COMMIT(w.set_text(node->second, field(e.tool_call, "arguments") ? text(*field(e.tool_call, "arguments")) : ""));
        } else if constexpr (std::is_same_v<T, rpc::assistant::Done> || std::is_same_v<T, rpc::assistant::Error>) {
            open_.blocks.clear();
            for (const auto& [_, node] : open_.messages) if (!open_.closed.contains(node))
                ADAPT(close(w, node, std::is_same_v<T, rpc::assistant::Error> ? MessageStatus::Error : MessageStatus::Done));
        }
        return {};
    }, ev);
}

Mapper::Result Mapper::event(const Writer& w, const rpc::Event& ev) {
    if (auto* unknown = std::get_if<rpc::event::Unknown>(&ev)) {
        auto* type = str(field(unknown->frame, "type")); auto* content = str(field(unknown->frame, "text"));
        if (rename_ == Rename::Sent && type && *type == "command_output" && content && content->starts_with("Session renamed to ")) {
            rename_ = Rename::Done; return {};
        }
    }
    bool required = std::holds_alternative<rpc::event::MessageStart>(ev) || std::holds_alternative<rpc::event::MessageUpdate>(ev) ||
        std::holds_alternative<rpc::event::MessageEnd>(ev) || std::holds_alternative<rpc::event::ToolExecutionStart>(ev) ||
        std::holds_alternative<rpc::event::ToolExecutionUpdate>(ev) || std::holds_alternative<rpc::event::ToolExecutionEnd>(ev) ||
        std::holds_alternative<rpc::SubagentLifecycle>(ev);
    if (auto* raw = std::get_if<rpc::event::Unknown>(&ev)) {
        if (auto* type = str(field(raw->frame, "type")); type && *type == "command_output" && !queued_.empty()) required = true;
    }
    if (required) { auto t = ensure_turn(w); if (!t) return std::unexpected(t.error()); }
    return std::visit([&](const auto& e) -> Result {
        using T = std::decay_t<decltype(e)>;
        if constexpr (std::is_same_v<T, rpc::event::AgentStart> || std::is_same_v<T, rpc::event::TurnStart>) {
            auto t = ensure_turn(w); if (!t) return std::unexpected(t.error());
        } else if constexpr (std::is_same_v<T, rpc::event::SessionSettled>) { ADAPT(finish(w, TurnStatus::Done)); }
        else if constexpr (std::is_same_v<T, rpc::event::PromptResult>) { ADAPT(prompt_result(w, e)); }
        else if constexpr (std::is_same_v<T, rpc::event::MessageStart>) {
            current_message_ = e.message_id;
            if (str(field(e.message, "role")) && *str(field(e.message, "role")) == "user" &&
                (!str(field(e.message, "attribution")) || *str(field(e.message, "attribution")) == "user")) {
                if (open_.echo) open_.echo = false;
                else if (!queued_.empty()) { auto t = open_turn(w); if (!t) return std::unexpected(t.error()); open_.echo = false; }
            }
        } else if constexpr (std::is_same_v<T, rpc::event::MessageUpdate>) {
            if (e.role == "assistant") ADAPT(assistant(w, e.event, e.message_id));
        } else if constexpr (std::is_same_v<T, rpc::event::MessageEnd>) {
            auto prefix = e.message_id.value_or(current_message_.value_or("")) + ":";
            MessageStatus status = MessageStatus::Done;
            if (auto* reason = str(field(e.message, "stopReason"))) {
                if (*reason == "aborted") status = MessageStatus::Cancelled;
                if (*reason == "error") status = MessageStatus::Error;
            }
            std::optional<NodeId> last;
            for (const auto& [key, node] : open_.messages) if (key.starts_with(prefix)) {
                if (!open_.closed.contains(node)) ADAPT(close(w, node, status));
                if (!last || node > *last) last = node;
            }
            if (auto* role = str(field(e.message, "role")); role && *role == "assistant") {
                current_message_.reset();
                if (const J* usage = field(e.message, "usage"); usage && usage->is_object()) {
                    Attrs attrs;
                    for (auto [attr_name, key] : {std::pair{attr::INPUT_TOKENS, "input"}, {attr::OUTPUT_TOKENS, "output"},
                         {attr::CACHE_READ_TOKENS, "cacheRead"}, {attr::CACHE_WRITE_TOKENS, "cacheWrite"}})
                        if (auto* v = field(*usage, key)) if (auto n = v->as_i64()) attrs.emplace(attr_name, *n);
                    if (auto* cost = field(*usage, "cost")) if (auto* total = field(*cost, "total"))
                        if (auto number = total->as_f64()) attrs.emplace(attr::COST, *number);
                    auto* provider = str(field(e.message, "provider")); auto* model = str(field(e.message, "model"));
                    if (provider && model) attrs.emplace(attr::MODEL, *provider + "/" + *model);
                    else if (model || provider) attrs.emplace(attr::MODEL, model ? *model : *provider);
                    if (auto target = last ? last : turn()) COMMIT(w.insert(*target, ElementKind::Usage, std::move(attrs)));
                }
            }
        } else if constexpr (std::is_same_v<T, rpc::event::ToolExecutionStart>) {
            auto t = ensure_turn(w); if (!t) return std::unexpected(t.error());
            Attrs attrs{{std::string(attr::TOOL), e.tool_name}, {std::string(attr::CALL_ID), e.tool_call_id},
                {std::string(attr::STATUS), CallStatus::Running}};
            if (e.intent) attrs.emplace(attr::TITLE, *e.intent);
            auto call = w.insert(*t, ElementKind::Call, std::move(attrs));
            if (!call) return std::unexpected(AdapterError::Commit{call.error()});
            open_.calls[e.tool_call_id] = *call;
            if (e.tool_name == "bash" || e.tool_name == "shell" || e.tool_name == "exec" || e.tool_name == "remote_bash") {
                Attrs job{{std::string(attr::KIND), JobKind::Shell}, {std::string(attr::STATUS), JobStatus::Running},
                    {std::string(attr::ID), e.tool_call_id}};
                if (e.args) {
                    auto* command = field(*e.args, "command"); if (!command) command = field(*e.args, "cmd");
                    if (auto* s = str(command)) job.emplace(attr::COMMAND, *s);
                }
                auto j = w.insert(*t, ElementKind::Job, std::move(job));
                if (!j) return std::unexpected(AdapterError::Commit{j.error()});
                open_.jobs[e.tool_call_id] = *j;
            }
            if (e.args) {
                auto value = text(*e.args); open_.input_text[e.tool_call_id] = value;
                auto input = w.insert(*call, ElementKind::Input, Attrs{{std::string(attr::FORMAT), "json"}}, value);
                if (!input) return std::unexpected(AdapterError::Commit{input.error()});
                open_.inputs[e.tool_call_id] = *input;
            }
        } else if constexpr (std::is_same_v<T, rpc::event::ToolExecutionUpdate>) {
            if (e.args) if (auto node = open_.inputs.find(e.tool_call_id); node != open_.inputs.end()) {
                auto value = json(*e.args); auto& old = open_.input_text[e.tool_call_id];
                if (value.starts_with(old)) { if (value.size() != old.size()) COMMIT(w.append(node->second, value.substr(old.size()))); }
                else COMMIT(w.set_text(node->second, value));
                old = std::move(value);
            }
            if (e.partial_result) if (auto node = open_.jobs.find(e.tool_call_id); node != open_.jobs.end())
                COMMIT(w.set_text(node->second, result_text(*e.partial_result)));
        } else if constexpr (std::is_same_v<T, rpc::event::ToolExecutionEnd>) {
            if (auto call = open_.calls.find(e.tool_call_id); call != open_.calls.end()) {
                COMMIT(w.insert(call->second, ElementKind::Result, Attrs{{std::string(attr::IS_ERROR), e.is_error}},
                    e.result ? result_text(*e.result) : ""));
                ADAPT(close(w, call->second, e.is_error ? CallStatus::Error : CallStatus::Done));
            }
            if (auto job = open_.jobs.find(e.tool_call_id); job != open_.jobs.end())
                ADAPT(close(w, job->second, e.is_error ? JobStatus::Error : JobStatus::Done));
        } else if constexpr (std::is_same_v<T, rpc::event::TodoReminder>) { ADAPT(todos(w, e.todos)); }
        else if constexpr (std::is_same_v<T, rpc::event::TodoAutoClear>) {
            for (const auto& [_, id] : todos_) COMMIT(w.set(id, attr::STATE, TodoState::Cancelled));
        } else if constexpr (std::is_same_v<T, rpc::SubagentLifecycle>) {
            auto status = job_status(e.status);
            if (auto existing = open_.subagents.find(e.id); existing != open_.subagents.end()) {
                if (status == JobStatus::Running) COMMIT(w.set(existing->second, attr::STATUS, status));
                else ADAPT(close(w, existing->second, status));
            } else {
                Attrs attrs{{std::string(attr::KIND), JobKind::Subagent}, {std::string(attr::STATUS), status},
                    {std::string(attr::ID), e.id}, {std::string(attr::AGENT), e.agent}};
                if (e.description) attrs.emplace(attr::TITLE, *e.description);
                if (e.session_file && !blank(*e.session_file)) attrs.emplace(attr::SESSION, *e.session_file);
                auto node = w.insert(turn().value_or(session()), ElementKind::Job, std::move(attrs));
                if (!node) return std::unexpected(AdapterError::Commit{node.error()});
                if (status != JobStatus::Running) open_.closed.insert(*node);
                open_.subagents[e.id] = *node;
            }
        } else if constexpr (std::is_same_v<T, rpc::SubagentProgress>) {
            const NodeId* node = nullptr;
            if (auto* id = str(field(e.progress, "id")); id && !blank(*id)) {
                if (auto i = open_.subagents.find(*id); i != open_.subagents.end()) node = &i->second;
            } else if (e.parent_tool_call_id && !blank(*e.parent_tool_call_id) && e.index) {
                if (auto i = open_.subagents.find("agent:" + *e.parent_tool_call_id + ":" + std::to_string(*e.index)); i != open_.subagents.end()) node = &i->second;
            }
            if (node) COMMIT(w.append(*node, text(e.progress) + "\n"));
        } else if constexpr (std::is_same_v<T, rpc::event::CompactionEnd>) {
            if (e.result && !e.aborted && !e.skipped) {
                Value::List covers; for (auto id : turns_) covers.emplace_back(id);
                Attrs attrs{{std::string(attr::COVERS), Value(std::move(covers))}};
                if (e.result->tokens_before) attrs.emplace(attr::TOKENS_BEFORE, *e.result->tokens_before);
                COMMIT(w.insert(session(), ElementKind::Compaction, std::move(attrs), e.result->summary));
            }
        } else if constexpr (std::is_same_v<T, rpc::event::ConfigUpdate>) {
            if (e.model) COMMIT(w.set(session(), attr::MODEL, e.model->selector()));
            if (e.thinking_level) COMMIT(w.set(session(), attr::EFFORT, *e.thinking_level));
        } else if constexpr (std::is_same_v<T, rpc::event::ThinkingLevelChanged>) {
            if (e.thinking_level) COMMIT(w.set(session(), attr::EFFORT, *e.thinking_level));
        } else if constexpr (std::is_same_v<T, rpc::event::SessionInfoUpdate>) {
            COMMIT(w.set(session(), attr::NATIVE_ID, e.session_id));
            if (e.title && !blank(*e.title)) { titled_ = true; COMMIT(w.set(session(), attr::TITLE, *e.title)); }
        } else if constexpr (std::is_same_v<T, rpc::event::Notice>) {
            ADAPT(ext(w, "notice", obj({{"level", e.level}, {"message", e.message}, {"source", nullable(e.source)}})));
        } else if constexpr (std::is_same_v<T, rpc::event::Unknown>) { ADAPT(unknown(w, e.frame)); }
        return {};
    }, ev);
}

Mapper::Result Mapper::ui(const Writer& w, const rpc::UiRequest& request) {
    if (!rpc::is_dialog(request)) return std::visit([&](const auto& r) -> Result {
        using T = std::decay_t<decltype(r)>;
        if constexpr (std::is_same_v<T, rpc::ui::Cancel>) {
            if (auto d = open_.decisions.find(r.target_id); d != open_.decisions.end()) {
                open_.requests.erase(d->second); ledger_.answered(d->second);
                COMMIT(w.set(d->second, attr::STATUS, DecisionStatus::Cancelled));
            }
        } else if constexpr (std::is_same_v<T, rpc::ui::Notify>) {
            ADAPT(ext(w, "notice", obj({{"level", nullable(r.notify_type)}, {"message", r.message}, {"source", "extension"}})));
        } else if constexpr (std::is_same_v<T, rpc::ui::SetTitle>) {
            titled_ = true; COMMIT(w.set(session(), attr::TITLE, r.title));
        } else if constexpr (std::is_same_v<T, rpc::ui::OpenUrl>) {
            ADAPT(ext(w, "open_url", obj({{"url", r.url}, {"launchUrl", nullable(r.launch_url)}, {"instructions", nullable(r.instructions)}})));
        }
        return {};
    }, request);
    auto t = ensure_turn(w); if (!t) return std::unexpected(t.error());
    DecisionKind kind = DecisionKind::Question;
    std::string title, body;
    std::vector<std::string> options;
    bool multi = false;
    std::optional<std::uint64_t> timeout;
    std::optional<J> fields;
    std::visit([&](const auto& r) {
        using T = std::decay_t<decltype(r)>;
        if constexpr (std::is_same_v<T, rpc::ui::Select>) {
            title = r.title; body = r.title; timeout = r.timeout;
            if (auto approval = Approval::of(r.options)) {
                kind = DecisionKind::Approval; title = title.substr(0, title.find('\n')); options = approval->answers();
            } else options = r.options;
        } else if constexpr (std::is_same_v<T, rpc::ui::Confirm>) {
            kind = DecisionKind::Approval; title = r.title; body = r.message; options = {"yes", "no"}; timeout = r.timeout;
        } else if constexpr (std::is_same_v<T, rpc::ui::Input>) { title = body = r.title; timeout = r.timeout; }
        else if constexpr (std::is_same_v<T, rpc::ui::Editor>) { kind = DecisionKind::Plan; title = r.title; body = r.prefill.value_or(""); }
        else if constexpr (std::is_same_v<T, rpc::ui::Ask>) {
            timeout = r.timeout;
            if (r.questions.size() == 1) {
                const auto& q = r.questions.front(); title = q.header.value_or(q.question); body = q.question;
                options = labels(q); multi = q.multi;
            } else {
                kind = DecisionKind::Form; title = "Questions";
                for (const auto& q : r.questions) { if (!body.empty()) body += '\n'; body += q.question; }
                fields = form_fields(r.questions);
            }
        }
    }, request);
    Attrs attrs{{std::string(attr::KIND), kind}, {std::string(attr::STATUS), DecisionStatus::Waiting},
        {std::string(attr::REQUEST_ID), rpc::ui_id(request)}, {std::string(attr::TITLE), std::move(title)},
        {std::string(attr::OPTIONS), options}, {std::string(attr::MULTI), multi}};
    if (timeout) attrs.emplace(attr::TIMEOUT_MS, static_cast<std::int64_t>(*timeout));
    if (fields) attrs.emplace(attr::FIELDS, json(*fields));
    auto node = w.insert(*t, ElementKind::Decision, std::move(attrs), std::move(body));
    if (!node) return std::unexpected(AdapterError::Commit{node.error()});
    open_.decisions[rpc::ui_id(request)] = *node;
    open_.requests.emplace(*node, request); ledger_.wait(*node);
    COMMIT(w.set(*t, attr::STATUS, TurnStatus::Waiting)); return {};
}

Result<Reply> Mapper::reply(NodeId decision, const Answer& answer) const {
    auto it = open_.requests.find(decision);
    if (it == open_.requests.end()) return std::unexpected(AdapterError::UnknownDecision{decision});
    const auto& req = it->second;
    auto id = rpc::ui_id(req);
    const std::string* first = answer.values.empty() ? nullptr : &answer.values.front();
    auto cancelled = [&]() -> rpc::UiResponse { return rpc::ui_response::Cancelled{id, false}; };
    bool approval = false;
    rpc::UiResponse response = cancelled();
    std::visit([&](const auto& r) {
        using T = std::decay_t<decltype(r)>;
        if constexpr (std::is_same_v<T, rpc::ui::Select>) {
            if (first) {
                if (auto a = Approval::of(r.options)) { approval = true; if (auto label = a->label(*first)) response = rpc::ui_response::Value{id, *label}; }
                else if (std::find(r.options.begin(), r.options.end(), *first) == r.options.end() &&
                    std::find(r.options.begin(), r.options.end(), OTHER) != r.options.end()) response = rpc::ui_response::Value{id, std::string(OTHER)};
                else response = rpc::ui_response::Value{id, *first};
            }
        } else if constexpr (std::is_same_v<T, rpc::ui::Input> || std::is_same_v<T, rpc::ui::Editor>) {
            if (first) response = rpc::ui_response::Value{id, *first};
        } else if constexpr (std::is_same_v<T, rpc::ui::Confirm>) {
            approval = true; response = rpc::ui_response::Confirmed{id, first &&
                (*first == answer::ALLOW_ONCE || *first == answer::ALLOW_SESSION || *first == answer::APPROVE || *first == "yes" || *first == "true")};
        } else if constexpr (std::is_same_v<T, rpc::ui::Ask>) {
            if (auto answers = ask_answers(r.questions, answer.values)) response = rpc::ui_response::Answers{id, std::move(*answers)};
        }
    }, req);
    if (auto* s = std::get_if<rpc::ui::Select>(&req); s && Approval::of(s->options) && first &&
        std::holds_alternative<rpc::ui_response::Cancelled>(response))
        return std::unexpected(AdapterError::Protocol{"`" + *first + "` isn't an answer to this decision"});
    std::optional<std::string> steer;
    if (approval && first && *first == answer::DENY && answer.reason && !blank(*answer.reason)) {
        auto reason = *answer.reason;
        auto start = reason.find_first_not_of(" \t\r\n"), end = reason.find_last_not_of(" \t\r\n");
        steer = "I denied that tool call: " + reason.substr(start, end - start + 1);
    }
    return Reply{std::move(response), std::move(steer)};
}
Mapper::Result Mapper::answered(const Writer& w, NodeId decision, const Answer& answer) {
    COMMIT(w.set(decision, attr::ANSWER, Value(answer.values)));
    COMMIT(w.set(decision, attr::BY, answer.by));
    COMMIT(w.set(decision, attr::STATUS, DecisionStatus::Answered));
    if (answer.reason) COMMIT(w.set(decision, attr::REASON, *answer.reason));
    ledger_.answered(decision);
    if (auto t = turn()) COMMIT(w.set(*t, attr::STATUS, TurnStatus::Running));
    auto it = open_.requests.find(decision);
    if (it != open_.requests.end()) {
        if (auto* s = std::get_if<rpc::ui::Select>(&it->second); s && !Approval::of(s->options) && !answer.values.empty() &&
            std::find(s->options.begin(), s->options.end(), answer.values.front()) == s->options.end() &&
            std::find(s->options.begin(), s->options.end(), OTHER) != s->options.end()) open_.typed = answer.values.front();
        open_.requests.erase(it);
    }
    return {};
}
std::optional<rpc::UiResponse> Mapper::auto_reply(const rpc::UiRequest& request) {
    if (!rpc::is_dialog(request)) return std::nullopt;
    auto typed = std::exchange(open_.typed, std::nullopt);
    if (typed) if (auto* input = std::get_if<rpc::ui::Input>(&request)) return rpc::ui_response::Value{input->id, *typed};
    return std::nullopt;
}
Result<std::optional<Reply>> Mapper::answer(NodeId decision, const Answer& answer, const Writer& w) {
    auto r = reply(decision, answer); if (!r) return std::unexpected(r.error());
    ADAPT(answered(w, decision, answer));
    return std::optional<Reply>(std::move(*r));
}
Mapper::Result Mapper::prompt_result(const Writer& w, const rpc::event::PromptResult& result) {
    TurnStatus status = result.status == rpc::PromptStatus::Completed ? TurnStatus::Done :
        result.status == rpc::PromptStatus::Aborted ? TurnStatus::Cancelled : TurnStatus::Error;
    auto queued = result.id ? std::find_if(queued_.begin(), queued_.end(), [&](const auto& q) { return q.first == *result.id; }) : queued_.end();
    if (queued != queued_.end()) {
        Prompt prompt = std::move(queued->second); queued_.erase(queued);
        auto t = w.insert(session(), ElementKind::Turn, Attrs{{std::string(attr::STATUS), status}});
        if (!t) return std::unexpected(AdapterError::Commit{t.error()});
        COMMIT(w.set(*t, attr::ENDED, w.now())); turns_.push_back(*t);
        COMMIT(w.insert(*t, ElementKind::Message, Attrs{{std::string(attr::ROLE), Role::User},
            {std::string(attr::STATUS), MessageStatus::Done}, {std::string(attr::BY), prompt.by}}, prompt.text));
    } else if (!result.id || !open_.prompt || *open_.prompt == *result.id) {
        bool ran = open_.prompt.has_value();
        ADAPT(finish(w, status));
        if (ran && result.agent_invoked && status == TurnStatus::Done && rename_ == Rename::Idle) rename_ = Rename::Due;
    }
    if (result.error) {
        const auto& e = *result.error;
        ADAPT(ext(w, "prompt_error", obj({{"message", e.message}, {"provider", nullable(e.provider)},
            {"model", nullable(e.model)}, {"code", nullable(e.code)}, {"retryable", e.retryable}})));
    }
    return {};
}
Result<std::vector<Reply>> Mapper::apply(Input input, const Writer& w) {
    std::vector<Reply> replies;
    if (auto* sent = std::get_if<Sent>(&input)) queued_.emplace_back(std::move(sent->id), std::move(sent->prompt));
    else if (auto* answered_input = std::get_if<Answered>(&input)) {
        auto it = open_.decisions.find(answered_input->request);
        if (it == open_.decisions.end()) return std::unexpected(AdapterError::Protocol{"no dialog `" + answered_input->request + "`"});
        auto r = answer(it->second, answered_input->answer, w); if (!r) return std::unexpected(r.error());
        if (*r) replies.push_back(std::move(**r));
    } else if (auto* frame = std::get_if<InputFrame>(&input)) {
        if (auto* ev = std::get_if<rpc::Event>(&frame->frame)) ADAPT(event(w, *ev));
        else if (auto* request = std::get_if<rpc::UiRequest>(&frame->frame)) {
            if (auto response = auto_reply(*request)) replies.push_back(Reply{std::move(*response), std::nullopt});
            else ADAPT(ui(w, *request));
        }
    }
    return replies;
}
#undef COMMIT
#undef ADAPT
Input input_from_json(rpc::Json raw) {
    auto text = [&](std::string_view key) -> std::optional<std::string> {
        const auto* value = raw.find(key);
        const auto* str = value ? value->as_string() : nullptr;
        return str ? std::optional<std::string>(*str) : std::nullopt;
    };
    const auto tag = text("$");
    if (tag == "send")
        return Sent{text("id").value_or(""), loa::contracts::Prompt{text("text").value_or(""), {}, "owner"}};
    if (tag == "answer") {
        std::vector<std::string> values;
        if (const auto* list = raw.find("values"); list && list->as_array())
            for (const auto& value : *list->as_array())
                if (const auto* str = value.as_string()) values.push_back(*str);
        return Answered{text("request").value_or(""),
                        loa::contracts::Answer{std::move(values), text("reason"), text("by").value_or("owner")}};
    }
    return InputFrame{rpc::frame_from_json(std::move(raw))};
}

} // namespace loa::harness::omp
