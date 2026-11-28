// omp JSONL session file to DOM.

#include <algorithm>
#include <format>
#include <iterator>
#include <optional>
#include <unordered_map>
#include <unordered_set>
#include <vector>

#include "internal.hpp"
#include "loa/core/patch.hpp"
#include "loa/core/schema.hpp"
#include "loa/core/value.hpp"
#include "loa/harness/omp/native.hpp"
#include "loa/runtime/time.hpp"

namespace loa::harness::omp::native {

namespace json = loa::runtime::json;
namespace attr = loa::core::attr;
using loa::contracts::AdapterError;
using loa::contracts::Result;
using loa::core::Attrs;
using loa::core::CallStatus;
using loa::core::Draft;
using loa::core::ElementKind;
using loa::core::MessageKind;
using loa::core::MessageStatus;
using loa::core::NodeId;
using loa::core::Role;
using loa::core::SessionMode;
using loa::core::TurnStatus;
using loa::core::Value;

namespace {

// Session file version this module reads and writes.
constexpr std::uint64_t VERSION = 3;

// One non-blank, valid JSON object line of the file.
struct Line {
    // Position among the file's valid lines.
    std::int64_t line = 0;
    // The line as it goes back out: verbatim, or re-serialized after a migration.
    std::string raw;
    json::Object json;
    // A migration changed `json`, so `raw` must be rebuilt from it.
    bool changed = false;
    // Unix milliseconds of the entry.
    std::int64_t ts = 0;
};

// The member `key` (the last one, as serde_json keeps for duplicate keys).
const json::Value* get(const json::Object& map, std::string_view key) {
    for (auto it = map.rbegin(); it != map.rend(); ++it)
        if (it->first == key) return &it->second;
    return nullptr;
}

json::Value* get_mut(json::Object& map, std::string_view key) {
    for (auto it = map.rbegin(); it != map.rend(); ++it)
        if (it->first == key) return &it->second;
    return nullptr;
}

std::optional<std::string_view> str_field(const json::Object& map, std::string_view key) {
    const json::Value* v = get(map, key);
    const std::string* s = v ? v->as_string() : nullptr;
    if (!s) return std::nullopt;
    return std::string_view(*s);
}

// Inserts or replaces, keeping an existing key's position (serde_json `preserve_order`).
void insert(json::Object& map, std::string_view key, json::Value value) {
    if (json::Value* slot = get_mut(map, key)) {
        *slot = std::move(value);
        return;
    }
    map.emplace_back(std::string(key), std::move(value));
}

// serde_json `preserve_order` `Map::remove`: a swap remove.
void swap_remove(json::Object& map, std::string_view key) {
    auto it = std::ranges::find_if(map, [&](const auto& member) { return member.first == key; });
    if (it == map.end()) return;
    if (it != map.end() - 1) *it = std::move(map.back());
    map.pop_back();
}

bool is_blank(std::string_view s) {
    return std::ranges::all_of(s, [](unsigned char c) { return c == ' ' || (c >= '\t' && c <= '\r'); });
}

// Every non-blank line that parses as a JSON object. Malformed lines are skipped, as omp's
// lenient loader does.
std::vector<Line> parse_lines(std::string_view text) {
    std::vector<Line> out;
    while (!text.empty()) {
        const auto end = text.find('\n');
        std::string_view raw = text.substr(0, end);
        text = end == std::string_view::npos ? std::string_view() : text.substr(end + 1);
        if (raw.ends_with('\r')) raw.remove_suffix(1);
        if (is_blank(raw)) continue;
        auto parsed = json::parse(raw);
        if (!parsed) continue;
        json::Object* object = parsed->as_object();
        if (!object) continue;
        out.push_back(Line{static_cast<std::int64_t>(out.size()), std::string(raw), std::move(*object), false, 0});
    }
    return out;
}

// Brings a file to version 3 the way omp's session migrations do.
//
// v1 entries get ids from their position among the logical entries (header at 0, so the first
// entry is `00000001`), a linear parent chain, and compaction `firstKeptEntryIndex` (which
// counts the header) becomes `firstKeptEntryId`. v2 `hookMessage` roles become `custom`.
void migrate(Line& header, std::vector<Line>& entries) {
    const json::Value* raw_version = get(header.json, "version");
    const std::optional<double> version = raw_version ? raw_version->as_f64() : std::nullopt;
    if (version && *version >= static_cast<double>(VERSION)) return;
    if (!version || *version < 2.0) {
        std::vector<std::string> ids;
        ids.reserve(entries.size());
        for (std::size_t i = 1; i <= entries.size(); ++i) ids.push_back(std::format("{:08x}", i));
        json::Value parent;
        for (std::size_t i = 0; i < entries.size(); ++i) {
            Line& entry = entries[i];
            insert(entry.json, "id", ids[i]);
            insert(entry.json, "parentId", std::move(parent));
            parent = ids[i];
            std::optional<std::uint64_t> kept_index;
            if (str_field(entry.json, "type") == "compaction")
                if (const json::Value* index = get(entry.json, "firstKeptEntryIndex")) kept_index = index->as_u64();
            if (kept_index) {
                if (*kept_index >= 1 && *kept_index - 1 < ids.size())
                    insert(entry.json, "firstKeptEntryId", ids[*kept_index - 1]);
                swap_remove(entry.json, "firstKeptEntryIndex");
            }
            entry.changed = true;
        }
    }
    for (Line& entry : entries) {
        if (str_field(entry.json, "type") != "message") continue;
        json::Value* message = get_mut(entry.json, "message");
        json::Object* fields = message ? message->as_object() : nullptr;
        if (fields && str_field(*fields, "role") == "hookMessage") {
            insert(*fields, "role", "custom");
            entry.changed = true;
        }
    }
    insert(header.json, "version", json::Value(VERSION));
    header.changed = true;
}

// Indices of the entries from the root to the leaf (the last entry in the file), following
// `parentId` and stopping on a repeated id.
std::vector<std::size_t> active_path(const std::vector<Line>& entries) {
    std::unordered_map<std::string_view, std::size_t> by_id;
    for (std::size_t index = 0; index < entries.size(); ++index)
        if (auto id = str_field(entries[index].json, "id")) by_id.insert_or_assign(*id, index);
    std::vector<std::size_t> path;
    std::unordered_set<std::string_view> seen;
    std::optional<std::size_t> current;
    if (!entries.empty()) current = entries.size() - 1;
    while (current) {
        const Line& entry = entries[*current];
        if (auto id = str_field(entry.json, "id"); id && !seen.insert(*id).second) break;
        path.push_back(*current);
        current.reset();
        if (auto parent = str_field(entry.json, "parentId"))
            if (auto it = by_id.find(*parent); it != by_id.end()) current = it->second;
    }
    std::ranges::reverse(path);
    return path;
}

void put(Attrs& attrs, std::string_view name, Value value) { attrs.insert_or_assign(std::string(name), std::move(value)); }

// Attributes of an `<ext>` that keeps `line` as its text.
Attrs ext_attrs(std::string_view kind, const Line& line) {
    Attrs attrs;
    put(attrs, attr::HARNESS, detail::HARNESS);
    put(attrs, attr::TYPE, kind);
    put(attrs, detail::X_LINE, line.line);
    return attrs;
}

// Adds the entry's raw line and position to an element's attributes.
void mark(Attrs& attrs, const Line& entry) {
    put(attrs, detail::X_ENTRY, entry.raw);
    put(attrs, detail::X_LINE, entry.line);
}

AdapterError commit_error(loa::core::CommitError error) { return AdapterError::Commit{std::move(error)}; }

// The turn entries are currently added to.
struct OpenTurn {
    NodeId node;
    // Timestamp of the last entry placed in the turn: its `ended`.
    std::int64_t last_ts;
    // Status set at close, from the last assistant `stopReason`.
    TurnStatus status;
    // `<call>` nodes by the harness's tool call id.
    std::unordered_map<std::string, NodeId> calls;
};

class Importer {
  public:
    Importer(loa::core::PatchSink& sink, NodeId session) : sink_(sink), session_(session) {}

