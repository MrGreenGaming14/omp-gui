#pragma once

// A TOML document as plain values: the in-memory form of `toml::Table` in the
// Rust app. Parsing goes through toml++ in one TU; writing mirrors the
// `toml` crate's `to_string` byte for byte (sorted keys, dotted table headers).

#include <cstdint>
#include <expected>
#include <map>
#include <string>
#include <string_view>
#include <variant>
#include <vector>

namespace loa::settings::app::toml {

// An offset date-time, local date-time, local date or local time, kept as its
// canonical text (`toml_datetime`'s `Display`).
struct Datetime {
    std::string text;
    friend bool operator==(const Datetime&, const Datetime&) = default;
};

struct Value;
using Array = std::vector<Value>;
using Table = std::map<std::string, Value, std::less<>>;

struct Value {
    using Storage = std::variant<std::string, std::int64_t, double, bool, Datetime, Array, Table>;
    Storage data;

    Value() = default;
    Value(std::string v) : data(std::move(v)) {}
    Value(const char* v) : data(std::string(v)) {}
    Value(std::int64_t v) : data(v) {}
    Value(int v) : data(std::int64_t{v}) {}
    Value(double v) : data(v) {}
    Value(bool v) : data(v) {}
    Value(Datetime v) : data(std::move(v)) {}
    Value(Array v) : data(std::move(v)) {}
    Value(Table v) : data(std::move(v)) {}

    [[nodiscard]] const std::string* as_string() const noexcept { return std::get_if<std::string>(&data); }
    [[nodiscard]] const std::int64_t* as_integer() const noexcept { return std::get_if<std::int64_t>(&data); }
    [[nodiscard]] const double* as_float() const noexcept { return std::get_if<double>(&data); }
    [[nodiscard]] const bool* as_bool() const noexcept { return std::get_if<bool>(&data); }
    [[nodiscard]] const Datetime* as_datetime() const noexcept { return std::get_if<Datetime>(&data); }
    [[nodiscard]] const Array* as_array() const noexcept { return std::get_if<Array>(&data); }
    [[nodiscard]] const Table* as_table() const noexcept { return std::get_if<Table>(&data); }
    [[nodiscard]] Table* as_table() noexcept { return std::get_if<Table>(&data); }

    // The serde type name used in error messages: "string", "integer", "table", ...
    [[nodiscard]] std::string_view type_name() const noexcept;

    friend bool operator==(const Value&, const Value&) = default;
};

// Parses TOML text; the error is toml++'s description with line and column.
[[nodiscard]] std::expected<Table, std::string> parse(std::string_view text);

// The document as the Rust `toml::to_string(&Table)` writes it.
[[nodiscard]] std::string write(const Table& doc);

// A value as it appears after `=`: `"text"`, `[1, 2]`, `{ a = 1 }`.
[[nodiscard]] std::string write_value(const Value& value);

} // namespace loa::settings::app::toml
