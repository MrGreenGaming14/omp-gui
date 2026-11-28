#pragma once

// The typed config schema (port of `loa_settings::app::schema`). Default member
// values are the serde defaults of the Rust structs.

#include <chrono>
#include <cstdint>
#include <filesystem>
#include <map>
#include <optional>
#include <set>
#include <string>
#include <string_view>
#include <variant>
#include <vector>

#include "loa/contracts/harness.hpp"
#include "loa/core/schema.hpp"
#include "loa/settings/app/error.hpp"
#include "loa/settings/app/toml.hpp"

namespace loa::settings::app {

// Schema version this build reads and writes.
inline constexpr std::uint32_t CURRENT_VERSION = 1;
// The p2p port (UDP for iroh, TCP fallback). The one definition; p2p re-exports it.
inline constexpr std::uint16_t DEFAULT_PORT = 31415;

// A key the schema doesn't know, by its table path, kept for the next save.
// The schema has no arrays of tables, so a path is table keys only.
struct UnknownKey {
    std::vector<std::string> path;
    toml::Value value;

    // `network.x-mtu`.
    [[nodiscard]] std::string dotted() const;
    friend bool operator==(const UnknownKey&, const UnknownKey&) = default;
};

struct ActiveProject {
    std::string name;
    std::filesystem::path cwd;
    friend bool operator==(const ActiveProject&, const ActiveProject&) = default;
};

// A theme by registry name or file path; the legacy `system`, `dark` and `light` are registry names.
struct ThemeRef {
    static constexpr std::string_view SYSTEM = "system";
    std::string name{SYSTEM};

    [[nodiscard]] const std::string& as_str() const noexcept { return name; }
    friend bool operator==(const ThemeRef&, const ThemeRef&) = default;
};

// The UI theme plus token overrides; config names a theme, never its colors. Written as
// `theme = "<name>"`, or as a `[theme]` table with `name` and the overrides.
struct Appearance {
    ThemeRef theme;
    toml::Table overrides;
    friend bool operator==(const Appearance&, const Appearance&) = default;
};

// Headless Chrome's own screencast, or headed Chrome on an Xvfb display captured raw.
enum class BrowserCapture : std::uint8_t { Screencast, VirtualDisplay };
[[nodiscard]] std::string_view to_string(BrowserCapture c) noexcept;

// How live surfaces stream: H.264/HEVC video, and where browser frames come from.
struct SurfaceSettings {
    bool video = true;
    BrowserCapture browser_capture = BrowserCapture::Screencast;
    friend bool operator==(const SurfaceSettings&, const SurfaceSettings&) = default;
};

// The harness new sessions use, executable overrides by harness name, and omp's approval mode.
struct HarnessSettings {
    std::string default_ = "omp"; // `default` in the file
    std::map<std::string, std::filesystem::path> executables;
    std::optional<std::string> approval_mode;

    [[nodiscard]] std::optional<std::filesystem::path> executable(std::string_view harness) const;
    friend bool operator==(const HarnessSettings&, const HarnessSettings&) = default;
};

// Relay used when no direct path to a peer exists: `"default"` (iroh's), `"off"` or an `http(s)` URL.
struct Relay {
    struct Default {
        friend bool operator==(Default, Default) = default;
    };
    struct Off {
        friend bool operator==(Off, Off) = default;
    };
    struct Url {
        std::string url;
        friend bool operator==(const Url&, const Url&) = default;
    };
    std::variant<Default, Off, Url> value;

    // The file form: "default", "off" or the URL.
    [[nodiscard]] std::string to_string() const;
    [[nodiscard]] static Relay from_string(std::string text);
    friend bool operator==(const Relay&, const Relay&) = default;
};

// How peers are found without typing their key: mDNS and Tailscale MagicDNS.
struct Discovery {
    bool mdns = true;
    bool tailscale = true;
    friend bool operator==(const Discovery&, const Discovery&) = default;
};

struct NetworkSettings {
    std::uint16_t port = DEFAULT_PORT;
    Relay relay;
    Discovery discovery;
    friend bool operator==(const NetworkSettings&, const NetworkSettings&) = default;
};

// Update checks; `pins` holds a version per component, `trusted_keys` hex ed25519 release keys.
struct UpdaterSettings {
    std::uint64_t check_interval_secs = 6 * 60 * 60;
    std::map<std::string, std::string> pins;
    std::optional<std::string> github_token;
    std::vector<std::string> trusted_keys;

