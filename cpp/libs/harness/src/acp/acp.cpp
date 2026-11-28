#include "loa/harness/acp/acp.hpp"

#include <asio.hpp>
#include <asio/experimental/awaitable_operators.hpp>

#include <algorithm>
#include <atomic>
#include <format>
#include <utility>

#include "../kit/sync.hpp"
#include "loa/core/writer.hpp"
#include "loa/harness/kit/jsonrpc.hpp"
#include "loa/harness/kit/ledger.hpp"
#include "loa/harness/kit/rpc.hpp"
#include "loa/harness/registry/registry.hpp"
#include "loa/runtime/exec.hpp"
#include "loa/runtime/log.hpp"

namespace loa::harness::acp {

namespace {

using namespace loa::contracts;
using namespace loa::core;
using kit::RpcError;
using kit::RpcResult;
using loa::runtime::json::Array;
using loa::runtime::json::Object;
namespace json = loa::runtime::json;

// `clientInfo.version`: the workspace version of the Rust app.
constexpr std::string_view CLIENT_VERSION = "0.1.0";
constexpr std::string_view LOG = "loa::harness::acp";

AdapterError commit_error(CommitError e) { return AdapterError(AdapterError::Commit{std::move(e)}); }

template <typename T>
Result<T> committed(std::expected<T, CommitError> r) {
    if (!r) return std::unexpected(commit_error(std::move(r).error()));
    if constexpr (std::is_void_v<T>) return {};
    else return std::move(*r);
}

AdapterError malformed(const std::string& e) { return AdapterError(AdapterError::Protocol{"malformed response: " + e}); }

struct Modes {
    std::vector<std::string> available;
    std::optional<std::string> initial;
};

// The inbound port and the state the session shares with it.
struct Shared final : kit::Inbound {
    mutable std::mutex mutex;
    mutable AcpMapper mapper;
    std::shared_ptr<PatchSink> sink;
    std::shared_ptr<Host> host;
    mutable kit::StateWatch state;
    mutable std::atomic<bool> shutting_down{false};

    mutable std::mutex slots;
    mutable std::optional<std::string> session_id;
    mutable std::optional<kit::Writer> writer;
    mutable kit::detail::WaitList writer_ready;
    mutable Modes modes;
    mutable std::optional<std::vector<HarnessModel>> models;

    Shared(AcpMapper m, std::shared_ptr<PatchSink> s, std::shared_ptr<Host> h)
        : mapper(std::move(m)), sink(std::move(s)), host(std::move(h)), state(mapper.ledger().state()) {}

    [[nodiscard]] std::string id() const {
        std::scoped_lock lock(slots);
        return session_id.value_or("");
    }

    [[nodiscard]] std::optional<kit::Writer> current_writer() const {
        std::scoped_lock lock(slots);
        return writer;
    }

    // Runs `f` on the mapper under its lock and publishes the state after.
    template <typename T, typename F>
    Result<T> with(F&& f) const {
        std::scoped_lock lock(mutex);
        Result<T> result = f(mapper, DomWriter(*sink));
        state.set(mapper.ledger().state());
        return result;
    }

    Result<> feed(Input in) const {
        auto replies = with<std::vector<Reply>>([&](AcpMapper& m, const DomWriter& w) { return m.apply(std::move(in), w); });
        if (!replies) return std::unexpected(std::move(replies).error());
        for (auto& r : *replies)
            if (auto sent = reply(std::move(r)); !sent) return sent;
        return {};
    }

    Result<> reply(Reply r) const {
        const kit::JsonRpc2 framing(true);
        Json frame;
        if (auto* respond = std::get_if<reply::Respond>(&r)) {
            frame = framing.response(std::move(respond->id), "", std::move(respond->result));
        } else if (auto* reject = std::get_if<reply::Reject>(&r)) {
            frame = framing.response(std::move(reject->id), "",
                                     std::unexpected(RpcError(RpcError::Command{"request", std::move(reject->message), "-32602"})));
        } else {
            frame = framing.notification("session/cancel", Json(Object{{"sessionId", id()}}));
        }
        auto w = current_writer();
        if (!w) return std::unexpected(AdapterError(AdapterError::Closed{}));
        if (auto sent = w->send(frame); !sent) return std::unexpected(sent.error().to_adapter_error());
        return {};
    }

    void report(const Result<>& result) const {
        if (!result) loa::runtime::log::error(LOG, "ACP event handling failed: " + result.error().message());
    }

