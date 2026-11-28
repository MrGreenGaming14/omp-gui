#pragma once

// The Surfaces panel: browser tabs, documents, VMs and declared surfaces, drawn by
// the surface renderer. The protocol values and the live `Surface` transport are the
// contracts'; the launcher's status types and `Intent` are this screen's own, and
// `port` holds what the app implements for decoding and document rendering.

#include <array>
#include <cstdint>
#include <cstddef>
#include <expected>
#include <filesystem>
#include <functional>
#include <memory>
#include <optional>
#include <span>
#include <string>
#include <string_view>
#include <variant>
#include <vector>

#include "loa/contracts/surface.hpp"

namespace loa::view::surface {

using loa::contracts::surface::Document;
using loa::contracts::surface::Encoding;
using loa::contracts::surface::Frame;
using loa::contracts::surface::Input;
using loa::contracts::surface::InputEvent;
using loa::contracts::surface::Modifiers;
using loa::contracts::surface::MouseButton;
using loa::contracts::surface::Rect;
using loa::contracts::surface::StreamInfo;
using loa::contracts::surface::Surface;
using loa::contracts::surface::SurfaceError;
using loa::contracts::surface::SurfaceInfo;
using loa::contracts::surface::SurfaceMode;
using loa::contracts::surface::SurfaceUri;

// Server name of the built-in document surfaces.
inline constexpr std::string_view DOCUMENT_SERVER = "docs";

// Whether browser surfaces can be opened, as the launcher says it.
namespace browser_status {
// The browser executable, and whether Chrome's sandbox is on.
struct Ready {
    std::filesystem::path executable;
    bool sandboxed = false;
    friend bool operator==(const Ready&, const Ready&) = default;
};
struct Missing {
    friend bool operator==(const Missing&, const Missing&) = default;
};
} // namespace browser_status

using BrowserStatus = std::variant<browser_status::Ready, browser_status::Missing>;

// How browser frames are captured.
enum class Capture : std::uint8_t {
    // Headless Chrome's JPEG screencast.
    Screencast,
    // Headed Chrome on an Xvfb virtual display per tab, captured raw.
    VirtualDisplay,
};

// The browser capture choice, as the launcher shows it.
struct CaptureStatus {
    Capture selected = Capture::Screencast;
    // Why the virtual display can't run here; `std::nullopt` when it can.
    std::optional<std::string> virtual_display_missing = "not checked";
    // The running browser keeps the other capture until its tabs close.
    bool pending = false;

    friend bool operator==(const CaptureStatus&, const CaptureStatus&) = default;
};

// A surface that can be opened: a configured VM endpoint, or one an MCP server declares.
struct AvailableSurface {
    SurfaceInfo info;
    // The server declaring it, e.g. `vm` or the MCP server's name.
    std::string server;

    friend bool operator==(const AvailableSurface&, const AvailableSurface&) = default;
};

// Live video encoding for viewers across the network, as the launcher shows it. Tabs in this
// app always get the source frames.
struct VideoStatus {
    bool enabled = false;
    // What network viewers get; `std::nullopt` while the machine's encoders are tried.
    std::optional<StreamInfo> encoder;
    // Hardware encoders that work here, e.g. `h264_vaapi`.
    std::vector<std::string> hardware;

    friend bool operator==(const VideoStatus&, const VideoStatus&) = default;
};

// What the Surfaces panel asks of the app.
namespace intent {
struct OpenBrowser {
    std::string url;
    friend bool operator==(const OpenBrowser&, const OpenBrowser&) = default;
};
struct OpenDocument {
    std::filesystem::path path;
    friend bool operator==(const OpenDocument&, const OpenDocument&) = default;
};
struct OpenVm {
    std::string address;
    std::optional<std::string> password;
    friend bool operator==(const OpenVm&, const OpenVm&) = default;
};
// Open a listed surface.
struct Open {
    SurfaceUri uri;
    friend bool operator==(const Open&, const Open&) = default;
};
// The user closed the tab of an open surface.
struct Close {
    SurfaceUri uri;
    friend bool operator==(const Close&, const Close&) = default;
};
// An open surface streams `encoding`, which the renderer can't decode; the renderer has
// asked the surface to switch to one of `supported`.
struct EncodingUnsupported {
    SurfaceUri uri;
    Encoding encoding = Encoding::Rgba;
    std::vector<Encoding> supported;
    friend bool operator==(const EncodingUnsupported&, const EncodingUnsupported&) = default;
};
// The user turned live video encoding on or off (only with live video built in).
struct SetVideo {
    bool enabled = false;
    friend bool operator==(const SetVideo&, const SetVideo&) = default;
};
// The user picked how browser frames are captured.
struct SetCapture {
    Capture capture = Capture::Screencast;
    friend bool operator==(const SetCapture&, const SetCapture&) = default;
};
} // namespace intent

using Intent = std::variant<intent::OpenBrowser, intent::OpenDocument, intent::OpenVm, intent::Open, intent::Close,
                            intent::EncodingUnsupported, intent::SetVideo, intent::SetCapture>;

} // namespace loa::view::surface

