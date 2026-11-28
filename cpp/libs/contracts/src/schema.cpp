#include "loa/contracts/schema.hpp"

namespace loa::contracts::schema {

namespace json = loa::runtime::json;

namespace {

Json typed(std::string_view type) { return json::Object{{"type", type}}; }

} // namespace

Json string() { return typed("string"); }

Json boolean() { return typed("boolean"); }

Json integer(std::string_view format) { return json::Object{{"type", "integer"}, {"format", format}}; }

Json unsigned_integer(std::string_view format) {
    return json::Object{{"type", "integer"}, {"format", format}, {"minimum", 0}};
}

Json number(std::string_view format) { return json::Object{{"type", "number"}, {"format", format}}; }

Json array(Json items) { return json::Object{{"type", "array"}, {"items", std::move(items)}}; }

Json ref(std::string_view name) { return json::Object{{"$ref", "#/$defs/" + std::string(name)}}; }

Json constant(std::string_view value) { return json::Object{{"type", "string"}, {"const", value}}; }

Json string_enum(const std::vector<std::string_view>& values) {
    json::Array list;
    list.reserve(values.size());
    for (auto v : values) list.emplace_back(v);
    return json::Object{{"type", "string"}, {"enum", std::move(list)}};
}

Json one_of(std::vector<Json> variants, std::string_view description) {
    json::Object out;
    if (!description.empty()) out.emplace_back("description", description);
    out.emplace_back("oneOf", json::Array(std::move(variants)));
    return out;
}

Json nullable(Json schema) {
    if (const auto* type = schema.find("type"); type && type->is_string()) {
        schema.set("type", json::Array{*type, "null"});
        return schema;
    }
    return json::Object{{"anyOf", json::Array{std::move(schema), typed("null")}}};
}

Json range(Json schema, std::optional<std::int64_t> min, std::optional<std::int64_t> max) {
    if (min) schema.set("minimum", *min);
    if (max) schema.set("maximum", *max);
    return schema;
}

Json describe(Json schema, std::string_view description) {
    auto* obj = schema.as_object();
    if (!obj) return schema;
    obj->insert(obj->begin(), {"description", description});
    return schema;
}

Json with_default(Json schema, Json value) {
    schema.set("default", std::move(value));
    return schema;
}

Object& Object::description(std::string text) {
    description_ = std::move(text);
    return *this;
}

Object& Object::property(std::string name, Json schema, bool required) {
    if (required) required_.push_back(name);
    properties_.emplace_back(std::move(name), std::move(schema));
    return *this;
}

Object& Object::deny_unknown_fields() {
    deny_unknown_ = true;
    return *this;
}

Json Object::to_json() const {
    json::Object out;
    if (!description_.empty()) out.emplace_back("description", description_);
    out.emplace_back("type", "object");
    if (!properties_.empty()) out.emplace_back("properties", properties_);
    if (deny_unknown_) out.emplace_back("additionalProperties", false);
    if (!required_.empty()) {
        json::Array required(required_.begin(), required_.end());
        out.emplace_back("required", std::move(required));
    }
    return out;
}

Root& Root::define(std::string name, Json schema) {
    defs_.emplace_back(std::move(name), std::move(schema));
    return *this;
}

Json Root::to_json() const {
    json::Object out{{"$schema", "https://json-schema.org/draft/2020-12/schema"}, {"title", title_}};
    auto object = object_.to_json();
    for (auto& member : *object.as_object()) out.push_back(std::move(member));
    if (!defs_.empty()) out.emplace_back("$defs", defs_);
    return out;
}

std::string Root::dump() const { return json::dump(to_json()); }

} // namespace loa::contracts::schema