    Result<NodeId> insert(NodeId parent, ElementKind kind, Attrs attrs, std::string text, std::int64_t ts) {
        auto patch = sink_.commit_at(Draft::insert(parent, kind, std::move(attrs), std::move(text)), ts);
        if (!patch) return std::unexpected(commit_error(std::move(patch.error())));
        return patch->node;
    }

    Result<> set(NodeId node, std::string_view name, Value value, std::int64_t ts) {
        auto patch = sink_.commit_at(Draft::set(node, std::string(name), std::move(value)), ts);
        if (!patch) return std::unexpected(commit_error(std::move(patch.error())));
        return {};
    }

    // Maps one entry of the active path.
    Result<> entry(const Line& entry) {
        const json::Object& json = entry.json;
        const std::int64_t ts = entry.ts;
        const std::string_view type = str_field(json, "type").value_or("");
        if (type == "message") return message(entry);
        if (type == "compaction") return compaction(entry);
        if (type == "model_change") {
            const auto role = str_field(json, "role");
            if (auto model = str_field(json, "model"); model && (!role || *role == "default"))
                if (auto r = set(session_, attr::MODEL, *model, ts); !r) return r;
            return ext(entry, type);
        }
        if (type == "thinking_level_change") {
            if (auto level = str_field(json, "thinkingLevel"))
                if (auto r = set(session_, attr::EFFORT, *level, ts); !r) return r;
            return ext(entry, type);
        }
        if (type == "mode_change") {
            const SessionMode mode = str_field(json, "mode") == "plan" ? SessionMode::Plan : SessionMode::Normal;
            if (auto r = set(session_, attr::MODE, mode, ts); !r) return r;
            return ext(entry, type);
        }
        if (type == "title_change") {
            if (auto title = str_field(json, "title"))
                if (auto r = set(session_, attr::TITLE, *title, ts); !r) return r;
            return ext(entry, type);
        }
        return ext(entry, type);
    }

