#pragma once

// `SurfaceHub`: a registry of `SurfaceProvider`s, the open surfaces and their
// events. Port of `loa-surfaces/src/hub/service.rs`.

#include <atomic>
#include <cstdint>
#include <expected>
#include <filesystem>
#include <map>
#include <memory>
#include <mutex>
#include <optional>
#include <string>
#include <string_view>
#include <utility>
#include <variant>
#include <vector>

#include "loa/contracts/async.hpp"
#include "loa/contracts/surface.hpp"
#include "loa/runtime/registry.hpp"
#include "loa/surfaces/browser/chrome.hpp"
#include "loa/surfaces/docs/raster.hpp"
#include "loa/surfaces/feed.hpp"
#include "loa/surfaces/hub/error.hpp"
#include "loa/surfaces/vnc/vnc.hpp"
#ifdef LOA_SURFACES_REMOTE_VIDEO
#include "loa/surfaces/hub/video.hpp"
#endif

namespace loa::mcp {
class Manager;
}
namespace loa::surfaces::browser {
class Tab;
}
namespace loa::surfaces::docs {
class DocSurface;
}

namespace loa::surfaces::hub {

using loa::contracts::Awaitable;
using Path = std::filesystem::path;
using loa::surfaces::CaptureMode;

// Xvfb and a Chrome that can run headed.
struct VirtualDisplaySetup {
    Path xvfb;
    Path chrome;

    // Xvfb on `path_var` and a headed Chrome, or what is missing.
    [[nodiscard]] static std::expected<VirtualDisplaySetup, std::string> find(std::optional<std::string_view> path_var,
                                                                              const std::optional<Path>& home);

    friend bool operator==(const VirtualDisplaySetup&, const VirtualDisplaySetup&) = default;
};

// What the hub starts with.
struct HubConfig {
    // The browser for browser surfaces; none when no Chrome or Chromium is installed.
    std::optional<browser::BrowserConfig> browser;
    // Fonts for document pages and agent screenshots.
    docs::Fonts fonts;
    // Where text-only `surface_screenshot` results are saved.
    Path screenshot_dir;
    vnc::RfbOptions rfb;
#ifdef LOA_SURFACES_REMOTE_VIDEO
    // Live video for viewers across the network.
    VideoConfig video;
#endif
    // How browser tabs are captured.
    CaptureMode capture = CaptureMode::Screencast;
    // What the virtual display capture runs with, or why it can't run.
    std::expected<VirtualDisplaySetup, std::string> virtual_display = std::unexpected(std::string());

    // Finds Chrome, Xvfb, ffmpeg and the system fonts; profiles and screenshots go under `state_dir`.
    [[nodiscard]] static HubConfig discover(const Path& state_dir);
};

// Whether unprivileged user namespaces (Chrome's Linux sandbox) are allowed.
[[nodiscard]] bool user_namespaces();

// Which provider a surface comes from.
class Origin {
  public:
    enum class Kind : std::uint8_t { Browser, Document, Vm, Mcp };

    [[nodiscard]] static Origin browser() { return Origin(Kind::Browser, {}); }
    [[nodiscard]] static Origin document() { return Origin(Kind::Document, {}); }
    [[nodiscard]] static Origin vm() { return Origin(Kind::Vm, {}); }
    // Declared by the named third party MCP server.
    [[nodiscard]] static Origin mcp(std::string server) { return Origin(Kind::Mcp, std::move(server)); }

    [[nodiscard]] Kind kind() const noexcept { return kind_; }
    // The server name shown and recorded with the surface.
    [[nodiscard]] std::string_view server() const noexcept;
    // The registry key of the provider.
    [[nodiscard]] std::string_view provider() const noexcept;

    friend bool operator==(const Origin&, const Origin&) = default;

  private:
    Origin(Kind kind, std::string server) : kind_(kind), server_(std::move(server)) {}
    Kind kind_;
    std::string server_;
};

// An open surface; `surface` serves viewers and tools in this process.
struct Opened {
    // The surface as it was when opened.
    cs::SurfaceInfo info;
    Origin origin;
    std::shared_ptr<cs::Surface> surface;

