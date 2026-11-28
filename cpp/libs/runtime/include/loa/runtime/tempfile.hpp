#pragma once

// RAII temporary files and directories over mkstemp and mkdtemp (the Rust `tempfile` crate).

#include <expected>
#include <filesystem>
#include <string_view>

#include "loa/runtime/error.hpp"

namespace loa::runtime {

// A file created with mode 0600 and a random `.tmpXXXXXX` name; closed and
// unlinked on destruction unless persisted.
class TempFile {
  public:
    // In `dir`, or the system temp directory when empty.
    [[nodiscard]] static std::expected<TempFile, Error> create(const std::filesystem::path& dir = {});

    TempFile(TempFile&& other) noexcept;
    TempFile& operator=(TempFile&& other) noexcept;
    TempFile(const TempFile&) = delete;
    TempFile& operator=(const TempFile&) = delete;
    ~TempFile();

    [[nodiscard]] const std::filesystem::path& path() const noexcept { return path_; }
    // The open read/write descriptor; owned by this object.
    [[nodiscard]] int fd() const noexcept { return fd_; }

    [[nodiscard]] std::expected<void, Error> write_all(std::string_view bytes);
    [[nodiscard]] std::expected<void, Error> sync();
    // Renames the file to `target` (replacing it) and closes it; nothing is removed afterwards.
    [[nodiscard]] std::expected<void, Error> persist(const std::filesystem::path& target);

  private:
    TempFile(int fd, std::filesystem::path path) noexcept : fd_(fd), path_(std::move(path)) {}
    void reset() noexcept;

    int fd_ = -1;
    std::filesystem::path path_;
};

// A directory created with mode 0700; removed recursively on destruction.
class TempDir {
  public:
    // In `dir`, or the system temp directory when empty.
    [[nodiscard]] static std::expected<TempDir, Error> create(const std::filesystem::path& dir = {});

    TempDir(TempDir&& other) noexcept;
    TempDir& operator=(TempDir&& other) noexcept;
    TempDir(const TempDir&) = delete;
    TempDir& operator=(const TempDir&) = delete;
    ~TempDir();

    [[nodiscard]] const std::filesystem::path& path() const noexcept { return path_; }

  private:
    explicit TempDir(std::filesystem::path path) noexcept : path_(std::move(path)) {}
    void reset() noexcept;

    std::filesystem::path path_;
};

} // namespace loa::runtime
