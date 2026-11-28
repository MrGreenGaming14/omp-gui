// The public helpers: geometry, latency, keys, picker, framebuffer, decoding names, document
// pages and provider styles. Ports of the unit tests in `crates/loa-ui-surface/src/*.rs`.

#include <doctest/doctest.h>

#include <QtCore/qnamespace.h>

#include <fstream>
#include <mutex>

#include "loa/runtime/tempfile.hpp"
#include "loa/ui/surface/decode.hpp"
#include "loa/ui/surface/geometry.hpp"
#include "loa/ui/surface/keys.hpp"
#include "loa/ui/surface/latency.hpp"
#include "loa/ui/surface/picker.hpp"
#include "loa/ui/surface/tab.hpp"
#include "loa/ui/surface/theme.hpp"

using namespace loa::ui::surface;
namespace vs = loa::view::surface;
namespace in = loa::ui::surface::input;

TEST_CASE("panel points map through bars and scale") {
    struct Case {
        std::pair<std::uint32_t, std::uint32_t> surface;
        Size panel;
        float scale;
        Point origin;
        Point at;
        std::optional<Point> want;
    };
    const std::vector<Case> cases{
        // Wide panel: bars left and right, half size.
        {{800, 600}, {1000, 300}, 0.5F, {300, 0}, {300, 0}, Point{0, 0}},
        {{800, 600}, {1000, 300}, 0.5F, {300, 0}, {500, 150}, Point{400, 300}},
        {{800, 600}, {1000, 300}, 0.5F, {300, 0}, {299, 150}, std::nullopt},
        {{800, 600}, {1000, 300}, 0.5F, {300, 0}, {700, 150}, std::nullopt},
        // Tall panel: bars top and bottom, upscaled.
        {{100, 50}, {400, 600}, 4.0F, {0, 200}, {40, 240}, Point{10, 10}},
        {{100, 50}, {400, 600}, 4.0F, {0, 200}, {40, 100}, std::nullopt},
        {{100, 50}, {400, 600}, 4.0F, {0, 200}, {40, 400}, std::nullopt},
    };
    for (const auto& c : cases) {
        const auto placed = fit(c.surface.first, c.surface.second, c.panel);
        REQUIRE(placed);
        CHECK(placed->scale == c.scale);
        CHECK(placed->rect.position() == c.origin);
        CHECK(placed->to_surface(c.at) == c.want);
        if (c.want) CHECK(placed->to_panel(*c.want) == c.at);
    }
}

TEST_CASE("drags pin to the edge and empty sides have no placement") {
    const auto p = fit(800, 600, {1000, 300});
    REQUIRE(p);
    CHECK(p->to_surface_clamped({0, 150}) == Point{0, 300});
    const auto right = p->to_surface_clamped({1000, -50});
    CHECK((right.x < 800 && right.x > 799.9F && right.y == 0));
    CHECK_FALSE(fit(0, 10, {10, 10}));
    CHECK_FALSE(fit(10, 10, {0, 10}));
}

namespace {

// Feeds `(input seq, sent ms, frame input_seq, shown ms)` rounds, returning the reading.
std::optional<std::tuple<std::size_t, std::uint64_t, std::uint64_t, bool>>
run(LatencyMeter& meter, const std::vector<std::array<std::uint64_t, 4>>& rounds) {
    for (const auto& [seq, sent, covers, shown] : rounds) {
        if (seq > 0) meter.input_sent(seq, sent * 1000);
        meter.frame_shown(covers, shown * 1000);
    }
    const auto r = meter.reading();
    if (!r) return std::nullopt;
    return std::tuple{r->samples, r->median_us / 1000, r->p95_us / 1000, r->warning};
}

} // namespace

