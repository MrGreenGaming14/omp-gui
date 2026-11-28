// One smoke test against a real local Chrome, skipped with a loud message
// unless `LOA_CHROME` is set. Port of `loa-surfaces/tests/browser.rs`.

#include <doctest/doctest.h>

#include <asio.hpp>

#include <cstdlib>
#include <filesystem>
#include <iostream>
#include <tuple>

#include "loa/contracts/surface_channel.hpp"
#include "loa/runtime/fs.hpp"
#include "loa/surfaces/browser/server.hpp"
#include "loa/surfaces/testkit.hpp"

using namespace loa::surfaces::browser;
namespace cs = loa::contracts::surface;
using loa::surfaces::testkit::html_data_url;
using loa::surfaces::testkit::next_frame;

namespace {

constexpr std::string_view PAGE = R"(<title>Smoke</title><style>body{margin:0} .abs{position:absolute} .gone{display:none}</style>
<script>console.log('script text must not show')</script>
<div class=gone>Hidden secret</div>
<button class=abs style='left:20px;top:30px;width:100px;height:40px' onclick="document.getElementById('out').textContent='saved ' + innerWidth">Save</button>
<p class=abs id=out style='left:20px;top:150px'>idle</p>)";

// The Chrome for real-browser tests, which run only when `LOA_CHROME` is set:
// to the browser's path, or to anything else to find the installed one.
std::optional<std::filesystem::path> chrome() {
    const char* wanted = std::getenv("LOA_CHROME");
    if (!wanted) return std::nullopt;
    if (std::filesystem::is_regular_file(wanted)) return std::filesystem::path(wanted);
    const char* path = std::getenv("PATH");
    auto found = find_chrome(path ? std::optional<std::string_view>(path) : std::nullopt, loa::runtime::fs::home(), false);
    REQUIRE_MESSAGE(found, "LOA_CHROME is set but no Chrome or Chromium is installed");
    return found;
}

bool process_running(std::uint32_t pid) { return std::filesystem::exists("/proc/" + std::to_string(pid)); }

asio::awaitable<void> smoke(std::filesystem::path executable) {
    auto config = BrowserConfig::make(std::move(executable));
    config.sandbox = false;
    config.width = 640;
    config.height = 480;
    auto launched = co_await BrowserServer::launch(config);
    REQUIRE_MESSAGE(launched, (launched ? "" : launched.error().message()));
    auto server = *launched;
    auto opened = co_await server->open_tab(html_data_url(PAGE));
    REQUIRE_MESSAGE(opened, (opened ? "" : opened.error().message()));
    auto tab = *opened;
    auto listed = co_await server->list();
    REQUIRE(listed);
    REQUIRE(listed->size() == 1);
    CHECK(std::tuple((*listed)[0].name, (*listed)[0].width, (*listed)[0].height) == std::tuple(std::string("Smoke"), 640U, 480U));
    auto doc = co_await tab->document();
    CHECK((!doc && doc.error().kind() == cs::SurfaceError::Kind::Mode));

    auto text = co_await tab->page_text();
    REQUIRE(text);
    CHECK_MESSAGE((text->find("idle") != std::string::npos && text->find("script text") == std::string::npos &&
                   text->find("Hidden secret") == std::string::npos),
                  *text);
    auto outline = co_await tab->outline();
    REQUIRE(outline);
    const auto save = std::ranges::find_if(*outline, [](const OutlineNode& n) { return n.role == "button" && n.name == "Save"; });
    REQUIRE_MESSAGE(save != outline->end(), "the Save button");
    CHECK(save->bounds == Bounds{20, 30, 100, 40});

    auto frames = tab->frames();
    REQUIRE(frames);
    // Chrome may paint once before the viewport override takes effect; the
    // keyframe is asked for after the page settles.
    std::shared_ptr<const cs::Frame> first;
    for (;;) {
        first = co_await next_frame(**frames);
        if (first->width == 640 && first->height == 480) break;
    }
    REQUIRE(co_await tab->request_keyframe());
    CHECK((co_await next_frame(**frames))->seq > first->seq);

    auto input = [](std::uint64_t seq, cs::Input in) { return cs::InputEvent{seq, 0, std::move(in)}; };
    REQUIRE(co_await tab->input(input(1, cs::input::Resize{320, 240})));
    CHECK(std::pair(tab->info().width, tab->info().height) == std::pair(320U, 240U));
    const auto [x, y] = save->bounds.center();
    for (auto [seq, pressed] : {std::pair<std::uint64_t, bool>{2, true}, {3, false}}) {
        REQUIRE(co_await tab->input(input(seq, cs::input::MouseButton{x, y, cs::MouseButton::Left, pressed, {}})));
    }
    auto after = co_await tab->page_text();
    REQUIRE(after);
    CHECK_MESSAGE(after->find("saved 320") != std::string::npos, *after);
    std::shared_ptr<const cs::Frame> resized;
    for (;;) {
        resized = co_await next_frame(**frames);
        if (resized->width == 320) break;
    }
    CHECK(resized->height == 240);
    auto png = co_await tab->screenshot();
    REQUIRE(png);
    REQUIRE(png->size() >= 8);
    CHECK(std::vector<std::uint8_t>(png->begin(), png->begin() + 8) ==
          std::vector<std::uint8_t>{0x89, 'P', 'N', 'G', '\r', '\n', 0x1a, '\n'});

    const auto pids = co_await server->process_ids();
    REQUIRE(!pids.empty());
    CHECK(process_running(pids[0]));
    REQUIRE(co_await server->shutdown());
    CHECK_MESSAGE(!process_running(pids[0]), "chrome still running");
    auto again = co_await server->open_tab("about:blank");
    CHECK((!again && again.error().kind() == BrowserError::Kind::Closed));
    auto empty = co_await server->list();
    REQUIRE(empty);
    CHECK(empty->empty());
}

} // namespace

TEST_CASE("a tab streams, reads, takes input, resizes and shuts down") {
    auto executable = chrome();
    if (!executable) {
        std::cerr << "SKIPPED: real-Chrome test; set LOA_CHROME=1 (or to a browser path) to run it\n";
        return;
    }
    asio::io_context io;
    bool done = false;
    asio::co_spawn(io, smoke(*executable), [&](std::exception_ptr e) {
        io.stop();
        if (e) std::rethrow_exception(e);
        done = true;
    });
    io.run_for(std::chrono::seconds(120));
    CHECK(done);
}
