#pragma once

// Copies workspaces through the stub, as tar archives, when the provider does not mount a host
// directory (Rust `loa_sandbox::sync`). Git administration files stay on the host. Every member
// name of an archive from the guest is checked on its own before anything is unpacked, so an
// archive the host can't take changes nothing.

#include <filesystem>
#include <optional>
#include <string>
#include <string_view>
#include <vector>

#include "loa/sandbox/channel.hpp"

namespace loa::sandbox {

class StubClient;

namespace sync {

// Uploads every file but the host's Git administration data to an empty stub root. The guest
// needs a POSIX shell with `cat` and `tar`. Blocks the calling thread.
[[nodiscard]] Result<> copy_in(const StubClient& client, const std::filesystem::path& workspace);
// Replaces `workspace`, but for its Git data, with the stub root: brings back additions, edits
// and removals. Blocks the calling thread.
[[nodiscard]] Result<> copy_out(const StubClient& client, const std::filesystem::path& workspace);

namespace detail {
// The names no platform takes as a single path component.
[[nodiscard]] Result<> check_name(std::string_view name);
// Why Windows can't hold a file called `name` as the guest named it, if so.
[[nodiscard]] std::optional<std::string_view> windows_name_problem(std::string_view name);
// The checks on one entry of a guest directory that apply on this host.
[[nodiscard]] Result<> check_guest_name(std::string_view name, bool top);
// Windows file names ignore case; checked on Windows only.
[[nodiscard]] Result<> check_collisions(const std::vector<std::string>& names, std::string_view dir);
// The checked names of an archive member's path; `./` and a trailing `/` are dropped.
[[nodiscard]] Result<std::vector<std::string>> member_names(std::string_view path);
// Checks every member of an archive from the guest before any of it reaches the host.
[[nodiscard]] Result<> check_archive(std::string_view archive);
// Applies an untrusted archive to `workspace` through a staging directory; `.git` never reaches it.
[[nodiscard]] Result<> unpack(const std::filesystem::path& workspace, std::string_view archive);
// Runs the host's tar in `dir` with `input` on its stdin; its stdout.
[[nodiscard]] Result<std::string> tar(const std::filesystem::path& dir, const std::vector<std::string>& args, std::string_view input);
} // namespace detail

} // namespace sync
} // namespace loa::sandbox
