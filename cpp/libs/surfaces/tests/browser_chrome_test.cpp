// Port of the unit tests in `loa-surfaces/src/browser/chrome.rs`.

#include <doctest/doctest.h>

#include <algorithm>
#include <fstream>

#include "loa/runtime/exec.hpp"
#include "loa/runtime/tempfile.hpp"
#include "loa/surfaces/browser/chrome.hpp"

using namespace loa::surfaces::browser;
namespace fs = std::filesystem;

namespace {

// Where Playwright puts Chromium and the headless shell below their `<name>-<rev>` folders.
#if defined(_WIN32)
constexpr const char* CHROMIUM = "chrome-win64/chrome.exe";
constexpr const char* SHELL = "chrome-headless-shell-win64/chrome-headless-shell.exe";
constexpr const char* EXE = ".exe";
#else
constexpr const char* CHROMIUM = "chrome-linux64/chrome";
constexpr const char* SHELL = "chrome-headless-shell-linux64/chrome-headless-shell";
constexpr const char* EXE = "";
#endif

// Makes an executable at `path`. Windows runs files by extension, so the file
// is an empty `.exe` there: discovery only looks, never runs.
void touch_exe(const fs::path& path) {
    fs::create_directories(path.parent_path());
#if defined(_WIN32)
    std::ofstream(path).close();
#else
    REQUIRE(loa::runtime::exec::write_executable(path, "#!/bin/sh\n"));
#endif
}

Env no_env() {
    return [](std::string_view) -> std::optional<std::string> { return std::nullopt; };
}

} // namespace

TEST_CASE("discovery ranks PATH over playwright and newest first") {
    auto tmp = loa::runtime::TempDir::create();
    REQUIRE(tmp);
    const auto bin = tmp->path() / "bin";
    const auto home = tmp->path() / "home";
    const auto cache = home / ".cache/ms-playwright";
    const auto shell = cache / "chromium_headless_shell-1300" / SHELL;
    const std::vector<fs::path> revisions = {cache / "chromium-1200" / CHROMIUM, cache / "chromium-900" / CHROMIUM};
    for (const auto& exe : revisions) touch_exe(exe);
    touch_exe(shell);
    fs::create_directories(bin);
    // Unix: no execute bit. Windows: no executable extension.
    std::ofstream(bin / "chrome") << "not executable";
    const auto bin_str = bin.string();
    auto ours = [&](bool headed) {
        auto found = chrome_candidates_with(bin_str, home, headed, no_env());
        std::erase_if(found, [&](const fs::path& p) { return !p.string().starts_with(tmp->path().string()); });
        return found;
    };
    std::vector<fs::path> want = revisions;
    want.push_back(shell);
    CHECK(ours(false) == want);
    const auto headed = ours(true);
    CHECK_MESSAGE(std::ranges::find(headed, shell) == headed.end(), "headed discovery skips the headless shell");
    const auto on_path = bin / (std::string("chromium") + EXE);
    touch_exe(on_path);
    const auto first = chrome_candidates_with(bin_str, home, false, no_env());
    REQUIRE(!first.empty());
    CHECK(first.front() == on_path);
}

#if defined(__linux__)
TEST_CASE("xvfb is found on the search path only") {
    auto tmp = loa::runtime::TempDir::create();
    REQUIRE(tmp);
    const auto bin = tmp->path() / "bin";
    fs::create_directories(bin);
    const auto bin_str = bin.string();
    CHECK(find_xvfb(bin_str) == std::nullopt);
    touch_exe(bin / "Xvfb");
    CHECK(find_xvfb(bin_str) == std::optional<fs::path>(bin / "Xvfb"));
}
#else
TEST_CASE("xvfb is never found off linux") {
    auto tmp = loa::runtime::TempDir::create();
    REQUIRE(tmp);
    std::ofstream(tmp->path() / "Xvfb").close();
    std::ofstream(tmp->path() / "Xvfb.exe").close();
    CHECK(find_xvfb(tmp->path().string()) == std::nullopt);
}
#endif

TEST_CASE("config defaults and the unavailable text") {
    const auto config = BrowserConfig::make("/bin/chrome");
    CHECK(config.width == 1280);
    CHECK(config.height == 720);
    CHECK(config.jpeg_quality == 80);
    CHECK(config.sandbox);
    CHECK(std::holds_alternative<Screencast>(config.capture));
    const auto display = VirtualDisplayConfig::make("/usr/bin/Xvfb");
    CHECK(display.socket_dir == fs::path("/tmp/.X11-unix"));
    CHECK(display.fps == 60);
    CHECK(VIRTUAL_DISPLAY_UNAVAILABLE == "needs Linux (Xvfb)");
    CHECK(BrowserError::no_browser().message() == "no Chrome or Chromium executable found");
}
