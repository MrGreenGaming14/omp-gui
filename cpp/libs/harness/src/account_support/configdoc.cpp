#include "loa/harness/account_support/configdoc.hpp"

#include <algorithm>
#include <format>
#include <vector>

#include "loa/settings/app/toml.hpp"

namespace loa::harness::account_support {

namespace json = loa::runtime::json;
namespace toml = loa::settings::app::toml;

namespace {

// Rust `{:?}` of a key.
std::string debug(std::string_view key) { return std::format("\"{}\"", key); }

Error empty_path() { return Error::invalid("path", "empty key path"); }

json::Value* member(json::Object& object, std::string_view key) {
    auto it = std::ranges::find_if(object, [&](const auto& kv) { return kv.first == key; });
    return it == object.end() ? nullptr : &it->second;
}

const json::Value* member(const json::Object& object, std::string_view key) {
    auto it = std::ranges::find_if(object, [&](const auto& kv) { return kv.first == key; });
    return it == object.end() ? nullptr : &it->second;
}

} // namespace

// ---- JSON ----

Result<JsonDoc> JsonDoc::parse(std::string_view text, std::string_view what) {
    std::string_view trimmed = text;
    while (!trimmed.empty() && std::isspace(static_cast<unsigned char>(trimmed.front()))) trimmed.remove_prefix(1);
    if (trimmed.empty()) return JsonDoc(json::Object{}, std::string(what));
    auto value = json::parse(text);
    if (!value) return std::unexpected(Error::parse(std::string(what), value.error().message()));
    auto* root = value->as_object();
    if (root == nullptr) return std::unexpected(Error::unsupported(std::string(what), "its root is not an object"));
    return JsonDoc(std::move(*root), std::string(what));
}

std::optional<std::string> JsonDoc::get(KeyPath path) const {
    if (path.empty()) return std::nullopt;
    const json::Value* item = member(root_, path.front());
    for (auto key : path.subspan(1)) {
        if (item == nullptr || item->as_object() == nullptr) return std::nullopt;
        item = member(*item->as_object(), key);
    }
    if (item == nullptr || item->as_string() == nullptr) return std::nullopt;
    return *item->as_string();
}

Result<> JsonDoc::set(KeyPath path, std::optional<std::string_view> value) {
    if (path.empty()) return std::unexpected(empty_path());
    json::Object* map = &root_;
    for (auto parent : path.first(path.size() - 1)) {
        json::Value* item = member(*map, parent);
        if (item == nullptr) {
            if (!value) return {};
            map->emplace_back(std::string(parent), json::Object{});
            item = &map->back().second;
        }
        if (!value && !item->is_object()) return {};
        map = item->as_object();
        if (map == nullptr) return std::unexpected(Error::unsupported(what_, debug(parent) + " is not an object"));
    }
    const std::string_view key = path.back();
    if (value) {
        if (json::Value* existing = member(*map, key)) *existing = json::Value(std::string(*value));
        else map->emplace_back(std::string(key), json::Value(std::string(*value)));
    } else {
        std::erase_if(*map, [&](const auto& kv) { return kv.first == key; });
    }
    return {};
}

std::string JsonDoc::render() const { return json::dump_pretty(json::Value(root_)) + "\n"; }

// ---- TOML ----

struct TomlDoc::Parsed {
    toml::Table table;
};

TomlDoc::TomlDoc(std::string text, std::string what, std::unique_ptr<Parsed> parsed)
    : text_(std::move(text)), what_(std::move(what)), parsed_(std::move(parsed)) {}
TomlDoc::TomlDoc(TomlDoc&&) noexcept = default;
TomlDoc& TomlDoc::operator=(TomlDoc&&) noexcept = default;
TomlDoc::~TomlDoc() = default;

Result<TomlDoc> TomlDoc::parse(std::string_view text, std::string_view what) {
    auto table = toml::parse(text);
    if (!table) return std::unexpected(Error::parse(std::string(what), table.error()));
    return TomlDoc(std::string(text), std::string(what), std::make_unique<Parsed>(Parsed{std::move(*table)}));
}

namespace {

using Path = std::vector<std::string>;

// One line-level item of a TOML document, as byte offsets into its text.
struct Item {
    bool header = false;
    // `[[x]]` headers.
    bool array = false;
    // Header: the table path. Key/value: the full path (section path + dotted key).
    Path path;
    // Where its decoration (comments and blank lines before it) starts.
    std::size_t decor_start = 0;
    std::size_t line_start = 0;
    std::size_t value_start = 0;
    std::size_t value_end = 0;
    // After the line's newline (or the end of the text).
    std::size_t end = 0;
};

class Scanner {
  public:
    explicit Scanner(std::string_view text) : s_(text) {}

