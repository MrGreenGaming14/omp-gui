#pragma once

// Shared helpers of the updater sources: JSON files and serde-style field access.

#include <cstdint>
#include <expected>
#include <functional>
#include <optional>
#include <string>
#include <string_view>

#include "loa/runtime/fs.hpp"
#include "loa/runtime/json.hpp"
#include "loa/settings/updates.hpp"
#include "loa/updater/error.hpp"

namespace loa::updater::detail {

using Json = loa::runtime::json::Value;
using Object = loa::runtime::json::Object;
using Array = loa::runtime::json::Array;
template <typename T>
using Decoded = std::expected<T, std::string>;

// Reads JSON at `path` and decodes it; a missing file is none.
template <typename T, typename F>
Result<std::optional<T>> read_json(const Path& path, F&& decode) {
    auto bytes = loa::runtime::fs::read_optional(path);
    if (!bytes) return std::unexpected(Error::io(path, bytes.error()));
    if (!*bytes) return std::optional<T>{};
    auto value = loa::runtime::json::parse(**bytes);
    if (!value) return std::unexpected(Error::json(path, value.error().message()));
    Decoded<T> decoded = decode(*value);
    if (!decoded) return std::unexpected(Error::json(path, decoded.error()));
    return std::optional<T>{std::move(*decoded)};
}

// Replaces `path` with `value` as pretty JSON, atomically.
inline Result<> write_json(const Path& path, const Json& value) {
    if (auto r = loa::runtime::fs::write_json_atomic(path, value); !r) return std::unexpected(Error::io(path, r.error()));
    return {};
}

// Thrown by the field helpers on a shape mismatch; `decoding` turns it into an error value.
struct DecodeError {
    std::string message;
};

const Object& object(const Json& v, std::string_view what);
const Json* field(const Object& o, std::string_view key);
const Json& required(const Object& o, std::string_view key);
std::string string(const Json& v);
// Missing or null is none.
std::optional<std::string> opt_string(const Json* v);
std::uint64_t u64(const Json& v);
std::uint32_t u32(const Json& v);
bool boolean(const Json& v);
const Array& array(const Json& v);
Version version(const Json& v);
std::optional<Version> opt_version(const Json* v);
Json opt(const std::optional<std::string>& v);
Json opt(const std::optional<Version>& v);

template <typename F>
auto decoding(F&& f) -> Decoded<decltype(f())> {
    try {
        return f();
    } catch (const DecodeError& e) {
        return std::unexpected(e.message);
    }
}

// A settings decoder's value, or `DecodeError`.
template <typename T>
T settings_value(const Json& v) {
    auto r = loa::settings::from_json<T>(v);
    if (!r) throw DecodeError{r.error().message()};
    return std::move(*r);
}

// A `Decoded` value, or `DecodeError`.
template <typename T>
T value(Decoded<T> r) {
    if (!r) throw DecodeError{std::move(r.error())};
    return std::move(*r);
}

} // namespace loa::updater::detail
