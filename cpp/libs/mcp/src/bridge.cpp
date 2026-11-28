#include "loa/mcp/bridge.hpp"

#include <asio.hpp>
#include <asio/experimental/channel.hpp>

#include <algorithm>
#include <fstream>
#include <iostream>
#include <system_error>
#include <variant>

#include "loa/harness/omp/rpc/rpc.hpp"
#include "loa/harness/omp/rpc/typed.hpp"
#include "loa/mcp/provider.hpp"
#include "loa/mcp/toolkit.hpp"
#include "loa/runtime/executor.hpp"
#include "loa/runtime/text.hpp"

namespace loa::mcp::bridge {

namespace json = loa::runtime::json;
namespace rpc = loa::harness::omp::rpc;
namespace fs = std::filesystem;
using contracts::Awaitable;
using contracts::HostToolSpec;
using contracts::ImageBlock;
using contracts::OutputBlock;
using contracts::TextBlock;
using contracts::ToolOutput;
using contracts::ToolResult;

// ---- configuration ----

OmpBridgeConfig OmpBridgeConfig::make(fs::path cwd, fs::path state_dir) {
    OmpBridgeConfig config;
    config.cwd = std::move(cwd);
    config.state_dir = std::move(state_dir);
    for (auto tool : DEFAULT_TOOLS) config.tools.emplace_back(tool);
    return config;
}

std::expected<OmpBridgeConfig, std::string> OmpBridgeConfig::parse(std::span<const std::string> args) {
    std::optional<fs::path> state, cwd, program;
    std::optional<std::vector<std::string>> tools;
    std::vector<fs::path> overlays;
    for (std::size_t i = 0; i < args.size(); i += 2) {
        const std::string& flag = args[i];
        if (i + 1 >= args.size()) return std::unexpected(flag + " needs a value");
        const std::string& value = args[i + 1];
        if (flag == "--state-dir") state = value;
        else if (flag == "--cwd") cwd = value;
        else if (flag == "--omp") program = value;
        else if (flag == "--tools") {
            std::vector<std::string> names;
            std::size_t start = 0;
            for (;;) {
                const auto comma = value.find(',', start);
                names.push_back(value.substr(start, comma == std::string::npos ? std::string::npos : comma - start));
                if (comma == std::string::npos) break;
                start = comma + 1;
            }
            tools = std::move(names);
        } else if (flag == "--config") overlays.emplace_back(value);
        else return std::unexpected("unknown argument `" + flag + "`");
    }
    if (!cwd) {
        std::error_code ec;
        auto current = fs::current_path(ec);
        if (ec) return std::unexpected(ec.message());
        cwd = std::move(current);
    }
    if (!state) return std::unexpected(std::string("--state-dir is required"));
    auto config = make(std::move(*cwd), std::move(*state));
    if (program) config.program = std::move(*program);
    if (tools) config.tools = std::move(*tools);
    config.config_overlays = std::move(overlays);
    return config;
}

std::vector<std::string> OmpBridgeConfig::args() const {
    std::string joined;
    for (std::size_t i = 0; i < tools.size(); ++i) joined += (i ? "," : "") + tools[i];
    std::vector<std::string> out = {"--state-dir", state_dir.string(), "--cwd", cwd.string(), "--omp", program.string(), "--tools", joined};
    for (const auto& overlay : config_overlays) {
        out.emplace_back("--config");
        out.push_back(overlay.string());
    }
    return out;
}

contracts::McpServerSpec OmpBridgeConfig::session_spec(const fs::path& program, std::span<const std::string_view> prefix) const {
    contracts::McpStdio stdio{program.string(), {}, {}};
    for (auto arg : prefix) stdio.args.emplace_back(arg);
    for (auto& arg : args()) stdio.args.push_back(std::move(arg));
    for (const auto& [key, value] : env) stdio.env.emplace(key, value);
    return contracts::McpServerSpec{std::string(SERVER_NAME), std::move(stdio)};
}

std::string models_config(std::string_view base_url) {
    json::Value model = json::Object{
        {"id", MODEL_ID},
        {"name", "omp tools bridge (no model)"},
        {"reasoning", false},
        {"input", json::Array{"text"}},
        {"cost", json::Object{{"input", 0}, {"output", 0}, {"cacheRead", 0}, {"cacheWrite", 0}}},
        {"contextWindow", 1'000'000},
        {"maxTokens", 4096},
    };
    json::Value provider = json::Object{
        {"baseUrl", base_url}, {"apiKey", TOKEN_ENV}, {"api", "openai-completions"}, {"models", json::Array{std::move(model)}}};
    return json::dump(json::Object{{"providers", json::Object{{std::string(PROVIDER_ID), std::move(provider)}}}});
}

std::string bridge_config() {
    return json::dump(json::Object{{"astGrep", json::Object{{"enabled", true}}}, {"tools", json::Object{{"xdev", false}}}});
}

// ---- omp frames ----

Result<std::vector<HostToolSpec>> select_tools(const json::Array& dumped, std::span<const std::string> wanted) {
    std::vector<HostToolSpec> out;
    for (const auto& name : wanted) {
        auto found = std::ranges::find_if(dumped, [&](const json::Value& tool) {
            const auto* n = tool.find("name");
            return n && n->as_string() && *n->as_string() == name;
        });
        if (found == dumped.end()) return std::unexpected(Error::bridge("unknown tool `" + name + "`"));
        const auto* schema = found->find("parameters");
        if (!schema || !schema->is_object()) return std::unexpected(Error::bridge("omp reported no schema for `" + name + "`"));
        const auto* description = found->find("description");
        out.push_back(HostToolSpec{name, description && description->as_string() ? *description->as_string() : std::string(),
                                   json::dump(*schema)});
    }
    return out;
}

ToolOutput tool_output(const json::Value* result, bool is_error) {
    ToolOutput out;
    out.is_error = is_error;
    const auto* content = result ? result->find("content") : nullptr;
    if (!content || !content->as_array()) return out;
    for (const auto& block : *content->as_array()) {
        const auto* type = block.find("type");
        if (type && type->as_string() && *type->as_string() == "image") {
            const auto* data = block.find("data");
            const auto* mime = block.find("mimeType");
            if (data && data->as_string() && mime && mime->as_string()) {
                if (auto decoded = image(*data->as_string(), *mime->as_string())) {
                    out.blocks.push_back(std::move(*decoded));
                    continue;
                }
            }
        }
        const auto* text = block.find("text");
        out.blocks.emplace_back(TextBlock{text && text->as_string() ? *text->as_string() : json::dump(block)});
    }
    return out;
}

// ---- the executor ----

namespace {

using Strand = asio::strand<asio::any_io_executor>;

// What a call waits on: omp's frames, or the response to its prompt (by command id).
struct Ack {
    std::string id;
    rpc::RpcResult<rpc::PromptAck> result;
};
using Wake = std::variant<std::optional<rpc::Incoming>, Ack>;
using Inbox = asio::experimental::channel<void(asio::error_code, Wake)>;

// Forwards every omp frame into the inbox until the stream ends.
Awaitable<void> pump(std::shared_ptr<rpc::Events> events, std::shared_ptr<Inbox> inbox) {
    for (;;) {
        auto frame = co_await events->recv();
        const bool end = !frame;
        auto [ec] = co_await inbox->async_send(asio::error_code{}, Wake(std::move(frame)), asio::as_tuple(asio::use_awaitable));
        if (ec || end) co_return;
    }
}

std::string status_text(rpc::PromptStatus status) {
    switch (status) {
    case rpc::PromptStatus::Completed: return "completed";
    case rpc::PromptStatus::Aborted: return "aborted";
    case rpc::PromptStatus::Error: return "error";
    }
    return "completed";
}

std::size_t output_bytes(const ToolOutput& output) {
    std::size_t total = 0;
    for (const auto& block : output.blocks) {
        if (const auto* text = std::get_if<TextBlock>(&block)) total += text->text.size();
        else total += std::get<ImageBlock>(block).data.size();
    }
    return total;
}

Result<void> write_file(const fs::path& path, const std::string& text) {
    std::ofstream out(path, std::ios::binary | std::ios::trunc);
    out << text;
    out.close();
    if (!out) return std::unexpected(Error::bridge("cannot write " + path.string()));
    return {};
}

struct Launched {
    rpc::RpcClient rpc;
    rpc::Events events;
    std::unique_ptr<ScriptedProvider> provider;
    json::Array dumped;
};

// Starts omp against the loopback provider and returns `get_state`'s `dumpTools`. `tools`
// limits omp to those tools.
Awaitable<Result<Launched>> launch(OmpBridgeConfig config, std::optional<std::vector<std::string>> tools) {
    const std::string token = runtime::text::random_hex(16);
    auto provider = ScriptedProvider::start(token);
    if (!provider) co_return std::unexpected(Error::bridge(provider.error()));
    const fs::path agent_dir = config.state_dir / "agent";
    std::error_code ec;
    fs::create_directories(agent_dir, ec);
    if (ec) co_return std::unexpected(Error::bridge(ec.message()));
    // JSON is valid YAML, so the JSON writer writes omp's YAML config files.
    if (auto written = write_file(agent_dir / "models.yml", models_config((*provider)->base_url())); !written)
        co_return std::unexpected(written.error());
    const fs::path overlay = config.state_dir / "bridge-config.yml";
    if (auto written = write_file(overlay, bridge_config()); !written) co_return std::unexpected(written.error());

    rpc::SpawnOptions options(config.program);
    options.no_ui = true;
    options.args = {"--no-session", "--no-extensions", "--no-skills", "--no-rules", "--no-context-files", "--no-title", "--offline",
                    "--auto-approve"};
    options.args.insert(options.args.end(),
                        {"--model", std::string(PROVIDER_ID) + "/" + std::string(MODEL_ID), "--config", overlay.string()});
    for (const auto& extra : config.config_overlays) {
        options.args.emplace_back("--config");
        options.args.push_back(extra.string());
    }
    if (tools) {
        std::string joined;
        for (std::size_t i = 0; i < tools->size(); ++i) joined += (i ? "," : "") + (*tools)[i];
        options.args.emplace_back("--tools");
        options.args.push_back(std::move(joined));
    }
    options.cwd = config.cwd;
    options.env = {{"PI_CODING_AGENT_DIR", agent_dir.string()}, {std::string(TOKEN_ENV), token}};
    options.env.insert(options.env.end(), config.env.begin(), config.env.end());
    for (auto name : INHERITED_SESSION_ENV) options.env_remove.emplace_back(name);
    auto spawned = co_await rpc::RpcClient::spawn(std::move(options));
    if (!spawned) co_return std::unexpected(Error::bridge(spawned.error().message()));
    auto state = co_await spawned->client.request<rpc::SessionState>(rpc::command::GetState{});
    if (!state) co_return std::unexpected(Error::bridge(state.error().message()));
    auto dump = std::ranges::find_if(state->extra, [](const auto& entry) { return entry.first == "dumpTools"; });
    if (dump == state->extra.end() || !dump->second.as_array()) co_return std::unexpected(Error::bridge("get_state has no dumpTools"));
    json::Array dumped = std::move(*dump->second.as_array());
    co_return Launched{std::move(spawned->client), std::move(spawned->events), std::move(*provider), std::move(dumped)};
}

} // namespace

struct OmpExecutor::Impl {
    Strand strand;
    rpc::RpcClient rpc;
    std::shared_ptr<Inbox> inbox;
    // One call at a time: a slot in this channel is the lock.
    asio::experimental::channel<void(asio::error_code)> lock;
    std::unique_ptr<ScriptedProvider> provider;
    std::uint64_t calls = 0;
    std::size_t history_bytes = 0;

