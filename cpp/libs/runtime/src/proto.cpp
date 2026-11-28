#include "loa/runtime/proto.hpp"

namespace loa::runtime::proto {
namespace {

std::unexpected<loa::Error> truncated(std::string_view what) {
    return std::unexpected(loa::Error(ErrorCode::parse, "proto: truncated " + std::string(what)));
}

} // namespace

void put_varint(Bytes& out, std::uint64_t v) {
    while (v >= 0x80) {
        out.push_back(static_cast<std::uint8_t>(v | 0x80));
        v >>= 7;
    }
    out.push_back(static_cast<std::uint8_t>(v));
}

void put_fixed32(Bytes& out, std::uint32_t v) {
    for (int i = 0; i < 4; ++i) out.push_back(static_cast<std::uint8_t>(v >> (8 * i)));
}

void put_fixed64(Bytes& out, std::uint64_t v) {
    for (int i = 0; i < 8; ++i) out.push_back(static_cast<std::uint8_t>(v >> (8 * i)));
}

void put_tag(Bytes& out, std::uint32_t field, WireType type) {
    put_varint(out, (static_cast<std::uint64_t>(field) << 3) | static_cast<std::uint64_t>(type));
}

void put_len(Bytes& out, std::span<const std::uint8_t> bytes) {
    put_varint(out, bytes.size());
    out.insert(out.end(), bytes.begin(), bytes.end());
}

void put_len(Bytes& out, std::string_view bytes) {
    put_varint(out, bytes.size());
    out.insert(out.end(), bytes.begin(), bytes.end());
}

std::size_t varint_size(std::uint64_t v) noexcept {
    std::size_t n = 1;
    while (v >= 0x80) {
        v >>= 7;
        ++n;
    }
    return n;
}

std::expected<std::uint64_t, loa::Error> Reader::varint() {
    std::uint64_t v = 0;
    for (int shift = 0; shift < 70; shift += 7) {
        if (pos_ >= data_.size()) return truncated("varint");
        const std::uint8_t b = data_[pos_++];
        if (shift == 63 && b > 1) return std::unexpected(loa::Error(ErrorCode::parse, "proto: varint overflow"));
        v |= static_cast<std::uint64_t>(b & 0x7f) << shift;
        if ((b & 0x80) == 0) return v;
    }
    return std::unexpected(loa::Error(ErrorCode::parse, "proto: varint too long"));
}

std::expected<std::uint32_t, loa::Error> Reader::fixed32() {
    if (data_.size() - pos_ < 4) return truncated("fixed32");
    std::uint32_t v = 0;
    for (int i = 0; i < 4; ++i) v |= static_cast<std::uint32_t>(data_[pos_++]) << (8 * i);
    return v;
}

std::expected<std::uint64_t, loa::Error> Reader::fixed64() {
    if (data_.size() - pos_ < 8) return truncated("fixed64");
    std::uint64_t v = 0;
    for (int i = 0; i < 8; ++i) v |= static_cast<std::uint64_t>(data_[pos_++]) << (8 * i);
    return v;
}

std::expected<Tag, loa::Error> Reader::tag() {
    auto key = varint();
    if (!key) return std::unexpected(key.error());
    const auto field = *key >> 3;
    const auto type = static_cast<std::uint8_t>(*key & 7);
    if (field == 0 || field > 0x1fffffff) return std::unexpected(loa::Error(ErrorCode::parse, "proto: invalid field number"));
    if (type > 5) return std::unexpected(loa::Error(ErrorCode::parse, "proto: invalid wire type " + std::to_string(type)));
    return Tag{static_cast<std::uint32_t>(field), static_cast<WireType>(type)};
}

std::expected<std::span<const std::uint8_t>, loa::Error> Reader::len() {
    auto n = varint();
    if (!n) return std::unexpected(n.error());
    if (*n > data_.size() - pos_) return truncated("length-delimited field");
    auto out = data_.subspan(pos_, static_cast<std::size_t>(*n));
    pos_ += out.size();
    return out;
}

std::expected<void, loa::Error> Reader::skip(Tag tag) {
    switch (tag.type) {
    case WireType::varint:
        if (auto r = varint(); !r) return std::unexpected(r.error());
        return {};
    case WireType::fixed64:
        if (auto r = fixed64(); !r) return std::unexpected(r.error());
        return {};
    case WireType::fixed32:
        if (auto r = fixed32(); !r) return std::unexpected(r.error());
        return {};
    case WireType::len:
        if (auto r = len(); !r) return std::unexpected(r.error());
        return {};
    case WireType::start_group:
        for (;;) {
            auto inner = this->tag();
            if (!inner) return std::unexpected(inner.error());
            if (inner->type == WireType::end_group) {
                if (inner->field != tag.field) return std::unexpected(loa::Error(ErrorCode::parse, "proto: mismatched end group"));
                return {};
            }
            if (auto r = skip(*inner); !r) return r;
        }
    case WireType::end_group:
        return std::unexpected(loa::Error(ErrorCode::parse, "proto: unexpected end group"));
    }
    return std::unexpected(loa::Error(ErrorCode::parse, "proto: invalid wire type"));
}

} // namespace loa::runtime::proto
