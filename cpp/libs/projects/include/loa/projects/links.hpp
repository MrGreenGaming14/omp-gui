#pragma once

// A project's `.omgui` folder, inside the project's folder: one symlink per directory its sessions run in,
// so the project's work is one folder away wherever it lives. A `.gitignore` of `*` keeps the links out of git.

#include <expected>
#include <filesystem>
#include <set>
#include <string_view>

#include "loa/runtime/error.hpp"

namespace loa::projects::links {

// Name of the links folder inside a project's folder.
inline constexpr std::string_view LINKS_DIR = ".omgui";
inline constexpr std::string_view GITIGNORE = ".gitignore";

// Makes `<folder>/.omgui` hold exactly one link to each of `dirs` that exists. The folder itself gets no
// link. A link is named after its directory, with `-2`, `-3`, … on a clash. Links to other directories are
// removed; anything else in the folder is left alone. With no links left the folder is removed.
[[nodiscard]] std::expected<void, loa::Error> sync(const std::filesystem::path& folder,
                                                   const std::set<std::filesystem::path>& dirs);

// Removes `<folder>/.omgui`'s links and `.gitignore`, then the folder when nothing else is in it.
[[nodiscard]] std::expected<void, loa::Error> remove(const std::filesystem::path& folder);

} // namespace loa::projects::links
