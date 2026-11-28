#include "loa/harness/claude/native.hpp"

#include <algorithm>
#include <array>
#include <cctype>
#include <fstream>
#include <iterator>
#include <map>
#include <random>
#include <set>
#include <unordered_map>
#include <unordered_set>

#include "loa/core/writer.hpp"
#include "loa/harness/kit/residual.hpp"
#include "loa/runtime/json.hpp"
#include "loa/runtime/time.hpp"

namespace loa::harness::claude::native {
namespace fs = std::filesystem;
namespace j = loa::runtime::json;
using J = j::Value;
using O = j::Object;
using A = j::Array;
using namespace loa::core;
using loa::contracts::AdapterError;
using loa::contracts::Result;

namespace {
constexpr std::string_view HARNESS = "claude-code";
constexpr std::string_view X_ENVELOPE = "x-claude", X_BLOCK = "x-claude-block", X_LINE = "x-claude-line";
constexpr std::string_view X_PART = "x-claude-part", X_CONTENT = "x-claude-content", X_SPLIT = "x-claude-split";
constexpr std::string_view X_FILE = "x-claude-file", X_PATH = "x-path";

const J* field(const J* value, std::string_view name) { return value ? value->find(name) : nullptr; }
std::optional<std::string> string(const J* value) {
    if (value) if (auto* s = value->as_string()) return *s;
    return std::nullopt;
}
std::optional<std::string> str_field(const J* value, std::string_view name) { return string(field(value, name)); }
std::optional<std::int64_t> integer(const J* value) { return value ? value->as_i64() : std::nullopt; }
bool eq(const J* value, std::string_view s) { return value && value->as_string() && *value->as_string() == s; }
bool flag(const J* value) { return value && value->as_bool() && *value->as_bool(); }
void erase_key(O& obj, std::string_view key) { std::erase_if(obj, [&](const auto& p) { return p.first == key; }); }
void object_set(O& obj, std::string_view key, J value) {
    for (auto& [name, item] : obj) if (name == key) { item = std::move(value); return; }
    obj.emplace_back(key, std::move(value));
}
void sort_json(J& value) {
    if (auto* object = value.as_object()) {
        for (auto& [_, entry] : *object) sort_json(entry);
        std::ranges::sort(*object, {}, [](const auto& p) -> const std::string& { return p.first; });
    } else if (auto* array = value.as_array()) for (auto& item : *array) sort_json(item);
}
std::string sorted(J value) { sort_json(value); return j::dump(value); }
std::string leftover(const J& block, std::initializer_list<std::string_view> fields) {
    if (auto* o = block.as_object()) { auto copy = *o; return sorted(J(kit::residual::take(copy, std::span<const std::string_view>(fields.begin(), fields.size())))); }
    return sorted(block);
}
J parse_or_string(std::string_view text) { auto value = j::parse(text); return value ? std::move(*value) : J(text); }
std::string read_file(const fs::path& path) {
    std::ifstream in(path, std::ios::binary);
    if (!in) throw AdapterError(AdapterError::Io{loa::Error(loa::ErrorCode::io, "cannot read " + path.string())});
    std::string text(std::istreambuf_iterator<char>{in}, std::istreambuf_iterator<char>{});
    if (in.bad()) throw AdapterError(AdapterError::Io{loa::Error(loa::ErrorCode::io, "cannot read " + path.string())});
    return text;
}
void write_file(const fs::path& path, std::string_view bytes) {
    std::error_code ec;
    fs::create_directories(path.parent_path(), ec);
    if (ec) throw AdapterError(AdapterError::Io{loa::Error(loa::ErrorCode::io, ec.message())});
    std::ofstream out(path, std::ios::binary | std::ios::trunc);
    out.write(bytes.data(), static_cast<std::streamsize>(bytes.size()));
    if (!out) throw AdapterError(AdapterError::Io{loa::Error(loa::ErrorCode::io, "cannot write " + path.string())});
}
std::optional<fs::path> side_dir(const fs::path& file) {
    if (!file.has_extension()) return std::nullopt;
    fs::path side = file; side.replace_extension(); return side;
}
std::string uuid(bool simple = false) {
    static thread_local std::mt19937_64 random(std::random_device{}());
    std::array<unsigned char, 16> b{};
    for (unsigned i = 0; i < 16; i += 8) { auto bits = random(); for (unsigned n = 0; n < 8; ++n) b[i+n] = static_cast<unsigned char>(bits >> (n*8)); }
    b[6] = (b[6] & 0x0f) | 0x40; b[8] = (b[8] & 0x3f) | 0x80;
    constexpr char hex[] = "0123456789abcdef";
    std::string out; out.reserve(simple ? 32 : 36);
    for (unsigned i = 0; i < 16; ++i) { if (!simple && (i == 4 || i == 6 || i == 8 || i == 10)) out += '-'; out += hex[b[i] >> 4]; out += hex[b[i] & 15]; }
    return out;
}
template <typename T> T checked(std::expected<T, CommitError> result) {
    if (!result) throw AdapterError(AdapterError::Commit{result.error()});
    return std::move(*result);
}
void checked(std::expected<void, CommitError> result) {
    if (!result) throw AdapterError(AdapterError::Commit{result.error()});
}
struct Encoding { std::string text; Attrs attrs; std::optional<A> items; };
Encoding encode(const J* content, bool format) {
    Encoding out;
    std::string_view kind = "absent";
    if (content) {
        if (auto* s = content->as_string()) { out.text = *s; kind = "string"; }
        else if (auto* blocks = content->as_array(); blocks && !blocks->empty() &&
                 std::ranges::all_of(*blocks, [](const J& b) { return eq(field(&b, "type"), "text") && field(&b, "text") && field(&b, "text")->is_string(); })) {
            std::vector<std::string_view> parts;
            out.items.emplace();
            for (const auto& b : *blocks) {
                parts.push_back(*field(&b, "text")->as_string());
                auto copy = *b.as_object();
                out.items->emplace_back(kit::residual::take(copy, std::array<std::string_view, 1>{"text"}));
            }
            auto joined = kit::residual::join_parts(parts, "\n");
            out.text = std::move(joined.first);
            out.attrs.emplace(X_CONTENT, "texts"); out.attrs.emplace(X_SPLIT, Value(std::move(joined.second)));
            return out;
        } else { out.text = j::dump(*content); kind = "json"; }
    }
    out.attrs.emplace(X_CONTENT, kind);
    if (format && kind == "json") out.attrs.emplace(attr::FORMAT, "json");
    return out;
}
struct Origin {
    std::int64_t index;
    std::string envelope;
    Origin(std::int64_t i, O line) : index(i) {
        for (auto& [key, entry] : line) if (key == "message") if (auto* message = entry.as_object()) erase_key(*message, "content");
        envelope = sorted(J(std::move(line)));
    }
    Attrs attrs(std::optional<std::size_t> part) const {
        Attrs attrs{{std::string(X_LINE), index}};
        if (part) attrs.emplace(X_PART, static_cast<std::int64_t>(*part));
        if (part.value_or(0) == 0) attrs.emplace(X_ENVELOPE, envelope);
        return attrs;
    }
};
void merge(Attrs& dst, Attrs src) { dst.merge(src); }
Attrs message_attrs(Role role, std::optional<std::string_view> id) {
    Attrs a{{std::string(attr::ROLE), role}, {std::string(attr::STATUS), MessageStatus::Done}};
    if (id) a.emplace(attr::ID, *id);
    return a;
}
std::optional<std::string_view> block_text(const J& block, std::string_view kind) {
    if (eq(field(&block, "type"), kind)) if (auto* text = field(&block, kind)) if (auto* s = text->as_string()) return *s;
    return std::nullopt;
}
std::optional<std::string> call_title(const J* input) {
    if (!input || !input->is_object()) return std::nullopt;
    constexpr std::string_view keys[]{"description", "command", "file_path", "notebook_path", "path", "pattern", "url", "query", "prompt"};
    for (auto key : keys) if (auto text = str_field(input, key)) {
        if (text->find_first_not_of(" \t\r\n") == std::string::npos) continue;
        std::string_view line(*text);
        while (!line.empty()) {
            auto end = line.find('\n'); auto first = line.substr(0, end);
            auto begin = first.find_first_not_of(" \t\r\n");
            if (begin != first.npos) {
                first.remove_prefix(begin); first = first.substr(0, first.find_last_not_of(" \t\r\n") + 1);
                return std::string(first.substr(0, 120));
            }
            if (end == line.npos) break;
            line.remove_prefix(end+1);
        }
        return std::string{};
    }
    return std::nullopt;
}
struct Subagent { std::string id; std::optional<std::string> type, description; };
struct CallState { NodeId node; std::int64_t started; bool answered = false; std::optional<NodeId> job; std::optional<J> input; };
struct Line { std::string text; std::optional<J> json; };

class Importer {
    PatchSink& sink;
    NodeId root;
    bool subagent_file = false;
    std::unordered_map<std::string, Subagent> subagents;
    std::unordered_set<std::string> side_paths;
    std::int64_t ts = 0;
    std::optional<std::pair<NodeId, std::int64_t>> turn;
    Value::List uncompacted;
    std::vector<CallState> calls;
    std::unordered_map<std::string, std::size_t> call_index;
    std::unordered_set<std::string> messages;
    std::optional<std::string> model, custom_title, summary;
    std::optional<std::int64_t> pre_tokens;
    DomWriter w() { return DomWriter(sink).at(ts); }
    NodeId ext_parent() const { return turn ? turn->first : root; }
    void close_turn() {
        if (turn) { checked(w().set(turn->first, attr::ENDED, turn->second)); turn.reset(); }
    }
    NodeId open_turn(std::optional<std::string_view> id) {
        close_turn(); Attrs attrs{{std::string(attr::STATUS), TurnStatus::Done}, {std::string(attr::STARTED), ts}};
        if (id) attrs.emplace(attr::ID, *id);
        auto node = checked(w().insert(root, ElementKind::Turn, std::move(attrs)));
        uncompacted.emplace_back(node); turn = {{node, ts}}; return node;
    }
    NodeId current_turn() { return turn ? turn->first : open_turn(std::nullopt); }
    void line_ext(std::int64_t i, std::string_view text, std::optional<std::string_view> type = {}) {
        Attrs attrs{{std::string(attr::HARNESS), HARNESS}, {std::string(X_LINE), i}};
        if (type) attrs.emplace(attr::TYPE, *type);
        checked(w().insert(ext_parent(), ElementKind::Ext, std::move(attrs), std::string(text)));
    }
    void block_ext(NodeId parent, const Origin& origin, std::size_t part, const J& block) {
        Attrs attrs{{std::string(attr::HARNESS), HARNESS}, {std::string(attr::TYPE), "block"}};
        merge(attrs, origin.attrs(part));
        checked(w().insert(parent, ElementKind::Ext, std::move(attrs), j::dump(block)));
    }
    NodeId text_message(NodeId parent, Role role, std::optional<std::string_view> id, const Origin& origin,
                        std::optional<std::size_t> part, const J& block, std::string_view text) {
        auto attrs = message_attrs(role, id);
        if (part) attrs.emplace(X_BLOCK, leftover(block, {"text"}));
        merge(attrs, origin.attrs(part));
        return checked(w().insert(parent, ElementKind::Message, std::move(attrs), std::string(text)));
    }
    Attrs job_attrs(std::string_view id, NodeId call, const J* input, std::optional<std::string> agent, std::optional<std::string> title) {
        Attrs a{{std::string(attr::KIND), JobKind::Subagent}, {std::string(attr::ID), id}, {std::string(attr::CALL), call}};
        if (!agent) agent = str_field(input, "subagent_type");
        if (!title) title = str_field(input, "description");
        if (agent) a.emplace(attr::AGENT, *agent);
        if (title) a.emplace(attr::TITLE, *title);
        const std::string transcript = "subagents/agent-" + std::string(id) + ".jsonl";
        if (side_paths.contains(transcript)) a.emplace(X_FILE, transcript);
        return a;
    }
    void compaction(const Origin& origin, const J* content) {
        close_turn();
        auto enc = encode(content, false); auto attrs = std::move(enc.attrs);
        attrs.emplace(attr::COVERS, Value(std::move(uncompacted))); uncompacted.clear();
        attrs.emplace(attr::CREATED, ts);
        if (pre_tokens) { attrs.emplace(attr::TOKENS_BEFORE, *pre_tokens); pre_tokens.reset(); }
        if (enc.items) attrs.emplace(X_BLOCK, j::dump(J(O{{"content", J(std::move(*enc.items))}})));
        merge(attrs, origin.attrs(std::nullopt));
        checked(w().insert(root, ElementKind::Compaction, std::move(attrs), std::move(enc.text)));
    }
    void result(const Origin& origin, std::size_t part, const J& block, std::size_t index, const J* agent) {
        auto enc = encode(field(&block, "content"), true);
        auto error = field(&block, "is_error"); auto is_error = error ? error->as_bool() : nullptr;
        auto copy = *block.as_object();
        auto rest = is_error ? kit::residual::take(copy, std::array<std::string_view, 3>{"tool_use_id", "content", "is_error"})
                             : kit::residual::take(copy, std::array<std::string_view, 2>{"tool_use_id", "content"});
        if (enc.items) object_set(rest, "content", J(std::move(*enc.items)));
        auto attrs = std::move(enc.attrs);
        if (is_error) attrs.emplace(attr::IS_ERROR, *is_error);
        attrs.emplace(X_BLOCK, sorted(J(std::move(rest)))); merge(attrs, origin.attrs(part));
        const auto& state = calls[index];
        checked(w().insert(state.node, ElementKind::Result, std::move(attrs), std::move(enc.text)));
        bool failed = is_error && *is_error;
        checked(w().close(state.node, Value(failed ? CallStatus::Error : CallStatus::Done)));
        auto job = state.job;
        if (job) checked(w().close(*job, Value(failed ? JobStatus::Error : JobStatus::Done)));
        else if (auto id = str_field(agent, "agentId")) {
            auto attrs = job_attrs(*id, state.node, state.input ? &*state.input : nullptr, str_field(agent, "agentType"), {});
            attrs.emplace(attr::STATUS, failed ? JobStatus::Error : JobStatus::Done);
            attrs.emplace(attr::STARTED, state.started); attrs.emplace(attr::ENDED, ts);
            job = checked(w().insert(state.node, ElementKind::Job, std::move(attrs)));
        }
        calls[index].answered = true; calls[index].job = job;
    }
    void results(const Origin& origin, std::optional<std::string_view> id, const J& object, const A& blocks) {
        auto count = std::ranges::count_if(blocks, [](const J& b) { return eq(field(&b, "type"), "tool_result"); });
        const J* agent = count == 1 ? field(&object, "toolUseResult") : nullptr;
        for (std::size_t part = 0; part < blocks.size(); ++part) {
            const auto& block = blocks[part];
            auto call = eq(field(&block, "type"), "tool_result") ? str_field(&block, "tool_use_id") : std::nullopt;
            auto found = call ? call_index.find(*call) : call_index.end();
            if (found != call_index.end()) result(origin, part, block, found->second, agent);
            else if (auto text = block_text(block, "text")) text_message(current_turn(), Role::User, id, origin, part, block, *text);
            else block_ext(ext_parent(), origin, part, block);
        }
    }
    void user(std::int64_t i, std::string_view raw, const J& object) {
        auto content = field(field(&object, "message"), "content");
        const A* blocks = content ? content->as_array() : nullptr;
        if ((!content || (!content->is_string() && (!blocks || blocks->empty())))) { line_ext(i, raw, "user"); return; }
        auto id = str_field(&object, "uuid"); Origin origin(i, *object.as_object());
        if (flag(field(&object, "isCompactSummary"))) { compaction(origin, content); return; }
        if (blocks && std::ranges::any_of(*blocks, [](const J& b) { return eq(field(&b, "type"), "tool_result"); })) {
            results(origin, id, object, *blocks); return;
        }
        bool meta = flag(field(&object, "isMeta"));
        auto parent = meta ? current_turn() : open_turn(id);
        auto role = meta ? Role::System : Role::User;
        if (auto* text = content->as_string()) text_message(parent, role, id, origin, {}, J{}, *text);
        else for (std::size_t part = 0; part < blocks->size(); ++part) {
            const auto& block = (*blocks)[part];
            if (auto text = block_text(block, "text")) text_message(parent, role, id, origin, part, block, *text);
            else block_ext(parent, origin, part, block);
        }
    }
    NodeId call(NodeId parent, const Origin& origin, std::size_t part, const J& block, std::string_view id, std::string_view name) {
        auto input = field(&block, "input");
        Attrs attrs{{std::string(attr::TOOL), name}, {std::string(attr::CALL_ID), id},
                    {std::string(attr::STATUS), CallStatus::Running}, {std::string(attr::STARTED), ts}};
        if (auto title = call_title(input)) attrs.emplace(attr::TITLE, *title);
        attrs.emplace(X_BLOCK, leftover(block, {"id", "name", "input"})); merge(attrs, origin.attrs(part));
        auto node = checked(w().insert(parent, ElementKind::Call, std::move(attrs)));
        if (input) checked(w().insert(node, ElementKind::Input, {{std::string(attr::FORMAT), "json"}}, j::dump(*input)));
        std::optional<NodeId> job;
        if (auto found = subagents.find(std::string(id)); found != subagents.end()) {
            auto attrs = job_attrs(found->second.id, node, input, found->second.type, found->second.description);
            attrs.emplace(attr::STATUS, JobStatus::Running); attrs.emplace(attr::STARTED, ts);
            job = checked(w().insert(node, ElementKind::Job, std::move(attrs)));
        }
        call_index[std::string(id)] = calls.size(); calls.push_back({node, ts, false, job, input ? std::optional<J>(*input) : std::nullopt});
        return node;
    }
    void assistant(std::int64_t i, std::string_view raw, const J& object) {
        auto message = field(&object, "message"); auto blocks = field(message, "content");
        auto array = blocks ? blocks->as_array() : nullptr;
        if (!array || array->empty()) { line_ext(i, raw, "assistant"); return; }
        auto parent = current_turn();
        auto current_model = str_field(message, "model"); if (current_model) model = *current_model;
        auto id = str_field(&object, "uuid"); Origin origin(i, *object.as_object());
        auto msg_id = str_field(message, "id"); bool first = !msg_id || messages.insert(*msg_id).second;
        NodeId usage_parent = parent;
        for (std::size_t part = 0; part < array->size(); ++part) {
            const auto& block = (*array)[part];
            auto kind = str_field(&block, "type");
            auto text = kind && (*kind == "text" || *kind == "thinking") ? block_text(block, *kind) : std::nullopt;
            NodeId node;
            if (text) {
                auto attrs = message_attrs(Role::Assistant, id);
                if (*kind == "thinking") attrs.emplace(attr::KIND, MessageKind::Thinking);
                if (current_model) attrs.emplace(attr::MODEL, *current_model);
                attrs.emplace(X_BLOCK, leftover(block, {*kind})); merge(attrs, origin.attrs(part));
                node = checked(w().insert(parent, ElementKind::Message, std::move(attrs), std::string(*text)));
            } else if (eq(field(&block, "type"), "tool_use") && str_field(&block, "id") && str_field(&block, "name")) {
                node = call(parent, origin, part, block, *str_field(&block, "id"), *str_field(&block, "name"));
            } else { block_ext(parent, origin, part, block); continue; }
            if (part == 0) usage_parent = node;
        }
        if (first) if (auto* usage = field(message, "usage")) if (usage->is_object()) {
            Attrs attrs;
            constexpr std::pair<std::string_view, std::string_view> names[] = {
                {attr::INPUT_TOKENS, "input_tokens"}, {attr::OUTPUT_TOKENS, "output_tokens"},
                {attr::CACHE_READ_TOKENS, "cache_read_input_tokens"}, {attr::CACHE_WRITE_TOKENS, "cache_creation_input_tokens"}};
            for (auto [name, key] : names) if (auto value = integer(field(usage, key))) attrs.emplace(name, *value);
            if (current_model) attrs.emplace(attr::MODEL, *current_model);
            checked(w().insert(usage_parent, ElementKind::Usage, std::move(attrs)));
        }
    }
  public:
    Importer(PatchSink& target, const std::vector<Line>& lines, const fs::path& path,
             const std::vector<std::pair<std::string, std::string>>& files) : sink(target), root(NodeId::DOCUMENT) {
        auto first_str = [&](std::string_view name) -> std::optional<std::string> {
            for (const auto& line : lines) if (auto text = str_field(line.json ? &*line.json : nullptr, name)) return text;
            return std::nullopt;
        };
        auto session = first_str("sessionId"); std::int64_t created = 0;
        for (const auto& line : lines) if (auto stamp = str_field(line.json ? &*line.json : nullptr, "timestamp"))
            if (auto parsed = loa::runtime::time::parse_iso_ms(*stamp)) { created = *parsed; break; }
        bool has_conversation = false; subagent_file = true;
        for (const auto& line : lines) if (line.json && (eq(field(&*line.json, "type"), "user") || eq(field(&*line.json, "type"), "assistant"))) {
            has_conversation = true;
            if (!flag(field(&*line.json, "isSidechain"))) subagent_file = false;
        }
        subagent_file &= has_conversation;
        Attrs attrs{{std::string(attr::ID), session.value_or(path.stem().string())}, {std::string(attr::HARNESS), HARNESS},
                    {std::string(attr::NODE), ""}, {std::string(attr::CREATED), created},
                    {std::string(attr::SCHEMA), static_cast<std::int64_t>(SCHEMA_VERSION)}};
        if (session) attrs.emplace(attr::NATIVE_ID, *session);
        if (auto cwd = first_str("cwd")) attrs.emplace(attr::CWD, *cwd);
        root = checked(DomWriter(sink).at(created).insert(NodeId::DOCUMENT, ElementKind::Session, std::move(attrs)));
        for (const auto& [file, bytes] : files) {
            side_paths.insert(file);
            if (file.starts_with("subagents/agent-") && file.ends_with(".meta.json")) {
                auto parsed = j::parse(bytes); if (!parsed) continue;
                auto id = str_field(&*parsed, "toolUseId"); if (!id) continue;
                subagents[*id] = {file.substr(16, file.size()-16-10), str_field(&*parsed, "agentType"), str_field(&*parsed, "description")};
            }
        }
    }
    void line(std::int64_t i, const Line& line) {
        const J* value = line.json ? &*line.json : nullptr;
        if (auto stamp = str_field(value, "timestamp")) if (auto parsed = loa::runtime::time::parse_iso_ms(*stamp)) ts = *parsed;
        if (!value || !value->is_object()) { line_ext(i, line.text); return; }
        auto type = str_field(value, "type"); bool own = subagent_file || !flag(field(value, "isSidechain"));
        if (own && type == "user") user(i, line.text, *value);
        else if (own && type == "assistant") assistant(i, line.text, *value);
        else {
            if (type == "custom-title") { if (auto title = str_field(value, "customTitle")) custom_title = *title; }
            if (type == "summary") { if (auto title = str_field(value, "summary")) summary = *title; }
            if (type == "system" && eq(field(value, "subtype"), "compact_boundary")) pre_tokens = integer(field(field(value, "compactMetadata"), "preTokens"));
            line_ext(i, line.text, type);
        }
        if (turn) turn->second = ts;
    }
    void finish(const std::vector<std::pair<std::string, std::string>>& files) {
        close_turn();
        for (const auto& state : calls) if (!state.answered) {
            checked(w().set(state.node, attr::STATUS, Value(CallStatus::Cancelled)));
            if (state.job) checked(w().set(*state.job, attr::STATUS, Value(JobStatus::Cancelled)));
        }
        for (const auto& [path, bytes] : files) {
            Attrs attrs{{std::string(attr::HARNESS), HARNESS}, {std::string(attr::TYPE), "file"}, {std::string(X_PATH), path}};
            std::string text = bytes;
            // Rust String::from_utf8 rejects malformed UTF-8; keep raw bytes instead.
            auto valid = [](std::string_view s) {
                for (std::size_t i = 0; i < s.size();) {
                    unsigned char c = s[i]; std::size_t n = c < 0x80 ? 1 : c >= 0xC2 && c <= 0xDF ? 2 : c >= 0xE0 && c <= 0xEF ? 3 : c >= 0xF0 && c <= 0xF4 ? 4 : 0;
                    if (!n || i+n > s.size()) return false;
                    if (n == 3 && ((c == 0xE0 && static_cast<unsigned char>(s[i+1]) < 0xA0) || (c == 0xED && static_cast<unsigned char>(s[i+1]) >= 0xA0))) return false;
                    if (n == 4 && ((c == 0xF0 && static_cast<unsigned char>(s[i+1]) < 0x90) || (c == 0xF4 && static_cast<unsigned char>(s[i+1]) >= 0x90))) return false;
                    i += n;
                }
                return true;
            };
            if (!valid(text)) { attrs.emplace(attr::RAW, Value::Bytes(bytes.begin(), bytes.end())); text.clear(); }
            checked(w().insert(root, ElementKind::Ext, std::move(attrs), std::move(text)));
        }
        if (model) checked(w().set(root, attr::MODEL, *model));
        if (custom_title) checked(w().set(root, attr::TITLE, *custom_title));
        else if (summary) checked(w().set(root, attr::TITLE, *summary));
    }
};

class Writer {
    const Dom& dom;
    const Node& root;
    std::string out;
    std::optional<std::string> last_uuid;
    std::unordered_map<NodeId, std::string> call_lines, call_ids;
    std::optional<std::pair<NodeId, std::string>> assistant;

