#include "wire.hpp"

#include <array>

namespace loa::remote::wire {

std::optional<std::uint8_t> Reader::u8() {
    if (data_.size() - off_ < 1 || off_ > data_.size()) return std::nullopt;
    return static_cast<std::uint8_t>(data_[off_++]);
}

std::optional<std::uint32_t> Reader::u32() {
    if (off_ > data_.size() || data_.size() - off_ < 4) return std::nullopt;
    std::uint32_t v = 0;
    for (int i = 0; i < 4; ++i) v = (v << 8) | static_cast<std::uint8_t>(data_[off_++]);
    return v;
}

std::optional<std::uint64_t> Reader::u64() {
    if (off_ > data_.size() || data_.size() - off_ < 8) return std::nullopt;
    std::uint64_t v = 0;
    for (int i = 0; i < 8; ++i) v = (v << 8) | static_cast<std::uint8_t>(data_[off_++]);
    return v;
}

std::optional<std::string_view> Reader::raw(std::size_t n) {
    if (off_ > data_.size() || data_.size() - off_ < n) return std::nullopt;
    const auto out = data_.substr(off_, n);
    off_ += n;
    return out;
}

std::optional<std::string_view> Reader::string() {
    const auto n = u32();
    if (!n) return std::nullopt;
    return raw(*n);
}

void put_u8(Bytes& out, std::uint8_t v) { out.push_back(static_cast<char>(v)); }

void put_u32(Bytes& out, std::uint32_t v) {
    for (int s = 24; s >= 0; s -= 8) out.push_back(static_cast<char>((v >> s) & 0xff));
}

void put_u64(Bytes& out, std::uint64_t v) {
    for (int s = 56; s >= 0; s -= 8) out.push_back(static_cast<char>((v >> s) & 0xff));
}

void put_string(Bytes& out, std::string_view s) {
    put_u32(out, static_cast<std::uint32_t>(s.size()));
    out.append(s);
}

namespace {

constexpr std::string_view ALPHABET = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
constexpr std::uint8_t INVALID = 0xff;

constexpr std::array<std::uint8_t, 256> table() {
    std::array<std::uint8_t, 256> t{};
    for (auto& v : t) v = INVALID;
    for (std::size_t i = 0; i < ALPHABET.size(); ++i) t[static_cast<std::uint8_t>(ALPHABET[i])] = static_cast<std::uint8_t>(i);
    return t;
}

constexpr auto TABLE = table();

std::string invalid_byte(std::size_t offset, std::uint8_t byte) {
    return "Invalid symbol " + std::to_string(byte) + ", offset " + std::to_string(offset) + ".";
}

} // namespace

std::string base64_encode(std::string_view bytes) {
    std::string out;
    out.reserve((bytes.size() + 2) / 3 * 4);
    std::size_t i = 0;
    for (; i + 3 <= bytes.size(); i += 3) {
        const std::uint32_t n = (static_cast<std::uint8_t>(bytes[i]) << 16) | (static_cast<std::uint8_t>(bytes[i + 1]) << 8) |
                                static_cast<std::uint8_t>(bytes[i + 2]);
        out += ALPHABET[(n >> 18) & 63];
        out += ALPHABET[(n >> 12) & 63];
        out += ALPHABET[(n >> 6) & 63];
        out += ALPHABET[n & 63];
    }
    const auto left = bytes.size() - i;
    if (left == 1) {
        const std::uint32_t n = static_cast<std::uint8_t>(bytes[i]) << 16;
        out += ALPHABET[(n >> 18) & 63];
        out += ALPHABET[(n >> 12) & 63];
        out += "==";
    } else if (left == 2) {
        const std::uint32_t n = (static_cast<std::uint8_t>(bytes[i]) << 16) | (static_cast<std::uint8_t>(bytes[i + 1]) << 8);
        out += ALPHABET[(n >> 18) & 63];
        out += ALPHABET[(n >> 12) & 63];
        out += ALPHABET[(n >> 6) & 63];
        out += '=';
    }
    return out;
}

std::optional<Bytes> base64_decode(std::string_view in, std::string* error) {
    const auto fail = [&](std::string message) -> std::optional<Bytes> {
        if (error) *error = std::move(message);
        return std::nullopt;
    };
    const auto byte = [&](std::size_t i) { return static_cast<std::uint8_t>(in[i]); };
    const auto rem = in.size() % 4;
    if (rem == 1) {
        const auto last = byte(in.size() - 1);
        if (last != '=' && TABLE[last] == INVALID) return fail(invalid_byte(in.size() - 1, last));
    }
    std::size_t quads = in.size() - rem;
    if (rem == 0) quads = quads >= 4 ? quads - 4 : 0;
    Bytes out;
    out.reserve(in.size() / 4 * 3 + 3);
    for (std::size_t i = 0; i < quads; i += 4) {
        std::uint32_t n = 0;
        for (std::size_t j = 0; j < 4; ++j) {
            const auto b = byte(i + j);
            if (TABLE[b] == INVALID) return fail(invalid_byte(i + j, b));
            n = (n << 6) | TABLE[b];
        }
        out.push_back(static_cast<char>((n >> 16) & 0xff));
        out.push_back(static_cast<char>((n >> 8) & 0xff));
        out.push_back(static_cast<char>(n & 0xff));
    }
    std::array<std::uint8_t, 4> morsels{};
    std::size_t count = 0, padding = 0, first_pad = 0;
    std::uint8_t last_symbol = 0;
    for (std::size_t li = 0; quads + li < in.size(); ++li) {
        const auto b = byte(quads + li);
        if (b == '=') {
            if (li < 2) return fail(invalid_byte(quads + li, b));
            if (padding == 0) first_pad = li;
            ++padding;
            continue;
        }
        if (padding > 0) return fail(invalid_byte(quads + first_pad, '='));
        last_symbol = b;
        if (TABLE[b] == INVALID) return fail(invalid_byte(quads + li, b));
        morsels[count++] = TABLE[b];
    }
    if (!in.empty() && count < 2) return fail("Invalid input length: " + std::to_string(quads + count));
    if ((padding + count) % 4 != 0) return fail("Invalid padding");
    const auto bytes = count * 6 / 8;
    std::uint32_t num = (std::uint32_t{morsels[0]} << 26) | (std::uint32_t{morsels[1]} << 20) | (std::uint32_t{morsels[2]} << 14) |
                        (std::uint32_t{morsels[3]} << 8);
    const std::uint32_t mask = ~std::uint32_t{0} >> (bytes * 8);
    if ((num & mask) != 0) {
        return fail("Invalid last symbol " + std::to_string(last_symbol) + ", offset " + std::to_string(quads + count - 1) + ".");
    }
    for (std::size_t i = 0; i < bytes; ++i) {
        out.push_back(static_cast<char>(num >> 24));
        num <<= 8;
    }
    return out;
}

} // namespace loa::remote::wire
