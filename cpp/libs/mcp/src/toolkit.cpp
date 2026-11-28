#include "loa/mcp/toolkit.hpp"

#include <asio.hpp>

#include <unistd.h>

#include <array>
#include <cerrno>
#include <condition_variable>
#include <cstring>
#include <map>
#include <mutex>

#include "httpd.hpp"
#include "loa/core/base64.hpp"
#include "loa/runtime/executor.hpp"
#include "loa/runtime/log.hpp"
#include "loa/runtime/text.hpp"

namespace loa::mcp {

namespace json = loa::runtime::json;
using contracts::CallCtx;
using contracts::ImageBlock;
using contracts::OutputBlock;
using contracts::TextBlock;
using contracts::ToolOutput;

namespace {

// `CARGO_PKG_VERSION` of the Rust crate, reported as the server version.
constexpr std::string_view VERSION = "0.1.0";
constexpr std::string_view LATEST_PROTOCOL = "2025-06-18";
constexpr std::array<std::string_view, 3> PROTOCOLS = {"2024-11-05", "2025-03-26", "2025-06-18"};

// Standard base64 with padding; none on any other byte.
std::optional<std::vector<std::byte>> base64_decode(std::string_view text) {
    static constexpr auto table = [] {
        std::array<int, 256> t{};
        t.fill(-1);
        constexpr std::string_view alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        for (std::size_t i = 0; i < alphabet.size(); ++i) t[static_cast<unsigned char>(alphabet[i])] = static_cast<int>(i);
        return t;
    }();
    if (text.size() % 4 != 0) return std::nullopt;
    std::vector<std::byte> out;
    out.reserve(text.size() / 4 * 3);
    for (std::size_t i = 0; i < text.size(); i += 4) {
        int v[4];
        int pad = 0;
        for (int k = 0; k < 4; ++k) {
            const char c = text[i + k];
            if (c == '=' && i + 4 == text.size() && k >= 2) {
                v[k] = 0;
                ++pad;
                continue;
            }
            if (pad > 0) return std::nullopt;
            v[k] = table[static_cast<unsigned char>(c)];
            if (v[k] < 0) return std::nullopt;
        }
        const std::uint32_t n = (static_cast<std::uint32_t>(v[0]) << 18) | (static_cast<std::uint32_t>(v[1]) << 12) |
                                (static_cast<std::uint32_t>(v[2]) << 6) | static_cast<std::uint32_t>(v[3]);
        out.push_back(static_cast<std::byte>(n >> 16));
        if (pad < 2) out.push_back(static_cast<std::byte>((n >> 8) & 0xff));
        if (pad < 1) out.push_back(static_cast<std::byte>(n & 0xff));
    }
    return out;
}

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

// An MCP request (`method` and `id`); notifications and responses aren't.
bool is_request(const json::Value& message) { return string_field(&message, "method") && message.find("id"); }

json::Value list_tools(const contracts::ToolSet& tools) {
    json::Array out;
    for (const auto& spec : tools.specs()) {
        auto schema = json::parse(spec.input_schema);
        json::Value input = schema && schema->is_object() ? std::move(*schema) : json::Value(json::Object{});
        const auto* tool = tools.get(spec.name);
        const auto hints = tool ? tool->annotations() : contracts::ToolAnnotations{};
        json::Object entry{{"name", spec.name}};
        if (hints.title) entry.emplace_back("title", *hints.title);
        entry.emplace_back("description", spec.description);
        entry.emplace_back("inputSchema", std::move(input));
        // No hints means MCP's defaults (which assume destructive), not an explicit "safe".
        if (hints != contracts::ToolAnnotations{}) {
            entry.emplace_back("annotations", json::Object{{"readOnlyHint", hints.read_only}, {"destructiveHint", hints.destructive}});
        }
        out.emplace_back(std::move(entry));
    }
    return json::Object{{"tools", std::move(out)}};
}

} // namespace

std::optional<OutputBlock> image(std::string_view data, std::string_view mime) {
    auto bytes = base64_decode(data);
    if (!bytes) return std::nullopt;
    return ImageBlock{std::string(mime), std::move(*bytes)};
}

json::Value to_result(const ToolOutput& output) {
    json::Array content;
    for (const auto& block : output.blocks) {
        if (const auto* text = std::get_if<TextBlock>(&block)) {
            content.emplace_back(json::Object{{"type", "text"}, {"text", text->text}});
        } else {
            const auto& img = std::get<ImageBlock>(block);
            const auto* ptr = reinterpret_cast<const std::uint8_t*>(img.data.data());
            content.emplace_back(json::Object{{"type", "image"}, {"data", core::base64::encode(std::span(ptr, img.data.size()))},
                                              {"mimeType", img.mime}});
        }
    }
    return json::Object{{"content", std::move(content)}, {"isError", output.is_error}};
}

ToolSetMcp::ToolSetMcp(contracts::ToolSet tools, std::string_view name)
    : tools_(std::make_shared<const contracts::ToolSet>(std::move(tools))), name_(name) {}

ToolSetMcp ToolSetMcp::with_instructions(std::string_view text) const {
    ToolSetMcp out = *this;
    out.instructions_ = std::string(text);
    return out;
}

contracts::Awaitable<std::optional<json::Value>> ToolSetMcp::handle(json::Value message, CallCtx ctx) const {
    if (!is_request(message)) co_return std::nullopt;
    const std::string method = *string_field(&message, "method");
    const json::Value id = *message.find("id");
    const json::Value* params = message.find("params");
    if (method == "initialize") {
        std::string version(LATEST_PROTOCOL);
        if (const auto* asked = string_field(params, "protocolVersion")) {
            for (auto known : PROTOCOLS) {
                if (*asked == known) version = *asked;
            }
        }
        json::Object result{{"protocolVersion", version},
                            {"capabilities", json::Object{{"tools", json::Object{}}}},
                            {"serverInfo", json::Object{{"name", name_}, {"version", std::string(VERSION)}}}};
        if (instructions_) result.emplace_back("instructions", *instructions_);
        co_return reply(id, std::move(result));
    }
    if (method == "ping") co_return reply(id, json::Object{});
    if (method == "tools/list") co_return reply(id, list_tools(*tools_));
    if (method == "tools/call") {
        const auto* name = string_field(params, "name");
        if (!name) co_return failure(id, -32602, "missing tool name");
        if (!tools_->get(*name)) co_return failure(id, -32602, "unknown tool: " + *name);
        const auto* arguments = params->find("arguments");
        json::Value args = arguments && arguments->is_object() ? *arguments : json::Value(json::Object{});
        auto tools = tools_;
        auto result = co_await tools->call(ctx, *name, std::move(args));
        co_return reply(id, to_result(result ? std::move(*result) : ToolOutput::error(std::move(result).error())));
    }
    co_return failure(id, -32601, "Method not found");
}

namespace {

// Requests in flight on a stdio connection, and the writer they share.
struct StdioState {
    struct Call {
        explicit Call(asio::io_context& context) : strand(asio::make_strand(context)) {}
        asio::cancellation_signal signal;
        asio::strand<asio::io_context::executor_type> strand;
        bool answered = false;
    };