    kit::Awaitable<RpcResult<Json>> on_request(std::string method, Json params) const override {
        co_return co_await host->serve(std::move(method), std::move(params));
    }
    bool defers_requests() const override { return true; }
    kit::Awaitable<void> on_deferred(Json id, std::string method, Json params) const override {
        auto executor = co_await asio::this_coro::executor;
        std::optional<kit::Writer> w;
        for (;;) {
            std::shared_ptr<kit::detail::Signal> signal;
            {
                std::scoped_lock lock(slots);
                if (writer) { w = writer; break; }
                signal = writer_ready.prepare(executor);
            }
            co_await kit::detail::WaitList::wait(signal);
        }
        if (method == "session/request_permission") {
            report(feed(input::Permission{std::move(id), std::move(params)}));
            co_return;
        }
        asio::co_spawn(executor,
            [](std::shared_ptr<Host> host, kit::Writer writer, Json id, std::string method, Json params) -> kit::Awaitable<void> {
                auto result = co_await host->serve(method, std::move(params));
                auto frame = kit::JsonRpc2(true).response(std::move(id), method, std::move(result));
                if (auto sent = writer.send(frame); !sent)
                    loa::runtime::log::warn(LOG, "couldn't answer ACP request: " + sent.error().message());
            }(host, *w, std::move(id), std::move(method), std::move(params)),
            asio::detached);
    }
    kit::Awaitable<void> on_notification(std::string method, Json params) const override {
        if (method == "session/update") report(feed(input::Update{std::move(params)}));
        co_return;
    }
    kit::Awaitable<void> on_response(std::string id, const RpcResult<Json>& result) const override {
        std::expected<Json, std::string> outcome = result ? std::expected<Json, std::string>(*result)
                                                          : std::unexpected(result.error().message());
        report(feed(input::Response{std::move(id), std::move(outcome)}));
        co_return;
    }
    kit::Awaitable<void> on_closed(std::optional<loa::runtime::exec::ExitStatus>, std::string) const override {
        report(feed(input::Closed{shutting_down.load()}));
        host->release_all();
        co_return;
    }
};

// The config's MCP servers in ACP's `McpServer` shape. HTTP servers need the agent's
// `mcpCapabilities.http`; stdio is mandatory for every agent.
Result<Array> mcp_servers(const SessionConfig& config, const wire::AgentCapabilities& agent) {
    auto pairs = [](const std::map<std::string, std::string>& map) {
        Array out;
        for (const auto& [name, value] : map) out.emplace_back(Object{{"name", name}, {"value", value}});
        return out;
    };
    Array out;
    for (const auto& server : config.mcp_servers) {
        if (const auto* stdio = std::get_if<McpStdio>(&server.transport)) {
            Array args(stdio->args.begin(), stdio->args.end());
            out.emplace_back(Object{{"name", server.name}, {"command", stdio->command}, {"args", std::move(args)}, {"env", pairs(stdio->env)}});
        } else if (agent.mcp_capabilities.http) {
            const auto& http = std::get<McpHttp>(server.transport);
            out.emplace_back(Object{{"type", "http"}, {"name", server.name}, {"url", http.url}, {"headers", pairs(http.headers)}});
        } else {
            return std::unexpected(AdapterError(AdapterError::Unsupported{HARNESS, "HTTP MCP servers"}));
        }
    }
    return out;
}

} // namespace

std::string file_uri(const std::filesystem::path& path) {
    std::string uri = "file://";
    for (const char c : path.native()) {
        const auto byte = static_cast<unsigned char>(c);
        if ((byte >= 'A' && byte <= 'Z') || (byte >= 'a' && byte <= 'z') || (byte >= '0' && byte <= '9') || byte == '-' ||
            byte == '.' || byte == '_' || byte == '~' || byte == '/')
            uri.push_back(c);
        else
            uri += std::format("%{:02X}", byte);
    }
    return uri;
}

std::vector<Json> prompt_blocks(const Prompt& prompt) {
    std::vector<Json> blocks;
    blocks.reserve(1 + prompt.attachments.size());
    blocks.emplace_back(Object{{"type", "text"}, {"text", prompt.text}});
    for (const auto& attachment : prompt.attachments) {
        const auto file = attachment.path.filename();
        std::string name = file.empty() ? attachment.path.string() : file.string();
        Object block{{"type", "resource_link"}, {"name", std::move(name)}, {"uri", file_uri(attachment.path)}};
        if (attachment.mime) block.emplace_back("mimeType", *attachment.mime);
        blocks.emplace_back(std::move(block));
    }
    return blocks;
}

struct AcpSession::Impl {
    std::shared_ptr<Shared> shared;
    kit::StdioRpc<kit::JsonRpc2> rpc;

