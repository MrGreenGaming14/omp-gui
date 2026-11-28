#include "loa/updater/unpack.hpp"

#include <archive.h>
#include <archive_entry.h>
#include <fcntl.h>
#include <sys/stat.h>
#include <unistd.h>

#include <cerrno>
#include <filesystem>
#include <optional>
#include <string>
#include <vector>

#include "loa/runtime/fs.hpp"

namespace loa::updater::unpack {
namespace {

namespace stdfs = std::filesystem;

Error unsafe(std::string_view entry, std::string_view reason) { return Error::unsafe_entry(entry, reason); }
std::error_code last_error() { return {errno, std::generic_category()}; }

// Rust `Path::components` of a `/`-separated path: a leading `/` is the root, empty parts and
// non-leading `.` vanish, a leading `.` is `CurDir`.
struct Component {
    enum class Kind { Root, CurDir, ParentDir, Normal } kind;
    std::string name;
};

std::vector<Component> components(std::string_view path) {
    std::vector<Component> out;
    if (path.starts_with('/')) out.push_back({Component::Kind::Root, {}});
    std::size_t start = 0;
    bool first = true;
    while (start <= path.size()) {
        auto end = path.find('/', start);
        if (end == std::string_view::npos) end = path.size();
        const auto part = path.substr(start, end - start);
        if (part == "..") out.push_back({Component::Kind::ParentDir, {}});
        else if (part == ".") {
            if (first && !path.starts_with('/')) out.push_back({Component::Kind::CurDir, {}});
        } else if (!part.empty()) {
            out.push_back({Component::Kind::Normal, std::string(part)});
        }
        if (!part.empty() || start != 0) first = false;
        if (end == path.size()) break;
        start = end + 1;
    }
    return out;
}

// The payload-relative path of an entry minus `strip` leading parts; none when nothing remains.
Result<std::optional<stdfs::path>> relative(std::string_view raw, std::size_t strip, std::string_view shown) {
    stdfs::path out;
    std::size_t index = 0;
    for (const auto& part : components(raw)) {
        if (part.kind == Component::Kind::CurDir) continue;
        switch (part.kind) {
            case Component::Kind::Normal:
                if (index >= strip) out /= part.name;
                break;
            case Component::Kind::ParentDir: return std::unexpected(unsafe(shown, "path contains `..`"));
            default: return std::unexpected(unsafe(shown, "absolute path"));
        }
        ++index;
    }
    if (out.empty()) return std::optional<stdfs::path>{};
    return std::optional<stdfs::path>{std::move(out)};
}

// Writes entries under `root`; every path is confined to it, links included.
class EntrySink {
  public:
    static Result<EntrySink> create(const Path& root) {
        if (::mkdir(root.c_str(), 0777) != 0) return std::unexpected(Error::io(root, last_error()));
        return EntrySink(root);
    }

    Result<Path> confine(const Path& rel, std::string_view shown) const {
        auto r = loa::runtime::fs::confine(root_, rel);
        if (r) return std::move(*r);
        if (r.error().code() == loa::ErrorCode::permission_denied) return std::unexpected(unsafe(shown, "path leaves the payload"));
        return std::unexpected(Error::io(root_ / rel, r.error()));
    }

    Result<Path> dirs(const Path& rel, std::string_view shown) const {
        Path parent = root_;
        for (const auto& part : rel) {
            parent /= part;
            struct stat st {};
            if (::lstat(parent.c_str(), &st) == 0) {
                if (S_ISLNK(st.st_mode)) return std::unexpected(unsafe(shown, "entry parent is a symlink"));
                continue;
            }
            if (errno == ENOENT) break;
            return std::unexpected(Error::io(parent, last_error()));
        }
        auto path = confine(rel, shown);
        if (!path) return path;
        std::error_code ec;
        stdfs::create_directories(*path, ec);
        if (ec) return std::unexpected(Error::io(*path, ec));
        return path;
    }

    // A fresh leaf at `rel`: parents made, an earlier file or link there removed.
    Result<Path> leaf(const Path& rel, std::string_view shown) const {
        const auto name = rel.filename();
        if (name.empty()) return std::unexpected(unsafe(shown, "no file name"));
        auto dir = dirs(rel.parent_path(), shown);
        if (!dir) return dir;
        Path dest = *dir / name;
        struct stat st {};
        if (::lstat(dest.c_str(), &st) == 0) {
            if (S_ISDIR(st.st_mode)) return std::unexpected(unsafe(shown, "replaces a directory"));
            if (::unlink(dest.c_str()) != 0) return std::unexpected(Error::io(dest, last_error()));
        }
        return dest;
    }

