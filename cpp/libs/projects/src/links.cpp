#include "loa/projects/links.hpp"

#include <algorithm>
#include <fstream>
#include <system_error>

namespace loa::projects::links {

namespace fs = std::filesystem;

namespace {

std::unexpected<loa::Error> fail(const fs::path& path, const std::error_code& ec) {
    return std::unexpected(loa::Error(ErrorCode::io, path.string() + ": " + ec.message()));
}

// Unix links are files; Windows directory links are removed as directories.
std::expected<void, loa::Error> remove_link(const fs::path& link) {
    std::error_code ec;
    fs::remove(link, ec);
    if (ec) return fail(link, ec);
    return {};
}

} // namespace

std::expected<void, loa::Error> sync(const fs::path& folder, const std::set<fs::path>& dirs) {
    const auto links = folder / LINKS_DIR;
    std::error_code ec;
    std::set<fs::path> wanted;
    for (const auto& dir : dirs)
        if (dir != folder && fs::is_directory(dir, ec)) wanted.insert(dir);
    std::set<std::string> taken;
    if (fs::is_directory(links, ec)) {
        fs::directory_iterator it(links, ec);
        if (ec) return fail(links, ec);
        for (; it != fs::directory_iterator(); it.increment(ec)) {
            if (ec) return fail(links, ec);
            const auto status = it->symlink_status(ec);
            if (ec) return fail(it->path(), ec);
            bool keep = !fs::is_symlink(status);
            if (!keep) {
                const auto target = fs::read_symlink(it->path(), ec);
                if (ec) return fail(it->path(), ec);
                keep = wanted.erase(target) > 0;
            }
            // Anything that isn't a link stays, and so does a link to a wanted directory.
            if (keep) {
                taken.insert(it->path().filename().string());
            } else if (auto r = remove_link(it->path()); !r) {
                return r;
            }
        }
    }
    const bool only_ignore = std::ranges::all_of(taken, [](const std::string& name) { return name == GITIGNORE; });
    if (wanted.empty() && only_ignore) return links::remove(folder);
    fs::create_directories(links, ec);
    if (ec) return fail(links, ec);
    const auto ignore = links / GITIGNORE;
    if (!fs::exists(ignore, ec)) {
        std::ofstream out(ignore, std::ios::binary | std::ios::trunc);
        out << "*\n";
        if (!out) return fail(ignore, std::make_error_code(std::errc::io_error));
        taken.insert(std::string(GITIGNORE));
    }
    for (const auto& dir : wanted) {
        const auto file = dir.filename().string();
        const std::string base = file.empty() ? "root" : file;
        std::string name = base;
        for (int n = 2; taken.contains(name); ++n) name = base + "-" + std::to_string(n);
        fs::create_directory_symlink(dir, links / name, ec);
        if (ec) return fail(links / name, ec);
        taken.insert(std::move(name));
    }
    return {};
}

std::expected<void, loa::Error> remove(const fs::path& folder) {
    const auto links = folder / LINKS_DIR;
    std::error_code ec;
    if (!fs::is_directory(links, ec)) return {};
    fs::directory_iterator it(links, ec);
    if (ec) return fail(links, ec);
    for (; it != fs::directory_iterator(); it.increment(ec)) {
        if (ec) return fail(links, ec);
        const auto status = it->symlink_status(ec);
        if (ec) return fail(it->path(), ec);
        if (fs::is_symlink(status) || it->path().filename() == GITIGNORE) {
            if (auto r = remove_link(it->path()); !r) return r;
        }
    }
    fs::remove(links, ec);
    // Something of the user's is still in it.
    if (ec == std::errc::directory_not_empty) return {};
    if (ec) return fail(links, ec);
    return {};
}

} // namespace loa::projects::links
