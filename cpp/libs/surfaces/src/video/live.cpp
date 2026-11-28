// Port of `loa-surfaces/src/proto/video/mod.rs`.

#include "loa/surfaces/video/live.hpp"

#include <asio/co_spawn.hpp>
#include <asio/detached.hpp>
#include <asio/use_future.hpp>

#include <algorithm>
#include <atomic>
#include <format>
#include <future>
#include <mutex>
#include <system_error>

#include "loa/contracts/surface_channel.hpp"
#include "loa/runtime/log.hpp"
#include "loa/surfaces/video/decode.hpp"

namespace loa::surfaces::video {

namespace log = loa::runtime::log;

namespace {

constexpr std::string_view TARGET = "loa_surfaces::proto::video";

cs::StreamInfo source_stream(Encoding encoding) { return {.encoding = encoding, .encoder = std::string(SOURCE), .hardware = false}; }

asio::awaitable<void, asio::any_io_executor> ask_keyframe(std::shared_ptr<cs::Surface> source) {
    if (auto result = co_await source->request_keyframe(); !result)
        log::debug(TARGET, "asking the source surface for a keyframe: " + result.error().message());
}

asio::awaitable<cs::Result<std::shared_ptr<const Frame>>, asio::any_io_executor> recv_one(std::shared_ptr<cs::FrameReceiver> source) {
    co_return co_await source->recv();
}

} // namespace

LiveVideo LiveVideo::probe(std::optional<FfmpegConfig> ffmpeg) {
    LiveVideo live;
    live.probed_ = video::probe(ffmpeg ? &*ffmpeg : nullptr);
    live.ffmpeg_ = std::move(ffmpeg);
    return live;
}

LiveVideo LiveVideo::with_probed(std::optional<FfmpegConfig> ffmpeg, std::vector<ProbeResult> probed) {
    LiveVideo live;
    live.ffmpeg_ = std::move(ffmpeg);
    live.probed_ = std::move(probed);
    return live;
}

std::optional<EncoderInfo> LiveVideo::choose(std::span<const Encoding> accepted) const {
    for (const Encoding e : accepted) {
        for (const Codec codec : {Codec::H264, Codec::Hevc}) {
            if (encoding(codec) != e) continue;
            const auto found = std::ranges::find_if(
                probed_, [&](const ProbeResult& p) { return p.works() && is_hardware(p.info.accel) && p.info.codec == codec; });
            if (found != probed_.end()) return found->info;
        }
    }
    if (std::ranges::contains(accepted, Encoding::H264)) return software_info();
    return std::nullopt;
}

Result<LiveEncoder> LiveVideo::open(const EncoderInfo& info, std::pair<std::uint32_t, std::uint32_t> size) const {
    std::unique_ptr<VideoEncoder> encoder;
    if (info.name.starts_with(NAME_PREFIX)) {
        const std::string_view name = std::string_view(info.name).substr(NAME_PREFIX.size());
        if (!ffmpeg_) return std::unexpected(video_error(std::format("{} needs an ffmpeg binary", name)));
        auto spawned = FfmpegEncoder::spawn(*ffmpeg_, name, size);
        if (!spawned) return std::unexpected(spawned.error());
        encoder = std::move(*spawned);
    } else {
        auto software = SoftwareH264::make(size.first, size.second);
        if (!software) return std::unexpected(software.error());
        encoder = std::move(*software);
    }
    return LiveEncoder(size, info.codec, std::move(encoder));
}

cs::StreamInfo stream_info(const EncoderInfo& info) {
    std::string_view name = info.name;
    while (name.starts_with(NAME_PREFIX)) name.remove_prefix(NAME_PREFIX.size());
    return {.encoding = encoding(info.codec), .encoder = std::string(name), .hardware = is_hardware(info.accel)};
}

struct VideoSurface::Shared : std::enable_shared_from_this<Shared> {
    // Where a warming encoder lands (Rust `mpsc` channel of one value).
    struct Slot {
        std::mutex mutex;
        std::optional<Result<LiveEncoder>> value;
        // The receiving side is gone: nobody waits.
        bool abandoned = false;
    };
    struct Warming {
        EncoderInfo info;
        std::pair<std::uint32_t, std::uint32_t> size;
        std::shared_ptr<Slot> ready;
    };

    std::shared_ptr<const LiveVideo> live;
    // Keyframe requests to it run on `executor`.
    std::weak_ptr<cs::Surface> source;
    asio::any_io_executor executor;
    std::mutex mutex;
    // The negotiated encoder; `std::nullopt` passes source frames on.
    std::optional<EncoderInfo> wanted;
    std::optional<cs::StreamInfo> stream;
    // What the last viewer to negotiate decodes.
    std::vector<Encoding> accepted;
    std::atomic<bool> keyframe{true};
    std::atomic<bool> enabled{false};
    std::atomic<bool> stop{false};
    std::mutex warming_mutex;
    std::optional<Warming> warming;

