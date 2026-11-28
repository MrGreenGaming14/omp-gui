#pragma once

// Everything that can go wrong driving Chrome or a virtual display. Port of
// `loa-surfaces/src/browser/error.rs`.

#include <cstdint>
#include <expected>
#include <optional>
#include <string>

#include "loa/contracts/surface.hpp"

namespace loa::surfaces::browser {

class BrowserError {
  public:
    enum class Kind : std::uint8_t { NoBrowser, Config, Cdp, Io, Display, X11, Script, NotFound, Closed, Surface };

    [[nodiscard]] static BrowserError no_browser() { return {Kind::NoBrowser, {}}; }
    [[nodiscard]] static BrowserError config(std::string m) { return {Kind::Config, std::move(m)}; }
    [[nodiscard]] static BrowserError cdp(std::string m) { return {Kind::Cdp, std::move(m)}; }
    [[nodiscard]] static BrowserError io(std::string m) { return {Kind::Io, std::move(m)}; }
    [[nodiscard]] static BrowserError display(std::string m) { return {Kind::Display, std::move(m)}; }
    [[nodiscard]] static BrowserError x11(std::string m) { return {Kind::X11, std::move(m)}; }
    [[nodiscard]] static BrowserError script(std::string m) { return {Kind::Script, std::move(m)}; }
    [[nodiscard]] static BrowserError not_found(const loa::contracts::surface::SurfaceUri& uri);
    [[nodiscard]] static BrowserError closed() { return {Kind::Closed, {}}; }
    [[nodiscard]] static BrowserError surface(loa::contracts::surface::SurfaceError error);

    [[nodiscard]] Kind kind() const noexcept { return kind_; }
    // The message (Config, Cdp, Io, Display, X11, Script) or uri text (NotFound).
    [[nodiscard]] const std::string& detail() const noexcept { return detail_; }
    // The Rust `Display` text.
    [[nodiscard]] std::string message() const;
    // `From<BrowserError> for SurfaceError`.
    [[nodiscard]] loa::contracts::surface::SurfaceError to_surface() const;

  private:
    BrowserError(Kind kind, std::string detail) : kind_(kind), detail_(std::move(detail)) {}

    Kind kind_;
    std::string detail_;
    std::optional<loa::contracts::surface::SurfaceError> surface_;
};

template <typename T>
using Result = std::expected<T, BrowserError>;

} // namespace loa::surfaces::browser
