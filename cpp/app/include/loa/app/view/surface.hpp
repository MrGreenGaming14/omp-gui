#pragma once

// The surface hub's state as the Surfaces launcher shows it, and the capture choice back.

#include "loa/surfaces/feed.hpp"
#include "loa/surfaces/hub/service.hpp"
#include "loa/view/surface.hpp"

namespace loa::app::view {

[[nodiscard]] ::loa::view::surface::Capture capture_to_view(::loa::surfaces::CaptureMode capture) noexcept;
[[nodiscard]] ::loa::surfaces::CaptureMode capture_from_view(::loa::view::surface::Capture capture) noexcept;
// What the launcher says about the browser.
[[nodiscard]] ::loa::view::surface::BrowserStatus browser_status(const ::loa::surfaces::hub::SurfaceHub& hub);
// What the launcher says about the browser capture.
[[nodiscard]] ::loa::view::surface::CaptureStatus capture_status(const ::loa::surfaces::hub::SurfaceHub& hub, bool pending);
// A listed surface the launcher offers.
[[nodiscard]] ::loa::view::surface::AvailableSurface available(::loa::surfaces::hub::Listed listed);

} // namespace loa::app::view
