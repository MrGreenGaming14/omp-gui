#pragma once

// The `SurfaceProvider` adapters: the browser, documents, VNC endpoints and MCP
// declared surfaces. Port of `loa-surfaces/src/hub/providers.rs`.

#include <map>
#include <memory>
#include <mutex>
#include <optional>
#include <string>
#include <utility>
#include <vector>

#include "loa/mcp/manager.hpp"
#include "loa/surfaces/docs/server.hpp"
#include "loa/surfaces/hub/service.hpp"
#include "loa/surfaces/vnc/surface.hpp"

namespace loa::surfaces::browser {
class BrowserServer;
}

namespace loa::surfaces::hub {

// Browser tabs on the installed Chrome, launched on first use and relaunched to switch capture.
class BrowserProvider final : public SurfaceProvider {
  public:
    BrowserProvider(std::optional<browser::BrowserConfig> config, std::expected<VirtualDisplaySetup, std::string> virtual_display,
                    CaptureMode capture);

    [[nodiscard]] CaptureMode capture() const;
    // Picks how new tabs are captured; the virtual display needs Xvfb and a headed Chrome.
    [[nodiscard]] Result<void> set_capture(CaptureMode mode);
    // Whether the running browser still captures the other way (until its tabs are closed).
    [[nodiscard]] bool capture_pending() const;
    // The tab behind `uri`, for the browser's structured tools.
    [[nodiscard]] Result<std::shared_ptr<browser::Tab>> tab(const cs::SurfaceUri& uri) const;

    [[nodiscard]] std::string_view name() const override;
    [[nodiscard]] Awaitable<Result<Opened>> open(OpenRequest req) override;
    [[nodiscard]] Awaitable<Result<void>> close(cs::SurfaceUri uri) override;
    [[nodiscard]] Awaitable<void> shutdown() override;

  private:
    [[nodiscard]] Result<VirtualDisplaySetup> display() const;
    // The running browser, launched (or relaunched for another capture once its tabs are closed) as needed.
    [[nodiscard]] Awaitable<Result<std::shared_ptr<browser::BrowserServer>>> server();

    std::optional<browser::BrowserConfig> config_;
    std::expected<VirtualDisplaySetup, std::string> virtual_display_;
    mutable std::mutex mutex_;
    CaptureMode capture_;
    std::shared_ptr<browser::BrowserServer> server_;
};

// PDF, Office and image files.
class DocsProvider final : public SurfaceProvider {
  public:
    explicit DocsProvider(docs::Fonts fonts) : server_(std::move(fonts)) {}

    [[nodiscard]] Result<std::shared_ptr<docs::DocSurface>> document(const cs::SurfaceUri& uri) const;

    [[nodiscard]] std::string_view name() const override;
    [[nodiscard]] Awaitable<Result<Opened>> open(OpenRequest req) override;
    [[nodiscard]] Awaitable<Result<void>> close(cs::SurfaceUri uri) override;

  private:
    docs::DocsServer server_;
};

// RFB endpoints by `host:port`; they stay listed after closing for reconnects.
class VncProvider final : public SurfaceProvider {
  public:
    explicit VncProvider(vnc::RfbOptions options);

    [[nodiscard]] std::string_view name() const override;
    [[nodiscard]] Awaitable<std::vector<Listed>> list() override;
    [[nodiscard]] Awaitable<Result<Opened>> open(OpenRequest req) override;

  private:
    // VNC passwords the user typed, kept in memory for reconnects.
    struct Passwords {
        std::mutex mutex;
        std::map<std::string, std::string> by_endpoint;
    };
    std::shared_ptr<Passwords> passwords_;
    vnc::RfbServer server_;
};

// Surfaces third party MCP servers declare, opened as `McpSurface`s.
class McpDeclared final : public SurfaceProvider {
  public:
    [[nodiscard]] std::map<std::string, std::vector<cs::SurfaceInfo>> declared() const;
    // Reads the `surface://` resources of every running server of `manager`; returns the failed
    // listings. The MCP calls block, so they run on their own thread.
    [[nodiscard]] Awaitable<std::vector<std::pair<std::string, std::string>>> refresh(mcp::Manager manager);

    [[nodiscard]] std::string_view name() const override { return "mcp"; }
    [[nodiscard]] Awaitable<std::vector<Listed>> list() override;
    [[nodiscard]] Awaitable<Result<Opened>> open(OpenRequest req) override;

  private:
    [[nodiscard]] std::vector<Listed> listed() const;

    mutable std::mutex mutex_;
    std::map<std::string, std::vector<cs::SurfaceInfo>> declared_;
    std::optional<mcp::Manager> manager_;
};

// What a declared resource says about itself; without the `omp/surface` metadata it is a
// pixel surface (a document when it has a MIME type) of unknown size.
[[nodiscard]] cs::SurfaceInfo declared_info(cs::SurfaceUri uri, const mcp::model::Resource& resource);

} // namespace loa::surfaces::hub
