#include "loa/mcp/model.hpp"

namespace loa::mcp::model {

namespace json = loa::runtime::json;

namespace {

std::optional<std::string> opt_string(const json::Value& object, std::string_view key) {
    const auto* value = object.find(key);
    if (value == nullptr) return std::nullopt;
    const auto* text = value->as_string();
    return text ? std::optional(*text) : std::nullopt;
}

std::expected<std::string, std::string> req_string(const json::Value& object, std::string_view key) {
    const auto* value = object.find(key);
    const auto* text = value ? value->as_string() : nullptr;
    if (text == nullptr) return std::unexpected("missing or invalid `" + std::string(key) + "`");
    return *text;
}

} // namespace

std::expected<Tool, std::string> Tool::from_json(const json::Value& value) {
    if (!value.is_object()) return std::unexpected(std::string("a tool isn't an object"));
    Tool tool;
    auto name = req_string(value, "name");
    if (!name) return std::unexpected(name.error());
    tool.name = std::move(*name);
    tool.title = opt_string(value, "title");
    tool.description = opt_string(value, "description");
    if (const auto* schema = value.find("inputSchema"); schema && schema->as_object()) tool.input_schema = *schema->as_object();
    tool.raw = value;
    return tool;
}

std::expected<Resource, std::string> Resource::from_json(const json::Value& value) {
    if (!value.is_object()) return std::unexpected(std::string("a resource isn't an object"));
    Resource resource;
    auto uri = req_string(value, "uri");
    if (!uri) return std::unexpected(uri.error());
    auto name = req_string(value, "name");
    if (!name) return std::unexpected(name.error());
    resource.uri = std::move(*uri);
    resource.name = std::move(*name);
    resource.description = opt_string(value, "description");
    resource.mime_type = opt_string(value, "mimeType");
    resource.raw = value;
    return resource;
}

std::expected<ResourceContents, std::string> resource_contents_from_json(const json::Value& value) {
    if (!value.is_object()) return std::unexpected(std::string("resource contents aren't an object"));
    auto uri = req_string(value, "uri");
    if (!uri) return std::unexpected(uri.error());
    auto mime = opt_string(value, "mimeType");
    if (auto text = opt_string(value, "text")) return TextResourceContents{std::move(*uri), std::move(mime), std::move(*text)};
    if (auto blob = opt_string(value, "blob")) return BlobResourceContents{std::move(*uri), std::move(mime), std::move(*blob)};
    return std::unexpected(std::string("resource contents have neither `text` nor `blob`"));
}

const std::string* CallToolResult::text(std::size_t index) const {
    if (index >= content.size()) return nullptr;
    const auto& block = content[index];
    const auto* kind = block.find("type");
    if (kind == nullptr || kind->as_string() == nullptr || *kind->as_string() != "text") return nullptr;
    const auto* text = block.find("text");
    return text ? text->as_string() : nullptr;
}

std::expected<CallToolResult, std::string> CallToolResult::from_json(const json::Value& value) {
    if (!value.is_object()) return std::unexpected(std::string("a tool result isn't an object"));
    CallToolResult result;
    if (const auto* content = value.find("content"); content && content->as_array()) result.content = *content->as_array();
    if (const auto* structured = value.find("structuredContent"); structured && !structured->is_null()) result.structured_content = *structured;
    if (const auto* error = value.find("isError"); error && error->as_bool()) result.is_error = *error->as_bool();
    return result;
}

} // namespace loa::mcp::model
