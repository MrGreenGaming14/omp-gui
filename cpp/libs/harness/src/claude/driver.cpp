#include "loa/harness/claude/driver.hpp"

#include <algorithm>
#include <array>
#include <chrono>
#include <fstream>
#include <iomanip>
#include <mutex>
#include <sstream>

#include <asio/steady_timer.hpp>
#include <asio/this_coro.hpp>
#include <asio/co_spawn.hpp>
#include <asio/detached.hpp>
#include <openssl/rand.h>

#include "loa/harness/claude/mapper.hpp"
#include "loa/harness/claude/mcp.hpp"
#include "loa/harness/claude/native.hpp"
#include "loa/runtime/exec.hpp"

namespace loa::harness::claude {
namespace {
using namespace loa::contracts;
using Json = kit::Json;
using loa::runtime::json::Array;
using loa::runtime::json::Object;

const Json* field(const Json& obj, std::string_view key) { return obj.find(key); }
std::string string(const Json* value) { return value && value->as_string() ? *value->as_string() : std::string{}; }
std::string uuid() {
    std::array<unsigned char, 16> bytes{};
    if (RAND_bytes(bytes.data(), bytes.size()) != 1) return {};
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    std::ostringstream out;
    for (std::size_t i = 0; i < bytes.size(); ++i) {
        if (i == 4 || i == 6 || i == 8 || i == 10) out << '-';
        out << std::hex << std::setfill('0') << std::setw(2) << unsigned(bytes[i]);
    }
    return out.str();
}

class Shared final : public kit::Inbound, public std::enable_shared_from_this<Shared> {
  public:
    Shared(Mapper mapper, SessionConfig config) : mapper_(std::move(mapper)), config_(std::move(config)) {}
    void set_writer(kit::Writer writer) { writer_.emplace(std::move(writer)); }
    SessionState state() const { std::scoped_lock lock(mutex_); return mapper_.state(); }
    bool turn_open() const { std::scoped_lock lock(mutex_); return mapper_.turn_open(); }
    std::string native_id() const { std::scoped_lock lock(mutex_); return mapper_.native_id().value_or("default"); }
    template<class Fn> auto mapped(Fn&& fn) const {
        std::scoped_lock lock(mutex_);
        return fn(mapper_);
    }
    kit::RpcResult<void> write(const Json& line) const {
        if (!writer_) return std::unexpected(kit::RpcError(kit::RpcError::Closed{}));
        return writer_->send(line);
    }
    Awaitable<kit::RpcResult<Json>> on_request(std::string method, Json) const override {
        co_return std::unexpected(kit::RpcError(kit::RpcError::Protocol{"unsupported control request `" + method + "`"}));
    }
    Awaitable<void> on_notification(std::string, Json msg) const override {
        auto out = mapped([&](Mapper& mapper) { return mapper.handle(std::move(msg)); });
        if (!out || !*out) co_return;
        if (auto* write = std::get_if<Write>(&**out)) {
            (void)this->write(write->value);
        } else if (auto* call = std::get_if<Mcp>(&**out)) {
            auto self = shared_from_this();
            auto request = std::move(*call);
            auto executor = co_await asio::this_coro::executor;
            asio::co_spawn(executor, serve_mcp(std::move(self), std::move(request)), asio::detached);
        }
    }
    static Awaitable<void> serve_mcp(std::shared_ptr<const Shared> self, Mcp request) {
        auto reply = request.server == mcp::HOST_SERVER
            ? co_await mcp::handle(self->config_.host_tools, self->config_.host_tool_handler,
                                   self->config_.session_id, request.message)
            : mcp::error(request.message, "server `" + request.server + "` not found");
        (void)self->write(control_response(request.request_id, Json(Object{{"mcp_response", std::move(reply)}})));
    }
    Awaitable<void> on_closed(std::optional<loa::runtime::exec::ExitStatus>, std::string) const override {
        (void)mapped([](Mapper& mapper) { return mapper.exited(); });
        co_return;
    }
  private:
    mutable std::mutex mutex_;
    mutable Mapper mapper_;
    SessionConfig config_;
    std::optional<kit::Writer> writer_;
};

class ClaudeSession final : public HarnessSession {
  public:
    ClaudeSession(std::shared_ptr<Shared> shared, kit::StdioRpc<ClaudeControl> rpc,
                  std::optional<std::vector<HarnessModel>> models)
        : shared_(std::move(shared)), rpc_(std::move(rpc)), models_(std::move(models)) {}
    Awaitable<Result<>> send(Prompt prompt) const override {
        if (state() == SessionState::Closed) co_return std::unexpected(AdapterError(AdapterError::Closed{}));
        Json content(prompt.text);
        if (!prompt.attachments.empty()) {
            Array blocks{Json(Object{{"type", "text"}, {"text", prompt.text}})};
            for (auto& attachment : prompt.attachments) {
                auto mime = attachment.mime.value_or("");
                if (mime.empty()) {
                    auto extension = attachment.path.extension().string();
                    if (extension == ".png") mime = "image/png";
                    else if (extension == ".jpg" || extension == ".jpeg") mime = "image/jpeg";
                    else if (extension == ".gif") mime = "image/gif";
                    else if (extension == ".webp") mime = "image/webp";
                }
                if (mime.starts_with("image/") && mime != "image/svg+xml") {
                    std::ifstream stream(attachment.path, std::ios::binary);
                    if (!stream) co_return std::unexpected(AdapterError(AdapterError::Io{loa::Error(loa::ErrorCode::io, "reading attachment failed")}));
                    std::string bytes((std::istreambuf_iterator<char>(stream)), {});
                    static constexpr char alphabet[] = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
                    std::string encoded;
                    encoded.reserve((bytes.size() + 2) / 3 * 4);
                    for (std::size_t i = 0; i < bytes.size(); i += 3) {
                        auto a = static_cast<unsigned char>(bytes[i]);
                        auto b = i + 1 < bytes.size() ? static_cast<unsigned char>(bytes[i + 1]) : 0;
                        auto c = i + 2 < bytes.size() ? static_cast<unsigned char>(bytes[i + 2]) : 0;
                        encoded += alphabet[a >> 2]; encoded += alphabet[((a & 3) << 4) | (b >> 4)];
                        encoded += i + 1 < bytes.size() ? alphabet[((b & 15) << 2) | (c >> 6)] : '=';
                        encoded += i + 2 < bytes.size() ? alphabet[c & 63] : '=';
                    }
                    blocks.emplace_back(Object{{"type", "image"}, {"source", Object{{"type", "base64"}, {"media_type", mime}, {"data", std::move(encoded)}}}});
                } else {
                    blocks.emplace_back(Object{{"type", "text"}, {"text", "Attached file: " + attachment.path.string()}});
                }
            }
            content = Json(std::move(blocks));
        }
        auto id = uuid();
        auto mapped = shared_->mapped([&](Mapper& mapper) { return mapper.open_turn(prompt.text, prompt.by, id); });
        if (!mapped) co_return std::unexpected(mapped.error());
        Json line(Object{{"type", "user"}, {"message", Object{{"role", "user"}, {"content", std::move(content)}}},
                         {"parent_tool_use_id", nullptr}, {"session_id", shared_->native_id()}, {"uuid", id}});
        auto written = shared_->write(line);
        if (!written) co_return std::unexpected(written.error().to_adapter_error());
        co_return Result<>{};
    }
    Awaitable<Result<>> interrupt() const override {
        if (!shared_->turn_open()) co_return Result<>{};
        auto response = co_await control("interrupt", Json(Object{}));
        if (!response) co_return std::unexpected(response.error());
        co_return shared_->mapped([](Mapper& mapper) { return mapper.interrupt(); });
    }
    Awaitable<Result<>> answer(loa::core::NodeId decision, Answer answer) const override {
        auto response = shared_->mapped([&](Mapper& mapper) { return mapper.answer(decision, std::move(answer)); });
        if (!response) co_return std::unexpected(response.error());
        if (*response) {
            auto written = shared_->write(**response);
            if (!written) co_return std::unexpected(written.error().to_adapter_error());
        }
        co_return Result<>{};
    }
    Awaitable<Result<>> set_model(ModelSelection selection) const override {
        if (selection.role) co_return std::unexpected(AdapterError(AdapterError::Unsupported{HARNESS, "model roles"}));
        Json request(Object{{"model", selection.model}});
        auto response = co_await control("set_model", std::move(request));
        if (!response) co_return std::unexpected(response.error());
        auto recorded = shared_->mapped([&](Mapper& mapper) { return mapper.record_model(selection.model); });
        co_return recorded;
    }
    Awaitable<Result<>> set_mode(loa::core::SessionMode mode) const override {
        Json request(Object{{"mode", mode == loa::core::SessionMode::Plan ? "plan" : "default"}});
        auto response = co_await control("set_permission_mode", std::move(request));
        if (!response) co_return std::unexpected(response.error());
        auto recorded = shared_->mapped([&](Mapper& mapper) { return mapper.record_mode(mode); });
        co_return recorded;
    }
    Awaitable<Result<std::vector<HarnessModel>>> models() const override {
        if (models_) co_return *models_;
        co_return std::unexpected(AdapterError(AdapterError::Unsupported{HARNESS, "listing models with this claude version"}));
    }
    Awaitable<Result<>> compact(std::optional<std::string> instructions) const override {
        co_return co_await send(Prompt{instructions ? "/compact " + *instructions : "/compact", {}, ""});
    }
    std::string_view harness() const override { return HARNESS; }
    SessionState state() const override { return shared_->state(); }
    Awaitable<Result<>> wait_idle() const override {
        auto executor = co_await asio::this_coro::executor;
        asio::steady_timer timer(executor);
        while (state() != SessionState::Idle && state() != SessionState::Closed) {
            timer.expires_after(std::chrono::milliseconds(10));
            co_await timer.async_wait(asio::use_awaitable);
        }
        co_return Result<>{};
    }
    Awaitable<Result<>> shutdown() const override {
        auto interrupted = shared_->mapped([](Mapper& mapper) { return mapper.interrupt(); });
        if (!interrupted) co_return interrupted;
        (void)co_await rpc_.shutdown(EXIT_GRACE);
        co_return shared_->mapped([](Mapper& mapper) { return mapper.exited(); });
    }
  private:
    Awaitable<Result<Json>> control(std::string subtype, Json request) const {
        auto result = co_await rpc_.request_as(Json("loa_" + std::to_string(rpc_.next_id())), std::move(subtype), std::move(request));
        if (!result) {
            if (auto* command = result.error().get<kit::RpcError::Command>())
                co_return std::unexpected(AdapterError(AdapterError::Protocol{command->message}));
            co_return std::unexpected(result.error().to_adapter_error());
        }
        co_return *result;
    }
    std::shared_ptr<Shared> shared_;
    kit::StdioRpc<ClaudeControl> rpc_;
    std::optional<std::vector<HarnessModel>> models_;
};

Awaitable<Result<SessionPtr>> launch(SessionConfig config, Mapper mapper, const std::vector<std::string>& resume,
                                     const std::filesystem::path& executable) {
    if (!config.executable) config.executable = executable;
    auto args = command_args(config, resume);
    auto cmd = loa::runtime::exec::command(*config.executable, std::move(args), {}, config.cwd);
    for (const auto& [key, value] : config.env) if (key != "CLAUDECODE") cmd.env.emplace_back(key, value);
    // A nested Claude Code shell must not leak its marker to the child.
    cmd.env_remove.push_back("CLAUDECODE");
    auto executor = co_await asio::this_coro::executor;
    auto shared = std::make_shared<Shared>(std::move(mapper), std::move(config));
    auto rpc = kit::StdioRpc<ClaudeControl>::spawn(executor, cmd, ClaudeControl{}, shared);
    if (!rpc) co_return std::unexpected(rpc.error().to_adapter_error());
    shared->set_writer(rpc->writer());
    Json id("loa_" + std::to_string(rpc->next_id()));
    Json request(Object{{"hooks", nullptr}});
    auto init = co_await rpc->request_as(std::move(id), "initialize", std::move(request));
    if (!init) {
        if (auto* command = init.error().get<kit::RpcError::Command>())
            co_return std::unexpected(AdapterError(AdapterError::Protocol{command->message}));
        co_return std::unexpected(init.error().to_adapter_error());
    }
    std::optional<std::vector<HarnessModel>> models;
    if (auto* list = field(*init, "models"); list && list->as_array()) {
        models.emplace();
        for (const auto& item : *list->as_array()) {
            auto id = field(item, "value");
            if (!id || !id->as_string()) continue;
            HarnessModel model{*id->as_string(), string(field(item, "displayName")), "anthropic", {}};
            if (model.name.empty()) model.name = model.id;
            if (auto levels = field(item, "supportedEffortLevels"); levels && levels->as_array())
                for (const auto& level : *levels->as_array()) if (level.as_string()) model.efforts.push_back(*level.as_string());
            models->push_back(std::move(model));
        }
    }
    SessionPtr session = std::make_unique<ClaudeSession>(std::move(shared), std::move(*rpc), std::move(models));
    co_return Result<SessionPtr>(std::move(session));
}
} // namespace

std::vector<std::string> command_args(const SessionConfig& config, const std::vector<std::string>& resume) {
    std::vector<std::string> args{"--print", "--output-format", "stream-json", "--verbose", "--input-format", "stream-json",
                                  "--forward-subagent-text", "--include-partial-messages", "--permission-prompt-tool", "stdio"};
    auto flag = [&](std::string key, std::string value) { args.push_back(std::move(key)); args.push_back(std::move(value)); };
    if (config.model) flag("--model", *config.model);
    if (config.effort) flag("--effort", *config.effort);
    if (config.mode == loa::core::SessionMode::Plan) flag("--permission-mode", "plan");
    if (config.context && !config.context->empty()) flag("--append-system-prompt", *config.context);
    if (!config.disabled_tools.empty()) {
        std::string disabled;
        for (const auto& tool : config.disabled_tools) { if (!disabled.empty()) disabled += ','; disabled += tool; }
        flag("--disallowedTools", std::move(disabled));
    }
    Object servers;
    for (const auto& server : config.mcp_servers) {
        Json transport;
        if (auto* stdio = std::get_if<McpStdio>(&server.transport)) {
            Array command_args;
            for (const auto& arg : stdio->args) command_args.emplace_back(arg);
            Object env;
            for (const auto& [key, value] : stdio->env) env.emplace_back(key, value);
            transport = Json(Object{{"args", std::move(command_args)}, {"command", stdio->command}, {"env", std::move(env)}, {"type", "stdio"}});
        } else {
            const auto& http = std::get<McpHttp>(server.transport);
            Object headers;
            for (const auto& [key, value] : http.headers) headers.emplace_back(key, value);
            transport = Json(Object{{"headers", std::move(headers)}, {"type", "http"}, {"url", http.url}});
        }
        servers.emplace_back(server.name, std::move(transport));
    }
    if (!config.host_tools.empty()) servers.emplace_back(std::string(mcp::HOST_SERVER), Json(Object{{"name", std::string(mcp::HOST_SERVER)}, {"type", "sdk"}}));
    if (!servers.empty()) {
        std::ranges::sort(servers, {}, &std::pair<std::string, Json>::first);
        flag("--mcp-config", loa::runtime::json::dump(Json(Object{{"mcpServers", std::move(servers)}})));
    }
    args.insert(args.end(), resume.begin(), resume.end());
    return args;
}

std::optional<std::string> fork_point(const loa::core::Dom& dom) {
    std::optional<std::string> latest;
    loa::core::NodeId last{};
    for (const auto& n : dom.iter()) {
        const auto* node = &n;
        std::optional<std::string_view> id;
        if (node->kind == loa::core::ElementKind::Message) id = node->str_attr(loa::core::attr::ID);
        else if (node->kind == loa::core::ElementKind::Call || node->kind == loa::core::ElementKind::Result)
            id = node->str_attr(UUID_ATTR);
        if (id && (!latest || node->id > last)) { last = node->id; latest = *id; }
    }
    return latest;
}

Result<std::vector<std::string>> resume_args(const loa::core::Dom& dom, bool fork) {
    const auto* root = dom.root();
    auto id = root ? root->str_attr(loa::core::attr::NATIVE_ID) : std::nullopt;
    if (!id) return std::unexpected(AdapterError(AdapterError::Format{"the session has no Claude Code session id"}));
    std::vector<std::string> args{"--resume=" + std::string(*id)};
    if (fork) {
        args.push_back("--fork-session");
        if (auto at = fork_point(dom)) args.push_back("--resume-session-at=" + *at);
    }
    return args;
}

Json ClaudeControl::request(const Json& id, std::string_view method, Json params) const {
    params.set("subtype", std::string(method));
    if (auto* object = params.as_object()) std::ranges::sort(*object, {}, &std::pair<std::string, Json>::first);
    return Json(Object{{"request", std::move(params)}, {"request_id", id}, {"type", "control_request"}});
}
Json ClaudeControl::notification(std::string_view method, Json params) const {
    params.set("type", std::string(method));
    return params;
}
Json ClaudeControl::response(Json id, std::string_view, kit::RpcResult<Json> result) const {
    auto key = string(&id);
    if (result) return control_response(key, *result);
    if (auto command = result.error().get<kit::RpcError::Command>()) return control_response(key, std::unexpected(command->message));
    return control_response(key, std::unexpected(result.error().message()));
}
std::optional<kit::Incoming> ClaudeControl::classify(Json msg) const {
    if (string(field(msg, "type")) != "control_response")
        return kit::incoming::Notification{string(field(msg, "type")), std::move(msg)};
    const auto* response = field(msg, "response");
    auto id = response ? string(field(*response, "request_id")) : std::string{};
    if (response && string(field(*response, "subtype")) == "error") {
        auto error = string(field(*response, "error"));
        return kit::incoming::Response{std::move(id), std::unexpected(kit::RpcError(kit::RpcError::Command{"control", error.empty() ? "control request failed" : error, std::nullopt}))};
    }
    auto value = response ? field(*response, "response") : nullptr;
    return kit::incoming::Response{std::move(id), value ? *value : Json{}};
}

Awaitable<Availability> ClaudeAdapter::availability() const { co_return Availability::probe(executable_, {"--version"}); }
Capabilities ClaudeAdapter::capabilities() const {
    return Capabilities::builder().schema(loa::core::SCHEMA_VERSION)
        .features(std::array{Feature::Fork, Feature::Compaction, Feature::Subagents, Feature::PlanMode})
        .model_switch(ModelSwitch::PerTurn)
        .decisions(std::array{loa::core::DecisionKind::Approval, loa::core::DecisionKind::Question, loa::core::DecisionKind::Plan})
        .context(std::array{ContextDelivery::SystemPrompt}).host_tools().mcp_servers().effort().import().export_().build();
}
Awaitable<Result<SessionPtr>> ClaudeAdapter::start(SessionConfig config, std::shared_ptr<loa::core::PatchSink> sink) const {
    auto mapper = Mapper::create(sink, config);
    if (!mapper) co_return std::unexpected(mapper.error());
    co_return co_await launch(std::move(config), std::move(*mapper), {}, executable_);
}
Awaitable<Result<SessionPtr>> ClaudeAdapter::resume(SessionConfig config, const loa::core::Dom& dom, std::shared_ptr<loa::core::PatchSink> sink) const {
    auto args = resume_args(dom, false);
    if (!args) co_return std::unexpected(args.error());
    auto mapper = Mapper::attach(sink, dom);
    if (!mapper) co_return std::unexpected(mapper.error());
    co_return co_await launch(std::move(config), std::move(*mapper), *args, executable_);
}
Awaitable<Result<SessionPtr>> ClaudeAdapter::fork(SessionConfig config, const loa::core::Dom& dom, std::shared_ptr<loa::core::PatchSink> sink) const {
    auto args = resume_args(dom, true);
    if (!args) co_return std::unexpected(args.error());
    auto mapper = Mapper::attach(sink, dom);
    if (!mapper) co_return std::unexpected(mapper.error());
    co_return co_await launch(std::move(config), std::move(*mapper), *args, executable_);
}
Result<> ClaudeAdapter::import(const std::filesystem::path& path, loa::core::PatchSink& sink) const { return native::import(path, sink); }
Result<> ClaudeAdapter::export_(const loa::core::Dom& dom, const std::filesystem::path& out) const { return native::export_(dom, out); }
} // namespace loa::harness::claude
