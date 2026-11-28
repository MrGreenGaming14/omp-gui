#pragma once

// The `browser` surface server and its tabs. Port of
// `loa-surfaces/src/browser/{mod,server,tab,screencast}.rs`.
//
// `BrowserServer` is the `browser` `SurfaceServer`: each open tab is a pixel
// surface `surface://browser/<tab>`. Chrome is driven over the DevTools
// protocol. Frames come from one of two capture strategies: Chrome's own
// `Page.startScreencast` on a headless browser, or a headed browser on an Xvfb
// virtual display captured as raw RGBA. Input always goes through CDP
// `Input.*` events.

#include <atomic>
#include <cstddef>
#include <cstdint>
#include <map>
#include <memory>
#include <mutex>
#include <string>
#include <string_view>
#include <vector>

#include "loa/contracts/async.hpp"
#include "loa/contracts/surface.hpp"
#include "loa/surfaces/browser/chrome.hpp"
#include "loa/surfaces/browser/error.hpp"
#include "loa/surfaces/browser/structured.hpp"

namespace loa::surfaces::browser {

// Frames a slow viewer may fall behind before it loses the oldest.
inline constexpr std::size_t FRAME_BACKLOG = 8;

// Server part of every browser surface uri.
inline constexpr std::string_view SERVER_NAME = "browser";

class Instance;

// A browser tab: `surface://browser/<tab>`.
class Tab : public loa::contracts::surface::Surface {
  public:
    struct Impl;
    explicit Tab(std::shared_ptr<Impl> impl);
    ~Tab() override;

    [[nodiscard]] const loa::contracts::surface::SurfaceUri& uri() const;

    // Loads `url` and waits for the load event.
    [[nodiscard]] loa::contracts::Awaitable<Result<PageInfo>> navigate(std::string url);
    // Current URL and title. Also updates the name shown on the surface tab.
    [[nodiscard]] loa::contracts::Awaitable<Result<PageInfo>> page_info();
    // Readable text of the page, without markup, scripts or hidden elements.
    [[nodiscard]] loa::contracts::Awaitable<Result<std::string>> page_text();
    // Interactive elements with role, name and box in surface coordinates.
    [[nodiscard]] loa::contracts::Awaitable<Result<std::vector<OutlineNode>>> outline();

    [[nodiscard]] loa::contracts::surface::SurfaceInfo info() const override;
    [[nodiscard]] loa::contracts::surface::Result<std::shared_ptr<loa::contracts::surface::FrameReceiver>>
    frames() override;
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::surface::Result<void>> request_keyframe() override;
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::surface::Result<void>>
    input(loa::contracts::surface::InputEvent event) override;
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::surface::Result<loa::contracts::surface::Document>>
    document() override;
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::surface::Result<loa::contracts::surface::Bytes>>
    screenshot() override;

    // Internal to the server.
    [[nodiscard]] Impl& impl() const { return *impl_; }

  private:
    std::shared_ptr<Impl> impl_;
};

// The `browser` surface server. Tabs are `surface://browser/tab<n>`.
class BrowserServer : public loa::contracts::surface::SurfaceServer {
  public:
    // Starts the server on the calling coroutine's executor. In screencast
    // mode this launches headless Chrome now; in display mode each tab gets
    // its own display and browser when it opens.
    [[nodiscard]] static loa::contracts::Awaitable<Result<std::shared_ptr<BrowserServer>>> launch(BrowserConfig config);

    ~BrowserServer() override;

    [[nodiscard]] const BrowserConfig& config() const { return config_; }

    // Opens a tab at the configured size and loads `url` in it.
    [[nodiscard]] loa::contracts::Awaitable<Result<std::shared_ptr<Tab>>> open_tab(std::string url);
    // The open tab behind `uri`.
    [[nodiscard]] Result<std::shared_ptr<Tab>> tab(const loa::contracts::surface::SurfaceUri& uri) const;
    // Open tabs in the order they were opened.
    [[nodiscard]] std::vector<std::shared_ptr<Tab>> tabs() const;
    // Closes the tab behind `uri`. Its viewers' frame streams end.
    [[nodiscard]] loa::contracts::Awaitable<Result<void>> close_tab(loa::contracts::surface::SurfaceUri uri);
    // Process ids of the running browsers.
    [[nodiscard]] loa::contracts::Awaitable<std::vector<std::uint32_t>> process_ids() const;
    // Closes every tab and stops every browser and display. Later calls to
    // `open_tab` fail with `BrowserError::closed`.
    [[nodiscard]] loa::contracts::Awaitable<Result<void>> shutdown();

    [[nodiscard]] std::string_view name() const override { return SERVER_NAME; }
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::surface::Result<std::vector<loa::contracts::surface::SurfaceInfo>>>
    list() override;
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::surface::Result<std::shared_ptr<loa::contracts::surface::Surface>>>
    open(const loa::contracts::surface::SurfaceUri& uri) override;

  private:
    BrowserServer(BrowserConfig config, std::shared_ptr<Instance> shared);

    [[nodiscard]] loa::contracts::Awaitable<Result<std::shared_ptr<Tab>>>
    open_screencast_tab(loa::contracts::surface::SurfaceUri uri);
    [[nodiscard]] loa::contracts::Awaitable<Result<std::shared_ptr<Tab>>>
    open_display_tab(loa::contracts::surface::SurfaceUri uri, VirtualDisplayConfig display);
    [[nodiscard]] bool shares(const Tab& tab) const;
    [[nodiscard]] loa::contracts::Awaitable<void> close(std::shared_ptr<Tab> tab);

    BrowserConfig config_;
    // The one headless browser all tabs share, in screencast mode.
    std::shared_ptr<Instance> shared_;
    mutable std::mutex mutex_;
    std::map<std::uint64_t, std::shared_ptr<Tab>> tabs_;
    std::atomic<std::uint64_t> next_{0};
    std::atomic<bool> closed_{false};
};

} // namespace loa::surfaces::browser
