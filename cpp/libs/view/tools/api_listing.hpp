#pragma once

// The public API snapshot of `loa::view` (port of the Rust `tests/api.rs`): lists
// the public declarations of `include/loa/view/*.hpp` and compares the listing with
// `api.txt`, whose first line is `version = <loa::view::VERSION>`.
//
// - The listing changed and `VERSION` still equals the header: fails, asking for a bump.
// - `VERSION` was bumped: `UPDATE_VIEW_API=1 ctest -R loa_view_api` rewrites `api.txt`;
//   without it the check fails as stale. A removed or changed line needs a major bump,
//   additions alone a minor one.
//
// Listing format: blocks sorted by their first line. A type is `struct|class|enum
// <qualified name>` followed by its public members indented by two spaces (nested
// types' members by four); any other declaration is one `<namespace>: <declaration>`
// line. Whitespace is collapsed and comments are dropped.

#include <filesystem>
#include <string>
#include <string_view>

namespace loa::view::api {

// The listing of every header directly in `include_dir`.
[[nodiscard]] std::string render(const std::filesystem::path& include_dir);

struct GateResult {
    bool ok = false;
    // `api.txt` must be rewritten as `version = <version>\n<rendered>`.
    bool write = false;
    std::string message;
};

// Checks `rendered` against the saved `api.txt` text for `version`; `update` is `UPDATE_VIEW_API=1`.
[[nodiscard]] GateResult gate(std::string_view saved, std::string_view rendered, std::string_view version, bool update);

} // namespace loa::view::api