    std::optional<EncoderInfo> wanted_now() {
        std::lock_guard lock(mutex);
        return wanted;
    }
    void set_stream(cs::StreamInfo info) {
        std::lock_guard lock(mutex);
        stream = std::move(info);
    }
    bool accepts_h264() {
        std::lock_guard lock(mutex);
        return std::ranges::contains(accepted, Encoding::H264);
    }

    // Asks the source for its current picture.
    void source_keyframe() {
        if (stop.load(std::memory_order_acquire)) return;
        if (auto s = source.lock()) asio::co_spawn(executor, ask_keyframe(std::move(s)), asio::detached);
    }

    // Starts `info` for `size` on its own thread (one size at a time), then
    // asks the source for its picture so the encoder takes over at once.
    void warm(const EncoderInfo& info, std::pair<std::uint32_t, std::uint32_t> size) {
        std::lock_guard lock(warming_mutex);
        if (warming && warming->info == info && warming->size == size) return;
        auto slot = std::make_shared<Slot>();
        try {
            std::thread([self = shared_from_this(), slot, info, size] {
                std::optional<Result<LiveEncoder>> primed;
                try {
                    // Device setup happens on the first picture (~100 ms with VA-API): code a blank one.
                    auto encoder = self->live->open(info, size);
                    if (encoder) {
                        std::vector<std::uint8_t> blank(static_cast<std::size_t>(size.first) * size.second * 4);
                        if (auto coded = encoder->encode(blank, 0, 0); !coded) {
                            primed.emplace(std::unexpected(coded.error()));
                        } else {
                            encoder->request_keyframe();
                            primed.emplace(std::move(*encoder));
                        }
                    } else {
                        primed.emplace(std::unexpected(encoder.error()));
                    }
                } catch (...) {
                    primed.emplace(std::unexpected(video_error("the encoder opening thread died")));
                }
                // The worker may have moved on to another size; then nobody waits.
                bool delivered = false;
                {
                    std::lock_guard slot_lock(slot->mutex);
                    if (!slot->abandoned) {
                        slot->value = std::move(primed);
                        delivered = true;
                    }
                }
                if (delivered) self->source_keyframe();
            }).detach();
        } catch (const std::system_error& error) {
            log::warn(TARGET, std::format("starting the video encoder opening thread: {}", error.what()));
            return;
        }
        if (warming) abandon(*warming);
        warming = Warming{info, size, std::move(slot)};
    }

    static void abandon(Warming& w) {
        std::lock_guard lock(w.ready->mutex);
        w.ready->abandoned = true;
    }

    // The encoder `warm` started for `info` at `size`, once it is up (or failed).
    std::optional<Result<LiveEncoder>> warmed(const EncoderInfo& info, std::pair<std::uint32_t, std::uint32_t> size) {
        std::lock_guard lock(warming_mutex);
        if (!warming || warming->info != info || warming->size != size) return std::nullopt;
        std::optional<Result<LiveEncoder>> result;
        {
            std::lock_guard slot_lock(warming->ready->mutex);
            if (!warming->ready->value) return std::nullopt;
            result = std::move(warming->ready->value);
            warming->ready->abandoned = true;
        }
        warming.reset();
        return result;
    }

    void abandon_warming() {
        std::lock_guard lock(warming_mutex);
        if (warming) abandon(*warming);
        warming.reset();
    }
};

namespace {

class Worker {
  public:
    Worker(std::shared_ptr<VideoSurface::Shared> shared, std::shared_ptr<cs::FrameSender> out)
        : shared_(std::move(shared)), out_(std::move(out)) {}

    void run(const std::shared_ptr<cs::FrameReceiver>& source) {
        for (;;) {
            if (stopped()) return;
            auto next = asio::co_spawn(shared_->executor, recv_one(source), asio::use_future);
            while (next.wait_for(std::chrono::milliseconds(50)) != std::future_status::ready)
                if (stopped()) return;
            cs::Result<std::shared_ptr<const Frame>> first;
            try {
                first = next.get();
            } catch (...) {
                return; // the executor went away
            }
            if (!first) return; // closed
            if (source->take_lagged() > 0) shared_->source_keyframe();
            if (stopped()) return;
            // Everything already queued goes into this one picture.
            std::vector<std::shared_ptr<const Frame>> batch{std::move(*first)};
            while (auto frame = source->try_recv()) batch.push_back(std::move(frame));
            if (source->take_lagged() > 0) shared_->source_keyframe();
            handle(batch);
        }
    }

