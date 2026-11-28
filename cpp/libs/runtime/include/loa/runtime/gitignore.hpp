#pragma once

// Gitignore matching as the `ignore` 0.4 crate does it: per-file rules where the last
// matching line wins (`!` re-includes, a trailing `/` matches directories only, `**`
// spans directories, a pattern without an inner `/` matches at any depth), and nested
// files where the deepest directory with a match decides.

#include <expected>
#include <filesystem>
#include <memory>
#include <string>
#include <string_view>
#include <vector>

#include "loa/runtime/error.hpp"

namespace loa::runtime::gitignore {

using Path = std::filesystem::path;

enum class Match { none, ignore, whitelist };

// The rules of one ignore file, matched relative to `root` (the file's directory).
class Gitignore {
  public:
    Gitignore() = default;
    // Lines that fail to compile (an unclosed `[`) are skipped, as `ignore` does.
    [[nodiscard]] static Gitignore parse(Path root, std::string_view text);
    // An empty matcher when `file` is missing or unreadable.
    [[nodiscard]] static Gitignore from_file(Path root, const Path& file);

    [[nodiscard]] const Path& root() const noexcept { return root_; }
    [[nodiscard]] bool empty() const noexcept { return rules_.empty(); }

    // `path` is absolute under `root` or relative to it.
    [[nodiscard]] Match matched(const Path& path, bool is_dir) const;

    struct Rule;

  private:
    Path root_;
    std::vector<std::shared_ptr<const Rule>> rules_;
};

// The `.ignore` and `.gitignore` files of a directory and its ancestors (gitignores stop
// at the enclosing repository, the first ancestor holding `.git`, whose
// `.git/info/exclude` also applies). `.ignore` matches take precedence over gitignore ones.
class Matcher {
  public:
    // Loads the files of `dir` and every ancestor.
    [[nodiscard]] static Matcher for_dir(const Path& dir);
    // This matcher plus the files of `child`, a subdirectory of the deepest directory.
    [[nodiscard]] Matcher descend(const Path& child) const;

    [[nodiscard]] Match matched(const Path& path, bool is_dir) const;
    [[nodiscard]] bool is_ignored(const Path& path, bool is_dir) const { return matched(path, is_dir) == Match::ignore; }

  private:
    struct Level {
        Gitignore ignore;
        Gitignore git;
        bool has_git = false;
    };
    static Level load(const Path& dir);

    // Outermost first.
    std::vector<Level> levels_;
    Gitignore exclude_;
};

struct Entry {
    std::string name;
    bool is_dir = false;
    friend bool operator==(const Entry&, const Entry&) = default;
};

// The direct children of `dir` that the ignore files of `dir` and its ancestors keep,
// dotfiles included and `.git` hidden, in directory order. A symlink counts as a directory
// when it points at one.
[[nodiscard]] std::expected<std::vector<Entry>, Error> list_dir(const Path& dir);

} // namespace loa::runtime::gitignore
