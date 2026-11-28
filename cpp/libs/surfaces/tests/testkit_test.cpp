#include "loa/surfaces/testkit.hpp"

#include <doctest/doctest.h>

#include <asio.hpp>

using namespace loa::surfaces::testkit;
namespace cs = loa::contracts::surface;

TEST_CASE("FakeSurface hands frames, inputs and keyframe requests to its probe") {
    asio::io_context io;
    cs::SurfaceInfo info{*cs::SurfaceUri::parse("surface://fake/one")};
    auto [surface, probe] = FakeSurface::make(io.get_executor(), info, std::nullopt);
    bool done = false;
    asio::co_spawn(io, [&, surface = surface, probe = probe]() -> asio::awaitable<void> {
        auto rx = surface->frames();
        REQUIRE(rx);
        cs::Frame frame;
        frame.seq = 7;
        probe.frames->send(std::make_shared<const cs::Frame>(frame));
        CHECK((co_await next_frame(**rx))->seq == 7);
        REQUIRE(co_await surface->request_keyframe());
        CHECK(surface->keyframes_requested() == 1);
        CHECK(co_await probe.keyframes->recv());
        cs::InputEvent event{3, 4, cs::input::Text{"hi"}};
        REQUIRE(co_await surface->input(event));
        CHECK(co_await probe.inputs->recv() == event);
        const auto doc = co_await surface->document();
        REQUIRE_FALSE(doc);
        CHECK(doc.error().kind() == cs::SurfaceError::Kind::Mode);
        CHECK((co_await surface->screenshot())->empty());
        done = true;
    }, asio::detached);
    io.run();
    CHECK(done);
}

TEST_CASE("solid and html_data_url") {
    CHECK(solid(2, 1, {1, 2, 3, 4}) == std::vector<std::uint8_t>{1, 2, 3, 4, 1, 2, 3, 4});
    CHECK(html_data_url("<b>a b</b>").starts_with("data:text/html,"));
}