    explicit StdioState(int out_fd) : out_fd(out_fd) {}

    int out_fd;
    std::mutex mutex;
    std::condition_variable idle;
    std::map<std::string, std::shared_ptr<Call>> calls;
    bool write_failed = false;

    // Writes one line; the lock must be held.
    void write_locked(const json::Value& message) {
        std::string line = json::dump(message);
        line.push_back('\n');
        std::size_t done = 0;
        while (done < line.size() && !write_failed) {
            const auto n = ::write(out_fd, line.data() + done, line.size() - done);
            if (n < 0 && errno == EINTR) continue;
            if (n <= 0) write_failed = true;
            else done += static_cast<std::size_t>(n);
        }
    }
};

// The answer to one request; parameters by value so the frame owns them.
contracts::Awaitable<json::Value> answer_call(ToolSetMcp self, json::Value message, json::Value id) {
    try {
        auto answer = co_await self.handle(std::move(message), {});
        co_return answer ? std::move(*answer) : json::Value();
    } catch (const std::exception& e) {
        co_return failure(id, -32603, e.what());
    }
}

} // namespace

Result<void> ToolSetMcp::serve(runtime::Executor& exec, int in_fd, int out_fd) const {
    auto state = std::make_shared<StdioState>(out_fd);
    auto dispatch = [&](json::Value message) {
        const auto* method = string_field(&message, "method");
        if (method && *method == "notifications/cancelled") {
            const auto* request = message.find("params") ? message.find("params")->find("requestId") : nullptr;
            if (!request) return;
            std::scoped_lock lock(state->mutex);
            auto it = state->calls.find(json::dump(*request));
            if (it == state->calls.end() || it->second->answered) return;
            it->second->answered = true;
            state->write_locked(reply(*request, to_result(ToolOutput::error("tool call cancelled"))));
            auto call = it->second;
            asio::post(call->strand, [call] { call->signal.emit(asio::cancellation_type::terminal); });
            return;
        }
        // Other notifications and responses need no answer.
        if (!is_request(message)) return;
        const json::Value id = *message.find("id");
        const std::string key = json::dump(id);
        auto call = std::make_shared<StdioState::Call>(exec.context());
        {
            std::scoped_lock lock(state->mutex);
            state->calls[key] = call;
        }
        asio::co_spawn(call->strand, answer_call(*this, std::move(message), id),
                       asio::bind_cancellation_slot(call->signal.slot(), [state, call, key](std::exception_ptr, json::Value answer) {
            std::scoped_lock lock(state->mutex);
            if (!call->answered && !answer.is_null()) state->write_locked(answer);
            call->answered = true;
            state->calls.erase(key);
            state->idle.notify_all();
        }));
    };

    std::string buffer;
    std::array<char, 64 * 1024> chunk{};
    std::optional<std::string> failed;
    for (;;) {
        const auto n = ::read(in_fd, chunk.data(), chunk.size());
        if (n < 0 && errno == EINTR) continue;
        if (n < 0) {
            failed = std::strerror(errno);
            break;
        }
        if (n == 0) break;
        buffer.append(chunk.data(), static_cast<std::size_t>(n));
        std::size_t start = 0;
        for (auto end = buffer.find('\n'); end != std::string::npos; end = buffer.find('\n', start)) {
            std::string_view line(buffer.data() + start, end - start);
            start = end + 1;
            if (!line.empty() && line.back() == '\r') line.remove_suffix(1);
            if (line.find_first_not_of(" \t") == std::string_view::npos) continue;
            auto message = json::parse(line);
            if (!message) {
                runtime::log::debug("loa_mcp", "mcp server: unreadable message: " + message.error().message());
                continue;
            }
            dispatch(std::move(*message));
        }
        buffer.erase(0, start);
    }

    // The client left: cancel what still runs and wait for it to wind down.
    std::unique_lock lock(state->mutex);
    for (auto& [key, call] : state->calls) {
        call->answered = true;
        asio::post(call->strand, [call] { call->signal.emit(asio::cancellation_type::terminal); });
    }
    state->idle.wait(lock, [&] { return state->calls.empty(); });
    if (failed) return std::unexpected(Error::bridge("MCP transport error: " + *failed));
    return {};
}

Result<void> ToolSetMcp::serve_stdio(runtime::Executor& exec) const { return serve(exec, STDIN_FILENO, STDOUT_FILENO); }

ToolSetMcpHttp::ToolSetMcpHttp(std::unique_ptr<detail::HttpServer> server, std::string url)
    : server_(std::move(server)), url_(std::move(url)) {}
ToolSetMcpHttp::~ToolSetMcpHttp() = default;
ToolSetMcpHttp::ToolSetMcpHttp(ToolSetMcpHttp&&) noexcept = default;
ToolSetMcpHttp& ToolSetMcpHttp::operator=(ToolSetMcpHttp&&) noexcept = default;
std::uint16_t ToolSetMcpHttp::port() const noexcept { return server_ ? server_->port() : 0; }
void ToolSetMcpHttp::stop() {
    if (server_) server_->stop();
}

Result<ToolSetMcpHttp> ToolSetMcp::serve_http(runtime::Executor& exec, const std::string& host, std::uint16_t port,
                                              HttpAuth auth) const {
    auto* context = &exec.context();
    auto handler = [self = *this, context, auth = std::move(auth)](const detail::HttpRequest& request) {
        detail::HttpResponse response;
        CallCtx ctx;
        if (auth) {
            auto admitted = auth(request.headers);
            if (!admitted) return detail::HttpResponse{.status = 401};
            ctx = std::move(*admitted);
        }
        if (request.method == "DELETE") return detail::HttpResponse{.status = 200};
        if (request.method != "POST") {
            response.status = 405;
            response.headers.emplace_back("Allow", "POST, DELETE");
            return response;
        }
        auto message = json::parse(request.body);
        if (!message) {
            response.status = 400;
            response.body = json::dump(failure(nullptr, -32700, "Parse error"));
            response.content_type = "application/json";
            return response;
        }
        if (!is_request(*message)) return detail::HttpResponse{.status = 202};
        const bool initialize = *string_field(&*message, "method") == "initialize";
        const json::Value id = *message->find("id");
        std::optional<json::Value> answer;
        try {
            answer = asio::co_spawn(*context, self.handle(std::move(*message), std::move(ctx)), asio::use_future).get();
        } catch (const std::exception& e) {
            answer = failure(id, -32603, e.what());
        }
        if (initialize) response.headers.emplace_back("Mcp-Session-Id", runtime::text::random_hex(16));
        response.body = json::dump(*answer);
        response.content_type = "application/json";
        return response;
    };
    auto server = detail::HttpServer::start(host, port, std::move(handler));
    if (!server) return std::unexpected(Error::bridge("MCP transport error: " + server.error()));
    std::string url = "http://" + host + ":" + std::to_string((*server)->port()) + "/mcp";
    return ToolSetMcpHttp(std::move(*server), std::move(url));
}

} // namespace loa::mcp