    // Keeps an entry as `<ext type>` under the open turn, or the session.
    Result<> ext(const Line& entry, std::string_view kind) {
        NodeId parent = session_;
        if (turn_) {
            turn_->last_ts = entry.ts;
            parent = turn_->node;
        }
        auto node = insert(parent, ElementKind::Ext, ext_attrs(kind, entry), entry.raw, entry.ts);
        if (!node) return std::unexpected(std::move(node.error()));
        return {};
    }

    Result<> close_turn() {
        if (!turn_) return {};
        OpenTurn open = std::move(*turn_);
        turn_.reset();
        if (auto r = set(open.node, attr::STATUS, open.status, open.last_ts); !r) return r;
        return set(open.node, attr::ENDED, open.last_ts, open.last_ts);
    }

  private:
    Result<> message(const Line& entry) {
        const json::Value* raw = get(entry.json, "message");
        const json::Object* message = raw ? raw->as_object() : nullptr;
        if (!message) return ext(entry, "message");
        const std::string_view role = str_field(*message, "role").value_or("");
        if (role == "user") {
            const json::Value* synthetic = get(*message, "synthetic");
            const bool is_synthetic = synthetic && synthetic->as_bool() && *synthetic->as_bool();
            if (!is_synthetic && str_field(*message, "attribution") != "agent") return user(entry, *message);
        } else if (role == "assistant") {
            return assistant(entry, *message);
        } else if (role == "toolResult") {
            return tool_result(entry, *message);
        }
        return ext(entry, std::format("message:{}", role));
    }

