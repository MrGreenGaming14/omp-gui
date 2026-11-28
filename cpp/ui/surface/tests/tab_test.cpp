// A surface tab over a fake `Surface`, driven through `State::update` and the work it returns.
// Port of `crates/loa-ui-surface/tests/tab.rs` (the iced_test render test is not ported; the
// viewer's input forwarding is covered in view_test.cpp).

#include <doctest/doctest.h>

#include <atomic>

#include "loa/ui/surface/tab.hpp"
#include "support.hpp"

using namespace loa::ui::surface;
namespace vs = loa::view::surface;
namespace in = loa::ui::surface::input;

namespace {

struct FakeClock final : Clock {
    std::atomic<std::uint64_t> now{0};
    [[nodiscard]] std::uint64_t now_us() const override { return now; }
};

vs::Rect full(std::uint32_t w, std::uint32_t h) { return {0, 0, w, h}; }

std::shared_ptr<const vs::Frame> frame(std::uint64_t seq, std::pair<std::uint32_t, std::uint32_t> size, vs::Rect region, vs::Encoding encoding,
                                       std::vector<std::uint8_t> data, std::uint64_t input_seq = 0) {
    const bool keyframe = region == full(size.first, size.second);
    return std::make_shared<const vs::Frame>(vs::Frame{seq, 0, input_seq, size.first, size.second, region, encoding, keyframe, std::move(data)});
}

PointerInput button(Point at, bool pressed) { return pointer::Button{at, vs::MouseButton::Left, pressed}; }

struct Harness {
    std::shared_ptr<support::FakeSurface> surface;
    std::shared_ptr<FakeClock> clock = std::make_shared<FakeClock>();
    std::optional<State> state;
    Task opened;
    // Every input sent, in order.
    std::vector<vs::InputEvent> sent;
    std::size_t keyframes = 0;

    Harness(vs::SurfaceMode mode, std::pair<std::uint32_t, std::uint32_t> size, std::optional<vs::Document> document, Options options) {
        std::optional<std::string> mime;
        if (document) mime = document->mime;
        vs::SurfaceInfo info{support::uri("fake", "one"), "Fake", size.first, size.second, mode, mime};
        surface = std::make_shared<support::FakeSurface>(std::move(info), std::move(document));
        options.clock = clock;
        auto [s, task] = State::open(surface, options);
        state.emplace(std::move(s));
        opened = std::move(task);
    }

    static Harness pixel(std::uint32_t w, std::uint32_t h) { return Harness(vs::SurfaceMode::Pixel, {w, h}, std::nullopt, support::options()); }

    // Updates and runs the returned work the way a runner would, synchronously.
    std::optional<Intent> update(Message message) {
        auto [task, event] = state->update(std::move(message));
        for (auto& w : task.work) {
            if (auto* s = std::get_if<work::Send>(&w)) sent.push_back(s->event);
            if (std::holds_alternative<work::RequestKeyframe>(w)) ++keyframes;
        }
        return event;
    }

    void pointer(Size panel, PointerInput input) { (void)update(msg::Pointer{panel, std::move(input)}); }

    std::vector<vs::Input> inputs() const {
        std::vector<vs::Input> out;
        for (const auto& e : sent) out.push_back(e.input);
        return out;
    }

    std::array<std::uint8_t, 4> pixel_at(std::uint32_t x, std::uint32_t y) const {
        const auto* fb = state->pixel()->framebuffer();
        REQUIRE(fb != nullptr);
        const std::size_t i = (static_cast<std::size_t>(y) * fb->width() + x) * 4;
        auto p = fb->pixels();
        return {p[i], p[i + 1], p[i + 2], p[i + 3]};
    }
};

using Rgba = std::array<std::uint8_t, 4>;

} // namespace

