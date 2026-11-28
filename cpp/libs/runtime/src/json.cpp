#include "loa/runtime/json.hpp"

#include <yyjson.h>

#include <cstdlib>
#include <limits>
#include <memory>

namespace loa::runtime::json {
namespace {

Value from_yy(yyjson_val* v) {
    switch (yyjson_get_type(v)) {
    case YYJSON_TYPE_NULL: return Value{};
    case YYJSON_TYPE_BOOL: return Value(yyjson_get_bool(v));
    case YYJSON_TYPE_NUM:
        if (yyjson_is_uint(v)) {
            const std::uint64_t u = yyjson_get_uint(v);
            if (u <= static_cast<std::uint64_t>(std::numeric_limits<std::int64_t>::max())) return Value(static_cast<std::int64_t>(u));
            return Value(u);
        }
        if (yyjson_is_sint(v)) return Value(yyjson_get_sint(v));
        return Value(yyjson_get_real(v));
    case YYJSON_TYPE_STR: return Value(std::string(yyjson_get_str(v), yyjson_get_len(v)));
    case YYJSON_TYPE_ARR: {
        Array out;
        out.reserve(yyjson_arr_size(v));
        yyjson_val* item;
        yyjson_arr_iter it = yyjson_arr_iter_with(v);
        while ((item = yyjson_arr_iter_next(&it)) != nullptr) out.push_back(from_yy(item));
        return Value(std::move(out));
    }
    case YYJSON_TYPE_OBJ: {
        Object out;
        out.reserve(yyjson_obj_size(v));
        yyjson_val* key;
        yyjson_obj_iter it = yyjson_obj_iter_with(v);
        while ((key = yyjson_obj_iter_next(&it)) != nullptr) {
            std::string k(yyjson_get_str(key), yyjson_get_len(key));
            // serde_json keeps the last duplicate in the first key's position.
            bool replaced = false;
            for (auto& [ek, ev] : out)
                if (ek == k) {
                    ev = from_yy(yyjson_obj_iter_get_val(key));
                    replaced = true;
                    break;
                }
            if (!replaced) out.emplace_back(std::move(k), from_yy(yyjson_obj_iter_get_val(key)));
        }
        return Value(std::move(out));
    }
    default: return Value{};
    }
}

yyjson_mut_val* to_yy(yyjson_mut_doc* doc, const Value& value) {
    return std::visit(
        [doc](const auto& v) -> yyjson_mut_val* {
            using T = std::decay_t<decltype(v)>;
            if constexpr (std::is_same_v<T, std::monostate>) return yyjson_mut_null(doc);
            else if constexpr (std::is_same_v<T, bool>) return yyjson_mut_bool(doc, v);
            else if constexpr (std::is_same_v<T, std::int64_t>) return yyjson_mut_sint(doc, v);
            else if constexpr (std::is_same_v<T, std::uint64_t>) return yyjson_mut_uint(doc, v);
            else if constexpr (std::is_same_v<T, double>) return yyjson_mut_real(doc, v);
            else if constexpr (std::is_same_v<T, std::string>) return yyjson_mut_strn(doc, v.data(), v.size());
            else if constexpr (std::is_same_v<T, Array>) {
                yyjson_mut_val* arr = yyjson_mut_arr(doc);
                for (const auto& item : v) yyjson_mut_arr_append(arr, to_yy(doc, item));
                return arr;
            } else {
                yyjson_mut_val* obj = yyjson_mut_obj(doc);
                for (const auto& [k, item] : v) yyjson_mut_obj_add(obj, yyjson_mut_strn(doc, k.data(), k.size()), to_yy(doc, item));
                return obj;
            }
        },
        value.storage());
}

} // namespace

std::optional<std::int64_t> Value::as_i64() const noexcept {
    if (auto* i = std::get_if<std::int64_t>(&v_)) return *i;
    return std::nullopt; // uint64 values above INT64_MAX do not fit
}

std::optional<std::uint64_t> Value::as_u64() const noexcept {
    if (auto* u = std::get_if<std::uint64_t>(&v_)) return *u;
    if (auto* i = std::get_if<std::int64_t>(&v_); i && *i >= 0) return static_cast<std::uint64_t>(*i);
    return std::nullopt;
}

std::optional<double> Value::as_f64() const noexcept {
    if (auto* d = std::get_if<double>(&v_)) return *d;
    if (auto* i = std::get_if<std::int64_t>(&v_)) return static_cast<double>(*i);
    if (auto* u = std::get_if<std::uint64_t>(&v_)) return static_cast<double>(*u);
    return std::nullopt;
}

const Value* Value::find(std::string_view key) const noexcept {
    const Object* obj = as_object();
    if (obj == nullptr) return nullptr;
    for (const auto& [k, v] : *obj)
        if (k == key) return &v;
    return nullptr;
}

Value& Value::set(std::string_view key, Value value) {
    if (!is_object()) v_ = Object{};
    Object& obj = std::get<Object>(v_);
    for (auto& [k, v] : obj)
        if (k == key) return v = std::move(value);
    return obj.emplace_back(std::string(key), std::move(value)).second;
}

std::expected<Value, loa::Error> parse(std::string_view text) {
    yyjson_read_err err{};
    std::unique_ptr<yyjson_doc, decltype(&yyjson_doc_free)> doc(
        yyjson_read_opts(const_cast<char*>(text.data()), text.size(), 0, nullptr, &err), &yyjson_doc_free);
    if (!doc) {
        return std::unexpected(loa::Error(ErrorCode::parse,
                                          std::string("json: ") + (err.msg ? err.msg : "parse error") + " at byte " + std::to_string(err.pos)));
    }
    return from_yy(yyjson_doc_get_root(doc.get()));
}

std::string dump(const Value& value) {
    std::unique_ptr<yyjson_mut_doc, decltype(&yyjson_mut_doc_free)> doc(yyjson_mut_doc_new(nullptr), &yyjson_mut_doc_free);
    yyjson_mut_doc_set_root(doc.get(), to_yy(doc.get(), value));
    std::size_t len = 0;
    // Non-finite doubles are written as null, as serde_json does.
    char* out = yyjson_mut_write(doc.get(), YYJSON_WRITE_INF_AND_NAN_AS_NULL, &len);
    if (out == nullptr) return "null";
    std::string result(out, len);
    std::free(out);
    return result;
}

} // namespace loa::runtime::json
