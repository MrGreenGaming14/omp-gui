// The runner does a tab's surface work on the executor and answers on the "GUI" thread (here a
// queue the test pumps): frames stream in, calls report Done, inputs go out in order, and the
// document arrives once.

#include <doctest/doctest.h>

#include <chrono>
#include <deque>
#include <functional>
#include <mutex>
#include <thread>

#include <asio.hpp>

#include "loa/contracts/surface_channel.hpp"
#include "loa/runtime/executor.hpp"
#include "loa/ui/surface/runner.hpp"
#include "support.hpp"

using namespace loa::ui::surface;
namespace vs = loa::view::surface;

namespace {

struct Gui {
    std::mutex lock;
    std::deque<std::function<void()>> queue;
    std::vector<Message> received;

    Runner::Post post() {
        return [this](std::function<void()> fn) {
            std::scoped_lock g(lock);
            queue.push_back(std::move(fn));
        };
    }
    Runner::Deliver deliver() {
        return [this](Message m) { received.push_back(std::move(m)); };
    }
    // Runs queued GUI work until `done` holds or two seconds pass.
    bool pump(const std::function<bool()>& done) {
        const auto until = std::chrono::steady_clock::now() + std::chrono::seconds(2);
        while (std::chrono::steady_clock::now() < until) {
            std::deque<std::function<void()>> now;
            {
                std::scoped_lock g(lock);
                now.swap(queue);
            }
            for (auto& fn : now) fn();
            if (done()) return true;
            std::this_thread::sleep_for(std::chrono::milliseconds(2));
        }
        return done();
    }
    template <typename T> std::size_t count() const {
        return static_cast<std::size_t>(std::ranges::count_if(received, [](const Message& m) { return std::holds_alternative<T>(m); }));
    }
};

} // namespace

TEST_CASE("the runner streams frames, reports calls and sends inputs in order") {
    loa::runtime::Executor executor(2);
    vs::SurfaceInfo info{support::uri("fake", "pixels"), "Pixels", 2, 2, vs::SurfaceMode::Pixel, std::nullopt};
    auto surface = std::make_shared<support::FakeSurface>(info, std::nullopt);
    surface->sender = std::make_shared<loa::contracts::surface::FrameSender>(executor.context().get_executor(), 8);
    auto [state, opened] = State::open(surface, support::options());
    Gui gui;
    {
        Runner runner(surface, executor, gui.post(), gui.deliver());
        runner.run(std::move(opened));
        // Keyframe request and negotiation both report back.
        CHECK(gui.pump([&] { return gui.count<msg::Done>() == 2; }));
        CHECK(surface->keyframes_requested() == 1);

        auto f = std::make_shared<const vs::Frame>(vs::Frame{1, 0, 0, 2, 2, {0, 0, 2, 2}, vs::Encoding::Rgba, true, support::solid(2, 2, {1, 2, 3, 4})});
        CHECK(surface->sender->send(f) == 1);
        REQUIRE(gui.pump([&] { return gui.count<msg::Frame>() == 1; }));
        for (auto& m : gui.received)
            if (std::holds_alternative<msg::Frame>(m)) (void)state.update(std::move(m));
        CHECK(state.pixel()->shown_seq() == 1U);

        Task inputs;
        for (std::uint64_t seq = 1; seq <= 20; ++seq) inputs.work.emplace_back(work::Send{{seq, seq, input::Text{std::to_string(seq)}}});
        runner.run(std::move(inputs));
        REQUIRE(gui.pump([&] { return surface->inputs().size() == 20; }));
        const auto sent = surface->inputs();
        for (std::size_t i = 0; i < sent.size(); ++i) CHECK(sent[i].seq == i + 1);
        CHECK(gui.count<msg::Failed>() == 0);
    }
    // Dropping the runner stops its stream: frames sent afterwards never arrive.
    const auto frames = gui.count<msg::Frame>();
    (void)surface->sender->send(std::make_shared<const vs::Frame>(vs::Frame{2, 0, 0, 2, 2, {0, 0, 2, 2}, vs::Encoding::Rgba, true, {}}));
    (void)gui.pump([] { return false; });
    CHECK(gui.count<msg::Frame>() == frames);
    executor.stop();
}

TEST_CASE("the runner fetches a document once and reports a closed stream") {
    loa::runtime::Executor executor(1);
    vs::SurfaceInfo info{support::uri("docs", "a"), "a.png", 30, 20, vs::SurfaceMode::Document, "image/png"};
    auto surface = std::make_shared<support::FakeSurface>(info, vs::Document{"a.png", "image/png", {1, 2, 3}});
    Gui gui;
    Runner runner(surface, executor, gui.post(), gui.deliver());
    runner.run(Task::of(work::FetchDocument{}));
    REQUIRE(gui.pump([&] { return gui.count<msg::Document>() == 1; }));
    const auto& doc = std::get<msg::Document>(gui.received.back()).document;
    REQUIRE(doc);
    CHECK((*doc)->data == std::vector<std::uint8_t>{1, 2, 3});

    vs::SurfaceInfo pixels{support::uri("fake", "closing"), "Closing", 2, 2, vs::SurfaceMode::Pixel, std::nullopt};
    auto closing = std::make_shared<support::FakeSurface>(pixels, std::nullopt);
    closing->sender = std::make_shared<loa::contracts::surface::FrameSender>(executor.context().get_executor(), 8);
    auto rx = closing->sender->subscribe();
    Runner stream(closing, executor, gui.post(), gui.deliver());
    stream.run(Task::of(work::StreamFrames{rx}));
    closing->sender.reset();
    CHECK(gui.pump([&] { return gui.count<msg::StreamEnded>() == 1; }));
    executor.stop();
}
