// Parser for unified diffs, plain (`diff -u`) or git style, with one or many files.
// Port of `crates/loa-ui-panel/src/diff/parse.rs`.

#include <array>
#include <format>
#include <limits>

#include "diff_builder.hpp"

namespace loa::ui::panel::diff {

namespace {

// A file being collected; `git` when `diff --git` started it, so `---`/`+++` belong to it.
struct Pending {
    std::optional<std::string> old_path, new_path;
    bool git = false, has_hunks = false, binary = false;
    Builder builder;
    FileDiff finish() { return builder.finish(std::move(old_path), std::move(new_path), binary); }
};

// Lines still expected in the current hunk (old, new), and the next line numbers.
struct HunkState {
    std::array<std::uint32_t, 2> left{}, next{};
    [[nodiscard]] bool open() const { return left[0] != 0 || left[1] != 0; }
};

bool strip_prefix(std::string_view& s, std::string_view p) {
    if (!s.starts_with(p)) return false;
    s.remove_prefix(p.size());
    return true;
}

bool is_ws(char c) { return c == ' ' || c == '\t' || c == '\n' || c == '\r' || c == '\v' || c == '\f'; }
std::string_view trim_start(std::string_view s) {
    while (!s.empty() && is_ws(s.front())) s.remove_prefix(1);
    return s;
}
std::string_view trim_end(std::string_view s) {
    while (!s.empty() && is_ws(s.back())) s.remove_suffix(1);
    return s;
}

// Rust's `u32::from_str`: an optional `+`, then at least one digit, no overflow.
std::optional<std::uint32_t> parse_u32(std::string_view s) {
    if (s.starts_with('+')) s.remove_prefix(1);
    if (s.empty()) return std::nullopt;
    std::uint64_t v = 0;
    for (const char c : s) {
        if (c < '0' || c > '9') return std::nullopt;
        v = v * 10 + static_cast<std::uint64_t>(c - '0');
        if (v > std::numeric_limits<std::uint32_t>::max()) return std::nullopt;
    }
    return static_cast<std::uint32_t>(v);
}

std::optional<std::pair<std::uint32_t, std::uint32_t>> range(std::string_view t) {
    if (const auto comma = t.find(','); comma != std::string_view::npos) {
        const auto s = parse_u32(t.substr(0, comma));
        const auto l = parse_u32(t.substr(comma + 1));
        if (!s || !l) return std::nullopt;
        return std::pair{*s, *l};
    }
    const auto s = parse_u32(t);
    if (!s) return std::nullopt;
    return std::pair{*s, 1U};
}

// Parses `-a[,b] +c[,d] @@ section`; a zero length side names the line before the hunk.
std::optional<std::pair<HunkState, std::string_view>> hunk_header(std::string_view rest) {
    const auto at = rest.find("@@");
    if (at == std::string_view::npos) return std::nullopt;
    auto ranges = rest.substr(0, at);
    const auto section = rest.substr(at + 2);
    std::vector<std::string_view> parts;
    while (true) {
        ranges = trim_start(ranges);
        if (ranges.empty()) break;
        std::size_t n = 0;
        while (n < ranges.size() && !is_ws(ranges[n])) ++n;
        parts.push_back(ranges.substr(0, n));
        ranges.remove_prefix(n);
    }
    if (parts.size() < 2) return std::nullopt;
    if (!strip_prefix(parts[0], "-") || !strip_prefix(parts[1], "+")) return std::nullopt;
    const auto o = range(parts[0]);
    const auto n = range(parts[1]);
    if (!o || !n || parts.size() != 2) return std::nullopt;
    auto first = [](std::uint32_t s, std::uint32_t l) { return l == 0 ? s + 1 : s; };
    return std::pair{HunkState{{o->second, n->second}, {first(o->first, o->second), first(n->first, n->second)}}, trim_start(section)};
}

// The path of a `---`/`+++` line without timestamp or `a/`/`b/`; `/dev/null` is no file.
std::optional<std::string> header_path(std::string_view rest, std::string_view prefix) {
    auto path = trim_end(rest.substr(0, rest.find('\t')));
    if (path == "/dev/null") return std::nullopt;
    strip_prefix(path, prefix);
    return std::string(path);
}

std::size_t utf8_len(unsigned char c) { return c < 0x80 ? 1 : c >= 0xF0 ? 4 : c >= 0xE0 ? 3 : c >= 0xC0 ? 2 : 1; }

std::expected<void, ParseError> hunk_line(Builder& b, HunkState& h, std::string_view line, std::size_t number) {
    std::string_view marker = line, text;
    if (!line.empty()) {
        const auto first = std::min(utf8_len(static_cast<unsigned char>(line[0])), line.size());
        if (first < line.size()) marker = line.substr(0, first), text = line.substr(first);
    }
    Tag tag{};
    std::array<bool, 2> sides{};
    // Some tools strip the space of empty context lines.
    if (marker == " " || marker.empty()) tag = Tag::Equal, sides = {true, true};
    else if (marker == "-") tag = Tag::Delete, sides = {true, false};
    else if (marker == "+") tag = Tag::Insert, sides = {false, true};
    // "\ No newline at end of file"
    else if (marker == "\\") return {};
    else return std::unexpected(ParseError{ParseError::Kind::Truncated, number, h.left[0], h.left[1]});
    for (std::size_t s = 0; s < 2; ++s) {
        if (!sides[s]) continue;
        if (h.left[s] == 0) return std::unexpected(ParseError{ParseError::Kind::Overflow, number});
        --h.left[s];
    }
    b.push(tag, sides[0] ? std::optional(h.next[0]) : std::nullopt, sides[1] ? std::optional(h.next[1]) : std::nullopt, text);
    for (std::size_t s = 0; s < 2; ++s) h.next[s] += sides[s] ? 1 : 0;
    return {};
}

} // namespace

std::string ParseError::message() const {
    switch (kind) {
    case Kind::HunkHeader: return std::format("line {}: malformed hunk header", line);
    case Kind::Orphan: return std::format("line {}: hunk or file header outside a file", line);
    case Kind::Overflow: return std::format("line {}: hunk has more lines than its header says", line);
    case Kind::Truncated: return std::format("line {}: hunk ends with {} old and {} new lines missing", line, old_left, new_left);
    }
    return {};
}

std::expected<std::vector<FileDiff>, ParseError> parse_unified(std::string_view input) {
    std::vector<FileDiff> files;
    std::optional<Pending> current;
    std::optional<HunkState> hunk;
    std::size_t last = 0;
    auto flush = [&] {
        if (current) files.push_back(current->finish());
        current.reset();
    };
    std::size_t pos = 0, index = 0;
    while (pos < input.size()) {
        const auto nl = input.find('\n', pos);
        const auto stop = nl == std::string_view::npos ? input.size() : nl + 1;
        auto line = input.substr(pos, stop - pos);
        pos = stop;
        const auto number = ++index;
        while (line.ends_with('\n')) line.remove_suffix(1);
        while (line.ends_with('\r')) line.remove_suffix(1);
        last = number;
        if (hunk && hunk->open()) {
            if (!current) return std::unexpected(ParseError{ParseError::Kind::Orphan, number});
            if (auto r = hunk_line(current->builder, *hunk, line, number); !r) return std::unexpected(r.error());
            continue;
        }
        hunk.reset();
        std::string_view rest = line;
        if (strip_prefix(rest, "diff --git ")) {
            flush();
            rest = trim_end(rest);
            std::string_view old = rest, nw = rest;
            std::string_view r = rest;
            std::size_t b = std::string_view::npos;
            if (strip_prefix(r, "a/") && (b = r.rfind(" b/")) != std::string_view::npos) {
                old = r.substr(0, b), nw = r.substr(b + 3);
            } else if (const auto sp = rest.find(' '); sp != std::string_view::npos) {
                old = rest.substr(0, sp), nw = rest.substr(sp + 1);
            }
            current.emplace();
            current->old_path = std::string(old), current->new_path = std::string(nw), current->git = true;
        } else if (strip_prefix(rest, "--- ")) {
            if (current && current->git && !current->has_hunks) {
                current->old_path = header_path(rest, "a/");
            } else {
                flush();
                current.emplace();
                current->old_path = header_path(rest, "a/");
            }
        } else if (strip_prefix(rest, "+++ ")) {
            if (!current) return std::unexpected(ParseError{ParseError::Kind::Orphan, number});
            current->new_path = header_path(rest, "b/");
        } else if (strip_prefix(rest, "@@ ")) {
            if (!current) return std::unexpected(ParseError{ParseError::Kind::Orphan, number});
            const auto h = hunk_header(rest);
            if (!h) return std::unexpected(ParseError{ParseError::Kind::HunkHeader, number});
            current->builder.hunk_break(h->second);
            current->has_hunks = true;
            hunk = h->first;
        } else if (current) {
            // Git's extended header lines between `diff --git` and the hunks.
            if (strip_prefix(rest, "rename from ")) current->old_path = std::string(rest);
            else if (strip_prefix(rest, "rename to ")) current->new_path = std::string(rest);
            else if (line.starts_with("new file mode")) current->old_path.reset();
            else if (line.starts_with("deleted file mode")) current->new_path.reset();
            else if (line.starts_with("Binary files ") || line == "GIT binary patch") current->binary = true;
        }
    }
    if (hunk && hunk->open()) return std::unexpected(ParseError{ParseError::Kind::Truncated, last, hunk->left[0], hunk->left[1]});
    flush();
    return files;
}

} // namespace loa::ui::panel::diff
