#include "loa/harness/codex/mapper.hpp"

#include <algorithm>
#include <utility>

#include "loa/harness/codex/adapter.hpp"
#include "loa/harness/kit/jsonrpc.hpp"

namespace loa::harness::codex {
namespace {
using namespace loa::core;
using loa::contracts::AdapterError;
using loa::contracts::Result;
namespace json = loa::runtime::json;

#define COMMIT(expr) do { auto r_ = (expr); if (!r_) return std::unexpected(AdapterError::Commit{r_.error()}); } while (false)
#define FORWARD(expr) do { auto r_ = (expr); if (!r_) return std::unexpected(r_.error()); } while (false)

const Json* at(const Json& v, std::string_view name) { return v.find(name); }
const Json* ptr(const Json& v, std::string_view first, std::string_view second) {
    const Json* child = at(v, first);
    return child ? at(*child, second) : nullptr;
}
std::optional<std::string_view> str(const Json& v, std::string_view key) {
    const Json* found = at(v, key);
    if (!found) return std::nullopt;
    auto* s = found->as_string();
    return s ? std::optional<std::string_view>(*s) : std::nullopt;
}
std::string_view text(const Json& v, std::string_view key) { return str(v, key).value_or(""); }
std::optional<std::int64_t> integer(const Json& v, std::string_view key) {
    const Json* found = at(v, key);
    return found ? found->as_i64() : std::nullopt;
}
const json::Array& array(const Json* v) {
    static const json::Array empty;
    return v && v->as_array() ? *v->as_array() : empty;
}
void sort_json(Json& value) {
    if (auto* object = value.as_object()) {
        for (auto& [key, child] : *object) sort_json(child);
        std::sort(object->begin(), object->end(), [](const auto& a, const auto& b) { return a.first < b.first; });
    } else if (auto* elements = value.as_array()) {
        for (auto& child : *elements) sort_json(child);
    }
}
std::string canonical(Json value) { sort_json(value); return json::dump(value); }
std::string dump(const Json* v) { return v ? canonical(*v) : std::string{}; }
std::string first_line(std::string_view s) { return std::string(s.substr(0, s.find('\n'))); }
std::string change_line(const Json& change) {
    const Json* kind = at(change, "kind");
    return std::string(kind ? str(*kind, "type").value_or("update") : "update") + " " + std::string(text(change, "path"));
}
std::string content_text(const Json* value) {
    if (!value || !value->as_array()) return {};
    std::string out;
    for (const auto& part : *value->as_array()) {
        auto line = str(part, "text");
        if (!line) return canonical(*value);
        if (!out.empty()) out += '\n';
        out += *line;
    }
    return out;
}
std::string reasoning_text(const Json& item) {
    std::string out;
    for (auto key : {"summary", "content"}) {
        for (const auto& part : array(at(item, key))) {
            auto* s = part.as_string();
            if (!s) continue;
            if (!out.empty()) out += "\n\n";
            out += *s;
        }
    }
    return out;
}
Value::List options(bool session) {
    Value::List out{Value(answer::ALLOW_ONCE)};
    if (session) out.emplace_back(answer::ALLOW_SESSION);
    out.emplace_back(answer::DENY);
    return out;
}
Reply result(Json id, Json value) { return {Reply::Kind::Result, std::move(id), std::move(value)}; }
} // namespace

Mapper::Mapper(NodeId session, std::optional<std::string> model) : ledger_(session), model_(std::move(model)) {}

Result<Mapper> Mapper::start(const loa::contracts::SessionConfig& config, std::string_view thread_id,
                             std::optional<std::string_view> model, const DomWriter& w) {
    std::optional<std::string> chosen = model ? std::optional<std::string>(*model) : config.model;
    Attrs attrs{{std::string(attr::ID), config.session_id}, {std::string(attr::HARNESS), HARNESS},
                {std::string(attr::NODE), config.node}, {std::string(attr::CREATED), w.now()},
                {std::string(attr::SCHEMA), SCHEMA_VERSION}, {std::string(attr::CWD), config.cwd.string()},
                {std::string(attr::NATIVE_ID), thread_id}};
    if (config.project) attrs.emplace(attr::PROJECT, *config.project);
    if (chosen) attrs.emplace(attr::MODEL, *chosen);
    if (config.model_role) attrs.emplace(attr::MODEL_ROLE, *config.model_role);
    if (config.effort) attrs.emplace(attr::EFFORT, *config.effort);
    if (config.mode) attrs.emplace(attr::MODE, *config.mode);
    auto root = w.insert(NodeId::DOCUMENT, ElementKind::Session, std::move(attrs));
    COMMIT(root);
    return Mapper(*root, std::move(chosen));
}

Result<Mapper> Mapper::attach(const Dom& dom, const DomWriter& w) {
    const Node* root = dom.root();
    if (!root || root->kind != ElementKind::Session)
        return std::unexpected(AdapterError::Format{"the DOM has no <session> root"});
    auto model = root->str_attr(attr::MODEL);
    Mapper mapper(root->id, model ? std::optional<std::string>(*model) : std::nullopt);
    for (const Node* child : dom.children(root->id)) {
        if (child->kind == ElementKind::Turn) mapper.turns_.push_back(child->id);
        if (child->kind == ElementKind::Todo) mapper.todos_.push_back(child->id);
        if (child->kind == ElementKind::Compaction) {
            const Value* covers = child->attr(attr::COVERS);
            auto list = covers ? covers->as_list() : std::nullopt;
            if (list && !list->empty()) {
                auto last = list->back().as_int();
                auto found = std::find_if(mapper.turns_.begin(), mapper.turns_.end(),
                                          [&](NodeId turn) { return last == static_cast<std::int64_t>(turn.value); });
                mapper.covered_ = found == mapper.turns_.end() ? 0 : static_cast<std::size_t>(found - mapper.turns_.begin() + 1);
            }
        }
    }
    for (const Node& node : dom.descendants(root->id)) {
        auto status = node.str_attr(attr::STATUS).value_or("");
        if (node.kind == ElementKind::Message && status == "streaming") COMMIT(w.set(node.id, attr::STATUS, MessageStatus::Cancelled));
        if (node.kind == ElementKind::Turn && (status == "running" || status == "waiting")) COMMIT(w.close(node.id, TurnStatus::Cancelled));
        if (node.kind == ElementKind::Call && (status == "running" || status == "pending")) COMMIT(w.close(node.id, CallStatus::Cancelled));
        if (node.kind == ElementKind::Decision && status == "waiting") COMMIT(w.close(node.id, DecisionStatus::Cancelled));
        if (node.kind == ElementKind::Job && status == "running") COMMIT(w.close(node.id, JobStatus::Cancelled));
    }
    return mapper;
}

std::optional<std::string_view> Mapper::turn_id() const { return turn_id_ ? std::optional<std::string_view>(*turn_id_) : std::nullopt; }
std::optional<std::string_view> Mapper::model() const { return model_ ? std::optional<std::string_view>(*model_) : std::nullopt; }

Result<> Mapper::opened(NodeId node, std::optional<std::string_view> id, bool prompted, const DomWriter& w) {
    COMMIT(w.set(node, attr::STARTED, w.now()));
    turns_.push_back(node);
    turn_id_.reset();
    prompted_ = prompted;
    usage_.reset();
    items_.clear();
    if (id) FORWARD(turn_accepted(*id, w));
    return {};
}
Result<> Mapper::begin_turn(const loa::contracts::Prompt& prompt, const DomWriter& w) {
    if (ledger_.turn()) return std::unexpected(AdapterError::Busy{});
    auto node = ledger_.begin(w, prompt);
    COMMIT(node);
    return opened(*node, std::nullopt, true, w);
}
Result<> Mapper::turn_accepted(std::string_view id, const DomWriter& w) {
    if (const auto* turn = ledger_.turn(); turn && !turn_id_) {
        COMMIT(w.set(turn->node, attr::ID, id));
        turn_id_ = id;
    }
    return {};
}
Result<> Mapper::turn_rejected(std::string_view message, const DomWriter& w) {
    if (const auto* turn = ledger_.turn()) COMMIT(w.ext(turn->node, HARNESS, "error", std::string(message)));
    return end_turn(TurnStatus::Error, w);
}
Result<> Mapper::process_exited(std::string_view detail, const DomWriter& w) {
    if (const auto* turn = ledger_.turn()) {
        COMMIT(w.ext(turn->node, HARNESS, "exit", std::string(detail)));
        FORWARD(end_turn(TurnStatus::Error, w));
    }
    ledger_.close();
    return {};
}
Result<> Mapper::set_model(std::string_view model, const DomWriter& w) {
    COMMIT(w.set(ledger_.session, attr::MODEL, model));
    model_ = model;
    return {};
}
Result<> Mapper::set_effort(std::string_view effort, const DomWriter& w) {
    COMMIT(w.set(ledger_.session, attr::EFFORT, effort));
    return {};
}
Result<> Mapper::set_mode(SessionMode mode, const DomWriter& w) {
    COMMIT(w.set(ledger_.session, attr::MODE, mode));
    return {};
}

Result<NodeId> Mapper::ensure_turn(std::optional<std::string_view> id, const DomWriter& w) {
    bool open = !ledger_.turn() || (id && turn_id_ && *id != *turn_id_);
    if (open) {
        FORWARD(end_turn(TurnStatus::Done, w));
        auto node = ledger_.begin_with(w, nullptr);
        COMMIT(node);
        FORWARD(opened(*node, id, false, w));
    } else if (id) {
        FORWARD(turn_accepted(*id, w));
    }
    if (const auto* turn = ledger_.turn()) return turn->node;
    return std::unexpected(AdapterError::Protocol{"no turn"});
}

void Mapper::track(std::string_view id, NodeId node, Kind kind) {
    if (kind != Kind::User) ledger_.open(std::string(id), node);
    items_.insert_or_assign(std::string(id), Item{node, kind});
}
Result<NodeId> Mapper::message_item(std::optional<std::string_view> turn, std::string_view id, Kind kind,
                                    std::string_view type, const DomWriter& w) {
    if (auto found = items_.find(std::string(id)); found != items_.end()) return found->second.node;
    Attrs attrs{{std::string(attr::ROLE), Role::Assistant}, {std::string(attr::STATUS), MessageStatus::Streaming},
                {std::string(attr::ID), id}};
    if (kind == Kind::Thinking) attrs.emplace(attr::KIND, MessageKind::Thinking);
    if (model_) attrs.emplace(attr::MODEL, *model_);
    if (type == "plan") attrs.emplace("x-codex-item", "plan");
    auto parent = ensure_turn(turn, w);
    if (!parent) return std::unexpected(parent.error());
    auto node = w.insert(*parent, ElementKind::Message, std::move(attrs));
    COMMIT(node);
    track(id, *node, kind);
    return *node;
}
Result<> Mapper::text_delta(const Json& params, Kind kind, std::string_view type, const DomWriter& w) {
    auto id = str(params, "itemId"), delta = str(params, "delta");
    if (!id || !delta || delta->empty()) return {};
    auto node = message_item(str(params, "turnId"), *id, kind, type, w);
    if (!node) return std::unexpected(node.error());
    COMMIT(w.append(*node, std::string(*delta)));
    items_.at(std::string(*id)).streamed += delta->size();
    return {};
}
Result<> Mapper::output_delta(const Json& params, const DomWriter& w) {
    auto id = str(params, "itemId"), delta = str(params, "delta");
    if (!id || !delta) return {};
    auto found = items_.find(std::string(*id));
    if (found == items_.end()) return {};
    auto& item = found->second;
    if (item.result) COMMIT(w.append(*item.result, std::string(*delta)));
    else {
        auto result = w.insert(item.node, ElementKind::Result, {}, std::string(*delta));
        COMMIT(result);
        item.result = *result;
    }
    item.result_streamed += delta->size();
    return {};
}

Result<bool> Mapper::start_call(std::optional<std::string_view> turn, const Json& item, const DomWriter& w) {
    auto type = text(item, "type");
    std::string tool, title;
    Json input;
    if (type == "commandExecution") {
        tool = "exec_command";
        title = text(item, "command");
        input = json::Object{{"command", title}, {"cwd", at(item, "cwd") ? *at(item, "cwd") : Json{}}};
    } else if (type == "fileChange") {
        tool = "apply_patch";
        title = "Edit ";
        for (const auto& change : array(at(item, "changes"))) {
            if (title != "Edit ") title += ", ";
            title += text(change, "path");
        }
        input = json::Object{{"changes", at(item, "changes") ? *at(item, "changes") : Json{}}};
    } else if (type == "mcpToolCall") {
        tool = "mcp__" + std::string(text(item, "server")) + "__" + std::string(text(item, "tool"));
        title = std::string(text(item, "server")) + ": " + std::string(text(item, "tool"));
        input = at(item, "arguments") ? *at(item, "arguments") : Json{};
    } else if (type == "dynamicToolCall") {
        tool = std::string(str(item, "namespace").value_or(""));
        if (!tool.empty()) tool += '.';
        tool += text(item, "tool");
        input = at(item, "arguments") ? *at(item, "arguments") : Json{};
    } else if (type == "collabAgentToolCall") {
        for (char c : text(item, "tool")) {
            if (c >= 'A' && c <= 'Z') { tool += '_'; c = static_cast<char>(c + ('a' - 'A')); }
            tool += c;
        }
        title = first_line(text(item, "prompt"));
        input = json::Object{{"prompt", at(item, "prompt") ? *at(item, "prompt") : Json{}},
                             {"model", at(item, "model") ? *at(item, "model") : Json{}},
                             {"receiverThreadIds", at(item, "receiverThreadIds") ? *at(item, "receiverThreadIds") : Json{}}};
    } else if (type == "webSearch" || type == "imageView") {
        tool = type == "webSearch" ? "web_search" : "view_image";
        auto key = type == "webSearch" ? "query" : "path";
        title = text(item, key);
        input = json::Object{{key, title}};
    } else return false;
    auto id = str(item, "id");
    if (!id || items_.contains(std::string(*id))) return true;
    Attrs attrs{{std::string(attr::TOOL), tool}, {std::string(attr::CALL_ID), *id},
                {std::string(attr::STATUS), CallStatus::Running},
                {std::string(attr::STARTED), integer(item, "startedAtMs").value_or(w.now())}};
    if (!title.empty() || type == "commandExecution" || type == "webSearch" || type == "imageView")
        attrs.emplace(attr::TITLE, title);
    auto parent = ensure_turn(turn, w);
    if (!parent) return std::unexpected(parent.error());
    auto node = w.insert(*parent, ElementKind::Call, std::move(attrs));
    COMMIT(node);
    COMMIT(w.insert(*node, ElementKind::Input, {}, canonical(input)));
    track(*id, *node, Kind::Call);
    if (type == "fileChange") {
        auto& diff = items_.at(std::string(*id)).diff;
        for (const auto& change : array(at(item, "changes"))) {
            diff += change_line(change) + '\n';
            if (auto patch = str(change, "diff")) {
                diff += *patch;
                if (!patch->ends_with('\n')) diff += '\n';
            }
        }
    }
    return true;
}

Result<> Mapper::item(const Json& params, bool started, const DomWriter& w) {
    auto turn = str(params, "turnId");
    const Json* data = at(params, "item");
    if (!data) return {};
    auto id = str(*data, "id");
    if (!id) return {};
    auto type = text(*data, "type");
    if (type == "userMessage") {
        auto parent = ensure_turn(turn, w);
        if (!parent) return std::unexpected(parent.error());
        if (items_.contains(std::string(*id))) return {};
        if (prompted_) { track(*id, *parent, Kind::User); return {}; }
        prompted_ = true;
        std::string body;
        for (const auto& part : array(at(*data, "content"))) {
            if (auto line = str(part, "text")) {
                if (!body.empty()) body += '\n';
                body += *line;
            }
        }
        auto node = w.insert(*parent, ElementKind::Message,
                             {{std::string(attr::ROLE), Role::User}, {std::string(attr::STATUS), MessageStatus::Done},
                              {std::string(attr::ID), *id}}, body);
        COMMIT(node);
        track(*id, *node, Kind::User);
        return {};
    }
    if (type == "contextCompaction" && !started) {
        Value::List covers;
        for (std::size_t i = covered_; i < turns_.size(); ++i) covers.emplace_back(turns_[i]);
        covered_ = turns_.size();
        COMMIT(w.insert(ledger_.session, ElementKind::Compaction,
                        {{std::string(attr::COVERS), std::move(covers)}, {std::string(attr::CREATED), w.now()}}));
        return {};
    }
    if (type != "agentMessage" && type != "plan" && type != "reasoning") {
        if (started) { auto call = start_call(turn, *data, w); if (!call) return std::unexpected(call.error()); return {}; }
        if (!items_.contains(std::string(*id))) {
            auto call = start_call(turn, *data, w);
            if (!call) return std::unexpected(call.error());
            if (!*call) {
                auto parent = ensure_turn(turn, w);
                if (!parent) return std::unexpected(parent.error());
                COMMIT(w.ext(*parent, HARNESS, "item:" + std::string(type), canonical(*data)));
                return {};
            }
        }
        return finish_call(*id, *data, w);
    }
    Kind kind = type == "reasoning" ? Kind::Thinking : Kind::Message;
    std::string body = kind == Kind::Thinking ? reasoning_text(*data) : std::string(text(*data, "text"));
    auto node = message_item(turn, *id, kind, type, w);
    if (!node) return std::unexpected(node.error());
    auto& entry = items_.at(std::string(*id));
    if (started) {
        if (!body.empty() && !entry.streamed) {
            COMMIT(w.append(*node, body));
            entry.streamed = body.size();
        }
        return {};
    }
    if (entry.streamed != body.size()) COMMIT(w.set_text(*node, body));
    COMMIT(w.set(*node, attr::STATUS, MessageStatus::Done));
    items_.erase(std::string(*id));
    ledger_.take(std::string(*id));
    return {};
}

Result<> Mapper::finish_call(std::string_view id, const Json& item, const DomWriter& w) {
    auto found = items_.find(std::string(id));
    if (found == items_.end()) return {};
    Item entry = std::move(found->second);
    items_.erase(found);
    ledger_.take(std::string(id));
    auto type = text(item, "type");
    std::optional<std::int64_t> exit_code = type == "commandExecution" ? integer(item, "exitCode") : std::nullopt;
    std::optional<std::string> output;
    CallStatus status = CallStatus::Done;
    if (type == "commandExecution") {
        if (auto v = str(item, "aggregatedOutput")) output = std::string(*v);
    } else if (type == "fileChange") {
        output = "";
        for (const auto& change : array(at(item, "changes"))) {
            if (!output->empty()) *output += '\n';
            *output += change_line(change);
        }
    } else if (type == "mcpToolCall") {
        if (const Json* error = at(item, "error"); error && str(*error, "message")) {
            output = std::string(*str(*error, "message"));
            status = CallStatus::Error;
        } else output = content_text(ptr(item, "result", "content"));
    } else if (type == "dynamicToolCall") {
        output = content_text(at(item, "contentItems"));
        if (const Json* success = at(item, "success"); success && success->as_bool() && !*success->as_bool())
            status = CallStatus::Error;
    } else if (type == "collabAgentToolCall") {
        FORWARD(agent_jobs(entry.node, item, w));
        output = dump(at(item, "agentsStates"));
    }
    auto state = text(item, "status");
    if (state == "failed" || (exit_code && *exit_code != 0)) status = CallStatus::Error;
    else if (state == "declined" || state == "interrupted") status = CallStatus::Cancelled;
    NodeId result_node;
    if (entry.result) {
        result_node = *entry.result;
        if (output && output->size() != entry.result_streamed) COMMIT(w.set_text(result_node, *output));
        COMMIT(w.set(result_node, attr::IS_ERROR, status == CallStatus::Error));
    } else {
        auto node = w.insert(entry.node, ElementKind::Result,
                             {{std::string(attr::IS_ERROR), status == CallStatus::Error}}, output.value_or(""));
        COMMIT(node);
        result_node = *node;
    }
    if (exit_code) COMMIT(w.set(result_node, "x-exit-code", *exit_code));
    if (auto duration = integer(item, "durationMs"))
        COMMIT(w.insert(entry.node, ElementKind::Usage, {{std::string(attr::DURATION_MS), *duration}}));
    COMMIT(w.set(entry.node, attr::STATUS, status));
    COMMIT(w.set(entry.node, attr::ENDED, integer(item, "completedAtMs").value_or(w.now())));
    return {};
}

Result<> Mapper::agent_jobs(NodeId call, const Json& item, const DomWriter& w) {
    if (!ledger_.turn()) return {};
    bool spawn = text(item, "tool") == "spawnAgent";
    for (const auto& value : array(at(item, "receiverThreadIds"))) {
        auto* thread = value.as_string();
        if (!thread) continue;
        const Json* states = at(item, "agentsStates");
        const Json* agent = states ? states->find(*thread) : nullptr;
        auto state = agent ? str(*agent, "status") : std::nullopt;
        JobStatus status = JobStatus::Running;
        if (state == "completed" || state == "shutdown") status = JobStatus::Done;
        if (state == "errored" || state == "notFound") status = JobStatus::Error;
        if (state == "interrupted") status = JobStatus::Cancelled;
        auto found = jobs_.find(*thread);
        if (found != jobs_.end() && state) {
            if (status == JobStatus::Running) COMMIT(w.set(found->second, attr::STATUS, status));
            else COMMIT(w.close(found->second, status));
        } else if (found == jobs_.end() && spawn) {
            Attrs attrs{{std::string(attr::KIND), JobKind::Subagent}, {std::string(attr::STATUS), status},
                        {std::string(attr::ID), *thread}, {std::string(attr::AGENT), *thread},
                        {std::string(attr::CALL), call}, {std::string(attr::STARTED), w.now()}};
            if (auto prompt = str(item, "prompt")) attrs.emplace(attr::TITLE, first_line(*prompt));
            auto node = w.insert(ledger_.turn()->node, ElementKind::Job, std::move(attrs));
            COMMIT(node);
            jobs_.emplace(*thread, *node);
        }
    }
    return {};
}

Result<> Mapper::plan_updated(const Json& params, const DomWriter& w) {
    const auto& steps = array(at(params, "plan"));
    for (std::size_t i = 0; i < steps.size(); ++i) {
        auto status = text(steps[i], "status");
        auto state = status == "completed" ? TodoState::Done :
                     status == "inProgress" ? TodoState::InProgress : TodoState::Pending;
        if (i < todos_.size()) {
            COMMIT(w.set(todos_[i], attr::STATE, state));
            COMMIT(w.set_text(todos_[i], std::string(text(steps[i], "step"))));
        } else {
            auto node = w.insert(ledger_.session, ElementKind::Todo,
                                 {{std::string(attr::STATE), state}}, std::string(text(steps[i], "step")));
            COMMIT(node);
            todos_.push_back(*node);
        }
    }
    for (std::size_t i = steps.size(); i < todos_.size(); ++i)
        COMMIT(w.set(todos_[i], attr::STATE, TodoState::Cancelled));
    todos_.resize(std::min(todos_.size(), steps.size()));
    return {};
}

Result<> Mapper::turn_completed(const Json& turn, const DomWriter& w) {
    auto state = text(turn, "status");
    TurnStatus status;
    if (state == "completed") status = TurnStatus::Done;
    else if (state == "interrupted") status = TurnStatus::Cancelled;
    else if (state == "failed") status = TurnStatus::Error;
    else return {};
    if (auto id = str(turn, "id"); id && turn_id_ && ledger_.turn() && *id != *turn_id_) return {};
    if (const auto* open = ledger_.turn()) {
        if (const Json* error = at(turn, "error"); error && str(*error, "message"))
            COMMIT(w.ext(open->node, HARNESS, "error", std::string(*str(*error, "message"))));
    }
    return end_turn(status, w);
}

Result<> Mapper::end_turn(TurnStatus status, const DomWriter& w) {
    if (!ledger_.turn()) return {};
    if (usage_) {
        auto tokens = *usage_;
        COMMIT(w.insert(ledger_.turn()->node, ElementKind::Usage,
                        {{std::string(attr::INPUT_TOKENS), tokens[0]},
                         {std::string(attr::OUTPUT_TOKENS), tokens[1]},
                         {std::string(attr::CACHE_READ_TOKENS), tokens[2]},
                         {std::string(attr::CACHE_WRITE_TOKENS), tokens[3]}}));
        usage_.reset();
    }
    std::vector<std::pair<std::string, NodeId>> children;
    std::vector<NodeId> decisions;
    auto closed = ledger_.end_turn(w, status, [&](const kit::EndEvent<std::string>& event) {
        if (const auto* child = std::get_if<kit::end_event::Child<std::string>>(&event))
            children.emplace_back(*child->key, child->node);
        if (const auto* decision = std::get_if<kit::end_event::Decision>(&event))
            decisions.push_back(decision->node);
    });
    COMMIT(closed);
    for (const auto& [key, node] : children) {
        if (auto it = items_.find(key); it != items_.end() && it->second.kind == Kind::Call)
            COMMIT(w.set(node, attr::ENDED, w.now()));
        else if (status == TurnStatus::Done)
            COMMIT(w.set(node, attr::STATUS, MessageStatus::Done));
    }
    for (NodeId decision : decisions) forget(decision);
    turn_id_.reset();
    prompted_ = false;
    items_.clear();
    return {};
}

void Mapper::forget(NodeId decision) {
    auto it = decisions_.find(decision);
    if (it == decisions_.end()) return;
    if (it->second.kind == PendingKind::Question) groups_.erase(it->second.group);
    decisions_.erase(it);
}
Result<> Mapper::unwait(NodeId decision, const DomWriter& w) {
    const auto* before = ledger_.turn();
    if (!before) return {};
    bool was_waiting = !before->waiting.empty();
    ledger_.answered(decision);
    if (was_waiting && ledger_.turn()->waiting.empty())
        COMMIT(w.set(ledger_.turn()->node, attr::STATUS, TurnStatus::Running));
    return {};
}
Result<> Mapper::request_cleared(const Json& id, const DomWriter& w) {
    std::vector<NodeId> cleared;
    for (const auto& [node, pending] : decisions_)
        if (pending.rpc_id == id) cleared.push_back(node);
    for (NodeId node : cleared) {
        forget(node);
        COMMIT(w.close(node, DecisionStatus::Cancelled));
        FORWARD(unwait(node, w));
    }
    return {};
}

Result<std::pair<NodeId, std::optional<NodeId>>> Mapper::decision_target(const Json& params, const DomWriter& w) {
    if (auto id = str(params, "itemId")) {
        auto it = items_.find(std::string(*id));
        if (it != items_.end()) return std::pair{it->second.node, std::optional<NodeId>(it->second.node)};
    }
    if (ledger_.turn()) return std::pair{ledger_.turn()->node, std::optional<NodeId>{}};
    if (auto id = str(params, "turnId")) {
        auto turn = ensure_turn(id, w);
        if (!turn) return std::unexpected(turn.error());
        return std::pair{*turn, std::optional<NodeId>{}};
    }
    return std::pair{ledger_.session, std::optional<NodeId>{}};
}
Result<> Mapper::decide(const Json& id, const Json& params, DecisionKind kind,
                        Attrs attrs, std::string body, Pending pending, const DomWriter& w) {
    auto target = decision_target(params, w);
    if (!target) return std::unexpected(target.error());
    attrs.insert_or_assign(std::string(attr::KIND), kind);
    attrs.insert_or_assign(std::string(attr::STATUS), DecisionStatus::Waiting);
    attrs.insert_or_assign(std::string(attr::REQUEST_ID), kit::id_key(id));
    attrs.insert_or_assign(std::string(attr::CREATED), w.now());
    if (target->second) attrs.emplace(attr::CALL, *target->second);
    auto node = w.insert(target->first, ElementKind::Decision, std::move(attrs), std::move(body));
    COMMIT(node);
    decisions_.emplace(*node, std::move(pending));
    if (const auto* turn = ledger_.turn()) {
        if (turn->waiting.empty()) COMMIT(w.set(turn->node, attr::STATUS, TurnStatus::Waiting));
        ledger_.wait(*node);
    }
    return {};
}
Result<std::optional<Reply>> Mapper::approval(Json id, const Json& params, std::string title,
                                               std::string body, Pending pending, const DomWriter& w) {
    bool session = pending.kind != PendingKind::Command || pending.session_option;
    if (auto reason = str(params, "reason")) {
        if (!body.empty()) body += "\n\n";
        body += *reason;
    }
    FORWARD(decide(id, params, DecisionKind::Approval,
                   {{std::string(attr::TITLE), std::move(title)},
                    {std::string(attr::OPTIONS), options(session)}}, std::move(body), std::move(pending), w));
    return std::nullopt;
}

Result<std::optional<Reply>> Mapper::handle(std::optional<Json> id, std::string_view method,
                                            const Json& params, const DomWriter& w) {
    if (id) {
        if (method == "item/tool/call") {
            Reply reply{Reply::Kind::HostTool, std::move(*id)};
            reply.tool = text(params, "tool");
            reply.arguments = at(params, "arguments") ? canonical(*at(params, "arguments")) : "{}";
            return reply;
        }
        Pending pending{*id, PendingKind::Command};
        if (method == "item/commandExecution/requestApproval") {
            pending.session_option = text(params, "kind") != "humanCommand";
            const Json* network = at(params, "networkApprovalContext");
            if (network && !network->is_null()) {
                std::string host(text(*network, "host"));
                return approval(std::move(*id), params, "Allow network access to " + host, host, std::move(pending), w);
            }
            return approval(std::move(*id), params, "Run command", std::string(text(params, "command")), std::move(pending), w);
        }
        if (method == "item/fileChange/requestApproval") {
            pending.kind = PendingKind::FileChange;
            std::string diff;
            if (auto item_id = str(params, "itemId")) {
                auto it = items_.find(std::string(*item_id));
                if (it != items_.end()) diff = it->second.diff;
            }
            return approval(std::move(*id), params, "Apply patch", std::move(diff), std::move(pending), w);
        }
        if (method == "item/permissions/requestApproval") {
            pending.kind = PendingKind::Permissions;
            pending.permissions = at(params, "permissions") ? *at(params, "permissions") : Json{};
            auto body = canonical(pending.permissions);
            return approval(std::move(*id), params, "Grant permissions", std::move(body), std::move(pending), w);
        }
        if (method == "item/tool/requestUserInput") {
            const auto& questions = array(at(params, "questions"));
            if (questions.empty()) return result(std::move(*id), json::Object{{"answers", json::Object{}}});
            auto group = kit::id_key(*id);
            groups_.insert_or_assign(group, InputGroup{*id, questions.size(), {}});
            for (const auto& question : questions) {
                Value::List labels;
                for (const auto& option : array(at(question, "options")))
                    if (auto label = str(option, "label")) labels.emplace_back(*label);
                Attrs attrs{{std::string(attr::OPTIONS), std::move(labels)}};
                if (auto header = str(question, "header")) attrs.emplace(attr::TITLE, *header);
                Pending ask{*id, PendingKind::Question};
                ask.group = group;
                ask.question = text(question, "id");
                FORWARD(decide(*id, params, DecisionKind::Question, std::move(attrs),
                               std::string(text(question, "question")), std::move(ask), w));
            }
            return std::nullopt;
        }
        if (method == "mcpServer/elicitation/request") {
            std::string server(text(params, "serverName")), message(text(params, "message"));
            auto mode = text(params, "mode");
            if (mode == "form" || mode == "openai/form") {
                pending.kind = PendingKind::Form;
                FORWARD(decide(*id, params, DecisionKind::Form,
                               {{std::string(attr::TITLE), server},
                                {std::string(attr::FIELDS), dump(at(params, "requestedSchema"))}},
                               std::move(message), std::move(pending), w));
            } else {
                pending.kind = PendingKind::Url;
                FORWARD(decide(*id, params, DecisionKind::Approval,
                               {{std::string(attr::TITLE), server}, {std::string(attr::OPTIONS), options(false)}},
                               message + "\n\n" + std::string(text(params, "url")), std::move(pending), w));
            }
            return std::nullopt;
        }
        Reply reply{Reply::Kind::Error, std::move(*id)};
        reply.code = -32601;
        reply.message = std::string(method) + " isn't supported";
        return reply;
    }
    if (state() == loa::contracts::SessionState::Closed) return std::nullopt;
    if (method == "turn/started") {
        const Json* turn = at(params, "turn");
        auto node = ensure_turn(turn ? str(*turn, "id") : std::nullopt, w);
        if (!node) return std::unexpected(node.error());
    } else if (method == "turn/completed") {
        const Json* turn = at(params, "turn");
        FORWARD(turn_completed(turn ? *turn : Json{}, w));
    } else if (method == "item/started" || method == "item/completed") {
        FORWARD(item(params, method == "item/started", w));
    } else if (method == "item/agentMessage/delta" || method == "item/plan/delta" ||
               method == "item/reasoning/summaryTextDelta" || method == "item/reasoning/textDelta") {
        bool thinking = method.starts_with("item/reasoning/");
        FORWARD(text_delta(params, thinking ? Kind::Thinking : Kind::Message,
                           thinking ? "reasoning" : method == "item/plan/delta" ? "plan" : "agentMessage", w));
    } else if (method == "item/reasoning/summaryPartAdded") {
        if (auto id = str(params, "itemId")) {
            auto it = items_.find(std::string(*id));
            if (it != items_.end() && it->second.streamed) {
                COMMIT(w.append(it->second.node, "\n\n"));
                it->second.streamed += 2;
            }
        }
    } else if (method == "item/commandExecution/outputDelta" || method == "item/fileChange/outputDelta") {
        FORWARD(output_delta(params, w));
    } else if (method == "turn/plan/updated") {
        FORWARD(plan_updated(params, w));
    } else if (method == "thread/tokenUsage/updated") {
        const Json* last = ptr(params, "tokenUsage", "last");
        if (last && ledger_.turn()) {
            if (!usage_) usage_ = std::array<std::int64_t, 4>{};
            for (std::size_t i = 0; auto key : {"inputTokens", "outputTokens", "cachedInputTokens", "cacheWriteInputTokens"})
                (*usage_)[i++] += integer(*last, key).value_or(0);
        }
    } else if (method == "model/rerouted") {
        if (auto next = str(params, "toModel")) FORWARD(set_model(*next, w));
    } else if (method == "serverRequest/resolved") {
        if (const Json* request = at(params, "requestId")) FORWARD(request_cleared(*request, w));
    } else if (method == "error") {
        const Json* retry = at(params, "willRetry");
        if ((!retry || !retry->as_bool() || !*retry->as_bool()) && ledger_.turn()) {
            const Json* error = at(params, "error");
            COMMIT(w.ext(ledger_.turn()->node, HARNESS, "error", error ? std::string(text(*error, "message")) : ""));
        }
    } else if (ledger_.turn() && turn_id_ && str(params, "turnId") == std::optional<std::string_view>(*turn_id_)) {
        COMMIT(w.ext(ledger_.turn()->node, HARNESS, method, canonical(params)));
    }
    return std::nullopt;
}

Result<std::optional<Reply>> Mapper::answer(NodeId decision, const loa::contracts::Answer& response, const DomWriter& w) {
    auto it = decisions_.find(decision);
    if (it == decisions_.end()) return std::unexpected(AdapterError::UnknownDecision{decision});
    const Pending& pending = it->second;
    std::string_view first = response.values.empty() ? "" : std::string_view(response.values.front());
    auto unknown = [&]() -> AdapterError {
        return AdapterError::Protocol{"`" + std::string(first) + "` isn't an answer to this decision"};
    };
    auto approval = [&](bool session) -> Result<std::string_view> {
        if (first == loa::core::answer::ALLOW_ONCE) return "accept";
        if (first == loa::core::answer::ALLOW_SESSION && session) return "acceptForSession";
        if (first == loa::core::answer::DENY) return "decline";
        return std::unexpected(unknown());
    };
    Json value;
    switch (pending.kind) {
    case PendingKind::Command:
    case PendingKind::FileChange: {
        auto approved = approval(pending.kind == PendingKind::FileChange || pending.session_option);
        if (!approved) return std::unexpected(approved.error());
        value = json::Object{{"decision", *approved}};
        break;
    }
    case PendingKind::Permissions: {
        auto approved = approval(true);
        if (!approved) return std::unexpected(approved.error());
        value = *approved == "decline" ? Json(json::Object{{"permissions", json::Object{}}}) :
                Json(json::Object{{"permissions", pending.permissions},
                                  {"scope", *approved == "accept" ? "turn" : "session"}});
        break;
    }
    case PendingKind::Form: {
        if (first == loa::core::answer::DENY || first == loa::core::answer::REJECT)
            value = json::Object{{"action", "decline"}, {"content", Json{}}};
        else {
            auto content = json::parse(first);
            if (!content || !content->is_object())
                return std::unexpected(AdapterError::Protocol{"a form answer must be one JSON object"});
            value = json::Object{{"action", "accept"}, {"content", std::move(*content)}};
        }
        break;
    }
    case PendingKind::Url: {
        auto approved = approval(false);
        if (!approved) return std::unexpected(approved.error());
        value = json::Object{{"action", *approved == "accept" ? "accept" : "decline"}, {"content", Json{}}};
        break;
    }
    case PendingKind::Question: {
        auto group = groups_.find(pending.group);
        if (group == groups_.end()) return std::unexpected(AdapterError::UnknownDecision{decision});
        json::Array answers;
        for (const auto& line : response.values) answers.emplace_back(line);
        auto& pairs = group->second.answers;
        auto existing = std::find_if(pairs.begin(), pairs.end(), [&](const auto& pair) { return pair.first == pending.question; });
        Json answered(json::Object{{"answers", std::move(answers)}});
        if (existing == pairs.end()) pairs.emplace_back(pending.question, std::move(answered));
        else existing->second = std::move(answered);
        --group->second.waiting;
        break;
    }
    }
    std::optional<Reply> reply;
    if (pending.kind == PendingKind::Question) {
        auto group = groups_.find(pending.group);
        if (group->second.waiting == 0) {
            reply = result(std::move(group->second.rpc_id),
                           json::Object{{"answers", std::move(group->second.answers)}});
            groups_.erase(group);
        }
    } else reply = result(pending.rpc_id, std::move(value));
    decisions_.erase(it);
    COMMIT(w.set(decision, attr::ANSWER, response.values));
    COMMIT(w.set(decision, attr::BY, response.by));
    if (response.reason) COMMIT(w.set(decision, attr::REASON, *response.reason));
    COMMIT(w.close(decision, DecisionStatus::Answered));
    FORWARD(unwait(decision, w));
    return reply;
}

#undef COMMIT
#undef FORWARD
} // namespace loa::harness::codex
