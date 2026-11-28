// Live video: Annex B parsing, size checks, the ffmpeg encoder table and
// arguments, openh264 round trips, and the video session test of
// `loa-surfaces/tests/session.rs`.

#include <doctest/doctest.h>

#include <asio.hpp>

#include <atomic>
#include <chrono>
#include <functional>

#include "loa/contracts/surface_channel.hpp"
#include "loa/surfaces/proto/server.hpp"
#include "loa/surfaces/proto/viewer.hpp"
#include "loa/surfaces/testkit.hpp"
#include "loa/surfaces/video/decode.hpp"
#include "loa/surfaces/video/live.hpp"

using namespace loa::surfaces::video;
namespace proto = loa::surfaces::proto;
namespace cs = loa::contracts::surface;

namespace {

using V = std::vector<std::uint8_t>;

const std::vector<cs::Encoding> ALL{cs::Encoding::Rgba, cs::Encoding::Jpeg, cs::Encoding::Png, cs::Encoding::H264, cs::Encoding::Hevc};

cs::Frame frame(std::uint64_t seq, cs::Encoding encoding, bool keyframe, std::size_t len) {
    cs::Bytes data(len);
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

void run(std::function<asio::awaitable<void>(asio::any_io_executor)> test) {
    asio::io_context io;
    bool finished = false;
    asio::co_spawn(io, [&]() -> asio::awaitable<void> {
        co_await test(io.get_executor());
        finished = true;
        io.stop();
    }, [](std::exception_ptr e) { if (e) std::rethrow_exception(e); });
    io.run_for(std::chrono::seconds(30));
    REQUIRE(finished);
}

} // namespace

TEST_CASE("video: annex b parsing") {
    const V au{0, 0, 0, 1, 0x67, 0x42, 0, 0, 1, 0x68, 0xce, 0, 0, 0, 1, 0x65, 0x88, 0x84};
    CHECK(nal_types(Codec::H264, au) == V{7, 8, 5});
    CHECK((is_keyframe(Codec::H264, au) && has_parameter_sets(Codec::H264, au)));
    const V p_slice{0, 0, 0, 1, 0x41, 0x9a, 0x02};
    CHECK((!is_keyframe(Codec::H264, p_slice) && has_picture(Codec::H264, p_slice)));
    CHECK(is_keyframe(Codec::Hevc, V{0, 0, 0, 1, 0x40, 0x01, 0x0c, 0, 0, 0, 1, 0x26, 0x01, 0xaf}));
    CHECK(!is_keyframe(Codec::Hevc, V{0, 0, 0, 1, 0x02, 0x01, 0xd0}));
    const V aud_split{0, 0, 0, 1, 9, 0xf0, 0, 0, 1, 0x65, 1, 0, 0, 0, 1, 9, 0xf0};
    CHECK(next_access_unit(Codec::H264, aud_split) == std::optional<std::size_t>(11));
    CHECK(next_access_unit(Codec::H264, V{0, 0, 1, 9, 0xf0}) == std::nullopt);
}

TEST_CASE("video: size and picture checks") {
    CHECK(check_video_size(2, 2));
    CHECK(check_video_size(8192, 8192));
    for (auto [w, h] : {std::pair{0u, 2u}, {2u, 0u}, {3u, 2u}, {2u, 3u}, {8194u, 2u}, {2u, 8194u}}) {
        CAPTURE(w);
        CAPTURE(h);
        auto r = check_video_size(w, h);
        REQUIRE(!r);
        CHECK(r.error().message() == std::format("video: {}x{}: 4:2:0 video needs an even, nonzero size up to 8192", w, h));
    }
    CHECK(check_picture(V(4 * 3 * 4), 4, 3));
    auto r = check_picture(V(5), 1, 1);
    REQUIRE(!r);
    CHECK(r.error().message() == "video: 5 bytes for a 1x1 picture");
    CHECK(!check_picture(V(), 1, 1));
}

TEST_CASE("video: ffmpeg encoder table and arguments") {
    const std::vector<std::string_view> order{"h264_vaapi", "hevc_vaapi", "h264_nvenc", "hevc_nvenc",
                                              "h264_qsv",   "hevc_qsv",   "h264_v4l2m2m", "hevc_v4l2m2m"};
    for (std::size_t i = 0; i < KNOWN.size(); ++i) {
        CHECK(KNOWN[i].name == order[i]);
        CHECK(KNOWN[i].codec == (i % 2 == 0 ? Codec::H264 : Codec::Hevc));
    }
    CHECK(KNOWN[0].accel == Accel::Vaapi);
    CHECK(KNOWN[2].accel == Accel::Nvenc);
    CHECK(KNOWN[4].accel == Accel::Qsv);
    CHECK(KNOWN[6].accel == Accel::V4l2M2m);

    const std::vector<std::string> head{"-hide_banner", "-loglevel", "error", "-nostdin", "-f", "rawvideo", "-pixel_format", "rgba",
                                        "-video_size", "640x480", "-framerate", "60", "-i", "pipe:0"};
    auto expect = [&](std::vector<std::string> middle, std::string codec, std::string format) {
        auto args = head;
        args.insert(args.end(), middle.begin(), middle.end());
        args.insert(args.end(), {"-an", "-c:v", codec, "-g", "600", "-bf", "0", "-q:v", "26", "-flags", "+low_delay", "-bsf:v",
                                 format + "_metadata=aud=insert", "-flush_packets", "1", "-f", format, "pipe:1"});
        return args;
    };
    CHECK(start_args("h264_vaapi", Codec::H264, Accel::Vaapi, 640, 480, std::filesystem::path("/dev/dri/renderD128")) ==
          expect({"-vaapi_device", "/dev/dri/renderD128", "-vf", "format=nv12,hwupload", "-async_depth", "1", "-fps_mode",
                  "passthrough", "-profile:v", "constrained_baseline"},
                 "h264_vaapi", "h264"));
    CHECK(start_args("hevc_vaapi", Codec::Hevc, Accel::Vaapi, 640, 480, std::filesystem::path("/dev/dri/renderD129")) ==
          expect({"-vaapi_device", "/dev/dri/renderD129", "-vf", "format=nv12,hwupload", "-async_depth", "1", "-fps_mode", "passthrough"},
                 "hevc_vaapi", "hevc"));
    CHECK(start_args("h264_nvenc", Codec::H264, Accel::Nvenc, 640, 480, std::nullopt) ==
          expect({"-delay", "0", "-zerolatency", "1", "-fps_mode", "passthrough", "-profile:v", "baseline"}, "h264_nvenc", "h264"));
    CHECK(start_args("hevc_nvenc", Codec::Hevc, Accel::Nvenc, 640, 480, std::nullopt) ==
          expect({"-delay", "0", "-zerolatency", "1", "-fps_mode", "passthrough"}, "hevc_nvenc", "hevc"));
    CHECK(start_args("h264_qsv", Codec::H264, Accel::Qsv, 640, 480, std::nullopt) ==
          expect({"-vf", "format=nv12", "-async_depth", "1", "-fps_mode", "passthrough", "-profile:v", "baseline"}, "h264_qsv", "h264"));
    CHECK(start_args("hevc_qsv", Codec::Hevc, Accel::Qsv, 640, 480, std::nullopt) ==
          expect({"-vf", "format=nv12", "-async_depth", "1", "-fps_mode", "passthrough"}, "hevc_qsv", "hevc"));
    CHECK(start_args("h264_v4l2m2m", Codec::H264, Accel::V4l2M2m, 640, 480, std::nullopt) ==
          expect({"-vf", "format=yuv420p", "-fps_mode", "passthrough"}, "h264_v4l2m2m", "h264"));
    CHECK(start_args("hevc_v4l2m2m", Codec::Hevc, Accel::V4l2M2m, 640, 480, std::nullopt) ==
          expect({"-vf", "format=yuv420p", "-fps_mode", "passthrough"}, "hevc_v4l2m2m", "hevc"));

    auto unknown = FfmpegEncoder::spawn(FfmpegConfig{"/nonexistent/ffmpeg"}, "libx264", {64, 64});
    REQUIRE(!unknown);
    CHECK(unknown.error().message() == "video: unknown ffmpeg encoder libx264");
}

TEST_CASE("video: probing lists KNOWN in order, software last") {
    const auto software_only = probe(nullptr);
    REQUIRE(software_only.size() == 1);
    CHECK(software_only[0].info == software_info());
    CHECK(software_only[0].works());
    const FfmpegConfig missing{"/nonexistent/ffmpeg"};
    const auto all = probe(&missing);
    REQUIRE(all.size() == KNOWN.size() + 1);
    for (std::size_t i = 0; i < KNOWN.size(); ++i) {
        CHECK(all[i].info.name == "ffmpeg " + std::string(KNOWN[i].name));
        CHECK(!all[i].works());
    }
    CHECK(all.back().info.name == "rusty_h264");
    const auto live = LiveVideo::with_probed(std::nullopt, software_only);
    CHECK(live.choose(ALL) == software_info());
    CHECK(live.choose(std::vector{cs::Encoding::Hevc}) == std::nullopt);
    CHECK(stream_info(EncoderInfo{"ffmpeg h264_vaapi", Codec::H264, Accel::Vaapi}) == cs::StreamInfo{cs::Encoding::H264, "h264_vaapi", true});
}

TEST_CASE("video: an openh264 keyframe decodes back to the picture") {
    constexpr std::uint32_t W = 64, H = 32;
    auto encoder = SoftwareH264::make(W, H);
    REQUIRE(encoder);
    CHECK(!SoftwareH264::make(63, 32));
    V rgba(W * H * 4);
    for (std::uint32_t y = 0; y < H; ++y)
        for (std::uint32_t x = 0; x < W; ++x) {
            auto* p = &rgba[(y * W + x) * 4];
            p[0] = x < W / 2 ? 200 : 30;
            p[1] = y < H / 2 ? 180 : 60;
            p[2] = 90;
            p[3] = 255;
        }
    auto unit = (*encoder)->encode(rgba, true);
    REQUIRE(unit);
    CHECK(unit->keyframe);
    CHECK(has_parameter_sets(Codec::H264, unit->data));
    cs::Frame f{.width = W, .height = H, .region = {0, 0, W, H}, .encoding = cs::Encoding::H264, .keyframe = true, .data = unit->data};
    VideoDecoder decoder;
    CHECK(decoder.supports(cs::Encoding::H264));
    CHECK(!decoder.supports(cs::Encoding::Hevc));
    V out(W * H * 4);
    auto decoded = decoder.decode(f, out);
    REQUIRE(decoded);
    CHECK(*decoded);
    int worst = 0;
    for (std::size_t i = 0; i < out.size(); ++i) worst = std::max(worst, std::abs(int(out[i]) - int(rgba[i])));
    CHECK(worst < 64);
    auto second = (*encoder)->encode(rgba, false);
    REQUIRE(second);
    CHECK(!second->keyframe);

    V small(4);
    auto wrong = decoder.decode(f, small);
    REQUIRE(!wrong);
    CHECK(wrong.error().message() == "video: video frames must cover the whole picture");
    f.encoding = cs::Encoding::Hevc;
    CHECK(decoder.decode(f, out).error().message() == "video: no decoder for Hevc frames");

    FrameDecoder frames;
    loa::surfaces::RgbaImage canvas;
    cs::Frame bad{.width = 4, .height = 4, .region = {0, 0, 0, 4}};
    CHECK(frames.apply(bad, canvas).error().message() == "video: frame region is outside its picture");
}

TEST_CASE("a viewer that decodes h264 gets video and others get source frames") {
    struct Case {
        std::vector<cs::Encoding> accepted;
        cs::Encoding encoding;
        std::string encoder;
    };
    const std::vector<Case> cases{{ALL, cs::Encoding::H264, "rusty_h264"},
                                  {{ALL.begin(), ALL.begin() + 3}, cs::Encoding::Rgba, std::string(SOURCE)}};
    for (const auto& c : cases) {
        CAPTURE(c.encoder);
        run([&c](asio::any_io_executor ex) -> asio::awaitable<void> {
            cs::SurfaceInfo info{.uri = *cs::SurfaceUri::make("test", "live"), .name = "live", .width = 64, .height = 32,
                                 .mode = cs::SurfaceMode::Pixel, .mime = std::nullopt};
            auto [source, src] = loa::surfaces::testkit::FakeSurface::make(ex, info, std::nullopt);
            auto live = std::make_shared<const LiveVideo>(LiveVideo::with_probed(std::nullopt, probe(nullptr)));
            auto video = VideoSurface::make(source, live, true, ex);
            REQUIRE(video);
            auto [server_end, viewer_end] = proto::memory_pair(ex, std::size_t{1200});
            auto served = std::make_shared<bool>(false);
            asio::co_spawn(ex, proto::serve(*video, std::move(server_end)), [served](std::exception_ptr, auto) { *served = true; });
            auto opened = co_await proto::open(std::move(viewer_end), info.uri, proto::ViewerOptions(c.accepted));
            REQUIRE(opened);
            REQUIRE(std::holds_alternative<proto::PixelViewer>(*opened));
            auto& viewer = std::get<proto::PixelViewer>(*opened);
            REQUIRE(viewer.stream());
            CHECK(viewer.stream()->encoder == c.encoder);
            // The worker codes on source frames; send until the viewer shows one.
            const auto picture = std::make_shared<const cs::Frame>(frame(1, cs::Encoding::Rgba, true, 64 * 32 * 4));
            auto shown_flag = std::make_shared<std::atomic<bool>>(false);
            auto sending = std::make_shared<bool>(true);
            asio::co_spawn(ex, [frames = src.frames, picture, shown_flag]() -> asio::awaitable<void> {
                asio::steady_timer timer(co_await asio::this_coro::executor);
                while (!shown_flag->load()) {
                    frames->send(picture);
                    timer.expires_after(std::chrono::milliseconds(200));
                    co_await timer.async_wait(asio::use_awaitable);
                }
            }, [sending](std::exception_ptr) { *sending = false; });
            std::optional<cs::Frame> shown;
            while (!shown) {
                auto event = co_await viewer.next_event();
                REQUIRE(event);
                if (auto* f = std::get_if<cs::Frame>(&*event)) shown = std::move(*f);
            }
            shown_flag->store(true);
            CHECK(shown->encoding == c.encoding);
            if (c.encoding == cs::Encoding::H264) {
                CHECK(shown->keyframe);
                V out(64 * 32 * 4);
                auto decoded = VideoDecoder().decode(*shown, out);
                REQUIRE(decoded);
                CHECK(*decoded);
            }
            viewer.close();
            // The session ends before the context goes away.
            asio::steady_timer timer(ex);
            while (!*served || *sending) {
                timer.expires_after(std::chrono::milliseconds(5));
                co_await timer.async_wait(asio::use_awaitable);
            }
            video->reset();
        });
    }
}

TEST_CASE("video: probing a real ffmpeg reports every KNOWN encoder") {
    const auto ffmpeg = FfmpegConfig::discover();
    if (!ffmpeg) return;
    const auto found = probe(*ffmpeg);
    REQUIRE(found.size() == KNOWN.size());
    for (std::size_t i = 0; i < KNOWN.size(); ++i) {
        CHECK(found[i].info.name == "ffmpeg " + std::string(KNOWN[i].name));
        if (found[i].error) MESSAGE(found[i].info.name << ": " << *found[i].error);
    }
}
