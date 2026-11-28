#pragma once

// The Surfaces tab: the open surfaces as tabs, each drawn by its `State`, and a launcher that
// opens a browser, a document, a VM over RFB, or a surface an MCP server declares. The tab opens
// nothing itself: it emits `Intent`s, and the app hands opened surfaces back with
// `Panel::attach`. Port of `crates/loa-ui-surface/src/panel/mod.rs`. Live video (the Rust
// `remote-video` feature) is built with `LOA_REMOTE_VIDEO`.

#include <cstddef>
#include <filesystem>
#include <optional>
#include <string>
#include <utility>
#include <variant>
#include <vector>

#include "loa/ui/surface/picker.hpp"
#include "loa/ui/surface/tab.hpp"
#include "loa/view/surface.hpp"

namespace loa::ui::surface {

using loa::view::surface::AvailableSurface;
using loa::view::surface::BrowserStatus;
using loa::view::surface::Capture;
using loa::view::surface::CaptureStatus;
using loa::view::surface::VideoStatus;

// Characters of a surface name shown on its tab.
inline constexpr std::size_t TAB_LABEL_CHARS = 18;

namespace panel {

namespace msg {
// A message of one open surface's tab.
struct Surface {
    SurfaceUri uri;
    surface::Message message;
};
struct Select {
    SurfaceUri uri;
};
// Show the launcher.
struct Launcher {};
struct Close {
    SurfaceUri uri;
};
struct Url {
    std::string url;
};
struct OpenUrl {};
struct Picker {
    picker::Message message;
};
struct Address {
    std::string address;
};
struct Password {
    std::string password;
};
struct Connect {};
struct OpenAvailable {
    SurfaceUri uri;
};
#if defined(LOA_REMOTE_VIDEO)
// Turn live video encoding on or off.
struct Video {
    bool enabled = false;
};
#endif
// Pick how browser frames are captured.
struct Capture {
    loa::view::surface::Capture capture = loa::view::surface::Capture::Screencast;
};
} // namespace msg

using Message = std::variant<msg::Surface, msg::Select, msg::Launcher, msg::Close, msg::Url, msg::OpenUrl, msg::Picker,
                             msg::Address, msg::Password, msg::Connect, msg::OpenAvailable,
#if defined(LOA_REMOTE_VIDEO)
                             msg::Video,
#endif
                             msg::Capture>;

// Work for the tab of `uri`.
struct Routed {
    SurfaceUri uri;
    Work work;
    friend bool operator==(const Routed&, const Routed&) = default;
};

// The surface work of one panel update, routed to the tabs' runners.
struct Task {
    std::vector<Routed> work;

    [[nodiscard]] static Task none() { return {}; }
    [[nodiscard]] static Task of(const SurfaceUri& uri, surface::Task task);
    [[nodiscard]] bool empty() const { return work.empty(); }
    friend bool operator==(const Task&, const Task&) = default;
};

} // namespace panel

// One open surface: its tab state and how the strip shows it.
struct OpenTab {
    std::string server;
    // The name, cut to fit a tab.
    std::string label;
    State state;
};

class Panel {
  public:
    // `documents_dir` is where the file picker starts.
    Panel(Options options, BrowserStatus browser, std::filesystem::path documents_dir);

    std::pair<panel::Task, std::optional<Intent>> update(panel::Message message);

    // Shows an opened surface in a new tab and makes it active. The task streams its frames (or
    // fetches its document).
    panel::Task attach(SurfacePtr surface, std::string server);
    // Drops the tab of `uri`, e.g. when the surface closed elsewhere.
    void detach(const SurfaceUri& uri);
    // Opening failed: shown on the launcher.
    void failed(std::string error);

    void set_available(std::vector<AvailableSurface> available) { available_ = std::move(available); }
    void set_browser(BrowserStatus browser) { browser_ = std::move(browser); }
    void set_capture(CaptureStatus capture) { capture_ = std::move(capture); }
#if defined(LOA_REMOTE_VIDEO)
    void set_video(VideoStatus video) { video_ = std::move(video); }
    [[nodiscard]] const VideoStatus& video() const { return video_; }
#endif

    // The active surface, null on the launcher.
    [[nodiscard]] const SurfaceUri* active() const { return active_ ? &*active_ : nullptr; }
    // The tab of an open surface.
    [[nodiscard]] const State* surface(const SurfaceUri& uri) const;
    // Open surfaces in tab order.
    [[nodiscard]] const std::vector<OpenTab>& tabs() const { return tabs_; }
    [[nodiscard]] std::vector<const SurfaceInfo*> open_surfaces() const;
    [[nodiscard]] const std::optional<std::string>& pending() const { return pending_; }
    [[nodiscard]] const std::optional<std::string>& error() const { return error_; }
    // Surfaces the launcher lists for opening.
    [[nodiscard]] const std::vector<AvailableSurface>& available() const { return available_; }
    [[nodiscard]] const BrowserStatus& browser() const { return browser_; }
    [[nodiscard]] const CaptureStatus& capture() const { return capture_; }
    [[nodiscard]] const std::string& url() const { return url_; }
    [[nodiscard]] const std::string& address() const { return address_; }
    [[nodiscard]] const std::string& password() const { return password_; }
    [[nodiscard]] const FilePicker& picker() const { return picker_; }

  private:
    // Shows `what` as pending and passes `intent` on.
    Intent start(std::string what, Intent intent);
    [[nodiscard]] std::optional<std::size_t> index(const SurfaceUri& uri) const;

    Options options_;
    std::vector<OpenTab> tabs_;
    // `nullopt` shows the launcher.
    std::optional<SurfaceUri> active_;
    std::vector<AvailableSurface> available_;
    BrowserStatus browser_;
    std::string url_;
    std::string address_;
    std::string password_;
    FilePicker picker_;
    // What is being opened right now.
    std::optional<std::string> pending_;
    std::optional<std::string> error_;
#if defined(LOA_REMOTE_VIDEO)
    VideoStatus video_;
#endif
    CaptureStatus capture_;
};

// How the launcher names the browser: Playwright's builds by their kind, others by their
// executable name.
[[nodiscard]] std::string browser_name(const std::filesystem::path& executable);

} // namespace loa::ui::surface
