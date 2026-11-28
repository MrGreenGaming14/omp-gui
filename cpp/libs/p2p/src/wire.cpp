#include "loa/p2p/wire.hpp"

#include <format>
#include <utility>

#include "loa/bus/proto.hpp"
#include "loa/core/patch.hpp"
#include "loa/runtime/proto.hpp"

namespace loa::p2p::wire {
namespace {

namespace pb = loa::runtime::proto;
using pb::WireType;
using Span = std::span<const std::uint8_t>;

// ---- encoding (prost: proto3 defaults are skipped, optionals and oneof members are not) ----

void put_str(Bytes& out, std::uint32_t field, std::string_view s) {
    pb::put_tag(out, field, WireType::len);
    pb::put_len(out, s);
}
void put_bytes(Bytes& out, std::uint32_t field, Span b) {
    pb::put_tag(out, field, WireType::len);
    pb::put_len(out, b);
}
void put_uint(Bytes& out, std::uint32_t field, std::uint64_t v) {
    pb::put_tag(out, field, WireType::varint);
    pb::put_varint(out, v);
}
void str(Bytes& out, std::uint32_t field, const std::string& s) {
    if (!s.empty()) put_str(out, field, s);
}
void bytes(Bytes& out, std::uint32_t field, const Bytes& b) {
    if (!b.empty()) put_bytes(out, field, b);
}
void uint(Bytes& out, std::uint32_t field, std::uint64_t v) {
    if (v != 0) put_uint(out, field, v);
}
void sint(Bytes& out, std::uint32_t field, std::int64_t v) { uint(out, field, static_cast<std::uint64_t>(v)); }
void boolean(Bytes& out, std::uint32_t field, bool v) { uint(out, field, v ? 1 : 0); }
void strings(Bytes& out, std::uint32_t field, const std::vector<std::string>& v) {
    for (const auto& s : v) put_str(out, field, s);
}
template <class M>
void message(Bytes& out, std::uint32_t field, const M& m) {
    Bytes inner;
    encode(m, inner);
    put_bytes(out, field, inner);
}
template <class M>
void messages(Bytes& out, std::uint32_t field, const std::vector<M>& v) {
    for (const auto& m : v) message(out, field, m);
}
void patch(Bytes& out, std::uint32_t field, const WirePatch& p) { put_bytes(out, field, p); }

// ---- decoding ----

std::unexpected<NodeError> decode_error(const loa::Error& e) { return fail(NodeError::Kind::decode, e.message()); }
std::unexpected<NodeError> bad_wire(std::uint32_t field) {
    return fail(NodeError::Kind::decode, std::format("invalid wire type for field {}", field));
}

// Reads every field, calling `on(tag, reader)` → Result<bool> (false = unknown, skipped).
template <class F>
Result<> fields(Span bytes, F&& on) {
    pb::Reader r(bytes);
    while (!r.done()) {
        auto tag = r.tag();
        if (!tag) return decode_error(tag.error());
        auto known = on(*tag, r);
        if (!known) return std::unexpected(known.error());
        if (!*known) {
            if (auto s = r.skip(*tag); !s) return decode_error(s.error());
        }
    }
    return {};
}

Result<> read_len(pb::Tag tag, pb::Reader& r, Span& out) {
    if (tag.type != WireType::len) return bad_wire(tag.field);
    auto v = r.len();
    if (!v) return decode_error(v.error());
    out = *v;
    return {};
}
Result<> read(pb::Tag tag, pb::Reader& r, std::string& out) {
    Span s;
    if (auto ok = read_len(tag, r, s); !ok) return ok;
    out.assign(s.begin(), s.end());
    return {};
}
Result<> read(pb::Tag tag, pb::Reader& r, Bytes& out) {
    Span s;
    if (auto ok = read_len(tag, r, s); !ok) return ok;
    out.assign(s.begin(), s.end());
    return {};
}
Result<> read_varint(pb::Tag tag, pb::Reader& r, std::uint64_t& out) {
    if (tag.type != WireType::varint) return bad_wire(tag.field);
    auto v = r.varint();
    if (!v) return decode_error(v.error());
    out = *v;
    return {};
}
Result<> read(pb::Tag tag, pb::Reader& r, std::uint64_t& out) { return read_varint(tag, r, out); }
Result<> read(pb::Tag tag, pb::Reader& r, std::uint32_t& out) {
    std::uint64_t v = 0;
    if (auto ok = read_varint(tag, r, v); !ok) return ok;
    out = static_cast<std::uint32_t>(v);
    return {};
}
Result<> read(pb::Tag tag, pb::Reader& r, std::int64_t& out) {
    std::uint64_t v = 0;
    if (auto ok = read_varint(tag, r, v); !ok) return ok;
    out = static_cast<std::int64_t>(v);
    return {};
}
Result<> read(pb::Tag tag, pb::Reader& r, bool& out) {
    std::uint64_t v = 0;
    if (auto ok = read_varint(tag, r, v); !ok) return ok;
    out = v != 0;
    return {};
}
template <class T>
Result<> read(pb::Tag tag, pb::Reader& r, std::optional<T>& out) {
    T v{};
    if (auto ok = read(tag, r, v); !ok) return ok;
    out = std::move(v);
    return {};
}
template <class T>
Result<> read(pb::Tag tag, pb::Reader& r, std::vector<T>& out) {
    if constexpr (std::is_same_v<T, std::string> || std::is_same_v<T, Bytes>) {
        T v;
        if (auto ok = read(tag, r, v); !ok) return ok;
        out.push_back(std::move(v));
    } else {
        Span s;
        if (auto ok = read_len(tag, r, s); !ok) return ok;
        T v;
        if (auto ok = decode(s, v); !ok) return ok;
        out.push_back(std::move(v));
    }
    return {};
}
// A singular message field: prost merges repeats into the existing value; wire
// patches are opaque, so the last occurrence wins (prost would merge them).
template <class M>
Result<> read_message(pb::Tag tag, pb::Reader& r, M& out) {
    Span s;
    if (auto ok = read_len(tag, r, s); !ok) return ok;
    return decode(s, out);
}

// Field dispatch helper: `F(n, member)` arms in a switch.
#define F(n, member)                                                                                                   \
    case n: {                                                                                                          \
        if (auto ok = read(tag, r, m.member); !ok) return std::unexpected(ok.error());                                 \
        return true;                                                                                                   \
    }
#define DECODE(M, ...)                                                                                                 \
    Result<> decode(Span bytes, M& m) {                                                                                \
        m = M{};                                                                                                       \
        return fields(bytes, [&](pb::Tag tag, pb::Reader& r) -> Result<bool> {                                         \
            (void)r;                                                                                                   \
            (void)m;                                                                                                   \
            switch (tag.field) {                                                                                       \
                __VA_ARGS__                                                                                            \
            default: return false;                                                                                     \
            }                                                                                                          \
        });                                                                                                            \
    }

// Oneof bodies: each tag decodes its variant alternative.
template <class Variant, std::size_t I = 0>
Result<bool> oneof(pb::Tag tag, pb::Reader& r, std::optional<Variant>& body) {
    if constexpr (I == std::variant_size_v<Variant>) {
        return false;
    } else {
        if (tag.field != I + 1) return oneof<Variant, I + 1>(tag, r, body);
        using Alt = std::variant_alternative_t<I, Variant>;
        Span s;
        if (auto ok = read_len(tag, r, s); !ok) return std::unexpected(ok.error());
        // prost merges into the same variant, replaces a different one.
        Alt value;
        if (body && body->index() == I) value = std::get<I>(*body);
        Bytes merged;
        if (body && body->index() == I) encode(value, merged);
        merged.insert(merged.end(), s.begin(), s.end());
        if (auto ok = decode(merged, value); !ok) return std::unexpected(ok.error());
        body.emplace(std::in_place_index<I>, std::move(value));
        return true;
    }
}

} // namespace

// ---- encode ----

void encode(const Ack& m, Bytes& out) { str(out, 1, m.error); }
void encode(const ProtocolVersion& m, Bytes& out) {
    str(out, 1, m.name);
    uint(out, 2, m.version);
}
void encode(const Hello& m, Bytes& out) {
    str(out, 1, m.app_version);
    messages(out, 2, m.protocols);
    str(out, 3, m.name);
}
void encode(const Presence& m, Bytes& out) {
    boolean(out, 1, m.online);
    if (m.last_seen_ms) put_uint(out, 2, static_cast<std::uint64_t>(*m.last_seen_ms));
}
void encode(const GrantDelivery& m, Bytes& out) {
    bytes(out, 1, m.token);
    bytes(out, 2, m.root);
}
void encode(const GrantRenewal& m, Bytes& out) { bytes(out, 1, m.grant_id); }
void encode(const GrantRenewalResult& m, Bytes& out) {
    if (m.token) put_bytes(out, 1, *m.token);
    str(out, 2, m.refusal);
}
void encode(const GrantRevocation& m, Bytes& out) { bytes(out, 1, m.grant_id); }
void encode(const Challenge& m, Bytes& out) { bytes(out, 1, m.nonce); }
void encode(const PresentedGrant& m, Bytes& out) {
    bytes(out, 1, m.token);
    bytes(out, 2, m.proof);
}
void encode(const Present& m, Bytes& out) { messages(out, 1, m.grants); }
void encode(const ListSessions&, Bytes&) {}
void encode(const SessionEntry& m, Bytes& out) {
    str(out, 1, m.id);
    str(out, 2, m.title);
    str(out, 3, m.harness);
    sint(out, 4, m.created_ms);
    strings(out, 5, m.actions);
    boolean(out, 6, m.partial);
    uint(out, 7, m.last_seq);
}
void encode(const SessionList& m, Bytes& out) { messages(out, 1, m.sessions); }
void encode(const Seed& m, Bytes& out) {
    uint(out, 1, m.source_seq);
    for (const auto& p : m.patches) patch(out, 2, p);
    strings(out, 3, m.actions);
}
void encode(const DriveResult& m, Bytes& out) {
    if (m.patch) patch(out, 1, *m.patch);
    if (m.conflict_current) put_uint(out, 2, *m.conflict_current);
    str(out, 3, m.error);
}
void encode(const Prompt& m, Bytes& out) {
    str(out, 1, m.session);
    str(out, 2, m.text);
}
void encode(const Answer& m, Bytes& out) {
    str(out, 1, m.session);
    uint(out, 2, m.decision);
    strings(out, 3, m.values);
    if (m.reason) put_str(out, 4, *m.reason);
}
void encode(const Subscribe& m, Bytes& out) {
    str(out, 1, m.session);
    uint(out, 2, m.after_source_seq);
}
void encode(const WireUpdate& m, Bytes& out) {
    uint(out, 1, m.source_seq);
    if (m.patch) patch(out, 2, *m.patch);
}
void encode(const Drive& m, Bytes& out) {
    str(out, 1, m.session);
    uint(out, 2, m.base_seq);
    bytes(out, 3, m.draft);
    sint(out, 4, m.ts);
}
void encode(const FolderRequest& m, Bytes& out) {
    str(out, 1, m.op);
    str(out, 2, m.folder);
    str(out, 3, m.path);
    uint(out, 4, m.size);
    bytes(out, 5, m.data);
}
void encode(const WireEntry& m, Bytes& out) {
    str(out, 1, m.name);
    boolean(out, 2, m.dir);
    uint(out, 3, m.size);
}
void encode(const FolderReply& m, Bytes& out) {
    str(out, 1, m.error);
    boolean(out, 2, m.ready);
    messages(out, 3, m.entries);
    bytes(out, 4, m.data);
    boolean(out, 5, m.done);
}
void encode(const ControlFrame& m, Bytes& out) {
    if (m.body)
        std::visit([&](const auto& v) { message(out, static_cast<std::uint32_t>(m.body->index() + 1), v); }, *m.body);
}
void encode(const Frame& m, Bytes& out) {
    if (m.body)
        std::visit([&](const auto& v) { message(out, static_cast<std::uint32_t>(m.body->index() + 1), v); }, *m.body);
}

// ---- decode ----

DECODE(Ack, F(1, error))
DECODE(ProtocolVersion, F(1, name) F(2, version))
DECODE(Hello, F(1, app_version) F(2, protocols) F(3, name))
DECODE(Presence, F(1, online) F(2, last_seen_ms))
DECODE(GrantDelivery, F(1, token) F(2, root))
DECODE(GrantRenewal, F(1, grant_id))
DECODE(GrantRenewalResult, F(1, token) F(2, refusal))
DECODE(GrantRevocation, F(1, grant_id))
DECODE(Challenge, F(1, nonce))
DECODE(PresentedGrant, F(1, token) F(2, proof))
DECODE(Present, F(1, grants))
DECODE(ListSessions, )
DECODE(SessionEntry, F(1, id) F(2, title) F(3, harness) F(4, created_ms) F(5, actions) F(6, partial) F(7, last_seq))
DECODE(SessionList, F(1, sessions))
DECODE(Seed, F(1, source_seq) F(2, patches) F(3, actions))
DECODE(DriveResult, F(1, patch) F(2, conflict_current) F(3, error))
DECODE(Prompt, F(1, session) F(2, text))
DECODE(Answer, F(1, session) F(2, decision) F(3, values) F(4, reason))
DECODE(Subscribe, F(1, session) F(2, after_source_seq))
DECODE(WireUpdate, F(1, source_seq) F(2, patch))
DECODE(Drive, F(1, session) F(2, base_seq) F(3, draft) F(4, ts))
DECODE(FolderRequest, F(1, op) F(2, folder) F(3, path) F(4, size) F(5, data))
DECODE(WireEntry, F(1, name) F(2, dir) F(3, size))
DECODE(FolderReply, F(1, error) F(2, ready) F(3, entries) F(4, data) F(5, done))
#undef F
#undef DECODE

Result<> decode(Span bytes, ControlFrame& m) {
    m = {};
    return fields(bytes, [&](pb::Tag tag, pb::Reader& r) { return oneof(tag, r, m.body); });
}
Result<> decode(Span bytes, Frame& m) {
    m = {};
    return fields(bytes, [&](pb::Tag tag, pb::Reader& r) { return oneof(tag, r, m.body); });
}

// ---- framing ----

Result<> send_frame(SendHalf& tx, Span body) {
    if (body.empty() || body.size() > MAX_FRAME_SIZE)
        return fail(NodeError::Kind::protocol,
                    std::format("frame of {} bytes is empty or exceeds {}", body.size(), MAX_FRAME_SIZE));
    Bytes framed;
    framed.reserve(4 + body.size());
    const auto len = static_cast<std::uint32_t>(body.size());
    framed.push_back(static_cast<std::uint8_t>(len >> 24));
    framed.push_back(static_cast<std::uint8_t>(len >> 16));
    framed.push_back(static_cast<std::uint8_t>(len >> 8));
    framed.push_back(static_cast<std::uint8_t>(len));
    framed.insert(framed.end(), body.begin(), body.end());
    return tx.write_all(framed);
}

Result<std::optional<Bytes>> recv_frame(RecvHalf& rx) {
    std::uint8_t header[4];
    auto first = rx.read(std::span(header, 1));
    if (!first) return std::unexpected(first.error());
    if (*first == 0) return std::optional<Bytes>();
    if (auto rest = rx.read_exact(std::span(header + 1, 3)); !rest) return std::unexpected(rest.error());
    const std::size_t len = (std::size_t{header[0]} << 24) | (std::size_t{header[1]} << 16) |
                            (std::size_t{header[2]} << 8) | std::size_t{header[3]};
    if (len == 0 || len > MAX_FRAME_SIZE) return fail(NodeError::Kind::protocol, std::format("invalid frame size: {}", len));
    Bytes data(len);
    if (auto body = rx.read_exact(data); !body) return std::unexpected(body.error());
    return std::optional<Bytes>(std::move(data));
}

// ---- envelopes ----

ControlFrame make_ack(std::type_identity<ControlFrame>, std::optional<std::string> error) {
    return ControlFrame{Control(std::in_place_type<Ack>, Ack{error.value_or("")})};
}
Frame make_ack(std::type_identity<Frame>, std::optional<std::string> error) {
    return Frame{Body(std::in_place_type<Ack>, Ack{error.value_or("")})};
}
FolderReply make_ack(std::type_identity<FolderReply>, std::optional<std::string> error) {
    FolderReply reply;
    reply.done = error.has_value();
    reply.ready = !error.has_value();
    reply.error = error.value_or("");
    return reply;
}

namespace {
template <class M>
Result<M> frame_ack(M message) {
    if (!message.body) return fail(NodeError::Kind::protocol, "empty frame");
    if (const auto* a = std::get_if<Ack>(&*message.body); a && !a->error.empty())
        return fail(NodeError::Kind::refused, a->error);
    return message;
}
} // namespace

Result<ControlFrame> into_ack(ControlFrame message) { return frame_ack(std::move(message)); }
Result<Frame> into_ack(Frame message) { return frame_ack(std::move(message)); }
Result<FolderReply> into_ack(FolderReply message) {
    if (!message.error.empty()) return fail(NodeError::Kind::refused, message.error);
    return message;
}

// ---- patches ----

WirePatch to_wire(const loa::core::Patch& patch) { return loa::bus::proto::encode_patch(patch); }

Result<std::vector<loa::core::Patch>> patches(std::span<const WirePatch> wire) {
    std::vector<loa::core::Patch> out;
    out.reserve(wire.size());
    for (const auto& bytes : wire) {
        auto patch = loa::bus::proto::decode_patch(bytes);
        if (!patch) return fail(NodeError::Kind::protocol, loa::bus::proto::to_string(patch.error()));
        out.push_back(std::move(*patch));
    }
    return out;
}

} // namespace loa::p2p::wire