    Result<> user(const Line& entry, const json::Object& message) {
        if (auto r = close_turn(); !r) return r;
        const std::int64_t ts = entry.ts;
        const auto id = str_field(entry.json, "id");
        Attrs turn_attrs;
        if (id) put(turn_attrs, attr::ID, *id);
        put(turn_attrs, attr::STATUS, TurnStatus::Running);
        put(turn_attrs, attr::STARTED, ts);
        auto turn = insert(session_, ElementKind::Turn, std::move(turn_attrs), "", ts);
        if (!turn) return std::unexpected(std::move(turn.error()));
        turns_.push_back(*turn);
        turn_ = OpenTurn{*turn, ts, TurnStatus::Done, {}};

        Attrs attrs;
        put(attrs, attr::ROLE, Role::User);
        put(attrs, attr::STATUS, MessageStatus::Done);
        if (id) put(attrs, attr::ID, *id);
        mark(attrs, entry);
        auto node = insert(*turn, ElementKind::Message, std::move(attrs), content_text(get(message, "content")), ts);
        if (!node) return std::unexpected(std::move(node.error()));
        return {};
    }

    Result<> assistant(const Line& entry, const json::Object& message) {
        const std::int64_t ts = entry.ts;
        auto opened = open_turn(ts);
        if (!opened) return std::unexpected(std::move(opened.error()));
        const NodeId turn = *opened;
        std::optional<std::string> model;
        const auto provider_field = str_field(message, "provider");
        const auto model_field = str_field(message, "model");
        if (provider_field && model_field) model = std::format("{}/{}", *provider_field, *model_field);
        else if (model_field) model = std::string(*model_field);
        else if (provider_field) model = std::string(*provider_field);

        bool marked = false;
        std::optional<NodeId> last_message;
        const json::Value* content = get(message, "content");
        const json::Array* blocks = content ? content->as_array() : nullptr;
        const json::Array empty_blocks;
        for (const json::Value& value : blocks ? *blocks : empty_blocks) {
            const json::Object* block = value.as_object();
            if (!block) continue;
            const auto kind = str_field(*block, "type");
            if (kind == "text" || kind == "thinking") {
                const bool thinking = kind == "thinking";
                Attrs attrs;
                put(attrs, attr::ROLE, Role::Assistant);
                put(attrs, attr::STATUS, MessageStatus::Done);
                if (thinking) put(attrs, attr::KIND, MessageKind::Thinking);
                if (model) put(attrs, attr::MODEL, *model);
                if (!marked) {
                    mark(attrs, entry);
                    marked = true;
                }
                const std::string text(str_field(*block, thinking ? "thinking" : "text").value_or(""));
                auto node = insert(turn, ElementKind::Message, std::move(attrs), text, ts);
                if (!node) return std::unexpected(std::move(node.error()));
                last_message = *node;
            } else if (kind == "toolCall") {
                const auto call_id = str_field(*block, "id");
                Attrs attrs;
                put(attrs, attr::TOOL, str_field(*block, "name").value_or(""));
                put(attrs, attr::STATUS, CallStatus::Running);
                put(attrs, attr::STARTED, ts);
                if (call_id) put(attrs, attr::CALL_ID, *call_id);
                if (auto intent = str_field(*block, "intent")) put(attrs, attr::TITLE, *intent);
                if (!marked) {
                    mark(attrs, entry);
                    marked = true;
                }
                auto call = insert(turn, ElementKind::Call, std::move(attrs), "", ts);
                if (!call) return std::unexpected(std::move(call.error()));
                const json::Value* arguments = get(*block, "arguments");
                std::string input = arguments ? detail::to_line(*arguments) : "{}";
                Attrs input_attrs;
                put(input_attrs, attr::FORMAT, "json");
                if (auto r = insert(*call, ElementKind::Input, std::move(input_attrs), std::move(input), ts); !r)
                    return std::unexpected(std::move(r.error()));
                if (call_id && turn_) turn_->calls.insert_or_assign(std::string(*call_id), *call);
            }
        }
        if (!marked)
            if (auto r = ext(entry, "message"); !r) return r;
        const json::Value* usage_value = get(message, "usage");
        if (const json::Object* usage = usage_value ? usage_value->as_object() : nullptr) {
            const NodeId target = last_message.value_or(turn);
            Attrs attrs;
            const std::pair<std::string_view, std::string_view> counts[] = {
                {attr::INPUT_TOKENS, "input"},
                {attr::OUTPUT_TOKENS, "output"},
                {attr::CACHE_READ_TOKENS, "cacheRead"},
                {attr::CACHE_WRITE_TOKENS, "cacheWrite"},
            };
            for (const auto& [name, key] : counts) {
                const json::Value* count = get(*usage, key);
                if (auto n = count ? count->as_i64() : std::nullopt) put(attrs, name, *n);
            }
            if (const json::Value* cost = get(*usage, "cost"))
                if (const json::Value* total = cost->find("total"))
                    if (auto value = total->as_f64()) put(attrs, attr::COST, *value);
            if (model) put(attrs, attr::MODEL, *model);
            if (auto r = insert(target, ElementKind::Usage, std::move(attrs), "", ts); !r)
                return std::unexpected(std::move(r.error()));
        }
        if (turn_) {
            turn_->last_ts = ts;
            const auto stop = str_field(message, "stopReason");
            turn_->status = stop == "error" ? TurnStatus::Error : stop == "aborted" ? TurnStatus::Cancelled : TurnStatus::Done;
        }
        return {};
    }

