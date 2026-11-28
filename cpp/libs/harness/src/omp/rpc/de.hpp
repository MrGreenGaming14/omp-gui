#pragma once

#include <algorithm>
#include <cstdint>
#include <limits>
#include <optional>
#include <string>
#include <string_view>
#include <vector>

#include "loa/harness/omp/rpc/command.hpp"

namespace loa::harness::omp::rpc::detail {

inline const Json* field(const Json& obj, std::string_view key) { return obj.find(key); }

inline bool string(const Json& obj, std::string_view key, std::string& out) {
    const auto* v = field(obj, key);
    if (!v || !v->as_string()) return false;
    out = *v->as_string();
    return true;
}
inline bool optional_string(const Json& obj, std::string_view key, std::optional<std::string>& out) {
    const auto* v = field(obj, key);
    if (!v || v->is_null()) { out.reset(); return true; }
    if (!v->as_string()) return false;
    out = *v->as_string();
    return true;
}
inline bool default_string(const Json& obj, std::string_view key, std::string& out) {
    if (!field(obj, key)) { out.clear(); return true; }
    return string(obj, key, out);
}
inline bool boolean(const Json& obj, std::string_view key, bool& out) {
    const auto* v = field(obj, key);
    if (!v || !v->as_bool()) return false;
    out = *v->as_bool();
    return true;
}
inline bool default_boolean(const Json& obj, std::string_view key, bool& out) {
    return !field(obj, key) || boolean(obj, key, out);
}
inline bool optional_boolean(const Json& obj, std::string_view key, std::optional<bool>& out) {
    const auto* v = field(obj, key);
    if (!v || v->is_null()) { out.reset(); return true; }
    if (!v->as_bool()) return false;
    out = *v->as_bool();
    return true;
}
inline bool uint64(const Json& obj, std::string_view key, std::uint64_t& out) {
    const auto* v = field(obj, key);
    if (!v) return false;
    auto n = v->as_u64();
    if (!n) return false;
    out = *n;
    return true;
}
inline bool uint32(const Json& obj, std::string_view key, std::uint32_t& out) {
    std::uint64_t n;
    if (!uint64(obj, key, n) || n > std::numeric_limits<std::uint32_t>::max()) return false;
    out = static_cast<std::uint32_t>(n);
    return true;
}
inline bool optional_uint64(const Json& obj, std::string_view key, std::optional<std::uint64_t>& out) {
    const auto* v = field(obj, key);
    if (!v || v->is_null()) { out.reset(); return true; }
    std::uint64_t n;
    if (!uint64(obj, key, n)) return false;
    out = n;
    return true;
}
inline bool optional_uint32(const Json& obj, std::string_view key, std::optional<std::uint32_t>& out) {
    const auto* v = field(obj, key);
    if (!v || v->is_null()) { out.reset(); return true; }
    std::uint32_t n;
    if (!uint32(obj, key, n)) return false;
    out = n;
    return true;
}
inline bool optional_int64(const Json& obj, std::string_view key, std::optional<std::int64_t>& out) {
    const auto* v = field(obj, key);
    if (!v || v->is_null()) { out.reset(); return true; }
    auto n = v->as_i64();
    if (!n) return false;
    out = *n;
    return true;
}
inline bool optional_json(const Json& obj, std::string_view key, std::optional<Json>& out) {
    const auto* v = field(obj, key);
    if (!v || v->is_null()) { out.reset(); return true; }
    out = *v;
    return true;
}
inline bool default_json(const Json& obj, std::string_view key, Json& out) {
    if (const auto* v = field(obj, key)) out = *v;
    else out = Json{};
    return true;
}
inline bool string_array(const Json& obj, std::string_view key, std::vector<std::string>& out) {
    const auto* v = field(obj, key);
    if (!v || !v->as_array()) return false;
    out.clear();
    out.reserve(v->as_array()->size());
    for (const auto& item : *v->as_array()) {
        if (!item.as_string()) return false;
        out.push_back(*item.as_string());
    }
    return true;
}
inline bool optional_string_array(const Json& obj, std::string_view key, std::optional<std::vector<std::string>>& out) {
    const auto* v = field(obj, key);
    if (!v || v->is_null()) { out.reset(); return true; }
    std::vector<std::string> values;
    if (!string_array(obj, key, values)) return false;
    out = std::move(values);
    return true;
}
inline bool default_string_array(const Json& obj, std::string_view key, std::vector<std::string>& out) {
    if (!field(obj, key)) { out.clear(); return true; }
    return string_array(obj, key, out);
}
inline bool object(const Json& obj) { return obj.is_object(); }
inline RpcError json_error(std::string_view what) { return RpcError::Json{std::string("invalid ") + std::string(what)}; }

// serde_json's flattened unknown fields, preserving insertion order.
inline loa::runtime::json::Object extra(const Json& obj, std::initializer_list<std::string_view> known) {
    loa::runtime::json::Object out;
    if (!obj.as_object()) return out;
    for (const auto& [key, value] : *obj.as_object()) {
        if (std::ranges::find(known, key) == known.end()) out.emplace_back(key, value);
    }
    return out;
}

} // namespace loa::harness::omp::rpc::detail