    std::vector<Item> items() {
        std::vector<Item> out;
        Path section;
        std::size_t decor = 0;
        while (p_ < s_.size()) {
            skip_blank();
            if (p_ >= s_.size()) break;
            const char c = s_[p_];
            if (c == '\n' || c == '\r' || c == '#') {
                to_line_end();
                continue;
            }
            Item item;
            item.decor_start = decor;
            item.line_start = line_start(p_);
            if (c == '[') {
                item.header = true;
                ++p_;
                if (peek('[')) {
                    item.array = true;
                    ++p_;
                }
                item.path = key_path();
                ++p_; // ]
                if (item.array) ++p_;
                section = item.path;
            } else {
                Path key = key_path();
                item.path = section;
                item.path.insert(item.path.end(), key.begin(), key.end());
                ++p_; // =
                skip_blank();
                item.value_start = p_;
                value();
                item.value_end = p_;
            }
            to_line_end();
            item.end = p_;
            decor = p_;
            out.push_back(std::move(item));
        }
        return out;
    }

  private:
    [[nodiscard]] bool peek(char c) const { return p_ < s_.size() && s_[p_] == c; }
    [[nodiscard]] bool starts(std::string_view t) const { return s_.substr(p_).starts_with(t); }
    [[nodiscard]] std::size_t line_start(std::size_t at) const {
        const auto nl = s_.rfind('\n', at == 0 ? 0 : at - 1);
        return at == 0 || nl == std::string_view::npos ? 0 : nl + 1;
    }

    void skip_blank() {
        while (p_ < s_.size() && (s_[p_] == ' ' || s_[p_] == '\t')) ++p_;
    }
    // Past the rest of the line, its comment and its newline.
    void to_line_end() {
        const auto nl = s_.find('\n', p_);
        p_ = nl == std::string_view::npos ? s_.size() : nl + 1;
    }

    std::string simple_key() {
        std::string key;
        if (peek('"')) {
            ++p_;
            while (p_ < s_.size() && s_[p_] != '"') {
                if (s_[p_] == '\\' && p_ + 1 < s_.size()) {
                    const char e = s_[++p_];
                    key += e == 'n' ? '\n' : e == 't' ? '\t' : e == 'r' ? '\r' : e == 'b' ? '\b' : e == 'f' ? '\f' : e;
                    ++p_;
                    continue;
                }
                key += s_[p_++];
            }
            ++p_;
        } else if (peek('\'')) {
            const auto close = s_.find('\'', p_ + 1);
            key = std::string(s_.substr(p_ + 1, close - p_ - 1));
            p_ = close + 1;
        } else {
            while (p_ < s_.size() && (std::isalnum(static_cast<unsigned char>(s_[p_])) || s_[p_] == '_' || s_[p_] == '-'))
                key += s_[p_++];
        }
        return key;
    }

    Path key_path() {
        Path path;
        skip_blank();
        path.push_back(simple_key());
        skip_blank();
        while (peek('.')) {
            ++p_;
            skip_blank();
            path.push_back(simple_key());
            skip_blank();
        }
        return path;
    }

    void string() {
        if (starts("\"\"\"") || starts("'''")) {
            const std::string_view delim = s_.substr(p_, 3);
            p_ += 3;
            while (p_ < s_.size()) {
                if (delim[0] == '"' && s_[p_] == '\\') {
                    p_ += 2;
                    continue;
                }
                if (starts(delim)) {
                    p_ += 3;
                    // Up to two more quotes belong to the content.
                    while (peek(delim[0])) ++p_;
                    return;
                }
                ++p_;
            }
            return;
        }
        const char q = s_[p_++];
        while (p_ < s_.size() && s_[p_] != q) p_ += q == '"' && s_[p_] == '\\' ? 2 : 1;
        ++p_;
    }

    void value() {
        if (peek('"') || peek('\'')) return string();
        if (peek('[') || peek('{')) {
            int depth = 0;
            while (p_ < s_.size()) {
                const char c = s_[p_];
                if (c == '"' || c == '\'') {
                    string();
                    continue;
                }
                if (c == '#') {
                    const auto nl = s_.find('\n', p_);
                    p_ = nl == std::string_view::npos ? s_.size() : nl;
                    continue;
                }
                ++p_;
                if (c == '[' || c == '{') ++depth;
                else if ((c == ']' || c == '}') && --depth == 0) return;
            }
            return;
        }
        while (p_ < s_.size() && !std::string_view(" \t\r\n#,]}").contains(s_[p_])) ++p_;
    }

