#pragma once

// Form cards: fields declared as a JSON Schema object, filled in, validated and answered as one
// JSON object. Port of `crates/loa-ui-session/src/decisions/form.rs`.

#include <cstddef>
#include <cstdint>
#include <expected>
#include <optional>
#include <string>
#include <string_view>
#include <variant>
#include <vector>

#include "loa/runtime/json.hpp"

namespace loa::ui::session::decisions::form {

using Json = loa::runtime::json::Value;

// Why a schema can't be shown as a form.
struct FormError {
    enum class Kind : std::uint8_t { Json, NotObject, Unsupported };
    Kind kind = Kind::NotObject;
    // `Json`: the parser's message.
    std::string json;
    // `Unsupported`: the field and its type.
    std::string field;
    std::string ty;

    [[nodiscard]] static FormError not_object() { return {}; }
    [[nodiscard]] static FormError unsupported(std::string field, std::string ty) {
        return {Kind::Unsupported, {}, std::move(field), std::move(ty)};
    }
    [[nodiscard]] std::string message() const;
    friend bool operator==(const FormError&, const FormError&) = default;
};

// A field's type, which picks its input widget. Enum and multi `labels[i]` show `values[i]`;
// multi is an array of an enum, answered as a JSON array in option order.
struct FieldType {
    enum class Kind : std::uint8_t { String, Number, Integer, Boolean, Enum, Multi };
    Kind kind = Kind::String;
    std::vector<std::string> labels;
    std::vector<Json> values;

    friend bool operator==(const FieldType&, const FieldType&) = default;
};

// One property of the schema; `label` is its `title`, else its name.
struct Field {
    std::string name;
    std::string label;
    std::optional<std::string> description;
    FieldType ty;
    bool required = false;
    friend bool operator==(const Field&, const Field&) = default;
};

// What the user entered: raw text of string and number fields, an enum index, multi ticks.
namespace value {
struct Text {
    std::string text;
    friend bool operator==(const Text&, const Text&) = default;
};
struct Bool {
    bool on = false;
    friend bool operator==(const Bool&, const Bool&) = default;
};
struct Choice {
    std::optional<std::size_t> index;
    friend bool operator==(const Choice&, const Choice&) = default;
};
struct Picks {
    std::vector<bool> picks;
    friend bool operator==(const Picks&, const Picks&) = default;
};
} // namespace value
using FieldValue = std::variant<value::Text, value::Bool, value::Choice, value::Picks>;

// Why a field was refused on submit.
enum class FieldError : std::uint8_t { Required, NotNumber, NotInteger };
// "Required", "Enter a number", "Enter a whole number".
[[nodiscard]] std::string_view message(FieldError e) noexcept;

// A parsed schema with the values entered so far and the errors of the last submit.
struct Form {
    std::vector<Field> fields;
    std::vector<FieldValue> values;
    std::vector<std::optional<FieldError>> errors;

    // Parses `properties`, `required` and per property `type`, `enum`, `default`, `title`,
    // `description`, filling in the defaults.
    [[nodiscard]] static std::expected<Form, FormError> parse(std::string_view schema);
    // Replaces the value of field `index` and clears its error.
    void set(std::size_t index, FieldValue value);
    // Ticks or unticks value `option` of multi field `index`.
    void toggle(std::size_t index, std::size_t option);
    // The answer when every field is valid: one JSON object of typed values, leaving out empty
    // optional fields. Otherwise records the errors.
    [[nodiscard]] std::optional<std::string> submit();

    friend bool operator==(const Form&, const Form&) = default;
};

} // namespace loa::ui::session::decisions::form

namespace loa::ui::session::decisions {
using form::FieldError;
using form::FieldType;
using form::FieldValue;
using form::Form;
using form::FormError;
} // namespace loa::ui::session::decisions