    Impl(Strand strand, rpc::RpcClient rpc, std::unique_ptr<ScriptedProvider> provider)
        : strand(strand), rpc(std::move(rpc)), inbox(std::make_shared<Inbox>(strand, 1024)), lock(strand, 1),
          provider(std::move(provider)) {}

    ~Impl() {
        rpc.kill();
        asio::post(strand, [inbox = inbox] { inbox->close(); });
    }

    // Runs one tool through a prompt the loopback provider answers with that call. Runs on `strand`.
    Awaitable<ToolResult> run(std::string name, json::Object arguments);
};

Awaitable<ToolResult> OmpExecutor::Impl::run(std::string name, json::Object arguments) {
    co_await asio::this_coro::throw_if_cancelled(false);
    auto [locked] = co_await lock.async_send(asio::error_code{}, asio::as_tuple(asio::use_awaitable));
    if (locked) co_return std::unexpected(std::string("tool call cancelled"));
    struct Unlock {
        asio::experimental::channel<void(asio::error_code)>& lock;
        ~Unlock() { lock.try_receive([](asio::error_code) {}); }
    } unlock{lock};

    if (history_bytes > HISTORY_BUDGET_BYTES) {
        auto fresh = co_await rpc.request(rpc::command::NewSession{});
        if (!fresh) co_return std::unexpected(fresh.error().message());
        history_bytes = 0;
    }
    ++calls;
    const std::string call_id = "loa-omp-bridge-call-" + std::to_string(calls);
    const std::string id = rpc.reserve_id();
    // Spawned so the prompt is sent even if this call is cancelled.
    asio::co_spawn(
        strand,
        [client = rpc, inbox = inbox, id, message = call_prompt(call_id, name, std::move(arguments))]() -> Awaitable<void> {
            auto result = co_await client.request_as<rpc::PromptAck>(id, rpc::command::Prompt{message, {}, std::nullopt});
            co_await inbox->async_send(asio::error_code{}, Wake(Ack{id, std::move(result)}), asio::as_tuple(asio::use_awaitable));
        },
        asio::detached);

    bool acked = false;
    bool aborted = false;
    std::optional<ToolOutput> output;
    std::optional<std::string> rejection;
    auto not_run = [&](const std::string& reason) { return "omp ended the turn without running `" + name + "`: " + reason; };
    for (;;) {
        auto cancelled = (co_await asio::this_coro::cancellation_state).cancelled();
        if (!aborted && cancelled != asio::cancellation_type::none) {
            aborted = true;
            co_await asio::this_coro::reset_cancellation_state();
            co_await asio::this_coro::throw_if_cancelled(false);
            auto stopped = co_await rpc.request(rpc::command::Abort{});
            if (!stopped) co_return std::unexpected(stopped.error().message());
        }
        auto [ec, wake] = co_await inbox->async_receive(asio::as_tuple(asio::use_awaitable));
        if (ec == asio::error::operation_aborted) continue;
        if (ec) co_return std::unexpected(std::string("omp exited"));
        if (auto* ack = std::get_if<Ack>(&wake)) {
            if (ack->id != id || acked) continue;
            acked = true;
            if (!ack->result) rejection = ack->result.error().message();
            else if (ack->result->agent_invoked == false) {
                if (output) co_return std::move(*output);
                co_return std::unexpected(not_run("omp handled the prompt without an agent turn"));
            }
            continue;
        }
        auto& incoming = std::get<std::optional<rpc::Incoming>>(wake);
        if (!incoming) co_return std::unexpected(std::string("omp exited"));
        if (auto* exited = std::get_if<rpc::incoming::Exited>(&*incoming)) co_return std::unexpected("omp exited: " + exited->stderr);
        auto* event = std::get_if<rpc::incoming::Event>(&*incoming);
        if (!event) continue;
        if (auto* end = std::get_if<rpc::event::ToolExecutionEnd>(&event->event); end && end->tool_call_id == call_id) {
            auto out = tool_output(end->result ? &*end->result : nullptr, end->is_error);
            history_bytes += output_bytes(out);
            output = std::move(out);
        } else if (auto* done = std::get_if<rpc::event::PromptResult>(&event->event); done && done->id == id) {
            if (aborted) co_return std::unexpected(std::string("tool call cancelled"));
            const std::string status = "turn " + status_text(done->status);
            const std::string reason = done->error ? status + ": " + done->error->message : status;
            if (output) co_return std::move(*output);
            co_return std::unexpected(rejection ? *rejection : not_run(reason));
        }
    }
}

namespace {

struct OmpTool final : contracts::DynTool {
    HostToolSpec spec_;
    std::shared_ptr<OmpExecutor> executor;