    // `read` fills a buffer and returns its length, 0 at the end, or an error.
    template <typename Read>
    Result<> file(const Path& rel, std::string_view shown, Read&& read, unsigned mode) const {
        auto dest = leaf(rel, shown);
        if (!dest) return std::unexpected(dest.error());
        const int fd = ::open(dest->c_str(), O_WRONLY | O_CREAT | O_EXCL | O_CLOEXEC, 0666);
        if (fd < 0) return std::unexpected(Error::io(*dest, last_error()));
        std::vector<char> buf(64 * 1024);
        for (;;) {
            auto n = read(buf);
            if (!n) {
                ::close(fd);
                return std::unexpected(n.error());
            }
            if (*n == 0) break;
            for (std::size_t off = 0; off < *n;) {
                const auto w = ::write(fd, buf.data() + off, *n - off);
                if (w < 0) {
                    if (errno == EINTR) continue;
                    const auto ec = last_error();
                    ::close(fd);
                    return std::unexpected(Error::io(*dest, ec));
                }
                off += static_cast<std::size_t>(w);
            }
        }
        // Closed synced so no writable descriptor stays open once it may run.
        if (::fsync(fd) != 0) {
            const auto ec = last_error();
            ::close(fd);
            return std::unexpected(Error::io(*dest, ec));
        }
        ::close(fd);
        if (::chmod(dest->c_str(), (mode & 0777) | 0600) != 0) return std::unexpected(Error::io(*dest, last_error()));
        return {};
    }

    // A relative link that may only climb before it descends, and never above the payload.
    Result<> symlink(const Path& rel, std::string_view target, std::string_view shown) const {
        std::size_t depth = 0;
        for (const auto& _ : rel.parent_path()) {
            (void)_;
            ++depth;
        }
        bool descended = false;
        for (const auto& part : components(target)) {
            switch (part.kind) {
                case Component::Kind::ParentDir:
                    if (descended) return std::unexpected(unsafe(shown, "symlink target climbs after descending"));
                    if (depth == 0) return std::unexpected(unsafe(shown, "symlink points outside the payload"));
                    --depth;
                    break;
                case Component::Kind::Normal: descended = true; break;
                case Component::Kind::CurDir: break;
                case Component::Kind::Root: return std::unexpected(unsafe(shown, "symlink target is absolute"));
            }
        }
        if (target.empty()) return std::unexpected(unsafe(shown, "symlink without target"));
#if defined(_WIN32)
        return std::unexpected(unsafe(shown, "symlinks are unsupported here"));
#else
        auto dest = leaf(rel, shown);
        if (!dest) return std::unexpected(dest.error());
        if (::symlink(std::string(target).c_str(), dest->c_str()) != 0) return std::unexpected(Error::io(*dest, last_error()));
        return {};
#endif
    }

    Result<> hard_link(const Path& rel, const Path& target, std::string_view shown) const {
        auto source = confine(target, shown);
        if (!source) return std::unexpected(source.error());
        struct stat st {};
        if (::lstat(source->c_str(), &st) != 0 || !S_ISREG(st.st_mode))
            return std::unexpected(unsafe(shown, "hard link target is not a regular file"));
        auto dest = leaf(rel, shown);
        if (!dest) return std::unexpected(dest.error());
        if (::link(source->c_str(), dest->c_str()) != 0) return std::unexpected(Error::io(*dest, last_error()));
        return {};
    }

