#include "loa/settings/models/yaml.hpp"

#include <algorithm>
#include <charconv>
#include <cmath>
#include <cstdlib>
#include <limits>
#include <map>
#include <sstream>

#include <yaml-cpp/eventhandler.h>
#include <yaml-cpp/exceptions.h>
#include <yaml-cpp/mark.h>
#include <yaml-cpp/parser.h>

#include "text_util.hpp"

namespace loa::settings::models::yaml {

namespace json = runtime::json;

// ---------------------------------------------------------------------------
// Scalar resolution (yaml-rust2 `Yaml::from_str`, `parse_f64`)
// ---------------------------------------------------------------------------

namespace {

bool is_digit(char c) { return c >= '0' && c <= '9'; }

// Rust `i64::from_str_radix` / `str::parse::<i64>`: optional sign, then one or more digits.
std::optional<std::int64_t> parse_int(std::string_view s, int radix) {
    bool negative = false;
    if (!s.empty() && (s[0] == '+' || s[0] == '-')) {
        negative = s[0] == '-';
        s.remove_prefix(1);
    }
    if (s.empty()) {
        return std::nullopt;
    }
    // Accumulate as a negative number so i64::MIN fits.
    std::int64_t acc = 0;
    const std::int64_t min = std::numeric_limits<std::int64_t>::min();
    for (char c : s) {
        int d = 0;
        if (c >= '0' && c <= '9') {
            d = c - '0';
        } else if (c >= 'a' && c <= 'z') {
            d = c - 'a' + 10;
        } else if (c >= 'A' && c <= 'Z') {
            d = c - 'A' + 10;
        } else {
            return std::nullopt;
        }
        if (d >= radix) {
            return std::nullopt;
        }
        if (acc < (min + d) / radix) {
            return std::nullopt;
        }
        acc = acc * radix - d;
    }
    if (!negative) {
        if (acc == min) {
            return std::nullopt;
        }
        acc = -acc;
    }
    return acc;
}

// Rust `str::parse::<f64>` grammar for decimal text (inf/nan words are excluded by
// the caller's digit check).
bool rust_float_syntax(std::string_view s) {
    std::size_t i = 0;
    if (i < s.size() && (s[i] == '+' || s[i] == '-')) {
        ++i;
    }
    std::size_t digits = 0;
    while (i < s.size() && is_digit(s[i])) {
        ++i;
        ++digits;
    }
    if (i < s.size() && s[i] == '.') {
        ++i;
        while (i < s.size() && is_digit(s[i])) {
            ++i;
            ++digits;
        }
    }
    if (digits == 0) {
        return false;
    }
    if (i < s.size() && (s[i] == 'e' || s[i] == 'E')) {
        ++i;
        if (i < s.size() && (s[i] == '+' || s[i] == '-')) {
            ++i;
        }
        std::size_t exp = 0;
        while (i < s.size() && is_digit(s[i])) {
            ++i;
            ++exp;
        }
        if (exp == 0) {
            return false;
        }
    }
    return i == s.size();
}

std::optional<double> parse_f64(std::string_view v) {
    if (v == ".inf" || v == ".Inf" || v == ".INF" || v == "+.inf" || v == "+.Inf" || v == "+.INF") {
        return std::numeric_limits<double>::infinity();
    }
    if (v == "-.inf" || v == "-.Inf" || v == "-.INF") {
        return -std::numeric_limits<double>::infinity();
    }
    if (v == ".nan" || v == ".NaN" || v == ".NAN") {
        return std::numeric_limits<double>::quiet_NaN();
    }
    if (!std::ranges::any_of(v, is_digit) || !rust_float_syntax(v)) {
        return std::nullopt;
    }
    const std::string text(v);
    return std::strtod(text.c_str(), nullptr);
}

} // namespace

Yaml Yaml::from_str(std::string_view v) {
    if (v.starts_with("0x")) {
        if (auto i = parse_int(v.substr(2), 16)) {
            return Yaml(*i);
        }
    } else if (v.starts_with("0o")) {
        if (auto i = parse_int(v.substr(2), 8)) {
            return Yaml(*i);
        }
    } else if (v.starts_with('+')) {
        if (auto i = parse_int(v.substr(1), 10)) {
            return Yaml(*i);
        }
    }
    if (v.empty() || v == "~" || v == "null") {
        return Yaml(Null{});
    }
    if (v == "true" || v == "True" || v == "TRUE") {
        return Yaml(true);
    }
    if (v == "false" || v == "False" || v == "FALSE") {
        return Yaml(false);
    }
    if (auto i = parse_int(v, 10)) {
        return Yaml(*i);
    }
    if (parse_f64(v)) {
        return Yaml(Real{std::string(v)});
    }
    return Yaml(std::string(v));
}

std::optional<std::int64_t> Yaml::as_i64() const noexcept {
    if (const auto* i = std::get_if<std::int64_t>(&v_)) {
        return *i;
    }
    return std::nullopt;
}

std::optional<bool> Yaml::as_bool() const noexcept {
    if (const auto* b = std::get_if<bool>(&v_)) {
        return *b;
    }
    return std::nullopt;
}

std::optional<double> Yaml::as_f64() const noexcept {
    if (const auto* r = std::get_if<Real>(&v_)) {
        return parse_f64(r->text);
    }
    return std::nullopt;
}

const Yaml* Yaml::get(std::string_view key) const noexcept {
    const auto* h = as_hash();
    if (!h) {
        return nullptr;
    }
    for (const auto& [k, v] : *h) {
        if (const auto* s = k.as_str(); s && *s == key) {
            return &v;
        }
    }
    return nullptr;
}

// ---------------------------------------------------------------------------
// Loader: yaml-cpp events assembled the way yaml-rust2's `YamlLoader` does
// ---------------------------------------------------------------------------

namespace {

constexpr std::string_view CORE_TAG = "tag:yaml.org,2002:";

class Loader final : public YAML::EventHandler {
  public:
    explicit Loader(std::string_view text) : text_(text) {}

