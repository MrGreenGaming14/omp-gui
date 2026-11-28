#include "loa/surfaces/browser/x11.hpp"

#if defined(__linux__)

#include <sys/socket.h>
#include <sys/un.h>
#include <unistd.h>
#include <xcb/xcb.h>

#include <bit>
#include <cerrno>
#include <cstdlib>
#include <cstring>
#include <string>

#include "loa/surfaces/pixels.hpp"

namespace loa::surfaces::browser {

namespace cs = loa::contracts::surface;

namespace {

std::string hex(std::uint32_t v) {
    char buf[16];
    std::snprintf(buf, sizeof buf, "%#x", v);
    return buf;
}

// Byte index of the 8 bit channel selected by `mask` within a 32 bit pixel.
Result<std::size_t> channel_byte(std::uint32_t mask, bool msb_first) {
    const int shift = mask == 0 ? 32 : std::countr_zero(mask);
    if (shift >= 32 || (mask >> shift) != 0xff || shift % 8 != 0) {
        return std::unexpected(BrowserError::x11("unsupported channel mask " + hex(mask)));
    }
    const auto index = std::size_t(shift / 8);
    return msb_first ? 3 - index : index;
}

const char* connection_error(int code) {
    switch (code) {
    case XCB_CONN_ERROR: return "connection broken";
    case XCB_CONN_CLOSED_EXT_NOTSUPPORTED: return "extension not supported";
    case XCB_CONN_CLOSED_MEM_INSUFFICIENT: return "out of memory";
    case XCB_CONN_CLOSED_REQ_LEN_EXCEED: return "request too long";
    case XCB_CONN_CLOSED_PARSE_ERR: return "display parse error";
    case XCB_CONN_CLOSED_INVALID_SCREEN: return "invalid screen";
    case XCB_CONN_CLOSED_FDPASSING_FAILED: return "fd passing failed";
    default: return "connection failed";
    }
}

} // namespace

X11Capture::X11Capture(X11Capture&& other) noexcept
    : conn_(std::exchange(other.conn_, nullptr)), root_(other.root_), screen_width_(other.screen_width_),
      screen_height_(other.screen_height_), layout_(other.layout_), previous_(std::move(other.previous_)) {}

X11Capture& X11Capture::operator=(X11Capture&& other) noexcept {
    if (this != &other) {
        if (conn_) xcb_disconnect(conn_);
        conn_ = std::exchange(other.conn_, nullptr);
        root_ = other.root_;
        screen_width_ = other.screen_width_;
        screen_height_ = other.screen_height_;
        layout_ = other.layout_;
        previous_ = std::move(other.previous_);
    }
    return *this;
}

X11Capture::~X11Capture() {
    if (conn_) xcb_disconnect(conn_);
}

Result<X11Capture> X11Capture::connect(const std::filesystem::path& socket) {
    const auto path = socket.string();
    sockaddr_un addr{};
    addr.sun_family = AF_UNIX;
    if (path.size() >= sizeof addr.sun_path) return std::unexpected(BrowserError::io("socket path too long: " + path));
    std::memcpy(addr.sun_path, path.c_str(), path.size() + 1);
    const int fd = ::socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
    if (fd < 0) return std::unexpected(BrowserError::io(std::strerror(errno)));
    if (::connect(fd, reinterpret_cast<const sockaddr*>(&addr), sizeof addr) != 0) {
        const int err = errno;
        ::close(fd);
        return std::unexpected(BrowserError::io(std::strerror(err)));
    }
    X11Capture capture;
    // xcb owns `fd` from here and closes it on disconnect.
    capture.conn_ = xcb_connect_to_fd(fd, nullptr);
    if (const int err = xcb_connection_has_error(capture.conn_)) return std::unexpected(BrowserError::x11(connection_error(err)));
    const xcb_setup_t* setup = xcb_get_setup(capture.conn_);
    auto roots = xcb_setup_roots_iterator(setup);
    if (roots.rem == 0) return std::unexpected(BrowserError::x11("the server has no screen"));
    const xcb_screen_t* screen = roots.data;
    std::optional<std::uint8_t> bits;
    for (auto it = xcb_setup_pixmap_formats_iterator(setup); it.rem; xcb_format_next(&it)) {
        if (it.data->depth == screen->root_depth) {
            bits = it.data->bits_per_pixel;
            break;
        }
    }
    if (!bits) {
        return std::unexpected(BrowserError::x11("no pixmap format for depth " + std::to_string(screen->root_depth)));
    }
    const xcb_visualtype_t* visual = nullptr;
    for (auto d = xcb_screen_allowed_depths_iterator(screen); d.rem && !visual; xcb_depth_next(&d)) {
        for (auto v = xcb_depth_visuals_iterator(d.data); v.rem; xcb_visualtype_next(&v)) {
            if (v.data->visual_id == screen->root_visual) {
                visual = v.data;
                break;
            }
        }
    }
    if (!visual) return std::unexpected(BrowserError::x11("root visual not described"));
    if (*bits != 32) {
        return std::unexpected(BrowserError::x11(std::to_string(*bits) + " bits per pixel, only 32 is supported"));
    }
    const bool msb = setup->image_byte_order == XCB_IMAGE_ORDER_MSB_FIRST;
    auto red = channel_byte(visual->red_mask, msb);
    if (!red) return std::unexpected(red.error());
    auto green = channel_byte(visual->green_mask, msb);
    if (!green) return std::unexpected(green.error());
    auto blue = channel_byte(visual->blue_mask, msb);
    if (!blue) return std::unexpected(blue.error());
    capture.layout_ = {*red, *green, *blue};
    capture.root_ = screen->root;
    capture.screen_width_ = screen->width_in_pixels;
    capture.screen_height_ = screen->height_in_pixels;
    return capture;
}

Result<std::optional<cs::Frame>> X11Capture::grab(std::uint32_t width, std::uint32_t height, bool keyframe) {
    width = std::min(width, screen_width_);
    height = std::min(height, screen_height_);
    if (width == 0 || height == 0) return std::nullopt;
    // Both fit in u16: they are clipped to the screen size, which X sends as u16.
    const auto cookie = xcb_get_image(conn_, XCB_IMAGE_FORMAT_Z_PIXMAP, root_, 0, 0, std::uint16_t(width),
                                      std::uint16_t(height), ~0U);
    xcb_generic_error_t* error = nullptr;
    xcb_get_image_reply_t* reply = xcb_get_image_reply(conn_, cookie, &error);
    if (!reply) {
        std::string text = error ? "GetImage failed with error code " + std::to_string(error->error_code)
                                 : std::string(connection_error(xcb_connection_has_error(conn_)));
        std::free(error);
        return std::unexpected(BrowserError::x11(std::move(text)));
    }
    const auto* bytes = xcb_get_image_data(reply);
    const auto len = std::size_t(xcb_get_image_data_length(reply));
    const std::size_t row = std::size_t(width) * 4;
    if (len != row * height) {
        std::free(reply);
        return std::unexpected(BrowserError::x11("GetImage sent " + std::to_string(len) + " bytes for " +
                                                 std::to_string(width) + "x" + std::to_string(height)));
    }
    cs::Bytes full(len);
    // Rewrites server pixels as RGBA.
    for (std::size_t i = 0; i < len; i += 4) {
        full[i] = bytes[i + layout_.red];
        full[i + 1] = bytes[i + layout_.green];
        full[i + 2] = bytes[i + layout_.blue];
        full[i + 3] = 255;
    }
    std::free(reply);

    std::optional<std::pair<std::uint32_t, std::uint32_t>> band;
    if (previous_ && !keyframe && previous_->width == width && previous_->height == height) {
        band = changed_rows(previous_->rgba, full, width);
        if (!band) return std::nullopt;
    }
    previous_ = Previous{width, height, full};
    cs::Frame frame;
    frame.width = width;
    frame.height = height;
    frame.encoding = cs::Encoding::Rgba;
    if (band && (band->first > 0 || band->second + 1 < height)) {
        const auto [first, last] = *band;
        frame.region = {0, first, width, last + 1 - first};
        frame.keyframe = false;
        frame.data.assign(full.begin() + std::ptrdiff_t(first * row), full.begin() + std::ptrdiff_t((last + 1) * row));
    } else {
        frame.region = {0, 0, width, height};
        frame.keyframe = true;
        frame.data = std::move(full);
    }
    return frame;
}

} // namespace loa::surfaces::browser

#endif
