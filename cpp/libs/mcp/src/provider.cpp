#include "loa/mcp/provider.hpp"

#include <utility>

#include "httpd.hpp"

namespace loa::mcp::bridge {

namespace json = loa::runtime::json;

namespace {

// One requested tool call, carried in the prompt text after `CALL_PREFIX`.
struct CallMarker {
    std::string id;
    std::string tool;
    json::Object arguments;
};

// serde's derived struct decoding: an object with every field, or a sequence of them in order.
std::optional<CallMarker> marker_from_json(const json::Value& value) {
    const json::Value* fields[3] = {nullptr, nullptr, nullptr};
    if (value.is_object()) {
        fields[0] = value.find("id");
        fields[1] = value.find("tool");
        fields[2] = value.find("arguments");
    } else if (const auto* array = value.as_array(); array && array->size() == 3) {
        for (std::size_t i = 0; i < 3; ++i) fields[i] = &(*array)[i];
    } else {
        return std::nullopt;
    }
    if (!fields[0] || !fields[0]->as_string() || !fields[1] || !fields[1]->as_string() || !fields[2] || !fields[2]->as_object())
        return std::nullopt;
    return CallMarker{*fields[0]->as_string(), *fields[1]->as_string(), *fields[2]->as_object()};
}

std::optional<CallMarker> marker(std::string_view text) {
    if (!text.starts_with(CALL_PREFIX)) return std::nullopt;
    auto value = json::parse(text.substr(CALL_PREFIX.size()));
    if (!value) return std::nullopt;
    return marker_from_json(*value);
}

// The call marker in a user message, if it carries one.
std::optional<CallMarker> requested_call(const json::Value& message) {
    const auto* role = message.find("role");
    if (!role || !role->as_string() || *role->as_string() != "user") return std::nullopt;
    const auto* content = message.find("content");
    if (!content) return std::nullopt;
    if (const auto* text = content->as_string()) return marker(*text);
    if (const auto* parts = content->as_array()) {
        for (const auto& part : *parts) {
            const auto* text = part.find("text");
            if (!text || !text->as_string()) continue;
            if (auto found = marker(*text->as_string())) return found;
        }
    }
    return std::nullopt;
}

} // namespace

std::string call_prompt(std::string_view id, std::string_view tool, json::Object arguments) {
    json::Value marker = json::Object{{"id", id}, {"tool", tool}, {"arguments", std::move(arguments)}};
    return std::string(CALL_PREFIX) + json::dump(marker);
}

std::optional<std::string> completion(std::string_view body) {
    auto request = json::parse(body);
    if (!request) return std::nullopt;
    const auto* model_field = request->find("model");
    const std::string model = model_field && model_field->as_string() ? *model_field->as_string() : "scripted";
    std::optional<CallMarker> call;
    if (request->find("tools")) {
        const auto* messages = request->find("messages");
        if (messages && messages->as_array() && !messages->as_array()->empty()) call = requested_call(messages->as_array()->back());
    }
    json::Value delta;
    std::string finish;
    if (call) {
        json::Value function = json::Object{{"name", call->tool}, {"arguments", json::dump(json::Value(std::move(call->arguments)))}};
        json::Value sent = json::Object{{"index", 0}, {"id", call->id}, {"type", "function"}, {"function", std::move(function)}};
        delta = json::Object{{"role", "assistant"}, {"tool_calls", json::Array{std::move(sent)}}};
        finish = "tool_calls";
    } else {
        delta = json::Object{{"role", "assistant"}, {"content", DONE_TEXT}};
        finish = "stop";
    }
    std::string out;
    for (auto& choice : {json::Value(json::Object{{"index", 0}, {"delta", delta}, {"finish_reason", nullptr}}),
                         json::Value(json::Object{{"index", 0}, {"delta", json::Object{}}, {"finish_reason", finish}})}) {
        json::Value chunk = json::Object{{"id", "loa-omp-bridge"}, {"object", "chat.completion.chunk"}, {"created", 0},
                                         {"model", model}, {"choices", json::Array{choice}}};
        out += "data: " + json::dump(chunk) + "\n\n";
    }
    out += "data: [DONE]\n\n";
    return out;
}

ScriptedProvider::ScriptedProvider(std::unique_ptr<detail::HttpServer> server) : server_(std::move(server)) {}
ScriptedProvider::~ScriptedProvider() = default;

std::string ScriptedProvider::base_url() const { return "http://127.0.0.1:" + std::to_string(server_->port()) + "/v1"; }

std::expected<std::unique_ptr<ScriptedProvider>, std::string> ScriptedProvider::start(std::string_view token) {
    auto handler = [expected = "Bearer " + std::string(token)](const detail::HttpRequest& request) {
        const auto* auth = request.header("Authorization");
        if (!auth || *auth != expected) return detail::HttpResponse{.status = 401};
        if (request.method != "POST" || !request.path.ends_with("/chat/completions")) return detail::HttpResponse{.status = 404};
        auto stream = completion(request.body);
        if (!stream) return detail::HttpResponse{.status = 400};
        return detail::HttpResponse{.status = 200,
                                    .headers = {{"Cache-Control", "no-cache"}},
                                    .body = std::move(*stream),
                                    .content_type = "text/event-stream"};
    };
    auto server = detail::HttpServer::start("127.0.0.1", 0, std::move(handler));
    if (!server) return std::unexpected(std::move(server).error());
    return std::unique_ptr<ScriptedProvider>(new ScriptedProvider(std::move(*server)));
}

} // namespace loa::mcp::bridge
