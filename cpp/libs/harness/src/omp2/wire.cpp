#include "loa/harness/omp2/wire.hpp"

#include <algorithm>
#include <array>
#include <format>
#include <type_traits>
#include <limits>

namespace loa::harness::omp2::wire {

namespace {

using loa::runtime::json::Array;
using loa::runtime::json::Object;

template <typename T>
using Decoded = std::expected<T, std::string>;

constexpr std::array<std::string_view, 26> KNOWN_TAGS{
    "session", "meta", "body", "queues", "todo", "item", "jobs", "job", "directors", "director", "con", "var", "compaction",
    "turn", "user", "assistant", "developer", "notice", "steering", "prompts", "prompt", "subagent", "input", "result", "diag", "usage",
};

// serde's name for the JSON type of `v`, as its `invalid type` errors print it.
std::string unexpected(const Json& v) {
    if (v.is_null()) return "null";
    if (const bool* b = v.as_bool()) return std::format("boolean `{}`", *b);
    if (auto i = v.as_i64(); i && *i < 0) return std::format("integer `{}`", *i);
    if (auto u = v.as_u64()) return std::format("integer `{}`", *u);
    if (v.is_number()) return std::format("floating point `{}`", *v.as_f64());
    if (const std::string* s = v.as_string()) return std::format("string \"{}\"", *s);
    if (v.is_array()) return "sequence";
    return "map";
}

std::string invalid(const Json& v, std::string_view expected) { return std::format("invalid type: {}, expected {}", unexpected(v), expected); }
std::string missing(std::string_view key) { return std::format("missing field `{}`", key); }

Decoded<std::string> as_string(const Json& v) {
    if (const std::string* s = v.as_string()) return *s;
    return std::unexpected(invalid(v, "a string"));
}

Decoded<std::uint64_t> as_u64(const Json& v) {
    if (auto u = v.as_u64()) return *u;
    if (auto i = v.as_i64()) return std::unexpected(std::format("invalid value: integer `{}`, expected u64", *i));
    return std::unexpected(invalid(v, "u64"));
}

Decoded<std::uint32_t> as_u32(const Json& v) {
    if (auto u = v.as_u64(); u && *u <= std::numeric_limits<std::uint32_t>::max()) return static_cast<std::uint32_t>(*u);
    if (auto i = v.as_i64()) return std::unexpected(std::format("invalid value: integer `{}`, expected u32", *i));
    return std::unexpected(invalid(v, "u32"));
}

Decoded<Handle> as_handle(const Json& v) {
    auto u = as_u64(v);
    if (!u) return std::unexpected(u.error());
    return Handle{*u};
}

Decoded<std::optional<Handle>> as_opt_handle(const Json* v) {
    if (v == nullptr || v->is_null()) return std::optional<Handle>{};
    auto h = as_handle(*v);
    if (!h) return std::unexpected(h.error());
    return std::optional<Handle>{*h};
}

Decoded<std::optional<std::string>> opt_string(const Json& obj, std::string_view key) {
    const Json* v = obj.find(key);
    if (v == nullptr || v->is_null()) return std::optional<std::string>{};
    auto s = as_string(*v);
    if (!s) return std::unexpected(s.error());
    return std::optional<std::string>{std::move(*s)};
}

Decoded<std::string> req_string(const Json& obj, std::string_view key) {
    const Json* v = obj.find(key);
    if (v == nullptr) return std::unexpected(missing(key));
    return as_string(*v);
}

Decoded<bool> opt_bool(const Json& obj, std::string_view key) {
    const Json* v = obj.find(key);
    if (v == nullptr) return false;
    if (const bool* b = v->as_bool()) return *b;
    return std::unexpected(invalid(*v, "a boolean"));
}

Decoded<std::vector<std::string>> strings(const Json& v) {
    const Array* arr = v.as_array();
    if (arr == nullptr) return std::unexpected(invalid(v, "a sequence"));
    std::vector<std::string> out;
    for (const Json& item : *arr) {
        auto s = as_string(item);
        if (!s) return std::unexpected(s.error());
        out.push_back(std::move(*s));
    }
    return out;
}

Decoded<std::vector<std::string>> default_strings(const Json& obj, std::string_view key) {
    const Json* v = obj.find(key);
    if (v == nullptr) return std::vector<std::string>{};
    return strings(*v);
}

Decoded<NodeSpec> node_spec(const Json& v) {
    if (!v.is_object()) return std::unexpected(invalid(v, "struct NodeSpec"));
    NodeSpec spec;
    auto tag = req_string(v, "tag");
    if (!tag) return std::unexpected(tag.error());
    spec.tag = Tag::from_wire(*tag);
    if (const Json* props = v.find("props")) {
        const Array* arr = props->as_array();
        if (arr == nullptr) return std::unexpected(invalid(*props, "a sequence"));
        for (const Json& pair : *arr) {
            const Array* kv = pair.as_array();
            if (kv == nullptr) return std::unexpected(invalid(pair, "a tuple of size 2"));
            if (kv->size() != 2) return std::unexpected(std::format("invalid length {}, expected a tuple of size 2", kv->size()));
            auto name = as_string((*kv)[0]);
            if (!name) return std::unexpected(name.error());
            spec.props.emplace_back(prop_from_wire(*name), (*kv)[1]);
        }
    }
    if (const Json* kids = v.find("kids")) {
        const Array* arr = kids->as_array();
        if (arr == nullptr) return std::unexpected(invalid(*kids, "a sequence"));
        for (const Json& kid : *arr) {
            auto h = as_handle(kid);
            if (!h) return std::unexpected(h.error());
            spec.kids.push_back(*h);
        }
    }
    auto content = opt_string(v, "content");
    if (!content) return std::unexpected(content.error());
    spec.content = std::move(*content);
    return spec;
}

Decoded<Op> decode_op(const Json& v) {
    constexpr std::string_view untagged = "data did not match any variant of untagged enum Op";
    const Array* arr = v.as_array();
    if (arr == nullptr || arr->empty()) return std::unexpected(std::string(untagged));
    const std::string* code = (*arr)[0].as_string();
    if (code == nullptr) return std::unexpected(std::string(untagged));
    const Array& a = *arr;
    auto fail = [&](const auto& r) -> bool { return !r.has_value(); };
    if (*code == "ins" && a.size() == 4) {
        auto parent = as_handle(a[1]);
        auto after = as_opt_handle(&a[2]);
        auto spec = node_spec(a[3]);
        if (!fail(parent) && !fail(after) && !fail(spec)) return op::Ins{*parent, *after, std::move(*spec)};
    } else if (*code == "rm" && a.size() == 2) {
        if (auto h = as_handle(a[1])) return op::Rm{*h};
    } else if (*code == "set" && a.size() == 4) {
        auto h = as_handle(a[1]);
        auto prop = as_string(a[2]);
        if (h && prop) return op::Set{*h, prop_from_wire(*prop), a[3]};
    } else if (*code == "mv" && a.size() == 4) {
        auto h = as_handle(a[1]);
        auto parent = as_handle(a[2]);
        auto after = as_opt_handle(&a[3]);
        if (h && parent && after) return op::Mv{*h, *parent, *after};
    }
    return std::unexpected(std::string(untagged));
}

Decoded<Snapshot> snapshot(const Json& v) {
    if (!v.is_object()) return std::unexpected(invalid(v, "struct Snapshot"));
    Snapshot out;
    const Json* hw = v.find("high_water");
    if (hw == nullptr) return std::unexpected(missing("high_water"));
    auto high = as_u64(*hw);
    if (!high) return std::unexpected(high.error());
    out.high_water = *high;
    if (const Json* nodes = v.find("nodes")) {
        const Array* arr = nodes->as_array();
        if (arr == nullptr) return std::unexpected(invalid(*nodes, "a sequence"));
        for (const Json& n : *arr) {
            if (!n.is_object()) return std::unexpected(invalid(n, "struct SnapshotNode"));
            const Json* handle = n.find("handle");
            if (handle == nullptr) return std::unexpected(missing("handle"));
            auto h = as_handle(*handle);
            if (!h) return std::unexpected(h.error());
            auto parent = as_opt_handle(n.find("parent"));
            if (!parent) return std::unexpected(parent.error());
            const Json* node = n.find("node");
            if (node == nullptr) return std::unexpected(missing("node"));
            auto spec = node_spec(*node);
            if (!spec) return std::unexpected(spec.error());
            out.nodes.push_back(SnapshotNode{*h, *parent, std::move(*spec)});
        }
    }
    if (const Json* streams = v.find("streams")) {
        const Array* arr = streams->as_array();
        if (arr == nullptr) return std::unexpected(invalid(*streams, "a sequence"));
        for (const Json& s : *arr) {
            if (!s.is_object()) return std::unexpected(invalid(s, "struct SnapshotStream"));
            const Json* sid = s.find("sid");
            if (sid == nullptr) return std::unexpected(missing("sid"));
            auto id = as_u32(*sid);
            if (!id) return std::unexpected(id.error());
            const Json* node = s.find("node");
            if (node == nullptr) return std::unexpected(missing("node"));
            auto h = as_handle(*node);
            if (!h) return std::unexpected(h.error());
            auto prop = req_string(s, "prop");
            if (!prop) return std::unexpected(prop.error());
            auto text = req_string(s, "text");
            if (!text) return std::unexpected(text.error());
            out.streams.push_back(SnapshotStream{*id, *h, prop_from_wire(*prop), std::move(*text)});
        }
    }
    return out;
}

} // namespace

Tag Tag::from_wire(std::string_view wire) {
    if (wire.starts_with("tool:")) return Tag{std::string(wire.substr(5)), true};
    return Tag{std::string(wire), std::ranges::find(KNOWN_TAGS, wire) == KNOWN_TAGS.end()};
}

std::string prop_from_wire(std::string_view wire) {
    if (wire.starts_with("custom:")) return std::string(wire.substr(7));
    return std::string(wire);
}

Decoded<DomEvent> decode_dom_event(const Json& msg) {
    if (!msg.is_object()) return std::unexpected(invalid(msg, "internally tagged enum DomEvent"));
    const Json* kind = msg.find("kind");
    if (kind == nullptr) return std::unexpected(missing("kind"));
    const std::string* k = kind->as_string();
    if (k == nullptr) return std::unexpected(invalid(*kind, "variant identifier"));
    if (*k == "patch@1") {
        const Json* data = msg.find("data");
        if (data == nullptr) return std::unexpected(missing("data"));
        if (!data->is_object()) return std::unexpected(invalid(*data, "struct Patch"));
        const Json* ops = data->find("ops");
        if (ops == nullptr) return std::unexpected(missing("ops"));
        const Array* arr = ops->as_array();
        if (arr == nullptr) return std::unexpected(invalid(*ops, "a sequence"));
        dom_event::Patch out;
        for (const Json& o : *arr) {
            auto decoded = decode_op(o);
            if (!decoded) return std::unexpected(decoded.error());
            out.data.ops.push_back(std::move(*decoded));
        }
        return out;
    }
    if (*k == "stream@1") {
        dom_event::Stream out;
        const Json* sid = msg.find("sid");
        if (sid == nullptr) return std::unexpected(missing("sid"));
        auto id = as_u32(*sid);
        if (!id) return std::unexpected(id.error());
        out.sid = *id;
        auto op = req_string(msg, "op");
        if (!op) return std::unexpected(op.error());
        if (*op == "open") out.op = StreamOp::Open;
        else if (*op == "append") out.op = StreamOp::Append;
        else if (*op == "close") out.op = StreamOp::Close;
        else return std::unexpected(std::format("unknown variant `{}`, expected one of `open`, `append`, `close`", *op));
        auto node = as_opt_handle(msg.find("node"));
        if (!node) return std::unexpected(node.error());
        out.node = *node;
        auto prop = opt_string(msg, "prop");
        if (!prop) return std::unexpected(prop.error());
        if (*prop) out.prop = prop_from_wire(**prop);
        auto text = opt_string(msg, "text");
        if (!text) return std::unexpected(text.error());
        out.text = std::move(*text);
        return out;
    }
    if (*k == "snapshot@1") {
        const Json* data = msg.find("data");
        if (data == nullptr) return std::unexpected(missing("data"));
        auto snap = snapshot(*data);
        if (!snap) return std::unexpected(snap.error());
        return dom_event::Snapshot{std::move(*snap)};
    }
    return std::unexpected(std::format("unknown variant `{}`, expected one of `patch@1`, `stream@1`, `snapshot@1`", *k));
}

Decoded<Frame> decode_frame(const Json& msg) {
    if (!msg.is_object()) return std::unexpected(invalid(msg, "internally tagged enum Frame"));
    const Json* type = msg.find("type");
    if (type == nullptr) return std::unexpected(missing("type"));
    const std::string* t = type->as_string();
    if (t == nullptr) return std::unexpected(invalid(*type, "variant identifier"));
    if (*t == "agent_end") {
        frame::AgentEnd out;
        auto cancelled = opt_bool(msg, "cancelled");
        if (!cancelled) return std::unexpected(cancelled.error());
        auto error = opt_string(msg, "error");
        if (!error) return std::unexpected(error.error());
        out.cancelled = *cancelled;
        out.error = std::move(*error);
        return out;
    }
    if (*t == "tool_approval_request") {
        frame::ToolApprovalRequest out;
        auto prompt = req_string(msg, "promptId");
        if (!prompt) return std::unexpected(prompt.error());
        out.prompt_id = std::move(*prompt);
        for (auto [key, slot] : {std::pair{"toolCallId", &out.tool_call_id}, std::pair{"title", &out.title}, std::pair{"body", &out.body}}) {
            auto value = opt_string(msg, key);
            if (!value) return std::unexpected(value.error());
            *slot = std::move(*value);
        }
        if (const Json* scopes = msg.find("scopes"); scopes != nullptr && !scopes->is_null()) {
            auto list = strings(*scopes);
            if (!list) return std::unexpected(list.error());
            out.scopes = std::move(*list);
        }
        if (const Json* timeout = msg.find("timeoutMs"); timeout != nullptr && !timeout->is_null()) {
            auto ms = timeout->as_i64();
            if (!ms) return std::unexpected(invalid(*timeout, "i64"));
            out.timeout_ms = *ms;
        }
        return out;
    }
    if (*t == "extension_ui_request") {
        frame::ExtensionUiRequest out;
        const Json* id = msg.find("id");
        if (id == nullptr) return std::unexpected(missing("id"));
        out.id = *id;
        auto method = req_string(msg, "method");
        if (!method) return std::unexpected(method.error());
        out.method = std::move(*method);
        auto title = opt_string(msg, "title");
        if (!title) return std::unexpected(title.error());
        out.title = std::move(*title);
        auto options = default_strings(msg, "options");
        if (!options) return std::unexpected(options.error());
        out.options = std::move(*options);
        auto multi = opt_bool(msg, "multi");
        if (!multi) return std::unexpected(multi.error());
        out.multi = *multi;
        return out;
    }
    return frame::Other{};
}

Decoded<Input> decode_input(const Json& msg) {
    if (auto dom = decode_dom_event(msg)) return Input{std::move(*dom)};
    if (auto fr = decode_frame(msg)) return Input{std::move(*fr)};
    return std::unexpected(std::string("data did not match any variant of untagged enum Input"));
}

Json to_json(const Reply& value) {
    return std::visit(
        [](const auto& r) -> Json {
            using R = std::decay_t<decltype(r)>;
            Object out;
            if constexpr (std::is_same_v<R, reply::Approve>) {
                out.emplace_back("type", "approve");
                out.emplace_back("promptId", r.prompt_id);
                out.emplace_back("approved", r.approved);
                out.emplace_back("scope", r.scope);
                if (r.reason) out.emplace_back("reason", *r.reason);
            } else {
                out.emplace_back("type", "extension_ui_response");
                out.emplace_back("id", r.id);
                Array values;
                for (const auto& v : r.values) values.emplace_back(v);
                out.emplace_back("values", std::move(values));
                if (r.custom_input) out.emplace_back("customInput", *r.custom_input);
            }
            return out;
        },
        value);
}

Decoded<std::vector<ModelEntry>> decode_models(const Json& models) {
    const Array* arr = models.as_array();
    if (arr == nullptr) return std::unexpected(invalid(models, "a sequence"));
    std::vector<ModelEntry> out;
    for (const Json& m : *arr) {
        if (!m.is_object()) return std::unexpected(invalid(m, "struct ModelEntry"));
        ModelEntry entry;
        auto id = req_string(m, "id");
        if (!id) return std::unexpected(id.error());
        entry.id = std::move(*id);
        for (auto [key, slot] : {std::pair{"name", &entry.name}, std::pair{"provider", &entry.provider}}) {
            if (const Json* v = m.find(key)) {
                auto s = as_string(*v);
                if (!s) return std::unexpected(s.error());
                *slot = std::move(*s);
            }
        }
        auto reasoning = opt_bool(m, "reasoning");
        if (!reasoning) return std::unexpected(reasoning.error());
        entry.reasoning = *reasoning;
        out.push_back(std::move(entry));
    }
    return out;
}

} // namespace loa::harness::omp2::wire
