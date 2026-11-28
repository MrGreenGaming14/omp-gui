#include "loa/ui/surface/tab.hpp"

#include <algorithm>
#include <chrono>
#include <cmath>
#include <format>

#include "loa/contracts/surface.hpp"
#include "loa/ui/kit/theme.hpp"
#include "loa/ui/surface/theme.hpp"

namespace loa::ui::surface {

namespace in = loa::ui::surface::input;
using loa::view::surface::Rect;

namespace {

// Pages past the visible ones that are rendered ahead.
constexpr std::size_t LOOKAHEAD = 1;
// Rendered pages further than this from the visible ones are dropped.
constexpr std::size_t KEEP = 3;

template <class... Ts> struct overloaded : Ts... {
    using Ts::operator()...;
};

Notice error_notice(std::string message) { return Notice{Notice::Kind::Error, Encoding::Rgba, std::move(message)}; }

std::uint64_t div_ceil(std::uint64_t v, std::uint64_t d) { return (v + d - 1) / d; }

} // namespace

// ---- Clock and options -----------------------------------------------------------------------

namespace {
std::int64_t steady_ns() {
    return std::chrono::duration_cast<std::chrono::nanoseconds>(std::chrono::steady_clock::now().time_since_epoch()).count();
}
} // namespace

MonotonicClock::MonotonicClock() : start_ns_(steady_ns()) {}

std::uint64_t MonotonicClock::now_us() const {
    const std::int64_t ns = steady_ns() - start_ns_;
    return ns <= 0 ? 0 : static_cast<std::uint64_t>(ns / 1000);
}

Options Options::make(Decoders decoders, std::shared_ptr<const DocumentRenderer> documents) {
    return Options{std::make_shared<MonotonicClock>(), std::move(decoders), std::move(documents)};
}

std::string Notice::text() const {
    switch (kind) {
    case Kind::Unsupported:
        return std::string(encoding_name(encoding)) + " video can't be shown here, asked for PNG, JPEG or RGBA";
    case Kind::Error: return message;
    case Kind::Closed: return "The surface closed";
    }
    return {};
}

// ---- Pixel mode ------------------------------------------------------------------------------

PixelMode::PixelMode(std::shared_ptr<const Clock> clock, Chain decoders, std::uint32_t width, std::uint32_t height)
    : clock_(std::move(clock)), decoders_(std::move(decoders)), size_(width, height) {}

std::pair<PixelMode, Task> PixelMode::open(TabCtx& tab, const Options& options) {
    PixelMode mode(options.clock, Chain(options.decoders), tab.info.width, tab.info.height);
    Task work;
    // A late viewer gets the current picture now instead of waiting for the next change.
    if (auto frames = tab.surface->frames()) {
        work.work.emplace_back(work::StreamFrames{std::move(*frames)});
        work.work.emplace_back(work::RequestKeyframe{});
        work.work.emplace_back(work::Negotiate{mode.decoders_.accepted()});
    } else {
        tab.notice = error_notice(frames.error().message());
    }
    return {std::move(mode), std::move(work)};
}

std::optional<Placement> PixelMode::placement_in(Size panel) const {
    const auto [width, height] = fb_ ? std::pair{fb_->width(), fb_->height()} : size_;
    return fit(width, height, panel);
}

std::optional<StreamInfo> PixelMode::last_stream() const {
    if (!last_encoding_) return std::nullopt;
    return StreamInfo{*last_encoding_, {}, false};
}

Task PixelMode::send(loa::view::surface::Input input, bool measured) {
    const std::uint64_t seq = next_seq_++;
    const std::uint64_t sent_us = clock_->now_us();
    if (measured) meter_.input_sent(seq, sent_us);
    return Task::of(work::Send{InputEvent{seq, sent_us, std::move(input)}});
}

Task PixelMode::on_panel(Size size) {
    if (size == panel_) return Task::none();
    panel_ = size;
    const auto width = static_cast<std::uint32_t>(std::max(0.0F, std::round(size.width)));
    const auto height = static_cast<std::uint32_t>(std::max(0.0F, std::round(size.height)));
    if (width > 0 && height > 0) return send(in::Resize{width, height}, false);
    return Task::none();
}

Task PixelMode::on_pointer(PointerInput input) {
    const auto placement = this->placement();
    return std::visit(
        overloaded{
            [&](const pointer::Moved& m) {
                const bool dragging = buttons_held_ > 0;
                std::optional<Point> on_surface;
                if (placement) on_surface = dragging ? std::optional(placement->to_surface_clamped(m.at)) : placement->to_surface(m.at);
                cursor_ = LocalCursor{m.at, on_surface, dragging};
                if (on_surface) return send(in::MouseMove{on_surface->x, on_surface->y}, false);
                return Task::none();
            },
            [&](const pointer::Left&) {
                if (buttons_held_ == 0) cursor_.reset();
                return Task::none();
            },
            [&](const pointer::Button& b) {
                focused_ = focused_ || b.pressed;
                // A press must land on the picture; a release pins to its edge.
                std::optional<Point> target;
                if (placement) target = b.pressed ? placement->to_surface(b.at) : std::optional(placement->to_surface_clamped(b.at));
                if (!target) return Task::none();
                if (b.pressed) {
                    ++buttons_held_;
                } else if (buttons_held_ == 0) {
                    return Task::none();
                } else {
                    --buttons_held_;
                }
                cursor_ = LocalCursor{b.at, target, buttons_held_ > 0};
                return send(in::MouseButton{target->x, target->y, b.button, b.pressed, modifiers_}, b.pressed);
            },
            [&](const pointer::Scroll& s) {
                if (!placement) return Task::none();
                const auto target = placement->to_surface(s.at);
                if (!target) return Task::none();
                const float dx = placement->to_surface_len(s.dx);
                const float dy = placement->to_surface_len(s.dy);
                return send(in::Scroll{target->x, target->y, dx, dy, modifiers_}, true);
            },
        },
        input);
}

std::pair<Task, std::optional<Intent>> PixelMode::on_frame(const loa::view::surface::Frame& frame, TabCtx& tab) {
    if (last_seq_ && frame.seq <= *last_seq_) return {};
    const bool video = is_video(frame.encoding);
    if (decoders_.find(frame.encoding) == nullptr) {
        tab.notice = Notice{Notice::Kind::Unsupported, frame.encoding, {}};
        const auto asked = std::exchange(asked_for_still_, frame.encoding);
        if (asked == frame.encoding) return {};
        // Ask the surface for stills itself; the app hears about it too.
        auto still = decoders_.still();
        Intent intent = loa::view::surface::intent::EncodingUnsupported{tab.info.uri, frame.encoding, still};
        return {Task::of(work::Negotiate{std::move(still)}), std::move(intent)};
    }
    if (video && awaiting_keyframe_ && !frame.keyframe) return {};
    Task task;
    if (!fb_ || fb_->width() != frame.width || fb_->height() != frame.height) {
        auto fb = Framebuffer::make(frame.width, frame.height);
        if (!fb) {
            tab.notice = error_notice(std::format("a {}x{} frame is too large", frame.width, frame.height));
            return {};
        }
        fb_ = std::move(fb);
        const bool full = frame.region == Rect{0, 0, frame.width, frame.height};
        if (!full && !frame.keyframe) task = Task::of(work::RequestKeyframe{});
    }
    FrameDecoder* decoder = decoders_.find(frame.encoding);
    auto shown = apply(*decoder, frame, *fb_);
    if (shown) {
        last_seq_ = frame.seq;
        last_encoding_ = frame.encoding;
        awaiting_keyframe_ = false;
        if (*shown) {
            meter_.frame_shown(frame.input_seq, clock_->now_us());
            if (tab.notice && tab.notice->kind != Notice::Kind::Closed) tab.notice.reset();
        }
    } else {
        tab.notice = error_notice(shown.error().message);
        if (video) {
            decoder->reset();
            awaiting_keyframe_ = true;
            task = Task::of(work::RequestKeyframe{});
        }
    }
    return {std::move(task), std::nullopt};
}

Task PixelMode::on_lag() {
    if (last_encoding_ && is_video(*last_encoding_)) {
        awaiting_keyframe_ = true;
        decoders_.reset();
    }
    return Task::of(work::RequestKeyframe{});
}

std::pair<Task, std::optional<Intent>> PixelMode::update(Message message, TabCtx& tab) {
    if (auto* f = std::get_if<msg::Frame>(&message)) return f->frame ? on_frame(*f->frame, tab) : std::pair<Task, std::optional<Intent>>{};
    if (std::holds_alternative<msg::Lagged>(message)) return {on_lag(), std::nullopt};
    Task task;
    if (auto* p = std::get_if<msg::Panel>(&message)) {
        task = on_panel(p->size);
    } else if (auto* p = std::get_if<msg::Pointer>(&message)) {
        task = on_panel(p->panel);
        task.append(on_pointer(std::move(p->input)));
    } else if (auto* m = std::get_if<msg::Modifiers>(&message)) {
        modifiers_ = m->modifiers;
    } else if (auto* k = std::get_if<msg::Key>(&message)) {
        if (focused_) {
            if (auto input = k->key.to_input(held_keys_)) task = send(std::move(*input), k->key.pressed);
        }
    } else if (std::holds_alternative<msg::Blur>(message)) {
        focused_ = false;
    }
    return {std::move(task), std::nullopt};
}

std::string latency_label(std::optional<Reading> reading) {
    if (!reading) return "-- ms";
    return std::format("{} ms, p95 {} ms", div_ceil(reading->median_us, 1000), div_ceil(reading->p95_us, 1000));
}

std::string stream_label(const StreamInfo& stream) {
    const std::string encoding(encoding_name(stream.encoding));
    if (!is_video(stream.encoding) || stream.encoder.empty()) return encoding;
    return std::format("{} \u00b7 {} \u00b7 {}", encoding, stream.encoder, stream.hardware ? "hardware" : "software");
}

// ---- Document mode ---------------------------------------------------------------------------

float page_gap() { return kit::theme::sp(kit::theme::SpaceRole::S); }

std::pair<std::size_t, std::size_t> visible_pages(const std::vector<float>& heights, float gap, float top, float bottom) {
    float y = 0;
    std::optional<std::size_t> first;
    std::size_t end = 0;
    for (std::size_t i = 0; i < heights.size(); ++i) {
        if (y >= bottom) break;
        if (y + heights[i] > top) {
            if (!first) first = i;
            end = i + 1;
        }
        y += heights[i] + gap;
    }
    if (!first) return {0, 0};
    return {*first, end};
}

std::expected<Pages, std::string> Pages::make(std::unique_ptr<PagedDocument> paged) {
    Pages pages;
    const std::size_t count = paged->page_count();
    pages.sizes_.reserve(count);
    for (std::size_t i = 0; i < count; ++i) {
        auto size = paged->page_size(i);
        if (!size) return std::unexpected(size.error());
        pages.sizes_.push_back({size->width, size->height});
    }
    pages.rendered_.resize(count);
    pages.paged_ = std::move(paged);
    return pages;
}

const std::pair<float, RgbaPage>* Pages::rendered(std::size_t index) const {
    if (index >= rendered_.size() || !rendered_[index]) return nullptr;
    return &*rendered_[index];
}

std::expected<void, std::string> Pages::refresh(float zoom, float top, float bottom, float gap, const DocumentPalette& palette) {
    std::vector<float> heights;
    heights.reserve(sizes_.size());
    for (const auto& s : sizes_) heights.push_back(s.height * zoom);
    const auto [first, end] = visible_pages(heights, gap, top, bottom);
    const std::size_t wanted_first = first > LOOKAHEAD ? first - LOOKAHEAD : 0;
    const std::size_t wanted_end = std::min(end + LOOKAHEAD, sizes_.size());
    const std::size_t kept_first = first > KEEP ? first - KEEP : 0;
    const std::size_t kept_end = end + KEEP;
    for (std::size_t i = 0; i < rendered_.size(); ++i)
        if (i < kept_first || i >= kept_end) rendered_[i].reset();
    for (std::size_t i = wanted_first; i < wanted_end; ++i) {
        if (rendered_[i] && rendered_[i]->first == zoom) continue;
        auto page = paged_->render(i, zoom, palette);
        if (!page) return std::unexpected(page.error());
        rendered_[i] = std::pair{zoom, std::move(*page)};
    }
    return {};
}

DocumentMode::DocumentMode(std::shared_ptr<const DocumentRenderer> documents, Size viewport)
    : view_(Loading{}), documents_(std::move(documents)), viewport_(viewport) {}

std::pair<DocumentMode, Task> DocumentMode::open(const TabCtx& tab, const Options& options) {
    const Size viewport{static_cast<float>(tab.info.width), static_cast<float>(tab.info.height)};
    return {DocumentMode(options.documents, viewport), Task::of(work::FetchDocument{})};
}

std::optional<std::size_t> DocumentMode::page_count() const {
    if (std::holds_alternative<Image>(view_)) return 1;
    if (const auto* p = std::get_if<Pages>(&view_)) return p->sizes().size();
    return std::nullopt;
}

const std::string* DocumentMode::failure() const {
    const auto* f = std::get_if<Failed>(&view_);
    return f ? &f->error : nullptr;
}

std::expected<DocumentMode::View, std::string> DocumentMode::open_document(std::shared_ptr<const Document> document) const {
    if (!documents_) return std::unexpected("no document renderer");
    auto opened = documents_->open(*document);
    if (!opened) return std::unexpected(opened.error());
    if (auto* image = std::get_if<loa::view::surface::port::opened_document::Image>(&*opened)) {
        return View{Image{std::move(document), Size{static_cast<float>(image->width), static_cast<float>(image->height)}}};
    }
    auto& paged = std::get<loa::view::surface::port::opened_document::Paged>(*opened);
    auto pages = Pages::make(std::move(paged.document));
    if (!pages) return std::unexpected(pages.error());
    return View{std::move(*pages)};
}

std::pair<Task, std::optional<Intent>> DocumentMode::update(Message message, TabCtx& tab) {
    if (auto* d = std::get_if<msg::Document>(&message)) {
        auto view = d->document.and_then([this](std::shared_ptr<const Document> doc) { return open_document(std::move(doc)); });
        if (view) {
            view_ = std::move(*view);
        } else {
            tab.notice = error_notice(view.error());
            view_ = Failed{view.error()};
        }
    } else if (auto* s = std::get_if<msg::DocumentScrolled>(&message)) {
        top_ = s->top;
        viewport_ = s->viewport;
    } else if (std::holds_alternative<msg::ZoomIn>(message)) {
        zoom_ = std::min(zoom_ * ZOOM_STEP, MAX_ZOOM);
    } else if (std::holds_alternative<msg::ZoomOut>(message)) {
        zoom_ = std::max(zoom_ / ZOOM_STEP, MIN_ZOOM);
    } else if (std::holds_alternative<msg::ZoomReset>(message)) {
        zoom_ = 1;
    } else {
        return {};
    }
    if (auto* pages = std::get_if<Pages>(&view_)) {
        auto refreshed = pages->refresh(zoom_, top_, top_ + viewport_.height, page_gap(), document_palette());
        if (!refreshed) tab.notice = error_notice(refreshed.error());
    }
    return {};
}

// ---- The tab ---------------------------------------------------------------------------------

std::pair<State, Task> State::open(SurfacePtr surface, const Options& options) {
    TabCtx tab{surface->info(), surface, std::nullopt};
    if (tab.info.mode == loa::view::surface::SurfaceMode::Pixel) {
        auto [mode, work] = PixelMode::open(tab, options);
        return {State(std::move(tab), std::move(mode)), std::move(work)};
    }
    auto [mode, work] = DocumentMode::open(tab, options);
    return {State(std::move(tab), std::move(mode)), std::move(work)};
}

std::pair<Task, std::optional<Intent>> State::update(Message message) {
    if (std::holds_alternative<msg::StreamEnded>(message)) {
        tab_.notice = Notice{Notice::Kind::Closed, Encoding::Rgba, {}};
    } else if (auto* f = std::get_if<msg::Failed>(&message)) {
        tab_.notice = error_notice(std::move(f->message));
    } else if (std::holds_alternative<msg::Done>(message)) {
    } else {
        return std::visit([&](auto& mode) { return mode.update(std::move(message), tab_); }, mode_);
    }
    return {};
}

Tab State::tab() const { return Tab{tab_.info.name, notice(), latency()}; }

std::optional<std::string> State::notice() const {
    if (!tab_.notice) return std::nullopt;
    return tab_.notice->text();
}

std::optional<Reading> State::latency() const {
    const auto* p = pixel();
    return p ? p->latency() : std::nullopt;
}

std::optional<StreamInfo> State::stream() const {
    if (auto s = tab_.surface->stream()) return s;
    const auto* p = pixel();
    return p ? p->last_stream() : std::nullopt;
}

} // namespace loa::ui::surface
