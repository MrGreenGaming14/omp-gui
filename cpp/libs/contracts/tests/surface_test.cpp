#include "loa/contracts/surface.hpp"

#include <doctest/doctest.h>

#include <asio.hpp>

#include "loa/contracts/surface_channel.hpp"

using namespace loa::contracts::surface;

TEST_CASE("SurfaceUri accepts surface://<server>/<name> and rejects the rest with Uri") {
    const auto uri = SurfaceUri::parse("surface://browser/tab1/sub");
    REQUIRE(uri);
    CHECK(uri->server() == "browser");
    CHECK(uri->name() == "tab1/sub");
    CHECK(SurfaceUri::make("vm", "x")->as_str() == "surface://vm/x");
    for (const char* bad : {"", "surface://", "surface://browser", "surface://browser/", "surface:///tab", "surface:/b/t",
                            "Surface://b/t", "http://b/t", " surface://b/t"}) {
        CAPTURE(bad);
        const auto parsed = SurfaceUri::parse(bad);
        REQUIRE_FALSE(parsed);
        CHECK(parsed.error().kind() == SurfaceError::Kind::Uri);
        CHECK(parsed.error().detail() == bad);
    }
    CHECK_FALSE(SurfaceUri::make("", "x"));
}

TEST_CASE("ALPN and SurfaceError messages match Rust") {
    CHECK(ALPN == "omp/surface/1");
    CHECK(SCHEME == "surface");
    const auto uri = *SurfaceUri::parse("surface://docs/a.pdf");
    CHECK(SurfaceError::uri("x").message() == "`x` isn't a surface://<server>/<name> uri");
    CHECK(SurfaceError::not_found(uri).message() == "no surface surface://docs/a.pdf");
    CHECK(SurfaceError::mode(uri, SurfaceMode::Document).message() == "surface://docs/a.pdf is a document surface");
    CHECK(SurfaceError::closed().message() == "the surface is closed");
    CHECK(SurfaceError::backend("boom").message() == "boom");
}

TEST_CASE("a lagging frame receiver drops the oldest frames and ends Closed after the sender") {
    asio::io_context io;
    auto sender = std::make_unique<FrameSender>(io.get_executor(), 2);
    auto rx = sender->subscribe();
    for (std::uint64_t seq = 1; seq <= 3; ++seq) sender->send(std::make_shared<Frame>(Frame{.seq = seq}));
    sender.reset();
    std::vector<std::uint64_t> got;
    bool closed = false;
    asio::co_spawn(io, [&]() -> asio::awaitable<void> {
        for (;;) {
            auto frame = co_await rx->recv();
            if (!frame) {
                closed = frame.error().kind() == SurfaceError::Kind::Closed;
                co_return;
            }
            got.push_back((*frame)->seq);
        }
    }, asio::detached);
    io.run();
    CHECK(got == std::vector<std::uint64_t>{2, 3});
    CHECK(rx->take_lagged() == 1);
    CHECK(closed);
}
