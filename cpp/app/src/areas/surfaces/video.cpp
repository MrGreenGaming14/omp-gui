// Live video for peers (port of `loa-app/src/areas/surfaces/video.rs`): the encoder status the
// launcher shows. The toggle, saved in `[surfaces] video`, is in `surfaces.cpp`.

#include <array>

#include "surfaces.hpp"

namespace loa::app::areas::surfaces {

loa::view::surface::VideoStatus video_status(bool enabled, const LiveVideo& encoders) {
    constexpr std::string_view FFMPEG_PREFIX = "ffmpeg ";
    static constexpr std::array ACCEPTED{loa::view::surface::Encoding::H264};
    loa::view::surface::VideoStatus status{enabled, std::nullopt, {}};
    if (auto info = encoders.choose(ACCEPTED)) status.encoder = loa::surfaces::video::stream_info(*info);
    for (const auto& probe : encoders.encoders()) {
        if (!probe.works() || !loa::surfaces::video::is_hardware(probe.info.accel)) continue;
        std::string_view name = probe.info.name;
        if (name.starts_with(FFMPEG_PREFIX)) name.remove_prefix(FFMPEG_PREFIX.size());
        status.hardware.emplace_back(name);
    }
    return status;
}

} // namespace loa::app::areas::surfaces
