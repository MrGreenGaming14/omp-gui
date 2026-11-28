#include "loa/runtime/tempfile.hpp"

#include <fcntl.h>
#include <unistd.h>

#include <cstdio>
#include <string>
#include <system_error>
#include <utility>

#include "sys_error.hpp"

namespace loa::runtime {

namespace fs = std::filesystem;

namespace {

fs::path base_dir(const fs::path& dir) {
    if (!dir.empty()) return dir;
    std::error_code ec;
    auto tmp = fs::temp_directory_path(ec);
    return ec ? fs::path("/tmp") : tmp;
}

} // namespace

std::expected<TempFile, Error> TempFile::create(const fs::path& dir) {
    std::string tmpl = (base_dir(dir) / ".tmpXXXXXX").string();
    const int fd = ::mkostemp(tmpl.data(), O_CLOEXEC);
    if (fd < 0) return std::unexpected(detail::os_error(errno, "creating a temp file in " + base_dir(dir).string()));
    return TempFile(fd, fs::path(tmpl));
}

TempFile::TempFile(TempFile&& other) noexcept
    : fd_(std::exchange(other.fd_, -1)), path_(std::exchange(other.path_, {})) {}

TempFile& TempFile::operator=(TempFile&& other) noexcept {
    if (this != &other) {
        reset();
        fd_ = std::exchange(other.fd_, -1);
        path_ = std::exchange(other.path_, {});
    }
    return *this;
}

TempFile::~TempFile() { reset(); }

void TempFile::reset() noexcept {
    if (fd_ >= 0) ::close(fd_);
    if (!path_.empty()) ::unlink(path_.c_str());
    fd_ = -1;
    path_.clear();
}

std::expected<void, Error> TempFile::write_all(std::string_view bytes) {
    while (!bytes.empty()) {
        const auto n = ::write(fd_, bytes.data(), bytes.size());
        if (n < 0) {
            if (errno == EINTR) continue;
            return std::unexpected(detail::os_error(errno, "writing " + path_.string()));
        }
        bytes.remove_prefix(static_cast<std::size_t>(n));
    }
    return {};
}

std::expected<void, Error> TempFile::sync() {
    if (::fsync(fd_) != 0) return std::unexpected(detail::os_error(errno, "syncing " + path_.string()));
    return {};
}

std::expected<void, Error> TempFile::persist(const fs::path& target) {
    if (::rename(path_.c_str(), target.c_str()) != 0)
        return std::unexpected(detail::os_error(errno, "renaming " + path_.string() + " to " + target.string()));
    ::close(fd_);
    fd_ = -1;
    path_.clear();
    return {};
}

std::expected<TempDir, Error> TempDir::create(const fs::path& dir) {
    std::string tmpl = (base_dir(dir) / ".tmpXXXXXX").string();
    if (::mkdtemp(tmpl.data()) == nullptr)
        return std::unexpected(detail::os_error(errno, "creating a temp dir in " + base_dir(dir).string()));
    return TempDir(fs::path(tmpl));
}

TempDir::TempDir(TempDir&& other) noexcept : path_(std::exchange(other.path_, {})) {}

TempDir& TempDir::operator=(TempDir&& other) noexcept {
    if (this != &other) {
        reset();
        path_ = std::exchange(other.path_, {});
    }
    return *this;
}

TempDir::~TempDir() { reset(); }

void TempDir::reset() noexcept {
    if (path_.empty()) return;
    std::error_code ec;
    fs::remove_all(path_, ec);
    path_.clear();
}

} // namespace loa::runtime
