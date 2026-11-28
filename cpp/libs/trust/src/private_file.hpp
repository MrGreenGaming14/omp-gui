#pragma once

// Windows only: writes a key file with a protected owner-only DACL; never
// writes a key to an inherited ACL (Rust `private_file.rs`).

#if defined(_WIN32)

#include <cstdint>
#include <expected>
#include <filesystem>
#include <span>
#include <string>

namespace loa::trust::detail {

// Creates `.node-key-*` beside `path` with the private DACL, writes and syncs
// `secret`, then atomically replaces `path` with it.
[[nodiscard]] std::expected<void, std::string> store_private_windows(const std::filesystem::path& path,
                                                                     std::span<const std::uint8_t> secret);

} // namespace loa::trust::detail

#endif