    [[nodiscard]] static Opened make(std::shared_ptr<cs::Surface> surface, Origin origin);
};

// A surface the Surfaces tab lists: open, or ready to be opened.
struct Listed {
    cs::SurfaceInfo info;
    Origin origin;
    bool open = false;

    friend bool operator==(const Listed&, const Listed&) = default;
};

// What changed in the hub, for the UI and the session record.
namespace hub_event {
struct Opened {
    cs::SurfaceInfo info;
    Origin origin;
    friend bool operator==(const Opened&, const Opened&) = default;
};
struct Closed {
    cs::SurfaceUri uri;
    friend bool operator==(const Closed&, const Closed&) = default;
};
// The surfaces MCP servers declare changed.
struct Declared {
    friend bool operator==(const Declared&, const Declared&) = default;
};
} // namespace hub_event
using HubEvent = std::variant<hub_event::Opened, hub_event::Closed, hub_event::Declared>;

// A subscriber to the hub's events (tokio `broadcast::Receiver`, 64 deep: the
// oldest events are dropped when it lags).
class HubEventReceiver {
  public:
    struct State;
    explicit HubEventReceiver(std::shared_ptr<State> state) : state_(std::move(state)) {}

    // The next event; waits on the calling coroutine's executor.
    [[nodiscard]] Awaitable<HubEvent> recv();
    [[nodiscard]] std::optional<HubEvent> try_recv();

  private:
    std::shared_ptr<State> state_;
};

// What to open; each variant goes to one provider.
namespace open_request {
// A browser tab at a URL.
struct Url {
    std::string url;
    friend bool operator==(const Url&, const Url&) = default;
};
// A document file.
struct File {
    Path path;
    friend bool operator==(const File&, const File&) = default;
};
// An RFB server at `host:port`; `password` answers VNC Authentication.
struct Vm {
    std::string address;
    std::optional<std::string> password;
    friend bool operator==(const Vm&, const Vm&) = default;
};
// A listed surface (a VM endpoint or a declared surface).
struct Uri {
    cs::SurfaceUri uri;
    friend bool operator==(const Uri&, const Uri&) = default;
};
} // namespace open_request
using OpenRequest = std::variant<open_request::Url, open_request::File, open_request::Vm, open_request::Uri>;

// The Rust `Debug` text of `req`, e.g. `Url("example.com")`.
[[nodiscard]] std::string debug(const OpenRequest& req);

// A source of surfaces the hub opens and closes.
class SurfaceProvider {
  public:
    virtual ~SurfaceProvider() = default;
    [[nodiscard]] virtual std::string_view name() const = 0;
    // Surfaces that can be opened by uri while closed. Default: none.
    [[nodiscard]] virtual Awaitable<std::vector<Listed>> list();
    [[nodiscard]] virtual Awaitable<Result<Opened>> open(OpenRequest req) = 0;
    // Default: nothing to do.
    [[nodiscard]] virtual Awaitable<Result<void>> close(cs::SurfaceUri uri);
    [[nodiscard]] virtual Awaitable<void> shutdown();
};

class BrowserProvider;
class DocsProvider;
class McpDeclared;

// The surface service: the providers, the open surfaces and their events.
class SurfaceHub {
  public:
    [[nodiscard]] static std::shared_ptr<SurfaceHub> make(HubConfig config);
    ~SurfaceHub();
    SurfaceHub(const SurfaceHub&) = delete;
    SurfaceHub& operator=(const SurfaceHub&) = delete;

    [[nodiscard]] const HubConfig& config() const noexcept { return config_; }
    [[nodiscard]] std::optional<Path> browser_executable() const;
    [[nodiscard]] bool browser_sandboxed() const;

    [[nodiscard]] HubEventReceiver events();

