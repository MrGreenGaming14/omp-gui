// The Surfaces tab driven through `Panel::update`, with fake surfaces. Port of
// `crates/loa-ui-surface/tests/panel.rs` (the launcher view test is in view_test.cpp).

#include <doctest/doctest.h>

#include "loa/runtime/tempfile.hpp"
#include "loa/ui/surface/panel.hpp"
#include "support.hpp"

using namespace loa::ui::surface;
namespace vs = loa::view::surface;
namespace pm = loa::ui::surface::panel::msg;

namespace {

Panel make_panel(const std::filesystem::path& dir) {
    return Panel(support::options(), vs::browser_status::Ready{"/usr/bin/chromium", true}, dir);
}

std::shared_ptr<support::FakeSurface> fake(std::string_view server, std::string_view name, vs::SurfaceMode mode) {
    vs::SurfaceInfo info{support::uri(server, name), std::string(name), 8, 4, mode, std::nullopt};
    return std::make_shared<support::FakeSurface>(std::move(info), std::nullopt);
}

} // namespace

TEST_CASE("the launcher asks for browser, document, VM and listed surfaces") {
    auto dir = loa::runtime::TempDir::create();
    REQUIRE(dir);
    auto panel = make_panel(dir->path());
    const auto deck = dir->path() / "deck.pptx";
    const auto declared = fake("figma", "board", vs::SurfaceMode::Pixel);
    panel.set_available({vs::AvailableSurface{declared->info(), "figma-mcp"}});
    const Intent vm = vs::intent::OpenVm{"10.0.0.5:5901", "secret"};
    // (message, event): fields update silently, submits ask the app.
    const std::vector<std::pair<panel::Message, std::optional<Intent>>> steps{
        {pm::OpenUrl{}, std::nullopt},
        {pm::Url{"example.com"}, std::nullopt},
        {pm::OpenUrl{}, vs::intent::OpenBrowser{"example.com"}},
        {pm::Picker{picker::msg::Choose{deck}}, vs::intent::OpenDocument{deck}},
        {pm::Address{"10.0.0.5:5901"}, std::nullopt},
        {pm::Password{"secret"}, std::nullopt},
        {pm::Connect{}, vm},
        {pm::OpenAvailable{declared->info_.uri}, vs::intent::Open{declared->info_.uri}},
    };
    for (std::size_t i = 0; i < steps.size(); ++i) {
        CAPTURE(i);
        CHECK(panel.update(steps[i].first).second == steps[i].second);
    }
    CHECK(panel.pending() == "Opening " + declared->info_.uri.as_str());
    CHECK(panel.password().empty()); // taken by the connect
    panel.failed("no Chrome or Chromium is installed");
    CHECK_FALSE(panel.pending().has_value());
    CHECK(panel.error() == "no Chrome or Chromium is installed");
    panel.set_browser(vs::browser_status::Missing{});
    CHECK_FALSE(panel.update(pm::OpenUrl{}).second.has_value()); // no browser, no request
}

TEST_CASE("attached surfaces become tabs and close") {
    auto dir = loa::runtime::TempDir::create();
    REQUIRE(dir);
    auto panel = make_panel(dir->path());
    const auto tab = fake("browser", "tab1", vs::SurfaceMode::Pixel);
    const auto doc = fake("docs", "1", vs::SurfaceMode::Document);
    const auto streams = panel.attach(tab, "browser");
    const auto fetch = panel.attach(doc, "docs");
    CHECK(streams.work.size() == 3);
    REQUIRE(fetch.work.size() == 1);
    CHECK(fetch.work[0] == panel::Routed{doc->info_.uri, work::FetchDocument{}});
    REQUIRE(panel.active());
    CHECK(*panel.active() == doc->info_.uri);
    CHECK(panel.open_surfaces().size() == 2);
    CHECK(panel.surface(tab->info_.uri)->pixel() != nullptr);
    CHECK(panel.surface(doc->info_.uri)->document() != nullptr);
    (void)panel.update(pm::Select{tab->info_.uri});
    CHECK(*panel.active() == tab->info_.uri);
    CHECK(panel.update(pm::Close{tab->info_.uri}).second == std::optional<Intent>{vs::intent::Close{tab->info_.uri}});
    REQUIRE(panel.active());
    CHECK(*panel.active() == doc->info_.uri); // the neighbour tab becomes active
    CHECK(panel.open_surfaces().size() == 1);
    (void)panel.update(pm::Launcher{});
    CHECK(panel.active() == nullptr);
    panel.detach(doc->info_.uri);
    CHECK(panel.open_surfaces().empty());
}

TEST_CASE("tab messages route to their surface and capture choices ask the app once") {
    auto dir = loa::runtime::TempDir::create();
    REQUIRE(dir);
    auto panel = make_panel(dir->path());
    const auto tab = fake("browser", "a-very-long-tab-name-indeed", vs::SurfaceMode::Pixel);
    (void)panel.attach(tab, "browser");
    CHECK(panel.tabs()[0].label.size() < tab->info_.name.size());
    auto [task, event] = panel.update(pm::Surface{tab->info_.uri, msg::Panel{{16, 8}}});
    REQUIRE(task.work.size() == 1);
    CHECK(task.work[0].uri == tab->info_.uri);
    CHECK(std::holds_alternative<work::Send>(task.work[0].work));
    // Re-attaching an open surface only activates it.
    (void)panel.update(pm::Launcher{});
    CHECK(panel.attach(tab, "browser").empty());
    CHECK(*panel.active() == tab->info_.uri);

    // The virtual display can't be picked while it is missing.
    CHECK_FALSE(panel.update(pm::Capture{Capture::VirtualDisplay}).second.has_value());
    panel.set_capture(CaptureStatus{Capture::Screencast, std::nullopt, false});
    CHECK(panel.update(pm::Capture{Capture::VirtualDisplay}).second == std::optional<Intent>{vs::intent::SetCapture{Capture::VirtualDisplay}});
    CHECK_FALSE(panel.update(pm::Capture{Capture::VirtualDisplay}).second.has_value());
}

#if defined(LOA_REMOTE_VIDEO)
TEST_CASE("the live video toggle asks the app only on a change") {
    auto dir = loa::runtime::TempDir::create();
    REQUIRE(dir);
    auto panel = make_panel(dir->path());
    CHECK_FALSE(panel.update(pm::Video{false}).second.has_value());
    CHECK(panel.update(pm::Video{true}).second == std::optional<Intent>{vs::intent::SetVideo{true}});
    CHECK(panel.video().enabled);
    CHECK_FALSE(panel.update(pm::Video{true}).second.has_value());
}
#endif

TEST_CASE("browsers are named by kind or executable") {
    CHECK(browser_name("/home/u/.cache/ms-playwright/chromium_headless_shell-1/chrome") == "Chromium headless shell (Playwright)");
    CHECK(browser_name("/home/u/.cache/ms-playwright/chromium-1/chrome") == "Chromium (Playwright)");
    CHECK(browser_name("/usr/bin/chromium") == "chromium");
}
