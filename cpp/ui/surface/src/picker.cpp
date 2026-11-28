#include "loa/ui/surface/picker.hpp"

#include <algorithm>
#include <cctype>
#include <system_error>

#include "loa/runtime/fs.hpp"

namespace loa::ui::surface::picker {

namespace {

std::string lower(std::string_view s) {
    std::string out(s);
    for (char& c : out) c = static_cast<char>(std::tolower(static_cast<unsigned char>(c)));
    return out;
}

std::string trim(std::string_view s) {
    const auto ws = [](char c) { return std::isspace(static_cast<unsigned char>(c)) != 0; };
    while (!s.empty() && ws(s.front())) s.remove_prefix(1);
    while (!s.empty() && ws(s.back())) s.remove_suffix(1);
    return std::string(s);
}

// Rust `io::Error`'s display: the OS message and its code.
std::string io_text(const std::error_code& ec) { return ec.message() + " (os error " + std::to_string(ec.value()) + ")"; }

// Visible subfolders, then files `supported` takes, each sorted by name.
template <typename F>
std::optional<std::vector<Entry>> read(const Path& dir, F supported, std::error_code& ec) {
    std::filesystem::directory_iterator it(dir, ec);
    if (ec) return std::nullopt;
    std::vector<Entry> entries;
    for (const auto& e : it) {
        std::string name = e.path().filename().string();
        if (name.starts_with('.')) continue;
        std::error_code ignore;
        const bool is_dir = std::filesystem::is_directory(e.path(), ignore);
        if (is_dir || supported(e.path())) entries.push_back({std::move(name), e.path(), is_dir});
    }
    std::ranges::stable_sort(entries, [](const Entry& a, const Entry& b) {
        if (a.is_dir != b.is_dir) return a.is_dir;
        return lower(a.name) < lower(b.name);
    });
    return entries;
}

} // namespace

FilePicker::FilePicker(Path dir, std::vector<std::string> extensions) : extensions_(std::move(extensions)) { enter(std::move(dir)); }

bool FilePicker::supported(const Path& path) const {
    const std::string ext = path.extension().string();
    if (ext.size() < 2) return false;
    const std::string want = lower(std::string_view(ext).substr(1));
    return std::ranges::any_of(extensions_, [&](const std::string& e) { return lower(e) == want; });
}

bool FilePicker::has_parent() const { return dir_.has_parent_path() && dir_.parent_path() != dir_; }

void FilePicker::enter(Path dir) {
    std::error_code ec;
    auto listed = read(dir, [this](const Path& p) { return supported(p); }, ec);
    if (!listed) {
        error_ = dir.string() + ": " + io_text(ec);
        return;
    }
    path_ = dir.string();
    dir_ = std::move(dir);
    entries_ = std::move(*listed);
    error_.reset();
}

std::optional<Path> FilePicker::update(Message message) {
    if (std::holds_alternative<msg::Up>(message)) {
        if (has_parent()) enter(dir_.parent_path());
    } else if (auto* e = std::get_if<msg::Enter>(&message)) {
        enter(std::move(e->dir));
    } else if (auto* c = std::get_if<msg::Choose>(&message)) {
        return std::move(c->file);
    } else if (auto* p = std::get_if<msg::PathChanged>(&message)) {
        path_ = std::move(p->path);
    } else {
        Path path = expand_with(trim(path_), loa::runtime::fs::home());
        std::error_code ec;
        if (std::filesystem::is_directory(path, ec)) {
            enter(std::move(path));
        } else if (std::filesystem::is_regular_file(path, ec) && supported(path)) {
            return path;
        } else if (std::filesystem::is_regular_file(path, ec)) {
            error_ = path.string() + " isn't a PDF, Office or image file";
        } else {
            error_ = path.string() + " doesn't exist";
        }
    }
    return std::nullopt;
}

Path expand_with(std::string_view path, const std::optional<Path>& home) {
    std::optional<std::string_view> rest;
    if (path.starts_with("~/") || path.starts_with("~\\")) rest = path.substr(2);
    if (rest && home) return *home / Path(std::string(*rest));
    return Path(std::string(path));
}

} // namespace loa::ui::surface::picker
