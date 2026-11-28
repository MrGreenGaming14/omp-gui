// The one TU of loa_surfaces that includes cpp-httplib.
#include "loa/surfaces/hub/endpoint.hpp"

#include <asio.hpp>
#include <asio/use_future.hpp>
#include <httplib.h>

#include <algorithm>
#include <array>
#include <cctype>
#include <optional>
#include <thread>

#include "loa/core/base64.hpp"
#include "loa/mcp/toolkit.hpp"
#include "loa/runtime/text.hpp"
#include "loa/surfaces/browser/server.hpp"
#include "loa/surfaces/docs/server.hpp"
#include "loa/surfaces/hub/mcp.hpp"
#include "loa/surfaces/hub/service.hpp"
#include "loa/surfaces/hub/tools.hpp"
#include "loa/surfaces/vnc/vnc.hpp"

namespace loa::surfaces::hub {

namespace json = loa::runtime::json;
using contracts::Awaitable;
using contracts::CallCtx;

namespace {

constexpr std::array<std::string_view, 3> SERVERS = {browser::SERVER_NAME, docs::SERVER_NAME, vnc::SERVER_NAME};

// rmcp's `ErrorData::resource_not_found` and `internal_error` codes.
constexpr std::int64_t RESOURCE_NOT_FOUND = -32002;
constexpr std::int64_t INTERNAL_ERROR = -32603;

json::Value reply(const json::Value& id, json::Value result) {
    return json::Object{{"jsonrpc", "2.0"}, {"id", id}, {"result", std::move(result)}};
}

json::Value failure(const json::Value& id, std::int64_t code, std::string message) {
    return json::Object{{"jsonrpc", "2.0"}, {"id", id}, {"error", json::Object{{"code", code}, {"message", std::move(message)}}}};
}

const std::string* string_field(const json::Value* object, std::string_view key) {
    const auto* value = object ? object->find(key) : nullptr;
    return value ? value->as_string() : nullptr;
}

bool iequals(std::string_view a, std::string_view b) {
    return std::ranges::equal(a, b, [](char x, char y) {
        return std::tolower(static_cast<unsigned char>(x)) == std::tolower(static_cast<unsigned char>(y));
    });
}

// One built-in surface server: its scoped `ToolSetMcp` plus its surfaces as resources.
struct Scoped {
    mcp::ToolSetMcp tools;
    std::shared_ptr<SurfaceHub> hub;
    std::string scope;

    Awaitable<json::Value> list_resources(json::Value id) const {
        const auto listed = co_await hub->list();
        json::Array resources;
        for (const auto& l : listed) {
            if (l.info.uri.server() == scope) resources.push_back(resource(l.info));
        }
        co_return reply(id, json::Object{{"resources", std::move(resources)}});
    }

    // A document surface's file, or a pixel surface's current picture as PNG.
    Awaitable<json::Value> read_resource(json::Value id, const json::Value* params) const {
        const auto* text = string_field(params, "uri");
        const std::string requested = text ? *text : std::string();
        auto uri = cs::SurfaceUri::parse(requested);
        if (!uri || uri->server() != scope) co_return failure(id, RESOURCE_NOT_FOUND, requested);
        auto opened = co_await hub->open(*uri);
        if (!opened) co_return failure(id, RESOURCE_NOT_FOUND, requested);
        std::string mime;
        cs::Bytes data;
        if (opened->info.mode == cs::SurfaceMode::Document) {
            auto document = co_await opened->surface->document();
            if (!document) co_return failure(id, INTERNAL_ERROR, document.error().message());
            mime = std::move(document->mime);
            data = std::move(document->data);
        } else {
            auto png = co_await opened->surface->screenshot();
            if (!png) co_return failure(id, INTERNAL_ERROR, png.error().message());
            mime = "image/png";
            data = std::move(*png);
        }
        json::Object contents{{"uri", requested}, {"mimeType", std::move(mime)}, {"blob", core::base64::encode(data)}};
        co_return reply(id, json::Object{{"contents", json::Array{std::move(contents)}}});
    }

