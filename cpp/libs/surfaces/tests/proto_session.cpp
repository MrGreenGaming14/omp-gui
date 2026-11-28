// End to end `omp/surface/1` sessions over the in-memory transport (port of
// `loa-surfaces/tests/session.rs`; the video test belongs to S1b).

#include <doctest/doctest.h>

#include <asio.hpp>

#include <chrono>
#include <optional>

#include "loa/contracts/surface_channel.hpp"
#include "loa/surfaces/proto/server.hpp"
#include "loa/surfaces/proto/viewer.hpp"
#include "proto/mailbox.hpp"

using namespace loa::surfaces::proto;
namespace cs = loa::contracts::surface;
using loa::surfaces::proto::detail::Mailbox;

namespace {

const std::vector<cs::Encoding> ALL{cs::Encoding::Rgba, cs::Encoding::Jpeg, cs::Encoding::Png, cs::Encoding::H264, cs::Encoding::Hevc};

cs::SurfaceUri uri(const std::string& name) { return *cs::SurfaceUri::make("test", name); }

// A scripted surface: the test publishes frames and reads back inputs and keyframe requests.
class FakeSurface final : public cs::Surface {
  public:
    FakeSurface(const asio::any_io_executor& ex, cs::SurfaceInfo info, std::optional<cs::Document> document)
        : info_(std::move(info)), document_(std::move(document)), sender(ex, 64),
          inputs(std::make_shared<Mailbox<cs::InputEvent>>(ex)), keyframes(std::make_shared<Mailbox<int>>(ex)) {}

    cs::SurfaceInfo info() const override { return info_; }
    cs::Result<std::shared_ptr<cs::FrameReceiver>> frames() override {
        if (info_.mode != cs::SurfaceMode::Pixel) return std::unexpected(cs::SurfaceError::mode(info_.uri, info_.mode));
        return sender.subscribe();
    }
    loa::contracts::Awaitable<cs::Result<void>> request_keyframe() override {
        keyframes->push(1);
        co_return cs::Result<void>{};
    }
    loa::contracts::Awaitable<cs::Result<void>> input(cs::InputEvent event) override {
        inputs->push(std::move(event));
        co_return cs::Result<void>{};
    }
    loa::contracts::Awaitable<cs::Result<cs::Document>> document() override {
        if (!document_) co_return std::unexpected(cs::SurfaceError::mode(info_.uri, info_.mode));
        co_return *document_;
    }
    loa::contracts::Awaitable<cs::Result<cs::Bytes>> screenshot() override { co_return cs::Bytes{}; }

    cs::SurfaceInfo info_;
    std::optional<cs::Document> document_;
    cs::FrameSender sender;
    std::shared_ptr<Mailbox<cs::InputEvent>> inputs;
    std::shared_ptr<Mailbox<int>> keyframes;
};

std::shared_ptr<FakeSurface> fake(const asio::any_io_executor& ex, const std::string& name, std::optional<Bytes> document) {
    const auto mode = document ? cs::SurfaceMode::Document : cs::SurfaceMode::Pixel;
    cs::SurfaceInfo info{.uri = uri(name), .name = name, .width = 64, .height = 32, .mode = mode, .mime = std::nullopt};
    std::optional<cs::Document> doc;
    if (document) doc = cs::Document{.name = name, .mime = "application/pdf", .data = std::move(*document)};
    return std::make_shared<FakeSurface>(ex, std::move(info), std::move(doc));
}

cs::Frame frame(std::uint64_t seq, cs::Encoding encoding, bool keyframe, std::size_t len) {
    Bytes data(len);
    for (std::size_t i = 0; i < len; ++i) data[i] = static_cast<std::uint8_t>(i * 31 + seq);
    return {.seq = seq,
            .captured_us = seq * 16'667,
            .input_seq = 0,
            .width = 64,
            .height = 32,
            .region = {.x = 0, .y = 0, .width = 64, .height = 32},
            .encoding = encoding,
            .keyframe = keyframe,
            .data = std::move(data)};
}

// Spawns `serve`; the returned slot fills with its result.
std::shared_ptr<std::optional<Result<void>>> spawn_serve(const asio::any_io_executor& ex, std::shared_ptr<cs::Surface> surface,
                                                         Transport transport) {
    auto done = std::make_shared<std::optional<Result<void>>>();
    asio::co_spawn(ex, serve(std::move(surface), std::move(transport)), [done](std::exception_ptr, Result<void> r) { *done = std::move(r); });
    return done;
}

asio::awaitable<void> until(const std::function<bool()>& ready) {
    asio::steady_timer timer(co_await asio::this_coro::executor);
    while (!ready()) {
        timer.expires_after(std::chrono::milliseconds(1));
        co_await timer.async_wait(asio::use_awaitable);
    }
}

asio::awaitable<std::optional<PixelViewer>> view(std::shared_ptr<cs::Surface> surface, std::pair<Transport, Transport> ends,
                                                 const std::vector<cs::Encoding>& encodings) {
    auto ex = co_await asio::this_coro::executor;
    const auto u = surface->info().uri;
    spawn_serve(ex, std::move(surface), std::move(ends.first));
    auto opened = co_await open(std::move(ends.second), u, ViewerOptions(encodings));
    if (!opened || !std::holds_alternative<PixelViewer>(*opened)) co_return std::nullopt;
    co_return std::move(std::get<PixelViewer>(*opened));
}

asio::awaitable<std::optional<cs::Frame>> next_frame(PixelViewer& viewer) {
    auto event = co_await viewer.next_event();
    if (!event || !std::holds_alternative<cs::Frame>(*event)) co_return std::nullopt;
    co_return std::get<cs::Frame>(std::move(*event));
}

// Runs `test` on a fresh context and fails if it does not finish within 20 s.
void run(std::function<asio::awaitable<void>(asio::any_io_executor)> test) {
    asio::io_context io;
    bool finished = false;
    asio::co_spawn(io, [&]() -> asio::awaitable<void> {
        co_await test(io.get_executor());
        finished = true;
        io.stop();
    }, [](std::exception_ptr e) { if (e) std::rethrow_exception(e); });
    io.run_for(std::chrono::seconds(20));
    REQUIRE(finished);
}

} // namespace

