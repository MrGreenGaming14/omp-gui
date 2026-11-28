#include "util.hpp"

#include <limits>

namespace loa::updater::detail {

const Object& object(const Json& v, std::string_view what) {
    if (const auto* o = v.as_object()) return *o;
    throw DecodeError{"invalid type: expected " + std::string(what)};
}

const Json* field(const Object& o, std::string_view key) {
    for (const auto& [k, v] : o)
        if (k == key) return &v;
    return nullptr;
}

const Json& required(const Object& o, std::string_view key) {
    if (const auto* v = field(o, key)) return *v;
    throw DecodeError{"missing field `" + std::string(key) + "`"};
}

std::string string(const Json& v) {
    if (const auto* s = v.as_string()) return *s;
    throw DecodeError{"invalid type: expected a string"};
}

std::optional<std::string> opt_string(const Json* v) {
    if (!v || v->is_null()) return std::nullopt;
    return string(*v);
}

std::uint64_t u64(const Json& v) {
    if (auto n = v.as_u64()) return *n;
    throw DecodeError{"invalid type: expected u64"};
}

std::uint32_t u32(const Json& v) {
    const auto n = u64(v);
    if (n > std::numeric_limits<std::uint32_t>::max()) throw DecodeError{"invalid value: expected u32"};
    return static_cast<std::uint32_t>(n);
}

bool boolean(const Json& v) {
    if (const auto* b = v.as_bool()) return *b;
    throw DecodeError{"invalid type: expected a boolean"};
}

const Array& array(const Json& v) {
    if (const auto* a = v.as_array()) return *a;
    throw DecodeError{"invalid type: expected a sequence"};
}

Version version(const Json& v) {
    auto parsed = Version::parse(string(v));
    if (!parsed) throw DecodeError{parsed.error().message()};
    return std::move(*parsed);
}

std::optional<Version> opt_version(const Json* v) {
    if (!v || v->is_null()) return std::nullopt;
    return version(*v);
}

Json opt(const std::optional<std::string>& v) { return v ? Json(*v) : Json(); }
Json opt(const std::optional<Version>& v) { return v ? Json(v->to_string()) : Json(); }

} // namespace loa::updater::detail
