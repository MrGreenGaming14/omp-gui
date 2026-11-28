#pragma once

// One surface open in the panel: the tab shell (name, notice, header) around its mode,
// `PixelMode` for streamed frames or `DocumentMode` for files rendered here. Port of
// `crates/loa-ui-surface/src/tab/{mod,pixel,document}.rs`.
//
// Plain C++: `State::update` returns the surface work it needs as a `Task` (stream frames, send
// one input, request a keyframe, negotiate, fetch the document), which a `Runner` carries out on
// the `loa::runtime` executor and answers with `Message`s.

#include <cstddef>
#include <cstdint>
#include <expected>
#include <memory>
#include <optional>
#include <string>
#include <utility>
#include <variant>
#include <vector>

#include "loa/ui/surface/decode.hpp"
#include "loa/ui/surface/framebuffer.hpp"
#include "loa/ui/surface/geometry.hpp"
#include "loa/ui/surface/keys.hpp"
#include "loa/ui/surface/latency.hpp"
#include "loa/view/surface.hpp"

namespace loa::contracts::surface {
class FrameReceiver;
}

namespace loa::ui::surface {

using loa::view::surface::Document;
using loa::view::surface::InputEvent;
using loa::view::surface::Intent;
using loa::view::surface::StreamInfo;
using loa::view::surface::SurfaceInfo;
using loa::view::surface::SurfaceUri;
using FrameReceiver = loa::contracts::surface::FrameReceiver;
using SurfacePtr = std::shared_ptr<loa::view::surface::Surface>;
using DocumentRenderer = loa::view::surface::port::DocumentRenderer;
using PagedDocument = loa::view::surface::port::PagedDocument;
using DocumentPalette = loa::view::surface::port::DocumentPalette;
using RgbaPage = loa::view::surface::port::RgbaPage;

inline constexpr float MIN_ZOOM = 0.25F;
inline constexpr float MAX_ZOOM = 4.0F;
inline constexpr float ZOOM_STEP = 1.25F;

// The viewer clock that stamps inputs and frame arrivals.
class Clock {
  public:
    virtual ~Clock() = default;
    // Microseconds since an arbitrary fixed point.
    [[nodiscard]] virtual std::uint64_t now_us() const = 0;
};

// `Clock` counting from its creation on the monotonic system clock.
class MonotonicClock final : public Clock {
  public:
    MonotonicClock();
    [[nodiscard]] std::uint64_t now_us() const override;

  private:
    std::int64_t start_ns_;
};

// What the tabs of new surfaces get.
struct Options {
    std::shared_ptr<const Clock> clock;
    // The decoders of each pixel tab.
    Decoders decoders;
    // Opens and renders the files of document tabs; its extensions are what the picker lists.
    std::shared_ptr<const DocumentRenderer> documents;

    // Options on the monotonic system clock.
    [[nodiscard]] static Options make(Decoders decoders, std::shared_ptr<const DocumentRenderer> documents);
};

// Pointer input in panel coordinates (pixels from the canvas's top left).
namespace pointer {
struct Moved {
    Point at;
    friend bool operator==(const Moved&, const Moved&) = default;
};
struct Left {
    friend bool operator==(const Left&, const Left&) = default;
};
struct Button {
    Point at;
    MouseButton button = MouseButton::Left;
    bool pressed = false;
    friend bool operator==(const Button&, const Button&) = default;
};
// Panel pixels, positive `dy` scrolls the content down.
struct Scroll {
    Point at;
    float dx = 0, dy = 0;
    friend bool operator==(const Scroll&, const Scroll&) = default;
};
} // namespace pointer
using PointerInput = std::variant<pointer::Moved, pointer::Left, pointer::Button, pointer::Scroll>;

namespace msg {
struct Frame {
    std::shared_ptr<const loa::view::surface::Frame> frame;
};
// The frame stream skipped this many frames.
struct Lagged {
    std::uint64_t frames = 0;
};
struct StreamEnded {};
// The canvas has a new size.
struct Panel {
    Size size;
};
// Pointer input in panel coordinates, with the panel size it was seen at.
struct Pointer {
    Size panel;
    PointerInput input;
};
struct Modifiers {
    loa::view::surface::Modifiers modifiers;
};
struct Key {
    KeyInput key;
};
// A click landed outside the surface.
struct Blur {};
struct Document {
    std::expected<std::shared_ptr<const loa::view::surface::Document>, std::string> document;
};
// The document view scrolled: `top` of the viewport in content pixels.
struct DocumentScrolled {
    float top = 0;
    Size viewport;
};
struct ZoomIn {};
struct ZoomOut {};
struct ZoomReset {};
// A call to the surface failed.
struct Failed {
    std::string message;
};
// A call to the surface went through.
struct Done {};
} // namespace msg

using Message = std::variant<msg::Frame, msg::Lagged, msg::StreamEnded, msg::Panel, msg::Pointer, msg::Modifiers, msg::Key,
                             msg::Blur, msg::Document, msg::DocumentScrolled, msg::ZoomIn, msg::ZoomOut, msg::ZoomReset,
                             msg::Failed, msg::Done>;

// Surface work an update asks for; a `Runner` does it and answers with messages.
namespace work {
// Deliver `receiver`'s frames as `Frame`, `Lagged` and finally `StreamEnded`.
struct StreamFrames {
    std::shared_ptr<FrameReceiver> receiver;
    friend bool operator==(const StreamFrames&, const StreamFrames&) = default;
};
// Send one input; inputs go out one at a time, in order. A failure answers `Failed`.
struct Send {
    InputEvent event;
    friend bool operator==(const Send&, const Send&) = default;
};
// `Surface::request_keyframe`, answered with `Done` or `Failed`.
struct RequestKeyframe {
    friend bool operator==(const RequestKeyframe&, const RequestKeyframe&) = default;
};
// `Surface::negotiate`, answered with `Done` or `Failed`.
struct Negotiate {
    std::vector<Encoding> accepted;
    friend bool operator==(const Negotiate&, const Negotiate&) = default;
};
// `Surface::document`, answered with `Document`.
struct FetchDocument {
    friend bool operator==(const FetchDocument&, const FetchDocument&) = default;
};
} // namespace work
using Work = std::variant<work::StreamFrames, work::Send, work::RequestKeyframe, work::Negotiate, work::FetchDocument>;

// The work of one update, in order (iced `Task<Message>` of a tab).
struct Task {
    std::vector<Work> work;

