#pragma once

// Native harness sessions on this machine, for the import picker: each harness's
// discovery, plus recognizing a picked file.

#include <filesystem>
#include <optional>
#include <string_view>
#include <vector>

#include "loa/harness/kit/native.hpp"

namespace loa::harness::native {

using loa::harness::kit::NativeSession;

// Harness ids, as `loa::contracts::Adapter::harness` returns them.
inline constexpr std::string_view OMP = "omp";
inline constexpr std::string_view CLAUDE = "claude-code";
inline constexpr std::string_view CODEX = "codex";
inline constexpr std::string_view OPENCODE = "opencode";

// omp sessions: `.omp/agent/sessions/<project>/*.jsonl`, titled by a leading `title` line.
[[nodiscard]] std::vector<NativeSession> discover_omp(const std::filesystem::path& home);
// Claude Code transcripts: `.claude/projects/<project>/*.jsonl`.
[[nodiscard]] std::vector<NativeSession> discover_claude(const std::filesystem::path& home);
// Codex rollouts: `$CODEX_HOME` (else `home/.codex`) `/sessions/YYYY/MM/DD/rollout-*.jsonl`.
[[nodiscard]] std::vector<NativeSession> discover_codex(const std::filesystem::path& home);

// Every native session under `home`, newest first. OpenCode sessions are picked by path.
[[nodiscard]] std::vector<NativeSession> discover(const std::filesystem::path& home);

// The harness a session file belongs to, from where it is and how it starts.
[[nodiscard]] std::optional<std::string_view> harness_of(const std::filesystem::path& path);

} // namespace loa::harness::native