TEST_CASE("frames, input, documents and rejection") {
    run([](asio::any_io_executor ex) -> asio::awaitable<void> {
        // Datagrams, then the stream fallback.
        for (std::optional<std::size_t> max_datagram : {std::optional<std::size_t>(1200), std::optional<std::size_t>()}) {
            CAPTURE(max_datagram.has_value());
            auto surface = fake(ex, "tab1", std::nullopt);
            auto viewer = co_await view(surface, memory_pair(ex, max_datagram), ALL);
            REQUIRE(viewer);
            CHECK(co_await surface->keyframes->pop());
            CHECK(viewer->info() == surface->info_);
            std::vector<cs::Frame> frames;
            for (std::uint64_t seq = 1; seq <= 3; ++seq) frames.push_back(frame(seq, cs::Encoding::Jpeg, true, 5000));
            for (const auto& f : frames) surface->sender.send(std::make_shared<cs::Frame>(f));
            for (const auto& f : frames) CHECK(co_await next_frame(*viewer) == f);
            auto big = frame(4, cs::Encoding::Png, true, 100);
            big.width = 128;
            big.height = 96;
            surface->sender.send(std::make_shared<cs::Frame>(big));
            auto resized = co_await viewer->next_event();
            REQUIRE(resized);
            CHECK(*resized == ViewerEvent(Resized{.width = 128, .height = 96}));
            CHECK(co_await next_frame(*viewer) == big);

            const std::vector<cs::Input> sent{
                cs::input::MouseMove{.x = 10, .y = 5},
                cs::input::MouseButton{.x = 10, .y = 5, .button = cs::MouseButton::Left, .pressed = true, .modifiers = {}},
                cs::input::Text{.text = "hello"},
            };
            for (std::size_t i = 0; i < sent.size(); ++i) {
                auto seq = viewer->input().send(sent[i]);
                REQUIRE(seq);
                CHECK(*seq == i + 1);
            }
            for (std::size_t i = 0; i < sent.size(); ++i) {
                auto got = co_await surface->inputs->pop();
                REQUIRE(got);
                CHECK(got->seq == i + 1);
                CHECK(got->input == sent[i]);
            }
        }

        // Documents arrive whole; another surface's uri is rejected.
        Bytes data(200'000);
        for (std::size_t i = 0; i < data.size(); ++i) data[i] = static_cast<std::uint8_t>(i % 251);
        auto doc = fake(ex, "report.pdf", data);
        {
            auto [server_end, viewer_end] = memory_pair(ex, 1200);
            auto server = spawn_serve(ex, doc, std::move(server_end));
            auto opened = co_await open(std::move(viewer_end), doc->info_.uri, ViewerOptions({}));
            REQUIRE(opened);
            REQUIRE(std::holds_alternative<DocumentViewer>(*opened));
            auto received = co_await std::get<DocumentViewer>(*opened).receive();
            REQUIRE(received);
            CHECK(received->data == data);
            co_await until([&] { return server->has_value(); });
            CHECK(server->value().has_value());
        }

        auto [server_end, viewer_end] = memory_pair(ex, 1200);
        spawn_serve(ex, doc, std::move(server_end));
        auto result = co_await open(std::move(viewer_end), uri("tab2"), ViewerOptions(ALL));
        REQUIRE_FALSE(result);
        CHECK(result.error().kind() == ProtoError::Kind::Rejected);
        CHECK(result.error().reject_code() == RejectCode::NotFound);
    });
}

TEST_CASE("closing a pixel viewer ends the session with a cloned input sender") {
    run([](asio::any_io_executor ex) -> asio::awaitable<void> {
        auto surface = fake(ex, "close", std::nullopt);
        auto [server_end, viewer_end] = memory_pair(ex, std::nullopt);
        auto server = spawn_serve(ex, surface, std::move(server_end));
        auto opened = co_await open(std::move(viewer_end), surface->info_.uri, ViewerOptions(ALL));
        REQUIRE(opened);
        REQUIRE(std::holds_alternative<PixelViewer>(*opened));
        auto& viewer = std::get<PixelViewer>(*opened);
        CHECK(co_await surface->keyframes->pop());
        auto input = viewer.input();
        const cs::Input event = cs::input::Text{.text = "before close"};
        auto seq = input.send(event);
        REQUIRE(seq);
        CHECK(*seq == 1);
        viewer.close();
        auto received = co_await surface->inputs->pop();
        REQUIRE(received);
        CHECK(received->input == event);
        co_await until([&] { return server->has_value(); });
        CHECK(server->value().has_value());
    });
}
