#pragma once

// Hand-written JSON Schema emitter for tool argument types. It reproduces what
// schemars 1.x (`schema_for!`) emits for the same Rust type, key order included,
// so `HostToolSpec::input_schema` stays byte-identical with the Rust app.

#include <cstdint>
#include <optional>
#include <string>
#include <string_view>
#include <utility>
#include <vector>

#include "loa/runtime/json.hpp"

namespace loa::contracts::schema {

using Json = loa::runtime::json::Value;

// Leaf schemas.
[[nodiscard]] Json string();
[[nodiscard]] Json boolean();
// Signed integer with schemars `format` (`int32`, `int64`, `int`).
[[nodiscard]] Json integer(std::string_view format);
// Unsigned integer (`uint32`, `uint64`, `uint` for usize): also `minimum: 0`.
[[nodiscard]] Json unsigned_integer(std::string_view format);
// `float` (f32) or `double` (f64).
[[nodiscard]] Json number(std::string_view format);
[[nodiscard]] Json array(Json items);
// `{"$ref": "#/$defs/<name>"}`; the definition goes into `Root::define`.
[[nodiscard]] Json ref(std::string_view name);
// A string literal: `{"type":"string","const":value}` (tags, described enum variants).
[[nodiscard]] Json constant(std::string_view value);
// A unit enum without variant docs: `{"type":"string","enum":[...]}`.
[[nodiscard]] Json string_enum(const std::vector<std::string_view>& values);
// `{"description"?, "oneOf": [...]}`: described unit enums and tagged enums.
[[nodiscard]] Json one_of(std::vector<Json> variants, std::string_view description = {});

// `Option<T>`: `"type": [t, "null"]`, or `anyOf [t, {"type":"null"}]` for a `$ref`.
[[nodiscard]] Json nullable(Json schema);
// `#[schemars(range(min, max))]`: sets `minimum`/`maximum` in place.
[[nodiscard]] Json range(Json schema, std::optional<std::int64_t> min, std::optional<std::int64_t> max = std::nullopt);
// A doc comment: `description` goes first.
[[nodiscard]] Json describe(Json schema, std::string_view description);
// A serializable `#[serde(default)]`: `default` goes last.
[[nodiscard]] Json with_default(Json schema, Json value);

// An object schema: a struct or a tagged enum variant.
class Object {
  public:
    Object& description(std::string text);
    // A field; `required` is false for `Option<T>` and `#[serde(default)]` fields.
    Object& property(std::string name, Json schema, bool required = true);
    // `#[serde(deny_unknown_fields)]`.
    Object& deny_unknown_fields();

    // `{"description"?, "type":"object", "properties"?, "additionalProperties"?, "required"?}`.
    [[nodiscard]] Json to_json() const;

  private:
    std::string description_;
    loa::runtime::json::Object properties_;
    std::vector<std::string> required_;
    bool deny_unknown_ = false;
};

// The root a tool's `Args` type emits: `$schema`, `title`, the object, `$defs`.
class Root {
  public:
    Root(std::string title, Object object) : title_(std::move(title)), object_(std::move(object)) {}
    // Adds a `$defs` entry; entries keep the order they were added in.
    Root& define(std::string name, Json schema);

    [[nodiscard]] Json to_json() const;
    // Compact text, as `serde_json::to_string`.
    [[nodiscard]] std::string dump() const;

  private:
    std::string title_;
    Object object_;
    loa::runtime::json::Object defs_;
};

} // namespace loa::contracts::schema
