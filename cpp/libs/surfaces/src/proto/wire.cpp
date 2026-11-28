#include "loa/surfaces/proto/wire.hpp"

#include <bit>
#include <format>

#include "loa/runtime/proto.hpp"

namespace loa::surfaces::proto {

namespace pb = loa::runtime::proto;
namespace cs = loa::contracts::surface;
using pb::WireType;

namespace {

// ---- encoding (prost proto3 rules: scalars at their default are omitted) ----

void put_u64(Bytes& out, std::uint32_t field, std::uint64_t v) {
    if (v == 0) return;
    pb::put_tag(out, field, WireType::varint);
    pb::put_varint(out, v);
}

void put_i32(Bytes& out, std::uint32_t field, std::int32_t v) {
    if (v == 0) return;
    pb::put_tag(out, field, WireType::varint);
    pb::put_varint(out, static_cast<std::uint64_t>(static_cast<std::int64_t>(v)));
}

void put_bool(Bytes& out, std::uint32_t field, bool v) {
    if (!v) return;
    pb::put_tag(out, field, WireType::varint);
    pb::put_varint(out, 1);
}

void put_float(Bytes& out, std::uint32_t field, float v) {
    if (v == 0.0F) return;
    pb::put_tag(out, field, WireType::fixed32);
    pb::put_fixed32(out, std::bit_cast<std::uint32_t>(v));
}

void put_str(Bytes& out, std::uint32_t field, std::string_view v, bool always = false) {
    if (v.empty() && !always) return;
    pb::put_tag(out, field, WireType::len);
    pb::put_len(out, v);
}

void put_bytes(Bytes& out, std::uint32_t field, const Bytes& v) {
    if (v.empty()) return;
    pb::put_tag(out, field, WireType::len);
    pb::put_len(out, std::span<const std::uint8_t>(v));
}

// A nested message: always written when present, even if empty.
template <typename M>
void put_msg(Bytes& out, std::uint32_t field, const M& msg);

void enc(Bytes& out, const Open& m) {
    put_u64(out, 1, m.version);
    put_str(out, 2, m.uri);
    if (!m.encodings.empty()) {
        Bytes packed;
        for (auto e : m.encodings) pb::put_varint(packed, static_cast<std::uint64_t>(static_cast<std::int64_t>(e)));
        pb::put_tag(out, 3, WireType::len);
        pb::put_len(out, std::span<const std::uint8_t>(packed));
    }
}

void enc(Bytes& out, const SurfaceInfoMsg& m) {
    put_str(out, 1, m.uri);
    put_str(out, 2, m.name);
    put_u64(out, 3, m.width);
    put_u64(out, 4, m.height);
    put_i32(out, 5, m.mode);
    if (m.mime) put_str(out, 6, *m.mime, true);
}

void enc(Bytes& out, const StreamInfoMsg& m) {
    put_i32(out, 1, m.encoding);
    put_str(out, 2, m.encoder);
    put_bool(out, 3, m.hardware);
}

void enc(Bytes& out, const Accept& m) {
    put_u64(out, 1, m.version);
    if (m.info) put_msg(out, 2, *m.info);
    put_bool(out, 3, m.datagrams);
    if (m.stream) put_msg(out, 4, *m.stream);
}

void enc(Bytes& out, const Reject& m) {
    put_i32(out, 1, m.code);
    put_str(out, 2, m.reason);
}

void enc(Bytes&, const KeyframeRequest&) {}

void enc(Bytes& out, const Resize& m) {
    put_u64(out, 1, m.width);
    put_u64(out, 2, m.height);
}

void enc(Bytes& out, const Close& m) { put_str(out, 1, m.reason); }

void enc(Bytes& out, const MouseMoveMsg& m) {
    put_float(out, 1, m.x);
    put_float(out, 2, m.y);
}

void enc(Bytes& out, const MouseButtonMsg& m) {
    put_float(out, 1, m.x);
    put_float(out, 2, m.y);
    put_i32(out, 3, m.button);
    put_bool(out, 4, m.pressed);
    put_u64(out, 5, m.modifiers);
}

void enc(Bytes& out, const ScrollMsg& m) {
    put_float(out, 1, m.x);
    put_float(out, 2, m.y);
    put_float(out, 3, m.dx);
    put_float(out, 4, m.dy);
    put_u64(out, 5, m.modifiers);
}

void enc(Bytes& out, const KeyMsg& m) {
    put_str(out, 1, m.key);
    put_str(out, 2, m.code);
    put_bool(out, 3, m.pressed);
    put_u64(out, 4, m.modifiers);
}

void enc(Bytes& out, const TextMsg& m) { put_str(out, 1, m.text); }

void enc(Bytes& out, const InputMsg& m) {
    put_u64(out, 1, m.seq);
    put_u64(out, 2, m.sent_us);
    if (m.kind) {
        std::visit([&](const auto& k) { put_msg(out, static_cast<std::uint32_t>(m.kind->index() + 3), k); }, *m.kind);
    }
}

void enc(Bytes& out, const FrameHeader& m) {
    put_u64(out, 1, m.seq);
    put_u64(out, 2, m.captured_us);
    put_u64(out, 3, m.input_seq);
    put_u64(out, 4, m.width);
    put_u64(out, 5, m.height);
    put_u64(out, 6, m.region_x);
    put_u64(out, 7, m.region_y);
    put_u64(out, 8, m.region_width);
    put_u64(out, 9, m.region_height);
    put_i32(out, 10, m.encoding);
    put_bool(out, 11, m.keyframe);
}

void enc(Bytes& out, const FrameMsg& m) {
    if (m.header) put_msg(out, 1, *m.header);
    put_bytes(out, 2, m.data);
}

void enc(Bytes& out, const Fragment& m) {
    put_u64(out, 1, m.seq);
    put_u64(out, 2, m.index);
    put_u64(out, 3, m.count);
    if (m.header) put_msg(out, 4, *m.header);
    put_bytes(out, 5, m.data);
}

void enc(Bytes& out, const DocumentHeader& m) {
    put_str(out, 1, m.name);
    put_str(out, 2, m.mime);
    put_u64(out, 3, m.length);
    put_u64(out, 4, m.crc32);
}

void enc(Bytes& out, const DocumentChunk& m) {
    put_u64(out, 1, m.offset);
    put_bytes(out, 2, m.data);
}

void enc(Bytes& out, const StreamMessage& m) {
    if (m.body) {
        std::visit([&](const auto& b) { put_msg(out, static_cast<std::uint32_t>(m.body->index() + 1), b); }, *m.body);
    }
}

void enc(Bytes& out, const Datagram& m) {
    if (m.kind) put_msg(out, 1, std::get<Fragment>(*m.kind));
}

template <typename M>
void put_msg(Bytes& out, std::uint32_t field, const M& msg) {
    Bytes inner;
    enc(inner, msg);
    pb::put_tag(out, field, WireType::len);
    pb::put_len(out, std::span<const std::uint8_t>(inner));
}

// ---- decoding ----

template <typename T>
using Ex = std::expected<T, loa::Error>;

loa::Error wire_error(std::uint32_t field, WireType got) {
    return loa::Error(ErrorCode::parse,
                      std::format("proto: field {} has unexpected wire type {}", field, static_cast<int>(got)));
}

Ex<std::uint64_t> want_varint(pb::Reader& r, pb::Tag t) {
    if (t.type != WireType::varint) return std::unexpected(wire_error(t.field, t.type));
    return r.varint();
}

Ex<std::uint32_t> want_u32(pb::Reader& r, pb::Tag t) {
    auto v = want_varint(r, t);
    if (!v) return std::unexpected(v.error());
    return static_cast<std::uint32_t>(*v);
}

Ex<std::int32_t> want_i32(pb::Reader& r, pb::Tag t) {
    auto v = want_varint(r, t);
    if (!v) return std::unexpected(v.error());
    return static_cast<std::int32_t>(static_cast<std::uint32_t>(*v));
}

Ex<bool> want_bool(pb::Reader& r, pb::Tag t) {
    auto v = want_varint(r, t);
    if (!v) return std::unexpected(v.error());
    return *v != 0;
}

Ex<float> want_float(pb::Reader& r, pb::Tag t) {
    if (t.type != WireType::fixed32) return std::unexpected(wire_error(t.field, t.type));
    auto v = r.fixed32();
    if (!v) return std::unexpected(v.error());
    return std::bit_cast<float>(*v);
}

Ex<std::span<const std::uint8_t>> want_len(pb::Reader& r, pb::Tag t) {
    if (t.type != WireType::len) return std::unexpected(wire_error(t.field, t.type));
    return r.len();
}

Ex<std::string> want_str(pb::Reader& r, pb::Tag t) {
    auto v = want_len(r, t);
    if (!v) return std::unexpected(v.error());
    return std::string(v->begin(), v->end());
}

Ex<Bytes> want_bytes(pb::Reader& r, pb::Tag t) {
    auto v = want_len(r, t);
    if (!v) return std::unexpected(v.error());
    return Bytes(v->begin(), v->end());
}

#define LOA_TRY_SET(lhs, expr)                                  \
    do {                                                        \
        auto _v = (expr);                                       \
        if (!_v) return std::unexpected(std::move(_v).error()); \
        lhs = std::move(*_v);                                   \
    } while (false)

template <typename M>
Ex<M> dec(std::span<const std::uint8_t> bytes);

template <typename M>
Ex<M> want_msg(pb::Reader& r, pb::Tag t) {
    auto v = want_len(r, t);
    if (!v) return std::unexpected(v.error());
    return dec<M>(*v);
}

// Runs `field(reader, tag)` per field; it returns false for unknown fields, which are skipped.
template <typename M, typename F>
Ex<M> fields(std::span<const std::uint8_t> bytes, F field) {
    M m{};
    pb::Reader r(bytes);
    while (!r.done()) {
        auto t = r.tag();
        if (!t) return std::unexpected(t.error());
        auto known = field(m, r, *t);
        if (!known) return std::unexpected(known.error());
        if (!*known) {
            if (auto s = r.skip(*t); !s) return std::unexpected(s.error());
        }
    }
    return m;
}

template <>
Ex<Open> dec<Open>(std::span<const std::uint8_t> bytes) {
    return fields<Open>(bytes, [](Open& m, pb::Reader& r, pb::Tag t) -> Ex<bool> {
        switch (t.field) {
        case 1: LOA_TRY_SET(m.version, want_u32(r, t)); return true;
        case 2: LOA_TRY_SET(m.uri, want_str(r, t)); return true;
        case 3:
            if (t.type == WireType::len) {
                auto packed = r.len();
                if (!packed) return std::unexpected(packed.error());
                pb::Reader p(*packed);
                while (!p.done()) {
                    auto v = p.varint();
                    if (!v) return std::unexpected(v.error());
                    m.encodings.push_back(static_cast<std::int32_t>(static_cast<std::uint32_t>(*v)));
                }
            } else {
                std::int32_t v = 0;
                LOA_TRY_SET(v, want_i32(r, t));
                m.encodings.push_back(v);
            }
            return true;
        default: return false;
        }
    });
}

template <>
Ex<SurfaceInfoMsg> dec<SurfaceInfoMsg>(std::span<const std::uint8_t> bytes) {
    return fields<SurfaceInfoMsg>(bytes, [](SurfaceInfoMsg& m, pb::Reader& r, pb::Tag t) -> Ex<bool> {
        switch (t.field) {
        case 1: LOA_TRY_SET(m.uri, want_str(r, t)); return true;
        case 2: LOA_TRY_SET(m.name, want_str(r, t)); return true;
        case 3: LOA_TRY_SET(m.width, want_u32(r, t)); return true;
        case 4: LOA_TRY_SET(m.height, want_u32(r, t)); return true;
        case 5: LOA_TRY_SET(m.mode, want_i32(r, t)); return true;
        case 6: LOA_TRY_SET(m.mime, want_str(r, t)); return true;
        default: return false;
        }
    });
}

template <>
Ex<StreamInfoMsg> dec<StreamInfoMsg>(std::span<const std::uint8_t> bytes) {
    return fields<StreamInfoMsg>(bytes, [](StreamInfoMsg& m, pb::Reader& r, pb::Tag t) -> Ex<bool> {
        switch (t.field) {
        case 1: LOA_TRY_SET(m.encoding, want_i32(r, t)); return true;
        case 2: LOA_TRY_SET(m.encoder, want_str(r, t)); return true;
        case 3: LOA_TRY_SET(m.hardware, want_bool(r, t)); return true;
        default: return false;
        }
    });
}

template <>
Ex<Accept> dec<Accept>(std::span<const std::uint8_t> bytes) {
    return fields<Accept>(bytes, [](Accept& m, pb::Reader& r, pb::Tag t) -> Ex<bool> {
        switch (t.field) {
        case 1: LOA_TRY_SET(m.version, want_u32(r, t)); return true;
        case 2: LOA_TRY_SET(m.info, want_msg<SurfaceInfoMsg>(r, t)); return true;
        case 3: LOA_TRY_SET(m.datagrams, want_bool(r, t)); return true;
        case 4: LOA_TRY_SET(m.stream, want_msg<StreamInfoMsg>(r, t)); return true;
        default: return false;
        }
    });
}

template <>
Ex<Reject> dec<Reject>(std::span<const std::uint8_t> bytes) {
    return fields<Reject>(bytes, [](Reject& m, pb::Reader& r, pb::Tag t) -> Ex<bool> {
        switch (t.field) {
        case 1: LOA_TRY_SET(m.code, want_i32(r, t)); return true;
        case 2: LOA_TRY_SET(m.reason, want_str(r, t)); return true;
        default: return false;
        }
    });
}

template <>
Ex<KeyframeRequest> dec<KeyframeRequest>(std::span<const std::uint8_t> bytes) {
    return fields<KeyframeRequest>(bytes, [](KeyframeRequest&, pb::Reader&, pb::Tag) -> Ex<bool> { return false; });
}

template <>
Ex<Resize> dec<Resize>(std::span<const std::uint8_t> bytes) {
    return fields<Resize>(bytes, [](Resize& m, pb::Reader& r, pb::Tag t) -> Ex<bool> {
        switch (t.field) {
        case 1: LOA_TRY_SET(m.width, want_u32(r, t)); return true;
        case 2: LOA_TRY_SET(m.height, want_u32(r, t)); return true;
        default: return false;
        }
    });
}

template <>
Ex<Close> dec<Close>(std::span<const std::uint8_t> bytes) {
    return fields<Close>(bytes, [](Close& m, pb::Reader& r, pb::Tag t) -> Ex<bool> {
        if (t.field != 1) return false;
        LOA_TRY_SET(m.reason, want_str(r, t));
        return true;
    });
}

template <>
Ex<MouseMoveMsg> dec<MouseMoveMsg>(std::span<const std::uint8_t> bytes) {
    return fields<MouseMoveMsg>(bytes, [](MouseMoveMsg& m, pb::Reader& r, pb::Tag t) -> Ex<bool> {
        switch (t.field) {
        case 1: LOA_TRY_SET(m.x, want_float(r, t)); return true;
        case 2: LOA_TRY_SET(m.y, want_float(r, t)); return true;
        default: return false;
        }
    });
}

template <>
Ex<MouseButtonMsg> dec<MouseButtonMsg>(std::span<const std::uint8_t> bytes) {
    return fields<MouseButtonMsg>(bytes, [](MouseButtonMsg& m, pb::Reader& r, pb::Tag t) -> Ex<bool> {
        switch (t.field) {
        case 1: LOA_TRY_SET(m.x, want_float(r, t)); return true;
        case 2: LOA_TRY_SET(m.y, want_float(r, t)); return true;
        case 3: LOA_TRY_SET(m.button, want_i32(r, t)); return true;
        case 4: LOA_TRY_SET(m.pressed, want_bool(r, t)); return true;
        case 5: LOA_TRY_SET(m.modifiers, want_u32(r, t)); return true;
        default: return false;
        }
    });
}

template <>
Ex<ScrollMsg> dec<ScrollMsg>(std::span<const std::uint8_t> bytes) {
    return fields<ScrollMsg>(bytes, [](ScrollMsg& m, pb::Reader& r, pb::Tag t) -> Ex<bool> {
        switch (t.field) {
        case 1: LOA_TRY_SET(m.x, want_float(r, t)); return true;
        case 2: LOA_TRY_SET(m.y, want_float(r, t)); return true;
        case 3: LOA_TRY_SET(m.dx, want_float(r, t)); return true;
        case 4: LOA_TRY_SET(m.dy, want_float(r, t)); return true;
        case 5: LOA_TRY_SET(m.modifiers, want_u32(r, t)); return true;
        default: return false;
        }
    });
}

template <>
Ex<KeyMsg> dec<KeyMsg>(std::span<const std::uint8_t> bytes) {
    return fields<KeyMsg>(bytes, [](KeyMsg& m, pb::Reader& r, pb::Tag t) -> Ex<bool> {
        switch (t.field) {
        case 1: LOA_TRY_SET(m.key, want_str(r, t)); return true;
        case 2: LOA_TRY_SET(m.code, want_str(r, t)); return true;
        case 3: LOA_TRY_SET(m.pressed, want_bool(r, t)); return true;
        case 4: LOA_TRY_SET(m.modifiers, want_u32(r, t)); return true;
        default: return false;
        }
    });
}

template <>
Ex<TextMsg> dec<TextMsg>(std::span<const std::uint8_t> bytes) {
    return fields<TextMsg>(bytes, [](TextMsg& m, pb::Reader& r, pb::Tag t) -> Ex<bool> {
        if (t.field != 1) return false;
        LOA_TRY_SET(m.text, want_str(r, t));
        return true;
    });
}

template <>
Ex<InputMsg> dec<InputMsg>(std::span<const std::uint8_t> bytes) {
    return fields<InputMsg>(bytes, [](InputMsg& m, pb::Reader& r, pb::Tag t) -> Ex<bool> {
        switch (t.field) {
        case 1: LOA_TRY_SET(m.seq, want_varint(r, t)); return true;
        case 2: LOA_TRY_SET(m.sent_us, want_varint(r, t)); return true;
        case 3: LOA_TRY_SET(m.kind, want_msg<MouseMoveMsg>(r, t)); return true;
        case 4: LOA_TRY_SET(m.kind, want_msg<MouseButtonMsg>(r, t)); return true;
        case 5: LOA_TRY_SET(m.kind, want_msg<ScrollMsg>(r, t)); return true;
        case 6: LOA_TRY_SET(m.kind, want_msg<KeyMsg>(r, t)); return true;
        case 7: LOA_TRY_SET(m.kind, want_msg<TextMsg>(r, t)); return true;
        case 8: LOA_TRY_SET(m.kind, want_msg<Resize>(r, t)); return true;
        default: return false;
        }
    });
}

template <>
Ex<FrameHeader> dec<FrameHeader>(std::span<const std::uint8_t> bytes) {
    return fields<FrameHeader>(bytes, [](FrameHeader& m, pb::Reader& r, pb::Tag t) -> Ex<bool> {
        switch (t.field) {
        case 1: LOA_TRY_SET(m.seq, want_varint(r, t)); return true;
        case 2: LOA_TRY_SET(m.captured_us, want_varint(r, t)); return true;
        case 3: LOA_TRY_SET(m.input_seq, want_varint(r, t)); return true;
        case 4: LOA_TRY_SET(m.width, want_u32(r, t)); return true;
        case 5: LOA_TRY_SET(m.height, want_u32(r, t)); return true;
        case 6: LOA_TRY_SET(m.region_x, want_u32(r, t)); return true;
        case 7: LOA_TRY_SET(m.region_y, want_u32(r, t)); return true;
        case 8: LOA_TRY_SET(m.region_width, want_u32(r, t)); return true;
        case 9: LOA_TRY_SET(m.region_height, want_u32(r, t)); return true;
        case 10: LOA_TRY_SET(m.encoding, want_i32(r, t)); return true;
        case 11: LOA_TRY_SET(m.keyframe, want_bool(r, t)); return true;
        default: return false;
        }
    });
}

template <>
Ex<FrameMsg> dec<FrameMsg>(std::span<const std::uint8_t> bytes) {
    return fields<FrameMsg>(bytes, [](FrameMsg& m, pb::Reader& r, pb::Tag t) -> Ex<bool> {
        switch (t.field) {
        case 1: LOA_TRY_SET(m.header, want_msg<FrameHeader>(r, t)); return true;
        case 2: LOA_TRY_SET(m.data, want_bytes(r, t)); return true;
        default: return false;
        }
    });
}

template <>
Ex<Fragment> dec<Fragment>(std::span<const std::uint8_t> bytes) {
    return fields<Fragment>(bytes, [](Fragment& m, pb::Reader& r, pb::Tag t) -> Ex<bool> {
        switch (t.field) {
        case 1: LOA_TRY_SET(m.seq, want_varint(r, t)); return true;
        case 2: LOA_TRY_SET(m.index, want_u32(r, t)); return true;
        case 3: LOA_TRY_SET(m.count, want_u32(r, t)); return true;
        case 4: LOA_TRY_SET(m.header, want_msg<FrameHeader>(r, t)); return true;
        case 5: LOA_TRY_SET(m.data, want_bytes(r, t)); return true;
        default: return false;
        }
    });
}

template <>
Ex<DocumentHeader> dec<DocumentHeader>(std::span<const std::uint8_t> bytes) {
    return fields<DocumentHeader>(bytes, [](DocumentHeader& m, pb::Reader& r, pb::Tag t) -> Ex<bool> {
        switch (t.field) {
        case 1: LOA_TRY_SET(m.name, want_str(r, t)); return true;
        case 2: LOA_TRY_SET(m.mime, want_str(r, t)); return true;
        case 3: LOA_TRY_SET(m.length, want_varint(r, t)); return true;
        case 4: LOA_TRY_SET(m.crc32, want_u32(r, t)); return true;
        default: return false;
        }
    });
}

template <>
Ex<DocumentChunk> dec<DocumentChunk>(std::span<const std::uint8_t> bytes) {
    return fields<DocumentChunk>(bytes, [](DocumentChunk& m, pb::Reader& r, pb::Tag t) -> Ex<bool> {
        switch (t.field) {
        case 1: LOA_TRY_SET(m.offset, want_varint(r, t)); return true;
        case 2: LOA_TRY_SET(m.data, want_bytes(r, t)); return true;
        default: return false;
        }
    });
}

template <>
Ex<StreamMessage> dec<StreamMessage>(std::span<const std::uint8_t> bytes) {
    return fields<StreamMessage>(bytes, [](StreamMessage& m, pb::Reader& r, pb::Tag t) -> Ex<bool> {
        switch (t.field) {
        case 1: LOA_TRY_SET(m.body, want_msg<Open>(r, t)); return true;
        case 2: LOA_TRY_SET(m.body, want_msg<Accept>(r, t)); return true;
        case 3: LOA_TRY_SET(m.body, want_msg<Reject>(r, t)); return true;
        case 4: LOA_TRY_SET(m.body, want_msg<InputMsg>(r, t)); return true;
        case 5: LOA_TRY_SET(m.body, want_msg<KeyframeRequest>(r, t)); return true;
        case 6: LOA_TRY_SET(m.body, want_msg<Resize>(r, t)); return true;
        case 7: LOA_TRY_SET(m.body, want_msg<FrameMsg>(r, t)); return true;
        case 8: LOA_TRY_SET(m.body, want_msg<DocumentHeader>(r, t)); return true;
        case 9: LOA_TRY_SET(m.body, want_msg<DocumentChunk>(r, t)); return true;
        case 10: LOA_TRY_SET(m.body, want_msg<Close>(r, t)); return true;
        default: return false;
        }
    });
}

template <>
Ex<Datagram> dec<Datagram>(std::span<const std::uint8_t> bytes) {
    return fields<Datagram>(bytes, [](Datagram& m, pb::Reader& r, pb::Tag t) -> Ex<bool> {
        if (t.field != 1) return false;
        LOA_TRY_SET(m.kind, want_msg<Fragment>(r, t));
        return true;
    });
}

#undef LOA_TRY_SET

} // namespace

void encode(Bytes& out, const StreamMessage& msg) { enc(out, msg); }
void encode(Bytes& out, const Datagram& msg) { enc(out, msg); }
void encode(Bytes& out, const InputMsg& msg) { enc(out, msg); }

void encode_length_delimited(Bytes& out, const StreamMessage& msg) {
    Bytes inner;
    enc(inner, msg);
    pb::put_len(out, std::span<const std::uint8_t>(inner));
}

Bytes encode_to_vec(const StreamMessage& msg) {
    Bytes out;
    enc(out, msg);
    return out;
}

Bytes encode_to_vec(const Datagram& msg) {
    Bytes out;
    enc(out, msg);
    return out;
}

Bytes encode_to_vec(const InputMsg& msg) {
    Bytes out;
    enc(out, msg);
    return out;
}

std::expected<StreamMessage, loa::Error> decode_stream_message(std::span<const std::uint8_t> bytes) {
    return dec<StreamMessage>(bytes);
}

std::expected<Datagram, loa::Error> decode_datagram(std::span<const std::uint8_t> bytes) {
    return dec<Datagram>(bytes);
}

std::expected<InputMsg, loa::Error> decode_input_msg(std::span<const std::uint8_t> bytes) {
    return dec<InputMsg>(bytes);
}

// ---- contract conversions ----

std::int32_t encoding_to_wire(cs::Encoding value) noexcept {
    switch (value) {
    case cs::Encoding::Rgba: return static_cast<std::int32_t>(WireEncoding::Rgba);
    case cs::Encoding::Jpeg: return static_cast<std::int32_t>(WireEncoding::Jpeg);
    case cs::Encoding::Png: return static_cast<std::int32_t>(WireEncoding::Png);
    case cs::Encoding::H264: return static_cast<std::int32_t>(WireEncoding::H264);
    case cs::Encoding::Hevc: return static_cast<std::int32_t>(WireEncoding::Hevc);
    }
    return 0;
}

std::optional<cs::Encoding> encoding_from_wire(std::int32_t value) noexcept {
    switch (static_cast<WireEncoding>(value)) {
    case WireEncoding::Rgba: return cs::Encoding::Rgba;
    case WireEncoding::Jpeg: return cs::Encoding::Jpeg;
    case WireEncoding::Png: return cs::Encoding::Png;
    case WireEncoding::H264: return cs::Encoding::H264;
    case WireEncoding::Hevc: return cs::Encoding::Hevc;
    default: return std::nullopt;
    }
}

std::int32_t button_to_wire(cs::MouseButton value) noexcept {
    switch (value) {
    case cs::MouseButton::Left: return static_cast<std::int32_t>(WireButton::Left);
    case cs::MouseButton::Middle: return static_cast<std::int32_t>(WireButton::Middle);
    case cs::MouseButton::Right: return static_cast<std::int32_t>(WireButton::Right);
    case cs::MouseButton::Back: return static_cast<std::int32_t>(WireButton::Back);
    case cs::MouseButton::Forward: return static_cast<std::int32_t>(WireButton::Forward);
    }
    return 0;
}

std::optional<cs::MouseButton> button_from_wire(std::int32_t value) noexcept {
    switch (static_cast<WireButton>(value)) {
    case WireButton::Left: return cs::MouseButton::Left;
    case WireButton::Middle: return cs::MouseButton::Middle;
    case WireButton::Right: return cs::MouseButton::Right;
    case WireButton::Back: return cs::MouseButton::Back;
    case WireButton::Forward: return cs::MouseButton::Forward;
    default: return std::nullopt;
    }
}

std::int32_t mode_to_wire(cs::SurfaceMode value) noexcept {
    return static_cast<std::int32_t>(value == cs::SurfaceMode::Pixel ? WireMode::Pixel : WireMode::Document);
}

std::optional<cs::SurfaceMode> mode_from_wire(std::int32_t value) noexcept {
    switch (static_cast<WireMode>(value)) {
    case WireMode::Pixel: return cs::SurfaceMode::Pixel;
    case WireMode::Document: return cs::SurfaceMode::Document;
    default: return std::nullopt;
    }
}

bool is_video(cs::Encoding encoding) noexcept {
    return encoding == cs::Encoding::H264 || encoding == cs::Encoding::Hevc;
}

StreamInfoMsg stream_info_to_wire(const cs::StreamInfo& stream) {
    return {.encoding = encoding_to_wire(stream.encoding), .encoder = stream.encoder, .hardware = stream.hardware};
}

std::optional<cs::StreamInfo> stream_info_from_wire(const StreamInfoMsg& msg) {
    auto encoding = encoding_from_wire(msg.encoding);
    if (!encoding) return std::nullopt;
    return cs::StreamInfo{.encoding = *encoding, .encoder = msg.encoder, .hardware = msg.hardware};
}

SurfaceInfoMsg surface_info_to_wire(const cs::SurfaceInfo& info) {
    return {.uri = info.uri.as_str(),
            .name = info.name,
            .width = info.width,
            .height = info.height,
            .mode = mode_to_wire(info.mode),
            .mime = info.mime};
}

Result<cs::SurfaceInfo> surface_info_from_wire(const SurfaceInfoMsg& msg) {
    auto mode = mode_from_wire(msg.mode);
    if (!mode) return std::unexpected(ProtoError::protocol(std::format("unknown surface mode {}", msg.mode)));
    auto uri = cs::SurfaceUri::parse(msg.uri);
    if (!uri) return std::unexpected(ProtoError(uri.error()));
    return cs::SurfaceInfo{.uri = std::move(*uri),
                           .name = msg.name,
                           .width = msg.width,
                           .height = msg.height,
                           .mode = *mode,
                           .mime = msg.mime};
}

FrameHeader FrameHeader::from(const cs::Frame& frame) {
    return {.seq = frame.seq,
            .captured_us = frame.captured_us,
            .input_seq = frame.input_seq,
            .width = frame.width,
            .height = frame.height,
            .region_x = frame.region.x,
            .region_y = frame.region.y,
            .region_width = frame.region.width,
            .region_height = frame.region.height,
            .encoding = encoding_to_wire(frame.encoding),
            .keyframe = frame.keyframe};
}

std::optional<cs::Frame> FrameHeader::into_frame(Bytes data) const {
    auto enc = encoding_from_wire(encoding);
    if (!enc) return std::nullopt;
    return cs::Frame{.seq = seq,
                     .captured_us = captured_us,
                     .input_seq = input_seq,
                     .width = width,
                     .height = height,
                     .region = {.x = region_x, .y = region_y, .width = region_width, .height = region_height},
                     .encoding = *enc,
                     .keyframe = keyframe,
                     .data = std::move(data)};
}

std::uint32_t modifiers_to_wire(cs::Modifiers m) noexcept {
    return static_cast<std::uint32_t>(m.shift) | static_cast<std::uint32_t>(m.ctrl) << 1U |
           static_cast<std::uint32_t>(m.alt) << 2U | static_cast<std::uint32_t>(m.meta) << 3U;
}

cs::Modifiers modifiers_from_wire(std::uint32_t bits) noexcept {
    auto on = [bits](unsigned bit) { return (bits & (1U << bit)) != 0; };
    return {.shift = on(0), .ctrl = on(1), .alt = on(2), .meta = on(3)};
}

InputMsg InputMsg::from(const cs::InputEvent& event) {
    InputKind kind = std::visit(
        [](const auto& in) -> InputKind {
            using T = std::decay_t<decltype(in)>;
            if constexpr (std::is_same_v<T, cs::input::MouseMove>) {
                return MouseMoveMsg{.x = in.x, .y = in.y};
            } else if constexpr (std::is_same_v<T, cs::input::MouseButton>) {
                return MouseButtonMsg{.x = in.x,
                                      .y = in.y,
                                      .button = button_to_wire(in.button),
                                      .pressed = in.pressed,
                                      .modifiers = modifiers_to_wire(in.modifiers)};
            } else if constexpr (std::is_same_v<T, cs::input::Scroll>) {
                return ScrollMsg{.x = in.x, .y = in.y, .dx = in.dx, .dy = in.dy, .modifiers = modifiers_to_wire(in.modifiers)};
            } else if constexpr (std::is_same_v<T, cs::input::Key>) {
                return KeyMsg{.key = in.key, .code = in.code, .pressed = in.pressed, .modifiers = modifiers_to_wire(in.modifiers)};
            } else if constexpr (std::is_same_v<T, cs::input::Text>) {
                return TextMsg{.text = in.text};
            } else {
                return Resize{.width = in.width, .height = in.height};
            }
        },
        event.input);
    return {.seq = event.seq, .sent_us = event.sent_us, .kind = std::move(kind)};
}

std::optional<cs::InputEvent> InputMsg::into_event() const& {
    if (!kind) return std::nullopt;
    std::optional<cs::Input> input = std::visit(
        [](const auto& m) -> std::optional<cs::Input> {
            using T = std::decay_t<decltype(m)>;
            if constexpr (std::is_same_v<T, MouseMoveMsg>) {
                return cs::input::MouseMove{.x = m.x, .y = m.y};
            } else if constexpr (std::is_same_v<T, MouseButtonMsg>) {
                auto button = button_from_wire(m.button);
                if (!button) return std::nullopt;
                return cs::input::MouseButton{
                    .x = m.x, .y = m.y, .button = *button, .pressed = m.pressed, .modifiers = modifiers_from_wire(m.modifiers)};
            } else if constexpr (std::is_same_v<T, ScrollMsg>) {
                return cs::input::Scroll{.x = m.x, .y = m.y, .dx = m.dx, .dy = m.dy, .modifiers = modifiers_from_wire(m.modifiers)};
            } else if constexpr (std::is_same_v<T, KeyMsg>) {
                return cs::input::Key{.key = m.key, .code = m.code, .pressed = m.pressed, .modifiers = modifiers_from_wire(m.modifiers)};
            } else if constexpr (std::is_same_v<T, TextMsg>) {
                return cs::input::Text{.text = m.text};
            } else {
                return cs::input::Resize{.width = m.width, .height = m.height};
            }
        },
        *kind);
    if (!input) return std::nullopt;
    return cs::InputEvent{.seq = seq, .sent_us = sent_us, .input = std::move(*input)};
}

} // namespace loa::surfaces::proto