    OmpTool(HostToolSpec spec, std::shared_ptr<OmpExecutor> executor) : spec_(std::move(spec)), executor(std::move(executor)) {}

    [[nodiscard]] HostToolSpec spec() const override { return spec_; }
    [[nodiscard]] contracts::ToolAnnotations annotations() const override {
        contracts::ToolAnnotations hints;
        hints.destructive = std::ranges::find(MUTATING_TOOLS, spec_.name) != MUTATING_TOOLS.end();
        return hints;
    }
    [[nodiscard]] Awaitable<ToolResult> call(const contracts::CallCtx&, json::Value args) const override {
        if (!args.as_object()) return contracts::ready(std::unexpected(std::string("arguments must be a JSON object")));
        return executor->call(spec_.name, std::move(*args.as_object()));
    }
};

} // namespace

OmpExecutor::OmpExecutor(std::unique_ptr<Impl> impl, std::vector<HostToolSpec> tools) : impl_(std::move(impl)), tools_(std::move(tools)) {}
OmpExecutor::~OmpExecutor() = default;

Awaitable<Result<std::shared_ptr<OmpExecutor>>> OmpExecutor::start(OmpBridgeConfig config) {
    Strand strand = asio::make_strand(co_await asio::this_coro::executor);
    auto tools = config.tools;
    auto launched = co_await asio::co_spawn(strand, launch(std::move(config), tools), asio::use_awaitable);
    if (!launched) co_return std::unexpected(std::move(launched).error());
    auto selected = select_tools(launched->dumped, tools);
    if (!selected) co_return std::unexpected(std::move(selected).error());
    auto impl = std::make_unique<Impl>(strand, std::move(launched->rpc), std::move(launched->provider));
    asio::co_spawn(strand, pump(std::make_shared<rpc::Events>(std::move(launched->events)), impl->inbox), asio::detached);
    co_return std::shared_ptr<OmpExecutor>(new OmpExecutor(std::move(impl), std::move(*selected)));
}

contracts::ToolSet OmpExecutor::tool_set() {
    contracts::ToolSet set;
    for (const auto& spec : tools_) set.add(std::make_shared<OmpTool>(spec, shared_from_this()));
    return set;
}

Awaitable<ToolResult> OmpExecutor::call(std::string name, json::Object arguments) {
    // Keeps the executor alive for the call; runs on the session strand, where its channels live.
    auto self = shared_from_this();
    co_return co_await asio::co_spawn(self->impl_->strand, self->impl_->run(std::move(name), std::move(arguments)), asio::use_awaitable);
}

Awaitable<Result<std::vector<HostToolSpec>>> list_omp_tools(OmpBridgeConfig config) {
    Strand strand = asio::make_strand(co_await asio::this_coro::executor);
    auto launched = co_await asio::co_spawn(strand, launch(std::move(config), std::nullopt), asio::use_awaitable);
    if (!launched) co_return std::unexpected(std::move(launched).error());
    std::vector<std::string> names;
    for (const auto& tool : launched->dumped) {
        const auto* name = tool.find("name");
        if (name && name->as_string()) names.push_back(*name->as_string());
    }
    launched->rpc.kill();
    co_return select_tools(launched->dumped, names);
}

int run_cli(std::span<const std::string> args) {
    auto config = OmpBridgeConfig::parse(args);
    if (!config) {
        std::cerr << config.error() << "\n" << USAGE << "\n";
        return 2;
    }
    runtime::Executor exec(2);
    auto executor = asio::co_spawn(exec.context(), OmpExecutor::start(std::move(*config)), asio::use_future).get();
    Result<void> served = executor ? ToolSetMcp((*executor)->tool_set(), SERVER_NAME).with_instructions(INSTRUCTIONS).serve_stdio(exec)
                                   : Result<void>(std::unexpected(executor.error()));
    if (!served) {
        std::cerr << "loa omp-bridge: " << served.error().to_string() << "\n";
        return 1;
    }
    return 0;
}

} // namespace loa::mcp::bridge