    std::optional<std::string> call_id(NodeId id) const {
        if (auto* node = dom.get(id)) if (auto own = node->str_attr(attr::CALL_ID)) return std::string(*own);
        if (auto it = call_ids.find(id); it != call_ids.end()) return it->second;
        return {};
    }
    O block_residual(const Node& node, std::string_view kind) const {
        if (auto text = node.str_attr(X_BLOCK)) {
            auto parsed = j::parse(*text);
            if (parsed && parsed->is_object()) return *parsed->as_object();
        }
        return {{"type", J(kind)}};
    }
    std::optional<J> content(const Node& node, std::optional<J> items) const {
        auto kind = node.str_attr(X_CONTENT);
        if (kind == "absent") return {};
        if (kind == "json" || (!kind && node.str_attr(attr::FORMAT) == "json")) return parse_or_string(node.text);
        if (kind == "texts") {
            A blocks = items && items->as_array() ? *items->as_array() : A{};
            std::optional<std::vector<std::string_view>> pieces;
            if (auto* v = node.attr(X_SPLIT)) if (auto lengths = v->as_list())
                pieces = kit::residual::split_parts(node.text, "\n", *lengths);
            if (pieces && pieces->size() == blocks.size()) {
                for (std::size_t i = 0; i < blocks.size(); ++i)
                    blocks[i].set("text", J((*pieces)[i]));
                return J(std::move(blocks));
            }
            return J(A{J(O{{"type", J("text")}, {"text", J(node.text)}})});
        }
        return J(node.text);
    }
    O tool_use(const Node& node) const {
        auto block = block_residual(node, "tool_use");
        if (auto id = call_id(node.id)) object_set(block, "id", J(*id));
        object_set(block, "name", J(node.str_attr(attr::TOOL).value_or("")));
        auto inputs = dom.children_of_kind(node.id, ElementKind::Input);
        if (!inputs.empty()) object_set(block, "input", parse_or_string(inputs.front()->text));
        else if (!node.attr(X_LINE)) object_set(block, "input", J(O{}));
        return block;
    }
    O tool_result(const Node& node) const {
        auto block = block_residual(node, "tool_result");
        std::optional<J> items;
        for (const auto& [key, value] : block) if (key == "content") { items = value; break; }
        erase_key(block, "content");
        if (auto id = call_id(node.parent)) object_set(block, "tool_use_id", J(*id));
        if (auto value = content(node, std::move(items))) object_set(block, "content", std::move(*value));
        if (auto error = node.bool_attr(attr::IS_ERROR)) object_set(block, "is_error", J(*error));
        return block;
    }
    std::optional<J> whole(const Node& node) const {
        if (node.kind == ElementKind::Message) return J(node.text);
        if (node.kind == ElementKind::Compaction) {
            auto block = block_residual(node, "text");
            for (auto& [name, value] : block) if (name == "content") return content(node, std::move(value));
            return content(node, {});
        }
        return block(node);
    }
    J block(const Node& node) const {
        if (node.kind == ElementKind::Message) {
            auto kind = node.str_attr(attr::KIND) == "thinking" ? "thinking" : "text";
            auto result = block_residual(node, kind);
            object_set(result, kind, J(node.text));
            return J(std::move(result));
        }
        if (node.kind == ElementKind::Call) return J(tool_use(node));
        if (node.kind == ElementKind::Result) return J(tool_result(node));
        if (node.kind == ElementKind::Ext) return parse_or_string(node.text);
        return J{};
    }
    void track(const J& line) {
        if (auto id = str_field(&line, "uuid"); id && field(&line, "parentUuid")) last_uuid = *id;
        if (eq(field(&line, "type"), "user")) assistant.reset();
    }
    void push(J line) {
        track(line);
        out += j::dump(line); out += '\n';
    }
    std::int64_t fresh_ts(const Node& node) const {
        if (node.kind == ElementKind::Result)
            if (auto* parent = dom.get(node.parent)) if (auto ended = parent->int_attr(attr::ENDED)) return *ended;
        if (auto started = node.int_attr(attr::STARTED)) return *started;
        if (auto created = node.int_attr(attr::CREATED)) return *created;
        for (auto* ancestor : dom.ancestors(node.id)) {
            if (auto started = ancestor->int_attr(attr::STARTED)) return *started;
            if (auto created = ancestor->int_attr(attr::CREATED)) return *created;
        }
        return 0;
    }
    J assistant_message(const Node& node, O item) {
        std::string id;
        if (assistant && assistant->first == node.parent) id = assistant->second;
        else id = "msg_" + uuid(true);
        assistant = {{node.parent, id}};
        O message{{"id", J(id)}, {"type", J("message")}, {"role", J("assistant")}};
        if (auto model = node.str_attr(attr::MODEL).or_else([&] { return root.str_attr(attr::MODEL); }))
            object_set(message, "model", J(*model));
        object_set(message, "content", J(A{J(std::move(item))}));
        return J(std::move(message));
    }
    void fresh(const Node& node) {
        std::string id = uuid();
        O extra;
        std::string_view type;
        J message;
        if (node.kind == ElementKind::Message) {
            if (node.str_attr(attr::ROLE) == "assistant") {
                type = "assistant";
                message = assistant_message(node, *block(node).as_object());
            } else {
                if (node.str_attr(attr::ROLE) == "system") object_set(extra, "isMeta", J(true));
                type = "user"; message = J(O{{"role", J("user")}, {"content", J(node.text)}});
            }
        } else if (node.kind == ElementKind::Call) {
            if (!node.str_attr(attr::CALL_ID)) call_ids[node.id] = "toolu_" + uuid(true);
            call_lines[node.id] = id;
            type = "assistant"; message = assistant_message(node, tool_use(node));
        } else if (node.kind == ElementKind::Result) {
            if (auto it = call_lines.find(node.parent); it != call_lines.end()) object_set(extra, "sourceToolAssistantUUID", J(it->second));
            type = "user";
            message = J(O{{"role", J("user")}, {"content", J(A{J(tool_result(node))})}});
        } else if (node.kind == ElementKind::Compaction) {
            object_set(extra, "isCompactSummary", J(true));
            type = "user"; message = J(O{{"role", J("user")}, {"content", J(node.text)}});
        } else return;
        auto session = root.str_attr(attr::NATIVE_ID).value_or(root.str_attr(attr::ID).value_or(""));
        O line{{"parentUuid", last_uuid ? J(*last_uuid) : J{}}, {"isSidechain", J(false)},
               {"userType", J("external")}, {"sessionId", J(session)}, {"type", J(type)},
               {"message", std::move(message)}, {"uuid", J(id)},
               {"timestamp", J(loa::runtime::time::format_iso_ms(fresh_ts(node)))}};
        if (auto cwd = root.str_attr(attr::CWD)) object_set(line, "cwd", J(*cwd));
        for (auto& [key, value] : extra) object_set(line, key, std::move(value));
        push(J(std::move(line)));
    }
    void group(const std::vector<const Node*>& nodes) {
        auto head = std::ranges::find_if(nodes, [](const Node* n) { return n->attr(X_ENVELOPE) != nullptr; });
        if (head == nodes.end()) {
            for (auto* node : nodes) {
                if (node->kind == ElementKind::Ext && !node->attr(X_PART)) {
                    if (auto parsed = j::parse(node->text)) track(*parsed);
                    out += node->text; out += '\n';
                } else if (node->kind != ElementKind::Ext) fresh(*node);
            }
            return;
        }
        auto parsed = j::parse(*(*head)->str_attr(X_ENVELOPE));
        if (!parsed || !parsed->is_object())
            throw AdapterError(AdapterError::Format{std::string(X_ENVELOPE) + " isn't a JSON object"});
        auto line = *parsed->as_object();
        std::optional<J> body;
        if (nodes.size() == 1 && !nodes.front()->attr(X_PART)) body = whole(*nodes.front());
        else {
            A blocks;
            for (auto* n : nodes) blocks.push_back(block(*n));
            body = J(std::move(blocks));
        }
        if (body) {
            bool has_message = false;
            for (auto& [name, value] : line) if (name == "message") {
                has_message = true;
                if (value.is_object()) value.set("content", std::move(*body));
                break;
            }
            if (!has_message) object_set(line, "message", J(O{{"content", std::move(*body)}}));
        }
        std::optional<std::string> line_id;
        for (const auto& [name, value] : line) if (name == "uuid") { line_id = string(&value); break; }
        if (line_id) for (auto* n : nodes) if (n->kind == ElementKind::Call) call_lines[n->id] = *line_id;
        push(J(std::move(line)));
    }
  public:
    Writer(const Dom& dom, const Node& root) : dom(dom), root(root) {}
    std::string run(std::map<std::int64_t, std::vector<std::pair<std::int64_t, const Node*>>>& groups,
                    const std::vector<const Node*>& fresh_nodes) {
        for (auto& [_, entries] : groups) {
            std::stable_sort(entries.begin(), entries.end(), [](const auto& a, const auto& b) { return a.first < b.first; });
            std::vector<const Node*> nodes; nodes.reserve(entries.size());
            for (auto& entry : entries) nodes.push_back(entry.second);
            group(nodes);
        }
        for (auto* node : fresh_nodes) fresh(*node);
        return std::move(out);
    }
};

} // namespace

Result<> import(const fs::path& path, PatchSink& sink) {
    try {
        const auto source = read_file(path);
        std::vector<Line> lines;
        std::size_t start = 0;
        while (start < source.size()) {
            auto end = source.find('\n', start); if (end == std::string::npos) end = source.size();
            auto text = source.substr(start, end-start);
            if (text.find_first_not_of(" \t\r\n") != std::string::npos) {
                auto parsed = j::parse(text); if (parsed) sort_json(*parsed);
                lines.push_back({std::move(text), parsed ? std::optional<J>(std::move(*parsed)) : std::nullopt});
            }
            start = end+1;
        }
        std::vector<std::pair<std::string, std::string>> files;
        if (auto dir = side_dir(path); dir && fs::is_directory(*dir)) {
            for (const auto& entry : fs::recursive_directory_iterator(*dir)) if (entry.is_regular_file())
                files.emplace_back(entry.path().lexically_relative(*dir).generic_string(), read_file(entry.path()));
            std::ranges::sort(files, {}, &std::pair<std::string, std::string>::first);
        }
        Importer importer(sink, lines, path, files);
        for (std::size_t i = 0; i < lines.size(); ++i) importer.line(static_cast<std::int64_t>(i), lines[i]);
        importer.finish(files);
        return {};
    } catch (const AdapterError& e) { return std::unexpected(e); }
      catch (const fs::filesystem_error& e) { return std::unexpected(AdapterError(AdapterError::Io{loa::Error(loa::ErrorCode::io, e.what())})); }
}
Result<> export_(const Dom& dom, const fs::path& out) {
    try {
        const Node* root = dom.root();
        if (!root) return std::unexpected(AdapterError(AdapterError::Format{"the session DOM is empty"}));
        std::map<std::int64_t, std::vector<std::pair<std::int64_t, const Node*>>> lines;
        std::vector<const Node*> fresh;
        std::vector<std::pair<std::string, std::string>> files;
        for (const auto& node : dom.iter()) {
            auto line = node.int_attr(X_LINE);
            if (node.kind == ElementKind::Ext && node.str_attr(attr::HARNESS) == HARNESS) {
                if (auto path = node.str_attr(X_PATH)) {
                    std::string bytes = node.text;
                    if (auto* raw = node.attr(attr::RAW)) if (auto data = raw->as_bytes())
                        bytes.assign(reinterpret_cast<const char*>(data->data()), data->size());
                    files.emplace_back(*path, std::move(bytes));
                } else if (line) lines[*line].emplace_back(node.int_attr(X_PART).value_or(-1), &node);
            } else if (node.kind == ElementKind::Message || node.kind == ElementKind::Call ||
                       node.kind == ElementKind::Result || node.kind == ElementKind::Compaction) {
                if (line) lines[*line].emplace_back(node.int_attr(X_PART).value_or(-1), &node);
                else fresh.push_back(&node);
            }
        }
        write_file(out, Writer(dom, *root).run(lines, fresh));
        if (!files.empty()) {
            auto side = side_dir(out);
            if (!side) return std::unexpected(AdapterError(AdapterError::Format{out.string() + " has no extension to strip for side files"}));
            for (const auto& [path, bytes] : files) {
                fs::path relative(path);
                if (path.empty() || relative.is_absolute())
                    return std::unexpected(AdapterError(AdapterError::Format{"side file path `" + path + "` leaves the side directory"}));
                for (const auto& part : relative)
                    if (part == "." || part == ".." || part.empty())
                        return std::unexpected(AdapterError(AdapterError::Format{"side file path `" + path + "` leaves the side directory"}));
                write_file(*side / relative, bytes);
            }
        }
        return {};
    } catch (const AdapterError& error) { return std::unexpected(error); }
      catch (const fs::filesystem_error& error) {
        return std::unexpected(AdapterError(AdapterError::Io{loa::Error(loa::ErrorCode::io, error.what())}));
    }
}


std::vector<kit::NativeSession> discover(const fs::path& home) {
    std::vector<kit::NativeSession> result;
    for (auto& path : kit::walk(home / ".claude/projects", 1, [](std::string_view name) { return name.ends_with(".jsonl"); }))
        if (auto session = kit::NativeSession::at(HARNESS, path, kit::parent_name(path), std::nullopt)) result.push_back(std::move(*session));
    return result;
}
Result<> ClaudeNative::import(const fs::path& path, PatchSink& sink) const { return native::import(path, sink); }
Result<> ClaudeNative::export_(const Dom& dom, const fs::path& out) const { return native::export_(dom, out); }
std::vector<kit::NativeSession> ClaudeNative::discover(const fs::path& home) const { return native::discover(home); }

} // namespace loa::harness::claude::native
