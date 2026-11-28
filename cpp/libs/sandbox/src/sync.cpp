#include "loa/sandbox/sync.hpp"

#include <archive.h>
#include <archive_entry.h>
#include <stdlib.h>

#include <algorithm>
#include <cctype>
#include <format>
#include <map>
#include <memory>
#include <set>
#include <system_error>

#include "loa/sandbox/client.hpp"
#include "loa/sandbox/isolation.hpp"
#include "process.hpp"

namespace loa::sandbox::sync {

namespace fs = std::filesystem;

namespace {

// Archive bytes per request, well under the frame limit.
constexpr std::size_t PART = 16u << 20;
// Where the archive collects in the stub root until it is unpacked.
constexpr std::string_view ARCHIVE = ".loa-sandbox-in.tar";
const std::vector<std::string> PACK{"-cf", "-", "--exclude=./.git", "."};

constexpr bool WINDOWS =
#ifdef _WIN32
    true;
#else
    false;
#endif

loa::Error invalid(std::string message) { return error::isolation(std::move(message)); }

// Rust `{:?}` of a string.
std::string debug_str(std::string_view s) {
    std::string out = "\"";
    for (char c : s) {
        if (c == '"' || c == '\\') out += '\\';
        if (c == '\0') {
            out += "\\0";
            continue;
        }
        out += c;
    }
    return out + "\"";
}

// The code points of UTF-8 text (invalid bytes as themselves).
std::vector<std::uint32_t> chars(std::string_view s) {
    std::vector<std::uint32_t> out;
    for (std::size_t i = 0; i < s.size();) {
        const auto c = static_cast<std::uint8_t>(s[i]);
        const std::size_t n = c < 0x80 ? 0 : (c & 0xe0) == 0xc0 ? 1 : (c & 0xf0) == 0xe0 ? 2 : 3;
        std::uint32_t cp = n == 0 ? c : n == 1 ? c & 0x1f : n == 2 ? c & 0x0f : c & 0x07;
        for (std::size_t k = 1; k <= n && i + k < s.size(); ++k) cp = (cp << 6) | (static_cast<std::uint8_t>(s[i + k]) & 0x3f);
        out.push_back(cp);
        i += n + 1;
    }
    return out;
}

std::string ascii_upper(std::string_view s) {
    std::string out(s);
    for (auto& c : out) c = static_cast<char>(std::toupper(static_cast<unsigned char>(c)));
    return out;
}

std::string ascii_lower(std::string_view s) {
    std::string out(s);
    for (auto& c : out) c = static_cast<char>(std::tolower(static_cast<unsigned char>(c)));
    return out;
}

} // namespace

namespace detail {

Result<> check_name(std::string_view name) {
    if (name.empty() || name == "." || name == ".." || name.find('/') != std::string_view::npos ||
        name.find('\\') != std::string_view::npos || name.find('\0') != std::string_view::npos) {
        return std::unexpected(invalid("invalid directory entry: " + debug_str(name)));
    }
    return {};
}

std::optional<std::string_view> windows_name_problem(std::string_view name) {
    if (name.find(':') != std::string_view::npos) return "contains ':' (a drive or stream)";
    for (const auto c : chars(name)) {
        const bool control = c < 0x20 || (c >= 0x7f && c <= 0x9f);
        if (control || c == '<' || c == '>' || c == '"' || c == '|' || c == '?' || c == '*') {
            return "contains a character Windows does not allow";
        }
    }
    if (name.ends_with('.') || name.ends_with(' ')) return "ends with a dot or a space";
    // `CON.txt` and `CON .txt` name the device too.
    auto stem_view = name.substr(0, name.find('.'));
    while (stem_view.ends_with(' ')) stem_view.remove_suffix(1);
    const auto stem = ascii_upper(stem_view);
    const auto numbered = [&](std::string_view prefix) {
        if (!stem.starts_with(prefix)) return false;
        const auto rest = chars(std::string_view(stem).substr(prefix.size()));
        return rest.size() == 1 && ((rest[0] >= '1' && rest[0] <= '9') || rest[0] == 0xb9 || rest[0] == 0xb2 || rest[0] == 0xb3);
    };
    for (std::string_view device : {"CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"}) {
        if (stem == device) return "is a reserved device name";
    }
    if (numbered("COM") || numbered("LPT")) return "is a reserved device name";
    if (ascii_lower(name) == "git~1") return "is the short name of .git";
    return std::nullopt;
}

Result<> check_guest_name(std::string_view name, bool top) {
    if (auto r = check_name(name); !r) return r;
    if (!WINDOWS) return {};
    if (auto problem = windows_name_problem(name)) {
        return std::unexpected(invalid(std::format("guest entry {} can't be copied to Windows: it {}", debug_str(name), *problem)));
    }
    // The checkout's own `.git` is never replaced, whatever case the guest uses.
    if (ascii_lower(name) == ".git" && !(top && name == ".git")) {
        return std::unexpected(invalid(std::format("guest entry {} can't be copied to Windows: it would replace .git", debug_str(name))));
    }
    return {};
}

Result<> check_collisions(const std::vector<std::string>& names, std::string_view dir) {
    if (!WINDOWS) return {};
    std::set<std::string> seen;
    for (const auto& name : names) {
        if (!seen.insert(ascii_lower(name)).second) {
            return std::unexpected(invalid(std::format("guest entries of {} differ only in case ({}); Windows can't hold both", dir, debug_str(name))));
        }
    }
    return {};
}

Result<std::vector<std::string>> member_names(std::string_view path) {
    if (!loa::sandbox::detail::valid_utf8(path)) {
        return std::unexpected(invalid("non-UTF8 workspace path: " + loa::sandbox::detail::lossy(path)));
    }
    while (path.ends_with('/')) path.remove_suffix(1);
    if (path.starts_with("./")) path.remove_prefix(2);
    std::vector<std::string> names;
    if (path.empty() || path == ".") return names;
    for (;;) {
        const auto slash = path.find('/');
        const auto name = path.substr(0, slash);
        if (auto r = check_name(name); !r) return std::unexpected(r.error());
        names.emplace_back(name);
        if (slash == std::string_view::npos) break;
        path.remove_prefix(slash + 1);
    }
    return names;
}

Result<> check_archive(std::string_view archive) {
    std::unique_ptr<struct archive, decltype(&archive_read_free)> reader(archive_read_new(), archive_read_free);
    archive_read_support_format_tar(reader.get());
    archive_read_support_format_gnutar(reader.get());
    if (archive_read_open_memory(reader.get(), archive.data(), archive.size()) != ARCHIVE_OK) {
        return std::unexpected(loa::Error(ErrorCode::io, archive_error_string(reader.get()) ? archive_error_string(reader.get()) : "unreadable archive"));
    }
    std::map<std::string, std::vector<std::string>> dirs;
    archive_entry* entry = nullptr;
    for (;;) {
        const int r = archive_read_next_header(reader.get(), &entry);
        if (r == ARCHIVE_EOF) break;
        if (r < ARCHIVE_WARN) {
            const char* message = archive_error_string(reader.get());
            return std::unexpected(loa::Error(ErrorCode::io, message ? message : "unreadable archive"));
        }
        const char* raw = archive_entry_pathname(entry);
        auto names = member_names(raw ? raw : "");
        if (!names) return std::unexpected(names.error());
        std::string wire;
        for (const auto& n : *names) wire += (wire.empty() ? "" : "/") + n;
        if (const char* link = archive_entry_hardlink(entry)) {
            // A hard link names another member of the archive.
            if (auto target = member_names(link); !target) return std::unexpected(target.error());
        } else {
            switch (archive_entry_filetype(entry)) {
            case AE_IFREG:
            case AE_IFDIR:
                break;
            case AE_IFLNK:
                if (WINDOWS) {
                    return std::unexpected(invalid(std::format(
                        "guest symlink {} can't be copied to Windows; replace it with a regular file or remove it", wire)));
                }
                break;
            default:
                return std::unexpected(invalid("unsupported stub entry: " + wire));
            }
        }
        for (std::size_t depth = 0; depth < names->size(); ++depth) {
            if (auto r = check_guest_name((*names)[depth], depth == 0); !r) return r;
            std::string dir;
            for (std::size_t i = 0; i < depth; ++i) dir += (i ? "/" : "") + (*names)[i];
            auto& entries = dirs[dir];
            if (std::ranges::find(entries, (*names)[depth]) == entries.end()) entries.push_back((*names)[depth]);
        }
    }
    for (const auto& [dir, names] : dirs) {
        if (auto r = check_collisions(names, dir.empty() ? "." : dir); !r) return r;
    }
    return {};
}

Result<std::string> tar(const fs::path& dir, const std::vector<std::string>& args, std::string_view input) {
    loa::sandbox::detail::Spawn spec;
    spec.program = "tar";
    spec.args = args;
    spec.cwd = dir;
    auto out = loa::sandbox::detail::run(spec, input);
    if (!out) return std::unexpected(out.error());
    if (!out->success()) return std::unexpected(invalid("tar failed: " + loa::sandbox::detail::lossy_trim(out->err)));
    return std::move(out->out);
}

Result<> unpack(const fs::path& workspace, std::string_view archive) {
    if (auto r = check_archive(archive); !r) return r;
    auto pattern = (workspace / ".loa-sandbox-outXXXXXX").string();
    if (!::mkdtemp(pattern.data())) return std::unexpected(error::os(workspace.string(), errno));
    const fs::path staging(pattern);
    struct Cleanup {
        fs::path path;
        ~Cleanup() {
            std::error_code ec;
            fs::remove_all(path, ec);
        }
    } cleanup{staging};
    if (auto r = tar(staging, {"-xf", "-", "--no-same-owner"}, archive); !r) return std::unexpected(r.error());
    std::set<std::string> kept;
    std::error_code ec;
    for (const auto& entry : fs::directory_iterator(staging, ec)) {
        const auto name = entry.path().filename().string();
        if (name == ".git") continue;
        const auto target = workspace / name;
        if (auto r = loa::sandbox::detail::remove_entry(target); !r) return r;
        fs::rename(entry.path(), target, ec);
        if (ec) return std::unexpected(error::os(target.string(), ec.value()));
        kept.insert(name);
    }
    if (ec) return std::unexpected(error::os(staging.string(), ec.value()));
    const auto staging_name = staging.filename().string();
    std::vector<fs::path> stale;
    for (const auto& entry : fs::directory_iterator(workspace, ec)) {
        const auto name = entry.path().filename().string();
        if (name != ".git" && !kept.contains(name) && name != staging_name) stale.push_back(entry.path());
    }
    if (ec) return std::unexpected(error::os(workspace.string(), ec.value()));
    for (const auto& path : stale) {
        if (auto r = loa::sandbox::detail::remove_entry(path); !r) return r;
    }
    return {};
}

} // namespace detail

Result<> copy_in(const StubClient& client, const fs::path& workspace) {
    auto archive = detail::tar(workspace, PACK, {});
    if (!archive) return std::unexpected(archive.error());
    std::string_view rest = *archive;
    for (std::size_t index = 0; !rest.empty(); ++index) {
        const auto part = rest.substr(0, PART);
        rest.remove_prefix(part.size());
        auto exec = proto::ExecRequest::shell(std::format("cat {} \"$1\"", index == 0 ? ">" : ">>"), {std::string(ARCHIVE)});
        exec.stdin_data = std::string(part);
        if (auto r = client.run_blocking(std::move(exec)); !r) return std::unexpected(r.error());
    }
    auto unpacked = client.run_blocking(proto::ExecRequest::shell(R"(tar -xf "$1" && rm -f "$1")", {std::string(ARCHIVE)}));
    if (!unpacked) return std::unexpected(unpacked.error());
    return {};
}

Result<> copy_out(const StubClient& client, const fs::path& workspace) {
    proto::ExecRequest exec;
    exec.argv = {"tar"};
    exec.argv.insert(exec.argv.end(), PACK.begin(), PACK.end());
    auto archive = client.run_blocking(std::move(exec));
    if (!archive) return std::unexpected(archive.error());
    return detail::unpack(workspace, *archive);
}

} // namespace loa::sandbox::sync
