#pragma once

// MCP protocol types returned by the client: tools, call results, resources. Each keeps the
// fields the app reads; `from_json` takes the wire object of the MCP spec.

#include <expected>
#include <optional>
#include <string>
#include <variant>
#include <vector>

#include "loa/runtime/json.hpp"

namespace loa::mcp::model {

using JsonObject = runtime::json::Object;

struct Tool {
    std::string name;
    std::optional<std::string> title;
    std::optional<std::string> description;
    // The JSON Schema object of the arguments.
    JsonObject input_schema;
    // The whole wire object, annotations and output schema included.
    runtime::json::Value raw;

    [[nodiscard]] static std::expected<Tool, std::string> from_json(const runtime::json::Value& value);
    friend bool operator==(const Tool&, const Tool&) = default;
};

struct Resource {
    std::string uri;
    std::string name;
    std::optional<std::string> description;
    std::optional<std::string> mime_type;
    runtime::json::Value raw;

    [[nodiscard]] static std::expected<Resource, std::string> from_json(const runtime::json::Value& value);
    friend bool operator==(const Resource&, const Resource&) = default;
};

struct TextResourceContents {
    std::string uri;
    std::optional<std::string> mime_type;
    std::string text;
    friend bool operator==(const TextResourceContents&, const TextResourceContents&) = default;
};

struct BlobResourceContents {
    std::string uri;
    std::optional<std::string> mime_type;
    // Base64.
    std::string blob;
    friend bool operator==(const BlobResourceContents&, const BlobResourceContents&) = default;
};

using ResourceContents = std::variant<TextResourceContents, BlobResourceContents>;

[[nodiscard]] std::expected<ResourceContents, std::string> resource_contents_from_json(const runtime::json::Value& value);

struct CallToolResult {
    // Content blocks as sent: objects tagged by `type` (`text`, `image`, `audio`, `resource`, `resource_link`).
    runtime::json::Array content;
    std::optional<runtime::json::Value> structured_content;
    std::optional<bool> is_error;

    // The text of content block `index` when it is a text block.
    [[nodiscard]] const std::string* text(std::size_t index = 0) const;

    [[nodiscard]] static std::expected<CallToolResult, std::string> from_json(const runtime::json::Value& value);
    friend bool operator==(const CallToolResult&, const CallToolResult&) = default;
};

} // namespace loa::mcp::model
