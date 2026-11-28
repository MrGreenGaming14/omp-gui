#include "loa/bus/proto.hpp"

#include <bit>
#include <format>
#include <optional>
#include <string_view>
#include <type_traits>
#include <utility>

#include "loa/runtime/proto.hpp"

namespace loa::bus::proto {

namespace wire = loa::runtime::proto;
using wire::WireType;

std::string to_string(const ProtoError& e) {
    namespace pe = proto_error;
    return std::visit(
        [](const auto& v) -> std::string {
            using T = std::decay_t<decltype(v)>;
            if constexpr (std::is_same_v<T, pe::Decode>) return "failed to decode Protobuf message: " + v.description;
            else if constexpr (std::is_same_v<T, pe::MissingOp>) return std::format("patch {} has no op", v.seq);
            else if constexpr (std::is_same_v<T, pe::MissingValue>) return "a value has no variant";
            else if constexpr (std::is_same_v<T, pe::UnknownKind>) return std::format("unknown element kind \"{}\"", v.kind);
            else if constexpr (std::is_same_v<T, pe::DuplicateAttr>)
                return std::format("attribute \"{}\" appears twice", v.name);
            else return core::to_string(v.error);
        },
        e);
}

loa::Error to_error(const ProtoError& e) { return loa::Error(ErrorCode::parse, to_string(e)); }

// ---- encoding ----

namespace {

void put_str(Bytes& out, std::uint32_t field, std::string_view s) {
    if (s.empty()) return;
    wire::put_tag(out, field, WireType::len);
    wire::put_len(out, s);
}

void put_u64(Bytes& out, std::uint32_t field, std::uint64_t v) {
    if (v == 0) return;
    wire::put_tag(out, field, WireType::varint);
    wire::put_varint(out, v);
}

// A length-delimited sub-message written by `body`.
template <class F>
void put_msg(Bytes& out, std::uint32_t field, F&& body) {
    Bytes inner;
    std::forward<F>(body)(inner);
    wire::put_tag(out, field, WireType::len);
    wire::put_len(out, std::span<const std::uint8_t>(inner));
}

void encode_attrs(const core::Attrs& attrs, std::uint32_t field, Bytes& out) {
    for (const auto& [name, value] : attrs) {
        put_msg(out, field, [&](Bytes& b) {
            put_str(b, 1, name);
            put_msg(b, 2, [&](Bytes& v) { encode(value, v); });
        });
    }
}

} // namespace

void encode(const core::Value& value, Bytes& out) {
    std::visit(
        [&](const auto& v) {
            using T = std::decay_t<decltype(v)>;
            if constexpr (std::is_same_v<T, bool>) {
                wire::put_tag(out, 1, WireType::varint);
                wire::put_varint(out, v ? 1 : 0);
            } else if constexpr (std::is_same_v<T, std::int64_t>) {
                wire::put_tag(out, 2, WireType::varint);
                wire::put_varint(out, wire::zigzag_encode(v));
            } else if constexpr (std::is_same_v<T, double>) {
                wire::put_tag(out, 3, WireType::fixed64);
                wire::put_fixed64(out, std::bit_cast<std::uint64_t>(v));
            } else if constexpr (std::is_same_v<T, std::string>) {
                wire::put_tag(out, 4, WireType::len);
                wire::put_len(out, std::string_view(v));
            } else if constexpr (std::is_same_v<T, core::Value::Bytes>) {
                wire::put_tag(out, 5, WireType::len);
                wire::put_len(out, std::span<const std::uint8_t>(v));
            } else {
                put_msg(out, 6, [&](Bytes& list) {
                    for (const auto& item : v) put_msg(list, 1, [&](Bytes& b) { encode(item, b); });
                });
            }
        },
        value.variant());
}

void encode(const core::Patch& patch, Bytes& out) {
    put_u64(out, 1, patch.seq);
    put_u64(out, 2, patch.node.value);
    put_u64(out, 3, static_cast<std::uint64_t>(patch.ts));
    put_msg(out, 4, [&](Bytes& op) {
        std::visit(
            [&](const auto& o) {
                using T = std::decay_t<decltype(o)>;
                if constexpr (std::is_same_v<T, core::op::Insert>) {
                    put_msg(op, 1, [&](Bytes& b) {
                        put_u64(b, 1, o.parent.value);
                        if (o.index) {
                            wire::put_tag(b, 2, WireType::varint);
                            wire::put_varint(b, *o.index);
                        }
                        put_str(b, 3, core::as_str(o.kind));
                        encode_attrs(o.attrs, 4, b);
                        put_str(b, 5, o.text);
                    });
                } else if constexpr (std::is_same_v<T, core::op::Set>) {
                    put_msg(op, 2, [&](Bytes& b) {
                        put_str(b, 1, o.name);
                        if (o.value) put_msg(b, 2, [&](Bytes& v) { encode(*o.value, v); });
                    });
                } else if constexpr (std::is_same_v<T, core::op::Append>) {
                    put_msg(op, 3, [&](Bytes& b) { put_str(b, 1, o.text); });
                } else {
                    put_msg(op, 4, [](Bytes&) {});
                }
            },
            patch.op);
    });
}

void encode(const core::Node& node, Bytes& out) {
    put_u64(out, 1, node.id.value);
    put_str(out, 2, core::as_str(node.kind));
    put_u64(out, 3, node.parent.value);
    if (!node.children.empty()) {
        Bytes packed;
        for (auto c : node.children) wire::put_varint(packed, c.value);
        wire::put_tag(out, 4, WireType::len);
        wire::put_len(out, std::span<const std::uint8_t>(packed));
    }
    encode_attrs(node.attrs, 5, out);
    put_str(out, 6, node.text);
}

void encode(const core::Dom& dom, Bytes& out) {
    put_u64(out, 1, dom.seq());
    put_u64(out, 2, dom.next_id().value);
    for (const auto& node : dom.iter()) put_msg(out, 3, [&](Bytes& b) { encode(node, b); });
}

Bytes encode_patch(const core::Patch& patch) {
    Bytes out;
    encode(patch, out);
    return out;
}

Bytes encode_dom(const core::Dom& dom) {
    Bytes out;
    encode(dom, out);
    return out;
}

// ---- decoding ----
//
// Decoding follows prost: unknown fields are skipped, a repeated sub-message
// field merges, the last scalar wins, and conversion checks (missing op or
// value, unknown kind, duplicate attribute) run on the decoded message.

namespace {

template <class T>
using Result = std::expected<T, ProtoError>;

std::unexpected<ProtoError> decode_error(std::string description) {
    return std::unexpected(ProtoError(proto_error::Decode{std::move(description)}));
}

std::unexpected<ProtoError> decode_error(const loa::Error& e) { return decode_error(e.message()); }

bool valid_utf8(std::span<const std::uint8_t> s) {
    std::size_t i = 0;
    while (i < s.size()) {
        const std::uint8_t c = s[i];
        std::size_t n = 0;
        std::uint32_t cp = 0;
        if (c < 0x80) {
            ++i;
            continue;
        } else if ((c & 0xE0) == 0xC0) {
            n = 1;
            cp = c & 0x1F;
        } else if ((c & 0xF0) == 0xE0) {
            n = 2;
            cp = c & 0x0F;
        } else if ((c & 0xF8) == 0xF0) {
            n = 3;
            cp = c & 0x07;
        } else {
            return false;
        }
        for (std::size_t k = 1; k <= n; ++k) {
            if (i + k >= s.size() || (s[i + k] & 0xC0) != 0x80) return false;
            cp = (cp << 6) | (s[i + k] & 0x3F);
        }
        if ((n == 1 && cp < 0x80) || (n == 2 && cp < 0x800) || (n == 3 && cp < 0x10000) || cp > 0x10FFFF ||
            (cp >= 0xD800 && cp <= 0xDFFF))
            return false;
        i += n + 1;
    }
    return true;
}

Result<void> expect(wire::Tag tag, WireType type) {
    if (tag.type != type)
        return decode_error(std::format("invalid wire type: {} (expected {})", static_cast<int>(tag.type),
                                        static_cast<int>(type)));
    return {};
}

Result<std::uint64_t> read_varint(wire::Reader& r, wire::Tag tag) {
    if (auto e = expect(tag, WireType::varint); !e) return std::unexpected(e.error());
    auto v = r.varint();
    if (!v) return decode_error(v.error());
    return *v;
}

Result<std::span<const std::uint8_t>> read_len(wire::Reader& r, wire::Tag tag) {
    if (auto e = expect(tag, WireType::len); !e) return std::unexpected(e.error());
    auto v = r.len();
    if (!v) return decode_error(v.error());
    return *v;
}

Result<std::string> read_string(wire::Reader& r, wire::Tag tag) {
    auto b = read_len(r, tag);
    if (!b) return std::unexpected(b.error());
    if (!valid_utf8(*b)) return decode_error("invalid string value: data is not UTF-8 encoded");
    return std::string(b->begin(), b->end());
}

Result<void> skip(wire::Reader& r, wire::Tag tag) {
    if (auto s = r.skip(tag); !s) return decode_error(s.error());
    return {};
}

// Runs `field(tag)` for every field of the message in `bytes`; `field`
// returns false for fields it doesn't know, which are skipped.
template <class F>
Result<void> fields(std::span<const std::uint8_t> bytes, F&& field) {
    wire::Reader r(bytes);
    while (!r.done()) {
        auto tag = r.tag();
        if (!tag) return decode_error(tag.error());
        auto known = field(r, *tag);
        if (!known) return std::unexpected(known.error());
        if (!*known)
            if (auto s = skip(r, *tag); !s) return s;
    }
    return {};
}

// A `Value` message as decoded: `kind` empty when no variant was set.
// `missing_inner` marks a list holding an item without a variant, which
// conversion reports as `MissingValue`.
struct PValue {
    std::optional<core::Value> kind;
    bool missing_inner = false;
};

Result<void> merge_value(std::span<const std::uint8_t> bytes, PValue& into);

// Merges `ValueList` bytes into `items`.
Result<void> merge_list(std::span<const std::uint8_t> bytes, core::Value::List& items, bool& missing) {
    return fields(bytes, [&](wire::Reader& r, wire::Tag tag) -> Result<bool> {
        if (tag.field != 1) return false;
        auto b = read_len(r, tag);
        if (!b) return std::unexpected(b.error());
        PValue item;
        if (auto m = merge_value(*b, item); !m) return std::unexpected(m.error());
        if (!item.kind || item.missing_inner) {
            missing = true;
            items.emplace_back(false);
        } else {
            items.push_back(std::move(*item.kind));
        }
        return true;
    });
}

Result<void> merge_value(std::span<const std::uint8_t> bytes, PValue& into) {
    return fields(bytes, [&](wire::Reader& r, wire::Tag tag) -> Result<bool> {
        switch (tag.field) {
        case 1: {
            auto v = read_varint(r, tag);
            if (!v) return std::unexpected(v.error());
            into.kind = core::Value(*v != 0);
            into.missing_inner = false;
            return true;
        }
        case 2: {
            auto v = read_varint(r, tag);
            if (!v) return std::unexpected(v.error());
            into.kind = core::Value(wire::zigzag_decode(*v));
            into.missing_inner = false;
            return true;
        }
        case 3: {
            if (auto e = expect(tag, WireType::fixed64); !e) return std::unexpected(e.error());
            auto v = r.fixed64();
            if (!v) return decode_error(v.error());
            into.kind = core::Value(std::bit_cast<double>(*v));
            into.missing_inner = false;
            return true;
        }
        case 4: {
            auto s = read_string(r, tag);
            if (!s) return std::unexpected(s.error());
            into.kind = core::Value(std::move(*s));
            into.missing_inner = false;
            return true;
        }
        case 5: {
            auto b = read_len(r, tag);
            if (!b) return std::unexpected(b.error());
            into.kind = core::Value(core::Value::Bytes(b->begin(), b->end()));
            into.missing_inner = false;
            return true;
        }
        case 6: {
            auto b = read_len(r, tag);
            if (!b) return std::unexpected(b.error());
            core::Value::List items;
            bool missing = false;
            // A repeated List field merges into the list already set.
            if (into.kind && into.kind->type() == core::ValueType::List) {
                auto existing = into.kind->as_list();
                items.assign(existing->begin(), existing->end());
                missing = into.missing_inner;
            }
            if (auto m = merge_list(*b, items, missing); !m) return std::unexpected(m.error());
            into.kind = core::Value(std::move(items));
            into.missing_inner = missing;
            return true;
        }
        default: return false;
        }
    });
}

// An optional `Value` field: present (even without a variant) or not.
struct OptValue {
    bool present = false;
    PValue value;
};

Result<std::optional<core::Value>> convert(OptValue v) {
    if (!v.present) return std::nullopt;
    if (!v.value.kind || v.value.missing_inner) return std::unexpected(ProtoError(proto_error::MissingValue{}));
    return std::move(v.value.kind);
}

struct PAttr {
    std::string name;
    OptValue value;
};

Result<void> merge_opt_value(wire::Reader& r, wire::Tag tag, OptValue& into) {
    auto b = read_len(r, tag);
    if (!b) return std::unexpected(b.error());
    into.present = true;
    return merge_value(*b, into.value);
}

Result<PAttr> decode_attr(std::span<const std::uint8_t> bytes) {
    PAttr out;
    auto ok = fields(bytes, [&](wire::Reader& r, wire::Tag tag) -> Result<bool> {
        if (tag.field == 1) {
            auto s = read_string(r, tag);
            if (!s) return std::unexpected(s.error());
            out.name = std::move(*s);
            return true;
        }
        if (tag.field == 2) {
            if (auto m = merge_opt_value(r, tag, out.value); !m) return std::unexpected(m.error());
            return true;
        }
        return false;
    });
    if (!ok) return std::unexpected(ok.error());
    return out;
}

Result<bool> push_attr(wire::Reader& r, wire::Tag tag, std::vector<PAttr>& attrs) {
    auto b = read_len(r, tag);
    if (!b) return std::unexpected(b.error());
    auto a = decode_attr(*b);
    if (!a) return std::unexpected(a.error());
    attrs.push_back(std::move(*a));
    return true;
}

Result<core::Attrs> attrs_from_proto(std::vector<PAttr> attrs) {
    core::Attrs out;
    for (auto& a : attrs) {
        if (!a.value.present) return std::unexpected(ProtoError(proto_error::MissingValue{}));
        auto v = convert(std::move(a.value));
        if (!v) return std::unexpected(v.error());
        if (out.contains(a.name)) return std::unexpected(ProtoError(proto_error::DuplicateAttr{a.name}));
        out.emplace(std::move(a.name), std::move(**v));
    }
    return out;
}

Result<core::ElementKind> kind_from_proto(std::string kind) {
    if (auto k = core::parse<core::ElementKind>(kind)) return *k;
    return std::unexpected(ProtoError(proto_error::UnknownKind{std::move(kind)}));
}

struct PInsert {
    std::uint64_t parent = 0;
    std::optional<std::uint32_t> index;
    std::string kind;
    std::vector<PAttr> attrs;
    std::string text;
};
struct PSet {
    std::string name;
    OptValue value;
};

// `Op`: which oneof variant is set (0 none, else its tag) and its fields.
struct POp {
    std::uint32_t which = 0;
    PInsert insert;
    PSet set;
    std::string append;
};

Result<void> merge_insert(std::span<const std::uint8_t> bytes, PInsert& in) {
    return fields(bytes, [&](wire::Reader& r, wire::Tag tag) -> Result<bool> {
        switch (tag.field) {
        case 1: {
            auto v = read_varint(r, tag);
            if (!v) return std::unexpected(v.error());
            in.parent = *v;
            return true;
        }
        case 2: {
            auto v = read_varint(r, tag);
            if (!v) return std::unexpected(v.error());
            in.index = static_cast<std::uint32_t>(*v);
            return true;
        }
        case 3: {
            auto s = read_string(r, tag);
            if (!s) return std::unexpected(s.error());
            in.kind = std::move(*s);
            return true;
        }
        case 4: return push_attr(r, tag, in.attrs);
        case 5: {
            auto s = read_string(r, tag);
            if (!s) return std::unexpected(s.error());
            in.text = std::move(*s);
            return true;
        }
        default: return false;
        }
    });
}

Result<void> merge_op(std::span<const std::uint8_t> bytes, POp& op) {
    return fields(bytes, [&](wire::Reader& r, wire::Tag tag) -> Result<bool> {
        if (tag.field < 1 || tag.field > 4) return false;
        auto b = read_len(r, tag);
        if (!b) return std::unexpected(b.error());
        if (op.which != tag.field) op = POp{tag.field, {}, {}, {}};
        switch (tag.field) {
        case 1:
            if (auto m = merge_insert(*b, op.insert); !m) return std::unexpected(m.error());
            break;
        case 2: {
            auto m = fields(*b, [&](wire::Reader& r2, wire::Tag t2) -> Result<bool> {
                if (t2.field == 1) {
                    auto s = read_string(r2, t2);
                    if (!s) return std::unexpected(s.error());
                    op.set.name = std::move(*s);
                    return true;
                }
                if (t2.field == 2) {
                    if (auto mv = merge_opt_value(r2, t2, op.set.value); !mv) return std::unexpected(mv.error());
                    return true;
                }
                return false;
            });
            if (!m) return std::unexpected(m.error());
            break;
        }
        case 3: {
            auto m = fields(*b, [&](wire::Reader& r2, wire::Tag t2) -> Result<bool> {
                if (t2.field != 1) return false;
                auto s = read_string(r2, t2);
                if (!s) return std::unexpected(s.error());
                op.append = std::move(*s);
                return true;
            });
            if (!m) return std::unexpected(m.error());
            break;
        }
        default: {
            auto m = fields(*b, [](wire::Reader&, wire::Tag) -> Result<bool> { return false; });
            if (!m) return std::unexpected(m.error());
        }
        }
        return true;
    });
}

Result<core::Op> op_from_proto(POp op, std::uint64_t seq) {
    switch (op.which) {
    case 1: {
        auto kind = kind_from_proto(std::move(op.insert.kind));
        if (!kind) return std::unexpected(kind.error());
        auto attrs = attrs_from_proto(std::move(op.insert.attrs));
        if (!attrs) return std::unexpected(attrs.error());
        return core::op::Insert{core::NodeId{op.insert.parent}, op.insert.index, *kind, std::move(*attrs),
                                std::move(op.insert.text)};
    }
    case 2: {
        auto v = convert(std::move(op.set.value));
        if (!v) return std::unexpected(v.error());
        return core::op::Set{std::move(op.set.name), std::move(*v)};
    }
    case 3: return core::op::Append{std::move(op.append)};
    case 4: return core::op::Remove{};
    default: return std::unexpected(ProtoError(proto_error::MissingOp{seq}));
    }
}

struct PNode {
    std::uint64_t id = 0;
    std::string kind;
    std::uint64_t parent = 0;
    std::vector<core::NodeId> children;
    std::vector<PAttr> attrs;
    std::string text;
};

Result<PNode> decode_node(std::span<const std::uint8_t> bytes) {
    PNode n;
    auto ok = fields(bytes, [&](wire::Reader& r, wire::Tag tag) -> Result<bool> {
        switch (tag.field) {
        case 1:
        case 3: {
            auto v = read_varint(r, tag);
            if (!v) return std::unexpected(v.error());
            (tag.field == 1 ? n.id : n.parent) = *v;
            return true;
        }
        case 2:
        case 6: {
            auto s = read_string(r, tag);
            if (!s) return std::unexpected(s.error());
            (tag.field == 2 ? n.kind : n.text) = std::move(*s);
            return true;
        }
        case 4: {
            // Packed or one by one, as prost accepts both.
            if (tag.type == WireType::varint) {
                auto v = r.varint();
                if (!v) return decode_error(v.error());
                n.children.push_back(core::NodeId{*v});
                return true;
            }
            auto b = read_len(r, tag);
            if (!b) return std::unexpected(b.error());
            wire::Reader packed(*b);
            while (!packed.done()) {
                auto v = packed.varint();
                if (!v) return decode_error(v.error());
                n.children.push_back(core::NodeId{*v});
            }
            return true;
        }
        case 5: return push_attr(r, tag, n.attrs);
        default: return false;
        }
    });
    if (!ok) return std::unexpected(ok.error());
    return n;
}

} // namespace

std::expected<core::Value, ProtoError> decode_value(std::span<const std::uint8_t> bytes) {
    OptValue v{true, {}};
    if (auto m = merge_value(bytes, v.value); !m) return std::unexpected(m.error());
    auto out = convert(std::move(v));
    if (!out) return std::unexpected(out.error());
    return std::move(**out);
}

std::expected<core::Patch, ProtoError> decode_patch(std::span<const std::uint8_t> bytes) {
    std::uint64_t seq = 0, node = 0, ts = 0;
    std::optional<POp> op;
    auto ok = fields(bytes, [&](wire::Reader& r, wire::Tag tag) -> Result<bool> {
        if (tag.field >= 1 && tag.field <= 3) {
            auto v = read_varint(r, tag);
            if (!v) return std::unexpected(v.error());
            (tag.field == 1 ? seq : tag.field == 2 ? node : ts) = *v;
            return true;
        }
        if (tag.field == 4) {
            auto b = read_len(r, tag);
            if (!b) return std::unexpected(b.error());
            if (!op) op.emplace();
            if (auto m = merge_op(*b, *op); !m) return std::unexpected(m.error());
            return true;
        }
        return false;
    });
    if (!ok) return std::unexpected(ok.error());
    if (!op) return std::unexpected(ProtoError(proto_error::MissingOp{seq}));
    auto converted = op_from_proto(std::move(*op), seq);
    if (!converted) return std::unexpected(converted.error());
    return core::Patch{seq, core::NodeId{node}, static_cast<std::int64_t>(ts), std::move(*converted)};
}

std::expected<core::Dom, ProtoError> decode_dom(std::span<const std::uint8_t> bytes) {
    std::uint64_t seq = 0, next_id = 0;
    std::vector<PNode> raw;
    auto ok = fields(bytes, [&](wire::Reader& r, wire::Tag tag) -> Result<bool> {
        if (tag.field == 1 || tag.field == 2) {
            auto v = read_varint(r, tag);
            if (!v) return std::unexpected(v.error());
            (tag.field == 1 ? seq : next_id) = *v;
            return true;
        }
        if (tag.field == 3) {
            auto b = read_len(r, tag);
            if (!b) return std::unexpected(b.error());
            auto n = decode_node(*b);
            if (!n) return std::unexpected(n.error());
            raw.push_back(std::move(*n));
            return true;
        }
        return false;
    });
    if (!ok) return std::unexpected(ok.error());
    std::vector<core::Node> nodes;
    nodes.reserve(raw.size());
    for (auto& n : raw) {
        auto kind = kind_from_proto(std::move(n.kind));
        if (!kind) return std::unexpected(kind.error());
        auto attrs = attrs_from_proto(std::move(n.attrs));
        if (!attrs) return std::unexpected(attrs.error());
        nodes.push_back(core::Node{core::NodeId{n.id}, *kind, core::NodeId{n.parent}, std::move(n.children),
                                   std::move(*attrs), std::move(n.text)});
    }
    auto dom = core::Dom::from_parts(std::move(nodes), core::NodeId{next_id}, seq);
    if (!dom) return std::unexpected(ProtoError(proto_error::Parts{dom.error()}));
    return std::move(*dom);
}

} // namespace loa::bus::proto
