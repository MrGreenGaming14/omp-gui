#include "loa/surfaces/browser/chrome.hpp"

#include <algorithm>
#include <array>
#include <charconv>
#include <cstdlib>
#include <set>
#include <span>
#include <system_error>
#include <tuple>

#include "loa/runtime/exec.hpp"
#include "loa/runtime/fs.hpp"

namespace loa::surfaces::browser {

namespace exec = loa::runtime::exec;

namespace {

// Executable names looked up on the search path, best first. Windows adds `.exe` itself.
constexpr std::array<std::string_view, 7> PATH_NAMES = {
    "google-chrome-stable", "google-chrome", "chromium", "chromium-browser", "chrome", "msedge", "chrome-headless-shell",
};

#if !defined(_WIN32)
// Install locations that are often not on the search path.
constexpr std::array<std::string_view, 4> FIXED_PATHS = {
    "/opt/google/chrome/chrome", "/usr/lib/chromium/chromium", "/usr/lib/chromium-browser/chromium-browser",
    "/snap/bin/chromium"};
// Playwright's full Chromium inside `chromium-<rev>`.
constexpr std::array<std::string_view, 1> PLAYWRIGHT_CHROMIUM = {"chrome-linux64/chrome"};
// Playwright's headless shell inside `chromium_headless_shell-<rev>`.
constexpr std::array<std::string_view, 1> PLAYWRIGHT_SHELL = {"chrome-headless-shell-linux64/chrome-headless-shell"};
constexpr char PATH_SEPARATOR = ':';
#else
// Install locations under environment variables, as `(variable, relative path)`.
constexpr std::array<std::pair<std::string_view, std::string_view>, 6> WINDOWS_INSTALLS = {{
    {"ProgramFiles", R"(Google\Chrome\Application\chrome.exe)"},
    {"ProgramFiles(x86)", R"(Google\Chrome\Application\chrome.exe)"},
    {"LOCALAPPDATA", R"(Google\Chrome\Application\chrome.exe)"},
    {"ProgramFiles(x86)", R"(Microsoft\Edge\Application\msedge.exe)"},
    {"ProgramFiles", R"(Microsoft\Edge\Application\msedge.exe)"},
    {"LOCALAPPDATA", R"(Microsoft\Edge\Application\msedge.exe)"},
}};
constexpr std::array<std::string_view, 2> PLAYWRIGHT_CHROMIUM = {"chrome-win64/chrome.exe", "chrome-win/chrome.exe"};
constexpr std::array<std::string_view, 2> PLAYWRIGHT_SHELL = {"chrome-headless-shell-win64/chrome-headless-shell.exe",
                                                              "chrome-headless-shell-win/chrome-headless-shell.exe"};
constexpr char PATH_SEPARATOR = ';';
#endif

// `std::env::split_paths`.
std::vector<Path> split_paths(std::string_view value) {
    std::vector<Path> out;
    std::size_t start = 0;
    for (;;) {
        const auto end = value.find(PATH_SEPARATOR, start);
        out.emplace_back(std::string(value.substr(start, end == std::string_view::npos ? end : end - start)));
        if (end == std::string_view::npos) break;
        start = end + 1;
    }
    return out;
}

// Fixed install locations to probe, best first.
std::vector<Path> fixed_paths([[maybe_unused]] const Env& env) {
    std::vector<Path> out;
#if !defined(_WIN32)
    for (auto p : FIXED_PATHS) out.emplace_back(p);
#else
    for (const auto& [variable, relative] : WINDOWS_INSTALLS) {
        if (auto root = env(variable); root && !root->empty()) out.push_back(Path(*root) / relative);
    }
#endif
    return out;
}

// Playwright installs `chromium-<rev>/<platform dir>/chrome` and
// `chromium_headless_shell-<rev>/<platform dir>/chrome-headless-shell`. Full
// Chromium comes before the headless shell, higher revisions first.
std::vector<Path> playwright_browsers(const Path& cache, bool headed) {
    std::error_code ec;
    std::filesystem::directory_iterator it(cache, ec);
    if (ec) return {};
    std::vector<std::tuple<std::uint8_t, std::uint64_t, Path>> found;
    for (const auto& entry : it) {
        const auto name = entry.path().filename().string();
        const auto dash = name.rfind('-');
        if (dash == std::string::npos) continue;
        const std::string_view kind(name.data(), dash);
        const std::string_view rev(name.data() + dash + 1, name.size() - dash - 1);
        std::uint64_t revision = 0;
        const auto [end, err] = std::from_chars(rev.data(), rev.data() + rev.size(), revision);
        if (rev.empty() || err != std::errc{} || end != rev.data() + rev.size()) continue;
        std::uint8_t rank = 0;
        std::span<const std::string_view> relatives;
        if (kind == "chromium") {
            rank = 0;
            relatives = PLAYWRIGHT_CHROMIUM;
        } else if (kind == "chromium_headless_shell" && !headed) {
            rank = 1;
            relatives = PLAYWRIGHT_SHELL;
        } else {
            continue;
        }
        for (auto relative : relatives) {
            auto exe = entry.path() / relative;
            if (exec::is_executable(exe)) {
                found.emplace_back(rank, revision, std::move(exe));
                break;
            }
        }
    }
    std::ranges::stable_sort(found, [](const auto& a, const auto& b) {
        if (std::get<0>(a) != std::get<0>(b)) return std::get<0>(a) < std::get<0>(b);
        return std::get<1>(a) > std::get<1>(b);
    });
    std::vector<Path> out;
    for (auto& f : found) out.push_back(std::move(std::get<2>(f)));
    return out;
}

std::optional<std::string> process_env(std::string_view name) {
    if (const char* v = std::getenv(std::string(name).c_str())) return std::string(v);
    return std::nullopt;
}

} // namespace

std::vector<Path> chrome_candidates_with(std::optional<std::string_view> path_var, const std::optional<Path>& home,
                                         bool headed, const Env& env) {
    std::vector<Path> found;
    const auto dirs = path_var ? split_paths(*path_var) : std::vector<Path>{};
    const auto pathext = env("PATHEXT");
    const std::optional<std::string_view> ext = pathext ? std::optional<std::string_view>(*pathext) : std::nullopt;
    for (auto name : PATH_NAMES) {
        if (headed && name == "chrome-headless-shell") continue;
        // A bare-name search per directory: a plain file called `chromium`
        // is no browser on Windows, where only `PATHEXT` names run.
        for (const auto& dir : dirs) {
            const auto native = dir.string();
            if (auto hit = exec::which_in(Path(name), std::string_view(native), ext)) found.push_back(std::move(*hit));
        }
    }
    for (auto& p : fixed_paths(env)) {
        if (exec::is_executable(p)) found.push_back(std::move(p));
    }
    if (home) {
        for (auto& p : playwright_browsers(*home / ".cache/ms-playwright", headed)) found.push_back(std::move(p));
    }
#if defined(_WIN32)
    if (auto local = env("LOCALAPPDATA"); local && !local->empty()) {
        for (auto& p : playwright_browsers(Path(*local) / "ms-playwright", headed)) found.push_back(std::move(p));
    }
#endif
    std::set<Path> seen;
    std::erase_if(found, [&](const Path& p) { return !seen.insert(p).second; });
    return found;
}

std::vector<Path> chrome_candidates(std::optional<std::string_view> path_var, const std::optional<Path>& home, bool headed) {
    return chrome_candidates_with(path_var, home, headed, process_env);
}

std::optional<Path> find_chrome(std::optional<std::string_view> path_var, const std::optional<Path>& home, bool headed) {
    auto all = chrome_candidates(path_var, home, headed);
    if (all.empty()) return std::nullopt;
    return std::move(all.front());
}

std::optional<Path> find_xvfb(std::optional<std::string_view> path_var) {
#if !defined(__linux__)
    (void)path_var;
    return std::nullopt;
#else
    if (!path_var) return std::nullopt;
    for (const auto& dir : split_paths(*path_var)) {
        if (auto hit = exec::which(dir / "Xvfb")) return hit;
    }
    return std::nullopt;
#endif
}

Result<BrowserConfig> BrowserConfig::discover() {
    const auto path = process_env("PATH");
    const auto home = loa::runtime::fs::home();
    auto found = find_chrome(path ? std::optional<std::string_view>(*path) : std::nullopt, home, false);
    if (!found) return std::unexpected(BrowserError::no_browser());
    return BrowserConfig::make(std::move(*found));
}

} // namespace loa::surfaces::browser