  private:
    [[nodiscard]] bool stopped() const { return shared_->stop.load(std::memory_order_acquire); }

    void handle(const std::vector<std::shared_ptr<const Frame>>& batch) {
        if (batch.empty()) return;
        const Frame& last = *batch.back();
        // Nobody watches: no encoder runs. A new subscriber asks for a keyframe.
        if (out_->receiver_count() == 0) {
            encoder_.reset();
            return;
        }
        // 4:2:0 video needs an even size; odd surfaces go as they come.
        const bool even = last.width % 2 == 0 && last.height % 2 == 0 && last.width > 0;
        auto wanted = shared_->wanted_now();
        if (!wanted || !even) {
            encoder_.reset();
            for (const auto& frame : batch) pass_on(*frame);
            return;
        }
        // Video codes the whole picture: rebuild it from the source frames.
        for (const auto& frame : batch) {
            if (auto applied = decoder_.apply(*frame, canvas_); !applied) {
                log::debug(TARGET, "source frame: " + applied.error().message());
                shared_->source_keyframe();
            }
        }
        encode(*wanted, last);
    }

    void send(Frame frame) {
        frame.seq = ++seq_;
        out_->send(std::make_shared<const Frame>(std::move(frame)));
    }

    void pass_on(const Frame& frame) {
        shared_->set_stream(source_stream(frame.encoding));
        send(frame);
    }

    void encode(const EncoderInfo& wanted, const Frame& last) {
        const std::pair size{canvas_.width, canvas_.height};
        if (!(encoder_ && encoder_->first == wanted && encoder_->second.size() == size)) {
            auto switched = switch_to(wanted, size);
            if (!switched) return fall_back(wanted, switched.error().message(), last);
            if (!*switched) {
                // No encoder this viewer decodes is up yet.
                encoder_.reset();
                return pass_on(last);
            }
        }
        if (!encoder_) return;
        if (shared_->keyframe.exchange(false, std::memory_order_acq_rel)) encoder_->second.request_keyframe();
        auto coded = encoder_->second.encode(canvas_.data, last.input_seq, last.captured_us);
        if (!coded) {
            const EncoderInfo info = encoder_->first;
            encoder_.reset();
            return fall_back(info, coded.error().message(), last);
        }
        if (*coded) send(std::move(**coded));
    }

    // Puts `wanted` in place for `size` once up, bridging with software
    // H.264 meanwhile. `false` when the viewer decodes no H.264 and `wanted`
    // isn't up yet.
    Result<bool> switch_to(const EncoderInfo& wanted, std::pair<std::uint32_t, std::uint32_t> size) {
        std::optional<LiveEncoder> opened;
        if (auto warmed = shared_->warmed(wanted, size)) {
            if (!*warmed) return std::unexpected(warmed->error());
            opened.emplace(std::move(**warmed));
        } else if (!is_hardware(wanted.accel)) {
            auto encoder = shared_->live->open(wanted, size);
            if (!encoder) return std::unexpected(encoder.error());
            opened.emplace(std::move(*encoder));
        }
        if (opened) {
            use_encoder(wanted, std::move(*opened));
            return true;
        }
        shared_->warm(wanted, size);
        const EncoderInfo bridge = software_info();
        if (encoder_ && encoder_->first == bridge && encoder_->second.size() == size) return true;
        if (!shared_->accepts_h264()) return false;
        auto encoder = shared_->live->open(bridge, size);
        if (!encoder) return std::unexpected(encoder.error());
        use_encoder(bridge, std::move(*encoder));
        return true;
    }

    void use_encoder(EncoderInfo info, LiveEncoder encoder) {
        shared_->set_stream(stream_info(info));
        encoder_.emplace(std::move(info), std::move(encoder));
    }

    // `failed` can't code: hardware falls back to software H.264, software to the source frames.
    void fall_back(const EncoderInfo& failed, const std::string& error, const Frame& last) {
        log::warn(TARGET, std::format("live video encoder failed: encoder={} error={}", failed.name, error));
        std::optional<EncoderInfo> next;
        if (is_hardware(failed.accel) && failed.codec == Codec::H264) next = software_info();
        {
            std::lock_guard lock(shared_->mutex);
            shared_->wanted = next;
        }
        shared_->keyframe.store(true, std::memory_order_release);
        if (next) return encode(*next, last);
        // The viewer needs a whole picture in the new encoding.
        shared_->source_keyframe();
        pass_on(last);
    }