// What the surface renderer needs from the app: frame decoders and the document renderer.
namespace loa::view::surface::port {

// Why a frame couldn't be decoded, as the tab shows it.
struct DecodeError {
    std::string message;

    friend bool operator==(const DecodeError&, const DecodeError&) = default;
};

// One decoding strategy of a pixel tab.
class FrameDecoder {
  public:
    virtual ~FrameDecoder() = default;
    // The encodings this decoder applies, best first.
    [[nodiscard]] virtual std::span<const Encoding> accepts() const = 0;
    // Applies `frame` onto `picture`, the `width` x `height` RGBA picture the tab
    // retains; `false` when it produced no picture yet.
    [[nodiscard]] virtual std::expected<bool, DecodeError> apply(const Frame& frame, std::span<std::uint8_t> picture,
                                                                 std::uint32_t width, std::uint32_t height) = 0;
    // Forgets reference pictures after lost frames; the next frame fed is a keyframe.
    virtual void reset() {}
};

// Makes one decoder for a new pixel tab.
using NewDecoder = std::function<std::unique_ptr<FrameDecoder>()>;

// The decoders each new pixel tab gets: `extra` first, then `still`.
struct Decoders {
    // e.g. H.264; without one, tabs ask video surfaces for stills.
    std::optional<NewDecoder> extra;
    // The still-image fallback (RGBA, PNG, JPEG); what it accepts is what a tab asks
    // for when a surface streams something it can't decode.
    NewDecoder still;
};

// Size of a page at zoom 1, in panel pixels.
struct PageSize {
    float width = 0;
    float height = 0;

    friend bool operator==(const PageSize&, const PageSize&) = default;
};

// A rendered page: 8 bit RGBA, row major, no padding.
struct RgbaPage {
    std::uint32_t width = 0;
    std::uint32_t height = 0;
    std::vector<std::uint8_t> data;

    friend bool operator==(const RgbaPage&, const RgbaPage&) = default;
};

// Straight RGBA colours of rendered pages.
struct DocumentPalette {
    std::array<std::uint8_t, 4> page{};
    std::array<std::uint8_t, 4> text{};
    // Table borders, header rules and picture placeholders.
    std::array<std::uint8_t, 4> rule{};
    // Sheet grid lines and slide table outlines.
    std::array<std::uint8_t, 4> grid{};
    std::array<std::uint8_t, 4> header{};
    std::array<std::uint8_t, 4> header_text{};

    friend bool operator==(const DocumentPalette&, const DocumentPalette&) = default;
};

// A parsed paged document (PDF, sheet, DOCX, slides).
class PagedDocument {
  public:
    virtual ~PagedDocument() = default;
    [[nodiscard]] virtual std::size_t page_count() const = 0;
    // Size of page `index` (from 0) at zoom 1.
    [[nodiscard]] virtual std::expected<PageSize, std::string> page_size(std::size_t index) const = 0;
    // Renders page `index` at `zoom` in `palette`.
    [[nodiscard]] virtual std::expected<RgbaPage, std::string> render(std::size_t index, float zoom,
                                                                      const DocumentPalette& palette) const = 0;
};

// A fetched document, ready to show.
namespace opened_document {
// An image the renderer shows from the document's bytes as they are.
struct Image {
    std::uint32_t width = 0;
    std::uint32_t height = 0;
};
struct Paged {
    std::unique_ptr<PagedDocument> document;
};
} // namespace opened_document

using OpenedDocument = std::variant<opened_document::Image, opened_document::Paged>;

// Parses fetched documents for local rendering.
class DocumentRenderer {
  public:
    virtual ~DocumentRenderer() = default;
    // File extensions the picker lists, compared case insensitively.
    [[nodiscard]] virtual std::vector<std::string> extensions() const = 0;
    // Opens `document`, or says why it can't be shown.
    [[nodiscard]] virtual std::expected<OpenedDocument, std::string> open(const Document& document) const = 0;
};

} // namespace loa::view::surface::port
