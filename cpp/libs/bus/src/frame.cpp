#include "loa/bus/frame.hpp"

#include <zlib.h>

#include <istream>

namespace loa::bus::frame {

std::uint32_t le_u32(std::span<const std::uint8_t> b, std::size_t i) noexcept {
    return std::uint32_t{b[i]} | std::uint32_t{b[i + 1]} << 8 | std::uint32_t{b[i + 2]} << 16 |
           std::uint32_t{b[i + 3]} << 24;
}

std::uint64_t le_u64(std::span<const std::uint8_t> b, std::size_t i) noexcept {
    return std::uint64_t{le_u32(b, i)} | std::uint64_t{le_u32(b, i + 4)} << 32;
}

void put_le_u32(std::uint8_t* out, std::uint32_t v) noexcept {
    for (int k = 0; k < 4; ++k) out[k] = static_cast<std::uint8_t>(v >> (8 * k));
}

std::uint32_t crc32(std::span<const std::uint8_t> bytes) noexcept {
    return static_cast<std::uint32_t>(::crc32_z(0, bytes.data(), bytes.size()));
}

std::array<std::uint8_t, preamble_len> preamble(const Magic& magic, std::uint32_t version) noexcept {
    std::array<std::uint8_t, preamble_len> out{};
    for (std::size_t i = 0; i < magic.size(); ++i) out[i] = magic[i];
    put_le_u32(out.data() + 8, version);
    return out;
}

std::expected<void, std::size_t> seal(Bytes& buf, std::size_t start) {
    const std::size_t body = start + header_len;
    const std::size_t len = buf.size() - body;
    if (len > max_payload) return std::unexpected(len);
    const std::uint32_t crc = crc32(std::span(buf).subspan(body));
    put_le_u32(buf.data() + start, static_cast<std::uint32_t>(len));
    put_le_u32(buf.data() + start + 4, crc);
    return {};
}

namespace {

loa::Error io_error(std::istream& in) {
    return loa::Error(loa::ErrorCode::io, in.eof() ? "failed to fill whole buffer" : "read failed");
}

}  // namespace

std::expected<Frame, loa::Error> read(std::istream& in, std::uint64_t room, Bytes& buf) try {
    if (room < header_len) return Frame{Kind::short_, 0};
    std::array<std::uint8_t, header_len> h{};
    if (!in.read(reinterpret_cast<char*>(h.data()), h.size())) return std::unexpected(io_error(in));
    const std::uint32_t len = le_u32(h, 0);
    if (header_len + len > room) return Frame{Kind::short_, 0};
    if (len == 0 || len > max_payload) return Frame{Kind::bad_len, len};
    buf.resize(len);
    if (!in.read(reinterpret_cast<char*>(buf.data()), len)) return std::unexpected(io_error(in));
    return Frame{crc32(buf) == le_u32(h, 4) ? Kind::whole : Kind::bad_crc, len};
}
catch (const std::ios_base::failure&) {
    return std::unexpected(io_error(in));
}

View read(std::span<const std::uint8_t> in) noexcept {
    if (in.size() < header_len) return {{Kind::short_, 0}, {}};
    const std::uint32_t len = le_u32(in, 0);
    if (header_len + len > in.size()) return {{Kind::short_, 0}, {}};
    if (len == 0 || len > max_payload) return {{Kind::bad_len, len}, {}};
    const auto payload = in.subspan(header_len, len);
    if (crc32(payload) != le_u32(in, 4)) return {{Kind::bad_crc, len}, {}};
    return {{Kind::whole, len}, payload};
}

}  // namespace loa::bus::frame
