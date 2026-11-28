#include "loa/harness/kit/jsonrpc.hpp"

#include <charconv>

namespace loa::harness::kit {

namespace json = loa::runtime::json;

namespace {

// Rust's `str::parse::<i64>`: an optional sign, then only digits.
std::optional<std::int64_t> parse_i64(std::string_view s) {
    if (s.starts_with('+')) s.remove_prefix(1);
    if (s.empty()) return std::nullopt;
    std::int64_t v = 0;
    auto [end, ec] = std::from_chars(s.data(), s.data() + s.size(), v);
    if (ec != std::errc{} || end != s.data() + s.size()) return std::nullopt;
    return v;
}

// Removes `key` from `msg` and returns its value (`null` when absent).
Json take(Json& msg, std::string_view key) {
    auto* obj = msg.as_object();
    if (!obj) return {};
    for (auto& [k, v] : *obj)
        if (k == key) return std::exchange(v, Json{});
    return {};
}

} // namespace

Json JsonRpc2::frame(Json msg) const {
    if (header) msg.set("jsonrpc", "2.0");
    return msg;
}

Json JsonRpc2::request(const Json& id, std::string_view method, Json params) const {
    return frame(json::Object{{"id", id}, {"method", method}, {"params", std::move(params)}});
}

Json JsonRpc2::notification(std::string_view method, Json params) const {
    if (params.is_null()) return frame(json::Object{{"method", method}});
    return frame(json::Object{{"method", method}, {"params", std::move(params)}});
}

Json JsonRpc2::response(Json id, std::string_view, RpcResult<Json> result) const {
    auto error = [&](std::int64_t code, std::string message) {
        return frame(json::Object{{"id", std::move(id)},
                                  {"error", json::Object{{"code", code}, {"message", std::move(message)}}}});
    };
    if (result) return frame(json::Object{{"id", std::move(id)}, {"result", std::move(*result)}});
    if (const auto* command = result.error().get<RpcError::Command>()) {
        auto code = command->code ? parse_i64(*command->code) : std::nullopt;
        return error(code.value_or(JSONRPC_INTERNAL), command->message);
    }
    return error(JSONRPC_INTERNAL, result.error().message());
}

std::optional<Incoming> JsonRpc2::classify(Json msg) const {
    Json params = take(msg, "params");
    std::optional<Json> id;
    if (const Json* found = msg.find("id"); found && !found->is_null()) id = take(msg, "id");
    const Json* method_value = msg.find("method");
    const std::string* method = method_value ? method_value->as_string() : nullptr;
    if (method && id) return incoming::Request{std::move(*id), *method, std::move(params)};
    if (method) return incoming::Notification{*method, std::move(params)};
    if (!id) return std::nullopt;
    if (msg.find("error")) {
        Json error = take(msg, "error");
        const Json* message = error.find("message");
        const std::string* text = message ? message->as_string() : nullptr;
        const Json* code = error.find("code");
        RpcError err = RpcError::Command{"request", text ? *text : "error",
                                         code ? std::optional<std::string>(json::dump(*code)) : std::nullopt};
        return incoming::Response{id_key(*id), std::unexpected(std::move(err))};
    }
    return incoming::Response{id_key(*id), take(msg, "result")};
}

} // namespace loa::harness::kit
