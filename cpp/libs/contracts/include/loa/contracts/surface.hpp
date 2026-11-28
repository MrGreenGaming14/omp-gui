#pragma once

// Surfaces: `surface://<server>/<name>` resources that stream frames and take
// input, or hand over a document. Port of `loa-contracts/src/surface.rs`.

#include <cstdint>
#include <expected>
#include <memory>
#include <optional>
#include <span>
#include <string>
#include <string_view>
#include <variant>
#include <vector>

#include "loa/contracts/async.hpp"
#include "loa/core/schema.hpp"
#include "loa/runtime/error.hpp"

namespace loa::contracts::surface {

using loa::core::SurfaceMode;

// ALPN of the frame and input stream.
inline constexpr std::string_view ALPN = "omp/surface/1";
// URI scheme of surface resources.
inline constexpr std::string_view SCHEME = "surface";

// Owned bytes (Rust `bytes::Bytes`).
using Bytes = std::vector<std::uint8_t>;

// `surface://<server>/<name>`.
class SurfaceUri;

class SurfaceError {
  public:
    enum class Kind : std::uint8_t { Uri, NotFound, Mode, Closed, Backend };

    [[nodiscard]] static SurfaceError uri(std::string text);
    [[nodiscard]] static SurfaceError not_found(const SurfaceUri& uri);
    [[nodiscard]] static SurfaceError mode(const SurfaceUri& uri, SurfaceMode mode);
    [[nodiscard]] static SurfaceError closed();
    [[nodiscard]] static SurfaceError backend(std::string message);

    [[nodiscard]] Kind kind() const noexcept { return kind_; }
    // The URI text (Uri, NotFound, Mode) or backend message (Backend).
    [[nodiscard]] const std::string& detail() const noexcept { return detail_; }
    // Set for `Mode` only.
    [[nodiscard]] std::optional<SurfaceMode> surface_mode() const noexcept { return mode_; }
    // The Rust `Display` text.
    [[nodiscard]] std::string message() const;
    [[nodiscard]] loa::Error to_error() const;

    friend bool operator==(const SurfaceError&, const SurfaceError&) = default;

  private:
    SurfaceError(Kind kind, std::string detail, std::optional<SurfaceMode> mode = std::nullopt)
        : kind_(kind), detail_(std::move(detail)), mode_(mode) {}

    Kind kind_;
    std::string detail_;
    std::optional<SurfaceMode> mode_;
};

template <typename T>
using Result = std::expected<T, SurfaceError>;

class SurfaceUri {
  public:
    // Accepts `surface://<server>/<name>` with a non-empty server and name.
    [[nodiscard]] static Result<SurfaceUri> parse(std::string_view text);
    [[nodiscard]] static Result<SurfaceUri> make(std::string_view server, std::string_view name);

    [[nodiscard]] const std::string& as_str() const noexcept { return text_; }
    [[nodiscard]] std::string_view server() const noexcept;
    // Everything after the server, e.g. `tab1`.
    [[nodiscard]] std::string_view name() const noexcept;

    friend bool operator==(const SurfaceUri&, const SurfaceUri&) = default;
    friend auto operator<=>(const SurfaceUri&, const SurfaceUri&) = default;

  private:
    explicit SurfaceUri(std::string text) : text_(std::move(text)) {}
    std::string text_;
};

// What a server declares for each surface.
struct SurfaceInfo {
    SurfaceUri uri;
    std::string name;
    // Surface pixels; for documents, the first page.
    std::uint32_t width = 0;
    std::uint32_t height = 0;
    SurfaceMode mode = SurfaceMode::Pixel;
    std::optional<std::string> mime;

    friend bool operator==(const SurfaceInfo&, const SurfaceInfo&) = default;
};

struct Rect {
    std::uint32_t x = 0;
    std::uint32_t y = 0;
    std::uint32_t width = 0;
    std::uint32_t height = 0;

    friend bool operator==(const Rect&, const Rect&) = default;
};

enum class Encoding : std::uint8_t {
    Rgba, // 8 bit RGBA, row major, no padding.
    Jpeg,
    Png,
    H264, // Annex B access units.
    Hevc,
};

// One update of a pixel surface.
struct Frame {
    // Counts up per surface; older frames are dropped.
    std::uint64_t seq = 0;
    std::uint64_t captured_us = 0;
    // Highest `InputEvent::seq` whose effect is in this frame.
    std::uint64_t input_seq = 0;
    std::uint32_t width = 0;
    std::uint32_t height = 0;
    // Part of the surface `data` covers; video covers it all.
    Rect region;
    Encoding encoding = Encoding::Rgba;
    bool keyframe = false;
    Bytes data;

