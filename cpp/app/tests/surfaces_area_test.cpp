// The surfaces area (port of `loa-app/src/areas/surfaces/{tests,video}.rs`): the launcher from
// the palette, the surface tools listed, the browser capture choice saved in `[surfaces]`, and
// the live video status the launcher shows.

#include "areas/surfaces/surfaces.hpp"

#include <algorithm>

#include <doctest/doctest.h>

#include "loa/app/view/surface.hpp"
#include "support/test_app.hpp"

using namespace loa::app;
using loa::app::testing::TestApp;
namespace sa = loa::app::areas::surfaces;
namespace vs = loa::view::surface;
namespace video = loa::surfaces::video;

namespace {

void capture(TestApp& test, vs::Capture mode) {
    test.send(sa::State::message(sa::msg::Panel{loa::ui::surface::panel::msg::Capture{mode}}));
}

video::ProbeResult probe(std::string name, video::Codec codec, video::Accel accel, std::optional<std::string> error = std::nullopt) {
    return video::ProbeResult{video::EncoderInfo{std::move(name), codec, accel}, std::move(error)};
}

} // namespace

TEST_CASE("surfaces: the launcher opens from the palette and hub events reach the area") {
    TestApp test;
    for (const auto* id : {"surfaces.browser", "surfaces.document", "surfaces.vm"}) {
        test.command(id);
        const auto& tabs = test.app->ctx.shell.tabs();
        CHECK_MESSAGE(std::ranges::any_of(tabs, [](const auto& tab) { return tab.id == sa::TAB; }), id, " opens the Surfaces tab");
        CHECK_FALSE(test.area<sa::State>().panel().active());
    }
    test.pump_feeds();
    CHECK_MESSAGE(std::ranges::any_of(test.app->ctx.app_tools, [](const auto& tool) { return tool.name.find("screenshot") != std::string::npos; }),
                  "the surface tools are listed");
}

TEST_CASE("surfaces: the browser capture choice is saved, and the virtual display only offered where it runs") {
    TestApp test;
    auto& area = test.area<sa::State>();
    CHECK(area.panel().capture().selected == vs::Capture::Screencast);

    // The launcher offers the virtual display only where it can run.
    const bool offered = !area.panel().capture().virtual_display_missing;
    capture(test, vs::Capture::VirtualDisplay);
    auto saved = test.app->ctx.services.storage.config.load();
    REQUIRE(saved);
    const auto expected = offered ? vs::Capture::VirtualDisplay : vs::Capture::Screencast;
    CHECK(area.panel().capture().selected == expected);
    CHECK(view::capture_to_view(area.hub()->capture()) == expected);
    CHECK(saved->surfaces.browser_capture ==
          (offered ? loa::settings::app::BrowserCapture::VirtualDisplay : loa::settings::app::BrowserCapture::Screencast));

    capture(test, vs::Capture::Screencast);
    saved = test.app->ctx.services.storage.config.load();
    REQUIRE(saved);
    CHECK(saved->surfaces.browser_capture == loa::settings::app::BrowserCapture::Screencast);
    CHECK(area.hub()->capture() == loa::surfaces::CaptureMode::Screencast);
    CHECK(area.panel().capture().selected == vs::Capture::Screencast);

    for (auto mode : {vs::Capture::Screencast, vs::Capture::VirtualDisplay})
        CHECK(view::capture_to_view(view::capture_from_view(mode)) == mode);
}

TEST_CASE("surfaces: live video shows the H.264 encoder and the working hardware encoders") {
    const auto live = video::LiveVideo::with_probed(
        std::nullopt, {probe("ffmpeg hevc_vaapi", video::Codec::Hevc, video::Accel::Vaapi),
                       probe("ffmpeg h264_nvenc", video::Codec::H264, video::Accel::Nvenc, "no device"),
                       probe("ffmpeg h264_vaapi", video::Codec::H264, video::Accel::Vaapi),
                       probe("openh264", video::Codec::H264, video::Accel::Software)});
    const auto status = sa::video_status(true, live);
    CHECK(status.enabled);
    REQUIRE(status.encoder);
    CHECK(status.encoder->encoding == vs::Encoding::H264);
    CHECK(status.hardware == std::vector<std::string>{"hevc_vaapi", "h264_vaapi"});

    // Without hardware the renderer still gets software H.264.
    const auto software = sa::video_status(false, video::LiveVideo::with_probed(std::nullopt, {}));
    CHECK_FALSE(software.enabled);
    REQUIRE(software.encoder);
    CHECK(software.encoder->encoding == vs::Encoding::H264);
    CHECK(software.hardware.empty());
}
