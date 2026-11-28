#include "loa/core/value.hpp"

#include <limits>

namespace loa::core {

std::string to_string(NodeId id) { return std::to_string(id.value); }

std::string_view debug_name(ValueType type) noexcept {
    static constexpr std::array<std::string_view, 6> names{"Bool", "Int", "Float", "Str", "Bytes", "List"};
    return names[static_cast<std::size_t>(type)];
}

Value::Value(List v) : v_(std::move(v)) {}

Value::Value(const std::vector<std::string>& v) : v_(List(v.begin(), v.end())) {}

Value::Value(NodeId id)
    : v_(static_cast<std::int64_t>(
          id.value > static_cast<std::uint64_t>(std::numeric_limits<std::int64_t>::max())
              ? std::numeric_limits<std::int64_t>::max()
              : id.value)) {}

std::optional<bool> Value::as_bool() const noexcept {
    if (auto* v = std::get_if<bool>(&v_)) return *v;
    return std::nullopt;
}
std::optional<std::int64_t> Value::as_int() const noexcept {
    if (auto* v = std::get_if<std::int64_t>(&v_)) return *v;
    return std::nullopt;
}
std::optional<double> Value::as_float() const noexcept {
    if (auto* v = std::get_if<double>(&v_)) return *v;
    return std::nullopt;
}
std::optional<std::string_view> Value::as_str() const noexcept {
    if (auto* v = std::get_if<std::string>(&v_)) return std::string_view(*v);
    return std::nullopt;
}
std::optional<std::span<const std::uint8_t>> Value::as_bytes() const noexcept {
    if (auto* v = std::get_if<Bytes>(&v_)) return std::span<const std::uint8_t>(*v);
    return std::nullopt;
}
std::optional<std::span<const Value>> Value::as_list() const noexcept {
    if (auto* v = std::get_if<List>(&v_)) return std::span<const Value>(*v);
    return std::nullopt;
}

std::vector<std::string_view> Value::str_items() const {
    std::vector<std::string_view> out;
    for (const Value& item : as_list().value_or(std::span<const Value>{}))
        if (auto s = item.as_str()) out.push_back(*s);
    return out;
}

bool operator==(const Value& a, const Value& b) { return a.v_ == b.v_; }

} // namespace loa::core
