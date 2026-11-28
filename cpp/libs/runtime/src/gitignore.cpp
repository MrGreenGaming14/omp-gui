#include "loa/runtime/gitignore.hpp"

#include <algorithm>
#include <cctype>
#include <fstream>
#include <iterator>
#include <optional>
#include <regex>
#include <system_error>

#include "sys_error.hpp"

namespace loa::runtime::gitignore {

namespace stdfs = std::filesystem;

struct Gitignore::Rule {
    std::regex re;
    bool whitelist = false;
    bool only_dir = false;
};

namespace {

void escape_into(std::string& re, char c) {
    if (std::string_view(R"(\^$.|?*+()[]{})").find(c) != std::string_view::npos) re += '\\';
    re += c;
}

// globset's translation with `literal_separator(true)` and `backslash_escape(true)`:
// `*`/`?` stay within a component; `**` spans components only as a whole component.
std::optional<std::string> glob_to_regex(std::string_view g) {
    std::string re = "^";
    std::size_t i = 0;
    while (i < g.size()) {
        const char c = g[i];
        if (c == '*' && i + 1 < g.size() && g[i + 1] == '*') {
            const bool at_start = i == 0 || g[i - 1] == '/';
            const bool at_end = i + 2 == g.size() || g[i + 2] == '/';
            if (at_start && at_end) {
                if (i == 0 && i + 2 == g.size()) {
                    re += ".*";
                    i += 2;
                } else if (i == 0) {
                    re += "(?:.*/)?"; // `**/` prefix
                    i += 3;
                } else if (i + 2 == g.size()) {
                    re += ".*"; // `/**` suffix; the `/` is already in `re`
                    i += 2;
                } else {
                    re += "(?:.*/)?"; // `/**/` infix; the leading `/` is already in `re`
                    i += 3;
                }
                continue;
            }
            re += "[^/]*[^/]*";
            i += 2;
            continue;
        }
        if (c == '*') {
            re += "[^/]*";
        } else if (c == '?') {
            re += "[^/]";
        } else if (c == '\\') {
            if (i + 1 < g.size()) escape_into(re, g[++i]);
            else escape_into(re, '\\');
        } else if (c == '[') {
            std::size_t j = i + 1;
            bool negated = false;
            if (j < g.size() && (g[j] == '!' || g[j] == '^')) {
                negated = true;
                ++j;
            }
            std::string cls;
            bool first = true;
            for (; j < g.size() && (first || g[j] != ']'); ++j, first = false) {
                if (g[j] == '-' && !first && j + 1 < g.size() && g[j + 1] != ']') {
                    cls += '-';
                } else {
                    if (std::string_view(R"(\]^[-)").find(g[j]) != std::string_view::npos) cls += '\\';
                    cls += g[j];
                }
            }
            if (j >= g.size()) return std::nullopt; // unclosed class
            re += negated ? "[^/" + cls + "]" : "[" + cls + "]";
            i = j;
        } else {
            escape_into(re, c);
        }
        ++i;
    }
    re += "$";
    return re;
}

// The `/`-joined path of `path` relative to `root`, or `path` itself when not under it.
std::string candidate_of(const Path& root, const Path& path) {
    Path rel = path;
    if (path.is_absolute() && !root.empty()) {
        auto [ri, pi] = std::mismatch(root.begin(), root.end(), path.begin(), path.end());
        if (ri == root.end() || (ri->empty() && std::next(ri) == root.end())) {
            rel.clear();
            for (; pi != path.end(); ++pi) rel /= *pi;
        }
    }
    std::string out = rel.generic_string();
    while (out.starts_with("./")) out.erase(0, 2);
    return out;
}

} // namespace

Gitignore Gitignore::parse(Path root, std::string_view text) {
    Gitignore gi;
    gi.root_ = std::move(root);
    std::size_t pos = 0;
    while (pos <= text.size()) {
        auto nl = text.find('\n', pos);
        auto line = text.substr(pos, nl == std::string_view::npos ? std::string_view::npos : nl - pos);
        pos = nl == std::string_view::npos ? text.size() + 1 : nl + 1;
        if (line.starts_with('#')) continue;
        if (!line.ends_with("\\ ")) {
            while (!line.empty() && std::isspace(static_cast<unsigned char>(line.back()))) line.remove_suffix(1);
        }
        if (line.empty()) continue;
        Rule rule;
        bool absolute = false;
        if (line.starts_with("\\!") || line.starts_with("\\#")) {
            line.remove_prefix(1);
            absolute = line.starts_with('/');
        } else {
            if (line.starts_with('!')) {
                rule.whitelist = true;
                line.remove_prefix(1);
            }
            if (line.starts_with('/')) {
                line.remove_prefix(1);
                absolute = true;
            }
        }
        if (line.ends_with('/')) {
            rule.only_dir = true;
            line.remove_suffix(1);
            if (line.ends_with('\\')) line.remove_suffix(1);
        }
        std::string actual(line);
        if (!absolute && actual.find('/') == std::string::npos && !(actual.starts_with("**/") || actual == "**"))
            actual = "**/" + actual;
        if (actual.ends_with("/**")) actual += "/*";
        auto re = glob_to_regex(actual);
        if (!re) continue;
        try {
            rule.re = std::regex(*re, std::regex::ECMAScript | std::regex::optimize);
        } catch (const std::regex_error&) {
            continue;
        }
        gi.rules_.push_back(std::make_shared<const Rule>(std::move(rule)));
    }
    return gi;
}

Gitignore Gitignore::from_file(Path root, const Path& file) {
    std::ifstream in(file, std::ios::binary);
    if (!in) {
        Gitignore gi;
        gi.root_ = std::move(root);
        return gi;
    }
    const std::string text{std::istreambuf_iterator<char>(in), {}};
    return parse(std::move(root), text);
}

Match Gitignore::matched(const Path& path, bool is_dir) const {
    if (rules_.empty()) return Match::none;
    const auto candidate = candidate_of(root_, path);
    for (auto it = rules_.rbegin(); it != rules_.rend(); ++it) {
        const auto& rule = **it;
        if (rule.only_dir && !is_dir) continue;
        if (std::regex_match(candidate, rule.re)) return rule.whitelist ? Match::whitelist : Match::ignore;
    }
    return Match::none;
}

Matcher::Level Matcher::load(const Path& dir) {
    std::error_code ec;
    return {Gitignore::from_file(dir, dir / ".ignore"), Gitignore::from_file(dir, dir / ".gitignore"),
            stdfs::exists(dir / ".git", ec)};
}

Matcher Matcher::for_dir(const Path& dir_in) {
    std::error_code ec;
    auto dir = stdfs::absolute(dir_in, ec);
    if (ec) dir = dir_in;
    dir = dir.lexically_normal();
    if (!dir.has_filename() && dir.has_relative_path()) dir = dir.parent_path();
    std::vector<Path> chain;
    for (auto p = dir;; p = p.parent_path()) {
        chain.push_back(p);
        if (!p.has_relative_path()) break;
    }
    Matcher m;
    for (auto it = chain.rbegin(); it != chain.rend(); ++it) m.levels_.push_back(load(*it));
    // The innermost repository's `.git/info/exclude`.
    for (const auto& p : chain)
        if (stdfs::exists(p / ".git", ec)) {
            m.exclude_ = Gitignore::from_file(p, p / ".git" / "info" / "exclude");
            break;
        }
    return m;
}

Matcher Matcher::descend(const Path& child) const {
    Matcher m = *this;
    m.levels_.push_back(load(child));
    if (m.levels_.back().has_git) m.exclude_ = Gitignore::from_file(child, child / ".git" / "info" / "exclude");
    return m;
}

Match Matcher::matched(const Path& path, bool is_dir) const {
    Match m_ignore = Match::none, m_git = Match::none;
    bool saw_git = false;
    for (auto it = levels_.rbegin(); it != levels_.rend(); ++it) {
        if (m_ignore == Match::none) m_ignore = it->ignore.matched(path, is_dir);
        if (!saw_git && m_git == Match::none) m_git = it->git.matched(path, is_dir);
        saw_git = saw_git || it->has_git;
    }
    if (m_ignore != Match::none) return m_ignore;
    if (m_git != Match::none) return m_git;
    return exclude_.matched(path, is_dir);
}

std::expected<std::vector<Entry>, Error> list_dir(const Path& dir) {
    std::error_code ec;
    stdfs::directory_iterator it(dir, ec);
    if (ec) return std::unexpected(detail::os_error(ec, dir.string()));
    const auto matcher = Matcher::for_dir(dir);
    std::vector<Entry> out;
    for (stdfs::directory_iterator end; it != end; it.increment(ec)) {
        if (ec) break;
        const auto& entry = *it;
        auto name = entry.path().filename().string();
        if (name == ".git") continue;
        std::error_code type_ec;
        const bool is_dir = entry.is_symlink(type_ec) ? stdfs::is_directory(entry.path(), type_ec) : entry.is_directory(type_ec);
        if (matcher.is_ignored(stdfs::absolute(entry.path(), type_ec), is_dir)) continue;
        out.push_back({std::move(name), is_dir});
    }
    return out;
}

} // namespace loa::runtime::gitignore
