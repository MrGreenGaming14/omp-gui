#pragma once

// omp's JSONL session files. Import maps the active path (leaf = last entry, walked up by
// `parentId`) onto the DOM and keeps every entry's exact line, so export reproduces the file:
// elements standing for an entry carry the line in `x-omp-entry` and its position in
// `x-omp-line`, `<ext harness="omp">` holds other lines as text, entries off the active path
// become `<ext type="branch">`. Old files are migrated to version 3 like omp does; malformed
// lines are skipped. DOM content added after an import is appended as new entries.

#include <cstdint>
#include <expected>
#include <filesystem>
#include <functional>
#include <optional>
#include <string>
#include <string_view>
#include <vector>

#include "loa/contracts/harness.hpp"
#include "loa/core/dom.hpp"
#include "loa/core/sink.hpp"
#include "loa/harness/kit/native.hpp"
#include "loa/runtime/error.hpp"
#include "loa/runtime/json.hpp"

namespace loa::harness::omp::native {

// Root attribute naming the native file a session resumes from.
inline constexpr std::string_view X_SESSION_FILE = "x-omp-session-file";

// Reads an omp session file into the empty `sink`; `node` owns the session.
[[nodiscard]] loa::contracts::Result<> import(const std::filesystem::path& path, loa::core::PatchSink& sink,
                                              std::string_view node);
[[nodiscard]] loa::contracts::Result<> import_str(std::string_view text, loa::core::PatchSink& sink,
                                                  std::string_view node);
[[nodiscard]] loa::contracts::Result<std::string> export_string(const loa::core::Dom& dom);

// Text of a message `content`: the string, or the text blocks joined by newlines.
[[nodiscard]] std::string content_text(const loa::runtime::json::Value* content);

// The omp `NativeCodec`; imports are owned by the local node (`""`).
class OmpNative final : public loa::harness::kit::NativeCodec {
  public:
    [[nodiscard]] loa::contracts::Result<> import(const std::filesystem::path& path,
                                                  loa::core::PatchSink& sink) const override;
    [[nodiscard]] loa::contracts::Result<> export_(const loa::core::Dom& dom,
                                                   const std::filesystem::path& out) const override;
    // `~/.omp/agent/sessions/<project>/<name>.jsonl`, titled by their `title` line.
    [[nodiscard]] std::vector<loa::harness::kit::NativeSession> discover(const std::filesystem::path& home) const override;
};

// Metadata for one native omp session matching a project folder.
struct ProjectSession {
    std::string id;
    std::filesystem::path path;
    std::string title;
    // File modification timestamp in Unix milliseconds.
    std::int64_t modified = 0;

    friend bool operator==(const ProjectSession&, const ProjectSession&) = default;
};

// Lists native omp sessions under `<agent_dir>/sessions/<project>/*.jsonl` whose header `cwd`
// canonicalizes to `folder`, sorted newest first with deterministic tie-breaking and
// deduplicated by native session `id`.
[[nodiscard]] std::expected<std::vector<ProjectSession>, loa::Error> project_history(const std::filesystem::path& agent_dir,
                                                                                     const std::filesystem::path& folder);

// Inspects a single omp session file at `path` (including copied paths) and returns its metadata
// if its header is valid and its `cwd` canonicalizes to `folder`.
[[nodiscard]] std::expected<std::optional<ProjectSession>, loa::Error> project_session(const std::filesystem::path& path,
                                                                                       const std::filesystem::path& folder);

// Deletes the native omp session `id` that `project_history` lists for `folder`: for every
// session file with that id, first its same-stem sidecar directory (artifacts and subagent
// sessions; a symlink there is left alone), then the file. A sidecar that can't be removed stops
// the delete before its file goes, so the session stays listed and can be deleted again. Errors
// name the path that failed; an `id` not listed for `folder` is `ErrorCode::not_found`.
[[nodiscard]] std::expected<void, loa::Error> delete_project_session(const std::filesystem::path& agent_dir,
                                                                     const std::filesystem::path& folder,
                                                                     std::string_view id);

namespace detail {

// Every session under `<agent_dir>/sessions/<project>/*.jsonl` whose header `cwd` canonicalizes to `folder`.
[[nodiscard]] std::expected<std::vector<ProjectSession>, loa::Error> matching_sessions(const std::filesystem::path& agent_dir,
                                                                                       const std::filesystem::path& folder);

// Removes each session's sidecar directory with `remove_dir`, then its file; tests pass a `remove_dir` that fails.
[[nodiscard]] std::expected<void, loa::Error> remove_sessions(
    const std::vector<ProjectSession>& sessions,
    const std::function<std::expected<void, loa::Error>(const std::filesystem::path&)>& remove_dir);

} // namespace detail

} // namespace loa::harness::omp::native
