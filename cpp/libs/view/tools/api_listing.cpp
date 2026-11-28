#include "api_listing.hpp"

#include <algorithm>
#include <array>
#include <cctype>
#include <charconv>
#include <format>
#include <fstream>
#include <iterator>
#include <optional>
#include <set>
#include <stdexcept>
#include <tuple>
#include <vector>

namespace loa::view::api {

namespace {

constexpr std::string_view HEADER = "version = ";

// Drops comments and preprocessor lines, keeping string and char literals.
std::string strip(std::string_view in) {
    std::string out;
    bool line_start = true;
    for (std::size_t i = 0; i < in.size();) {
        const char c = in[i];
        if (line_start && c == '#') {
            while (i < in.size() && in[i] != '\n') ++i;
            continue;
        }
        if (c == '/' && i + 1 < in.size() && in[i + 1] == '/') {
            while (i < in.size() && in[i] != '\n') ++i;
            continue;
        }
        if (c == '/' && i + 1 < in.size() && in[i + 1] == '*') {
            const auto end = in.find("*/", i + 2);
            i = end == std::string_view::npos ? in.size() : end + 2;
            out += ' ';
            continue;
        }
        if (c == '"' || c == '\'') {
            out += c;
            for (++i; i < in.size() && in[i] != c; ++i) {
                if (in[i] == '\\' && i + 1 < in.size()) out += in[i++];
                out += in[i];
            }
            if (i < in.size()) out += in[i++];
            line_start = false;
            continue;
        }
        if (c == '\n') line_start = true;
        else if (!std::isspace(static_cast<unsigned char>(c))) line_start = false;
        out += c;
        ++i;
    }
    return out;
}

// Collapses whitespace and drops it next to brackets and before `,` and `;`.
std::string normalize(std::string_view in) {
    std::string out;
    bool space = false;
    for (char c : in) {
        if (std::isspace(static_cast<unsigned char>(c))) {
            space = !out.empty();
            continue;
        }
        if (space && !std::string_view("),;]").contains(c) && !std::string_view("([").contains(out.back())) out += ' ';
        space = false;
        out += c;
    }
    return out;
}

struct Cursor {
    std::string_view text;
    std::size_t pos = 0;

    void skip_space() {
        while (pos < text.size() && std::isspace(static_cast<unsigned char>(text[pos]))) ++pos;
    }
    [[nodiscard]] bool done() const { return pos >= text.size(); }
    [[nodiscard]] char peek() const { return text[pos]; }

    // Position after the bracket matching the one at `pos`.
    std::size_t match(std::size_t at) const {
        int depth = 0;
        for (std::size_t i = at; i < text.size(); ++i) {
            const char c = text[i];
            if (c == '"' || c == '\'') {
                for (++i; i < text.size() && text[i] != c; ++i)
                    if (text[i] == '\\') ++i;
                continue;
            }
            if (c == '{' || c == '(' || c == '[') ++depth;
            if (c == '}' || c == ')' || c == ']') {
                if (--depth == 0) return i + 1;
            }
        }
        throw std::runtime_error("unbalanced brackets");
    }

    // Reads up to a top-level `;` (consumed) or `{` (not consumed); parentheses nest.
    std::string_view statement(char& stop) {
        const auto start = pos;
        while (pos < text.size()) {
            const char c = text[pos];
            if (c == '(' || c == '[' || c == '"' || c == '\'') {
                pos = c == '"' || c == '\'' ? skip_literal(pos) : match(pos);
                continue;
            }
            if (c == ';' || c == '{' || c == '}') {
                stop = c;
                const auto out = text.substr(start, pos - start);
                if (c == ';') ++pos;
                return out;
            }
            ++pos;
        }
        stop = 0;
        return text.substr(start);
    }

