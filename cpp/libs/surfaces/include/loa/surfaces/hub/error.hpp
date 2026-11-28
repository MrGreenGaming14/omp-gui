#pragma once

// What the surface hub reports. Port of `HubError` in `loa-surfaces/src/hub/mod.rs`.

#include <cstdint>
#include <expected>
#include <optional>
#include <string>

#include "loa/contracts/surface.hpp"

namespace loa::surfaces::browser {
class BrowserError;
}
namespace loa::surfaces::docs {
class DocsError;
}

namespace loa::surfaces::hub {

namespace cs = loa::contracts::surface;

class HubError {
  public:
    enum class Kind : std::uint8_t { NoBrowser, Address, NotFound, NoMcp, NoVirtualDisplay, Surface, Browser, Docs, Unsupported };

    [[nodiscard]] static HubError no_browser() { return {Kind::NoBrowser, {}}; }
    [[nodiscard]] static HubError address(std::string address) { return {Kind::Address, std::move(address)}; }
    [[nodiscard]] static HubError not_found(const cs::SurfaceUri& uri);
    [[nodiscard]] static HubError no_mcp() { return {Kind::NoMcp, {}}; }
    [[nodiscard]] static HubError no_virtual_display(std::string why) { return {Kind::NoVirtualDisplay, std::move(why)}; }
    [[nodiscard]] static HubError surface(cs::SurfaceError error);
    [[nodiscard]] static HubError browser(const browser::BrowserError& error);
    [[nodiscard]] static HubError docs(const docs::DocsError& error);
    [[nodiscard]] static HubError unsupported(std::string what) { return {Kind::Unsupported, std::move(what)}; }

    [[nodiscard]] Kind kind() const noexcept { return kind_; }
    // The payload: address, uri text, reason, or the inner error's text.
    [[nodiscard]] const std::string& detail() const noexcept { return detail_; }
    // The Rust `Display` text.
    [[nodiscard]] std::string message() const;
    // `From<HubError> for SurfaceError`: the inner surface error, `NotFound`, else `Backend(message)`.
    [[nodiscard]] cs::SurfaceError to_surface() const;

  private:
    HubError(Kind kind, std::string detail) : kind_(kind), detail_(std::move(detail)) {}

    Kind kind_;
    std::string detail_;
    std::optional<cs::SurfaceError> surface_;
};

template <typename T>
using Result = std::expected<T, HubError>;

} // namespace loa::surfaces::hub
