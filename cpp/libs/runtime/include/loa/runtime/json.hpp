#pragma once

#include <cstdint>
#include <expected>
#include <optional>
#include <string>
#include <string_view>
#include <utility>
#include <variant>
#include <vector>

#include "loa/runtime/error.hpp"

namespace loa::runtime::json {

class Value;
using Array = std::vector<Value>;
// Insertion-ordered, like serde_json with `preserve_order`.
using Object = std::vector<std::pair<std::string, Value>>;

class Value {
  public:
    using Storage = std::variant<std::monostate, bool, std::int64_t, std::uint64_t, double, std::string, Array, Object>;

    Value() = default;
    Value(std::nullptr_t) {}
    Value(bool v) : v_(v) {}
    Value(int v) : v_(std::int64_t{v}) {}
    Value(std::int64_t v) : v_(v) {}
    Value(std::uint64_t v) : v_(v) {}
    Value(double v) : v_(v) {}
    Value(const char* v) : v_(std::string(v)) {}
    Value(std::string_view v) : v_(std::string(v)) {}
    Value(std::string v) : v_(std::move(v)) {}
    Value(Array v) : v_(std::move(v)) {}
    Value(Object v) : v_(std::move(v)) {}

    [[nodiscard]] bool is_null() const noexcept { return std::holds_alternative<std::monostate>(v_); }
    [[nodiscard]] bool is_bool() const noexcept { return std::holds_alternative<bool>(v_); }
    [[nodiscard]] bool is_number() const noexcept {
        return std::holds_alternative<std::int64_t>(v_) || std::holds_alternative<std::uint64_t>(v_) || std::holds_alternative<double>(v_);
    }
    [[nodiscard]] bool is_string() const noexcept { return std::holds_alternative<std::string>(v_); }
    [[nodiscard]] bool is_array() const noexcept { return std::holds_alternative<Array>(v_); }
    [[nodiscard]] bool is_object() const noexcept { return std::holds_alternative<Object>(v_); }

    [[nodiscard]] const bool* as_bool() const noexcept { return std::get_if<bool>(&v_); }
    [[nodiscard]] const std::string* as_string() const noexcept { return std::get_if<std::string>(&v_); }
    [[nodiscard]] const Array* as_array() const noexcept { return std::get_if<Array>(&v_); }
    [[nodiscard]] Array* as_array() noexcept { return std::get_if<Array>(&v_); }
    [[nodiscard]] const Object* as_object() const noexcept { return std::get_if<Object>(&v_); }
    [[nodiscard]] Object* as_object() noexcept { return std::get_if<Object>(&v_); }
    // Integral values that fit; doubles are not converted.
    [[nodiscard]] std::optional<std::int64_t> as_i64() const noexcept;
    [[nodiscard]] std::optional<std::uint64_t> as_u64() const noexcept;
    [[nodiscard]] std::optional<double> as_f64() const noexcept;

    // Object member lookup (first match); nullptr if absent or not an object.
    [[nodiscard]] const Value* find(std::string_view key) const noexcept;
    // Inserts or replaces, keeping the original position of an existing key.
    Value& set(std::string_view key, Value value);

    [[nodiscard]] const Storage& storage() const noexcept { return v_; }
    friend bool operator==(const Value&, const Value&) = default;

  private:
    Storage v_;
};

[[nodiscard]] std::expected<Value, loa::Error> parse(std::string_view text);
// Compact form (no whitespace), keys in stored order.
[[nodiscard]] std::string dump(const Value& value);

} // namespace loa::runtime::json
