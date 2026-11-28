#include "loa/harness/native.hpp"

#include <algorithm>
#include <cstdlib>
#include <fstream>
#include <iterator>

#include "kit/utf8.hpp"
#include "loa/runtime/json.hpp"

namespace loa::harness::native {

namespace fs = std::filesystem;
namespace json = loa::runtime::json;
using kit::NativeSession;

namespace {

// The first line of `path` (without `\n` or `\r\n`), when the file opens and has one.
std::optional<std::string> first_line(const fs::path& path) {
    std::ifstream in(path, std::ios::binary);
    if (!in) return std::nullopt;
    std::string line;
    if (!std::getline(in, line) && line.empty()) return std::nullopt;
    if (line.ends_with('\r')) line.pop_back();
    return line;
}

std::string_view type_of(const json::Value& v) {
    const json::Value* type = v.find("type");
    const std::string* s = type ? type->as_string() : nullptr;
    return s ? std::string_view(*s) : std::string_view();
}

} // namespace

std::vector<NativeSession> discover_omp(const fs::path& home) {
    std::vector<NativeSession> out;
    auto files = kit::walk(home / ".omp/agent/sessions", 1, [](std::string_view name) { return name.ends_with(".jsonl"); });
    for (auto& path : files) {
        std::optional<std::string> title;
        if (auto line = first_line(path)) {
            if (auto value = json::parse(*line); value && type_of(*value) == "title") {
                const json::Value* t = value->find("title");
                if (t && t->as_string()) title = *t->as_string();
            }
        }
        auto project = kit::parent_name(path);
        if (auto s = NativeSession::at(OMP, std::move(path), std::move(project), std::move(title))) out.push_back(std::move(*s));
    }
    return out;
}

std::vector<NativeSession> discover_claude(const fs::path& home) {
    std::vector<NativeSession> out;
    auto files = kit::walk(home / ".claude/projects", 1, [](std::string_view name) { return name.ends_with(".jsonl"); });
    for (auto& path : files) {
        auto project = kit::parent_name(path);
        if (auto s = NativeSession::at(CLAUDE, std::move(path), std::move(project), std::nullopt)) out.push_back(std::move(*s));
    }
    return out;
}

std::vector<NativeSession> discover_codex(const fs::path& home) {
    const char* env = std::getenv("CODEX_HOME");
    const fs::path codex_home = env && *env ? fs::path(env) : home / ".codex";
    std::vector<NativeSession> out;
    auto files = kit::walk(codex_home / "sessions", 3, [](std::string_view name) {
        return name.starts_with("rollout-") && name.ends_with(".jsonl");
    });
    for (auto& path : files)
        if (auto s = NativeSession::at(CODEX, std::move(path), std::nullopt, std::nullopt)) out.push_back(std::move(*s));
    return out;
}

std::vector<NativeSession> discover(const fs::path& home) {
    std::vector<NativeSession> out = discover_omp(home);
    for (auto* more : {&discover_claude, &discover_codex}) {
        auto found = more(home);
        out.insert(out.end(), std::make_move_iterator(found.begin()), std::make_move_iterator(found.end()));
    }
    std::ranges::stable_sort(out, [](const NativeSession& a, const NativeSession& b) {
        if (a.modified != b.modified) return a.modified > b.modified;
        return a.path < b.path;
    });
    return out;
}

std::optional<std::string_view> harness_of(const fs::path& path) {
    const std::string text = path.string();
    if (text.find("/.omp/") != std::string::npos) return OMP;
    if (text.find("/.claude/") != std::string::npos) return CLAUDE;
    if (path.has_filename() && path.filename().string().starts_with("rollout-")) return CODEX;
    std::ifstream in(path, std::ios::binary);
    if (!in) return std::nullopt;
    std::string raw(4096, '\0');
    in.read(raw.data(), static_cast<std::streamsize>(raw.size()));
    if (in.bad()) return std::nullopt;
    raw.resize(static_cast<std::size_t>(in.gcount()));
    const std::string head = kit::detail::utf8_lossy(raw);
    if (path.extension() == ".json") {
        const auto start = head.find_first_not_of(" \t\n\r\f\v");
        if (start != std::string::npos && head[start] == '{' && head.find("\"info\"") != std::string::npos) return OPENCODE;
    }
    if (head.empty()) return std::nullopt;
    std::string_view first(head);
    first = first.substr(0, first.find('\n'));
    if (first.ends_with('\r')) first.remove_suffix(1);
    auto value = json::parse(first);
    if (!value) return std::nullopt;
    const std::string_view type = type_of(*value);
    if (type == "title" || type == "session") return OMP;
    if (type == "session_meta") return CODEX;
    if (value->find("sessionId")) return CLAUDE;
    return std::nullopt;
}

} // namespace loa::harness::native