    std::vector<Yaml> docs;
    std::optional<std::string> error;

    void OnDocumentStart(const YAML::Mark&) override {}
    void OnDocumentEnd() override {
        if (stack_.empty()) {
            docs.emplace_back(Yaml::BadValue{});
        } else {
            docs.push_back(std::move(stack_.back().first));
            stack_.clear();
        }
    }

    void OnNull(const YAML::Mark& mark, YAML::anchor_t anchor) override {
        // yaml-cpp also reads plain `Null` and `NULL` as null; yaml-rust2 keeps them as
        // strings.
        Yaml node{Yaml::Null{}};
        if (mark.pos >= 0 && static_cast<std::size_t>(mark.pos) < text_.size()) {
            const std::string_view at = text_.substr(static_cast<std::size_t>(mark.pos));
            for (std::string_view word : {std::string_view("Null"), std::string_view("NULL")}) {
                if (at.starts_with(word) &&
                    (at.size() == word.size() || std::string_view(" \t\r\n,]}#").find(at[word.size()]) != std::string_view::npos)) {
                    node = Yaml(std::string(word));
                }
            }
        }
        insert(std::move(node), anchor);
    }

    void OnAlias(const YAML::Mark&, YAML::anchor_t anchor) override {
        auto it = anchors_.find(anchor);
        insert(it == anchors_.end() ? Yaml(Yaml::BadValue{}) : it->second, 0);
    }

    void OnScalar(const YAML::Mark&, const std::string& tag, YAML::anchor_t anchor, const std::string& value) override {
        insert(resolve(tag, value), anchor);
    }

    void OnSequenceStart(const YAML::Mark&, const std::string&, YAML::anchor_t anchor, YAML::EmitterStyle::value) override {
        stack_.emplace_back(Yaml(Array{}), anchor);
    }
    void OnSequenceEnd() override { finish(); }

    void OnMapStart(const YAML::Mark&, const std::string&, YAML::anchor_t anchor, YAML::EmitterStyle::value) override {
        stack_.emplace_back(Yaml(Hash{}), anchor);
        keys_.emplace_back(Yaml::BadValue{});
    }
    void OnMapEnd() override {
        keys_.pop_back();
        finish();
    }

  private:
    static Yaml resolve(const std::string& tag, const std::string& v) {
        if (tag == "?") {
            return Yaml::from_str(v);
        }
        if (tag.starts_with(CORE_TAG)) {
            const std::string_view suffix = std::string_view(tag).substr(CORE_TAG.size());
            if (suffix == "bool") {
                if (v == "true" || v == "True" || v == "TRUE") {
                    return Yaml(true);
                }
                if (v == "false" || v == "False" || v == "FALSE") {
                    return Yaml(false);
                }
                return Yaml(Yaml::BadValue{});
            }
            if (suffix == "int") {
                auto i = parse_int(v, 10);
                return i ? Yaml(*i) : Yaml(Yaml::BadValue{});
            }
            if (suffix == "float") {
                return parse_f64(v) ? Yaml(Yaml::Real{v}) : Yaml(Yaml::BadValue{});
            }
            if (suffix == "null") {
                return v == "~" || v == "null" ? Yaml(Yaml::Null{}) : Yaml(Yaml::BadValue{});
            }
        }
        // Quoted and block scalars ("!"), `!!str` and unknown tags.
        return Yaml(v);
    }

    void finish() {
        auto node = std::move(stack_.back());
        stack_.pop_back();
        insert(std::move(node.first), node.second);
    }