  private:
    explicit EntrySink(Path root) : root_(std::move(root)) {}
    Path root_;
};

using Archive = std::unique_ptr<archive, decltype(&archive_read_free)>;

Result<Archive> open_archive(const Path& artifact, bool zip) {
    Archive a(archive_read_new(), &archive_read_free);
    if (zip) {
        archive_read_support_format_zip(a.get());
    } else {
        archive_read_support_filter_gzip(a.get());
        archive_read_support_format_tar(a.get());
    }
    if (archive_read_open_filename(a.get(), artifact.c_str(), 64 * 1024) != ARCHIVE_OK) {
        const int err = archive_errno(a.get());
        if (err == ENOENT || err == EACCES) return std::unexpected(Error::io(artifact, std::error_code(err, std::generic_category())));
        return std::unexpected(Error::archive(archive_error_string(a.get()) ? archive_error_string(a.get()) : "cannot open"));
    }
    return a;
}

Result<> unpack_archive(const Path& artifact, const Path& payload, std::size_t strip, bool zip) {
    auto sink = EntrySink::create(payload);
    if (!sink) return std::unexpected(sink.error());
    auto opened = open_archive(artifact, zip);
    if (!opened) return std::unexpected(opened.error());
    archive* a = opened->get();
    const auto failed = [&] { return Error::archive(archive_error_string(a) ? archive_error_string(a) : "read failed"); };
    archive_entry* entry = nullptr;
    for (;;) {
        const int r = archive_read_next_header(a, &entry);
        if (r == ARCHIVE_EOF) break;
        if (r < ARCHIVE_WARN) return std::unexpected(failed());
        const char* raw_c = archive_entry_pathname(entry);
        const std::string raw = raw_c ? raw_c : "";
        auto rel = relative(raw, strip, raw);
        if (!rel) return std::unexpected(rel.error());
        if (!*rel) continue;
        const auto type = archive_entry_filetype(entry);
        const char* hardlink = archive_entry_hardlink(entry);
        if (hardlink && !zip) {
            auto target = relative(hardlink, strip, raw);
            if (!target) return std::unexpected(target.error());
            if (!*target) return std::unexpected(unsafe(raw, "hard link to the archive root"));
            if (auto res = sink->hard_link(**rel, **target, raw); !res) return res;
        } else if (type == AE_IFDIR) {
            if (auto res = sink->dirs(**rel, raw); !res) return std::unexpected(res.error());
        } else if (type == AE_IFLNK) {
            const char* target = archive_entry_symlink(entry);
            if (!target) return std::unexpected(unsafe(raw, "link without target"));
            if (auto res = sink->symlink(**rel, target, raw); !res) return res;
        } else if (type == AE_IFREG) {
            const unsigned permissions = static_cast<unsigned>(archive_entry_perm(entry));
            const unsigned mode = zip && permissions == 0 ? 0644u : permissions;
            const auto read = [&](std::vector<char>& buf) -> Result<std::size_t> {
                const auto n = archive_read_data(a, buf.data(), buf.size());
                if (n < 0) return std::unexpected(failed());
                return static_cast<std::size_t>(n);
            };
            if (auto res = sink->file(**rel, raw, read, mode); !res) return res;
        } else {
            return std::unexpected(unsafe(raw, "unsupported entry type"));
        }
    }
    return {};
}

} // namespace

Result<> TarGz::unpack(const Path& artifact, const Path& payload) const { return unpack_archive(artifact, payload, strip, false); }

Result<> Zip::unpack(const Path& artifact, const Path& payload) const { return unpack_archive(artifact, payload, 0, true); }

Result<> Raw::unpack(const Path& artifact, const Path& payload) const {
    auto sink = EntrySink::create(payload);
    if (!sink) return std::unexpected(sink.error());
    const auto name = artifact.filename();
    if (name.empty()) return std::unexpected(Error::bad_name(artifact.string()));
    const int fd = ::open(artifact.c_str(), O_RDONLY | O_CLOEXEC);
    if (fd < 0) return std::unexpected(Error::io(artifact, last_error()));
    const auto read = [&](std::vector<char>& buf) -> Result<std::size_t> {
        for (;;) {
            const auto n = ::read(fd, buf.data(), buf.size());
            if (n >= 0) return static_cast<std::size_t>(n);
            if (errno != EINTR) return std::unexpected(Error::io(artifact, last_error()));
        }
    };
    auto r = sink->file(name, name.string(), read, 0755);
    ::close(fd);
    return r;
}

std::unique_ptr<Unpacker> unpacker(loa::settings::ArchiveFormat format) {
    using F = loa::settings::ArchiveFormat;
    switch (format) {
        case F::TarGz: return std::make_unique<TarGz>(0);
        case F::NpmTarball: return std::make_unique<TarGz>(1);
        case F::Zip: return std::make_unique<Zip>();
        case F::Raw: break;
    }
    return std::make_unique<Raw>();
}

} // namespace loa::updater::unpack