    std::string_view s_;
    std::size_t p_ = 0;
};

std::string encode_key(std::string_view key) {
    const bool bare = !key.empty() && std::ranges::all_of(key, [](char c) {
        return std::isalnum(static_cast<unsigned char>(c)) || c == '_' || c == '-';
    });
    if (bare) return std::string(key);
    return toml::write_value(toml::Value(std::string(key)));
}

std::string dotted(std::span<const std::string_view> path) {
    std::string out;
    for (auto key : path) {
        if (!out.empty()) out += '.';
        out += encode_key(key);
    }
    return out;
}

bool same(const Path& a, std::span<const std::string_view> b) { return std::ranges::equal(a, b); }

} // namespace

std::optional<std::string> TomlDoc::get(KeyPath path) const {
    if (path.empty()) return std::nullopt;
    const toml::Table* table = &parsed_->table;
    const toml::Value* item = nullptr;
    for (auto key : path) {
        if (table == nullptr) return std::nullopt;
        auto it = table->find(key);
        if (it == table->end()) return std::nullopt;
        item = &it->second;
        table = item->as_table();
    }
    if (const auto* s = item->as_string()) return *s;
    return std::nullopt;
}

Result<> TomlDoc::set(KeyPath path, std::optional<std::string_view> value) {
    if (path.empty()) return std::unexpected(empty_path());
    const KeyPath parents = path.first(path.size() - 1);
    // How many leading parents exist as tables.
    std::size_t existing = 0;
    const toml::Table* table = &parsed_->table;
    for (auto parent : parents) {
        auto it = table->find(parent);
        if (it == table->end()) break;
        table = it->second.as_table();
        if (table == nullptr) {
            if (!value) return {};
            return std::unexpected(Error::unsupported(what_, debug(parent) + " is not a table"));
        }
        ++existing;
    }
    if (existing < parents.size() && !value) return {};

    const std::vector<Item> items = Scanner(text_).items();
    auto found = std::ranges::find_if(items, [&](const Item& i) { return !i.header && same(i.path, path); });
    std::string text = text_;
    if (!value) {
        if (found == items.end()) {
            if (table->contains(path.back()))
                return std::unexpected(Error::unsupported(what_, debug(path.back()) + " is not on a line of its own"));
            return {};
        }
        text.erase(found->decor_start, found->end - found->decor_start);
    } else {
        const std::string encoded = toml::write_value(toml::Value(std::string(*value)));
        if (found != items.end()) {
            text.replace(found->value_start, found->value_end - found->value_start, encoded);
        } else if (existing == parents.size() && table->contains(path.back())) {
            return std::unexpected(Error::unsupported(what_, debug(path.back()) + " is not a string setting on its own line"));
        } else {
            const std::string line = encode_key(path.back()) + " = " + encoded + "\n";
            if (existing == parents.size()) {
                // The section of `parents`: its header, or the document root.
                std::optional<std::size_t> at;
                bool in_section = parents.empty();
                for (const Item& i : items) {
                    if (i.header) {
                        if (in_section && !parents.empty()) break;
                        in_section = !i.array && same(i.path, parents);
                        if (in_section) at = i.end;
                        else if (parents.empty()) break;
                        continue;
                    }
                    if (in_section) at = i.end;
                }
                std::string insert = line;
                if (!at && !parents.empty()) {
                    // An implicit table (only `[a.b]` exists for `a`): give it its own header.
                    if (!text.empty() && text.back() != '\n') text += '\n';
                    insert = (text.empty() ? "[" : "\n[") + dotted(parents) + "]\n" + line;
                    at = text.size();
                }
                const std::size_t pos = at.value_or(0);
                if (pos > 0 && text[pos - 1] != '\n') insert.insert(0, "\n");
                text.insert(pos, insert);
            } else {
                if (!text.empty() && text.back() != '\n') text += '\n';
                for (std::size_t level = existing; level < parents.size(); ++level) {
                    if (!text.empty()) text += '\n';
                    text += "[" + dotted(parents.first(level + 1)) + "]\n";
                }
                text += line;
            }
        }
    }
    auto reparsed = toml::parse(text);
    if (!reparsed) return std::unexpected(Error::edit_check(what_));
    TomlDoc next(std::move(text), what_, std::make_unique<Parsed>(Parsed{std::move(*reparsed)}));
    if (next.get(path) != (value ? std::optional<std::string>(std::string(*value)) : std::nullopt))
        return std::unexpected(Error::edit_check(what_));
    *this = std::move(next);
    return {};
}

} // namespace loa::harness::account_support