    void insert(Yaml node, YAML::anchor_t anchor) {
        if (error) {
            return;
        }
        if (anchor > 0) {
            anchors_[anchor] = node;
        }
        if (stack_.empty()) {
            stack_.emplace_back(std::move(node), 0);
            return;
        }
        Yaml& parent = stack_.back().first;
        if (auto* h = parent.as_mut_hash()) {
            Yaml& key = keys_.back();
            if (key.is_bad_value()) {
                key = std::move(node);
                return;
            }
            Yaml k = std::move(key);
            key = Yaml(Yaml::BadValue{});
            if (std::ranges::any_of(*h, [&](const auto& kv) { return kv.first == k; })) {
                error = "duplicated key in mapping";
                return;
            }
            h->emplace_back(std::move(k), std::move(node));
            return;
        }
        // An array (`Array` is the only other container pushed on the stack).
        auto* a = const_cast<Array*>(parent.as_vec());
        a->push_back(std::move(node));
    }

    std::string_view text_;
    std::vector<std::pair<Yaml, YAML::anchor_t>> stack_;
    std::vector<Yaml> keys_;
    std::map<YAML::anchor_t, Yaml> anchors_;
};

} // namespace

std::expected<std::vector<Yaml>, std::string> load_all(std::string_view text) {
    std::istringstream in{std::string(text)};
    Loader loader(text);
    try {
        YAML::Parser parser(in);
        while (parser.HandleNextDocument(loader)) {
            if (loader.error) {
                return std::unexpected(*loader.error);
            }
        }
    } catch (const YAML::Exception& e) {
        return std::unexpected(std::string(e.what()));
    }
    if (loader.error) {
        return std::unexpected(*loader.error);
    }
    return std::move(loader.docs);
}

// ---------------------------------------------------------------------------
// Document model
// ---------------------------------------------------------------------------

namespace {

std::string join(std::span<const std::string_view> path, std::size_t n) {
    std::string out;
    for (std::size_t i = 0; i < n && i < path.size(); ++i) {
        if (i > 0) {
            out += '.';
        }
        out += path[i];
    }
    return out;
}

Hash::iterator find_key(Hash& h, std::string_view key) {
    return std::ranges::find_if(h, [&](const auto& kv) {
        const auto* s = kv.first.as_str();
        return s && *s == key;
    });
}

bool is_content(std::string_view line) {
    std::string_view t = line;
    while (!t.empty() && t.front() == ' ') {
        t.remove_prefix(1);
    }
    return !detail::trim(t).empty() && !t.starts_with('#');
}

bool is_doc_marker(std::string_view line) {
    const std::string_view t = detail::trim_end(line);
    return t == "---" || t == "..." || t.starts_with("--- #") || t.starts_with("... #");
}

std::size_t indent_of(std::string_view line) {
    std::size_t n = 0;
    while (n < line.size() && line[n] == ' ') {
        ++n;
    }
    return n;
}

bool is_seq_item(std::string_view line) {
    const std::string_view t = line.substr(indent_of(line));
    return t == "-" || t.starts_with("- ") || detail::trim_end(t) == "-";
}

std::optional<std::string_view> first_content_line(std::string_view text) {
    // Rust `str::lines`: split on `\n`, strip a trailing `\r`, no empty last line.
    std::size_t start = 0;
    while (start < text.size()) {
        auto nl = text.find('\n', start);
        std::string_view line = text.substr(start, nl == std::string_view::npos ? std::string_view::npos : nl - start);
        if (line.ends_with('\r')) {
            line.remove_suffix(1);
        }
        if (is_content(line) && !is_doc_marker(line)) {
            return line;
        }
        if (nl == std::string_view::npos) {
            break;
        }
        start = nl + 1;
    }
    return std::nullopt;
}

Result<Yaml> load_root(std::string_view text, const std::string& what) {
    auto docs = load_all(text);
    if (!docs) {
        return std::unexpected(Error::parse(what, docs.error()));
    }
    if (docs->empty()) {
        return Yaml(Hash{});
    }
    if (docs->size() > 1) {
        return std::unexpected(Error::unsupported(what, "it holds several YAML documents"));
    }
    Yaml root = std::move(docs->front());
    if (root.is_null()) {
        return Yaml(Hash{});
    }
    if (root.as_hash()) {
        const auto first = first_content_line(text);
        if (first && detail::trim_start(*first).starts_with('{')) {
            return std::unexpected(Error::unsupported(what, "its root is a flow mapping"));
        }
        return root;
    }
    return std::unexpected(Error::unsupported(what, "its root is not a mapping"));
}

Result<> model_set(Yaml& root, std::span<const std::string_view> path, Yaml value, const std::string& what) {
    Yaml* node = &root;
    for (std::size_t depth = 0; depth < path.size(); ++depth) {
        if (node->is_null()) {
            *node = Yaml(Hash{});
        }
        Hash* map = node->as_mut_hash();
        if (!map) {
            return std::unexpected(Error::unsupported(what, "`" + join(path, depth) + "` is not a mapping"));
        }
        auto it = find_key(*map, path[depth]);
        if (depth + 1 == path.size()) {
            // hashlink `replace`: an existing key keeps its position.
            if (it != map->end()) {
                it->second = std::move(value);
            } else {
                map->emplace_back(Yaml(std::string(path[depth])), std::move(value));
            }
            return {};
        }
        if (it == map->end()) {
            map->emplace_back(Yaml(std::string(path[depth])), Yaml(Hash{}));
            it = std::prev(map->end());
        }
        node = &it->second;
    }
    return {};
}

std::optional<std::size_t> model_remove_go(Yaml& node, std::span<const std::string_view> path, std::size_t depth) {
    Hash* map = node.as_mut_hash();
    if (!map) {
        return std::nullopt;
    }
    auto it = find_key(*map, path[depth]);
    if (depth + 1 == path.size()) {
        if (it != map->end()) {
            map->erase(it);
        }
        return depth;
    }
    if (it == map->end()) {
        return std::nullopt;
    }
    auto removed = model_remove_go(it->second, path, depth + 1);
    if (!removed) {
        return std::nullopt;
    }
    if (const auto* h = it->second.as_hash(); h && h->empty()) {
        map->erase(it);
        return depth;
    }
    return removed;
}

// Removes `path` from the model, pruning parents left empty. Returns the depth of the
// outermost removed key.
std::size_t model_remove(Yaml& root, std::span<const std::string_view> path) {
    return model_remove_go(root, path, 0).value_or(path.size() - 1);
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

// serde_json's string literal form, which is a valid YAML double quoted scalar.
std::string json_quote(std::string_view s) {
    std::string out = "\"";
    for (char ch : s) {
        const auto c = static_cast<unsigned char>(ch);
        switch (c) {
        case '"':
            out += "\\\"";
            break;
        case '\\':
            out += "\\\\";
            break;
        case '\b':
            out += "\\b";
            break;
        case '\f':
            out += "\\f";
            break;
        case '\n':
            out += "\\n";
            break;
        case '\r':
            out += "\\r";
            break;
        case '\t':
            out += "\\t";
            break;
        default:
            if (c < 0x20) {
                static constexpr char HEX[] = "0123456789abcdef";
                out += "\\u00";
                out += HEX[c >> 4];
                out += HEX[c & 0xF];
            } else {
                out += ch;
            }
        }
    }
    out += '"';
    return out;
}

bool ascii_alnum(char c) { return (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') || (c >= '0' && c <= '9'); }

bool plain_safe(std::string_view s) {
    if (s.empty()) {
        return false;
    }
    const char first = s.front();
    if (!(ascii_alnum(first) || first == '_' || first == '.' || first == '/' || first == '$' || first == '~')) {
        return false;
    }
    if (s.ends_with(':') || s.ends_with(' ') || s.find(": ") != std::string_view::npos || s.find(" #") != std::string_view::npos ||
        s.find("  ") != std::string_view::npos) {
        return false;
    }
    constexpr std::string_view ALLOWED = "_-./@+$~:() *,";
    if (!std::ranges::all_of(s, [&](char c) { return ascii_alnum(c) || ALLOWED.find(c) != std::string_view::npos; })) {
        return false;
    }
    std::string lower(s);
    std::ranges::transform(lower, lower.begin(), [](char c) { return c >= 'A' && c <= 'Z' ? static_cast<char>(c + 32) : c; });
    for (std::string_view word : {"y", "n", "yes", "no", "on", "off", "true", "false", "null", "~"}) {
        if (lower == word) {
            return false;
        }
    }
    auto docs = load_all("k: " + std::string(s) + "\n");
    if (!docs || docs->empty()) {
        return false;
    }
    const Yaml* v = docs->front().get("k");
    return v && v->as_str() && *v->as_str() == s;
}

// Renders a string as a plain scalar when that reads back as the same string,
// otherwise double quoted.
std::string render_string(std::string_view s) { return plain_safe(s) ? std::string(s) : json_quote(s); }

// Renders a value that fits on the key line, or none for block values.
std::optional<std::string> render_inline(const Yaml& value) {
    return std::visit(
        [](const auto& v) -> std::optional<std::string> {
            using T = std::decay_t<decltype(v)>;
            if constexpr (std::is_same_v<T, std::string>) {
                return render_string(v);
            } else if constexpr (std::is_same_v<T, std::int64_t>) {
                return std::to_string(v);
            } else if constexpr (std::is_same_v<T, Yaml::Real>) {
                return v.text;
            } else if constexpr (std::is_same_v<T, bool>) {
                return std::string(v ? "true" : "false");
            } else if constexpr (std::is_same_v<T, Array>) {
                return v.empty() ? std::optional<std::string>("[]") : std::nullopt;
            } else if constexpr (std::is_same_v<T, Hash>) {
                return v.empty() ? std::optional<std::string>("{}") : std::nullopt;
            } else {
                return std::string("null");
            }
        },
        value.storage());
}

std::string render_key(const Yaml& key) {
    if (const auto* s = key.as_str()) {
        return render_string(*s);
    }
    return render_inline(key).value_or("null");
}

void render_block(const Yaml& value, std::size_t indent, std::size_t unit, std::vector<std::string>& out);

void render_value(const std::string& head, const std::string& comment, const Yaml& value, std::size_t indent, std::size_t unit,
                  std::vector<std::string>& out) {
    if (auto inline_value = render_inline(value)) {
        out.push_back(head + " " + *inline_value + comment);
    } else {
        out.push_back(head + comment);
        render_block(value, indent + unit, unit, out);
    }
}

void render_entry(const std::string& key, const Yaml& value, std::size_t indent, std::size_t unit, std::vector<std::string>& out) {
    render_value(std::string(indent, ' ') + key + ":", "", value, indent, unit, out);
}

// Renders a mapping or sequence as block lines at `indent`.
void render_block(const Yaml& value, std::size_t indent, std::size_t unit, std::vector<std::string>& out) {
    const std::string pad(indent, ' ');
    if (const auto* map = value.as_hash()) {
        for (const auto& [k, v] : *map) {
            render_entry(render_key(k), v, indent, unit, out);
        }
    } else if (const auto* items = value.as_vec()) {
        for (const auto& item : *items) {
            if (auto s = render_inline(item)) {
                out.push_back(pad + "- " + *s);
            } else {
                std::vector<std::string> inner;
                render_block(item, indent + 2, unit, inner);
                if (!inner.empty()) {
                    out.push_back(pad + "- " + inner.front().substr(indent + 2));
                    out.insert(out.end(), std::make_move_iterator(inner.begin() + 1), std::make_move_iterator(inner.end()));
                }
            }
        }
    } else if (auto s = render_inline(value)) {
        out.push_back(pad + *s);
    }
}

// ---------------------------------------------------------------------------
// Line editor
// ---------------------------------------------------------------------------

// One physical line: byte range of its text (without the line break) and the offset
// where the next line starts.
struct LineSpan {
    std::size_t start;
    std::size_t end;
    std::size_t next;
};

// A key line found in a mapping and the lines its value occupies.
struct Entry {
    std::size_t line = 0;
    std::size_t indent = 0;
    // Byte length (within the line) of the indent, key and colon.
    std::size_t head_len = 0;
    // The value written on the key line, without comment, trimmed.
    std::string inline_value;
    // The comment on the key line, from `#`.
    std::optional<std::string> comment;
    // One past the last line belonging to the value.
    std::size_t body_end = 0;
};

// A range of lines holding one block mapping.
struct Region {
    std::size_t start;
    std::size_t end;
};

// Parses a mapping key at the start of `line` (after `indent` spaces). Returns the
// decoded key and the byte length up to and including the colon.
std::optional<std::pair<std::string, std::size_t>> plain_key(std::string_view rest) {
    for (std::size_t i = 0; i < rest.size(); ++i) {
        const char b = rest[i];
        if (b == ':' && (i + 1 == rest.size() || rest[i + 1] == ' ' || rest[i + 1] == '\t')) {
            const std::string_view key = detail::trim_end(rest.substr(0, i));
            if (key.empty() || key.find(" #") != std::string_view::npos) {
                return std::nullopt;
            }
            return std::pair{std::string(key), key.size()};
        }
        if (b == '#' && i > 0 && rest[i - 1] == ' ') {
            return std::nullopt;
        }
    }
    return std::nullopt;
}

std::optional<std::pair<std::string, std::size_t>> parse_key(std::string_view line, std::size_t indent) {
    const std::string_view rest = line.substr(indent);
    if (rest.empty()) {
        return std::nullopt;
    }
    std::string key;
    std::size_t consumed = 0;
    switch (rest.front()) {
    case '"': {
        bool escaped = false;
        std::optional<std::size_t> close;
        for (std::size_t i = 1; i < rest.size(); ++i) {
            const char c = rest[i];
            if (escaped) {
                escaped = false;
            } else if (c == '\\') {
                escaped = true;
            } else if (c == '"') {
                close = i;
                break;
            }
        }
        if (!close) {
            return std::nullopt;
        }
        auto parsed = json::parse(rest.substr(0, *close + 1));
        if (!parsed || !parsed->as_string()) {
            return std::nullopt;
        }
        key = *parsed->as_string();
        consumed = *close + 1;
        break;
    }
    case '\'': {
        std::size_t i = 1;
        while (true) {
            if (i >= rest.size()) {
                return std::nullopt;
            }
            if (rest[i] == '\'') {
                if (i + 1 < rest.size() && rest[i + 1] == '\'') {
                    key += '\'';
                    i += 2;
                    continue;
                }
                break;
            }
            key += rest[i];
            ++i;
        }
        consumed = i + 1;
        break;
    }
    case '?':
    case '[':
    case '{':
    case '&':
    case '*':
    case '!':
    case '|':
    case '>':
    case '%':
    case '@':
    case '`':
        return std::nullopt;
    default: {
        if (rest.front() == '-' && (rest == "-" || rest.starts_with("- "))) {
            return std::nullopt;
        }
        auto plain = plain_key(rest);
        if (!plain) {
            return std::nullopt;
        }
        key = std::move(plain->first);
        consumed = plain->second;
    }
    }
    const std::string_view after = rest.substr(consumed);
    const auto colon = after.find_first_not_of(' ');
    if (colon == std::string_view::npos || after[colon] != ':') {
        return std::nullopt;
    }
    const std::string_view tail = after.substr(colon + 1);
    if (!(tail.empty() || tail.starts_with(' ') || tail.starts_with('\t'))) {
        return std::nullopt;
    }
    return std::pair{std::move(key), indent + consumed + colon + 1};
}

// Splits the text after a key's colon into the value and the trailing comment.
std::pair<std::string_view, std::optional<std::string_view>> split_comment(std::string_view rest) {
    std::optional<char> quote;
    std::size_t i = 0;
    while (i < rest.size()) {
        const char b = rest[i];
        if (quote) {
            if (*quote == '"' && b == '\\') {
                ++i;
            } else if (b == *quote) {
                if (*quote == '\'' && i + 1 < rest.size() && rest[i + 1] == '\'') {
                    ++i;
                } else {
                    quote.reset();
                }
            }
        } else if (b == '"' || b == '\'') {
            quote = b;
        } else if (b == '#' && (i == 0 || rest[i - 1] == ' ' || rest[i - 1] == '\t')) {
            return {detail::trim(rest.substr(0, i)), detail::trim_end(rest.substr(i))};
        }
        ++i;
    }
    return {detail::trim(rest), std::nullopt};
}

class Editor {
  public:
    Editor(std::string_view text, std::string_view what) : text_(text), what_(what) {
        std::size_t start = 0;
        while (start < text.size()) {
            const auto nl = text.find('\n', start);
            if (nl != std::string_view::npos) {
                const std::size_t end = nl > start && text[nl - 1] == '\r' ? nl - 1 : nl;
                lines_.push_back({start, end, nl + 1});
                start = nl + 1;
            } else {
                lines_.push_back({start, text.size(), text.size()});
                start = text.size();
            }
        }
        newline_ = text.find("\r\n") != std::string_view::npos ? "\r\n" : "\n";
        unit_ = detect_unit();
    }

    Result<std::string> set(std::span<const std::string_view> path, const Yaml& expected_root) const {
        auto walked = walk(path);
        if (!walked) {
            return std::unexpected(std::move(walked.error()));
        }
        auto& [depth, region, entry] = *walked;
        const Yaml* value = get_path(expected_root, path.first(depth + 1));
        if (!value) {
            return std::unexpected(unsupported("edited key missing from the result"));
        }
        if (entry) {
            if (depth + 1 < path.size() && !entry->inline_value.empty()) {
                return std::unexpected(unsupported("`" + join(path, depth + 1) +
                                                   "` is written inline (flow style); editing it would reformat its line, so edit it by hand"));
            }
            return replace_value(*entry, *value);
        }
        return insert_entry(region, depth == 0, path[depth], *value);
    }

    Result<std::string> remove(std::span<const std::string_view> path) const {
        auto walked = walk(path);
        if (!walked) {
            return std::unexpected(std::move(walked.error()));
        }
        auto& [depth, region, entry] = *walked;
        if (!entry) {
            return std::unexpected(unsupported("`" + join(path, depth + 1) + "` not found in the text"));
        }
        if (depth + 1 != path.size()) {
            return std::unexpected(unsupported("`" + join(path, depth + 1) + "` is not a block mapping"));
        }
        const std::size_t start = lines_[entry->line].start;
        const std::size_t end = span_end(entry->body_end);
        std::string out;
        out.reserve(text_.size());
        out.append(text_.substr(0, start));
        out.append(text_.substr(end));
        return out;
    }

  private:
    std::string_view line(std::size_t i) const {
        const auto& span = lines_[i];
        return text_.substr(span.start, span.end - span.start);
    }

    std::size_t detect_unit() const {
        std::optional<std::size_t> prev;
        for (std::size_t i = 0; i < lines_.size(); ++i) {
            const auto l = line(i);
            if (!is_content(l) || is_doc_marker(l)) {
                continue;
            }
            const auto ind = indent_of(l);
            if (prev && ind > *prev) {
                return ind - *prev;
            }
            prev = ind;
        }
        return 2;
    }

    Error unsupported(std::string reason) const { return Error::unsupported(std::string(what_), std::move(reason)); }

    Region root_region() const { return {0, lines_.size()}; }

    // Indent of the keys of the mapping in `region`, or none when empty.
    std::optional<std::size_t> key_indent(Region region) const {
        for (std::size_t i = region.start; i < region.end; ++i) {
            const auto l = line(i);
            if (is_content(l) && !is_doc_marker(l)) {
                return indent_of(l);
            }
        }
        return std::nullopt;
    }

    // The entries of the mapping in `region`, in order.
    Result<std::vector<std::pair<std::string, Entry>>> entries(Region region) const {
        std::vector<std::pair<std::string, Entry>> out;
        const auto key_indent_opt = key_indent(region);
        if (!key_indent_opt) {
            return out;
        }
        const std::size_t key_ind = *key_indent_opt;
        std::size_t i = region.start;
        while (i < region.end) {
            const auto l = line(i);
            if (!is_content(l) || is_doc_marker(l)) {
                ++i;
                continue;
            }
            if (indent_of(l) != key_ind || is_seq_item(l)) {
                return std::unexpected(unsupported("line " + std::to_string(i + 1) + " is not a mapping key"));
            }
            auto parsed = parse_key(l, key_ind);
            if (!parsed) {
                return std::unexpected(unsupported("line " + std::to_string(i + 1) + " has a key form the editor does not handle"));
            }
            auto [inline_value, comment] = split_comment(l.substr(parsed->second));
            const bool block_scalar = inline_value.starts_with('|') || inline_value.starts_with('>');
            std::size_t body_end = i + 1;
            std::size_t j = i + 1;
            while (j < region.end) {
                const auto lj = line(j);
                const auto ind = indent_of(lj);
                if (detail::trim(lj).empty()) {
                    ++j;
                    continue;
                }
                if (block_scalar) {
                    if (ind > key_ind) {
                        body_end = j + 1;
                        ++j;
                        continue;
                    }
                    if (!is_content(lj)) {
                        ++j;
                        continue;
                    }
                    break;
                }
                if (!is_content(lj)) {
                    ++j;
                    continue;
                }
                if (ind > key_ind || (ind == key_ind && is_seq_item(lj))) {
                    body_end = j + 1;
                    ++j;
                    continue;
                }
                break;
            }
            Entry entry;
            entry.line = i;
            entry.indent = key_ind;
            entry.head_len = parsed->second;
            entry.inline_value = std::string(inline_value);
            if (comment) {
                entry.comment = std::string(*comment);
            }
            entry.body_end = body_end;
            out.emplace_back(std::move(parsed->first), std::move(entry));
            i = body_end;
        }
        return out;
    }

    Result<std::optional<Entry>> find(Region region, std::string_view key) const {
        auto all = entries(region);
        if (!all) {
            return std::unexpected(std::move(all.error()));
        }
        for (auto& [k, e] : *all) {
            if (k == key) {
                return std::optional<Entry>(std::move(e));
            }
        }
        return std::optional<Entry>{};
    }

    // The region of a block mapping value, when `entry` holds one.
    std::optional<Region> child_mapping(const Entry& entry) const {
        if (!entry.inline_value.empty() || entry.body_end <= entry.line + 1) {
            return std::nullopt;
        }
        const Region region{entry.line + 1, entry.body_end};
        for (std::size_t i = region.start; i < region.end; ++i) {
            const auto l = line(i);
            if (is_content(l)) {
                if (is_seq_item(l) || indent_of(l) <= entry.indent) {
                    return std::nullopt;
                }
                return region;
            }
        }
        return std::nullopt;
    }

    struct Walked {
        std::size_t depth;
        Region region;
        std::optional<Entry> entry;
    };

    // Finds the first missing key, non-mapping ancestor, or the requested entry.
    Result<Walked> walk(std::span<const std::string_view> path) const {
        Region region = root_region();
        for (std::size_t depth = 0; depth < path.size(); ++depth) {
            auto entry = find(region, path[depth]);
            if (!entry) {
                return std::unexpected(std::move(entry.error()));
            }
            if (depth + 1 < path.size() && *entry) {
                if (auto child = child_mapping(**entry)) {
                    region = *child;
                    continue;
                }
            }
            return Walked{depth, region, std::move(*entry)};
        }
        return std::unexpected(unsupported("empty key path"));
    }

    // Byte offset where line `i` starts, or the text end.
    std::size_t span_end(std::size_t i) const { return i == 0 ? 0 : lines_[i - 1].next; }

    std::string replace_value(const Entry& entry, const Yaml& value) const {
        const auto l = line(entry.line);
        const std::string head(l.substr(0, entry.head_len));
        std::vector<std::string> new_lines;
        const std::string comment = entry.comment ? " " + *entry.comment : std::string();
        render_value(head, comment, value, entry.indent, unit_, new_lines);
        const std::size_t start = lines_[entry.line].start;
        const std::size_t end = span_end(entry.body_end);
        const auto& last = lines_[entry.body_end - 1];
        return splice(start, end, new_lines, last.next > last.end);
    }

    Result<std::string> insert_entry(Region region, bool is_root, std::string_view key, const Yaml& value) const {
        const std::size_t indent = key_indent(region).value_or(0);
        std::vector<std::string> new_lines;
        render_entry(render_string(key), value, indent, unit_, new_lines);
        auto all = entries(region);
        if (!all) {
            return std::unexpected(std::move(all.error()));
        }
        std::size_t at = 0;
        if (!all->empty()) {
            // After the body of the last entry of this mapping, before any trailing comments.
            at = span_end(all->back().second.body_end);
        } else if (is_root) {
            at = text_.size();
        } else {
            at = span_end(region.end);
        }
        std::string out;
        out.reserve(text_.size() + 64);
        out.append(text_.substr(0, at));
        if (at > 0 && !text_.substr(0, at).ends_with('\n')) {
            out.append(newline_);
        }
        for (const auto& l : new_lines) {
            out.append(l);
            out.append(newline_);
        }
        out.append(text_.substr(at));
        return out;
    }

    std::string splice(std::size_t start, std::size_t end, const std::vector<std::string>& new_lines, bool trailing_newline) const {
        std::string out;
        out.reserve(text_.size() + 64);
        out.append(text_.substr(0, start));
        for (std::size_t n = 0; n < new_lines.size(); ++n) {
            out.append(new_lines[n]);
            if (n + 1 < new_lines.size() || trailing_newline) {
                out.append(newline_);
            }
        }
        out.append(text_.substr(end));
        return out;
    }

    std::string_view text_;
    std::string_view what_;
    std::vector<LineSpan> lines_;
    std::string_view newline_ = "\n";
    std::size_t unit_ = 2;
};

} // namespace

const Yaml* get_path(const Yaml& root, std::span<const std::string_view> path) {
    const Yaml* node = &root;
    for (const auto key : path) {
        node = node->get(key);
        if (!node) {
            return nullptr;
        }
    }
    return node;
}

Result<YamlDocument> YamlDocument::parse(std::string_view text, std::string_view what) {
    auto root = load_root(text, std::string(what));
    if (!root) {
        return std::unexpected(std::move(root.error()));
    }
    return YamlDocument(std::string(text), std::move(*root), std::string(what));
}

Result<> YamlDocument::set(std::span<const std::string_view> path, Yaml value) {
    if (path.empty()) {
        return std::unexpected(Error::invalid("path", "empty key path"));
    }
    Yaml expected = root_;
    if (auto ok = model_set(expected, path, std::move(value), what_); !ok) {
        return ok;
    }
    if (expected == root_) {
        return {};
    }
    auto text = Editor(text_, what_).set(path, expected);
    if (!text) {
        return std::unexpected(std::move(text.error()));
    }
    return commit(std::move(*text), std::move(expected));
}

Result<bool> YamlDocument::remove(std::span<const std::string_view> path) {
    if (path.empty() || !get_path(root_, path)) {
        return false;
    }
    Yaml expected = root_;
    const std::size_t removed_depth = model_remove(expected, path);
    auto text = Editor(text_, what_).remove(path.first(removed_depth + 1));
    if (!text) {
        return std::unexpected(std::move(text.error()));
    }
    if (auto ok = commit(std::move(*text), std::move(expected)); !ok) {
        return std::unexpected(std::move(ok.error()));
    }
    return true;
}

Result<> YamlDocument::commit(std::string text, Yaml expected) {
    auto parsed = load_root(text, what_);
    if (!parsed || *parsed != expected) {
        return std::unexpected(Error::edit_check(what_));
    }
    text_ = std::move(text);
    root_ = std::move(*parsed);
    return {};
}

Yaml ystr(std::string s) { return Yaml(std::move(s)); }

Yaml ystrs(std::span<const std::string> items) {
    Array out;
    out.reserve(items.size());
    for (const auto& s : items) {
        out.emplace_back(s);
    }
    return Yaml(std::move(out));
}

Yaml ystrs(std::initializer_list<std::string_view> items) {
    Array out;
    out.reserve(items.size());
    for (const auto s : items) {
        out.emplace_back(std::string(s));
    }
    return Yaml(std::move(out));
}

Yaml ymap(std::vector<std::pair<std::string, Yaml>> pairs) {
    Hash map;
    for (auto& [k, v] : pairs) {
        auto it = find_key(map, k);
        if (it != map.end()) {
            it->second = std::move(v);
        } else {
            map.emplace_back(Yaml(std::move(k)), std::move(v));
        }
    }
    return Yaml(std::move(map));
}

json::Value to_json(const Yaml& value) {
    return std::visit(
        [](const auto& v) -> json::Value {
            using T = std::decay_t<decltype(v)>;
            if constexpr (std::is_same_v<T, std::string>) {
                return json::Value(v);
            } else if constexpr (std::is_same_v<T, std::int64_t>) {
                return json::Value(v);
            } else if constexpr (std::is_same_v<T, Yaml::Real>) {
                auto f = parse_f64(v.text);
                if (!f || !std::isfinite(*f)) {
                    return json::Value();
                }
                return json::Value(*f);
            } else if constexpr (std::is_same_v<T, bool>) {
                return json::Value(v);
            } else if constexpr (std::is_same_v<T, Array>) {
                json::Array out;
                out.reserve(v.size());
                for (const auto& item : v) {
                    out.push_back(to_json(item));
                }
                return json::Value(std::move(out));
            } else if constexpr (std::is_same_v<T, Hash>) {
                json::Value out{json::Object{}};
                for (const auto& [k, item] : v) {
                    const auto* s = k.as_str();
                    out.set(s ? *s : render_key(k), to_json(item));
                }
                return out;
            } else {
                return json::Value();
            }
        },
        value.storage());
}

} // namespace loa::settings::models::yaml