    [[nodiscard]] static Task none() { return {}; }
    [[nodiscard]] static Task of(Work w) { return Task{{std::move(w)}}; }
    void append(Task other) {
        for (auto& w : other.work) work.push_back(std::move(w));
    }
    [[nodiscard]] bool empty() const { return work.empty(); }
    friend bool operator==(const Task&, const Task&) = default;
};

// What the tab header shows.
struct Tab {
    std::string title;
    std::optional<std::string> notice;
    std::optional<Reading> latency;
    friend bool operator==(const Tab&, const Tab&) = default;
};

// A problem the tab header shows.
struct Notice {
    enum class Kind : std::uint8_t { Unsupported, Error, Closed };
    Kind kind = Kind::Closed;
    Encoding encoding = Encoding::Rgba; // `Unsupported`
    std::string message;                // `Error`

    [[nodiscard]] std::string text() const;
    friend bool operator==(const Notice&, const Notice&) = default;
};

// What a mode sees of its tab.
struct TabCtx {
    SurfaceInfo info;
    SurfacePtr surface;
    std::optional<Notice> notice;
};

// The locally drawn pointer.
struct LocalCursor {
    // Panel coordinates.
    Point at;
    // The surface point under it, `nullopt` over the bars around the picture.
    std::optional<Point> on_surface;
    // A button is held.
    bool pressed = false;
    friend bool operator==(const LocalCursor&, const LocalCursor&) = default;
};

// Pixel mode: frames go through the decoder chain onto a retained framebuffer, and pointer,
// wheel and keys go back in surface coordinates, stamped for the latency meter.
class PixelMode {
  public:
    static std::pair<PixelMode, Task> open(TabCtx& tab, const Options& options);

    // The current picture.
    [[nodiscard]] const Framebuffer* framebuffer() const { return fb_ ? &*fb_ : nullptr; }
    // Sequence number of the newest frame taken.
    [[nodiscard]] std::optional<std::uint64_t> shown_seq() const { return last_seq_; }
    // The pointer as drawn locally, independent of frames.
    [[nodiscard]] std::optional<LocalCursor> cursor() const { return cursor_; }
    // Whether keys go to the surface.
    [[nodiscard]] bool focused() const { return focused_; }
    [[nodiscard]] std::optional<Reading> latency() const { return meter_.reading(); }
    // Where the surface sits in the panel, as last measured.
    [[nodiscard]] std::optional<Placement> placement() const { return placement_in(panel_); }
    // Where the surface sits in a panel of size `panel`.
    [[nodiscard]] std::optional<Placement> placement_in(Size panel) const;
    // The panel size last measured.
    [[nodiscard]] Size panel() const { return panel_; }
    // The encoding of the last frame shown, as a stream.
    [[nodiscard]] std::optional<StreamInfo> last_stream() const;

    std::pair<Task, std::optional<Intent>> update(Message message, TabCtx& tab);

  private:
    PixelMode(std::shared_ptr<const Clock> clock, Chain decoders, std::uint32_t width, std::uint32_t height);
    [[nodiscard]] Task send(loa::view::surface::Input input, bool measured);
    [[nodiscard]] Task on_panel(Size size);
    [[nodiscard]] Task on_pointer(PointerInput input);
    std::pair<Task, std::optional<Intent>> on_frame(const loa::view::surface::Frame& frame, TabCtx& tab);
    [[nodiscard]] Task on_lag();

