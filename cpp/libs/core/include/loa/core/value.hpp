#pragma once

#include <array>
#include <compare>
#include <concepts>
#include <cstddef>
#include <cstdint>
#include <functional>
#include <map>
#include <optional>
#include <span>
#include <string>
#include <string_view>
#include <type_traits>
#include <utility>
#include <variant>
#include <vector>

namespace loa::core {

// Id of a node. Stable for the life of the session and never reused: every
// insert takes an id above all ids the session has ever used.
struct NodeId {
    std::uint64_t value = 0;

    // The parent of the root. No node has this id.
    static const NodeId DOCUMENT;

    friend constexpr auto operator<=>(NodeId, NodeId) = default;
};
inline constexpr NodeId NodeId::DOCUMENT{0};

[[nodiscard]] std::string to_string(NodeId id);

// String enums (port of `loa_util::str_enum!`): specialize `StrEnum<E>` with a
// `values` array whose order matches the enumerators.
template <class E>
struct StrEnum;

template <class E>
concept str_enum = std::is_enum_v<E> && requires { StrEnum<E>::values; };

template <str_enum E>
[[nodiscard]] constexpr std::string_view as_str(E e) noexcept {
    return StrEnum<E>::values[static_cast<std::size_t>(e)];
}

template <str_enum E>
[[nodiscard]] constexpr std::optional<E> parse(std::string_view text) noexcept {
    const auto& values = StrEnum<E>::values;
    for (std::size_t i = 0; i < values.size(); ++i)
        if (values[i] == text) return static_cast<E>(i);
    return std::nullopt;
}

// Every enumerator in declaration order.
template <str_enum E>
[[nodiscard]] constexpr auto all() noexcept {
    std::array<E, StrEnum<E>::values.size()> out{};
    for (std::size_t i = 0; i < out.size(); ++i) out[i] = static_cast<E>(i);
    return out;
}

// The wire-level type of a `Value`. The schema declares one per attribute.
enum class ValueType : std::uint8_t { Bool, Int, Float, Str, Bytes, List };

// Rust `Debug` name of the type ("Bool", "Int", ...).
[[nodiscard]] std::string_view debug_name(ValueType type) noexcept;

// One attribute value.
class Value {
  public:
    using Bytes = std::vector<std::uint8_t>;
    using List = std::vector<Value>;
    using Variant = std::variant<bool, std::int64_t, double, std::string, Bytes, List>;

    Value(bool v) : v_(v) {}
    template <std::integral T>
        requires(!std::same_as<T, bool>)
    Value(T v) : v_(static_cast<std::int64_t>(v)) {}
    Value(double v) : v_(v) {}
    Value(std::string v) : v_(std::move(v)) {}
    Value(std::string_view v) : v_(std::string(v)) {}
    Value(const char* v) : v_(std::string(v)) {}
    Value(Bytes v) : v_(std::move(v)) {}
    Value(List v);
    // A list of strings.
    Value(const std::vector<std::string>& v);
    // A node id as an `Int` (ids stay far below `INT64_MAX`).
    Value(NodeId id);
    // A status or kind enum as its wire string.
    template <str_enum E>
    Value(E e) : v_(std::string(loa::core::as_str(e))) {}

    [[nodiscard]] ValueType type() const noexcept { return static_cast<ValueType>(v_.index()); }
    [[nodiscard]] const Variant& variant() const noexcept { return v_; }

    [[nodiscard]] std::optional<bool> as_bool() const noexcept;
    [[nodiscard]] std::optional<std::int64_t> as_int() const noexcept;
    [[nodiscard]] std::optional<double> as_float() const noexcept;
    [[nodiscard]] std::optional<std::string_view> as_str() const noexcept;
    [[nodiscard]] std::optional<std::span<const std::uint8_t>> as_bytes() const noexcept;
    [[nodiscard]] std::optional<std::span<const Value>> as_list() const noexcept;
    // The strings of a list value, skipping non-string items.
    [[nodiscard]] std::vector<std::string_view> str_items() const;

    friend bool operator==(const Value& a, const Value& b);

  private:
    Variant v_;
};

// Attributes ordered by name (BTreeMap semantics).
using Attrs = std::map<std::string, Value, std::less<>>;

// Anything that lists `(name, value)` pairs, as `Attrs`.
template <class R>
[[nodiscard]] Attrs into_attrs(R&& pairs) {
    Attrs out;
    for (auto&& [k, v] : pairs) out.insert_or_assign(std::string(k), Value(v));
    return out;
}

} // namespace loa::core

template <>
struct std::hash<loa::core::NodeId> {
    std::size_t operator()(loa::core::NodeId id) const noexcept { return std::hash<std::uint64_t>{}(id.value); }
};
