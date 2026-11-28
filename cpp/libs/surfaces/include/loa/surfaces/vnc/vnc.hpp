#pragma once

// RFB (VNC) client mapped to a pixel surface. Port of
// `loa-surfaces/src/vnc/mod.rs`.
//
// `RfbServer` is the `vm` surface server. Each configured `RfbEndpoint` is the
// surface `surface://vm/<name>`; opening it connects an RFB 3.8 client
// (RFC 6143) that keeps a local copy of the remote framebuffer, publishes every
// framebuffer update as an RGBA frame and turns surface input into RFB pointer
// and key events. Remote cursor shapes are available through
// `RfbSurface::cursor`. A dropped connection is re-established with
// exponential backoff.

#include <chrono>
#include <cstddef>
#include <cstdint>
#include <expected>
#include <functional>
#include <memory>
#include <optional>
#include <string>
#include <string_view>
#include <vector>

#include "loa/contracts/async.hpp"
#include "loa/contracts/surface.hpp"

namespace loa::surfaces::vnc {

// Errors of the RFB client.
class RfbError {
  public:
    enum class Kind : std::uint8_t {
        Io,
        Timeout,
        Protocol,
        Refused,
        Auth,
        NoPassword,
        Security,
        Zlib,
        Key,
        ForwardButton,
        Disconnected,
    };

    [[nodiscard]] static RfbError io(std::string message) { return {Kind::Io, std::move(message)}; }
    [[nodiscard]] static RfbError timeout(std::string address) { return {Kind::Timeout, std::move(address)}; }
    [[nodiscard]] static RfbError protocol(std::string message) { return {Kind::Protocol, std::move(message)}; }
    [[nodiscard]] static RfbError refused(std::string reason) { return {Kind::Refused, std::move(reason)}; }
    [[nodiscard]] static RfbError auth(std::string reason) { return {Kind::Auth, std::move(reason)}; }
    [[nodiscard]] static RfbError no_password(std::string endpoint) { return {Kind::NoPassword, std::move(endpoint)}; }
    [[nodiscard]] static RfbError security(std::vector<std::uint8_t> types);
    [[nodiscard]] static RfbError zlib(std::string message) { return {Kind::Zlib, std::move(message)}; }
    [[nodiscard]] static RfbError key(std::string key) { return {Kind::Key, std::move(key)}; }
    [[nodiscard]] static RfbError forward_button() { return {Kind::ForwardButton, {}}; }
    [[nodiscard]] static RfbError disconnected() { return {Kind::Disconnected, {}}; }

    [[nodiscard]] Kind kind() const noexcept { return kind_; }
    // The variant's payload: message, address, reason, endpoint or key.
    [[nodiscard]] const std::string& detail() const noexcept { return detail_; }
    // The Rust `Display` text.
    [[nodiscard]] std::string message() const;
    // `SurfaceError::Backend` with the display text.
    [[nodiscard]] loa::contracts::surface::SurfaceError to_surface_error() const;

    friend bool operator==(const RfbError&, const RfbError&) = default;

  private:
    RfbError(Kind kind, std::string detail) : kind_(kind), detail_(std::move(detail)) {}

    Kind kind_;
    std::string detail_;
    std::vector<std::uint8_t> types_;
};

template <typename T>
using RfbResult = std::expected<T, RfbError>;

// Supplies an endpoint's VNC password for each VNC Authentication; the client
// uses it for one handshake and never stores it. `std::nullopt` fails the
// handshake.
class PasswordSource {
  public:
    virtual ~PasswordSource() = default;
    [[nodiscard]] virtual loa::contracts::Awaitable<std::optional<std::string>> password(std::string endpoint) = 0;
};

// A `PasswordSource` from a plain function (the Rust blanket impl for closures).
[[nodiscard]] std::shared_ptr<PasswordSource>
password_fn(std::function<std::optional<std::string>(std::string_view endpoint)> fn);

// The remote pointer shape, with transparent pixels wherever the mask is clear.
struct Cursor {
    std::uint32_t hotspot_x = 0;
    std::uint32_t hotspot_y = 0;
    std::uint32_t width = 0;
    std::uint32_t height = 0;
    // Eight-bit RGBA pixels in row-major order.
    std::vector<std::uint8_t> rgba;

    friend bool operator==(const Cursor&, const Cursor&) = default;
};

// Server part of the uris, `surface://vm/<name>`.
inline constexpr std::string_view SERVER_NAME = "vm";

// One VNC server the user configured.
struct RfbEndpoint {
    // Surface name, the last part of `surface://vm/<name>`.
    std::string name;
    // `host:port` of the RFB server.
    std::string address;

    friend bool operator==(const RfbEndpoint&, const RfbEndpoint&) = default;
};

// Connection tuning.
struct RfbOptions {
    // Limit for TCP connect plus handshake.
    std::chrono::milliseconds connect_timeout{10'000};
    // Wait before the first reconnect; doubles per failed attempt.
    std::chrono::milliseconds backoff_initial{250};
    // Longest wait between reconnect attempts.
    std::chrono::milliseconds backoff_max{30'000};
    // Scroll distance in surface pixels that makes one wheel click.
    float scroll_step = 40.0F;
    // Frames a subscriber may fall behind before it lags.
    std::size_t frame_capacity = 16;
    // ClientInit shared flag: leave other viewers connected.
    bool shared = true;
};

} // namespace loa::surfaces::vnc
