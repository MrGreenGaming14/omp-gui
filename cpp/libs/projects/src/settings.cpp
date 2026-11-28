#include "loa/projects/settings.hpp"

#include <algorithm>
#include <cerrno>
#include <cstring>
#include <fstream>
#include <iterator>
#include <system_error>
#include <unistd.h>

#include "loa/core/schema.hpp"
#include "loa/runtime/fs.hpp"

namespace loa::projects {

namespace json = runtime::json;
namespace fs = std::filesystem;
using settings::Ideas;
using settings::PinnedFile;
using settings::ProjectSettings;

namespace {

// `e` naming the file it happened to.
Error at(const fs::path& path, std::string_view e) { return Error::port(path.string() + ": " + std::string(e)); }

// How Rust's `io::Error` displays an OS error.
std::string os_error(int code) { return std::string(std::strerror(code)) + " (os error " + std::to_string(code) + ")"; }

bool valid_utf8(std::string_view s) {
    std::size_t i = 0;
    while (i < s.size()) {
        const auto b = static_cast<unsigned char>(s[i]);
        std::size_t n = 0;
        char32_t min = 0;
        if (b < 0x80) {
            ++i;
            continue;
        }
        if ((b >> 5) == 0x6) n = 1, min = 0x80;
        else if ((b >> 4) == 0xe) n = 2, min = 0x800;
        else if ((b >> 3) == 0x1e) n = 3, min = 0x10000;
        else return false;
        if (i + n >= s.size()) return false;
        char32_t c = b & (0x3f >> n);
        for (std::size_t k = 1; k <= n; ++k) {
            const auto cb = static_cast<unsigned char>(s[i + k]);
            if ((cb & 0xc0) != 0x80) return false;
            c = (c << 6) | (cb & 0x3f);
        }
        if (c < min || c > 0x10ffff || (c >= 0xd800 && c <= 0xdfff)) return false;
        i += n + 1;
    }
    return true;
}

// Deletes `path`; a missing file is fine. A directory is an error, as with `remove_file`.
Result<> remove_file(const fs::path& path) {
    if (::unlink(path.c_str()) != 0 && errno != ENOENT) return std::unexpected(at(path, os_error(errno)));
    return {};
}

std::string type_name(const json::Value& v) {
    if (v.is_null()) return "null";
    if (const auto* b = v.as_bool()) return std::string("boolean `") + (*b ? "true" : "false") + "`";
    if (const auto* s = v.as_string()) return "string \"" + *s + "\"";
    if (v.is_number()) return "number";
    if (v.is_array()) return "a sequence";
    return "a map";
}

std::expected<std::optional<std::string>, std::string> opt_string(const json::Value& obj, std::string_view key) {
    const auto* v = obj.find(key);
    if (!v || v->is_null()) return std::nullopt;
    if (const auto* s = v->as_string()) return *s;
    return std::unexpected("invalid type: " + type_name(*v) + ", expected a string");
}

} // namespace

std::string encode_name(std::string_view project) {
    static constexpr char HEX[] = "0123456789ABCDEF";
    std::string out;
    for (const char ch : project) {
        const auto b = static_cast<unsigned char>(ch);
        const bool keep = (b >= '0' && b <= '9') || (b >= 'a' && b <= 'z') || (b >= 'A' && b <= 'Z') || b == '-' || b == '_';
        if (keep) {
            out.push_back(ch);
        } else {
            out.push_back('%');
            out.push_back(HEX[b >> 4]);
            out.push_back(HEX[b & 0xf]);
        }
    }
    return out;
}

std::optional<std::string> decode_name(std::string_view stem) {
    const auto hex = [](char c) -> int {
        if (c >= '0' && c <= '9') return c - '0';
        if (c >= 'a' && c <= 'f') return c - 'a' + 10;
        if (c >= 'A' && c <= 'F') return c - 'A' + 10;
        return -1;
    };
    std::string out;
    for (std::size_t i = 0; i < stem.size(); ++i) {
        if (stem[i] != '%') {
            out.push_back(stem[i]);
            continue;
        }
        if (i + 2 >= stem.size()) return std::nullopt;
        const int hi = hex(stem[i + 1]), lo = hex(stem[i + 2]);
        if (hi < 0 || lo < 0) return std::nullopt;
        out.push_back(static_cast<char>(hi * 16 + lo));
        i += 2;
    }
    if (!valid_utf8(out)) return std::nullopt;
    return out;
}

json::Value settings_to_json(const ProjectSettings& s) {
    json::Array pinned;
    for (const auto& file : s.pinned) {
        json::Object f{{"path", file.path}};
        if (file.note) f.emplace_back("note", *file.note);
        pinned.emplace_back(std::move(f));
    }
    json::Object out{{"instructions", s.instructions}, {"pinned", std::move(pinned)}};
    if (s.digest_model) out.emplace_back("digest_model", *s.digest_model);
    if (s.folder) out.emplace_back("folder", s.folder->string());
    if (s.ideas_in_context) out.emplace_back("ideas_in_context", true);
    return out;
}

std::expected<ProjectSettings, std::string> settings_from_json(const json::Value& v) {
    if (!v.is_object()) return std::unexpected("invalid type: " + type_name(v) + ", expected struct ProjectSettings");
    ProjectSettings s;
    if (const auto* i = v.find("instructions")) {
        const auto* str = i->as_string();
        if (!str) return std::unexpected("invalid type: " + type_name(*i) + ", expected a string");
        s.instructions = *str;
    }
    if (const auto* p = v.find("pinned")) {
        const auto* items = p->as_array();
        if (!items) return std::unexpected("invalid type: " + type_name(*p) + ", expected a sequence");
        for (const auto& item : *items) {
            if (!item.is_object()) return std::unexpected("invalid type: " + type_name(item) + ", expected struct PinnedFile");
            const auto* path = item.find("path");
            if (!path) return std::unexpected("missing field `path`");
            const auto* str = path->as_string();
            if (!str) return std::unexpected("invalid type: " + type_name(*path) + ", expected a string");
            auto note = opt_string(item, "note");
            if (!note) return std::unexpected(note.error());
            s.pinned.push_back(PinnedFile{*str, std::move(*note)});
        }
    }
    auto model = opt_string(v, "digest_model");
    if (!model) return std::unexpected(model.error());
    s.digest_model = std::move(*model);
    auto folder = opt_string(v, "folder");
    if (!folder) return std::unexpected(folder.error());
    if (*folder) s.folder = fs::path(**folder);
    if (const auto* b = v.find("ideas_in_context")) {
        const auto* flag = b->as_bool();
        if (!flag) return std::unexpected("invalid type: " + type_name(*b) + ", expected a boolean");
        s.ideas_in_context = *flag;
    }
    return s;
}

fs::path FileSettingsStore::path(std::string_view project) const { return dir_ / (encode_name(project) + ".json"); }

fs::path FileSettingsStore::ideas_path(std::string_view project) const {
    return dir_ / (encode_name(project) + ".ideas.md");
}

Result<ProjectSettings> FileSettingsStore::load(std::string_view project) const {
    const auto file = path(project);
    auto bytes = runtime::fs::read_optional(file);
    if (!bytes) return std::unexpected(Error::io(bytes.error().to_string()));
    if (!*bytes) return ProjectSettings{};
    auto value = json::parse(**bytes);
    if (!value) return std::unexpected(at(file, value.error().message()));
    auto settings = settings_from_json(*value);
    if (!settings) return std::unexpected(at(file, settings.error()));
    return std::move(*settings);
}

Result<> FileSettingsStore::save(std::string_view project, const ProjectSettings& settings) const {
    const auto file = path(project);
    if (auto r = runtime::fs::write_atomic(file, json::dump_pretty(settings_to_json(settings))); !r)
        return std::unexpected(at(file, r.error().to_string()));
    return {};
}

Result<std::vector<std::string>> FileSettingsStore::names() const {
    std::vector<std::string> names;
    std::error_code ec;
    fs::directory_iterator it(dir_, ec);
    if (ec == std::errc::no_such_file_or_directory) return names;
    if (ec) return std::unexpected(Error::io(ec.message()));
    for (; it != fs::directory_iterator(); it.increment(ec)) {
        if (ec) return std::unexpected(Error::io(ec.message()));
        const auto file = it->path().filename().string();
        if (!file.ends_with(".json")) continue;
        auto name = decode_name(std::string_view(file).substr(0, file.size() - 5));
        if (!name) continue;
        // Only the file `path` writes for a name counts, so strays are skipped.
        if (path(*name).filename() != it->path().filename()) continue;
        const auto status = it->symlink_status(ec);
        if (ec) return std::unexpected(Error::io(ec.message()));
        if (status.type() == fs::file_type::regular) names.push_back(std::move(*name));
    }
    std::ranges::sort(names);
    return names;
}

// The notes go first: should that fail, the project is left whole.
Result<> FileSettingsStore::remove(std::string_view project) const {
    if (auto r = remove_file(ideas_path(project)); !r) return r;
    return remove_file(path(project));
}

Result<Ideas> FileSettingsStore::load_ideas(std::string_view project) const {
    const auto file = ideas_path(project);
    auto snapshot = runtime::fs::Snapshot::load(file);
    if (!snapshot) return std::unexpected(at(file, snapshot.error().to_string()));
    std::string text = snapshot->contents().value_or("");
    if (!valid_utf8(text)) return std::unexpected(at(file, "invalid utf-8 sequence"));
    return Ideas{std::move(text), std::move(*snapshot)};
}

Result<runtime::fs::Snapshot> FileSettingsStore::save_ideas(std::string_view project, runtime::fs::Snapshot seen,
                                                            std::string_view text) const {
    const auto file = ideas_path(project);
    if (seen.path() != file)
        return std::unexpected(Error::port(seen.path().string() + " isn't the ideas notes file of " + std::string(project)));
    auto saved = seen.save(text);
    if (!saved) return std::unexpected(at(file, saved.error().to_string()));
    if (!*saved) return std::unexpected(Error::changed_on_disk(file));
    return seen;
}

bool is_excluded(const core::Dom& dom) {
    const auto* root = dom.root();
    return root && root->bool_attr(core::attr::EXCLUDED) == true;
}

std::optional<core::Draft> exclude_draft(const core::Dom& dom, bool excluded) {
    const auto* root = dom.root();
    if (!root || is_excluded(dom) == excluded) return std::nullopt;
    return excluded ? core::Draft::set(root->id, std::string(core::attr::EXCLUDED), true)
                    : core::Draft::clear(root->id, std::string(core::attr::EXCLUDED));
}

} // namespace loa::projects