    friend bool operator==(const Frame&, const Frame&) = default;
};

enum class MouseButton : std::uint8_t { Left, Middle, Right, Back, Forward };

struct Modifiers {
    bool shift = false;
    bool ctrl = false;
    bool alt = false;
    bool meta = false;

    friend bool operator==(const Modifiers&, const Modifiers&) = default;
};

// Input in surface coordinates (pixels from the top left).
namespace input {
struct MouseMove {
    float x = 0, y = 0;
    friend bool operator==(const MouseMove&, const MouseMove&) = default;
};
struct MouseButton {
    float x = 0, y = 0;
    surface::MouseButton button = surface::MouseButton::Left;
    bool pressed = false;
    Modifiers modifiers;
    friend bool operator==(const MouseButton&, const MouseButton&) = default;
};
// Positive `dy` scrolls the content down.
struct Scroll {
    float x = 0, y = 0, dx = 0, dy = 0;
    Modifiers modifiers;
    friend bool operator==(const Scroll&, const Scroll&) = default;
};
// W3C `KeyboardEvent.key` and `.code`.
struct Key {
    std::string key;
    std::string code;
    bool pressed = false;
    Modifiers modifiers;
    friend bool operator==(const Key&, const Key&) = default;
};
// Committed text (typing, paste, IME).
struct Text {
    std::string text;
    friend bool operator==(const Text&, const Text&) = default;
};
// The viewer's panel changed size.
struct Resize {
    std::uint32_t width = 0, height = 0;
    friend bool operator==(const Resize&, const Resize&) = default;
};
} // namespace input

using Input = std::variant<input::MouseMove, input::MouseButton, input::Scroll, input::Key, input::Text, input::Resize>;

struct InputEvent {
    // Counts up per viewer from 1; echoed in `Frame::input_seq`.
    std::uint64_t seq = 0;
    std::uint64_t sent_us = 0;
    Input input;

    friend bool operator==(const InputEvent&, const InputEvent&) = default;
};

// The file behind a document surface.
struct Document {
    std::string name;
    std::string mime;
    Bytes data;

    friend bool operator==(const Document&, const Document&) = default;
};

// What a pixel surface streams right now, for the viewer's header.
struct StreamInfo {
    Encoding encoding = Encoding::Rgba;
    // e.g. `ffmpeg h264_vaapi`, `rusty_h264`, `screencast`.
    std::string encoder;
    bool hardware = false;

    friend bool operator==(const StreamInfo&, const StreamInfo&) = default;
};

// A receiver of one surface's frames (the Rust `broadcast::Receiver<Arc<Frame>>`);
// defined in `surface_channel.hpp`, which pulls in Asio.
class FrameReceiver;

// One open surface.
class Surface {
  public:
    virtual ~Surface() = default;

    [[nodiscard]] virtual SurfaceInfo info() const = 0;
    // Pixel mode: frames; a lagging receiver waits for the next keyframe.
    [[nodiscard]] virtual Result<std::shared_ptr<FrameReceiver>> frames() = 0;
    [[nodiscard]] virtual Awaitable<Result<void>> request_keyframe() = 0;
    [[nodiscard]] virtual Awaitable<Result<void>> input(InputEvent event) = 0;
    // Document mode: the file, sent once.
    [[nodiscard]] virtual Awaitable<Result<Document>> document() = 0;
    // The current picture as PNG.
    [[nodiscard]] virtual Awaitable<Result<Bytes>> screenshot() = 0;
    // Switches to the best of `accepted` (best first) and returns what it
    // streams now. Default: no negotiation (`std::nullopt`).
    [[nodiscard]] virtual Awaitable<Result<std::optional<StreamInfo>>> negotiate(std::vector<Encoding> accepted);
    [[nodiscard]] virtual std::optional<StreamInfo> stream() const { return std::nullopt; }
};

// A source of surfaces: the browser, a document viewer, a VM.
class SurfaceServer {
  public:
    virtual ~SurfaceServer() = default;

    // Server part of its uris, e.g. `browser`.
    [[nodiscard]] virtual std::string_view name() const = 0;
    [[nodiscard]] virtual Awaitable<Result<std::vector<SurfaceInfo>>> list() = 0;
    [[nodiscard]] virtual Awaitable<Result<std::shared_ptr<Surface>>> open(const SurfaceUri& uri) = 0;
};

} // namespace loa::contracts::surface
