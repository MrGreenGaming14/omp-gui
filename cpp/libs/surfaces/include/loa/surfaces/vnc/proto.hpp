#pragma once

// RFB wire format (RFC 6143): the handshake constants and the client to server
// messages. Port of `loa-surfaces/src/vnc/proto.rs`.

#include <array>
#include <cstddef>
#include <cstdint>
#include <optional>
#include <span>
#include <string>
#include <vector>

#include "loa/surfaces/vnc/vnc.hpp"

namespace loa::surfaces::vnc {

// The pixel format the client asks for: 32 bits per pixel, depth 24, little
// endian true color with red in the lowest byte. A pixel on the wire is then
// `R G B X`, and a ZRLE CPIXEL is `R G B`.
inline constexpr std::array<std::uint8_t, 16> PIXEL_FORMAT = {32, 24, 0, 1, 0, 255, 0, 255, 0, 255, 0, 8, 16, 0, 0, 0};

// Bytes per pixel in `PIXEL_FORMAT`.
inline constexpr std::size_t BPP = 4;

// What the server says about the desktop after the handshake.
struct ServerInit {
    std::uint16_t width = 0;
    std::uint16_t height = 0;
    std::string name;

    friend bool operator==(const ServerInit&, const ServerInit&) = default;
};

// A client to server message.
struct Msg {
    enum class Type : std::uint8_t { SetPixelFormat, SetEncodings, UpdateRequest, Key, Pointer };

    [[nodiscard]] static Msg set_pixel_format() { return Msg{Type::SetPixelFormat}; }
    [[nodiscard]] static Msg set_encodings(std::span<const std::int32_t> list) {
        Msg m{Type::SetEncodings};
        m.encodings = list;
        return m;
    }
    // FramebufferUpdateRequest for the whole `width` x `height` framebuffer.
    [[nodiscard]] static Msg update_request(bool incremental, std::uint16_t width, std::uint16_t height) {
        Msg m{Type::UpdateRequest};
        m.flag = incremental;
        m.x = width;
        m.y = height;
        return m;
    }
    [[nodiscard]] static Msg key(bool down, std::uint32_t keysym) {
        Msg m{Type::Key};
        m.flag = down;
        m.keysym = keysym;
        return m;
    }
    [[nodiscard]] static Msg pointer(std::uint8_t mask, std::uint16_t x, std::uint16_t y) {
        Msg m{Type::Pointer};
        m.mask = mask;
        m.x = x;
        m.y = y;
        return m;
    }

    // Appends the message's bytes to `buf`.
    void write(std::vector<std::uint8_t>& buf) const;
    // The message's bytes.
    [[nodiscard]] std::vector<std::uint8_t> bytes() const {
        std::vector<std::uint8_t> buf;
        write(buf);
        return buf;
    }

    Type type;
    std::span<const std::int32_t> encodings{};
    // `incremental` for UpdateRequest, `down` for Key.
    bool flag = false;
    std::uint8_t mask = 0;
    // Pointer position, or the UpdateRequest width and height.
    std::uint16_t x = 0;
    std::uint16_t y = 0;
    std::uint32_t keysym = 0;
};

// The minor version the client answers a 12 byte server banner with: 3, 7 or 8.
[[nodiscard]] RfbResult<std::uint32_t> parse_version(std::span<const std::uint8_t> banner);

// The response to a VNC Authentication challenge: the challenge encrypted with
// DES in ECB mode, keyed by the first eight bytes of `password` (zero padded)
// with every key byte bit-mirrored as VNC servers expect.
[[nodiscard]] std::array<std::uint8_t, 16> vnc_response(std::span<const std::uint8_t> password,
                                                       const std::array<std::uint8_t, 16>& challenge);

} // namespace loa::surfaces::vnc
