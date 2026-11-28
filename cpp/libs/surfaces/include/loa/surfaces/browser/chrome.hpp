#pragma once

// Browser configuration and discovery of Chrome and Xvfb executables. Port of
// `loa-surfaces/src/browser/chrome.rs`.

#include <cstdint>
#include <filesystem>
#include <functional>
#include <optional>
#include <string>
#include <string_view>
#include <variant>
#include <vector>

#include "loa/surfaces/browser/error.hpp"

namespace loa::surfaces::browser {

using Path = std::filesystem::path;

// Settings of the virtual display fallback.
struct VirtualDisplayConfig {
    // The Xvfb executable.
    Path xvfb;
    // Directory holding the X server sockets (`X<n>`).
    Path socket_dir = "/tmp/.X11-unix";
    // Upper bound on captures per second.
    std::uint32_t fps = 60;

    // Xvfb at `xvfb`, sockets in the standard `/tmp/.X11-unix`, 60 captures per second.
    [[nodiscard]] static VirtualDisplayConfig make(Path xvfb) { return {std::move(xvfb)}; }

    friend bool operator==(const VirtualDisplayConfig&, const VirtualDisplayConfig&) = default;
};

// Headless Chrome, frames from `Page.startScreencast` as JPEG.
struct Screencast {
    friend bool operator==(const Screencast&, const Screencast&) = default;
};

// Which capture strategy tabs use: screencast, or headed Chrome on its own
// Xvfb display per tab, captured as raw RGBA.
using CaptureConfig = std::variant<Screencast, VirtualDisplayConfig>;

// How the browser server launches Chrome.
struct BrowserConfig {
    // Chrome, Chromium or chrome-headless-shell.
    Path executable;
    // Parent directory of the throwaway profile directories. Empty uses the
    // system temp directory. Profiles are deleted on shutdown.
    std::optional<Path> profile_root;
    // Chrome's sandbox. Turn off only where it cannot run (tests, containers).
    bool sandbox = true;
    // Size of new tabs in surface pixels.
    std::uint32_t width = 1280;
    std::uint32_t height = 720;
    // Screencast JPEG quality, 0 to 100.
    std::uint8_t jpeg_quality = 80;
    CaptureConfig capture = Screencast{};
    // Passed to Chrome after the built in flags.
    std::vector<std::string> extra_args;

    // Headless screencast at 1280x720 with the sandbox on.
    [[nodiscard]] static BrowserConfig make(Path executable) {
        BrowserConfig config;
        config.executable = std::move(executable);
        return config;
    }
    // Looks for a browser on `$PATH`, in the usual install locations and in
    // the Playwright cache under the user's home.
    [[nodiscard]] static Result<BrowserConfig> discover();

    friend bool operator==(const BrowserConfig&, const BrowserConfig&) = default;
};

// Looks up an environment variable.
using Env = std::function<std::optional<std::string>(std::string_view)>;

// Every Chrome-like executable that exists, best first: the search path in
// `path_var` (a `$PATH` value), fixed install locations, then Playwright's
// cache under `home`, newest revision first. With `headed`, executables that
// can only run headless (chrome-headless-shell) are left out.
[[nodiscard]] std::vector<Path> chrome_candidates(std::optional<std::string_view> path_var, const std::optional<Path>& home,
                                                  bool headed);
// `chrome_candidates` with the environment passed in (tests).
[[nodiscard]] std::vector<Path> chrome_candidates_with(std::optional<std::string_view> path_var,
                                                       const std::optional<Path>& home, bool headed, const Env& env);
// The best browser `chrome_candidates` finds.
[[nodiscard]] std::optional<Path> find_chrome(std::optional<std::string_view> path_var, const std::optional<Path>& home,
                                              bool headed);

// Why the virtual display capture is not offered off Linux.
inline constexpr std::string_view VIRTUAL_DISPLAY_UNAVAILABLE = "needs Linux (Xvfb)";

// Xvfb on the search path in `path_var`. Always empty off Linux.
[[nodiscard]] std::optional<Path> find_xvfb(std::optional<std::string_view> path_var);

} // namespace loa::surfaces::browser