    std::shared_ptr<VideoSurface::Shared> shared_;
    std::shared_ptr<cs::FrameSender> out_;
    FrameDecoder decoder_;
    // The source picture, rebuilt from its frames.
    RgbaImage canvas_;
    std::optional<std::pair<EncoderInfo, LiveEncoder>> encoder_;
    std::uint64_t seq_ = 0;
};

} // namespace

cs::Result<std::shared_ptr<VideoSurface>> VideoSurface::make(std::shared_ptr<cs::Surface> inner, std::shared_ptr<const LiveVideo> live,
                                                             bool enabled, const asio::any_io_executor& executor) {
    auto source = inner->frames();
    if (!source) return std::unexpected(source.error());
    auto shared = std::make_shared<Shared>();
    shared->live = std::move(live);
    shared->source = inner;
    shared->executor = executor;
    shared->enabled.store(enabled);
    std::shared_ptr<VideoSurface> surface(new VideoSurface());
    surface->inner_ = std::move(inner);
    surface->shared_ = shared;
    surface->frames_ = std::make_shared<cs::FrameSender>(executor, 8);
    try {
        surface->worker_ = std::thread([worker = Worker(shared, surface->frames_), source = std::move(*source)]() mutable {
            worker.run(source);
        });
    } catch (const std::system_error& e) {
        return std::unexpected(cs::SurfaceError::backend(std::format("starting the video encoder thread: {}", e.what())));
    }
    return surface;
}

VideoSurface::~VideoSurface() {
    if (shared_) {
        shared_->stop.store(true, std::memory_order_release);
        shared_->abandon_warming();
    }
    if (worker_.joinable()) {
        if (worker_.get_id() == std::this_thread::get_id()) worker_.detach();
        else worker_.join();
    }
}

contracts::Awaitable<cs::Result<std::optional<cs::StreamInfo>>> VideoSurface::set_video(bool enabled) {
    shared_->enabled.store(enabled, std::memory_order_release);
    std::vector<Encoding> accepted;
    {
        std::lock_guard lock(shared_->mutex);
        accepted = shared_->accepted;
    }
    if (accepted.empty()) co_return stream();
    co_return co_await negotiate(std::move(accepted));
}

cs::SurfaceInfo VideoSurface::info() const { return inner_->info(); }

cs::Result<std::shared_ptr<cs::FrameReceiver>> VideoSurface::frames() {
    auto frames = frames_->subscribe();
    shared_->keyframe.store(true, std::memory_order_release);
    shared_->source_keyframe();
    return frames;
}

contracts::Awaitable<cs::Result<void>> VideoSurface::request_keyframe() {
    shared_->keyframe.store(true, std::memory_order_release);
    // The worker codes on source frames: the source must resend its picture.
    co_return co_await inner_->request_keyframe();
}

contracts::Awaitable<cs::Result<void>> VideoSurface::input(cs::InputEvent event) {
    if (const auto* resize = std::get_if<cs::input::Resize>(&event.input);
        resize && resize->width % 2 == 0 && resize->height % 2 == 0 && frames_->receiver_count() > 0) {
        if (auto wanted = shared_->wanted_now(); wanted && is_hardware(wanted->accel)) shared_->warm(*wanted, {resize->width, resize->height});
    }
    co_return co_await inner_->input(std::move(event));
}

contracts::Awaitable<cs::Result<cs::Document>> VideoSurface::document() { co_return co_await inner_->document(); }

contracts::Awaitable<cs::Result<cs::Bytes>> VideoSurface::screenshot() { co_return co_await inner_->screenshot(); }

contracts::Awaitable<cs::Result<std::optional<cs::StreamInfo>>> VideoSurface::negotiate(std::vector<Encoding> accepted) {
    std::optional<EncoderInfo> chosen;
    if (shared_->enabled.load(std::memory_order_acquire)) chosen = shared_->live->choose(accepted);
    cs::StreamInfo stream;
    if (chosen) {
        stream = stream_info(*chosen);
    } else {
        std::optional<Encoding> encoding;
        if (auto inner = inner_->stream()) encoding = inner->encoding;
        if (!encoding) {
            std::lock_guard lock(shared_->mutex);
            if (shared_->stream && shared_->stream->encoder == SOURCE) encoding = shared_->stream->encoding;
        }
        stream = source_stream(encoding.value_or(Encoding::Rgba));
    }
    {
        std::lock_guard lock(shared_->mutex);
        shared_->accepted = std::move(accepted);
        shared_->wanted = std::move(chosen);
        shared_->stream = stream;
    }
    if (auto requested = co_await request_keyframe(); !requested) co_return std::unexpected(requested.error());
    co_return std::optional<cs::StreamInfo>(std::move(stream));
}

std::optional<cs::StreamInfo> VideoSurface::stream() const {
    std::lock_guard lock(shared_->mutex);
    return shared_->stream;
}

} // namespace loa::surfaces::video
