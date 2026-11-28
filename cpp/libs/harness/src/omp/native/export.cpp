// DOM to omp JSONL session file.

#include <algorithm>
#include <format>
#include <optional>
#include <utility>
#include <unordered_set>
#include <vector>

#include "internal.hpp"
#include "loa/core/schema.hpp"
#include "loa/runtime/time.hpp"

namespace loa::harness::omp::native::detail {

namespace json = loa::runtime::json;
namespace attr = loa::core::attr;
using loa::contracts::AdapterError;
using loa::contracts::Result;
using loa::core::Dom;
using loa::core::ElementKind;
using loa::core::MessageKind;
using loa::core::Node;
using loa::core::NodeId;
using loa::core::Role;

namespace {

std::string_view str_or_empty(const Node& node, std::string_view name) { return node.str_attr(name).value_or(""); }

// The `id` of a parsed JSON line, when it is a string.
const json::Value* id_of(const std::optional<json::Value>& line) { return line ? line->find("id") : nullptr; }

// Builds entries for DOM content that has no raw line.
class Writer {
  public:
    // Id of the last entry written, the parent of the next one.
    json::Value parent;
    // Entry ids already in the file.
    std::unordered_set<std::string> used;
    std::vector<json::Object> entries;

    void compaction(const Node& node) {
        const std::int64_t ts = node.int_attr(attr::CREATED).value_or(0);
        emit(node.id, ts, "compaction",
             {{"summary", node.text}, {"tokensBefore", node.int_attr(attr::TOKENS_BEFORE).value_or(0)}});
        first_kept_.push_back(entries.size() - 1);
    }

    // Writes a live turn: user messages, then each run of assistant messages and calls as one
    // assistant message followed by the calls' results.
    void turn(const Dom& dom, const Node& turn) {
        const std::int64_t ts = turn.int_attr(attr::STARTED).value_or(0);
        std::vector<const Node*> group;
        for (const Node* item : dom.children(turn.id)) {
            const auto role_attr = item->str_attr(attr::ROLE);
            const std::optional<Role> role = role_attr ? loa::core::parse<Role>(*role_attr) : std::nullopt;
            if (item->kind == ElementKind::Message && role == Role::User) {
                assistant(dom, turn.id, group, ts);
                json::Object message{
                    {"role", "user"},
                    {"content", json::Array{json::Object{{"type", "text"}, {"text", item->text}}}},
                    {"timestamp", ts},
                };
                emit(item->id, ts, "message", {{"message", std::move(message)}});
            } else if ((item->kind == ElementKind::Message && role == Role::Assistant) || item->kind == ElementKind::Call) {
                group.push_back(item);
            }
        }
        assistant(dom, turn.id, group, ts);
    }

  private:
    // Indices of synthesized compactions waiting for the next entry's id.
    std::vector<std::size_t> first_kept_;

    // Appends an entry with the base fields and links it to the previous one.
    void emit(NodeId node, std::int64_t ts, std::string_view kind, json::Object fields) {
        const std::string base = std::format("{:08x}", node.value);
        std::string id = base;
        for (int suffix = 1; !used.insert(id).second; ++suffix) id = std::format("{}-{}", base, suffix);
        for (std::size_t index : first_kept_)
            if (index < entries.size()) set(entries[index], "firstKeptEntryId", id);
        first_kept_.clear();
        json::Object entry;
        entry.emplace_back("type", std::string(kind));
        entry.emplace_back("id", id);
        entry.emplace_back("parentId", std::exchange(parent, json::Value(id)));
        entry.emplace_back("timestamp", loa::runtime::time::format_iso_ms(ts));
        for (auto& [name, value] : fields) set(entry, name, std::move(value));
        entries.push_back(std::move(entry));
    }

    // Inserts or replaces, keeping an existing key's position.
    static void set(json::Object& map, std::string_view key, json::Value value) {
        for (auto& member : map)
            if (member.first == key) {
                member.second = std::move(value);
                return;
            }
        map.emplace_back(std::string(key), std::move(value));
    }

