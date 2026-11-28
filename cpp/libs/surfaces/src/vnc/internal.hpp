#pragma once

// Library internal helpers of the RFB client. Inside coroutines errors travel
// as a thrown `Failure`; every public entry point catches it and returns the
// `RfbError`, so no exception leaves the library.

#include <asio/awaitable.hpp>

#include <cstdint>
#include <format>
#include <string>
#include <functional>
#include <span>
#include <string_view>
#include <vector>

#include "loa/surfaces/vnc/decode.hpp"
#include "loa/surfaces/vnc/proto.hpp"
#include "loa/surfaces/vnc/vnc.hpp"

namespace loa::surfaces::vnc::detail {

struct Failure {
    RfbError error;
};

[[noreturn]] inline void fail(RfbError error) { throw Failure{std::move(error)}; }

inline std::string rect_debug(Rect r) {
    return std::format("Rect {{ x: {}, y: {}, width: {}, height: {} }}", r.x, r.y, r.width, r.height);
}

// Decodes UTF-8 `s` into code points (invalid bytes become U+FFFD).
std::u32string utf8_decode(std::string_view s);
// Encodes one code point as UTF-8.
std::string utf8_encode(char32_t c);

// Writes all of `bytes`; throws on failure.
using WriteFn = std::function<asio::awaitable<void>(std::span<const std::uint8_t> bytes)>;

// Runs the handshake up to ServerInit, then sends SetPixelFormat, SetEncodings
// and a full FramebufferUpdateRequest. Throws `Failure`.
asio::awaitable<ServerInit> handshake(Reader& rd, const WriteFn& write, const std::string& endpoint,
                                      PasswordSource& passwords, bool shared);

} // namespace loa::surfaces::vnc::detail