    Result<> tool_result(const Line& entry, const json::Object& message) {
        std::optional<NodeId> call;
        if (auto id = str_field(message, "toolCallId"); id && turn_)
            if (auto it = turn_->calls.find(std::string(*id)); it != turn_->calls.end()) call = it->second;
        if (!call) return ext(entry, "message");
        const std::int64_t ts = entry.ts;
        const json::Value* error = get(message, "isError");
        const bool is_error = error && error->as_bool() && *error->as_bool();
        Attrs attrs;
        put(attrs, attr::IS_ERROR, is_error);
        mark(attrs, entry);
        if (auto r = insert(*call, ElementKind::Result, std::move(attrs), content_text(get(message, "content")), ts); !r)
            return std::unexpected(std::move(r.error()));
        if (auto r = set(*call, attr::STATUS, is_error ? CallStatus::Error : CallStatus::Done, ts); !r) return r;
        if (auto r = set(*call, attr::ENDED, ts, ts); !r) return r;
        if (turn_) turn_->last_ts = ts;
        return {};
    }

    Result<> compaction(const Line& entry) {
        Value::List covers;
        covers.reserve(turns_.size());
        for (NodeId turn : turns_) covers.emplace_back(turn);
        Attrs attrs;
        put(attrs, attr::COVERS, std::move(covers));
        const json::Value* tokens_value = get(entry.json, "tokensBefore");
        if (auto tokens = tokens_value ? tokens_value->as_i64() : std::nullopt) put(attrs, attr::TOKENS_BEFORE, *tokens);
        put(attrs, attr::CREATED, entry.ts);
        mark(attrs, entry);
        const std::string summary(str_field(entry.json, "summary").value_or(""));
        auto node = insert(session_, ElementKind::Compaction, std::move(attrs), summary, entry.ts);
        if (!node) return std::unexpected(std::move(node.error()));
        return {};
    }

    // The open turn, starting one without a user message when none is open.
    Result<NodeId> open_turn(std::int64_t ts) {
        if (turn_) return turn_->node;
        Attrs attrs;
        put(attrs, attr::STATUS, TurnStatus::Running);
        put(attrs, attr::STARTED, ts);
        auto turn = insert(session_, ElementKind::Turn, std::move(attrs), "", ts);
        if (!turn) return turn;
        turns_.push_back(*turn);
        turn_ = OpenTurn{*turn, ts, TurnStatus::Done, {}};
        return *turn;
    }

