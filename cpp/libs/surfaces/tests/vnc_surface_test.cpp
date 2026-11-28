// The RFB client against a fake RFB server on loopback. Port of the surface
// tests in `vnc/tests.rs`.

#include <doctest/doctest.h>

#include <asio.hpp>
#include <asio/experimental/awaitable_operators.hpp>
#include <asio/experimental/channel.hpp>

#include <atomic>
#include <tuple>

#include "loa/contracts/surface_channel.hpp"
#include "loa/surfaces/testkit.hpp"
#include "loa/surfaces/vnc/proto.hpp"
#include "loa/surfaces/vnc/surface.hpp"
#include "loa/surfaces/vnc/testkit.hpp"

using namespace loa::surfaces;
using namespace loa::surfaces::vnc;
namespace cs = loa::contracts::surface;
namespace rfb = loa::surfaces::testkit::rfb;
using loa::surfaces::testkit::next_frame;
using rfb::FakeRfb;
using rfb::rect;
using rfb::update;
using Bytes = std::vector<std::uint8_t>;

namespace {

constexpr Pixel R{255, 0, 0, 255};
constexpr Pixel G{0, 255, 0, 255};
constexpr Pixel B{0, 0, 255, 255};
constexpr Pixel W{255, 255, 255, 255};
constexpr Pixel K{0, 0, 0, 255};

Bytes concat(std::initializer_list<Bytes> parts) {
    Bytes out;
    for (const auto& p : parts) out.insert(out.end(), p.begin(), p.end());
    return out;
}

Bytes px(std::initializer_list<Pixel> pixels) {
    Bytes out;
    for (const auto& p : pixels) out.insert(out.end(), p.begin(), p.end());
    return out;
}

// One row of pixels as a raw rectangle payload.
Bytes raw(std::initializer_list<Pixel> pixels) {
    return rfb::raw(RgbaImage{std::uint32_t(pixels.size()), 1, px(pixels)});
}

cs::SurfaceUri uri() { return *cs::SurfaceUri::make("vm", "desk"); }

// A coroutine started now and joined later (Rust `tokio::spawn` + `JoinHandle`).
template <typename T>
struct Join {
    explicit Join(const asio::any_io_executor& ex) : done(ex, 1) {}
    std::optional<T> value;
    asio::experimental::channel<void(asio::error_code)> done;

    asio::awaitable<T> get() {
        if (!value) co_await done.async_receive(asio::use_awaitable);
        co_return std::move(*value);
    }
};

template <typename T>
std::shared_ptr<Join<T>> spawn(const asio::any_io_executor& ex, asio::awaitable<T> task) {
    auto join = std::make_shared<Join<T>>(ex);
    asio::co_spawn(ex, std::move(task), [join](std::exception_ptr e, T value) {
        if (e) std::rethrow_exception(e);
        join->value = std::move(value);
        join->done.try_send(asio::error_code{});
    });
    return join;
}

using Opened = cs::Result<std::shared_ptr<RfbSurface>>;

struct Harness {
    asio::ip::tcp::acceptor listener;
    std::shared_ptr<RfbServer> server;
    std::shared_ptr<std::atomic<std::size_t>> password_calls = std::make_shared<std::atomic<std::size_t>>(0);

    // An `RfbServer` with the one endpoint `desk` on a loopback listener.
    Harness(const asio::any_io_executor& ex, std::optional<std::string> password)
        : listener(ex, asio::ip::tcp::endpoint(asio::ip::make_address("127.0.0.1"), 0)) {
        const auto address = "127.0.0.1:" + std::to_string(listener.local_endpoint().port());
        auto passwords = password_fn([calls = password_calls, password](std::string_view endpoint) {
            CHECK(endpoint == "desk");
            calls->fetch_add(1);
            return password;
        });
        RfbOptions options;
        options.backoff_initial = std::chrono::milliseconds(1);
        options.backoff_max = std::chrono::milliseconds(20);
        server = std::make_shared<RfbServer>(std::vector<RfbEndpoint>{{"desk", address}}, passwords, options);
    }

    std::shared_ptr<Join<Opened>> open() {
        return spawn(listener.get_executor(), [](std::shared_ptr<RfbServer> s) -> asio::awaitable<Opened> {
            co_return co_await s->open_rfb(uri());
        }(server));
    }

