#pragma once

// One RFB connection as a pixel surface, with reconnect, and the `vm` surface
// server. Port of `loa-surfaces/src/vnc/{surface,server}.rs`.

#include <cstdint>
#include <map>
#include <memory>
#include <mutex>
#include <optional>
#include <string>
#include <vector>

#include "loa/contracts/async.hpp"
#include "loa/contracts/surface.hpp"
#include "loa/surfaces/vnc/vnc.hpp"

namespace loa::surfaces::vnc {

// The latest remote pointer shape (Rust `watch::Receiver<Option<Arc<Cursor>>>`).
class CursorWatch {
  public:
    CursorWatch();
    ~CursorWatch();
    CursorWatch(const CursorWatch&) = delete;
    CursorWatch& operator=(const CursorWatch&) = delete;

    // The shape, or null until the server sends one.
    [[nodiscard]] std::shared_ptr<const Cursor> current() const;
    // Counts replacements; 0 before the first shape.
    [[nodiscard]] std::uint64_t version() const;
    // Waits until `version()` exceeds `seen`.
    [[nodiscard]] loa::contracts::Awaitable<void> changed(std::uint64_t seen);
    // Replaces the shape and wakes every waiter.
    void replace(std::shared_ptr<const Cursor> cursor);

  private:
    struct Impl;
    std::unique_ptr<Impl> impl_;
};

// A connected RFB endpoint. Destroying it closes the connection.
class RfbSurface : public loa::contracts::surface::Surface {
  public:
    // Connects to `endpoint` on the calling coroutine's executor and keeps the
    // connection up until destroyed. Fails when the first connection fails;
    // later drops are retried with backoff.
    [[nodiscard]] static loa::contracts::Awaitable<RfbResult<std::shared_ptr<RfbSurface>>>
    connect(loa::contracts::surface::SurfaceUri uri, RfbEndpoint endpoint, std::shared_ptr<PasswordSource> passwords,
            RfbOptions options);

    ~RfbSurface() override;
    RfbSurface(const RfbSurface&) = delete;
    RfbSurface& operator=(const RfbSurface&) = delete;

    // The remote pointer shape; empty until the server sends one.
    [[nodiscard]] std::shared_ptr<CursorWatch> cursor() const;

    [[nodiscard]] loa::contracts::surface::SurfaceInfo info() const override;
    [[nodiscard]] loa::contracts::surface::Result<std::shared_ptr<loa::contracts::surface::FrameReceiver>>
    frames() override;
    // Publishes the local framebuffer copy as a keyframe right away; it is the
    // complete picture, so no round trip to the server is needed.
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::surface::Result<void>> request_keyframe() override;
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::surface::Result<void>>
    input(loa::contracts::surface::InputEvent event) override;
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::surface::Result<loa::contracts::surface::Document>>
    document() override;
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::surface::Result<loa::contracts::surface::Bytes>>
    screenshot() override;

    struct Shared;

  private:
    explicit RfbSurface(std::shared_ptr<Shared> shared);
    std::shared_ptr<Shared> shared_;
};

// Serves each `RfbEndpoint` as the pixel surface `surface://vm/<name>`;
// opening a surface that is already open returns the same connection.
class RfbServer : public loa::contracts::surface::SurfaceServer {
  public:
    RfbServer(std::vector<RfbEndpoint> endpoints, std::shared_ptr<PasswordSource> passwords, RfbOptions options);

    // Adds `endpoint`, replacing one with the same name, and returns its uri.
    // When the address changed, the next open connects to the new address;
    // surfaces already open keep their connection until destroyed.
    [[nodiscard]] loa::contracts::surface::Result<loa::contracts::surface::SurfaceUri> add_endpoint(RfbEndpoint endpoint);

    // Opens `uri`, reusing its live connection.
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::surface::Result<std::shared_ptr<RfbSurface>>>
    open_rfb(loa::contracts::surface::SurfaceUri uri);

    [[nodiscard]] std::string_view name() const override { return SERVER_NAME; }
    // Every configured endpoint. A surface that is not open reports a 0x0
    // size, since the size is only known after connecting.
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::surface::Result<std::vector<loa::contracts::surface::SurfaceInfo>>>
    list() override;
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::surface::Result<std::shared_ptr<loa::contracts::surface::Surface>>>
    open(const loa::contracts::surface::SurfaceUri& uri) override;

  private:
    [[nodiscard]] std::shared_ptr<RfbSurface> live(const std::string& name);

    mutable std::mutex mutex_;
    std::vector<RfbEndpoint> endpoints_;
    std::shared_ptr<PasswordSource> passwords_;
    RfbOptions options_;
    std::map<std::string, std::weak_ptr<RfbSurface>> open_;
};

} // namespace loa::surfaces::vnc
