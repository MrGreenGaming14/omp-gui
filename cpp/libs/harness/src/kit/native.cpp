#include "loa/harness/kit/native.hpp"

#include <chrono>
#include <system_error>

namespace loa::harness::kit {

namespace fs = std::filesystem;

std::optional<NativeSession> NativeSession::at(std::string_view harness, fs::path path, std::optional<std::string> project,
                                               std::optional<std::string> title) {
    std::error_code ec;
    const auto status = fs::status(path, ec);
    if (ec || !fs::exists(status)) return std::nullopt;
    std::uint64_t size = 0;
    if (fs::is_regular_file(status)) {
        size = fs::file_size(path, ec);
        if (ec) return std::nullopt;
    }
    std::int64_t modified = 0;
    if (auto time = fs::last_write_time(path, ec); !ec) {
        const auto since = std::chrono::clock_cast<std::chrono::system_clock>(time).time_since_epoch();
        if (since.count() >= 0) modified = std::chrono::duration_cast<std::chrono::milliseconds>(since).count();
    }
    return NativeSession{std::string(harness), std::move(path), std::move(project), std::move(title), modified, size};
}

std::vector<fs::path> walk(const fs::path& dir, std::size_t depth, const std::function<bool(std::string_view)>& keep) {
    std::vector<fs::path> out;
    std::error_code ec;
    fs::directory_iterator it(dir, ec);
    for (; !ec && it != fs::directory_iterator(); it.increment(ec)) {
        const auto& entry = *it;
        // `DirEntry::file_type` does not follow symlinks.
        std::error_code type_ec;
        const auto type = entry.symlink_status(type_ec).type();
        if (type_ec) continue;
        if (type == fs::file_type::directory && depth > 0) {
            auto below = walk(entry.path(), depth - 1, keep);
            out.insert(out.end(), std::make_move_iterator(below.begin()), std::make_move_iterator(below.end()));
        } else if (type == fs::file_type::regular && depth == 0 && keep(entry.path().filename().string())) {
            out.push_back(entry.path());
        }
    }
    return out;
}

std::optional<std::string> parent_name(const fs::path& path) {
    if (!path.has_parent_path()) return std::nullopt;
    auto name = path.parent_path().filename();
    if (name.empty() || name == "." || name == "..") return std::nullopt;
    return name.string();
}

} // namespace loa::harness::kit