    // Opens `surface://vm/desk` against a fake that completes the handshake.
    asio::awaitable<std::pair<std::shared_ptr<RfbSurface>, FakeRfb>> connect(std::optional<std::string_view> password,
                                                                            std::uint16_t width, std::uint16_t height) {
        auto opening = open();
        auto fake = co_await FakeRfb::accept(listener);
        const bool shook = co_await fake.handshake(password, width, height);
        REQUIRE(shook);
        auto surface = co_await opening->get();
        REQUIRE(surface);
        co_return std::pair{std::move(*surface), std::move(fake)};
    }
};

void run(asio::awaitable<void> (*body)(asio::any_io_executor)) {
    asio::io_context io;
    bool done = false;
    asio::co_spawn(io, body(io.get_executor()), [&](std::exception_ptr e) {
        if (e) std::rethrow_exception(e);
        done = true;
    });
    io.run_for(std::chrono::seconds(60));
    CHECK(done);
}

asio::awaitable<void> updates_become_frames_and_keyframes(asio::any_io_executor ex) {
    Harness h(ex, std::nullopt);
    auto connected = co_await h.connect(std::nullopt, 4, 2);
    auto& surface = connected.first;
    auto& fake = connected.second;
    const auto info = surface->info();
    CHECK(std::tuple(info.name, info.width, info.height) == std::tuple(std::string("fake desk"), 4U, 2U));
    auto rx = *surface->frames();

    // Only part of the screen arrives, yet the first frame is the whole picture.
    const Bytes partial = update({rect(1, 0, 2, 1, 0, raw({R, G}))});
    co_await fake.send(partial);
    auto frame = co_await next_frame(*rx);
    CHECK(std::tuple(frame->keyframe, frame->seq, frame->width, frame->height, frame->encoding) ==
          std::tuple(true, 1ULL, 4U, 2U, cs::Encoding::Rgba));
    CHECK(frame->region == Rect{0, 0, 4, 2});
    CHECK(frame->data == px({K, R, G, K, K, K, K, K}));
    co_await fake.expect_request(4, 2);

    // CopyRect overlapping its source, after cut text, a bell and a colour map are skipped.
    const Bytes skipped = concat({Bytes{3, 0, 0, 0, 0, 0, 0, 5}, Bytes{'h', 'e', 'l', 'l', 'o'}, Bytes{2},
                                  Bytes{1, 0, 0, 0, 0, 1}, Bytes(6, 0)});
    const Bytes copy = concat({skipped, update({rect(1, 0, 3, 2, 1, Bytes{0, 0, 0, 0})})});
    co_await fake.send(copy);
    frame = co_await next_frame(*rx);
    CHECK_FALSE(frame->keyframe);
    CHECK(frame->region == Rect{1, 0, 3, 2});
    CHECK(frame->data == px({K, R, G, K, K, K}));
    co_await fake.expect_request(4, 2);
    const auto keyframe = co_await surface->request_keyframe();
    REQUIRE(keyframe);
    const auto again = co_await next_frame(*rx);
    CHECK(again->data == px({K, K, R, G, K, K, K, K}));

    // A cursor-only update changes no pixels, so no frame; a resize is a keyframe.
    const Bytes cursor_only = update({rect(0, 0, 1, 1, -239, concat({raw({W}), Bytes{0x80}}))});
    co_await fake.send(cursor_only);
    co_await fake.expect_request(4, 2);
    const Bytes resize = update({rect(0, 0, 3, 1, -223, Bytes{}), rect(0, 0, 3, 1, 0, raw({G, B, W}))});
    co_await fake.send(resize);
    frame = co_await next_frame(*rx);
    CHECK(std::tuple(frame->keyframe, frame->width, frame->height) == std::tuple(true, 3U, 1U));
    CHECK(frame->data == px({G, B, W}));
    CHECK(std::pair(surface->info().width, surface->info().height) == std::pair(3U, 1U));
    co_await fake.expect_request(3, 1);

    const auto shot = co_await surface->screenshot();
    REQUIRE(shot);
    const auto png = decode_png(*shot);
    REQUIRE(png);
    CHECK(std::tuple(png->width, png->height) == std::tuple(3U, 1U));
    CHECK(png->data == px({G, B, W}));
    const auto doc = co_await surface->document();
    REQUIRE_FALSE(doc);
    CHECK(doc.error().kind() == cs::SurfaceError::Kind::Mode);
}

asio::awaitable<void> cursor_shapes_are_published_without_changing_the_framebuffer(asio::any_io_executor ex) {
    using namespace asio::experimental::awaitable_operators;
    Harness h(ex, std::nullopt);
    auto connected = co_await h.connect(std::nullopt, 4, 2);
    auto& surface = connected.first;
    auto& fake = connected.second;
    auto frames = *surface->frames();
    auto cursors = surface->cursor();
    CHECK(cursors->current() == nullptr);
    const Bytes payload = concat({raw({R, G, B, W}), Bytes{0x80, 0x40}});
    const Bytes shaped = update({rect(8, 1, 2, 2, -239, payload), rect(0, 0, 1, 1, 0, raw({G}))});
    co_await fake.send(shaped);
    const auto frame = co_await next_frame(*frames);
    CHECK(frame->data == px({G, K, K, K, K, K, K, K}));
    asio::steady_timer timer(ex, std::chrono::seconds(10));
    const auto first = co_await (cursors->changed(0) || timer.async_wait(asio::use_awaitable));
    REQUIRE(first.index() == 0);
    auto cursor = cursors->current();
    REQUIRE(cursor);
    CHECK(std::tuple(cursor->hotspot_x, cursor->hotspot_y, cursor->width, cursor->height) == std::tuple(8U, 1U, 2U, 2U));
    CHECK(cursor->rgba == px({R, Pixel{0, 255, 0, 0}, Pixel{0, 0, 255, 0}, W}));
    const auto seen = cursors->version();
    co_await fake.expect_request(4, 2);
    const Bytes empty_shape = update({rect(0, 0, 0, 0, -239, Bytes{})});
    co_await fake.send(empty_shape);
    timer.expires_after(std::chrono::seconds(10));
    const auto second = co_await (cursors->changed(seen) || timer.async_wait(asio::use_awaitable));
    REQUIRE(second.index() == 0);
    cursor = cursors->current();
    CHECK(std::pair(cursor->width, cursor->height) == std::pair(0U, 0U));
    CHECK(cursor->rgba.empty());
}

asio::awaitable<void> input_goes_out_as_rfb_messages_and_stamps_the_next_frame(asio::any_io_executor ex) {
    Harness h(ex, std::nullopt);
    auto connected = co_await h.connect(std::nullopt, 4, 2);
    auto& surface = connected.first;
    auto& fake = connected.second;
    auto rx = *surface->frames();
    const cs::Modifiers none;
    const auto key = [](bool down, std::uint32_t sym) { return Msg::key(down, sym).bytes(); };
    const auto ptr = [](std::uint8_t mask, std::uint16_t x, std::uint16_t y) { return Msg::pointer(mask, x, y).bytes(); };
    const std::vector<std::tuple<std::uint64_t, cs::Input, Bytes>> cases = {
        {7, cs::input::MouseButton{2.0F, 1.0F, cs::MouseButton::Left, true, none}, ptr(1, 2, 1)},
        {8, cs::input::Key{"Enter", "Enter", true, none}, key(true, 0xff0d)},
        {9, cs::input::Text{"Hi"}, concat({key(true, 0x48), key(false, 0x48), key(true, 0x69), key(false, 0x69)})},
        {10, cs::input::Scroll{3.0F, 0.0F, 0.0F, -40.0F, none}, concat({ptr(1, 3, 0), ptr(1 | 8, 3, 0), ptr(1, 3, 0)})},
    };
    for (const auto& [seq, input, wire] : cases) {
        const auto sent = co_await surface->input(cs::InputEvent{seq, 0, input});
        REQUIRE(sent);
        co_await fake.expect(wire);
        co_await fake.expect_request(4, 2);
    }
    // Resize has no RFB message; the next update carries the last input's number.
    const auto resized = co_await surface->input(cs::InputEvent{11, 0, cs::input::Resize{10, 10}});
    REQUIRE(resized);
    const Bytes one = update({rect(0, 0, 1, 1, 0, Bytes{1, 2, 3, 0})});
    co_await fake.send(one);
    const auto stamped = co_await next_frame(*rx);
    CHECK(stamped->input_seq == 10);
}

asio::awaitable<void> vnc_auth_reconnects_after_a_drop_and_wrong_passwords_fail(asio::any_io_executor ex) {
    {
        Harness h(ex, "hunter2");
        auto connected = co_await h.connect("hunter2", 2, 1);
        auto surface = connected.first;
        auto rx = *surface->frames();
        CHECK(h.password_calls->load() == 1);
        connected.second.socket().close();
        auto fake = co_await FakeRfb::accept(h.listener);
        const bool shook = co_await fake.handshake("hunter2", 2, 1);
        REQUIRE(shook);
        // The password was asked for again, not kept from the first handshake.
        CHECK(h.password_calls->load() == 2);
        // The first update after reconnecting is a keyframe again; input goes to the new connection.
        const Bytes blue = update({rect(1, 0, 1, 1, 0, Bytes{0, 0, 255, 0})});
        co_await fake.send(blue);
        const auto frame = co_await next_frame(*rx);
        CHECK(frame->keyframe);
        CHECK(frame->data == px({K, B}));
        co_await fake.expect_request(2, 1);
        const auto moved = co_await surface->input(cs::InputEvent{1, 0, cs::input::MouseMove{1.0F, 0.0F}});
        REQUIRE(moved);
        co_await fake.expect(Msg::pointer(0, 1, 0).bytes());
    }
    {
        Harness h(ex, "wrong");
        auto opening = h.open();
        auto fake = co_await FakeRfb::accept(h.listener);
        const bool shook = co_await fake.handshake("right", 2, 1);
        CHECK_FALSE(shook);
        const auto opened = co_await opening->get();
        REQUIRE_FALSE(opened);
        CHECK(opened.error().message().find("bad password") != std::string::npos);
    }
    {
        // An RFB 3.3 server with no auth and no desktop name: the endpoint names the surface.
        Harness h(ex, std::nullopt);
        auto opening = h.open();
        auto fake = co_await FakeRfb::accept(h.listener);
        const Bytes banner{'R', 'F', 'B', ' ', '0', '0', '3', '.', '0', '0', '3', '\n'};
        co_await fake.send(banner);
        co_await fake.expect(banner);
        co_await fake.send(Bytes{0, 0, 0, 1});
        co_await fake.expect(Bytes{1});
        co_await fake.server_init(1, 1, "");
        const auto opened = co_await opening->get();
        REQUIRE(opened);
        CHECK((*opened)->info().name == "desk");
    }
}

asio::awaitable<void> server_lists_and_reuses_connections(asio::any_io_executor ex) {
    Harness h(ex, std::nullopt);
    auto listed = co_await h.server->list();
    REQUIRE(listed);
    REQUIRE(listed->size() == 1);
    CHECK(std::tuple((*listed)[0].name, (*listed)[0].width, (*listed)[0].uri) == std::tuple(std::string("desk"), 0U, uri()));
    auto connected = co_await h.connect(std::nullopt, 4, 2);
    auto& surface = connected.first;
    auto again = co_await h.server->open_rfb(uri());
    REQUIRE(again);
    CHECK(again->get() == surface.get());
    listed = co_await h.server->list();
    CHECK((*listed)[0].name == "fake desk");
    const auto missing = co_await h.server->open(*cs::SurfaceUri::make("vm", "nope"));
    REQUIRE_FALSE(missing);
    CHECK(missing.error().kind() == cs::SurfaceError::Kind::NotFound);
    CHECK(h.server->add_endpoint(RfbEndpoint{"other", "127.0.0.1:1"}).value() == *cs::SurfaceUri::make("vm", "other"));
    const auto relisted = co_await h.server->list();
    CHECK(relisted->size() == 2);
}

} // namespace

TEST_CASE("updates_become_frames_and_keyframes") { run(updates_become_frames_and_keyframes); }

TEST_CASE("cursor_shapes_are_published_without_changing_the_framebuffer") {
    run(cursor_shapes_are_published_without_changing_the_framebuffer);
}

TEST_CASE("input_goes_out_as_rfb_messages_and_stamps_the_next_frame") {
    run(input_goes_out_as_rfb_messages_and_stamps_the_next_frame);
}

TEST_CASE("vnc_auth_reconnects_after_a_drop_and_wrong_passwords_fail") {
    run(vnc_auth_reconnects_after_a_drop_and_wrong_passwords_fail);
}

TEST_CASE("RfbServer lists endpoints and reuses open connections") { run(server_lists_and_reuses_connections); }
