#include "loa/harness/codex/rollout.hpp"

#include <algorithm>
#include <tuple>
#include <array>
#include <cstdint>
#include <istream>
#include <limits>
#include <optional>
#include <ostream>
#include <string>
#include <unordered_map>
#include <utility>
#include <vector>

#include "loa/core/writer.hpp"
#include "loa/harness/kit/residual.hpp"
#include "loa/runtime/json.hpp"
#include "loa/runtime/time.hpp"

namespace loa::harness::codex::rollout {
namespace {
namespace json = loa::runtime::json;
namespace core = loa::core;
namespace attr = core::attr;
using loa::contracts::AdapterError;
using loa::contracts::Result;
using core::NodeId;
using core::ElementKind;
using Obj = json::Object;
using J = json::Value;
using Attrs = core::Attrs;
constexpr std::string_view harness = "codex", x_line = "x-line", x_codex = "x-codex", x_parts = "x-codex-parts";
constexpr std::string_view x_line_end = "x-line-end", x_codex_end = "x-codex-end", x_implicit = "x-codex-implicit";
constexpr std::string_view reasoning_separator = "\n\n";
constexpr std::string_view x_raw = "x-codex-raw", x_raw_end = "x-codex-raw-end";

AdapterError format(std::string detail) { return AdapterError{AdapterError::Format{std::move(detail)}}; }
AdapterError commit_error(const core::CommitError& e) { return AdapterError{AdapterError::Commit{e}}; }
const J* find(const Obj& obj, std::string_view key) {
    auto it = std::find_if(obj.begin(), obj.end(), [&](const auto& pair) { return pair.first == key; });
    return it == obj.end() ? nullptr : &it->second;
}
J* find(Obj& obj, std::string_view key) {
    auto it = std::find_if(obj.begin(), obj.end(), [&](const auto& pair) { return pair.first == key; });
    return it == obj.end() ? nullptr : &it->second;
}
void set(Obj& obj, std::string_view key, J value) {
    if (auto* old = find(obj, key)) *old = std::move(value);
    else obj.emplace_back(std::string(key), std::move(value));
}
std::optional<J> take(Obj& obj, std::string_view key) {
    auto it = std::find_if(obj.begin(), obj.end(), [&](const auto& pair) { return pair.first == key; });
    if (it == obj.end()) return std::nullopt;
    J value = std::move(it->second);
    obj.erase(it);
    return value;
}
std::optional<std::string> take_str(Obj& obj, std::string_view key) {
    if (auto* v = find(obj, key); v && v->as_string()) return *take(obj, key)->as_string();
    return std::nullopt;
}
std::optional<std::string_view> str(const J* v) { if (v && v->as_string()) return *v->as_string(); return std::nullopt; }
std::optional<std::string_view> str_at(const J* v, std::string_view key) { return v ? str(v->find(key)) : std::nullopt; }
void sort_json(J& value) {
    if (auto* object = value.as_object()) {
        for (auto& [_, child] : *object) sort_json(child);
        std::sort(object->begin(), object->end(), [](const auto& a, const auto& b) { return a.first < b.first; });
    } else if (auto* array = value.as_array()) {
        for (auto& child : *array) sort_json(child);
    }
}
std::string canonical(J value) {
    sort_json(value);
    return json::dump(value);
}
Obj& payload(Obj& line) {
    auto* item = find(line, "payload");
    if (!item) { set(line, "payload", Obj{}); item = find(line, "payload"); }
    if (!item->as_object()) *item = Obj{};
    return *item->as_object();
}
bool is_text_part(const Obj& part) {
    const auto type = str(find(part, "type"));
    return type && (*type == "input_text" || *type == "output_text" || *type == "summary_text" || *type == "reasoning_text" || *type == "text");
}
std::pair<std::string, std::vector<core::Value>> take_parts(Obj& obj, std::initializer_list<std::string_view> keys,
                                                            std::string_view separator) {
    std::vector<std::string_view> texts;
    // Keep removed strings alive while joining: collect owned strings instead of views into erased values.
    std::vector<std::string> owned;
    for (auto key : keys) {
        auto* v = find(obj, key);
        if (!v || !v->as_array()) continue;
        for (auto& part : *v->as_array()) {
            auto* p = part.as_object();
            if (p && is_text_part(*p)) {
                auto text = take_str(*p, "text");
                if (text) owned.push_back(std::move(*text));
            }
        }
    }
    texts.reserve(owned.size());
    for (const auto& text : owned) texts.push_back(text);
    return kit::residual::join_parts(texts, separator);
}
void restore_parts(Obj& obj, std::initializer_list<std::string_view> keys, std::string_view separator,
                   std::string_view text, std::span<const core::Value> lengths) {
    std::vector<Obj*> slots;
    for (auto key : keys) {
        auto* v = find(obj, key);
        if (!v || !v->as_array()) continue;
        for (auto& part : *v->as_array()) {
            auto* p = part.as_object();
            if (p && is_text_part(*p) && !find(*p, "text")) slots.push_back(p);
        }
    }
    auto pieces = kit::residual::split_parts(text, separator, lengths);
    if (pieces && pieces->size() != slots.size()) pieces.reset();
    for (std::size_t i = 0; i < slots.size(); ++i)
        set(*slots[i], "text", pieces ? J((*pieces)[i]) : J(i == 0 ? text : std::string_view{}));
}
Result<NodeId> insert(core::DomWriter& writer, NodeId parent, ElementKind kind, Attrs attrs, std::string text = {}) {
    auto result = writer.insert(parent, kind, std::move(attrs), std::move(text));
    if (!result) return std::unexpected(commit_error(result.error()));
    return *result;
}
Result<> set_attr(core::DomWriter& writer, NodeId node, std::string_view name, core::Value value) {
    auto r = writer.set(node, name, std::move(value));
    if (!r) return std::unexpected(commit_error(r.error()));
    return {};
}

struct Line {
    std::int64_t at;
    Obj payload;
    Obj rest;
    std::string residual() && { set(rest, "payload", std::move(payload)); return canonical(J(std::move(rest))); }
};
enum class CallLine { Function, Custom, LocalShell };
struct Turn {
    NodeId node;
    std::optional<std::string> id;
    bool implicit = false, replied = false;
    std::vector<NodeId> open_calls;
};
class Importer {
  public:
    Importer(core::PatchSink& sink, std::string_view node) : sink_(sink), node_(node) {}
    bool has_session() const { return session_.has_value(); }
    Result<> finish() { if (!session_) return std::unexpected(format("the rollout has no session_meta line")); return close_turn(core::TurnStatus::Cancelled); }
    Result<> line(std::size_t index, std::string_view raw) {
        raw_ = std::string(raw);
        auto parsed = json::parse(raw);
        if (!parsed) return std::unexpected(format("line " + std::to_string(index + 1) + ": " + parsed.error().message()));
        auto* root = parsed->as_object();
        if (!root) return std::unexpected(format("line " + std::to_string(index + 1) + ": not a JSON object"));
        Obj rest = std::move(*root);
        if (auto timestamp = str(find(rest, "timestamp"))) {
            if (auto ts = loa::runtime::time::parse_iso_ms(*timestamp)) ts_ = *ts;
        }
        const std::string kind(str(find(rest, "type")).value_or(""));
        auto sub_view = str_at(find(rest, "payload"), "type");
        std::optional<std::string> sub;
        if (sub_view) sub = std::string(*sub_view);
        const auto at = index > static_cast<std::size_t>(std::numeric_limits<std::int64_t>::max())
                            ? std::numeric_limits<std::int64_t>::max() : static_cast<std::int64_t>(index);
        if (!session_) {
            if (kind != "session_meta") return std::unexpected(format("line " + std::to_string(index + 1) + ": the rollout doesn't start with session_meta"));
            auto data = take(rest, "payload");
            if (!data || !data->as_object()) return std::unexpected(format("line " + std::to_string(index + 1) + ": session_meta without payload"));
            Obj p = std::move(*data->as_object());
            auto created = str(find(p, "timestamp"));
            const auto created_ms = created ? loa::runtime::time::parse_iso_ms(*created).value_or(ts_) : ts_;
            auto id = take_str(p, "id");
            if (!id) return std::unexpected(format("line " + std::to_string(index + 1) + ": session_meta without id"));
            auto cwd = take_str(p, "cwd");
            Attrs attrs{{std::string(attr::ID), *id}, {std::string(attr::HARNESS), std::string(harness)},
                        {std::string(attr::NODE), std::string(node_)}, {std::string(attr::CREATED), created_ms},
                        {std::string(attr::SCHEMA), core::SCHEMA_VERSION}, {std::string(attr::NATIVE_ID), *id}};
            if (cwd) attrs.emplace(attr::CWD, std::move(*cwd));
            auto result = put(NodeId::DOCUMENT, ElementKind::Session, std::move(attrs), {}, {at, std::move(p), std::move(rest)});
            if (!result) return std::unexpected(result.error());
            session_ = *result;
            return {};
        }
        if (kind == "turn_context") {
            auto r = turn_context(find(rest, "payload"));
            if (!r) return r;
        } else if (auto data = take(rest, "payload"); data && data->as_object()) {
            Line l{at, std::move(*data->as_object()), std::move(rest)};
            Result<bool> kept = false;
            if (kind == "response_item") {
                if (sub == "message" || sub == "reasoning") kept = message(std::move(l), sub == "reasoning");
                else if (sub == "function_call" || sub == "custom_tool_call" || sub == "local_shell_call")
                    kept = call(std::move(l), sub == "function_call" ? CallLine::Function : sub == "custom_tool_call" ? CallLine::Custom : CallLine::LocalShell);
                else if (sub == "function_call_output" || sub == "custom_tool_call_output") kept = output(std::move(l));
            } else if (kind == "compacted") kept = compacted(std::move(l));
            else if (kind == "event_msg") {
                if (sub == "task_started" || sub == "turn_started") kept = task_started(std::move(l));
                else if (sub == "task_complete" || sub == "turn_complete" || sub == "turn_aborted") {
                    const auto* extension = find(l.rest, "x-loa");
                    const bool failed = str_at(extension, "status") == "error";
                    kept = task_end(std::move(l), sub == "turn_aborted" ? core::TurnStatus::Cancelled : failed ? core::TurnStatus::Error : core::TurnStatus::Done);
                } else if (sub == "token_count") kept = token_count(std::move(l));
            }
            if (!kept) return std::unexpected(kept.error());
            if (*kept) return {};
        }
        const auto parent = turn_ ? turn_->node : *session_;
        const std::string ty = sub ? kind + ":" + *sub : kind;
        auto writer = w();
        auto ext = insert(writer, parent, ElementKind::Ext,
                          {{std::string(attr::HARNESS), std::string(harness)}, {std::string(attr::TYPE), ty}, {std::string(x_line), at}}, std::string(raw));
        if (!ext) return std::unexpected(ext.error());
        return {};
    }
  private:
    core::DomWriter w() { return core::DomWriter(sink_).at(ts_); }
    Result<NodeId> put(NodeId parent, ElementKind kind, Attrs attrs, std::string text, Line line) {
        attrs.emplace(x_line, line.at);
        attrs.emplace(x_raw, raw_);
        attrs.emplace(x_codex, std::move(line).residual());
        auto writer = w();
        return insert(writer, parent, kind, std::move(attrs), std::move(text));
    }
    Result<> close_turn(core::TurnStatus status) {
        if (!turn_) return {};
        auto turn = std::move(*turn_); turn_.reset();
        auto writer = w();
        for (auto id : turn.open_calls) { auto r = set_attr(writer, id, attr::STATUS, core::CallStatus::Cancelled); if (!r) return r; }
        return set_attr(writer, turn.node, attr::STATUS, turn.implicit ? core::TurnStatus::Done : status);
    }
    Result<NodeId> turn_for(bool prompt, bool replied) {
        if (!turn_ || (prompt && turn_->implicit && turn_->replied)) {
            auto r = close_turn(core::TurnStatus::Done); if (!r) return std::unexpected(r.error());
            auto writer = w();
            auto id = insert(writer, *session_, ElementKind::Turn,
                             {{std::string(attr::STATUS), core::TurnStatus::Running}, {std::string(attr::STARTED), ts_}, {std::string(x_implicit), true}});
            if (!id) return id;
            turns_.push_back(*id);
            turn_ = Turn{*id, {}, true, false, {}};
        }
        turn_->replied |= replied;
        return turn_->node;
    }
    Result<bool> message(Line l, bool thinking) {
        bool injected = false;
        if (auto* content = find(l.payload, "content"); content && content->as_array()) {
            bool any = false, all = true;
            constexpr std::array<std::string_view, 5> prefixes{"<environment_context>", "<user_instructions>", "# AGENTS.md instructions", "<permissions instructions>", "<turn_aborted>"};
            for (const auto& part : *content->as_array()) if (auto text = str_at(&part, "text")) {
                any = true;
                all &= std::any_of(prefixes.begin(), prefixes.end(), [&](auto prefix) { return text->starts_with(prefix); });
            }
            injected = any && all;
        }
        auto [text, lengths] = thinking ? take_parts(l.payload, {"summary", "content"}, reasoning_separator)
                                        : take_parts(l.payload, {"content"}, "");
        auto role = core::Role::System;
        if (thinking || str(find(l.payload, "role")) == "assistant") role = core::Role::Assistant;
        else if (str(find(l.payload, "role")) == "user" && !injected) role = core::Role::User;
        auto id = take_str(l.payload, "id");
        auto parent = turn_for(role == core::Role::User, role == core::Role::Assistant);
        if (!parent) return std::unexpected(parent.error());
        Attrs attrs{{std::string(attr::ROLE), role}, {std::string(attr::STATUS), core::MessageStatus::Done}, {std::string(x_parts), core::Value(std::move(lengths))}};
        if (thinking) attrs.emplace(attr::KIND, core::MessageKind::Thinking);
        if (id) attrs.emplace(attr::ID, std::move(*id));
        auto result = put(*parent, ElementKind::Message, std::move(attrs), std::move(text), std::move(l));
        if (!result) return std::unexpected(result.error());
        return true;
    }
    Result<bool> call(Line l, CallLine kind) {
        std::string tool;
        std::optional<std::string> input, title;
        if (kind == CallLine::LocalShell) {
            auto action = take(l.payload, "action");
            if (action) {
                if (const auto* commands = action->find("command"); commands && commands->as_array()) {
                    std::string joined;
                    for (const auto& command : *commands->as_array()) if (auto part = str(&command)) {
                        if (!joined.empty()) joined += ' ';
                        joined += *part;
                    }
                    title = std::move(joined);
                }
                input = canonical(std::move(*action));
            }
            tool = "local_shell";
        } else {
            auto name = take_str(l.payload, "name");
            if (!name) return false;
            tool = std::move(*name);
            input = take_str(l.payload, kind == CallLine::Function ? "arguments" : "input");
        }
        auto call_id = take_str(l.payload, "call_id");
        auto key = call_id ? call_id : std::optional<std::string>(str(find(l.payload, "id")).value_or(""));
        if (key && key->empty()) key.reset();
        auto parent = turn_for(false, true);
        if (!parent) return std::unexpected(parent.error());
        Attrs attrs{{std::string(attr::TOOL), std::move(tool)}, {std::string(attr::STATUS), core::CallStatus::Running}};
        if (call_id) attrs.emplace(attr::CALL_ID, std::move(*call_id));
        if (title) attrs.emplace("title", std::move(*title));
        auto node = put(*parent, ElementKind::Call, std::move(attrs), {}, std::move(l));
        if (!node) return std::unexpected(node.error());
        if (input) {
            Attrs args;
            if (kind == CallLine::Custom) args.emplace(attr::FORMAT, "text");
            auto writer = w();
            auto result = insert(writer, *node, ElementKind::Input, std::move(args), std::move(*input));
            if (!result) return std::unexpected(result.error());
        }
        if (key) calls_.insert_or_assign(std::move(*key), *node);
        turn_->open_calls.push_back(*node);
        return true;
    }
    Result<bool> output(Line l) {
        auto call_id = str(find(l.payload, "call_id"));
        if (!call_id) return false;
        const auto found = calls_.find(std::string(*call_id));
        if (found == calls_.end()) return false;
        auto call = found->second;
        take(l.payload, "call_id");
        Attrs attrs;
        std::string text;
        if (auto* out = find(l.payload, "output")) {
            if (out->as_string()) text = *take_str(l.payload, "output");
            else if (out->as_array()) {
                auto parts = take_parts(l.payload, {"output"}, "");
                text = std::move(parts.first);
                attrs.emplace(x_parts, core::Value(std::move(parts.second)));
            } else text = canonical(*out);
        }
        const auto* extension = find(l.rest, "x-loa");
        bool failed = extension && extension->find("is_error") && extension->find("is_error")->as_bool() && *extension->find("is_error")->as_bool();
        if (failed) attrs.emplace(attr::IS_ERROR, true);
        auto node = put(call, ElementKind::Result, std::move(attrs), std::move(text), std::move(l));
        if (!node) return std::unexpected(node.error());
        auto writer = w();
        auto closed = writer.close(call, failed ? core::CallStatus::Error : core::CallStatus::Done);
        if (!closed) return std::unexpected(commit_error(closed.error()));
        if (turn_) std::erase(turn_->open_calls, call);
        return true;
    }
    Result<bool> compacted(Line l) {
        auto summary = take_str(l.payload, "message").value_or("");
        core::Value::List covers;
        for (auto i = covered_; i < turns_.size(); ++i) covers.emplace_back(turns_[i]);
        covered_ = turns_.size();
        auto result = put(*session_, ElementKind::Compaction,
                          {{std::string(attr::COVERS), core::Value(std::move(covers))}, {std::string(attr::CREATED), ts_}},
                          std::move(summary), std::move(l));
        if (!result) return std::unexpected(result.error());
        return true;
    }
    Result<> turn_context(const J* p) {
        if (!p) return {};
        auto writer = w();
        for (auto [field, name, key] : {std::tuple{&model_, attr::MODEL, "model"}, std::tuple{&effort_, attr::EFFORT, "effort"}}) {
            if (auto value = str_at(p, key); value && field->value_or("") != *value) {
                *field = *value;
                auto r = set_attr(writer, *session_, name, std::string(*value)); if (!r) return r;
            }
        }
        if (auto value = str_at(p->find("collaboration_mode"), "mode")) {
            auto mode = *value == "plan" ? core::SessionMode::Plan : core::SessionMode::Normal;
            if (!mode_ || *mode_ != mode) {
                mode_ = mode;
                auto r = set_attr(writer, *session_, attr::MODE, mode); if (!r) return r;
            }
        }
        return {};
    }
    Result<bool> task_started(Line l) {
        auto id = take_str(l.payload, "turn_id");
        auto close = close_turn(core::TurnStatus::Cancelled);
        if (!close) return std::unexpected(close.error());
        Attrs attrs{{std::string(attr::STATUS), core::TurnStatus::Running}, {std::string(attr::STARTED), ts_}};
        if (id) attrs.emplace(attr::ID, *id);
        auto node = put(*session_, ElementKind::Turn, std::move(attrs), {}, std::move(l));
        if (!node) return std::unexpected(node.error());
        turns_.push_back(*node);
        turn_ = Turn{*node, std::move(id), false, false, {}};
        return true;
    }
    Result<bool> task_end(Line l, core::TurnStatus status) {
        if (!turn_) return false;
        if (auto actual = str(find(l.payload, "turn_id")); turn_->id && actual) {
            if (*turn_->id != *actual) return false;
            take(l.payload, "turn_id");
        }
        auto writer = w();
        auto raw_result = set_attr(writer, turn_->node, x_raw_end, raw_);
        if (!raw_result) return std::unexpected(raw_result.error());
        auto r = set_attr(writer, turn_->node, x_codex_end, std::move(l).residual()); if (!r) return std::unexpected(r.error());
        r = set_attr(writer, turn_->node, x_line_end, l.at); if (!r) return std::unexpected(r.error());
        r = set_attr(writer, turn_->node, attr::ENDED, ts_); if (!r) return std::unexpected(r.error());
        r = close_turn(status); if (!r) return std::unexpected(r.error());
        return true;
    }
    Result<bool> token_count(Line l) {
        const auto* info = find(l.payload, "info");
        const auto* usage = info ? info->find("last_token_usage") : nullptr;
        if (!usage || !turn_) return false;
        Attrs attrs;
        for (auto [field, name] : {std::pair{"input_tokens", attr::INPUT_TOKENS}, {"output_tokens", attr::OUTPUT_TOKENS},
                                   {"cached_input_tokens", attr::CACHE_READ_TOKENS}, {"cache_write_input_tokens", attr::CACHE_WRITE_TOKENS}}) {
            if (auto* count = usage->find(field)) if (auto n = count->as_i64()) attrs.emplace(name, *n);
        }
        auto node = put(turn_->node, ElementKind::Usage, std::move(attrs), {}, std::move(l));
        if (!node) return std::unexpected(node.error());
        return true;
    }
    core::PatchSink& sink_;
    std::string node_;
    std::int64_t ts_ = 0;
    std::optional<NodeId> session_;
    std::optional<Turn> turn_;
    std::unordered_map<std::string, NodeId> calls_;
    std::vector<NodeId> turns_;
    std::size_t covered_ = 0;
    std::optional<std::string> model_, effort_;
    std::string raw_;
    std::optional<core::SessionMode> mode_;
};

class Exporter {
  public:
    explicit Exporter(const core::Dom& dom) : dom_(dom) {}
    Result<> run(std::ostream& out) {
        const auto* root = dom_.root();
        if (!root) return std::unexpected(format("the DOM is empty"));
        if (root->kind != ElementKind::Session) return std::unexpected(format("the DOM root isn't a <session>"));
        ts_ = root->int_attr(attr::CREATED).value_or(0);
        auto r = node(*root);
        if (!r) return r;
        std::stable_sort(lines_.begin(), lines_.end(), [](const auto& a, const auto& b) { return a.first < b.first; });
        for (const auto& [_, line] : lines_) out << line << '\n';
        if (!out) return std::unexpected(AdapterError{AdapterError::Io{loa::Error(loa::ErrorCode::io, "writing Codex rollout failed")}});
        return {};
    }
  private:
    Result<std::optional<Obj>> residual(const core::Node& node, std::string_view name) const {
        auto source = node.str_attr(name);
        if (!source) return std::optional<Obj>{};
        auto parsed = json::parse(*source);
        if (!parsed || !parsed->as_object())
            return std::unexpected(format("node " + core::to_string(node.id) + " has a malformed " + std::string(name)));
        return std::optional<Obj>(std::move(*parsed->as_object()));
    }
    Obj new_line(std::string_view kind, J p) const {
        return {{"timestamp", loa::runtime::time::format_iso_ms(ts_)}, {"type", std::string(kind)}, {"payload", std::move(p)}};
    }
    void push(std::optional<std::int64_t> at, std::string line) {
        key_ = std::max(key_, at.value_or(key_));
        lines_.emplace_back(at.value_or(key_), std::move(line));
    }
    void push(std::optional<std::int64_t> at, Obj line) { push(at, canonical(J(std::move(line)))); }
    void push_node(const core::Node& node, std::optional<std::int64_t> at, Obj line,
                   std::string_view raw_name = x_raw) {
        if (auto raw = node.str_attr(raw_name)) {
            auto original = json::parse(*raw);
            if (original && canonical(*original) == canonical(J(line))) {
                push(at, std::string(*raw));
                return;
            }
        }
        push(at, std::move(line));
    }
    void stamp(const core::Node& n, std::string_view field) { if (auto ts = n.int_attr(field)) ts_ = *ts; }
    Result<> children(const core::Node& n) {
        for (auto* child : dom_.children(n.id)) { auto r = node(*child); if (!r) return r; }
        return {};
    }
    Result<> node(const core::Node& n) {
        const auto at = n.int_attr(x_line);
        switch (n.kind) {
        case ElementKind::Session: {
            const auto id = n.str_attr(attr::NATIVE_ID).value_or(n.str_attr(attr::ID).value_or(""));
            auto imported = residual(n, x_codex); if (!imported) return std::unexpected(imported.error());
            Obj line = imported->value_or(new_line("session_meta",
                Obj{{"session_id", id}, {"timestamp", loa::runtime::time::format_iso_ms(ts_)}, {"cwd", ""},
                    {"originator", "loa"}, {"cli_version", "0.5.0"}, {"source", "cli"}, {"model_provider", nullptr}}));
            set(payload(line), "id", id);
            if (auto cwd = n.str_attr(attr::CWD)) set(payload(line), "cwd", *cwd);
            push_node(n, at, std::move(line));
            return children(n);
        }
        case ElementKind::Turn: return turn(n);
        case ElementKind::Message: return message(n);
        case ElementKind::Call: return call(n);
        case ElementKind::Compaction: {
            stamp(n, attr::CREATED);
            auto imported = residual(n, x_codex); if (!imported) return std::unexpected(imported.error());
            auto line = imported->value_or(new_line("compacted", Obj{}));
            set(payload(line), "message", n.text);
            push_node(n, at, std::move(line));
            return {};
        }
        case ElementKind::Usage: return usage(n);
        case ElementKind::Ext:
            if (at && n.str_attr(attr::HARNESS) == harness) push(at, n.text);
            return {};
        default: return {};
        }
    }
    Result<> turn(const core::Node& n) {
        stamp(n, attr::STARTED);
        const auto id = n.str_attr(attr::ID);
        const auto turn_id = id ? std::string(*id) : "loa-turn-" + std::to_string(n.id.value);
        const bool implicit = n.bool_attr(x_implicit) == true;
        if (!implicit) {
            auto imported = residual(n, x_codex); if (!imported) return std::unexpected(imported.error());
            auto line = imported->value_or(new_line("event_msg", Obj{{"type", "task_started"}, {"turn_id", turn_id}, {"model_context_window", nullptr}}));
            if (id) set(payload(line), "turn_id", *id);
            push_node(n, n.int_attr(x_line), std::move(line));
        }
        auto r = children(n); if (!r) return r;
        stamp(n, attr::ENDED);
        auto imported = residual(n, x_codex_end); if (!imported) return std::unexpected(imported.error());
        std::optional<Obj> end = std::move(*imported);
        const auto status = n.str_attr(attr::STATUS);
        if (end) {
            if (id && !find(payload(*end), "turn_id")) set(payload(*end), "turn_id", *id);
        } else if (!implicit) {
            if (status == "done" || status == "error")
                end = new_line("event_msg", Obj{{"type", "task_complete"}, {"turn_id", turn_id}, {"last_agent_message", nullptr}});
            else if (status == "cancelled")
                end = new_line("event_msg", Obj{{"type", "turn_aborted"}, {"turn_id", turn_id}, {"reason", "interrupted"}});
        }
        if (end) {
            if (status == "error") set(*end, "x-loa", Obj{{"status", "error"}});
            push_node(n, n.int_attr(x_line_end), std::move(*end), x_raw_end);
        }
        return {};
    }
    Result<> message(const core::Node& n) {
        const bool thinking = n.str_attr(attr::KIND) == "thinking";
        auto imported = residual(n, x_codex); if (!imported) return std::unexpected(imported.error());
        Obj line;
        if (*imported) {
            line = std::move(**imported);
            auto lengths = n.attr(x_parts);
            auto list = lengths ? lengths->as_list() : std::optional<std::span<const core::Value>>{};
            restore_parts(payload(line), {"summary", "content"}, thinking ? reasoning_separator : "", n.text, list.value_or(std::span<const core::Value>{}));
        } else if (thinking) {
            json::Array summary;
            if (!n.text.empty()) summary.emplace_back(Obj{{"type", "summary_text"}, {"text", n.text}});
            line = new_line("response_item", Obj{{"type", "reasoning"}, {"summary", std::move(summary)},
                                                 {"content", nullptr}, {"encrypted_content", nullptr}});
        } else {
            const auto role = n.str_attr(attr::ROLE);
            const auto user = role == "user", assistant = role == "assistant";
            line = new_line("response_item", Obj{{"type", "message"},
                {"role", assistant ? "assistant" : user ? "user" : "developer"},
                {"content", json::Array{Obj{{"type", assistant ? "output_text" : "input_text"}, {"text", n.text}}}}});
        }
        if (auto id = n.str_attr(attr::ID)) set(payload(line), "id", *id);
        push_node(n, n.int_attr(x_line), std::move(line));
        return {};
    }
    static std::string call_id(const core::Node& n) {
        if (auto id = n.str_attr(attr::CALL_ID)) return std::string(*id);
        return "call_loa_" + std::to_string(n.id.value);
    }
    Result<> call(const core::Node& n) {
        stamp(n, attr::STARTED);
        const auto tool = n.str_attr(attr::TOOL).value_or("");
        auto inputs = dom_.children_of_kind(n.id, ElementKind::Input);
        const auto* input = inputs.empty() ? nullptr : inputs.front();
        auto imported = residual(n, x_codex); if (!imported) return std::unexpected(imported.error());
        CallLine kind = CallLine::Function;
        if (*imported) {
            auto* p = find(**imported, "payload");
            if (str_at(p, "type") == "custom_tool_call") kind = CallLine::Custom;
            else if (str_at(p, "type") == "local_shell_call") kind = CallLine::LocalShell;
        } else if (input && input->str_attr(attr::FORMAT) == "text") kind = CallLine::Custom;
        else if (tool == "local_shell" && input) {
            auto parsed = json::parse(input->text);
            if (parsed && parsed->as_object()) kind = CallLine::LocalShell;
        }
        const bool live = !*imported;
        const auto type = kind == CallLine::Custom ? "custom_tool_call" : kind == CallLine::LocalShell ? "local_shell_call" : "function_call";
        const auto field = kind == CallLine::Custom ? "input" : kind == CallLine::LocalShell ? "action" : "arguments";
        Obj line = imported->value_or(new_line("response_item", Obj{{"type", type}}));
        auto& p = payload(line);
        if (live || n.attr(attr::CALL_ID)) set(p, "call_id", call_id(n));
        if (kind != CallLine::LocalShell) set(p, "name", tool);
        if (input) {
            if (kind == CallLine::LocalShell) {
                auto parsed = json::parse(input->text);
                set(p, field, parsed ? std::move(*parsed) : J(input->text));
            } else set(p, field, input->text);
        } else if (live && kind == CallLine::Function) set(p, field, "{}");
        if (live && kind != CallLine::Function) set(p, "status", "completed");
        push_node(n, n.int_attr(x_line), std::move(line));
        stamp(n, attr::ENDED);
        for (auto* result : dom_.children_of_kind(n.id, ElementKind::Result)) {
            auto r = result_line(*result, n, kind); if (!r) return r;
        }
        return {};
    }
    Result<> result_line(const core::Node& n, const core::Node& call, CallLine kind) {
        auto imported = residual(n, x_codex); if (!imported) return std::unexpected(imported.error());
        Obj line = imported->value_or(new_line("response_item", Obj{{"type", kind == CallLine::Custom ? "custom_tool_call_output" : "function_call_output"}}));
        auto& p = payload(line);
        set(p, "call_id", call_id(call));
        auto lengths = n.attr(x_parts);
        auto list = lengths ? lengths->as_list() : std::optional<std::span<const core::Value>>{};
        auto* output = find(p, "output");
        if (list && output && output->as_array()) restore_parts(p, {"output"}, "", n.text, *list);
        else if (!output) set(p, "output", n.text);
        if (n.bool_attr(attr::IS_ERROR) == true) {
            auto* extension = find(line, "x-loa");
            if (!extension) set(line, "x-loa", Obj{});
            if (auto* fields = find(line, "x-loa")->as_object()) set(*fields, "is_error", true);
        }
        push_node(n, n.int_attr(x_line), std::move(line));
        return {};
    }
    Result<> usage(const core::Node& n) {
        auto imported = residual(n, x_codex); if (!imported) return std::unexpected(imported.error());
        if (*imported) { push_node(n, n.int_attr(x_line), std::move(**imported)); return {}; }
        const auto* parent = dom_.get(n.parent);
        if (!parent || parent->kind != ElementKind::Turn) return {};
        auto count = [&](std::string_view name) { return n.int_attr(name).value_or(0); };
        const auto input = count(attr::INPUT_TOKENS), output = count(attr::OUTPUT_TOKENS);
        Obj tokens{{"input_tokens", input}, {"cached_input_tokens", count(attr::CACHE_READ_TOKENS)},
                   {"output_tokens", output}, {"reasoning_output_tokens", 0}, {"total_tokens", input + output}};
        if (auto write = n.int_attr(attr::CACHE_WRITE_TOKENS)) set(tokens, "cache_write_input_tokens", *write);
        auto line = new_line("event_msg", Obj{{"type", "token_count"},
            {"info", Obj{{"total_token_usage", tokens}, {"last_token_usage", std::move(tokens)}, {"model_context_window", nullptr}}},
            {"rate_limits", nullptr}});
        push(n.int_attr(x_line), std::move(line));
        return {};
    }
    const core::Dom& dom_;
    std::vector<std::pair<std::int64_t, std::string>> lines_;
    std::int64_t key_ = -1, ts_ = 0;
};

} // namespace

Result<> import(std::istream& input, core::PatchSink& sink, std::string_view node) {
    Importer importer(sink, node);
    std::string line;
    std::size_t index = 0;
    while (std::getline(input, line)) {
        if (line.find_first_not_of(" \t\r\n") != std::string::npos) {
            auto r = importer.line(index, line);
            if (!r) return r;
        }
        ++index;
    }
    if (input.bad()) return std::unexpected(AdapterError{AdapterError::Io{loa::Error(loa::ErrorCode::io, "reading Codex rollout failed")}});
    return importer.finish();
}

Result<> export_(const core::Dom& dom, std::ostream& output) { return Exporter(dom).run(output); }

Result<> CodexNative::import(std::istream& input, core::PatchSink& sink, std::string_view node) {
    return rollout::import(input, sink, node);
}
Result<> CodexNative::export_(const core::Dom& dom, std::ostream& output) {
    return rollout::export_(dom, output);
}

} // namespace loa::harness::codex::rollout