    [[nodiscard]] std::chrono::seconds check_interval() const { return std::chrono::seconds(check_interval_secs); }
    friend bool operator==(const UpdaterSettings&, const UpdaterSettings&) = default;
};

// What to answer when a decision has waited `timeout_secs`; `reason` is recorded with it.
struct DecisionDefault {
    std::uint64_t timeout_secs = 0;
    std::string answer;
    std::optional<std::string> reason;
    friend bool operator==(const DecisionDefault&, const DecisionDefault&) = default;
};

// Default answers for decisions nobody answers in time; a kind without one waits forever.
struct UnattendedSettings {
    std::optional<DecisionDefault> approval;
    std::optional<DecisionDefault> question;
    std::optional<DecisionDefault> plan;
    std::optional<DecisionDefault> form;

    [[nodiscard]] const DecisionDefault* for_kind(loa::core::DecisionKind kind) const noexcept;
    friend bool operator==(const UnattendedSettings&, const UnattendedSettings&) = default;
};

// A registered MCP server; a disabled one stays registered but isn't started.
struct McpServer {
    loa::contracts::McpTransport transport;
    bool enabled = true;

    [[nodiscard]] loa::contracts::McpServerSpec spec(std::string_view name) const;
    friend bool operator==(const McpServer&, const McpServer&) = default;
};

// An SSH host (name, address or `~/.ssh/config` alias) and what the agent may do there:
// touch the absolute `allowed_paths` and below, run commands, skip approval cards.
// The remote library enforces the limits.
struct RemoteHost {
    std::string host;
    std::optional<std::string> user;
    std::optional<std::uint16_t> port;
    std::optional<std::string> proxy_jump;
    std::vector<std::string> allowed_paths;
    bool read_only = true;
    bool commands = false;
    bool auto_approve = false;
    friend bool operator==(const RemoteHost&, const RemoteHost&) = default;
};

// Settings shared by every session of one project: instructions, always-included files,
// the omp role and token budget for session digests, and switched-off tools.
struct ProjectSettings {
    std::string instructions;
    std::vector<std::filesystem::path> pinned_files;
    std::string digest_role = "smol";
    std::uint32_t token_budget = 8000;
    std::set<std::string> disabled_tools;
    // By MCP server name.
    std::map<std::string, std::set<std::string>> disabled_mcp_tools;

    [[nodiscard]] bool mcp_tool_enabled(std::string_view server, std::string_view tool) const;
    friend bool operator==(const ProjectSettings&, const ProjectSettings&) = default;
};

// The whole central config file. `version` is checked by the loader and written by the encoder.
struct Config {
    Appearance appearance; // `theme` in the file
    // Session ids pinned in the sidebar.
    std::set<std::string> pinned;
    std::set<std::string> pinned_projects;
    // Restored at startup while its folder exists.
    std::optional<ActiveProject> active_project;
    // Whether open projects show as tabs across the top.
    bool project_tabs = false;
    HarnessSettings harness;
    NetworkSettings network;
    UpdaterSettings updater;
    UnattendedSettings unattended;
    SurfaceSettings surfaces;
    std::map<std::string, McpServer> mcp_servers;
    // SSH hosts the `remote` MCP server may reach, by name.
    std::map<std::string, RemoteHost> remote_hosts;
    // By project id.
    std::map<std::string, ProjectSettings> projects;
    // Keys the schema doesn't know, written back on save.
    std::vector<UnknownKey> unknown;

    // Settings of project `id`, or the defaults when it has none.
    [[nodiscard]] ProjectSettings project(std::string_view id) const;

    // Adds enabled MCP servers, the project's disabled tools and the harness
    // executable override to `session`, keeping whatever the caller set.
    void apply_to_session(std::string_view harness, loa::contracts::SessionConfig& session) const;

    // Checks the validation rules in order. The error names the offending key.
    [[nodiscard]] Result<> validate() const;

    friend bool operator==(const Config&, const Config&) = default;
};

} // namespace loa::settings::app