    std::size_t skip_literal(std::size_t at) const {
        const char q = text[at];
        std::size_t i = at + 1;
        for (; i < text.size() && text[i] != q; ++i)
            if (text[i] == '\\') ++i;
        return i + 1;
    }
};

// The keyword (`struct`, `class`, `enum`) when `head` declares a type, after any `template<…>`.
std::optional<std::string_view> type_keyword(std::string_view head) {
    auto h = normalize(head);
    if (h.starts_with("template")) {
        const auto close = h.find('>');
        if (close == std::string::npos) return std::nullopt;
        h = normalize(h.substr(close + 1));
    }
    for (std::string_view kw : {"struct", "class", "enum", "union"})
        if (h.starts_with(kw) && (h.size() == kw.size() || h[kw.size()] == ' ')) return kw;
    return std::nullopt;
}

// `head` with the declared name qualified by `ns`.
std::string qualify(std::string_view head, const std::string& ns) {
    auto h = normalize(head);
    const auto kw = *type_keyword(h);
    auto at = h.find(kw);
    at += kw.size() + 1;
    if (kw == "enum" && (h.compare(at, 6, "class ") == 0 || h.compare(at, 7, "struct ") == 0))
        at = h.find(' ', at) + 1;
    return h.substr(0, at) + ns + "::" + h.substr(at);
}

void type_body(Cursor& c, std::string_view keyword, int indent, std::vector<std::string>& lines);

// Members of a struct or class body starting after `{`; leaves the cursor after the `}`.
void members(Cursor& c, bool is_class, int indent, std::vector<std::string>& lines) {
    bool visible = !is_class;
    const std::string pad(static_cast<std::size_t>(indent), ' ');
    for (;;) {
        c.skip_space();
        if (c.done()) throw std::runtime_error("unterminated type body");
        if (c.peek() == '}') {
            ++c.pos;
            return;
        }
        for (std::string_view access : {"public:", "private:", "protected:"}) {
            if (c.text.substr(c.pos).starts_with(access)) {
                visible = access == "public:";
                c.pos += access.size();
                goto next;
            }
        }
        {
            char stop = 0;
            const auto head = c.statement(stop);
            if (stop == '{') {
                if (auto kw = type_keyword(head)) {
                    std::vector<std::string> nested;
                    ++c.pos;
                    type_body(c, *kw, indent + 2, nested);
                    c.skip_space();
                    if (!c.done() && c.peek() == ';') ++c.pos;
                    if (visible) {
                        lines.push_back(pad + normalize(head));
                        lines.insert(lines.end(), nested.begin(), nested.end());
                    }
                    continue;
                }
                const auto open = c.pos;
                c.pos = c.match(open);
                c.skip_space();
                std::string text = normalize(head);
                if (!c.done() && c.peek() == ';') {
                    // A brace initializer: part of the member.
                    text = normalize(std::string(head) + std::string(c.text.substr(open, c.pos - open)));
                    ++c.pos;
                }
                if (visible) lines.push_back(pad + text);
                continue;
            }
            if (stop == '}') continue;
            if (visible && !normalize(head).empty()) lines.push_back(pad + normalize(head));
        }
    next:;
    }
}

void type_body(Cursor& c, std::string_view keyword, int indent, std::vector<std::string>& lines) {
    if (keyword != "enum") {
        members(c, keyword == "class", indent, lines);
        return;
    }
    const auto open = c.pos - 1;
    const auto close = c.match(open);
    const auto body = c.text.substr(open + 1, close - open - 2);
    c.pos = close;
    std::string item;
    int depth = 0;
    auto flush = [&] {
        if (auto n = normalize(item); !n.empty()) lines.push_back(std::string(static_cast<std::size_t>(indent), ' ') + n);
        item.clear();
    };
    for (char ch : body) {
        if (ch == '(' || ch == '{') ++depth;
        if (ch == ')' || ch == '}') --depth;
        if (ch == ',' && depth == 0) flush();
        else item += ch;
    }
    flush();
}

// Declarations of a namespace body; stops after its `}` or at the end of the text.
void scope(Cursor& c, const std::string& ns, std::vector<std::string>& blocks) {
    for (;;) {
        c.skip_space();
        if (c.done()) return;
        if (c.peek() == '}') {
            ++c.pos;
            return;
        }
        char stop = 0;
        const auto head = c.statement(stop);
        const auto h = normalize(head);
        if (stop == '{' && h.starts_with("namespace")) {
            ++c.pos;
            auto name = normalize(h.substr(9));
            const auto inner = ns.empty() ? name : name.empty() ? ns : ns + "::" + name;
            scope(c, inner, blocks);
            continue;
        }
        if (stop == '{' && type_keyword(h)) {
            std::vector<std::string> lines{qualify(h, ns)};
            ++c.pos;
            type_body(c, *type_keyword(h), 2, lines);
            c.skip_space();
            if (!c.done() && c.peek() == ';') ++c.pos;
            std::string block;
            for (const auto& l : lines) block += l + '\n';
            blocks.push_back(std::move(block));
            continue;
        }
        if (stop == '{') {
            // A function body or a brace initializer: keep it, it is part of the API.
            const auto open = c.pos;
            c.pos = c.match(open);
            c.skip_space();
            if (!c.done() && c.peek() == ';') ++c.pos;
            blocks.push_back(ns + ": " + normalize(std::string(head) + std::string(c.text.substr(open, c.pos - open))) + '\n');
            continue;
        }
        if (stop == '}') continue;
        if (!h.empty()) blocks.push_back(ns + ": " + h + '\n');
    }
}

using Semver = std::tuple<std::uint64_t, std::uint64_t, std::uint64_t>;

std::optional<Semver> semver(std::string_view v) {
    std::array<std::uint64_t, 3> parts{};
    for (std::size_t i = 0; i < 3; ++i) {
        const auto end = i < 2 ? v.find('.') : v.size();
        if (end == std::string_view::npos) return std::nullopt;
        const auto piece = v.substr(0, end);
        const auto [ptr, ec] = std::from_chars(piece.data(), piece.data() + piece.size(), parts[i]);
        if (piece.empty() || ec != std::errc{} || ptr != piece.data() + piece.size()) return std::nullopt;
        v.remove_prefix(i < 2 ? end + 1 : end);
    }
    return Semver{parts[0], parts[1], parts[2]};
}

// Each line prefixed with the item (and member) it belongs to.
std::set<std::string> qualified(std::string_view listing) {
    std::set<std::string> out;
    std::string item, member;
    while (!listing.empty()) {
        const auto nl = listing.find('\n');
        const auto line = listing.substr(0, nl);
        listing.remove_prefix(nl == std::string_view::npos ? listing.size() : nl + 1);
        const auto lead = line.find_first_not_of(' ');
        const auto trimmed = lead == std::string_view::npos ? std::string_view{} : line.substr(lead);
        if (lead == 0) {
            item = line;
            out.emplace(line);
        } else if (lead == 2) {
            member = trimmed;
            out.insert(item + " › " + member);
        } else {
            out.insert(item + " › " + member + " › " + std::string(trimmed));
        }
    }
    return out;
}

// Lines only in `old` as `-`, only in `now` as `+`.
std::string diff(std::string_view old, std::string_view now) {
    const auto a = qualified(old), b = qualified(now);
    std::string out;
    for (const auto& l : a)
        if (!b.contains(l)) out += "-" + l + "\n";
    for (const auto& l : b)
        if (!a.contains(l)) out += "+" + l + "\n";
    return out;
}

} // namespace

std::string render(const std::filesystem::path& include_dir) {
    std::vector<std::filesystem::path> files;
    for (const auto& e : std::filesystem::directory_iterator(include_dir))
        if (e.is_regular_file() && e.path().extension() == ".hpp") files.push_back(e.path());
    std::vector<std::string> blocks;
    for (const auto& file : files) {
        std::ifstream in(file, std::ios::binary);
        const std::string text{std::istreambuf_iterator<char>(in), {}};
        const auto stripped = strip(text);
        Cursor c{stripped};
        scope(c, "", blocks);
    }
    std::ranges::sort(blocks);
    std::string out;
    for (const auto& b : blocks) out += b;
    return out;
}

GateResult gate(std::string_view saved, std::string_view rendered, std::string_view version, bool update) {
    std::string_view saved_version, saved_body;
    if (const auto nl = saved.find('\n'); nl != std::string_view::npos) {
        auto head = saved.substr(0, nl);
        saved_version = head.starts_with(HEADER) ? head.substr(HEADER.size()) : std::string_view{};
        saved_body = saved.substr(nl + 1);
    }
    if (rendered == saved_body && version == saved_version) return {true, false, {}};
    const auto d = diff(saved_body, rendered);
    if (rendered != saved_body && version == saved_version)
        return {false, false,
                "the loa::view API changed: bump loa::view::VERSION (major for removed or changed items, minor for "
                "additions), then run `UPDATE_VIEW_API=1 ctest -R loa_view_api`\n" + d};
    if (!saved_version.empty()) {
        const auto old = semver(saved_version), now = semver(version);
        if (!old || !now) return {false, false, "versions must be major.minor.patch"};
        if (!(*now > *old))
            return {false, false,
                    std::format("loa::view::VERSION {} must be greater than api.txt's {}", version, saved_version)};
        const bool removed = d.starts_with('-') || d.contains("\n-");
        const bool added = d.starts_with('+') || d.contains("\n+");
        auto major = [](Semver v) {
            return std::get<0>(v) == 0 ? std::pair{std::uint64_t{0}, std::get<1>(v)}
                                       : std::pair{std::get<0>(v), std::uint64_t{0}};
        };
        auto minor = [](Semver v) { return std::get<0>(v) == 0 ? v : Semver{std::get<0>(v), std::get<1>(v), 0}; };
        if (removed && !(major(*now) > major(*old)))
            return {false, false,
                    std::format("items were removed or changed, which needs a major bump of loa::view::VERSION (from "
                                "{}, now {})\n{}",
                                saved_version, version, d)};
        if (added && !(minor(*now) > minor(*old)))
            return {false, false,
                    std::format("items were added, which needs at least a minor bump of loa::view::VERSION (from {}, "
                                "now {})\n{}",
                                saved_version, version, d)};
    }
    if (!update)
        return {false, false,
                std::format("api.txt is stale for loa::view::VERSION {}: run `UPDATE_VIEW_API=1 ctest -R loa_view_api`\n{}",
                            version, d)};
    return {true, true, {}};
}

} // namespace loa::view::api
