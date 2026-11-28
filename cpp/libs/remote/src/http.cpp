#include "loa/remote/http.hpp"

#include <cctype>

#include "loa/mcp/toolkit.hpp"
#include "loa/remote/tools.hpp"
#include "loa/runtime/text.hpp"

namespace loa::remote {

namespace {

constexpr std::string_view INSTRUCTIONS =
    "Work on SSH hosts that do not run the app. Host names come from the user's Remote hosts list.";

bool iequals(std::string_view a, std::string_view b) {
    if (a.size() != b.size()) return false;
    for (std::size_t i = 0; i < a.size(); ++i) {
        if (std::tolower(static_cast<unsigned char>(a[i])) != std::tolower(static_cast<unsigned char>(b[i]))) return false;
    }
    return true;
}

} // namespace

std::expected<RemoteHttp, std::string> RemoteHttp::start(loa::contracts::ToolSet tools, loa::runtime::Executor& exec) {
    auto tokens = std::make_shared<Tokens>();
    auto auth = [tokens](const loa::mcp::HttpHeaders& headers) -> std::optional<loa::contracts::CallCtx> {
        for (const auto& [name, value] : headers) {
            if (!iequals(name, "authorization")) continue;
            if (!value.starts_with("Bearer ")) return std::nullopt;
            std::lock_guard lock(tokens->mu);
            const auto it = tokens->by_token.find(value.substr(7));
            if (it == tokens->by_token.end()) return std::nullopt;
            return loa::contracts::CallCtx{it->second};
        }
        return std::nullopt;
    };
    const auto mcp = loa::mcp::ToolSetMcp(std::move(tools), SERVER_NAME).with_instructions(INSTRUCTIONS);
    auto server = mcp.serve_http(exec, "127.0.0.1", 0, std::move(auth));
    if (!server) return std::unexpected(server.error().to_string());
    auto url = server->url();
    return RemoteHttp(std::make_unique<loa::mcp::ToolSetMcpHttp>(std::move(*server)), std::move(tokens), std::move(url));
}

RemoteHttp::RemoteHttp(std::unique_ptr<loa::mcp::ToolSetMcpHttp> server, std::shared_ptr<Tokens> tokens, std::string url)
    : server_(std::move(server)), tokens_(std::move(tokens)), url_(std::move(url)) {}

RemoteHttp::RemoteHttp(RemoteHttp&&) noexcept = default;
RemoteHttp& RemoteHttp::operator=(RemoteHttp&&) noexcept = default;

RemoteHttp::~RemoteHttp() { shutdown(); }

loa::contracts::McpServerSpec RemoteHttp::session_spec(std::string_view session_id) const {
    auto token = loa::runtime::text::random_hex(32);
    {
        std::lock_guard lock(tokens_->mu);
        tokens_->by_token.insert_or_assign(token, std::string(session_id));
    }
    std::map<std::string, std::string> headers;
    headers.emplace("Authorization", "Bearer " + token);
    return loa::contracts::McpServerSpec{std::string(SERVER_NAME), loa::contracts::McpHttp{url_, std::move(headers)}};
}

void RemoteHttp::forget(std::string_view session_id) const {
    std::lock_guard lock(tokens_->mu);
    std::erase_if(tokens_->by_token, [&](const auto& entry) { return entry.second == session_id; });
}

void RemoteHttp::shutdown() {
    if (server_) server_->stop();
}

} // namespace loa::remote
