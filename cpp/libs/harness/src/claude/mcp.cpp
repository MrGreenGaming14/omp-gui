#include "loa/harness/claude/mcp.hpp"

#include <asio/awaitable.hpp>

#include <algorithm>
#include <expected>
#include <utility>

namespace loa::harness::claude::mcp {
namespace json = loa::runtime::json;
namespace {

constexpr std::string_view PROTOCOL_VERSION = "2025-06-18";
// Cargo workspace package version of loa-harness.
constexpr std::string_view SERVER_VERSION = "0.1.0";

const kit::Json* member(const kit::Json& value, std::string_view key) { return value.find(key); }

const kit::Json* param(const kit::Json& message, std::string_view key) {
    const kit::Json* params = member(message, "params");
    return params ? member(*params, key) : nullptr;
}

std::string_view string_or_empty(const kit::Json* value) {
    if (value) {
        if (const auto* text = value->as_string()) return *text;
    }
    return {};
}

kit::Json id_of(const kit::Json& message) {
    if (const kit::Json* id = member(message, "id")) return *id;
    return {};
}

kit::Json response(const kit::Json& message, kit::Json result) {
    return json::Object{{"jsonrpc", "2.0"}, {"id", id_of(message)}, {"result", std::move(result)}};
}

} // namespace

kit::Json error(const kit::Json& message, std::string text) {
    return json::Object{{"jsonrpc", "2.0"},
                        {"id", id_of(message)},
                        {"error", json::Object{{"code", -32601}, {"message", std::move(text)}}}};
}

loa::contracts::Awaitable<kit::Json> handle(const std::vector<loa::contracts::HostToolSpec>& tools,
                                            const std::shared_ptr<const loa::contracts::HostTools>& handler,
                                            std::string_view session_id, const kit::Json& message) {
    const std::string_view method = string_or_empty(member(message, "method"));
    if (method == "initialize") {
        const kit::Json* requested = param(message, "protocolVersion");
        const auto* requested_string = requested ? requested->as_string() : nullptr;
        co_return response(message, json::Object{
            {"protocolVersion", requested_string ? kit::Json(*requested_string) : kit::Json(PROTOCOL_VERSION)},
            {"capabilities", json::Object{{"tools", json::Object{{"listChanged", false}}}}},
            {"serverInfo", json::Object{{"name", HOST_SERVER}, {"version", SERVER_VERSION}}}});
    }
    if (method == "ping") co_return response(message, json::Object{});
    if (method == "tools/list") {
        json::Array listed;
        listed.reserve(tools.size());
        for (const auto& tool : tools) {
            auto schema = json::parse(tool.input_schema);
            kit::Json input_schema = schema && schema->is_object() ? std::move(*schema) : kit::Json(json::Object{{"type", "object"}});
            listed.emplace_back(json::Object{{"name", tool.name},
                                             {"description", tool.description},
                                             {"inputSchema", std::move(input_schema)}});
        }
        co_return response(message, json::Object{{"tools", std::move(listed)}});
    }
    if (method == "tools/call") {
        const std::string_view name = string_or_empty(param(message, "name"));
        std::expected<std::string, std::string> outcome;
        if (std::none_of(tools.begin(), tools.end(), [name](const auto& tool) { return tool.name == name; })) {
            outcome = std::unexpected("unknown tool `" + std::string(name) + "`");
        } else if (!handler) {
            outcome = std::unexpected("host tools have no handler");
        } else {
            const kit::Json* args = param(message, "arguments");
            outcome = co_await handler->call(std::string(session_id), std::string(name), args ? json::dump(*args) : "{}");
        }
        const bool is_error = !outcome;
        std::string text = is_error ? std::move(outcome.error()) : std::move(*outcome);
        co_return response(message, json::Object{{"content", json::Array{json::Object{{"type", "text"}, {"text", std::move(text)}}}},
                                                  {"isError", is_error}});
    }
    if (method.starts_with("notifications/")) co_return response(message, json::Object{});
    co_return error(message, "method `" + std::string(method) + "` not found");
}

} // namespace loa::harness::claude::mcp
