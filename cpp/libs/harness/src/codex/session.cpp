#include "loa/harness/codex/session.hpp"

#include <asio.hpp>

#include <chrono>
#include <expected>
#include <iterator>
#include <map>
#include <utility>

#include "loa/harness/codex/adapter.hpp"
#include "loa/harness/codex/mapper.hpp"
#include "loa/runtime/exec.hpp"
#include "loa/runtime/log.hpp"
#include "loa/runtime/text.hpp"

namespace loa::harness::codex {
namespace {
namespace json = loa::runtime::json;
using loa::contracts::AdapterError;
using loa::contracts::Result;
using loa::contracts::SessionState;
using loa::runtime::json::Array;
using loa::runtime::json::Object;
using loa::runtime::json::Value;

const kit::JsonRpc2 FRAMES(false);

const Value* member(const Value& value, std::string_view key) { return value.find(key); }
const Value* nested(const Value& value, std::string_view a, std::string_view b) {
    const auto* parent = member(value, a);
    return parent ? member(*parent, b) : nullptr;
}
std::optional<std::string_view> string_at(const Value& value, std::string_view key) {
    const auto* item = member(value, key);
    if (item) if (const auto* s = item->as_string()) return *s;
    return std::nullopt;
}
std::string toml_table(const std::map<std::string, std::string>& entries) {
    std::string result = "{";
    for (const auto& [key, value] : entries) {
        if (result.size() > 1) result += ',';
        result += json::dump(Value(key)) + '=' + json::dump(Value(value));
    }
    return result + '}';
}
Value tool_result(std::expected<std::string, std::string> result) {
    const bool success = result.has_value();
    std::string text = success ? std::move(*result) : std::move(result.error());
    return Object{{"contentItems", Array{Object{{"type", "inputText"}, {"text", std::move(text)}}}}, {"success", success}};
}
loa::contracts::Awaitable<void> run_host_tool(std::shared_ptr<const loa::contracts::HostTools> handler,
                                               kit::Writer writer, std::string session, Reply reply) {
    auto outcome = co_await handler->call(session, reply.tool, reply.arguments);
    auto sent = writer.send(FRAMES.response(std::move(reply.id), "", tool_result(std::move(outcome))));
    if (!sent) loa::runtime::log::warn("codex", "couldn't return an app tool result: " + sent.error().message());
}
Value turn_input(const loa::contracts::Prompt& prompt) {
    auto text = [](std::string value) -> Value {
        return Object{{"type", "text"}, {"text", std::move(value)}, {"text_elements", Array{}}};
    };
    Array input;
    input.reserve(prompt.attachments.size() + 1);
    input.push_back(text(prompt.text));
    for (const auto& attachment : prompt.attachments) {
        const auto path = attachment.path.string();
        const auto mime = attachment.mime ? std::optional<std::string_view>(*attachment.mime) : loa::runtime::text::mime_of(attachment.path);
        if (mime && mime->starts_with("image/")) input.emplace_back(Object{{"type", "localImage"}, {"path", path}});
        else input.push_back(text("Attached file: " + path));
    }
    return input;
}
std::optional<loa::contracts::HarnessModel> listed_model(const Value& entry) {
    if (const auto* hidden = member(entry, "hidden"); hidden && hidden->as_bool() && *hidden->as_bool()) return std::nullopt;
    auto id = string_at(entry, "model");
    if (!id) id = string_at(entry, "id");
    if (!id) return std::nullopt;
    loa::contracts::HarnessModel model{std::string(*id), std::string(string_at(entry, "displayName").value_or(*id)), "openai", {}};
    if (const auto* levels = member(entry, "supportedReasoningEfforts"); levels && levels->as_array()) {
        for (const auto& level : *levels->as_array()) {
            const auto* effort = member(level, "reasoningEffort");
            if (const auto* name = (effort ? effort : &level)->as_string()) model.efforts.push_back(*name);
        }
    }
    return model;
}
} // namespace

std::vector<std::string> mcp_overrides(const loa::contracts::SessionConfig& config) {
    std::vector<std::string> out;
    for (const auto& server : config.mcp_servers) {
        std::string name = server.name;
        for (char& c : name) {
            if (!((c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') || (c >= '0' && c <= '9') || c == '_' || c == '-')) c = '_';
        }
        const std::string key = "mcp_servers." + name;
        auto add = [&](std::string value) { out.emplace_back("-c"); out.emplace_back(std::move(value)); };
        if (const auto* stdio = std::get_if<loa::contracts::McpStdio>(&server.transport)) {
            add(key + ".command=" + json::dump(Value(stdio->command)));
            Array args;
            args.reserve(stdio->args.size());
            for (const auto& arg : stdio->args) args.emplace_back(arg);
            add(key + ".args=" + json::dump(Value(std::move(args))));
            if (!stdio->env.empty()) add(key + ".env=" + toml_table(stdio->env));
        } else if (const auto* http = std::get_if<loa::contracts::McpHttp>(&server.transport)) {
            add(key + ".url=" + json::dump(Value(http->url)));
            if (!http->headers.empty()) add(key + ".http_headers=" + toml_table(http->headers));
        }
    }
    return out;
}

std::pair<std::string_view, Value> open_request(const loa::contracts::SessionConfig& config, const Open& open) {
    Value params = Object{{"cwd", config.cwd.string()}};
    if (config.model) params.set("model", *config.model);
    if (config.context) params.set("developerInstructions", *config.context);
    if (open.kind == Open::Kind::Start) {
        params.set("approvalPolicy", "on-request");
        params.set("sandbox", "workspace-write");
        if (!config.host_tools.empty()) {
            Array tools;
            tools.reserve(config.host_tools.size());
            for (const auto& tool : config.host_tools) {
                auto schema = json::parse(tool.input_schema);
                tools.emplace_back(Object{{"type", "function"}, {"name", tool.name}, {"description", tool.description},
                                          {"inputSchema", schema ? std::move(*schema) : Value(Object{{"type", "object"}})}});
            }
            params.set("dynamicTools", std::move(tools));
        }
        return {"thread/start", std::move(params)};
    }
    params.set("threadId", open.thread_id);
    params.set("excludeTurns", true);
    if (open.kind == Open::Kind::Fork && open.last_turn_id) params.set("lastTurnId", *open.last_turn_id);
    return {open.kind == Open::Kind::Resume ? "thread/resume" : "thread/fork", std::move(params)};
}

Value turn_params(const loa::contracts::Prompt& prompt, std::string_view thread_id, std::optional<std::string_view> model,
                  std::optional<std::string_view> effort, std::optional<loa::core::SessionMode> mode) {
    Value params = Object{{"threadId", Value(thread_id)}, {"input", turn_input(prompt)}};
    if (model) params.set("model", Value(*model));
    if (effort) params.set("effort", Value(*effort));
    if (mode && model) {
        params.set("collaborationMode", Object{{"mode", *mode == loa::core::SessionMode::Plan ? "plan" : "default"},
                                                 {"settings", Object{{"model", Value(*model)},
                                                                     {"reasoning_effort", effort ? Value(*effort) : Value()},
                                                                     {"developer_instructions", Value()}}}});
    }
    return params;
}

struct CodexSession::Shared final : kit::Inbound {
    mutable std::mutex mapper_mutex;
    mutable std::optional<Mapper> mapper;
    std::shared_ptr<loa::core::PatchSink> sink;
    mutable std::mutex writer_mutex;
    std::optional<kit::Writer> writer;
    mutable std::vector<Value> pending_replies;
    kit::StateWatch state{SessionState::Idle};
    std::string session_id;
    std::shared_ptr<const loa::contracts::HostTools> host_tools;

    void install_writer(kit::Writer w) {
        std::scoped_lock lock(writer_mutex);
        writer.emplace(std::move(w));
        for (const auto& frame : pending_replies) {
            if (auto sent = writer->send(frame); !sent)
                loa::runtime::log::warn("codex", "couldn't answer an early request: " + sent.error().message());
        }
        pending_replies.clear();
    }

    template <typename T, typename F>
    Result<T> mapped(F&& f) const {
        std::scoped_lock lock(mapper_mutex);
        if (!mapper) return std::unexpected(AdapterError::Closed{});
        const loa::core::DomWriter w(*sink);
        auto result = f(*mapper, w);
        if (state.get() != SessionState::Closed) state.set(mapper->state());
        return result;
    }
    Result<> reply(Reply reply) const {
        // Frames arriving as the process starts may precede writer installation.
        if (reply.kind == Reply::Kind::HostTool) {
            return std::unexpected(AdapterError::Protocol{"host tool reply requires async handling"});
        }
        kit::RpcResult<Value> result = reply.kind == Reply::Kind::Result
            ? kit::RpcResult<Value>(std::move(reply.result))
            : kit::RpcResult<Value>(std::unexpected(kit::RpcError(kit::RpcError::Command{"", std::move(reply.message),
                                                                                         std::to_string(reply.code)})));
        Value frame = FRAMES.response(std::move(reply.id), "", std::move(result));
        std::scoped_lock lock(writer_mutex);
        if (!writer) {
            pending_replies.push_back(std::move(frame));
            return {};
        }
        auto sent = writer->send(frame);
        if (!sent) return std::unexpected(sent.error().to_adapter_error());
        return {};
    }
    loa::contracts::Awaitable<void> dispatch(Reply reply) const {
        if (reply.kind != Reply::Kind::HostTool) {
            if (auto result = this->reply(std::move(reply)); !result) loa::runtime::log::warn("codex", result.error().message());
            co_return;
        }
        std::optional<kit::Writer> w;
        {
            std::scoped_lock lock(writer_mutex);
            w = writer;
        }
        if (!w) co_return;
        if (!host_tools) {
            reply.kind = Reply::Kind::Result;
            reply.result = tool_result(std::unexpected("no handler for app tool `" + reply.tool + "`"));
            if (auto result = this->reply(std::move(reply)); !result) loa::runtime::log::warn("codex", result.error().message());
            co_return;
        }
        // The host tool can be slow: the read loop must remain available for later frames.
        auto ex = co_await asio::this_coro::executor;
        asio::co_spawn(ex, run_host_tool(host_tools, *w, session_id, std::move(reply)), asio::detached);
    }
    loa::contracts::Awaitable<kit::RpcResult<Value>> on_request(std::string method, Value) const override {
        co_return std::unexpected(kit::RpcError(kit::RpcError::Protocol{method + " isn't served on its own task"}));
    }
    bool defers_requests() const override { return true; }
    loa::contracts::Awaitable<void> on_deferred(Value id, std::string method, Value params) const override {
        auto reply_value = mapped<std::optional<Reply>>([&](Mapper& m, const loa::core::DomWriter& w) {
            return m.handle(id, method, params, w);
        });
        if (!reply_value) {
            const bool closed = reply_value.error().get<AdapterError::Closed>();
            Reply error{};
            error.kind = Reply::Kind::Error;
            error.id = std::move(id);
            error.code = closed ? -32601 : -32602;
            error.message = closed ? "not ready" : reply_value.error().message();
            co_await dispatch(std::move(error));
        } else if (*reply_value) co_await dispatch(std::move(**reply_value));
    }
    loa::contracts::Awaitable<void> on_notification(std::string method, Value params) const override {
        auto result = mapped<std::optional<Reply>>([&](Mapper& m, const loa::core::DomWriter& w) {
            return m.handle(std::nullopt, method, params, w);
        });
        if (!result && !result.error().get<AdapterError::Closed>())
            loa::runtime::log::warn("codex", "couldn't record an event: " + result.error().message());
        co_return;
    }
    loa::contracts::Awaitable<void> on_closed(std::optional<loa::runtime::exec::ExitStatus>, std::string stderr) const override {
        if (!stderr.empty()) loa::runtime::log::debug("codex", stderr);
        auto result = mapped<void>([](Mapper& m, const loa::core::DomWriter& w) { return m.process_exited("codex exited", w); });
        if (!result && !result.error().get<AdapterError::Closed>())
            loa::runtime::log::warn("codex", "couldn't record the exit: " + result.error().message());
        state.set(SessionState::Closed);
        co_return;
    }
};

CodexSession::CodexSession(std::shared_ptr<Shared> shared, kit::StdioRpc<kit::JsonRpc2> rpc,
                           std::string thread_id, TurnSettings settings)
    : shared_(std::move(shared)), rpc_(std::move(rpc)), thread_id_(std::move(thread_id)), settings_(std::move(settings)) {}

loa::contracts::Awaitable<Result<loa::contracts::SessionPtr>> CodexSession::open(
    const CodexAdapter& adapter, loa::contracts::SessionConfig config, std::shared_ptr<loa::core::PatchSink> sink,
    Open open, Attach attach) {
    if (!config.host_tools.empty() && !config.host_tool_handler)
        co_return std::unexpected(AdapterError::Protocol{"host tools need a host tool handler"});
    auto args = adapter.args();
    auto overrides = mcp_overrides(config);
    args.insert(args.end(), std::make_move_iterator(overrides.begin()), std::make_move_iterator(overrides.end()));
    args.emplace_back("app-server");
    args.emplace_back("--listen");
    args.emplace_back("stdio://");
    std::vector<std::pair<std::string, std::string>> env(config.env.begin(), config.env.end());
    auto command = loa::runtime::exec::command(config.executable.value_or(adapter.executable()), std::move(args),
                                                std::move(env), config.cwd);
    auto shared = std::make_shared<Shared>();
    shared->sink = std::move(sink);
    shared->session_id = config.session_id;
    shared->host_tools = config.host_tool_handler;
    auto executor = co_await asio::this_coro::executor;
    auto rpc = kit::StdioRpc<kit::JsonRpc2>::spawn(executor, command, kit::JsonRpc2(false), shared);
    if (!rpc) co_return std::unexpected(rpc.error().to_adapter_error());
    shared->install_writer(rpc->writer());
    CodexSession session(shared, std::move(*rpc), {}, {});
    Value client = Object{{"clientInfo", Object{{"name", "loa"}, {"title", "loa"},
                                                 {"version", std::string(loa::contracts::INTERFACE_VERSION)}}},
                          {"capabilities", Object{{"experimentalApi", true}}}};
    auto initialized = co_await session.request("initialize", std::move(client));
    if (!initialized) co_return std::unexpected(std::move(initialized).error());
    if (auto notified = session.rpc_.notify("initialized", Value()); !notified)
        co_return std::unexpected(notified.error().to_adapter_error());
    auto [method, params] = open_request(config, open);
    auto opened = co_await session.request(std::string(method), std::move(params));
    if (!opened) co_return std::unexpected(std::move(opened).error());
    const auto* thread = nested(*opened, "thread", "id");
    const auto* thread_id = thread ? thread->as_string() : nullptr;
    if (!thread_id) co_return std::unexpected(AdapterError::Protocol{std::string(method) + " returned no thread id"});
    session.thread_id_ = *thread_id;
    loa::core::DomWriter writer(*shared->sink);
    auto mapper = attach(session.thread_id_, string_at(*opened, "model"), writer);
    if (!mapper) co_return std::unexpected(std::move(mapper).error());
    if (auto model = mapper->model()) session.settings_.model = std::string(*model);
    session.settings_.effort = std::move(config.effort);
    session.settings_.mode = config.mode;
    {
        std::scoped_lock lock(shared->mapper_mutex);
        shared->state.set(mapper->state());
        shared->mapper.emplace(std::move(*mapper));
    }
    co_return loa::contracts::SessionPtr(new CodexSession(std::move(shared), std::move(session.rpc_),
                                                          std::move(session.thread_id_), std::move(session.settings_)));
}

loa::contracts::Awaitable<Result<Value>> CodexSession::request(std::string method, Value params) const {
    auto result = co_await rpc_.request_as(Value(rpc_.next_id()), method, std::move(params));
    if (!result) {
        if (const auto* cmd = result.error().get<kit::RpcError::Command>())
            co_return std::unexpected(AdapterError::Protocol{method + ": " + cmd->message});
        co_return std::unexpected(result.error().to_adapter_error());
    }
    co_return std::move(*result);
}

loa::contracts::Awaitable<Result<>> CodexSession::send(loa::contracts::Prompt prompt) const {
    if (state() == SessionState::Closed) co_return std::unexpected(AdapterError::Closed{});
    auto begin = shared_->mapped<void>([&](Mapper& mapper, const loa::core::DomWriter& writer) {
        return mapper.begin_turn(prompt, writer);
    });
    if (!begin) co_return std::unexpected(std::move(begin).error());
    Value params;
    {
        std::scoped_lock lock(settings_mutex_);
        params = turn_params(prompt, thread_id_, settings_.model, settings_.effort, settings_.mode);
    }
    auto outcome = co_await request("turn/start", std::move(params));
    auto recorded = shared_->mapped<void>([&](Mapper& mapper, const loa::core::DomWriter& writer) -> Result<> {
        if (!outcome) return mapper.turn_rejected(outcome.error().message(), writer);
        const auto* turn = nested(*outcome, "turn", "id");
        if (const auto* id = turn ? turn->as_string() : nullptr) return mapper.turn_accepted(*id, writer);
        return {};
    });
    if (!recorded) co_return std::unexpected(std::move(recorded).error());
    if (!outcome) co_return std::unexpected(std::move(outcome).error());
    co_return Result<>{};
}
loa::contracts::Awaitable<Result<>> CodexSession::interrupt() const {
    auto turn = shared_->mapped<std::optional<std::string>>([](Mapper& mapper, const loa::core::DomWriter&) -> Result<std::optional<std::string>> {
        const auto id = mapper.turn_id();
        return id ? std::optional<std::string>(*id) : std::nullopt;
    });
    if (!turn) co_return std::unexpected(std::move(turn).error());
    if (!*turn) co_return Result<>{};
    auto result = co_await request("turn/interrupt", Object{{"threadId", thread_id_}, {"turnId", **turn}});
    if (!result) co_return std::unexpected(std::move(result).error());
    co_return Result<>{};
}
loa::contracts::Awaitable<Result<>> CodexSession::answer(loa::core::NodeId decision, loa::contracts::Answer answer) const {
    auto reply = shared_->mapped<std::optional<Reply>>([&](Mapper& mapper, const loa::core::DomWriter& writer) {
        return mapper.answer(decision, answer, writer);
    });
    if (!reply) co_return std::unexpected(std::move(reply).error());
    if (*reply) co_await shared_->dispatch(std::move(**reply));
    co_return Result<>{};
}
loa::contracts::Awaitable<Result<>> CodexSession::set_model(loa::contracts::ModelSelection selection) const {
    if (selection.role) co_return std::unexpected(AdapterError::Unsupported{HARNESS, "model roles"});
    {
        std::scoped_lock lock(settings_mutex_);
        settings_.model = selection.model;
    }
    co_return shared_->mapped<void>([&](Mapper& mapper, const loa::core::DomWriter& w) {
        return mapper.set_model(selection.model, w);
    });
}
loa::contracts::Awaitable<Result<>> CodexSession::set_mode(loa::core::SessionMode mode) const {
    {
        std::scoped_lock lock(settings_mutex_);
        settings_.mode = mode;
    }
    co_return shared_->mapped<void>([&](Mapper& mapper, const loa::core::DomWriter& w) { return mapper.set_mode(mode, w); });
}
loa::contracts::Awaitable<Result<std::vector<loa::contracts::HarnessModel>>> CodexSession::models() const {
    std::vector<loa::contracts::HarnessModel> models;
    std::optional<std::string> cursor;
    for (;;) {
        auto page = co_await request("model/list", cursor ? Value(Object{{"cursor", *cursor}}) : Value(Object{}));
        if (!page) co_return std::unexpected(std::move(page).error());
        const auto* data = member(*page, "data");
        if (!data || !data->as_array()) co_return std::unexpected(AdapterError::Protocol{"model/list returned no data"});
        for (const auto& entry : *data->as_array()) if (auto model = listed_model(entry)) models.push_back(std::move(*model));
        auto next = string_at(*page, "nextCursor");
        if (!next || (cursor && *cursor == *next)) co_return models;
        cursor = std::string(*next);
    }
}
loa::contracts::Awaitable<Result<>> CodexSession::set_effort(std::string effort) const {
    {
        std::scoped_lock lock(settings_mutex_);
        settings_.effort = effort;
    }
    co_return shared_->mapped<void>([&](Mapper& mapper, const loa::core::DomWriter& w) { return mapper.set_effort(effort, w); });
}
loa::contracts::Awaitable<Result<>> CodexSession::compact(std::optional<std::string> instructions) const {
    if (instructions) co_return std::unexpected(AdapterError::Unsupported{HARNESS, "compaction instructions"});
    auto result = co_await request("thread/compact/start", Object{{"threadId", thread_id_}});
    if (!result) co_return std::unexpected(std::move(result).error());
    co_return Result<>{};
}
std::string_view CodexSession::harness() const { return HARNESS; }
SessionState CodexSession::state() const { return shared_->state.get(); }
loa::contracts::Awaitable<Result<>> CodexSession::wait_idle() const {
    co_await shared_->state.wait_settled();
    co_return Result<>{};
}
loa::contracts::Awaitable<Result<>> CodexSession::shutdown() const {
    (void)co_await rpc_.shutdown(std::chrono::seconds(5));
    shared_->state.set(SessionState::Closed);
    co_return Result<>{};
}
} // namespace loa::harness::codex
