#pragma once

// Comment preserving editor for YAML files made of block mappings, such as omp's
// `config.yml` and `models.yml` (port of `models/yaml.rs`).
//
// Edits are made on the document text: the editor finds the touched key's lines,
// rewrites only those lines and leaves every other byte alone. Each edit is checked:
// the new text is parsed and must equal the old document with exactly the intended
// change, otherwise the edit is refused and the document is unchanged.
//
// Parsing uses yaml-cpp's event API and resolves scalars the way yaml-rust2's
// `YamlLoader` does, so `Yaml` values compare like the Rust ones.

#include <cstdint>
#include <initializer_list>
#include <optional>
#include <span>
#include <string>
#include <string_view>
#include <utility>
#include <variant>
#include <vector>

#include "loa/runtime/json.hpp"
#include "loa/settings/models/error.hpp"

namespace loa::settings::models::yaml {

class Yaml;
using Array = std::vector<Yaml>;
// Insertion ordered, like yaml-rust2's `Hash` (a linked hash map); equality is order sensitive.
using Hash = std::vector<std::pair<Yaml, Yaml>>;

// A YAML node, as yaml-rust2's `Yaml`. Aliases are resolved while loading.
class Yaml {
  public:
    // A float kept as its source text (`Yaml::Real`).
    struct Real {
        std::string text;
        friend bool operator==(const Real&, const Real&) = default;
    };
    struct Null {
        friend bool operator==(Null, Null) = default;
    };
    // A value of the wrong form for its tag (`!!int abc`).
    struct BadValue {
        friend bool operator==(BadValue, BadValue) = default;
    };
    using Storage = std::variant<Null, Real, std::int64_t, std::string, bool, Array, Hash, BadValue>;

    Yaml() = default;
    Yaml(Null) {}
    Yaml(BadValue v) : v_(v) {}
    Yaml(Real v) : v_(std::move(v)) {}
    Yaml(int v) : v_(std::int64_t{v}) {}
    Yaml(const char* v) : v_(std::string(v)) {}
    Yaml(std::string_view v) : v_(std::string(v)) {}
    Yaml(std::int64_t v) : v_(v) {}
    Yaml(std::string v) : v_(std::move(v)) {}
    Yaml(bool v) : v_(v) {}
    Yaml(Array v) : v_(std::move(v)) {}
    Yaml(Hash v) : v_(std::move(v)) {}

    [[nodiscard]] bool is_null() const noexcept { return std::holds_alternative<Null>(v_); }
    [[nodiscard]] bool is_bad_value() const noexcept { return std::holds_alternative<BadValue>(v_); }
    [[nodiscard]] const std::string* as_str() const noexcept { return std::get_if<std::string>(&v_); }
    [[nodiscard]] std::optional<std::int64_t> as_i64() const noexcept;
    [[nodiscard]] std::optional<bool> as_bool() const noexcept;
    // `Real` parsed as a float (yaml-rust2 `as_f64`): integers are not converted.
    [[nodiscard]] std::optional<double> as_f64() const noexcept;
    [[nodiscard]] const Real* as_real() const noexcept { return std::get_if<Real>(&v_); }
    [[nodiscard]] const Array* as_vec() const noexcept { return std::get_if<Array>(&v_); }
    [[nodiscard]] const Hash* as_hash() const noexcept { return std::get_if<Hash>(&v_); }
    [[nodiscard]] Hash* as_mut_hash() noexcept { return std::get_if<Hash>(&v_); }

    // Mapping lookup by string key; nullptr when absent or not a mapping.
    [[nodiscard]] const Yaml* get(std::string_view key) const noexcept;

    [[nodiscard]] const Storage& storage() const noexcept { return v_; }
    friend bool operator==(const Yaml&, const Yaml&) = default;

    // yaml-rust2 `Yaml::from_str`: how a plain scalar resolves.
    [[nodiscard]] static Yaml from_str(std::string_view v);

  private:
    Storage v_;
};

// Looks up `path` in nested mappings.
[[nodiscard]] const Yaml* get_path(const Yaml& root, std::span<const std::string_view> path);

// A YAML document whose root is a block mapping (or empty), edited in place.
class YamlDocument {
  public:
    // Parses `text`. `what` names the document in errors (usually its path).
    [[nodiscard]] static Result<YamlDocument> parse(std::string_view text, std::string_view what);

    // The current document text.
    [[nodiscard]] const std::string& text() const noexcept { return text_; }
    // The parsed root mapping.
    [[nodiscard]] const Yaml& root() const noexcept { return root_; }
    // The value at `path` (a key per mapping level), if present.
    [[nodiscard]] const Yaml* get(std::span<const std::string_view> path) const { return get_path(root_, path); }
    [[nodiscard]] const Yaml* get(std::initializer_list<std::string_view> path) const {
        return get(std::span<const std::string_view>(path.begin(), path.size()));
    }

    // Sets the value at `path`, creating missing parent mappings. Only the lines of the
    // touched key change.
    [[nodiscard]] Result<> set(std::span<const std::string_view> path, Yaml value);
    [[nodiscard]] Result<> set(std::initializer_list<std::string_view> path, Yaml value) {
        return set(std::span<const std::string_view>(path.begin(), path.size()), std::move(value));
    }

    // Removes the key at `path`. A parent mapping left empty is removed too. Returns
    // whether the key existed.
    [[nodiscard]] Result<bool> remove(std::span<const std::string_view> path);
    [[nodiscard]] Result<bool> remove(std::initializer_list<std::string_view> path) {
        return remove(std::span<const std::string_view>(path.begin(), path.size()));
    }

  private:
    YamlDocument(std::string text, Yaml root, std::string what)
        : text_(std::move(text)), root_(std::move(root)), what_(std::move(what)) {}
    Result<> commit(std::string text, Yaml expected);

    std::string text_;
    Yaml root_;
    std::string what_;
};

// Parses `text` with yaml-rust2's `YamlLoader` semantics: every document, in order.
[[nodiscard]] std::expected<std::vector<Yaml>, std::string> load_all(std::string_view text);

// Builds a YAML string.
[[nodiscard]] Yaml ystr(std::string s);
// Builds a YAML sequence of strings.
[[nodiscard]] Yaml ystrs(std::span<const std::string> items);
[[nodiscard]] Yaml ystrs(std::initializer_list<std::string_view> items);
// Builds a YAML mapping from key/value pairs, in order.
[[nodiscard]] Yaml ymap(std::vector<std::pair<std::string, Yaml>> pairs);

// Converts a YAML value to JSON (for comparing with omp's JSON output).
[[nodiscard]] runtime::json::Value to_json(const Yaml& value);

} // namespace loa::settings::models::yaml