    // Answers one JSON-RPC request: resources here, everything else from the tool server.
    Awaitable<json::Value> handle(json::Value message) const {
        const std::string method = *string_field(&message, "method");
        const json::Value id = *message.find("id");
        if (method == "resources/list") co_return co_await list_resources(id);
        if (method == "resources/read") co_return co_await read_resource(id, message.find("params"));
        auto answer = co_await tools.handle(std::move(message), CallCtx{});
        json::Value out = answer ? std::move(*answer) : failure(id, INTERNAL_ERROR, "no answer");
        if (method == "initialize") {
            // The tool server declares tools; this one also declares resources.
            if (auto* top = out.as_object()) {
                for (auto& [key, result] : *top) {
                    if (key != "result") continue;
                    if (auto* fields = result.as_object()) {
                        for (auto& [name, capabilities] : *fields) {
                            if (name == "capabilities") capabilities.set("resources", json::Object{});
                        }
                    }
                }
            }
        }
        co_return out;
    }
};

} // namespace

struct McpEndpoint::Impl {
    std::string base;
    std::string token;
    httplib::Server server;
    std::thread thread;

    ~Impl() {
        server.stop();
        if (thread.joinable()) thread.join();
    }
};

McpEndpoint::McpEndpoint(std::unique_ptr<Impl> impl) : impl_(std::move(impl)) {}
McpEndpoint::~McpEndpoint() = default;
McpEndpoint::McpEndpoint(McpEndpoint&&) noexcept = default;
McpEndpoint& McpEndpoint::operator=(McpEndpoint&&) noexcept = default;

Awaitable<std::expected<McpEndpoint, loa::Error>> McpEndpoint::start(std::shared_ptr<SurfaceTools> tools) {
    const auto executor = co_await asio::this_coro::executor;
    auto impl = std::make_unique<Impl>();
    impl->token = runtime::text::random_hex(24);
    std::vector<std::pair<std::string, Scoped>> routes;
    for (const auto scope : SERVERS) {
        const auto& hub = tools->hub();
        routes.emplace_back("/" + std::string(scope),
                            Scoped{mcp::ToolSetMcp(tool_set(hub, scope), std::string(SERVER_PREFIX) + std::string(scope)), hub, std::string(scope)});
    }
    auto serve = [routes = std::move(routes), expected = "Bearer " + impl->token, executor](const httplib::Request& req,
                                                                                             httplib::Response& res) {
        const bool allowed = std::ranges::any_of(req.headers, [&](const auto& h) { return iequals(h.first, "authorization") && h.second == expected; });
        if (!allowed) {
            res.status = 401;
            return;
        }
        const auto route = std::ranges::find_if(routes, [&](const auto& r) { return r.first == req.path; });
        if (route == routes.end()) {
            res.status = 404;
            return;
        }
        if (req.method == "DELETE") {
            res.status = 200;
            return;
        }
        if (req.method != "POST") {
            res.status = 405;
            res.set_header("Allow", "POST, DELETE");
            return;
        }
        auto message = json::parse(req.body);
        if (!message) {
            res.status = 400;
            res.set_content(json::dump(failure(nullptr, -32700, "Parse error")), "application/json");
            return;
        }
        if (!string_field(&*message, "method") || !message->find("id")) {
            res.status = 202;
            return;
        }
        const bool initialize = *string_field(&*message, "method") == "initialize";
        const json::Value id = *message->find("id");
        json::Value answer;
        try {
            answer = asio::co_spawn(executor, route->second.handle(std::move(*message)), asio::use_future).get();
        } catch (const std::exception& e) {
            answer = failure(id, INTERNAL_ERROR, e.what());
        }
        res.status = 200;
        if (initialize) res.set_header("Mcp-Session-Id", runtime::text::random_hex(16));
        res.set_content(json::dump(answer), "application/json");
    };
    const std::string any = ".*";
    impl->server.Get(any, serve).Post(any, serve).Put(any, serve).Delete(any, serve).Patch(any, serve).Options(any, serve);
    const int port = impl->server.bind_to_any_port("127.0.0.1");
    if (port <= 0) co_return std::unexpected(loa::Error(loa::ErrorCode::io, "cannot bind 127.0.0.1:0"));
    impl->base = "http://127.0.0.1:" + std::to_string(port);
    auto* server = &impl->server;
    impl->thread = std::thread([server] { server->listen_after_bind(); });
    impl->server.wait_until_ready();
    co_return McpEndpoint(std::move(impl));
}

std::string McpEndpoint::url(std::string_view server) const { return impl_->base + "/" + std::string(server); }

const std::string& McpEndpoint::token() const noexcept { return impl_->token; }

std::vector<contracts::McpServerSpec> McpEndpoint::specs() const {
    std::vector<contracts::McpServerSpec> out;
    for (const auto server : SERVERS) {
        out.push_back(contracts::McpServerSpec{
            std::string(SERVER_PREFIX) + std::string(server),
            contracts::McpHttp{url(server), {{"Authorization", "Bearer " + impl_->token}}},
        });
    }
    return out;
}

} // namespace loa::surfaces::hub
