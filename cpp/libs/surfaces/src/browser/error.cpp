#include "loa/surfaces/browser/error.hpp"

namespace loa::surfaces::browser {

namespace cs = loa::contracts::surface;

BrowserError BrowserError::not_found(const cs::SurfaceUri& uri) { return {Kind::NotFound, uri.as_str()}; }

BrowserError BrowserError::surface(cs::SurfaceError error) {
    BrowserError out(Kind::Surface, error.message());
    out.surface_ = std::move(error);
    return out;
}

std::string BrowserError::message() const {
    switch (kind_) {
    case Kind::NoBrowser: return "no Chrome or Chromium executable found";
    case Kind::Config: return "browser configuration: " + detail_;
    case Kind::Cdp: return "chrome devtools protocol: " + detail_;
    case Kind::Io: return "io: " + detail_;
    case Kind::Display: return "virtual display: " + detail_;
    case Kind::X11: return "x11: " + detail_;
    case Kind::Script: return "page script: " + detail_;
    case Kind::NotFound: return "no tab " + detail_;
    case Kind::Closed: return "the browser is shut down";
    case Kind::Surface: return detail_;
    }
    return detail_;
}

cs::SurfaceError BrowserError::to_surface() const {
    switch (kind_) {
    case Kind::NotFound: {
        if (auto uri = cs::SurfaceUri::parse(detail_)) return cs::SurfaceError::not_found(*uri);
        return cs::SurfaceError::backend(message());
    }
    case Kind::Closed: return cs::SurfaceError::closed();
    case Kind::Surface: return *surface_;
    default: return cs::SurfaceError::backend(message());
    }
}

} // namespace loa::surfaces::browser
