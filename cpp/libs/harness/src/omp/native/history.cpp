// Project-scoped history discovery for omp's JSONL session files.

#include "loa/harness/omp/native.hpp"

#include <algorithm>
#include <cerrno>
#include <chrono>
#include <fstream>
#include <system_error>
#include <unordered_set>

#include "loa/runtime/fs.hpp"

namespace loa::harness::omp::native {

namespace fs = std::filesystem;
namespace json = loa::runtime::json;
using loa::Error;
using loa::ErrorCode;

namespace {

// Maximum prefix bytes inspected per file so large transcripts stay bounded.
constexpr std::size_t MAX_PREFIX_BYTES = 64 * 1024;

template <typename T>
using IoResult = std::expected<T, Error>;

Error io_error(const std::error_code& ec, std::string_view what = {}) {
    const auto code = ec == std::errc::no_such_file_or_directory ? ErrorCode::not_found
                    : ec == std::errc::invalid_argument ? ErrorCode::invalid_argument
                    : ec == std::errc::permission_denied ? ErrorCode::permission_denied : ErrorCode::io;
    std::string message = ec.message() + " (os error " + std::to_string(ec.value()) + ")";
    if (!what.empty()) message = std::string(what) + ": " + message;
    return Error(code, std::move(message));
}

std::optional<std::string_view> str_field(const json::Value& map, std::string_view key) {
    const json::Value* field = map.find(key);
    const std::string* text = field ? field->as_string() : nullptr;
    if (!text) return std::nullopt;
    return *text;
}

std::optional<std::string_view> non_empty_str(const json::Value& map, std::string_view key) {
    auto text = str_field(map, key);
    if (!text || std::ranges::all_of(*text, [](unsigned char c) { return c == ' ' || (c >= '\t' && c <= '\r'); }))
        return std::nullopt;
    return text;
}

// `std::string_view` is bytes; reject invalid UTF-8 before JSON parsing, as `str::from_utf8` does.
bool valid_utf8(std::string_view text) {
    for (std::size_t i = 0; i < text.size();) {
        const unsigned char first = static_cast<unsigned char>(text[i]);
        if (first < 0x80) {
            ++i;
            continue;
        }
        int more = 0;
        std::uint32_t codepoint = 0;
        if (first >= 0xc2 && first <= 0xdf) { more = 1; codepoint = first & 0x1f; }
        else if (first >= 0xe0 && first <= 0xef) { more = 2; codepoint = first & 0x0f; }
        else if (first >= 0xf0 && first <= 0xf4) { more = 3; codepoint = first & 0x07; }
        else return false;
        if (i + static_cast<std::size_t>(more) >= text.size()) return false;
        for (int k = 0; k < more; ++k) {
            const unsigned char next = static_cast<unsigned char>(text[++i]);
            if ((next & 0xc0) != 0x80) return false;
            codepoint = (codepoint << 6) | (next & 0x3f);
        }
        if ((more == 2 && codepoint < 0x800) || (more == 3 && codepoint < 0x10000) ||
            (codepoint >= 0xd800 && codepoint <= 0xdfff) || codepoint > 0x10ffff) return false;
        ++i;
    }
    return true;
}

// The first title slot (if any), then the session header. Malformed JSON lines are skipped.
std::optional<std::pair<std::optional<json::Value>, json::Value>> read_header(std::string_view prefix) {
    std::optional<json::Value> title_slot;
    while (!prefix.empty()) {
        const auto end = prefix.find('\n');
        std::string_view line = prefix.substr(0, end);
        prefix = end == std::string_view::npos ? std::string_view() : prefix.substr(end + 1);
        if (!valid_utf8(line)) return std::nullopt;
        while (!line.empty() && (line.front() == ' ' || (line.front() >= '\t' && line.front() <= '\r'))) line.remove_prefix(1);
        while (!line.empty() && (line.back() == ' ' || (line.back() >= '\t' && line.back() <= '\r'))) line.remove_suffix(1);
        if (line.empty()) continue;
        auto parsed = json::parse(line);
        if (!parsed || !parsed->is_object()) continue;
        if (!title_slot && str_field(*parsed, "type") == "title") {
            title_slot = std::move(*parsed);
            continue;
        }
        if (str_field(*parsed, "type") != "session") return std::nullopt;
        return std::pair(std::move(title_slot), std::move(*parsed));
    }
    return std::nullopt;
}

IoResult<std::optional<ProjectSession>> inspect_session(const fs::path& path, const fs::path& canonical_folder) {
    errno = 0;
    std::ifstream file(path, std::ios::binary);
    if (!file) return std::unexpected(io_error(std::error_code(errno ? errno : EIO, std::generic_category()), path.string()));
    std::string prefix(MAX_PREFIX_BYTES, '\0');
    file.read(prefix.data(), static_cast<std::streamsize>(prefix.size()));
    if (file.bad()) return std::unexpected(io_error(std::error_code(errno ? errno : EIO, std::generic_category()), path.string()));
    prefix.resize(static_cast<std::size_t>(file.gcount()));
    auto parsed = read_header(prefix);
    if (!parsed) return std::nullopt;
    const json::Value& header = parsed->second;
    const auto id = non_empty_str(header, "id");
    const auto cwd = str_field(header, "cwd");
    if (!id || !cwd) return std::nullopt;
    const fs::path cwd_path(*cwd);
    if (!cwd_path.is_absolute()) return std::nullopt;
    auto canonical_cwd = loa::runtime::fs::canonicalize(cwd_path);
    if (!canonical_cwd) {
        if (canonical_cwd.error().code() == ErrorCode::not_found || canonical_cwd.error().code() == ErrorCode::invalid_argument)
            return std::nullopt;
        return std::unexpected(std::move(canonical_cwd.error()));
    }
    if (*canonical_cwd != canonical_folder) return std::nullopt;
    std::error_code ec;
    const auto modified_time = fs::last_write_time(path, ec);
    if (ec) return std::unexpected(io_error(ec, path.string()));
    const auto ms = std::max<std::int64_t>(0, std::chrono::duration_cast<std::chrono::milliseconds>(
        std::chrono::clock_cast<std::chrono::system_clock>(modified_time).time_since_epoch()).count());
    const auto title = parsed->first ? non_empty_str(*parsed->first, "title") : std::nullopt;
    const auto selected = title ? title : non_empty_str(header, "title");
    return ProjectSession{std::string(*id), path, selected ? std::string(*selected) : path.stem().string(), ms};
}

} // namespace

namespace detail {

std::expected<std::vector<ProjectSession>, Error> matching_sessions(const fs::path& agent_dir, const fs::path& folder) {
    std::error_code ec;
    const fs::path sessions_dir = agent_dir / "sessions";
    fs::directory_iterator subdirs(sessions_dir, ec);
    if (ec == std::errc::no_such_file_or_directory) return std::vector<ProjectSession>{};
    if (ec) return std::unexpected(io_error(ec, sessions_dir.string()));
    auto canonical_folder = loa::runtime::fs::canonicalize(folder);
    if (!canonical_folder) return std::unexpected(std::move(canonical_folder.error()));
    std::vector<ProjectSession> sessions;
    const fs::directory_iterator end;
    for (; subdirs != end; subdirs.increment(ec)) {
        if (ec) {
            ec.clear();
            continue;
        }
        if (!fs::is_directory(subdirs->symlink_status(ec)) || ec) { ec.clear(); continue; }
        const fs::path dir = subdirs->path();
        fs::directory_iterator entries(dir, ec);
        if (ec) { ec.clear(); continue; }
        for (; entries != end; entries.increment(ec)) {
            if (ec) { ec.clear(); continue; }
            if (!fs::is_regular_file(entries->symlink_status(ec)) || ec) { ec.clear(); continue; }
            const fs::path path = entries->path();
            if (path.extension() != ".jsonl") continue;
            auto session = inspect_session(path, *canonical_folder);
            if (session && *session) sessions.push_back(std::move(**session));
        }
        ec.clear();
    }
    return sessions;
}

std::expected<void, Error> remove_sessions(
    const std::vector<ProjectSession>& sessions,
    const std::function<std::expected<void, Error>(const fs::path&)>& remove_dir) {
    for (const ProjectSession& session : sessions) {
        fs::path sidecar = session.path;
        sidecar.replace_extension();
        std::error_code ec;
        const auto status = fs::symlink_status(sidecar, ec);
        if (!ec && fs::is_directory(status)) {
            if (auto removed = remove_dir(sidecar); !removed)
                return std::unexpected(removed.error().context("can't remove " + sidecar.string()));
        }
        if (!fs::remove(session.path, ec) || ec) {
            if (!ec) ec = std::make_error_code(std::errc::no_such_file_or_directory);
            return std::unexpected(io_error(ec).context("can't remove " + session.path.string()));
        }
    }
    return {};
}

} // namespace detail

std::expected<std::vector<ProjectSession>, Error> project_history(const fs::path& agent_dir, const fs::path& folder) {
    auto sessions = detail::matching_sessions(agent_dir, folder);
    if (!sessions) return std::unexpected(std::move(sessions.error()));
    std::ranges::sort(*sessions, [](const ProjectSession& a, const ProjectSession& b) {
        if (a.modified != b.modified) return a.modified > b.modified;
        if (a.path != b.path) return a.path < b.path;
        return a.id < b.id;
    });
    std::unordered_set<std::string> seen;
    std::erase_if(*sessions, [&](const ProjectSession& session) { return !seen.insert(session.id).second; });
    return sessions;
}

std::expected<std::optional<ProjectSession>, Error> project_session(const fs::path& path, const fs::path& folder) {
    auto canonical_folder = loa::runtime::fs::canonicalize(folder);
    if (!canonical_folder) return std::unexpected(std::move(canonical_folder.error()));
    return inspect_session(path, *canonical_folder);
}

std::expected<void, Error> delete_project_session(const fs::path& agent_dir, const fs::path& folder, std::string_view id) {
    auto sessions = detail::matching_sessions(agent_dir, folder);
    if (!sessions) return std::unexpected(std::move(sessions.error()));
    std::erase_if(*sessions, [&](const ProjectSession& session) { return session.id != id; });
    if (sessions->empty())
        return std::unexpected(Error(ErrorCode::not_found,
                                     "no saved OMP session " + std::string(id) + " for " + folder.string()));
    return detail::remove_sessions(*sessions, [](const fs::path& path) -> std::expected<void, Error> {
        std::error_code ec;
        fs::remove_all(path, ec);
        if (ec) return std::unexpected(io_error(ec));
        return {};
    });
}

} // namespace loa::harness::omp::native
