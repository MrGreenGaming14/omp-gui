#pragma once

// Raw frame capture from an X server (the virtual display fallback). Port of
// `loa-surfaces/src/browser/x11.rs`. Linux only.
//
// Frames are read with core `GetImage` on the root window, converted to RGBA
// in place, and diffed against the previous frame so a mostly static page
// sends only the band of rows that changed.

#if defined(__linux__)

#include <cstddef>
#include <cstdint>
#include <filesystem>
#include <memory>
#include <optional>
#include <utility>

#include "loa/contracts/surface.hpp"
#include "loa/surfaces/browser/error.hpp"

struct xcb_connection_t;

namespace loa::surfaces::browser {

// A connection to an X server that captures its first screen.
class X11Capture {
  public:
    // Connects over the server's Unix socket (e.g. `/tmp/.X11-unix/X99`).
    [[nodiscard]] static Result<X11Capture> connect(const std::filesystem::path& socket);

    X11Capture(X11Capture&& other) noexcept;
    X11Capture& operator=(X11Capture&& other) noexcept;
    X11Capture(const X11Capture&) = delete;
    X11Capture& operator=(const X11Capture&) = delete;
    ~X11Capture();

    // Size of the X screen.
    [[nodiscard]] std::pair<std::uint32_t, std::uint32_t> screen_size() const { return {screen_width_, screen_height_}; }

    // Captures the top left `width` x `height` (clipped to the screen) as an
    // RGBA frame without numbering or timing. Returns empty when nothing
    // changed since the last capture, a band of changed rows otherwise, and
    // the whole picture when `keyframe` is set or the size changed.
    [[nodiscard]] Result<std::optional<loa::contracts::surface::Frame>> grab(std::uint32_t width, std::uint32_t height,
                                                                            bool keyframe);

  private:
    // Byte offsets of red, green and blue inside one 32 bit server pixel.
    struct PixelLayout {
        std::size_t red, green, blue;
    };
    struct Previous {
        std::uint32_t width, height;
        loa::contracts::surface::Bytes rgba;
    };

    X11Capture() = default;

    xcb_connection_t* conn_ = nullptr;
    std::uint32_t root_ = 0;
    std::uint32_t screen_width_ = 0;
    std::uint32_t screen_height_ = 0;
    PixelLayout layout_{};
    std::optional<Previous> previous_;
};

} // namespace loa::surfaces::browser

#endif