TEST_CASE("streamed frames are drawn and input reaches the surface in order") {
    auto h = Harness::pixel(4, 2);
    // Opening subscribes to frames, asks for a keyframe and negotiates what the chain decodes.
    REQUIRE(h.opened.work.size() == 3);
    CHECK(std::holds_alternative<work::StreamFrames>(h.opened.work[0]));
    CHECK(h.opened.work[1] == Work{work::RequestKeyframe{}});
    CHECK(h.opened.work[2] == Work{work::Negotiate{{vs::Encoding::Rgba, vs::Encoding::Png, vs::Encoding::Jpeg}}});

    (void)h.update(msg::Frame{frame(1, {4, 2}, full(4, 2), vs::Encoding::Rgba, support::solid(4, 2, {10, 20, 30, 255}))});
    CHECK(h.state->pixel()->shown_seq() == 1U);
    CHECK(h.pixel_at(3, 1) == Rgba{10, 20, 30, 255});

    // 4x2 surface in a 400x200 panel: scale 100, no bars.
    const Size panel{400, 200};
    h.clock->now = 1'000;
    h.pointer(panel, pointer::Moved{{150, 50}});
    h.clock->now = 2'000;
    h.pointer(panel, button({150, 50}, true));
    h.pointer(panel, button({250, 150}, false));
    const auto press = [](float x, float y, bool pressed) { return vs::Input{in::MouseButton{x, y, vs::MouseButton::Left, pressed, {}}}; };
    const std::vector<vs::InputEvent> want{
        {1, 1'000, in::Resize{400, 200}},
        {2, 1'000, in::MouseMove{1.5F, 0.5F}},
        {3, 2'000, press(1.5F, 0.5F, true)},
        {4, 2'000, press(2.5F, 1.5F, false)},
    };
    CHECK(h.sent == want);
}

TEST_CASE("still frames land on the retained picture in order") {
    auto h = Harness::pixel(8, 8);
    // The fake still decoder's PNG and JPEG carry RGBA as is; real codecs are the app's.
    const auto rect = [](std::uint32_t x, std::uint32_t y, std::uint32_t w, std::uint32_t hh) { return vs::Rect{x, y, w, hh}; };
    struct Step {
        std::uint64_t seq;
        vs::Rect region;
        vs::Encoding encoding;
        std::vector<std::uint8_t> data;
    };
    // Later regions overwrite, older frames are dropped.
    const std::vector<Step> frames{
        {5, full(8, 8), vs::Encoding::Rgba, support::solid(8, 8, {1, 1, 1, 255})},
        {6, rect(2, 1, 2, 2), vs::Encoding::Rgba, support::solid(2, 2, {9, 9, 9, 255})},
        {3, full(8, 8), vs::Encoding::Rgba, support::solid(8, 8, {3, 3, 3, 255})},
        {7, rect(0, 4, 4, 4), vs::Encoding::Png, support::solid(4, 4, {0, 200, 0, 255})},
        {8, rect(4, 4, 4, 4), vs::Encoding::Jpeg, support::solid(4, 4, {200, 0, 0, 255})},
    };
    for (const auto& f : frames) CHECK_FALSE(h.update(msg::Frame{frame(f.seq, {8, 8}, f.region, f.encoding, f.data)}).has_value());
    CHECK(h.state->pixel()->shown_seq() == 8U);
    CHECK(h.pixel_at(2, 1) == Rgba{9, 9, 9, 255});
    CHECK(h.pixel_at(3, 2) == Rgba{9, 9, 9, 255});
    CHECK(h.pixel_at(1, 1) == Rgba{1, 1, 1, 255});
    CHECK(h.pixel_at(0, 7) == Rgba{0, 200, 0, 255});
    const auto [r, g, b, a] = h.pixel_at(5, 5);
    CHECK((r > 180 && g < 30 && b < 30 && a == 255));
    REQUIRE(h.state->stream());
    CHECK(stream_label(*h.state->stream()) == "JPEG");
}

TEST_CASE("video without a decoder asks once for a still encoding") {
    auto h = Harness::pixel(2, 2);
    const auto h264 = [](std::uint64_t seq) { return msg::Frame{frame(seq, {2, 2}, full(2, 2), vs::Encoding::H264, {0, 0, 0, 1})}; };
    const auto uri = h.state->info().uri;
    const std::vector<vs::Encoding> supported{vs::Encoding::Rgba, vs::Encoding::Png, vs::Encoding::Jpeg};
    auto [task, event] = h.state->update(h264(1));
    CHECK(event == std::optional<Intent>{vs::intent::EncodingUnsupported{uri, vs::Encoding::H264, supported}});
    CHECK(task.work == std::vector<Work>{work::Negotiate{supported}});
    REQUIRE(h.state->tab().notice);
    CHECK(h.state->tab().notice->find("H.264") != std::string::npos);
    CHECK_FALSE(h.update(h264(2)).has_value()); // asked once, not on every frame
    // Once the server switches, the picture shows and the notice goes.
    (void)h.update(msg::Frame{frame(3, {2, 2}, full(2, 2), vs::Encoding::Rgba, support::solid(2, 2, {7, 7, 7, 255}))});
    CHECK(h.pixel_at(1, 1) == Rgba{7, 7, 7, 255});
    CHECK_FALSE(h.state->tab().notice.has_value());
}

namespace {

// Fills the picture with the first byte of each access unit, fails on 0xFF.
struct FakeDecoder final : vs::port::FrameDecoder {
    std::shared_ptr<std::atomic<std::size_t>> resets;
    explicit FakeDecoder(std::shared_ptr<std::atomic<std::size_t>> r) : resets(std::move(r)) {}
    [[nodiscard]] std::span<const vs::Encoding> accepts() const override {
        static constexpr std::array E{vs::Encoding::H264};
        return E;
    }
    [[nodiscard]] std::expected<bool, vs::port::DecodeError> apply(const vs::Frame& frame, std::span<std::uint8_t> picture, std::uint32_t,
                                                                   std::uint32_t) override {
        if (frame.data[0] == 0xFF) return std::unexpected(vs::port::DecodeError{"corrupt"});
        std::ranges::fill(picture, frame.data[0]);
        return true;
    }
    void reset() override { ++*resets; }
};

} // namespace

TEST_CASE("the decoder chain recovers video on a keyframe and lag asks for one") {
    auto resets = std::make_shared<std::atomic<std::size_t>>(0);
    auto options = support::options();
    options.decoders.extra = [resets] { return std::make_unique<FakeDecoder>(resets); };
    Harness h(vs::SurfaceMode::Pixel, {2, 2}, std::nullopt, options);
    // The chain negotiates H.264 first, then the stills.
    CHECK(h.opened.work[2] == Work{work::Negotiate{{vs::Encoding::H264, vs::Encoding::Rgba, vs::Encoding::Png, vs::Encoding::Jpeg}}});
    const auto video = [](std::uint64_t seq, std::uint8_t byte, bool keyframe) {
        auto f = *frame(seq, {2, 2}, full(2, 2), vs::Encoding::H264, {byte});
        f.keyframe = keyframe;
        return msg::Frame{std::make_shared<const vs::Frame>(std::move(f))};
    };
    // A broken unit resets and asks for a keyframe; deltas are skipped until it comes.
    struct Step {
        std::uint64_t seq;
        std::uint8_t byte;
        bool keyframe;
        std::uint8_t shown;
    };
    for (const auto& s : {Step{1, 40, true, 40}, Step{2, 0xFF, false, 40}, Step{3, 50, false, 40}, Step{4, 60, true, 60}, Step{5, 70, false, 70}}) {
        CAPTURE(s.seq);
        (void)h.update(video(s.seq, s.byte, s.keyframe));
        CHECK(h.pixel_at(0, 0) == Rgba{s.shown, s.shown, s.shown, s.shown});
    }
    CHECK(resets->load() == 1);
    CHECK(h.keyframes == 1);
    (void)h.update(msg::Lagged{3});
    CHECK(resets->load() == 2);
    CHECK(h.keyframes == 2);
}

TEST_CASE("pointer and wheel map through letterboxing and scale") {
    auto h = Harness::pixel(800, 600);
    const vs::Modifiers shift{true, false, false, false};
    (void)h.update(msg::Modifiers{shift});
    // 800x600 in 1000x300: scale 0.5, picture at x 300..700. Over the bars nothing is sent.
    const Size panel{1000, 300};
    h.pointer(panel, pointer::Moved{{100, 150}});
    h.pointer(panel, pointer::Moved{{500, 150}});
    h.pointer(panel, pointer::Scroll{{500, 150}, 0, 40});
    h.pointer(panel, pointer::Scroll{{900, 150}, 0, 40});
    h.pointer(panel, pointer::Moved{{300, 0}});
    const std::vector<vs::Input> want{in::Resize{1000, 300}, in::MouseMove{400, 300}, in::Scroll{400, 300, 0, 80, shift}, in::MouseMove{0, 0}};
    CHECK(h.inputs() == want);
}

TEST_CASE("the cursor follows the pointer and keys go out only with focus") {
    auto h = Harness::pixel(100, 100);
    const Size panel{100, 100};
    const auto key = [](std::string text, bool pressed) {
        std::optional<std::string> typed;
        if (pressed) typed = text;
        return msg::Key{KeyInput{text, "KeyX", pressed, {}, typed}};
    };
    (void)h.update(key("a", true));
    h.pointer(panel, pointer::Moved{{10, 20}});
    const auto cursor = [&] { return h.state->pixel()->cursor(); };
    CHECK(cursor() == LocalCursor{{10, 20}, Point{10, 20}, false});
    h.pointer(panel, button({5, 5}, true));
    h.pointer(panel, pointer::Moved{{31, 41}});
    REQUIRE(cursor());
    CHECK(cursor()->pressed); // dragging
    h.pointer(panel, button({31, 41}, false));
    (void)h.update(key("b", true));
    (void)h.update(key("b", false));
    (void)h.update(msg::Blur{});
    (void)h.update(key("c", true));
    h.pointer(panel, pointer::Left{});
    CHECK_FALSE(cursor().has_value());
    CHECK_FALSE(h.state->pixel()->shown_seq().has_value()); // no frame was needed
    std::vector<vs::Input> typed;
    for (const auto& i : h.inputs())
        if (std::holds_alternative<in::Text>(i) || std::holds_alternative<in::Key>(i)) typed.push_back(i);
    CHECK(typed == std::vector<vs::Input>{in::Text{"b"}});
}

TEST_CASE("the latency meter pairs clicks with the frame that shows them") {
    auto h = Harness::pixel(10, 10);
    // Seq 1 is the resize, then a press and a release per round; only the press is measured.
    const auto round = [&](std::uint64_t press_seq, std::uint64_t sent, std::uint64_t arrived) {
        const Size panel{10, 10};
        const Point at{1, 1};
        h.clock->now = sent;
        h.pointer(panel, button(at, true));
        h.pointer(panel, button(at, false));
        h.clock->now = arrived;
        (void)h.update(msg::Frame{frame(press_seq, {10, 10}, full(10, 10), vs::Encoding::Rgba, support::solid(10, 10, {0, 0, 0, 0}), press_seq)});
        REQUIRE(h.state->tab().latency);
        return *h.state->tab().latency;
    };
    (void)round(2, 1'000, 13'000);
    const auto r = round(4, 20'000, 30'000);
    CHECK(r == Reading{10'000, 12'000, 2, false});
    CHECK(latency_label(r) == "10 ms, p95 12 ms");
    CHECK(round(6, 40'000, 120'000).warning);
    CHECK(latency_label(std::nullopt) == "-- ms");
}

TEST_CASE("documents render here and zoom without touching the surface") {
    const vs::Document paged{"a.pages", std::string(support::PAGED_MIME), {}};
    const vs::Document png{"a.png", "image/png", {}};
    const vs::Document unknown{"a.bin", "application/x-unknown", {}};
    struct Case {
        vs::Document document;
        std::optional<std::size_t> pages;
        std::optional<std::string> notice;
    };
    for (const auto& c : {Case{paged, 1, std::nullopt}, Case{png, 1, std::nullopt}, Case{unknown, std::nullopt, "no viewer for application/x-unknown"}}) {
        CAPTURE(c.document.name);
        Harness h(vs::SurfaceMode::Document, {300, 200}, c.document, support::options());
        CHECK(h.opened.work == std::vector<Work>{work::FetchDocument{}});
        (void)h.update(msg::Document{std::make_shared<const vs::Document>(c.document)});
        (void)h.update(msg::ZoomIn{});
        const auto* doc = h.state->document();
        REQUIRE(doc != nullptr);
        CHECK(doc->page_count() == c.pages);
        CHECK(h.state->notice() == c.notice);
        CHECK(doc->zoom() == 1.25F);
        if (const auto* pages = doc->pages()) {
            REQUIRE(pages->rendered(0) != nullptr);
            CHECK(pages->rendered(0)->first == 1.25F); // the visible page renders at the new zoom
        }
        CHECK(h.keyframes == 0);
        CHECK(h.state->pixel() == nullptr);
    }
}

TEST_CASE("zoom clamps to 0.25 and 4.0 and resets to 1") {
    Harness h(vs::SurfaceMode::Document, {300, 200}, vs::Document{"a.png", "image/png", {}}, support::options());
    for (int i = 0; i < 20; ++i) (void)h.update(msg::ZoomIn{});
    CHECK(h.state->document()->zoom() == MAX_ZOOM);
    CHECK(MAX_ZOOM == 4.0F);
    for (int i = 0; i < 30; ++i) (void)h.update(msg::ZoomOut{});
    CHECK(h.state->document()->zoom() == MIN_ZOOM);
    CHECK(MIN_ZOOM == 0.25F);
    (void)h.update(msg::ZoomReset{});
    CHECK(h.state->document()->zoom() == 1.0F);
}

TEST_CASE("a pixel tab whose surface can't stream says why, and a closed stream is noticed") {
    Harness doc(vs::SurfaceMode::Document, {1, 1}, std::nullopt, support::options());
    (void)doc.update(msg::Document{std::unexpected(std::string("gone"))});
    CHECK(doc.state->notice() == "gone");
    auto h = Harness::pixel(2, 2);
    (void)h.update(msg::StreamEnded{});
    CHECK(h.state->notice() == "The surface closed");
    (void)h.update(msg::Failed{"input failed"});
    CHECK(h.state->notice() == "input failed");
}