    loa::core::PatchSink& sink_;
    NodeId session_;
    std::optional<OpenTurn> turn_;
    // Every `<turn>` inserted so far, in order.
    std::vector<NodeId> turns_;
};

} // namespace

std::string content_text(const json::Value* content) {
    if (!content) return {};
    if (const std::string* text = content->as_string()) return *text;
    const json::Array* blocks = content->as_array();
    if (!blocks) return {};
    std::string out;
    bool first = true;
    for (const json::Value& block : *blocks) {
        const json::Value* type = block.find("type");
        if (!type || !type->as_string() || *type->as_string() != "text") continue;
        const json::Value* text = block.find("text");
        if (!text || !text->as_string()) continue;
        if (!first) out += '\n';
        out += *text->as_string();
        first = false;
    }
    return out;
}

namespace detail {

Result<> run_import(std::string_view text, loa::core::PatchSink& sink, std::string_view node) {
    std::vector<Line> lines = parse_lines(text);
    const auto no_header = [] { return std::unexpected(AdapterError(AdapterError::Format{"the file has no session header"})); };
    std::size_t next = 0;
    if (next == lines.size()) return no_header();
    std::optional<Line> title_slot;
    if (str_field(lines[next].json, "type") == "title") {
        title_slot = std::move(lines[next++]);
        if (next == lines.size()) return no_header();
    }
    Line header = std::move(lines[next++]);
    if (str_field(header.json, "type") != "session" || !str_field(header.json, "id"))
        return std::unexpected(AdapterError::Format{std::format("line {} isn't a session header", header.line)});
    std::vector<Line> entries(std::make_move_iterator(lines.begin() + static_cast<std::ptrdiff_t>(next)),
                              std::make_move_iterator(lines.end()));

    migrate(header, entries);
    if (header.changed) header.raw = to_line(header.json);
    for (Line& line : entries)
        if (line.changed) line.raw = to_line(line.json);

    const auto timestamp = [](const Line& line) -> std::optional<std::int64_t> {
        auto stamp = str_field(line.json, "timestamp");
        return stamp ? loa::runtime::time::parse_iso_ms(*stamp) : std::nullopt;
    };
    header.ts = timestamp(header).value_or(0);
    std::int64_t previous = header.ts;
    for (Line& entry : entries) {
        entry.ts = timestamp(entry).value_or(previous);
        previous = entry.ts;
    }

    std::optional<std::string_view> title;
    if (title_slot) title = str_field(title_slot->json, "title");
    if (!title) title = str_field(header.json, "title");
    const std::string_view id = str_field(header.json, "id").value_or("");
    Attrs attrs;
    put(attrs, attr::ID, id);
    put(attrs, attr::HARNESS, HARNESS);
    put(attrs, attr::NODE, node);
    put(attrs, attr::CREATED, header.ts);
    put(attrs, attr::SCHEMA, static_cast<std::int64_t>(loa::core::SCHEMA_VERSION));
    put(attrs, attr::NATIVE_ID, id);
    if (auto cwd = str_field(header.json, "cwd")) put(attrs, attr::CWD, *cwd);
    if (title) put(attrs, attr::TITLE, *title);
    put(attrs, X_ENTRY, header.raw);
    put(attrs, X_LINE, header.line);
    auto root = sink.commit_at(Draft::insert(NodeId::DOCUMENT, ElementKind::Session, std::move(attrs), ""), header.ts);
    if (!root) return std::unexpected(commit_error(std::move(root.error())));
    const NodeId session = root->node;

    Importer importer(sink, session);
    if (title_slot)
        if (auto r = importer.insert(session, ElementKind::Ext, ext_attrs("title", *title_slot), title_slot->raw, header.ts); !r)
            return std::unexpected(std::move(r.error()));

    std::vector<bool> on_path(entries.size(), false);
    for (std::size_t index : active_path(entries)) {
        on_path[index] = true;
        if (auto r = importer.entry(entries[index]); !r) return r;
    }
    if (auto r = importer.close_turn(); !r) return r;
    for (std::size_t index = 0; index < entries.size(); ++index) {
        if (on_path[index]) continue;
        const Line& entry = entries[index];
        if (auto r = importer.insert(session, ElementKind::Ext, ext_attrs("branch", entry), entry.raw, entry.ts); !r)
            return std::unexpected(std::move(r.error()));
    }
    return {};
}

} // namespace detail

} // namespace loa::harness::omp::native
