#include "loa/app/view/surface.hpp"

#include <string>
#include <utility>

namespace loa::app::view {

namespace shown = ::loa::view::surface;
using ::loa::surfaces::CaptureMode;

shown::Capture capture_to_view(CaptureMode capture) noexcept {
    switch (capture) {
    case CaptureMode::Screencast: return shown::Capture::Screencast;
    case CaptureMode::VirtualDisplay: return shown::Capture::VirtualDisplay;
    }
    return shown::Capture::Screencast;
}

CaptureMode capture_from_view(shown::Capture capture) noexcept {
    switch (capture) {
    case shown::Capture::Screencast: return CaptureMode::Screencast;
    case shown::Capture::VirtualDisplay: return CaptureMode::VirtualDisplay;
    }
    return CaptureMode::Screencast;
}

shown::BrowserStatus browser_status(const ::loa::surfaces::hub::SurfaceHub& hub) {
    if (auto executable = hub.browser_executable()) {
        return shown::browser_status::Ready{std::move(*executable), hub.browser_sandboxed()};
    }
    return shown::browser_status::Missing{};
}

shown::CaptureStatus capture_status(const ::loa::surfaces::hub::SurfaceHub& hub, bool pending) {
    const auto& virtual_display = hub.config().virtual_display;
    return shown::CaptureStatus{
        .selected = capture_to_view(hub.capture()),
        .virtual_display_missing = virtual_display ? std::nullopt : std::optional<std::string>(virtual_display.error()),
        .pending = pending,
    };
}

shown::AvailableSurface available(::loa::surfaces::hub::Listed listed) {
    return shown::AvailableSurface{.info = std::move(listed.info), .server = std::string(listed.origin.server())};
}

} // namespace loa::app::view
