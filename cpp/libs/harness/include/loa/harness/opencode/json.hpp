#pragma once

#include <cmath>
#include <cstdint>
#include <expected>
#include <limits>
#include <optional>
#include <span>
#include <string>
#include <string_view>
#include <vector>

#include "loa/runtime/json.hpp"

namespace loa::harness::opencode::json_util {
using Value = loa::runtime::json::Value;

inline std::string unescape(std::string_view token) {
    std::string result;
    for (std::size_t i = 0; i < token.size(); ++i) {
        if (token[i] == '~' && i + 1 < token.size() && (token[i + 1] == '0' || token[i + 1] == '1'))
            result += token[++i] == '0' ? '~' : '/';
        else result += token[i];
    }
    return result;
}
inline const Value* at(const Value& value, std::string_view pointer) {
    if (pointer.empty()) return &value;
    if (pointer.front() != '/') return nullptr;
    const Value* current = &value;
    std::size_t pos = 1;
    while (true) {
        const auto end = pointer.find('/', pos);
        const auto key = unescape(pointer.substr(pos, end == std::string_view::npos ? end : end - pos));
        if (auto* object = current->as_object()) current = current->find(key);
        else if (auto* array = current->as_array()) {
            if (key.empty() || (key.size() > 1 && key.front() == '0')) return nullptr;
            std::size_t index = 0;
            for (char c : key) {
                if (c < '0' || c > '9' || index > (std::numeric_limits<std::size_t>::max() - (c - '0')) / 10) return nullptr;
                index = index * 10 + (c - '0');
            }
            current = index < array->size() ? &(*array)[index] : nullptr;
        } else return nullptr;
        if (!current || end == std::string_view::npos) return current;
        pos = end + 1;
    }
}
inline const std::string* str_at(const Value& value, std::string_view pointer) {
    auto* found = at(value, pointer);
    return found ? found->as_string() : nullptr;
}
inline std::optional<std::int64_t> int_at(const Value& value, std::string_view pointer) {
    auto* found = at(value, pointer);
    if (!found) return std::nullopt;
    if (auto i = found->as_i64()) return i;
    auto f = found->as_f64();
    if (f && std::isfinite(*f) && std::trunc(*f) == *f && *f >= -0x1p63 && *f < 0x1p63)
        return static_cast<std::int64_t>(*f);
    return std::nullopt;
}
inline bool is_true(const Value& value, std::string_view pointer) {
    auto* found = at(value, pointer);
    return found && found->as_bool() && *found->as_bool();
}
// A copy of `value` without the leaves at `pointers`; emptied objects stay. Removal swaps the
// last member into the removed slot, as serde_json's `preserve_order` map does.
inline Value without(const Value& value, std::span<const std::string_view> pointers) {
    Value copy = value;
    for (auto pointer : pointers) {
        const auto slash = pointer.rfind('/');
        if (slash == std::string_view::npos) continue;
        auto* parent = const_cast<Value*>(at(copy, pointer.substr(0, slash)));
        auto* object = parent ? parent->as_object() : nullptr;
        if (!object) continue;
        const auto key = pointer.substr(slash + 1);
        for (std::size_t i = 0; i < object->size(); ++i) {
            if ((*object)[i].first != key) continue;
            if (i + 1 != object->size()) (*object)[i] = std::move(object->back());
            object->pop_back();
            break;
        }
    }
    return copy;
}
inline void put(Value& value, std::string_view pointer, Value leaf) {
    if (pointer.empty() || pointer.front() != '/') return;
    Value* current = &value;
    std::size_t pos = 1;
    while (true) {
        auto end = pointer.find('/', pos);
        auto key = std::string(pointer.substr(pos, end == std::string_view::npos ? end : end - pos));
        if (!current->is_object()) *current = loa::runtime::json::Object{};
        if (end == std::string_view::npos) { current->set(key, std::move(leaf)); return; }
        auto* next = current->find(key);
        current = &current->set(key, next ? *next : Value(loa::runtime::json::Object{}));
        pos = end + 1;
    }
}
inline std::string rest(const Value& value, std::span<const std::string_view> pointers) {
    Value copy = without(value, pointers);
    if (auto* object = copy.as_object(); object && object->empty()) return {};
    return loa::runtime::json::dump(copy);
}
inline std::expected<Value, loa::Error> parse_object(std::string_view text) {
    return text.empty() ? std::expected<Value, loa::Error>(Value(loa::runtime::json::Object{})) : loa::runtime::json::parse(text);
}
} // namespace loa::harness::opencode::json_util
