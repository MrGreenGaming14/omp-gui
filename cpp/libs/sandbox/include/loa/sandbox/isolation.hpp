#pragma once

// Isolated checkouts and their changes relative to the opening snapshot (Rust
// `loa_sandbox::isolation`). A `CheckoutBackend` makes the checkout: a `git worktree`, or a
// clone where worktrees can't be made. No Git command here changes the source index, HEAD or
// working tree; the checkout owns its own index.

#include <cstddef>
#include <filesystem>
#include <string>
#include <string_view>
#include <vector>

#include "loa/sandbox/channel.hpp"

namespace loa::sandbox {

// Makes and removes checkouts of a repository's HEAD.
class CheckoutBackend {
  public:
    virtual ~CheckoutBackend() = default;
    [[nodiscard]] virtual std::string_view name() const = 0;
    // Makes the checkout `name` of `repo` and returns its root.
    [[nodiscard]] virtual Result<std::filesystem::path> create(const std::filesystem::path& repo, std::string_view name) const = 0;
    [[nodiscard]] virtual Result<> remove(const std::filesystem::path& repo, std::string_view name,
                                          const std::filesystem::path& checkout) const = 0;
};

class GitWorktree final : public CheckoutBackend {
  public:
    [[nodiscard]] std::string_view name() const override { return "git worktree"; }
    [[nodiscard]] Result<std::filesystem::path> create(const std::filesystem::path& repo, std::string_view name) const override;
    [[nodiscard]] Result<> remove(const std::filesystem::path& repo, std::string_view name,
                                  const std::filesystem::path& checkout) const override;
};

// A clone has a private Git directory, so it can be made where a linked worktree can't.
class GitClone final : public CheckoutBackend {
  public:
    [[nodiscard]] std::string_view name() const override { return "git clone"; }
    [[nodiscard]] Result<std::filesystem::path> create(const std::filesystem::path& repo, std::string_view name) const override;
    [[nodiscard]] Result<> remove(const std::filesystem::path& repo, std::string_view name,
                                  const std::filesystem::path& checkout) const override;
};

// The isolation `IsolatedCheckout::create` uses on this machine.
[[nodiscard]] std::string_view backend_name();

// An isolated checkout, removed when destroyed. `patch` includes unstaged, staged and new files
// relative to its opening snapshot.
class IsolatedCheckout {
  public:
    [[nodiscard]] static Result<IsolatedCheckout> create(const std::filesystem::path& workspace);

    IsolatedCheckout(IsolatedCheckout&& other) noexcept;
    IsolatedCheckout& operator=(IsolatedCheckout&&) = delete;
    IsolatedCheckout(const IsolatedCheckout&) = delete;
    IsolatedCheckout& operator=(const IsolatedCheckout&) = delete;
    ~IsolatedCheckout();

    [[nodiscard]] const std::filesystem::path& path() const noexcept { return path_; }
    // A binary capable unified Git patch of the changes since the checkout was made.
    [[nodiscard]] Result<std::string> patch() const;

  private:
    IsolatedCheckout() = default;
    [[nodiscard]] Result<std::string> stage(const std::vector<std::string>& args) const;
    [[nodiscard]] Result<> remove() const;

    std::filesystem::path repo_;
    std::string name_;
    std::filesystem::path root_;
    std::filesystem::path path_;
    std::string baseline_;
    const CheckoutBackend* backend_ = nullptr;
};

// Applies a patch from `IsolatedCheckout::patch` to the repository that holds `workspace`.
// Checks the whole patch first, so a patch that doesn't apply cleanly changes nothing.
[[nodiscard]] Result<> apply_patch(const std::filesystem::path& workspace, std::string_view patch);

// One file a patch touches.
struct PatchFile {
    std::string path;
    std::size_t added = 0;
    std::size_t removed = 0;
    bool binary = false;
    friend bool operator==(const PatchFile&, const PatchFile&) = default;
};

// The files of a unified Git patch with their added and removed line counts.
[[nodiscard]] std::vector<PatchFile> patch_files(std::string_view patch);

namespace detail {
// Removes a file, symlink or directory tree; a missing path is fine.
[[nodiscard]] Result<> remove_entry(const std::filesystem::path& path);
} // namespace detail

} // namespace loa::sandbox
