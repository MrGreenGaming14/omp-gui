#include "loa/runtime/fs.hpp"

#include <fcntl.h>
#include <pwd.h>
#include <sys/stat.h>
#include <unistd.h>

#include <algorithm>
#include <climits>
#include <cstdlib>
#include <fstream>
#include <iterator>
#include <vector>

#include "loa/runtime/json.hpp"
#include "loa/runtime/tempfile.hpp"
#include "sys_error.hpp"

namespace loa::runtime::fs {

using detail::os_error;

std::expected<void, Error> write_atomic(const Path& path, std::string_view bytes, std::optional<unsigned> mode) {
    struct stat st {};
    const bool linked = ::lstat(path.c_str(), &st) == 0 && S_ISLNK(st.st_mode);
    Path target = path;
    if (linked) {
        auto real = canonicalize(path);
        if (!real) return std::unexpected(real.error());
        target = *real;
    }
    const Path dir = target.has_parent_path() ? target.parent_path() : Path(".");
    std::error_code ec;
    std::filesystem::create_directories(dir, ec);
    if (ec) return std::unexpected(os_error(ec, "creating " + dir.string()));
    auto tmp = TempFile::create(dir);
    if (!tmp) return std::unexpected(tmp.error());
    if (auto r = tmp->write_all(bytes); !r) return r;
    std::optional<mode_t> perms = mode;
    if (!perms && ::stat(target.c_str(), &st) == 0) perms = st.st_mode & 07777;
    if (perms && ::fchmod(tmp->fd(), *perms) != 0) return std::unexpected(os_error(errno, "setting permissions of " + tmp->path().string()));
    if (auto r = tmp->sync(); !r) return r;
    if (auto r = tmp->persist(target); !r) return r;
    return sync_dir(dir);
}

std::expected<void, Error> write_json_atomic(const Path& path, const json::Value& v) {
    return write_atomic(path, json::dump_pretty(v));
}

std::expected<std::optional<std::string>, Error> read_optional(const Path& path) {
    const int fd = ::open(path.c_str(), O_RDONLY | O_CLOEXEC);
    if (fd < 0) {
        if (errno == ENOENT) return std::optional<std::string>();
        return std::unexpected(os_error(errno, "reading " + path.string()));
    }
    std::string out;
    char buf[65536];
    for (;;) {
        const auto n = ::read(fd, buf, sizeof buf);
        if (n < 0) {
            if (errno == EINTR) continue;
            const int err = errno;
            ::close(fd);
            return std::unexpected(os_error(err, "reading " + path.string()));
        }
        if (n == 0) break;
        out.append(buf, static_cast<std::size_t>(n));
    }
    ::close(fd);
    return std::optional<std::string>(std::move(out));
}

std::expected<void, Error> sync_dir(const Path& dir) {
    const int fd = ::open(dir.c_str(), O_RDONLY | O_CLOEXEC);
    if (fd < 0) return std::unexpected(os_error(errno, "opening " + dir.string()));
    const int rc = ::fsync(fd);
    const int err = errno;
    ::close(fd);
    if (rc != 0) return std::unexpected(os_error(err, "syncing " + dir.string()));
    return {};
}

std::expected<Snapshot, Error> Snapshot::load(const Path& path) {
    auto original = read_optional(path);
    if (!original) return std::unexpected(original.error());
    return Snapshot(path, std::move(*original));
}

std::expected<bool, Error> Snapshot::save(std::string_view bytes) {
    auto current = read_optional(path_);
    if (!current) return std::unexpected(current.error());
    if (*current != original_) return false;
    if (auto r = write_atomic(path_, bytes); !r) return std::unexpected(r.error());
    original_ = std::string(bytes);
    return true;
}

std::expected<Path, Error> confine(const Path& root_in, const Path& rel) {
    const auto escape = [&] { return Error(ErrorCode::permission_denied, rel.string() + " is outside " + root_in.string()); };
    auto root = canonicalize(root_in);
    if (!root) return std::unexpected(root.error());
    Path joined = *root;
    for (const auto& part : rel) {
        const auto s = part.native();
        if (s.empty() || s == ".") continue;
        if (s == ".." || part.has_root_directory() || part.has_root_name()) return std::unexpected(escape());
        joined /= part;
    }
    std::vector<Path> missing;
    Path existing = joined;
    Path real;
    for (;;) {
        auto r = canonicalize(existing);
        if (r) {
            real = std::move(*r);
            break;
        }
        if (r.error().code() != ErrorCode::not_found) return std::unexpected(r.error());
        struct stat st {};
        // A dangling symlink could point anywhere.
        if (::lstat(existing.c_str(), &st) == 0) return std::unexpected(escape());
        missing.push_back(existing.filename());
        if (!existing.has_relative_path()) return std::unexpected(escape());
        existing = existing.parent_path();
    }
    // Component-wise `starts_with`, as Rust's `Path::starts_with`.
    auto [ri, ro] = std::mismatch(real.begin(), real.end(), root->begin(), root->end());
    if (ro != root->end()) return std::unexpected(escape());
    for (auto it = missing.rbegin(); it != missing.rend(); ++it) real /= *it;
    return real;
}

std::optional<Path> home() {
    if (const char* h = std::getenv("HOME"); h != nullptr && *h != '\0') return Path(h);
    std::vector<char> buf(16384);
    passwd pw{};
    passwd* found = nullptr;
    if (::getpwuid_r(::getuid(), &pw, buf.data(), buf.size(), &found) == 0 && found != nullptr && found->pw_dir != nullptr)
        return Path(found->pw_dir);
    return std::nullopt;
}

std::expected<Path, Error> canonicalize(const Path& path) {
    char* real = ::realpath(path.c_str(), nullptr);
    if (real == nullptr) return std::unexpected(os_error(errno, path.string()));
    Path out(real);
    std::free(real);
    return out;
}

namespace {

std::optional<Path> env_dir(const char* key) {
    const char* v = std::getenv(key);
    if (v == nullptr) return std::nullopt;
    Path p(v);
    if (!p.is_absolute()) return std::nullopt;
    return p;
}

} // namespace

std::optional<Path> data_dir(std::string_view app) {
    if (auto xdg = env_dir("XDG_DATA_HOME")) return *xdg / app;
    if (auto h = home()) return *h / ".local" / "share" / app;
    if (auto appdata = env_dir("APPDATA")) return *appdata / app;
    return std::nullopt;
}

} // namespace loa::runtime::fs