    // Opens `req` with its provider; an already open surface is returned as is.
    [[nodiscard]] Awaitable<Result<Opened>> open_request(OpenRequest req);
    // Opens a browser tab at `url` (see `normalize_url`), launching the browser on first use.
    [[nodiscard]] Awaitable<Result<Opened>> open_browser(std::string url);
    // Opens a PDF, XLSX, DOCX, PPTX or image file as a document surface.
    [[nodiscard]] Awaitable<Result<Opened>> open_document(Path path);
    // Connects to the RFB server at `host:port` as `surface://vm/<host:port>`.
    [[nodiscard]] Awaitable<Result<Opened>> open_vm(std::string address, std::optional<std::string> password);
    // Opens any listed surface.
    [[nodiscard]] Awaitable<Result<Opened>> open(cs::SurfaceUri uri);

#ifdef LOA_SURFACES_REMOTE_VIDEO
    [[nodiscard]] RemoteVideo& video() noexcept { return *video_; }
    // The open surface `uri` for a viewer across the network.
    [[nodiscard]] Awaitable<Result<std::shared_ptr<cs::Surface>>> network_surface(cs::SurfaceUri uri);
#endif

    [[nodiscard]] CaptureMode capture() const;
    [[nodiscard]] Result<void> set_capture(CaptureMode mode);
    [[nodiscard]] bool capture_pending() const;

    [[nodiscard]] Awaitable<Result<std::shared_ptr<browser::Tab>>> tab(cs::SurfaceUri uri);
    [[nodiscard]] Result<std::shared_ptr<docs::DocSurface>> document(const cs::SurfaceUri& uri) const;

    [[nodiscard]] std::optional<Opened> opened(const cs::SurfaceUri& uri) const;
    // Open surfaces in the order they were opened.
    [[nodiscard]] std::vector<Opened> open_surfaces() const;
    // Closes `uri`; VM endpoints and declared surfaces stay listed.
    [[nodiscard]] Awaitable<Result<void>> close(cs::SurfaceUri uri);
    // Open surfaces first, then the closed ones providers list.
    [[nodiscard]] Awaitable<std::vector<Listed>> list();

    // Surfaces MCP servers declare, by server.
    [[nodiscard]] std::map<std::string, std::vector<cs::SurfaceInfo>> declared() const;
    // Reads the `surface://` resources of every running server of `manager`; returns the failed listings.
    [[nodiscard]] Awaitable<std::vector<std::pair<std::string, std::string>>> refresh_declared(mcp::Manager manager);

    // Closes every surface and stops the providers.
    [[nodiscard]] Awaitable<void> shutdown();

    // The next input number shared by every tool registration on this hub (from 1).
    [[nodiscard]] std::uint64_t next_tool_input_seq() noexcept { return tool_input_seq_.fetch_add(1) + 1; }
    // The last input number handed out.
    [[nodiscard]] std::uint64_t tool_input_seq() const noexcept { return tool_input_seq_.load(); }

  private:
    explicit SurfaceHub(HubConfig config);
    void emit(const HubEvent& event);
    Opened register_opened(Opened opened);
    [[nodiscard]] Awaitable<std::vector<Listed>> listed();

    HubConfig config_;
    std::atomic<std::uint64_t> tool_input_seq_{0};
    loa::runtime::Registry<SurfaceProvider> providers_;
    std::shared_ptr<BrowserProvider> browser_;
    std::shared_ptr<DocsProvider> docs_;
    std::shared_ptr<McpDeclared> mcp_;
    mutable std::mutex mutex_;
    std::vector<Opened> open_;
    std::vector<std::weak_ptr<HubEventReceiver::State>> subscribers_;
#ifdef LOA_SURFACES_REMOTE_VIDEO
    std::unique_ptr<RemoteVideo> video_;
#endif
};

// Adds `https://` (or `http://` for localhost) to an address typed without a scheme.
[[nodiscard]] std::string normalize_url(std::string_view url);

} // namespace loa::surfaces::hub
