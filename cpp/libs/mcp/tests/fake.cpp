#include "fake.hpp"

#include <cstdio>
#include <cstdlib>
#include <span>
#include <unistd.h>

#include "loa/core/base64.hpp"

namespace fake {

namespace json = loa::runtime::json;

namespace {

json::Value tool(const char* name, const char* description) {
    return json::Object{{"name", name}, {"description", description}, {"inputSchema", json::Object{{"type", "object"}}}};
}

json::Value text_result(std::string text) {
    return json::Object{{"content", json::Array{json::Object{{"type", "text"}, {"text", std::move(text)}}}}, {"isError", false}};
}

json::Value error(int code, std::string message) { return json::Object{{"code", code}, {"message", std::move(message)}}; }

} // namespace

std::optional<json::Value> Fake::handle(const json::Value& message, const std::function<void(const json::Value&)>& notify) {
    const auto* id = message.find("id");
    const auto* method_value = message.find("method");
    if (id == nullptr || method_value == nullptr || method_value->as_string() == nullptr) return std::nullopt;
    const std::string method = *method_value->as_string();
    const json::Value empty = json::Object{};
    const auto* params = message.find("params");
    if (params == nullptr) params = &empty;

    std::optional<json::Value> result;
    std::optional<json::Value> failure;
    if (method == "initialize") {
        json::Object caps{{"tools", json::Object{{"listChanged", true}}}, {"resources", json::Object{}}};
        result = json::Object{{"protocolVersion", "2025-06-18"},
                              {"capabilities", std::move(caps)},
                              {"serverInfo", json::Object{{"name", "fake"}, {"version", "1"}}}};
    } else if (method == "ping") {
        result = json::Object{};
    } else if (method == "tools/list") {
        json::Array tools;
        for (const char* name : {"echo", "pid", "crash", "grow"}) tools.push_back(tool(name, name));
        tools.push_back(tool("vault_code", "Returns the current vault code. Takes no arguments."));
        tools.push_back(tool("picture", "picture"));
        if (grown_) tools.push_back(tool("grown", "grown"));
        result = json::Object{{"tools", std::move(tools)}};
    } else if (method == "tools/call") {
        const auto* name_value = params->find("name");
        const std::string name = name_value && name_value->as_string() ? *name_value->as_string() : "";
        const auto* arguments = params->find("arguments");
        if (name == "echo") {
            const auto* text = arguments ? arguments->find("text") : nullptr;
            result = text_result(text && text->as_string() ? *text->as_string() : "");
        } else if (name == "pid") {
            result = text_result(std::to_string(::getpid()));
        } else if (name == "crash") {
            std::fputs("fatal: crash requested\n", stderr);
            std::fflush(stderr);
            std::_Exit(3);
        } else if (name == "grow") {
            grown_ = true;
            notify(json::Object{{"jsonrpc", "2.0"}, {"method", "notifications/tools/list_changed"}});
            result = text_result("grown");
        } else if (name == "vault_code") {
            result = text_result(VAULT_CODE);
        } else if (name == "picture") {
            const auto png = loa::core::base64::encode(std::span<const std::uint8_t>(PNG));
            result = json::Object{{"content", json::Array{json::Object{{"type", "text"}, {"text", "a picture"}},
                                                          json::Object{{"type", "image"}, {"data", png}, {"mimeType", "image/png"}}}},
                                  {"isError", false}};
        } else {
            failure = error(-32602, "no tool " + name);
        }
    } else if (method == "resources/list") {
        json::Array resources{json::Object{{"uri", "surface://fake/tab1"}, {"name", "Tab 1"}, {"mimeType", "image/png"}},
                              json::Object{{"uri", "file:///notes.txt"}, {"name", "notes"}}};
        result = json::Object{{"resources", std::move(resources)}};
    } else if (method == "resources/read") {
        const auto* uri = params->find("uri");
        const std::string text = uri && uri->as_string() ? *uri->as_string() : "";
        if (text == "file:///notes.txt")
            result = json::Object{{"contents", json::Array{json::Object{{"uri", text}, {"text", "the notes"}}}}};
        else failure = error(-32002, "Resource not found");
    } else {
        failure = error(-32601, "Method not found");
    }
    json::Object reply{{"jsonrpc", "2.0"}, {"id", *id}};
    if (result) reply.emplace_back("result", std::move(*result));
    else reply.emplace_back("error", std::move(*failure));
    return json::Value(std::move(reply));
}

} // namespace fake
