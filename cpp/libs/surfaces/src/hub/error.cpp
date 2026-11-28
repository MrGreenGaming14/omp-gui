#include "loa/surfaces/hub/error.hpp"

#include "loa/surfaces/browser/error.hpp"
#include "loa/surfaces/docs/format.hpp"

namespace loa::surfaces::hub {

HubError HubError::not_found(const cs::SurfaceUri& uri) { return {Kind::NotFound, uri.as_str()}; }

HubError HubError::surface(cs::SurfaceError error) {
    HubError out(Kind::Surface, error.message());
    out.surface_ = std::move(error);
    return out;
}

HubError HubError::browser(const browser::BrowserError& error) { return {Kind::Browser, error.message()}; }

HubError HubError::docs(const docs::DocsError& error) { return {Kind::Docs, error.message()}; }

std::string HubError::message() const {
    switch (kind_) {
        case Kind::NoBrowser:
            return "no Chrome or Chromium is installed (looked on PATH, in the usual install locations and in "
                   "~/.cache/ms-playwright)";
        case Kind::Address: return "`" + detail_ + "` isn't a host:port address";
        case Kind::NotFound: return "no surface " + detail_;
        case Kind::NoMcp: return "the MCP servers aren't connected";
        case Kind::NoVirtualDisplay: return "the virtual display can't run here: " + detail_;
        case Kind::Surface:
        case Kind::Browser:
        case Kind::Docs:
        case Kind::Unsupported: break;
    }
    return detail_;
}

cs::SurfaceError HubError::to_surface() const {
    if (surface_) return *surface_;
    if (kind_ == Kind::NotFound) {
        if (auto uri = cs::SurfaceUri::parse(detail_)) return cs::SurfaceError::not_found(*uri);
    }
    return cs::SurfaceError::backend(message());
}

} // namespace loa::surfaces::hub