TEST_CASE("latency samples pair inputs with covering frames over the window") {
    CHECK(BUDGET_US == 50'000);
    CHECK(WINDOW == 100);
    using R = std::optional<std::tuple<std::size_t, std::uint64_t, std::uint64_t, bool>>;
    // Frames without new input count nothing.
    LatencyMeter idle;
    CHECK(run(idle, {{0, 0, 0, 5}}) == R{});
    // 1 and 2 are covered by the first frame, 3 waits; a repeated cover adds nothing.
    LatencyMeter m;
    m.input_sent(1, 1'000);
    CHECK(run(m, {{2, 3, 2, 11}, {0, 0, 2, 90}, {3, 4, 3, 24}}) == R{{3, 10, 20, false}});
    // 21 samples: p95 is the 20th; one past the budget warns.
    std::vector<std::array<std::uint64_t, 4>> quick;
    for (std::uint64_t s = 1; s <= 20; ++s) quick.push_back({s, 0, s, 20 + 60 * (s == 20 ? 1U : 0U)});
    LatencyMeter q;
    CHECK(run(q, quick) == R{{20, 20, 20, false}});
    CHECK(run(q, {{21, 0, 21, 51}}) == R{{21, 20, 51, true}});
    // Old samples leave the window.
    std::vector<std::array<std::uint64_t, 4>> window;
    const std::array<std::uint64_t, 6> ms{90, 90, 90, 5, 5, 5};
    for (std::uint64_t s = 1; s <= 6; ++s) window.push_back({s, 0, s, ms[s - 1]});
    LatencyMeter w(3);
    CHECK(run(w, window) == R{{3, 5, 5, false}});
    // The default window keeps the last 100 samples: 100 quick ones push out a slow one.
    LatencyMeter full;
    full.input_sent(1, 0);
    full.frame_shown(1, 90'000);
    CHECK(full.reading()->warning);
    for (std::uint64_t s = 2; s <= 101; ++s) {
        full.input_sent(s, 0);
        full.frame_shown(s, 10'000);
    }
    CHECK(full.reading() == Reading{10'000, 10'000, 100, false});
}

namespace {

KeyInput key_event(std::string key, std::string code, std::optional<std::string> text, vs::Modifiers m, bool pressed) {
    return KeyInput{std::move(key), std::move(code), pressed, m, pressed ? std::move(text) : std::nullopt};
}

} // namespace

TEST_CASE("keys map to text or W3C keys") {
    const vs::Modifiers none{};
    const vs::Modifiers shift{true, false, false, false};
    const vs::Modifiers ctrl{false, true, false, false};
    const auto w3c = [](std::string key, std::string code, bool pressed, bool c) {
        return std::optional<vs::Input>{in::Key{std::move(key), std::move(code), pressed, vs::Modifiers{false, c, false, false}}};
    };
    const auto text = [](std::string t) { return std::optional<vs::Input>{in::Text{std::move(t)}}; };
    std::vector<std::string> held;
    struct Case {
        KeyInput key;
        std::optional<vs::Input> sent;
    };
    // Applied in order to one held set.
    const std::vector<Case> cases{
        {key_event("A", "KeyA", "A", shift, true), text("A")},
        {key_event("A", "KeyA", std::nullopt, none, false), std::nullopt},
        {key_event(" ", "Space", " ", none, true), text(" ")},
        {key_event("Enter", "Enter", "\r", none, true), w3c("Enter", "Enter", true, false)},
        {key_event("c", "KeyC", "c", ctrl, true), w3c("c", "KeyC", true, true)},
        // Ctrl is up by the time C is released: the release still goes out.
        {key_event("c", "KeyC", std::nullopt, none, false), w3c("c", "KeyC", false, false)},
        // A release whose press never went out (focus came later) is dropped.
        {key_event("ArrowLeft", "ArrowLeft", std::nullopt, none, false), std::nullopt},
    };
    for (const auto& c : cases) {
        CAPTURE(c.key.key);
        CHECK(c.key.to_input(held) == c.sent);
    }
}

TEST_CASE("Qt keys map to W3C key and code names") {
    // X11 key codes are evdev + 8.
    const auto x11 = [](std::uint32_t evdev) { return evdev + 8; };
    const auto meta = KeyMap::from_qt(Qt::Key_Meta, x11(125), Qt::MetaModifier, "", true);
    CHECK(meta.key == "Meta");
    CHECK(meta.code == "MetaLeft");
    CHECK(meta.modifiers == vs::Modifiers{false, false, false, true});
    CHECK(KeyMap::key(Qt::Key_Super_L, "") == "Meta");
    CHECK(KeyMap::key(Qt::Key_Space, " ") == " ");
    CHECK(KeyMap::key(Qt::Key_Return, "\r") == "Enter");
    CHECK(KeyMap::key(Qt::Key_Left, "") == "ArrowLeft");
    CHECK(KeyMap::key(Qt::Key_F5, "") == "F5");
    CHECK(KeyMap::key(Qt::Key_A, "A") == "A");
    // Ctrl+C types a control character; the key is the character.
    CHECK(KeyMap::key(Qt::Key_C, "\x03") == "c");
    CHECK(KeyMap::code(x11(30)) == "KeyA");
    CHECK(KeyMap::code(x11(57)) == "Space");
    CHECK(KeyMap::code(x11(28)) == "Enter");
    CHECK(KeyMap::code(x11(105)) == "ArrowLeft");
    CHECK(KeyMap::code(x11(126)) == "MetaRight");
    CHECK(KeyMap::code(0) == "Unidentified");
    const auto press = KeyMap::from_qt(Qt::Key_A, x11(30), Qt::ShiftModifier, "A", true);
    CHECK(press == KeyInput{"A", "KeyA", true, {true, false, false, false}, "A"});
    const auto release = KeyMap::from_qt(Qt::Key_A, x11(30), 0, "A", false);
    CHECK_FALSE(release.text.has_value());
    CHECK(KeyMap::button(Qt::LeftButton) == vs::MouseButton::Left);
    CHECK(KeyMap::button(Qt::BackButton) == vs::MouseButton::Back);
    CHECK_FALSE(KeyMap::button(Qt::ExtraButton4).has_value());
}

TEST_CASE("wheel up scrolls content up") {
    CHECK(KeyMap::scroll_pixels(scroll_delta::Lines{0, 1}) == std::pair{-0.0F, -LINE_PX});
    CHECK(KeyMap::scroll_pixels(scroll_delta::Pixels{3, -12}) == std::pair{-3.0F, 12.0F});
}

TEST_CASE("the picker lists folders then documents and picks a file") {
    auto tmp = loa::runtime::TempDir::create();
    REQUIRE(tmp);
    const auto& dir = tmp->path();
    std::filesystem::create_directory(dir / "reports");
    std::filesystem::create_directory(dir / ".hidden");
    std::ofstream(dir / "b.PDF") << "%PDF-";
    std::ofstream(dir / "a.xlsx");
    std::ofstream(dir / "notes.txt");
    std::ofstream(dir / "reports" / "q1.docx");
    FilePicker picker(dir, {"pdf", "xlsx", "xlsm", "docx", "pptx"});
    std::vector<std::string> names;
    for (const auto& e : picker.entries()) names.push_back(e.name);
    CHECK(names == std::vector<std::string>{"reports", "a.xlsx", "b.PDF"});

    (void)picker.update(picker::msg::Enter{dir / "reports"});
    CHECK(picker.dir() == dir / "reports");
    CHECK(picker.update(picker::msg::Choose{dir / "reports" / "q1.docx"}) == dir / "reports" / "q1.docx");
    (void)picker.update(picker::msg::Up{});
    CHECK(picker.dir() == dir);

    (void)picker.update(picker::msg::PathChanged{(dir / "notes.txt").string()});
    CHECK_FALSE(picker.update(picker::msg::PathSubmitted{}).has_value());
    REQUIRE(picker.error());
    CHECK(picker.error()->find("isn't a PDF") != std::string::npos);
    (void)picker.update(picker::msg::PathChanged{(dir / "b.PDF").string()});
    CHECK(picker.update(picker::msg::PathSubmitted{}) == dir / "b.PDF");
    (void)picker.update(picker::msg::PathChanged{(dir / "missing").string()});
    (void)picker.update(picker::msg::PathSubmitted{});
    CHECK(picker.error()->find("doesn't exist") != std::string::npos);
}

TEST_CASE("home expands under both separators") {
    const std::optional<std::filesystem::path> home = "/h";
    CHECK(picker::expand_with("~/docs/a.pdf", home) == std::filesystem::path("/h") / "docs/a.pdf");
    CHECK(picker::expand_with("~\\docs\\a.pdf", home) == std::filesystem::path("/h") / "docs\\a.pdf");
    CHECK(picker::expand_with("/abs/~/x", home) == std::filesystem::path("/abs/~/x"));
    CHECK(picker::expand_with("~/x", std::nullopt) == std::filesystem::path("~/x"));
}

TEST_CASE("encodings are named and video ones need keyframes") {
    CHECK(encoding_name(vs::Encoding::Rgba) == "RGBA");
    CHECK(encoding_name(vs::Encoding::H264) == "H.264");
    CHECK(encoding_name(vs::Encoding::Hevc) == "HEVC");
    CHECK(is_video(vs::Encoding::H264));
    CHECK_FALSE(is_video(vs::Encoding::Jpeg));
    CHECK(stream_label({vs::Encoding::H264, "h264_vaapi", true}) == "H.264 \u00b7 h264_vaapi \u00b7 hardware");
    CHECK(stream_label({vs::Encoding::H264, "", false}) == "H.264");
    CHECK(stream_label({vs::Encoding::Jpeg, "screencast", false}) == "JPEG");
    auto fb = Framebuffer::make(2, 2);
    REQUIRE(fb);
    CHECK(fb->pixels().size() == 16);
    CHECK_FALSE(Framebuffer::make(0xFFFFFFFF, 0xFFFFFFFF).has_value()); // the byte count overflows
}

TEST_CASE("visible pages respect gaps") {
    const std::vector<float> heights(5, 100.0F);
    // Pages at 0..100, 108..208, 216..316, ...
    CHECK(visible_pages(heights, 8, 0, 50) == std::pair<std::size_t, std::size_t>{0, 1});
    CHECK(visible_pages(heights, 8, 100, 108) == std::pair<std::size_t, std::size_t>{0, 0});
    CHECK(visible_pages(heights, 8, 150, 220) == std::pair<std::size_t, std::size_t>{1, 3});
    CHECK(visible_pages(heights, 8, 0, 10'000) == std::pair<std::size_t, std::size_t>{0, 5});
}

namespace {

// Pages of 50x100 that record what is rendered.
struct FakePages final : vs::port::PagedDocument {
    std::size_t count;
    std::shared_ptr<std::vector<std::pair<std::size_t, float>>> renders;
    FakePages(std::size_t n, std::shared_ptr<std::vector<std::pair<std::size_t, float>>> r) : count(n), renders(std::move(r)) {}
    [[nodiscard]] std::size_t page_count() const override { return count; }
    [[nodiscard]] std::expected<vs::port::PageSize, std::string> page_size(std::size_t) const override { return vs::port::PageSize{50, 100}; }
    [[nodiscard]] std::expected<vs::port::RgbaPage, std::string> render(std::size_t index, float scale,
                                                                        const vs::port::DocumentPalette&) const override {
        renders->emplace_back(index, scale);
        const auto w = static_cast<std::uint32_t>(50 * scale);
        const auto h = static_cast<std::uint32_t>(100 * scale);
        return vs::port::RgbaPage{w, h, std::vector<std::uint8_t>(static_cast<std::size_t>(w) * h * 4)};
    }
};

} // namespace

TEST_CASE("only pages near view render and zoom rerenders them") {
    auto renders = std::make_shared<std::vector<std::pair<std::size_t, float>>>();
    auto pages = Pages::make(std::make_unique<FakePages>(20, renders));
    REQUIRE(pages);
    using V = std::vector<std::pair<std::size_t, float>>;
    const auto step = [&](float zoom, float top, float bottom) {
        REQUIRE(pages->refresh(zoom, top, bottom, 8, document_palette()));
        return std::exchange(*renders, {});
    };
    // Pages 0 and 1 show; page 2 is rendered ahead.
    CHECK(step(1, 0, 150) == V{{0, 1.0F}, {1, 1.0F}, {2, 1.0F}});
    // At page 10, earlier pages are not rendered again.
    CHECK(step(1, 1080, 1180) == V{{9, 1.0F}, {10, 1.0F}, {11, 1.0F}});
    CHECK(step(2, 2160, 2200) == V{{9, 2.0F}, {10, 2.0F}, {11, 2.0F}});
    CHECK(pages->rendered(0) == nullptr); // far pages are dropped
    REQUIRE(pages->rendered(10));
    CHECK(pages->rendered(10)->first == 2.0F);
}

TEST_CASE("provider styles go by name, then by mode") {
    using Icon = loa::view::theme::Icon;
    CHECK(SurfaceKindStyle::of(BROWSER, vs::SurfaceMode::Pixel) == SurfaceKindStyle{Icon::Globe, "Browser"});
    CHECK(SurfaceKindStyle::of(VM, vs::SurfaceMode::Pixel).title == "VM or desktop (VNC)");
    CHECK(SurfaceKindStyle::of(MCP, vs::SurfaceMode::Pixel).icon == Icon::Wrench);
    CHECK(SurfaceKindStyle::of("figma", vs::SurfaceMode::Document) == SurfaceKindStyle{Icon::FileText, "Document"});
    CHECK(SurfaceKindStyle::of("figma", vs::SurfaceMode::Pixel) == SurfaceKindStyle{Icon::Square, ""});
    // Pages are paper in either theme: light page and dark text.
    const auto p = document_palette();
    CHECK(int(p.page[0]) + p.page[1] + p.page[2] > int(p.text[0]) + p.text[1] + p.text[2]);
}