    Impl(std::shared_ptr<Shared> s, kit::StdioRpc<kit::JsonRpc2> r) : shared(std::move(s)), rpc(std::move(r)) {}

    Awaitable<Result<>> switch_mode(std::string mode) const {
        Json params(Object{{"sessionId", shared->id()}, {"modeId", mode}});
        auto done = co_await rpc.request_as(Json(rpc.next_id()), "session/set_mode", std::move(params));
        if (!done) co_return std::unexpected(done.error().to_adapter_error());
        co_return shared->with<void>([&](AcpMapper& m, const DomWriter& w) { return committed(m.mode_changed(w, mode)); });
    }

    Awaitable<Result<>> switch_model(std::string model) const {
        Json params(Object{{"sessionId", shared->id()}, {"modelId", model}});
        auto done = co_await rpc.request_as(Json(rpc.next_id()), "session/set_model", std::move(params));
        if (!done) co_return std::unexpected(done.error().to_adapter_error());
        co_return shared->with<void>([&](AcpMapper& m, const DomWriter& w) { return committed(m.model_changed(w, model)); });
    }

    std::optional<std::string> mode_id(SessionMode mode) const {
        std::scoped_lock lock(shared->slots);
        const auto& modes = shared->modes;
        auto has = [&](std::string_view id) { return std::ranges::find(modes.available, id) != modes.available.end(); };
        if (mode == SessionMode::Plan) return has("plan") ? std::optional<std::string>("plan") : std::nullopt;
        if (has("default")) return "default";
        if (modes.initial && *modes.initial != "plan") return modes.initial;
        for (const auto& m : modes.available)
            if (m != "plan") return m;
        return std::nullopt;
    }
};

AcpSession::AcpSession(std::unique_ptr<Impl> impl) : impl_(std::move(impl)) {}

AcpSession::~AcpSession() {
    impl_->shared->shutting_down.store(true);
    impl_->rpc.kill();
}

std::string AcpSession::native_id() const { return impl_->shared->id(); }

Awaitable<Result<>> AcpSession::send(Prompt prompt) const {
    const auto& shared = *impl_->shared;
    const auto id = impl_->rpc.next_id();
    Json params(Object{{"sessionId", shared.id()}, {"prompt", Array(prompt_blocks(prompt))}});
    const Json frame = impl_->rpc.framing().request(Json(id), "session/prompt", std::move(params));
    RpcResult<void> sent;
    {
        std::scoped_lock lock(shared.mutex);
        switch (shared.mapper.ledger().state()) {
        case SessionState::Closed: co_return std::unexpected(AdapterError(AdapterError::Closed{}));
        case SessionState::Idle: break;
        default: co_return std::unexpected(AdapterError(AdapterError::Busy{}));
        }
        auto applied = shared.mapper.apply(input::Prompt{std::to_string(id), std::move(prompt)}, DomWriter(*shared.sink));
        shared.state.set(shared.mapper.ledger().state());
        if (!applied) co_return std::unexpected(std::move(applied).error());
        sent = impl_->rpc.send(frame);
    }
    if (!sent) {
        if (auto fed = shared.feed(input::Response{std::to_string(id), std::unexpected(sent.error().message())}); !fed)
            co_return fed;
        co_return std::unexpected(sent.error().to_adapter_error());
    }
    co_return Result<>{};
}

Awaitable<Result<>> AcpSession::interrupt() const { co_return impl_->shared->feed(input::Interrupt{}); }

Awaitable<Result<>> AcpSession::answer(NodeId decision, Answer answer) const {
    auto reply = impl_->shared->with<std::optional<Reply>>([&](AcpMapper& m, const DomWriter& w) { return m.answer(decision, answer, w); });
    if (!reply) co_return std::unexpected(std::move(reply).error());
    if (*reply) co_return impl_->shared->reply(std::move(**reply));
    co_return Result<>{};
}

Awaitable<Result<>> AcpSession::set_mode(SessionMode mode) const {
    auto id = impl_->mode_id(mode);
    if (!id) co_return std::unexpected(AdapterError(AdapterError::Unsupported{HARNESS, "set_mode"}));
    {
        std::scoped_lock lock(impl_->shared->mutex);
        if (impl_->shared->mapper.mode_id() == std::optional<std::string_view>(*id)) co_return Result<>{};
    }
    co_return co_await impl_->switch_mode(*id);
}

Awaitable<Result<std::vector<HarnessModel>>> AcpSession::models() const {
    std::scoped_lock lock(impl_->shared->slots);
    if (!impl_->shared->models) co_return std::unexpected(AdapterError(AdapterError::Unsupported{HARNESS, "listing models"}));
    co_return *impl_->shared->models;
}

Awaitable<Result<>> AcpSession::set_model(ModelSelection selection) const {
    std::optional<bool> offered;
    {
        std::scoped_lock lock(impl_->shared->slots);
        if (const auto& models = impl_->shared->models)
            offered = std::ranges::any_of(*models, [&](const HarnessModel& m) { return m.id == selection.model; });
    }
    if (!offered) co_return std::unexpected(AdapterError(AdapterError::Unsupported{HARNESS, "set_model"}));
    if (selection.role) co_return std::unexpected(AdapterError(AdapterError::Unsupported{HARNESS, "model roles"}));
    if (!*offered)
        co_return std::unexpected(AdapterError(AdapterError::Protocol{std::format("the agent doesn't offer model {}", selection.model)}));
    co_return co_await impl_->switch_model(std::move(selection.model));
}

SessionState AcpSession::state() const { return impl_->shared->state.get(); }

Awaitable<Result<>> AcpSession::wait_idle() const {
    (void)co_await impl_->shared->state.wait_settled();
    co_return Result<>{};
}

Awaitable<Result<>> AcpSession::shutdown() const {
    impl_->shared->shutting_down.store(true);
    impl_->rpc.kill();
    (void)co_await impl_->rpc.closed();
    impl_->shared->host->release_all();
    if (state() != SessionState::Closed) co_return impl_->shared->feed(input::Closed{true});
    co_return Result<>{};
}

AcpAdapter::AcpAdapter(std::filesystem::path program, std::vector<std::string> args)
    : program_(std::move(program)), args_(std::move(args)) {}

AcpAdapter::AcpAdapter(const AcpAdapter& other) : program_(other.program_), args_(other.args_), missing_(other.missing_) {
    std::scoped_lock lock(other.mutex_);
    learned_ = other.learned_;
}

AcpAdapter AcpAdapter::with_missing_hint(std::string hint) && {
    missing_ = std::move(hint);
    return std::move(*this);
}

Capabilities AcpAdapter::base_capabilities() {
    constexpr std::array decisions{DecisionKind::Approval};
    constexpr std::array context{ContextDelivery::McpResource};
    return Capabilities::builder().schema(SCHEMA_VERSION).decisions(decisions).context(context).mcp_servers().build();
}

template <typename F>
void AcpAdapter::learn(F&& update) const {
    std::scoped_lock lock(mutex_);
    if (!learned_) learned_.emplace();
    update(*learned_);
}

Capabilities AcpAdapter::capabilities() const {
    auto capabilities = base_capabilities();
    std::optional<Learned> learned;
    {
        std::scoped_lock lock(mutex_);
        learned = learned_;
    }
    if (learned && learned->plan_mode) capabilities.features.push_back(Feature::PlanMode);
    if (learned && learned->models) capabilities.model_switch = ModelSwitch::PerTurn;
    return capabilities;
}

Awaitable<Result<std::unique_ptr<AcpSession>>> AcpAdapter::connect(const SessionConfig& config, std::shared_ptr<PatchSink> sink,
                                                                   NodeId root, std::optional<std::string> model) const {
    if (!config.host_tools.empty()) co_return std::unexpected(AdapterError(AdapterError::Unsupported{HARNESS, "host tools"}));
    auto host = Host::create(config.cwd);
    if (!host) co_return std::unexpected(AdapterError(AdapterError::Io{std::move(host).error()}));
    auto shared = std::make_shared<Shared>(AcpMapper(root, std::move(model)), std::move(sink), std::move(*host));
    auto cmd = loa::runtime::exec::command(config.executable.value_or(program_), args_,
                                           {config.env.begin(), config.env.end()}, config.cwd);
    auto executor = co_await asio::this_coro::executor;
    auto rpc = kit::StdioRpc<kit::JsonRpc2>::spawn(executor, cmd, kit::JsonRpc2(true), shared);
    if (!rpc) co_return std::unexpected(rpc.error().to_adapter_error());
    {
        std::scoped_lock lock(shared->slots);
        shared->writer = rpc->writer();
    }
    shared->writer_ready.notify_all();
    co_return std::unique_ptr<AcpSession>(new AcpSession(std::make_unique<AcpSession::Impl>(shared, std::move(*rpc))));
}

Awaitable<Result<wire::AgentCapabilities>> AcpAdapter::initialize(const AcpSession& session) const {
    const auto& impl = *session.impl_;
    Json params(Object{
        {"protocolVersion", std::int64_t{PROTOCOL_VERSION}},
        {"clientCapabilities", Object{{"fs", Object{{"readTextFile", true}, {"writeTextFile", true}}}, {"terminal", true}}},
        {"clientInfo", Object{{"name", "loa"}, {"version", CLIENT_VERSION}}},
    });
    auto raw = co_await impl.rpc.request("initialize", std::move(params));
    if (!raw) co_return std::unexpected(raw.error().to_adapter_error());
    auto init = wire::decode_initialize(*raw);
    if (!init) co_return std::unexpected(malformed(init.error()));
    if (init->protocol_version != PROTOCOL_VERSION)
        co_return std::unexpected(AdapterError(AdapterError::Version{
            HARNESS, std::format("the agent speaks ACP version {}, this client speaks {}", init->protocol_version, PROTOCOL_VERSION)}));
    if (const auto& agent = init->agent_info) {
        std::string name = agent->version ? std::format("{} {}", agent->name, *agent->version) : agent->name;
        auto set = impl.shared->with<void>([&](AcpMapper& m, const DomWriter& w) {
            return committed(w.set(m.root(), X_AGENT, Value(std::move(name))));
        });
        if (!set) co_return std::unexpected(std::move(set).error());
    }
    co_return init->agent_capabilities;
}

Awaitable<Result<>> AcpAdapter::create(const SessionConfig& config, const AcpSession& session) const {
    const auto& impl = *session.impl_;
    auto agent = co_await initialize(session);
    if (!agent) co_return std::unexpected(std::move(agent).error());
    auto servers = mcp_servers(config, *agent);
    if (!servers) co_return std::unexpected(std::move(servers).error());
    Json params(Object{{"cwd", impl.shared->host->root().string()}, {"mcpServers", std::move(*servers)}});
    auto raw = co_await impl.rpc.request("session/new", std::move(params));
    if (!raw) co_return std::unexpected(raw.error().to_adapter_error());
    auto created = wire::decode_new_session(*raw);
    if (!created) co_return std::unexpected(malformed(created.error()));
    auto set = impl.shared->with<void>([&](AcpMapper& m, const DomWriter& w) { return committed(m.set_native(w, created->session_id)); });
    if (!set) co_return std::unexpected(std::move(set).error());
    {
        std::scoped_lock lock(impl.shared->slots);
        if (!impl.shared->session_id) impl.shared->session_id = created->session_id;
    }
    auto models = co_await adopt_models(config, session, std::move(created->models));
    if (!models) co_return models;
    co_return co_await adopt_modes(session, std::move(created->modes), config.mode);
}

Awaitable<Result<>> AcpAdapter::load(const SessionConfig& config, const AcpSession& session, std::string native_id) const {
    const auto& impl = *session.impl_;
    auto agent = co_await initialize(session);
    if (!agent) co_return std::unexpected(std::move(agent).error());
    if (!agent->load_session) co_return std::unexpected(AdapterError(AdapterError::Unsupported{HARNESS, "resume"}));
    auto servers = mcp_servers(config, *agent);
    if (!servers) co_return std::unexpected(std::move(servers).error());
    Json params(Object{{"sessionId", native_id}, {"cwd", impl.shared->host->root().string()}, {"mcpServers", std::move(*servers)}});
    {
        std::scoped_lock lock(impl.shared->mutex);
        impl.shared->mapper.adopt_native(native_id);
    }
    {
        std::scoped_lock lock(impl.shared->slots);
        if (!impl.shared->session_id) impl.shared->session_id = native_id;
    }
    const auto id = impl.rpc.next_id();
    // The ordered response hook clears replay before later notifications.
    if (auto fed = impl.shared->feed(input::Load{std::to_string(id)}); !fed) co_return fed;
    auto raw = co_await impl.rpc.request_as(Json(id), "session/load", std::move(params));
    if (!raw) co_return std::unexpected(raw.error().to_adapter_error());
    wire::LoadSessionResponse loaded;
    if (!raw->is_null()) {
        auto decoded = wire::decode_load_session(*raw);
        if (!decoded) co_return std::unexpected(malformed(decoded.error()));
        loaded = std::move(*decoded);
    }
    auto models = co_await adopt_models(config, session, std::move(loaded.models));
    if (!models) co_return models;
    co_return co_await adopt_modes(session, std::move(loaded.modes), config.mode);
}

Awaitable<Result<>> AcpAdapter::adopt_models(const SessionConfig& config, const AcpSession& session,
                                             std::optional<wire::SessionModelState> models) const {
    const auto& impl = *session.impl_;
    learn([&](Learned& l) { l.models = models.has_value(); });
    if (!models) co_return Result<>{};
    std::vector<HarnessModel> offered;
    for (auto& model : models->available_models) {
        HarnessModel entry;
        entry.name = model.name.empty() ? model.model_id : std::move(model.name);
        entry.id = std::move(model.model_id);
        entry.provider = std::string(HARNESS);
        offered.push_back(std::move(entry));
    }
    std::optional<std::string> wanted;
    if (config.model && *config.model != models->current_model_id) wanted = config.model;
    std::optional<std::string> to;
    if (wanted && std::ranges::any_of(offered, [&](const HarnessModel& m) { return m.id == *wanted; })) to = wanted;
    {
        std::scoped_lock lock(impl.shared->slots);
        impl.shared->models = std::move(offered);
    }
    if (to) co_return co_await impl.switch_model(*to);
    if (!wanted)
        co_return impl.shared->with<void>([&](AcpMapper& m, const DomWriter& w) { return committed(m.model_changed(w, models->current_model_id)); });
    co_return std::unexpected(AdapterError(AdapterError::Protocol{std::format("the agent doesn't offer model {}", config.model.value_or(""))}));
}

Awaitable<Result<>> AcpAdapter::adopt_modes(const AcpSession& session, std::optional<wire::SessionModeState> modes,
                                            std::optional<SessionMode> wanted) const {
    const auto& impl = *session.impl_;
    if (!modes) {
        learn([](Learned& l) { l.plan_mode = false; });
        co_return Result<>{};
    }
    std::vector<std::string> available;
    for (auto& m : modes->available_modes) available.push_back(std::move(m.id));
    const bool plan = std::ranges::find(available, "plan") != available.end();
    learn([&](Learned& l) { l.plan_mode = plan; });
    {
        std::scoped_lock lock(impl.shared->slots);
        impl.shared->modes.available = std::move(available);
        impl.shared->modes.initial = modes->current_mode_id;
    }
    auto changed = impl.shared->with<void>([&](AcpMapper& m, const DomWriter& w) { return committed(m.mode_changed(w, modes->current_mode_id)); });
    if (!changed) co_return changed;
    if (wanted) {
        auto target = impl.mode_id(*wanted);
        if (!target) co_return std::unexpected(AdapterError(AdapterError::Unsupported{HARNESS, "the configured mode"}));
        if (*target != modes->current_mode_id) co_return co_await impl.switch_mode(*target);
    }
    co_return Result<>{};
}

namespace {

// Hands out `session` once `setup` succeeded, and shuts the agent down otherwise.
Awaitable<Result<SessionPtr>> finish(std::unique_ptr<AcpSession> session, Result<> setup) {
    if (setup) co_return SessionPtr(std::move(session));
    (void)co_await session->shutdown();
    co_return std::unexpected(std::move(setup).error());
}

} // namespace

Awaitable<Result<SessionPtr>> AcpAdapter::start(SessionConfig config, std::shared_ptr<PatchSink> sink) const {
    // The root goes in before the agent starts so updates that arrive right after
    // `session/new` already have a home.
    auto root = kit::insert_root(DomWriter(*sink), HARNESS, config);
    if (!root) co_return std::unexpected(std::move(root).error());
    auto session = co_await connect(config, std::move(sink), *root, config.model);
    if (!session) co_return std::unexpected(std::move(session).error());
    auto setup = co_await create(config, **session);
    co_return co_await finish(std::move(*session), std::move(setup));
}

Awaitable<Result<SessionPtr>> AcpAdapter::resume(SessionConfig config, const Dom& dom, std::shared_ptr<PatchSink> sink) const {
    const Node* root = dom.root();
    if (!root) co_return std::unexpected(AdapterError(AdapterError::Format{"the session DOM is empty"}));
    auto native = root->str_attr(attr::NATIVE_ID);
    if (!native) co_return std::unexpected(AdapterError(AdapterError::Format{"the session has no native_id"}));
    std::string native_id(*native);
    std::optional<std::string> model = config.model;
    if (!model)
        if (auto m = root->str_attr(attr::MODEL)) model = std::string(*m);
    auto session = co_await connect(config, std::move(sink), root->id, std::move(model));
    if (!session) co_return std::unexpected(std::move(session).error());
    {
        std::scoped_lock lock((*session)->impl_->shared->mutex);
        (*session)->impl_->shared->mapper.adopt(dom);
    }
    auto setup = co_await load(config, **session, std::move(native_id));
    co_return co_await finish(std::move(*session), std::move(setup));
}

Awaitable<Availability> AcpAdapter::availability() const {
    std::error_code ec;
    SessionConfig config;
    config.cwd = std::filesystem::current_path(ec);
    if (ec) config.cwd = "/";
    auto sink = std::make_shared<MemorySink>();
    auto probe = [this, config, sink]() -> Awaitable<Result<>> {
        Attrs attrs{{std::string(attr::ID), Value("availability-probe")},
                    {std::string(attr::HARNESS), Value(HARNESS)},
                    {std::string(attr::NODE), Value("")},
                    {std::string(attr::CREATED), Value(now_ms())}};
        auto root = sink->commit(Draft::insert(NodeId::DOCUMENT, ElementKind::Session, std::move(attrs), ""));
        if (!root) co_return std::unexpected(commit_error(std::move(root).error()));
        auto session = co_await connect(config, sink, root->node, std::nullopt);
        if (!session) co_return std::unexpected(std::move(session).error());
        auto initialized = co_await initialize(**session);
        (void)co_await (*session)->shutdown();
        if (!initialized) co_return std::unexpected(std::move(initialized).error());
        co_return Result<>{};
    };
    auto timeout = []() -> Awaitable<bool> {
        asio::steady_timer timer(co_await asio::this_coro::executor, Availability::PROBE_TIMEOUT);
        co_await timer.async_wait(asio::use_awaitable);
        co_return true;
    };
    using namespace asio::experimental::awaitable_operators;
    auto outcome = co_await (probe() || timeout());
    if (std::holds_alternative<bool>(outcome))
        co_return Availability::unavailable(AvailabilityStatus::Failed, "the agent didn't answer `initialize`");
    auto result = std::move(std::get<Result<>>(outcome));
    if (result) {
        std::optional<std::string> agent;
        const Dom dom = sink->dom();
        if (const Node* root = dom.root())
            if (auto name = root->str_attr(X_AGENT)) agent = std::string(*name);
        auto ready = Availability::ready(std::nullopt, std::nullopt);
        ready.detail = agent ? std::move(agent) : std::optional<std::string>("ACP agent");
        co_return ready;
    }
    if (const auto* missing = result.error().get<AdapterError::NotInstalled>())
        co_return Availability::unavailable(AvailabilityStatus::NotInstalled,
                                            missing_.value_or(std::format("`{}` not found", missing->executable)));
    if (const auto* version = result.error().get<AdapterError::Version>())
        co_return Availability::unavailable(AvailabilityStatus::Incompatible, version->detail);
    co_return Availability::unavailable(AvailabilityStatus::Failed, result.error().message());
}

void register_factory() {
    registry::register_build(HARNESS, registry::HarnessBuild{
        [](const registry::HarnessEnv&, std::optional<std::filesystem::path> exe) -> std::shared_ptr<Adapter> {
            const bool named = exe.has_value();
            auto launch = registry::acp_launch(std::move(exe));
            AcpAdapter adapter(std::move(launch.program), std::move(launch.args));
            if (named) return std::make_shared<AcpAdapter>(std::move(adapter));
            std::string names;
            for (const auto& agent : registry::ACP_AGENTS) names += (names.empty() ? "" : ", ") + std::string(agent.name);
            return std::make_shared<AcpAdapter>(std::move(adapter).with_missing_hint(
                launch.missing_hint.value_or(std::format("no ACP agent on PATH (looked for {})", names))));
        },
        {}});
}

} // namespace loa::harness::acp