    std::shared_ptr<const Clock> clock_;
    std::uint64_t next_seq_ = 1;
    bool focused_ = false;
    loa::view::surface::Modifiers modifiers_;
    std::vector<std::string> held_keys_;
    std::uint32_t buttons_held_ = 0;
    std::optional<LocalCursor> cursor_;
    Size panel_;
    LatencyMeter meter_;
    Chain decoders_;
    // The surface size the info gives, until the first frame sets the framebuffer's.
    std::pair<std::uint32_t, std::uint32_t> size_;
    std::optional<Framebuffer> fb_;
    std::optional<std::uint64_t> last_seq_;
    std::optional<Encoding> last_encoding_;
    bool awaiting_keyframe_ = false;
    std::optional<Encoding> asked_for_still_;
};

// Pages whose vertical extent meets `[top, bottom)`, stacked with `gap` between them, as a
// `[first, end)` index range.
[[nodiscard]] std::pair<std::size_t, std::size_t> visible_pages(const std::vector<float>& heights, float gap, float top,
                                                                float bottom);

// Space between document pages, in panel pixels.
[[nodiscard]] float page_gap();

// A paged document and its rendered pages, each with the zoom it was rendered at.
class Pages {
  public:
    [[nodiscard]] static std::expected<Pages, std::string> make(std::unique_ptr<PagedDocument> paged);

    // Page sizes at zoom 1.
    [[nodiscard]] const std::vector<Size>& sizes() const noexcept { return sizes_; }
    // The rendered page `index` and its zoom, if any. It may be from an earlier zoom; the view
    // scales it until the new render lands.
    [[nodiscard]] const std::pair<float, RgbaPage>* rendered(std::size_t index) const;
    // Renders the pages showing in `[top, bottom)` at `zoom` (plus a page ahead each way) and
    // drops pages far from view.
    std::expected<void, std::string> refresh(float zoom, float top, float bottom, float gap, const DocumentPalette& palette);

  private:
    Pages() = default;
    std::unique_ptr<PagedDocument> paged_;
    std::vector<Size> sizes_;
    std::vector<std::optional<std::pair<float, RgbaPage>>> rendered_;
};

// Document mode: the file arrives once and is rendered here with the app's document renderer,
// so scrolling and zooming never touch the surface.
class DocumentMode {
  public:
    // An image shown from the document's bytes as they are.
    struct Image {
        std::shared_ptr<const Document> document;
        Size size;
    };

    static std::pair<DocumentMode, Task> open(const TabCtx& tab, const Options& options);

    // Pages in the loaded document, 1 for an image, `nullopt` while loading or when it couldn't
    // be opened.
    [[nodiscard]] std::optional<std::size_t> page_count() const;
    // Paged documents' pages, null until loaded or for images.
    [[nodiscard]] const Pages* pages() const { return std::get_if<Pages>(&view_); }
    [[nodiscard]] const Image* image() const { return std::get_if<Image>(&view_); }
    [[nodiscard]] bool loading() const { return std::holds_alternative<Loading>(view_); }
    // Why the document can't be shown.
    [[nodiscard]] const std::string* failure() const;
    [[nodiscard]] float zoom() const { return zoom_; }

    std::pair<Task, std::optional<Intent>> update(Message message, TabCtx& tab);

  private:
    struct Loading {};
    struct Failed {
        std::string error;
    };
    using View = std::variant<Loading, Image, Pages, Failed>;

    DocumentMode(std::shared_ptr<const DocumentRenderer> documents, Size viewport);
    std::expected<View, std::string> open_document(std::shared_ptr<const Document> document) const;

    View view_;
    std::shared_ptr<const DocumentRenderer> documents_;
    float zoom_ = 1;
    float top_ = 0;
    Size viewport_;
};

// One surface open in the right panel.
class State {
  public:
    // Opens `surface`. The returned task streams frames (pixel mode) or fetches the file
    // (document mode).
    static std::pair<State, Task> open(SurfacePtr surface, const Options& options);

    std::pair<Task, std::optional<Intent>> update(Message message);

    // What the tab header shows: name, problems, latency.
    [[nodiscard]] Tab tab() const;
    [[nodiscard]] const SurfaceInfo& info() const { return tab_.info; }
    [[nodiscard]] const SurfacePtr& surface() const { return tab_.surface; }
    [[nodiscard]] std::optional<std::string> notice() const;
    [[nodiscard]] std::optional<Reading> latency() const;
    // What the surface streams: what it says, or else the encoding of the last frame shown.
    [[nodiscard]] std::optional<StreamInfo> stream() const;
    // The pixel mode, null for documents.
    [[nodiscard]] const PixelMode* pixel() const { return std::get_if<PixelMode>(&mode_); }
    // The document mode, null for pixel surfaces.
    [[nodiscard]] const DocumentMode* document() const { return std::get_if<DocumentMode>(&mode_); }

  private:
    State(TabCtx tab, std::variant<PixelMode, DocumentMode> mode) : tab_(std::move(tab)), mode_(std::move(mode)) {}
    TabCtx tab_;
    std::variant<PixelMode, DocumentMode> mode_;
};

// The latency meter text.
[[nodiscard]] std::string latency_label(std::optional<Reading> reading);
// What the header says a pixel surface streams: the codec and its encoder for video, the encoding
// alone for frames passed on as made.
[[nodiscard]] std::string stream_label(const StreamInfo& stream);

} // namespace loa::ui::surface
