#include "loa/ui/session/decisions/form.hpp"

#include <algorithm>
#include <charconv>
#include <cmath>
#include <utility>

namespace loa::ui::session::decisions::form {

namespace json = loa::runtime::json;

std::string FormError::message() const {
    switch (kind) {
    case Kind::Json: return "the field schema is not valid JSON: " + json;
    case Kind::NotObject: return "the field schema is not an object schema with `properties`";
    case Kind::Unsupported: return "field `" + field + "` has unsupported type `" + ty + "`";
    }
    return {};
}

std::string_view message(FieldError e) noexcept {
    switch (e) {
    case FieldError::Required: return "Required";
    case FieldError::NotNumber: return "Enter a number";
    case FieldError::NotInteger: return "Enter a whole number";
    }
    return {};
}

namespace {

// `s` trimmed of Unicode-agnostic ASCII whitespace (what Rust `str::trim` removes for ASCII input).
std::string_view trim(std::string_view s) {
    constexpr std::string_view ws = " \t\n\r\f\v";
    const auto b = s.find_first_not_of(ws);
    if (b == std::string_view::npos) return {};
    return s.substr(b, s.find_last_not_of(ws) - b + 1);
}

// Rust `parse` accepts a leading `+`; `from_chars` does not.
std::string_view unplus(std::string_view s) {
    if (s.size() > 1 && s[0] == '+' && s[1] != '-' && s[1] != '+') s.remove_prefix(1);
    return s;
}

std::optional<double> parse_f64(std::string_view s) {
    s = unplus(s);
    double v = 0;
    const auto [ptr, ec] = std::from_chars(s.data(), s.data() + s.size(), v, std::chars_format::general);
    if (ec != std::errc{} || ptr != s.data() + s.size()) return std::nullopt;
    return v;
}

std::optional<std::int64_t> parse_i64(std::string_view s) {
    s = unplus(s);
    std::int64_t v = 0;
    const auto [ptr, ec] = std::from_chars(s.data(), s.data() + s.size(), v);
    if (ec != std::errc{} || ptr != s.data() + s.size()) return std::nullopt;
    return v;
}

// `enum` of `p` as labels (strings as is, other values as JSON) and values.
std::optional<std::pair<std::vector<std::string>, std::vector<Json>>> enumeration(const Json& p) {
    const Json* e = p.find("enum");
    const json::Array* values = e ? e->as_array() : nullptr;
    if (!values) return std::nullopt;
    std::vector<std::string> labels;
    for (const auto& v : *values) labels.push_back(v.as_string() ? *v.as_string() : json::dump(v));
    return std::pair{std::move(labels), *values};
}

std::expected<FieldType, FormError> field_type(const std::string& name, const Json& prop) {
    if (auto e = enumeration(prop)) return FieldType{FieldType::Kind::Enum, std::move(e->first), std::move(e->second)};
    const Json* type = prop.find("type");
    if (type && type->as_string() && *type->as_string() == "array") {
        if (const Json* items = prop.find("items"))
            if (auto e = enumeration(*items)) return FieldType{FieldType::Kind::Multi, std::move(e->first), std::move(e->second)};
    }
    // `["string", "null"]` is a nullable string.
    std::string ty;
    if (!type) {
        ty = "string";
    } else if (const auto* s = type->as_string()) {
        ty = *s;
    } else if (const auto* types = type->as_array()) {
        std::vector<std::string> named;
        for (const auto& t : *types)
            if (const auto* s = t.as_string(); s && *s != "null") named.push_back(*s);
        if (named.size() != 1) return std::unexpected(FormError::unsupported(name, json::dump(*type)));
        ty = named[0];
    } else {
        return std::unexpected(FormError::unsupported(name, json::dump(*type)));
    }
    if (ty == "string") return FieldType{FieldType::Kind::String, {}, {}};
    if (ty == "number") return FieldType{FieldType::Kind::Number, {}, {}};
    if (ty == "integer") return FieldType{FieldType::Kind::Integer, {}, {}};
    if (ty == "boolean") return FieldType{FieldType::Kind::Boolean, {}, {}};
    return std::unexpected(FormError::unsupported(name, ty));
}

FieldValue initial_value(const FieldType& ty, const Json* def) {
    switch (ty.kind) {
    case FieldType::Kind::Boolean: return value::Bool{def && def->as_bool() ? *def->as_bool() : false};
    case FieldType::Kind::Enum: {
        value::Choice c;
        if (def) {
            const auto it = std::ranges::find(ty.values, *def);
            if (it != ty.values.end()) c.index = static_cast<std::size_t>(it - ty.values.begin());
        }
        return c;
    }
    case FieldType::Kind::Multi: {
        const json::Array* d = def ? def->as_array() : nullptr;
        value::Picks p;
        for (const auto& v : ty.values) p.picks.push_back(d && std::ranges::find(*d, v) != d->end());
        return p;
    }
    default:
        if (!def || def->is_null()) return value::Text{};
        if (const auto* s = def->as_string()) return value::Text{*s};
        return value::Text{json::dump(*def)};
    }
}

std::expected<std::optional<Json>, FieldError> convert(const Field& field, const FieldValue& value) {
    std::optional<Json> out;
    const auto k = field.ty.kind;
    if (const auto* b = std::get_if<value::Bool>(&value); b && k == FieldType::Kind::Boolean) {
        out = Json(b->on);
    } else if (const auto* c = std::get_if<value::Choice>(&value); c && k == FieldType::Kind::Enum) {
        if (c->index && *c->index < field.ty.values.size()) out = field.ty.values[*c->index];
    } else if (const auto* p = std::get_if<value::Picks>(&value); p && k == FieldType::Kind::Multi) {
        json::Array chosen;
        for (std::size_t i = 0; i < field.ty.values.size() && i < p->picks.size(); ++i)
            if (p->picks[i]) chosen.push_back(field.ty.values[i]);
        if (!chosen.empty()) out = Json(std::move(chosen));
    } else if (const auto* t = std::get_if<value::Text>(&value); t && !trim(t->text).empty()) {
        const auto raw = trim(t->text);
        if (k == FieldType::Kind::Number) {
            const auto v = parse_f64(raw);
            if (!v || !std::isfinite(*v)) return std::unexpected(FieldError::NotNumber);
            out = Json(*v);
        } else if (k == FieldType::Kind::Integer) {
            const auto v = parse_i64(raw);
            if (!v) return std::unexpected(FieldError::NotInteger);
            out = Json(*v);
        } else {
            out = Json(t->text);
        }
    }
    // Empty text, or a value of the wrong variant, counts as empty.
    if (!out && field.required) return std::unexpected(FieldError::Required);
    return out;
}

} // namespace

std::expected<Form, FormError> Form::parse(std::string_view schema) {
    auto parsed = json::parse(schema);
    if (!parsed) return std::unexpected(FormError{FormError::Kind::Json, parsed.error().message(), {}, {}});
    const Json* props = parsed->find("properties");
    const json::Object* properties = props ? props->as_object() : nullptr;
    if (!properties) return std::unexpected(FormError::not_object());
    std::vector<std::string> required;
    if (const Json* r = parsed->find("required"); r && r->as_array())
        for (const auto& v : *r->as_array())
            if (v.as_string()) required.push_back(*v.as_string());
    Form form;
    form.errors.assign(properties->size(), std::nullopt);
    for (const auto& [name, prop] : *properties) {
        auto ty = field_type(name, prop);
        if (!ty) return std::unexpected(std::move(ty.error()));
        form.values.push_back(initial_value(*ty, prop.find("default")));
        const auto text = [&](std::string_view key) -> std::optional<std::string> {
            const Json* v = prop.find(key);
            if (v && v->as_string()) return *v->as_string();
            return std::nullopt;
        };
        form.fields.push_back(Field{name, text("title").value_or(name), text("description"), std::move(*ty),
                                    std::ranges::find(required, name) != required.end()});
    }
    return form;
}

void Form::set(std::size_t index, FieldValue value) {
    if (index < values.size() && index < errors.size()) {
        values[index] = std::move(value);
        errors[index] = std::nullopt;
    }
}

void Form::toggle(std::size_t index, std::size_t option) {
    if (index >= values.size()) return;
    const auto* p = std::get_if<value::Picks>(&values[index]);
    if (!p || option >= p->picks.size()) return;
    auto picks = p->picks;
    picks[option] = !picks[option];
    set(index, value::Picks{std::move(picks)});
}

std::optional<std::string> Form::submit() {
    json::Object object;
    const auto n = std::min({fields.size(), values.size(), errors.size()});
    for (std::size_t i = 0; i < n; ++i) {
        auto converted = convert(fields[i], values[i]);
        if (converted) {
            errors[i] = std::nullopt;
            if (*converted) object.emplace_back(fields[i].name, std::move(**converted));
        } else {
            errors[i] = converted.error();
        }
    }
    if (!std::ranges::all_of(errors, [](const auto& e) { return !e.has_value(); })) return std::nullopt;
    return json::dump(Json(std::move(object)));
}

} // namespace loa::ui::session::decisions::form
