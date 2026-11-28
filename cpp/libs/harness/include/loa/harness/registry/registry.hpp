#pragma once

// The harness registry: one `HarnessFactory` per harness, so the app builds adapters and
// asks about harness behavior by id instead of matching on names (port of `registry.rs`).

#include <array>
#include <filesystem>
#include <functional>
#include <memory>
#include <optional>
#include <span>
#include <string>
#include <string_view>
#include <utility>
#include <vector>

#include "loa/contracts/account.hpp"

namespace loa::harness::registry {

// What the factories need from the app.
struct HarnessEnv {
    // This node's key, written to `<session node>`.
    std::string node;
    // Where omp keeps its state for the app.
    std::filesystem::path omp_dir;
};

// omp's `--approval-mode` values; the first is what app sessions start on.
inline constexpr std::array<std::string_view, 4> OMP_APPROVAL_MODES{"always-ask", "always-ask-cached", "write", "yolo"};

// An ACP agent known by command line.
struct AcpAgent {
    std::string_view name;
    std::span<const std::string_view> args;
};
inline constexpr std::array<std::string_view, 1> ACP_OPENCODE_ARGS{"acp"};
inline constexpr std::array<std::string_view, 1> ACP_GEMINI_ARGS{"--experimental-acp"};
// ACP agents known by command line, tried in order.
inline constexpr std::array<AcpAgent, 2> ACP_AGENTS{AcpAgent{"opencode", ACP_OPENCODE_ARGS},
                                                    AcpAgent{"gemini", ACP_GEMINI_ARGS}};

// The ACP agent to run.
struct AcpLaunch {
    std::filesystem::path program;
    std::vector<std::string> args;
    // Set when no known agent is on `PATH`: what the adapter reports as missing.
    std::optional<std::string> missing_hint;
    friend bool operator==(const AcpLaunch&, const AcpLaunch&) = default;
};

// The ACP agent `exe` (with a known agent's arguments when its file stem matches), else the
// first known agent on `PATH` (`path_var`, the process `PATH` when `nullopt`), else the first
// known one with a missing hint.
[[nodiscard]] AcpLaunch acp_launch(std::optional<std::filesystem::path> exe,
                                   std::optional<std::string_view> path_var = std::nullopt);

// `$variable` when set and non-empty, else `$HOME/default`, else `default`.
[[nodiscard]] std::filesystem::path config_dir(const char* variable, std::string_view fallback);

// How one harness builds its adapter and account; the parts each adapter library supplies.
struct HarnessBuild {
    std::function<std::shared_ptr<loa::contracts::Adapter>(const HarnessEnv&, std::optional<std::filesystem::path>)> adapter;
    std::function<std::shared_ptr<loa::contracts::HarnessAccount>(const HarnessEnv&, std::optional<std::filesystem::path>)> account;
};

// One registry entry: the harness's description and its build strategy.
struct HarnessEntry {
    std::string_view id;
    std::string_view label;
    std::string_view icon;
    std::string_view program;
    loa::contracts::HarnessTraits traits;
};

// The entries of every harness in display order: omp, claude-code, codex, opencode, acp, omp2.
[[nodiscard]] std::span<const HarnessEntry> entries() noexcept;

// The registered builds, by harness id. Each adapter library registers its own
// (`register_build`) when the app wires it in.
void register_build(std::string_view id, HarnessBuild build);

// The factories of every harness in display order. A factory whose adapter has not been
// registered builds nothing (`build` and `account` return null).
[[nodiscard]] std::vector<std::shared_ptr<loa::contracts::HarnessFactory>> factories(const HarnessEnv& env);

} // namespace loa::harness::registry

namespace loa::harness {
using registry::factories;
using registry::HarnessEnv;
} // namespace loa::harness
