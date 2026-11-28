#include "loa/surfaces/hub/video.hpp"

#include <asio.hpp>

#include "loa/runtime/log.hpp"
#include "loa/surfaces/hub/service.hpp"
#include "off_thread.hpp"

namespace loa::surfaces::hub {

VideoConfig VideoConfig::discover() { return VideoConfig{true, video::FfmpegConfig::discover(), nullptr}; }

RemoteVideo::RemoteVideo(const VideoConfig& config)
    : ffmpeg_(config.ffmpeg), encoders_(config.encoders), enabled_(config.enabled) {}

loa::contracts::Awaitable<std::shared_ptr<const video::LiveVideo>> RemoteVideo::encoders() {
    {
        std::lock_guard lock(mutex_);
        if (encoders_) co_return encoders_;
    }
    auto probed = co_await detail::off_thread<std::shared_ptr<const video::LiveVideo>>(
        [ffmpeg = ffmpeg_] { return std::make_shared<const video::LiveVideo>(video::LiveVideo::probe(ffmpeg)); });
    std::lock_guard lock(mutex_);
    if (!encoders_) encoders_ = std::move(probed);
    co_return encoders_;
}

loa::contracts::Awaitable<void> RemoteVideo::set_enabled(bool enabled) {
    enabled_.store(enabled, std::memory_order_release);
    std::vector<std::shared_ptr<video::VideoSurface>> videos;
    {
        std::lock_guard lock(mutex_);
        for (const auto& [_, video] : videos_) videos.push_back(video);
    }
    for (const auto& video : videos) {
        if (auto switched = co_await video->set_video(enabled); !switched) {
            runtime::log::warn("loa_surfaces::hub", "switching a surface's video: " + switched.error().message());
        }
    }
}

std::shared_ptr<video::VideoSurface> RemoteVideo::coded(const cs::SurfaceUri& uri) const {
    for (const auto& [open, video] : videos_) {
        if (open == uri) return video;
    }
    return nullptr;
}

loa::contracts::Awaitable<Result<std::shared_ptr<cs::Surface>>> RemoteVideo::surface(Opened opened) {
    const bool live = (opened.origin.kind() == Origin::Kind::Browser || opened.origin.kind() == Origin::Kind::Vm) &&
                      opened.info.mode == cs::SurfaceMode::Pixel;
    if (!live) co_return std::move(opened.surface);
    {
        std::lock_guard lock(mutex_);
        if (auto video = coded(opened.info.uri)) co_return std::shared_ptr<cs::Surface>(std::move(video));
    }
    auto encoders = co_await this->encoders();
    const auto executor = co_await asio::this_coro::executor;
    std::lock_guard lock(mutex_);
    if (auto video = coded(opened.info.uri)) co_return std::shared_ptr<cs::Surface>(std::move(video));
    auto video = video::VideoSurface::make(std::move(opened.surface), std::move(encoders), enabled(), executor);
    if (!video) co_return std::unexpected(HubError::surface(std::move(video.error())));
    videos_.emplace_back(opened.info.uri, *video);
    co_return std::shared_ptr<cs::Surface>(std::move(*video));
}

void RemoteVideo::forget(const cs::SurfaceUri* uri) {
    std::lock_guard lock(mutex_);
    std::erase_if(videos_, [&](const auto& entry) { return !uri || entry.first == *uri; });
}

} // namespace loa::surfaces::hub