    // Writes the pending assistant messages and calls, then the call results.
    void assistant(const Dom& dom, NodeId turn, std::vector<const Node*>& group, std::int64_t ts) {
        if (group.empty()) return;
        const NodeId first = group.front()->id;
        json::Array content;
        content.reserve(group.size());
        std::optional<std::string_view> model;
        bool has_call = false;
        for (const Node* item : group) {
            if (item->kind == ElementKind::Call) {
                has_call = true;
                json::Value arguments = json::Object{};
                const auto inputs = dom.children_of_kind(item->id, ElementKind::Input);
                if (!inputs.empty())
                    if (auto parsed = json::parse(inputs.front()->text); parsed && parsed->is_object()) arguments = std::move(*parsed);
                json::Object block{
                    {"type", "toolCall"},
                    {"id", str_or_empty(*item, attr::CALL_ID)},
                    {"name", str_or_empty(*item, attr::TOOL)},
                    {"arguments", std::move(arguments)},
                };
                if (auto intent = item->str_attr(attr::TITLE)) block.emplace_back("intent", *intent);
                content.emplace_back(std::move(block));
                continue;
            }
            if (!model) model = item->str_attr(attr::MODEL);
            const auto kind_attr = item->str_attr(attr::KIND);
            const bool thinking = kind_attr && loa::core::parse<MessageKind>(*kind_attr) == MessageKind::Thinking;
            content.emplace_back(thinking ? json::Object{{"type", "thinking"}, {"thinking", item->text}}
                                          : json::Object{{"type", "text"}, {"text", item->text}});
        }
        std::string_view provider = "unknown";
        std::string_view model_name = model.value_or("unknown");
        if (model)
            if (const auto slash = model->find('/'); slash != std::string_view::npos) {
                provider = model->substr(0, slash);
                model_name = model->substr(slash + 1);
            }
        const Node* usage = nullptr;
        for (const Node* item : group) {
            if (item->kind != ElementKind::Message) continue;
            if (auto usages = dom.children_of_kind(item->id, ElementKind::Usage); !usages.empty()) {
                usage = usages.front();
                break;
            }
        }
        if (!usage)
            if (auto usages = dom.children_of_kind(turn, ElementKind::Usage); !usages.empty()) usage = usages.front();
        const auto count = [&](std::string_view name) -> std::int64_t {
            return usage ? usage->int_attr(name).value_or(0) : 0;
        };
        const loa::core::Value* cost_attr = usage ? usage->attr(attr::COST) : nullptr;
        const double cost = cost_attr ? cost_attr->as_float().value_or(0.0) : 0.0;
        std::string_view stop_reason = "stop";
        if (has_call) {
            stop_reason = "toolUse";
        } else if (const Node* node = dom.get(turn)) {
            const auto status = node->str_attr(attr::STATUS);
            if (status == "error") stop_reason = "error";
            else if (status == "cancelled") stop_reason = "aborted";
        }
        json::Object message{
            {"role", "assistant"},
            {"content", std::move(content)},
            {"api", "unknown"},
            {"provider", provider},
            {"model", model_name},
            {"usage",
             json::Object{
                 {"input", count(attr::INPUT_TOKENS)},
                 {"output", count(attr::OUTPUT_TOKENS)},
                 {"cacheRead", count(attr::CACHE_READ_TOKENS)},
                 {"cacheWrite", count(attr::CACHE_WRITE_TOKENS)},
                 {"totalTokens", count(attr::INPUT_TOKENS) + count(attr::OUTPUT_TOKENS)},
                 {"cost", json::Object{{"input", 0}, {"output", 0}, {"cacheRead", 0}, {"cacheWrite", 0}, {"total", cost}}},
             }},
            {"stopReason", stop_reason},
            {"timestamp", ts},
        };
        emit(first, ts, "message", {{"message", std::move(message)}});

        for (const Node* call : group) {
            if (call->kind != ElementKind::Call) continue;
            const auto results = dom.children_of_kind(call->id, ElementKind::Result);
            if (results.empty()) continue;
            const Node* result = results.front();
            json::Object result_message{
                {"role", "toolResult"},
                {"toolCallId", str_or_empty(*call, attr::CALL_ID)},
                {"toolName", str_or_empty(*call, attr::TOOL)},
                {"content", json::Array{json::Object{{"type", "text"}, {"text", result->text}}}},
                {"isError", result->bool_attr(attr::IS_ERROR).value_or(false)},
                {"timestamp", ts},
            };
            emit(result->id, ts, "message", {{"message", std::move(result_message)}});
        }
        group.clear();
    }
};

} // namespace

Result<std::string> run_export(const Dom& dom) {
    const Node* session = dom.root();
    if (!session || session->kind != ElementKind::Session)
        return std::unexpected(AdapterError::Format{"the DOM has no <session> root"});

    std::vector<std::pair<std::int64_t, std::string_view>> raws;
    for (const Node& node : dom.iter()) {
        const auto line = node.int_attr(X_LINE);
        if (!line) continue;
        std::string_view raw;
        if (node.kind == ElementKind::Ext) {
            raw = node.text;
        } else if (auto entry = node.str_attr(X_ENTRY)) {
            raw = *entry;
        } else {
            return std::unexpected(AdapterError::Format{std::format("<{}> {} has {} but no {}", loa::core::as_str(node.kind),
                                                                    loa::core::to_string(node.id), X_LINE, X_ENTRY)});
        }
        raws.emplace_back(*line, raw);
    }
    std::ranges::stable_sort(raws, {}, &std::pair<std::int64_t, std::string_view>::first);

    std::string out;
    Writer writer;
    for (const auto& [_, raw] : raws) {
        out += raw;
        out += '\n';
        auto parsed = json::parse(raw);
        const json::Value* id = parsed ? parsed->find("id") : nullptr;
        if (id && id->as_string()) writer.used.insert(*id->as_string());
    }
    if (!raws.empty()) {
        // The last entry in the file is the leaf of the imported active path.
        auto parsed = json::parse(raws.back().second);
        const std::optional<json::Value> leaf = parsed ? std::optional<json::Value>(std::move(*parsed)) : std::nullopt;
        const json::Value* type = leaf ? leaf->find("type") : nullptr;
        const bool is_entry = type && type->as_string() && *type->as_string() != "title" && *type->as_string() != "session";
        if (const json::Value* id = id_of(leaf); is_entry && id && id->is_string()) writer.parent = *id;
    } else {
        const std::string_view id = session->str_attr(attr::NATIVE_ID).or_else([&] { return session->str_attr(attr::ID); }).value_or("");
        json::Object header{
            {"type", "session"},
            {"version", 3},
            {"id", id},
            {"timestamp", loa::runtime::time::format_iso_ms(session->int_attr(attr::CREATED).value_or(0))},
            {"cwd", str_or_empty(*session, attr::CWD)},
        };
        if (auto title = session->str_attr(attr::TITLE)) header.emplace_back("title", *title);
        out += to_line(json::Value(std::move(header)));
        out += '\n';
    }

    for (const Node* child : dom.children(session->id)) {
        if (child->kind == ElementKind::Turn) {
            bool has_line = false;
            for (const Node& node : dom.descendants(child->id))
                if (node.attrs.contains(X_LINE)) {
                    has_line = true;
                    break;
                }
            if (!has_line) writer.turn(dom, *child);
        } else if (child->kind == ElementKind::Compaction && !child->attrs.contains(X_LINE)) {
            writer.compaction(*child);
        }
    }
    for (auto& entry : writer.entries) {
        out += to_line(json::Value(std::move(entry)));
        out += '\n';
    }
    return out;
}

} // namespace loa::harness::omp::native::detail
