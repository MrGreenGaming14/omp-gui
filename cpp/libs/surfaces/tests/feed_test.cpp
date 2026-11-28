#include "loa/surfaces/feed.hpp"

#include <doctest/doctest.h>

#include <asio.hpp>

#include "loa/contracts/surface_channel.hpp"

using namespace loa::surfaces;

TEST_CASE("frames_are_stamped_with_the_last_input_sent_before_capture") {
    InputLog log;
    CHECK(log.seq_before(1'000) == 0);
    // A second viewer with lower seqs never moves the stamp back.
    log.record(1, 100);
    log.record(3, 200);
    log.record(2, 300);
    CHECK(log.seq_before(99) == 0);
    CHECK(log.seq_before(100) == 1);
    CHECK(log.seq_before(250) == 3);
    CHECK(log.seq_before(10'000) == 3);
}

TEST_CASE("InputLog keeps the last INPUT_HISTORY inputs") {
    InputLog log;
    for (std::uint64_t i = 1; i <= INPUT_HISTORY + 1; ++i) log.record(i, i * 10);
    CHECK(log.seq_before(10) == 0); // forgotten
    CHECK(log.seq_before(20) == 2);
}

TEST_CASE("FrameFeed numbers frames, counts viewers and republishes the last keyframe") {
    asio::io_context io;
    FrameFeed feed(io.get_executor(), 2);
    CHECK_FALSE(feed.republish_keyframe());
    feed.publish({}); // nobody watching
    auto rx = feed.subscribe();
    CHECK(feed.viewers() == 1);
    Frame key;
    key.seq = 99;
    key.keyframe = true;
    key.data = {1, 2};
    feed.publish(key);
    CHECK(feed.republish_keyframe());
    std::vector<std::shared_ptr<const Frame>> got;
    asio::co_spawn(io, [&]() -> asio::awaitable<void> {
        for (int i = 0; i < 2; ++i) got.push_back(*co_await rx->recv());
    }, asio::detached);
    io.run();
    REQUIRE(got.size() == 2);
    CHECK(got[0]->seq == 2);
    CHECK(got[1]->seq == 3);
    CHECK((got[1]->keyframe && got[1]->data == key.data));
    // A lagging viewer loses the oldest frames.
    for (int i = 0; i < 3; ++i) feed.publish({});
    CHECK(rx->take_lagged() == 1);
    rx.reset();
    CHECK(feed.viewers() == 0);
}
