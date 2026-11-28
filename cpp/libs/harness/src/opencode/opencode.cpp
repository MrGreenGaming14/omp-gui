#include "loa/harness/opencode/opencode.hpp"

#include <asio.hpp>
#include <chrono>
#include <condition_variable>
#include <mutex>
#include <thread>
#include <utility>

#include "loa/core/writer.hpp"
#include "loa/harness/opencode/ids.hpp"
#include "loa/harness/opencode/mapper.hpp"
#include "loa/harness/opencode/native.hpp"
#include "loa/harness/registry/registry.hpp"

namespace loa::harness::opencode {
namespace {
using namespace loa::contracts;
using loa::runtime::json::Value;
using loa::runtime::json::Object;
using loa::runtime::json::Array;
using loa::core::DomWriter;
using loa::core::PatchSink;

std::string string_at(const Value& v, std::string_view key, std::string fallback = {}) {
    if (auto* p = v.find(key); p && p->as_string()) return *p->as_string();
    return fallback;
}

Value object(std::initializer_list<std::pair<std::string, Value>> fields) { return Value(Object(fields)); }
Result<> check_supported(const SessionConfig& config) {
    if (!config.host_tools.empty() || !config.disabled_tools.empty())
        return std::unexpected(AdapterError::Unsupported{"opencode", "host tools or disabled tools"});
    return {};
}

Result<Mapper> resumed(const loa::core::Dom& dom) {
    auto mapper = Mapper::from_dom(dom);
    if (!mapper) return std::unexpected(mapper.error());
    if (mapper->native_id().empty()) return std::unexpected(AdapterError::Format{"session has no OpenCode native_id"});
    return mapper;
}
} // namespace

std::vector<HarnessModel> provider_models(const Value& providers) {
    std::vector<HarnessModel> out;
    auto* list = providers.find("providers");
    if (!list || !list->as_array()) return out;
    for (const auto& provider : *list->as_array()) {
        const auto* id = provider.find("id");
        const auto* models = provider.find("models");
        if (!id || !id->as_string() || !models || !models->as_object()) continue;
        for (const auto& [key, model] : *models->as_object()) {
            auto model_id = string_at(model, "id", key);
            out.push_back(HarnessModel{*id->as_string() + "/" + model_id,
                                       string_at(model, "name", model_id), *id->as_string(), {}});
        }
    }
    return out;
}

struct OpenCodeSession::Impl {
    Client client;
    std::shared_ptr<PatchSink> sink;
    mutable std::mutex mutex;
    mutable std::condition_variable changed;
    Mapper mapper;
    std::optional<std::string> context;
    loa::core::SessionMode mode;
    bool closed = false;
    std::shared_ptr<EventStream> events;
    std::thread worker;

    Impl(Client c, std::shared_ptr<PatchSink> s, Mapper m, std::optional<std::string> ctx,
         loa::core::SessionMode md, EventStream e)
        : client(std::move(c)), sink(std::move(s)), mapper(std::move(m)), context(std::move(ctx)), mode(md),
          events(std::make_shared<EventStream>(std::move(e))) {
        worker = std::thread([this] {
            while (true) {
                auto next = events->next();
                if (!next || !*next) break;
                std::lock_guard lock(mutex);
                if (closed) break;
                DomWriter writer(*sink);
                (void)mapper.apply_event(writer, std::move(**next));
                changed.notify_all();
            }
            std::lock_guard lock(mutex);
            closed = true;
            changed.notify_all();
        });
    }
    ~Impl() { stop(); }
    void stop() {
        {
            std::lock_guard lock(mutex);
            closed = true;
            changed.notify_all();
        }
        if (events) events->stop();
        if (worker.joinable()) worker.join();
        // Dropping the transfer closes the connection; the server keeps running.
        events.reset();
    }
};

OpenCodeSession::OpenCodeSession(std::unique_ptr<Impl> impl) : impl_(std::move(impl)) {}
OpenCodeSession::~OpenCodeSession() = default;

Awaitable<Result<>> OpenCodeSession::send(Prompt prompt) const {
    auto& inner = *impl_;
    const auto message_id = ids::ascending("msg", loa::core::now_ms());
    Array parts{object({{"type", "text"}, {"text", prompt.text}})};
    for (const auto& attachment : prompt.attachments) {
        if (!attachment.path.is_absolute()) co_return std::unexpected(AdapterError::Format{"attachment requires an absolute file path"});
        // curl's URL escaping is used by the HTTP client; file paths use RFC 8089 slash-delimited URLs.
        std::string url = "file://" + attachment.path.generic_string();
        parts.push_back(object({{"type", "file"}, {"url", url},
            {"filename", attachment.path.filename().string()},
            {"mime", attachment.mime.value_or("application/octet-stream")}}));
    }
    Value body = object({{"messageID", message_id}, {"parts", std::move(parts)}});
    loa::core::NodeId turn;
    std::string native_id;
    {
        std::lock_guard lock(inner.mutex);
        if (inner.closed) co_return std::unexpected(AdapterError::Closed{});
        if (inner.mapper.state() != SessionState::Idle) co_return std::unexpected(AdapterError::Busy{});
        if (auto model = inner.mapper.model()) {
            auto slash = model->find('/');
            if (slash == std::string::npos) co_return std::unexpected(AdapterError::Format{"model must be provider/model"});
            body.set("model", object({{"providerID", model->substr(0, slash)}, {"modelID", model->substr(slash + 1)}}));
        }
        if (inner.context) body.set("system", *inner.context);
        if (inner.mode == loa::core::SessionMode::Plan) body.set("agent", "plan");
        DomWriter writer(*inner.sink);
        auto started = inner.mapper.begin_turn(writer, message_id, prompt.text, prompt.by);
        if (!started) co_return std::unexpected(started.error());
        turn = *started;
        native_id = inner.mapper.native_id();
        inner.changed.notify_all();
    }
    auto response = inner.client.request("POST", "/session/" + native_id + "/prompt_async", std::move(body));
    if (!response) {
        std::lock_guard lock(inner.mutex);
        DomWriter writer(*inner.sink);
        auto failed = inner.mapper.fail_turn(writer, turn, response.error().message());
        inner.changed.notify_all();
        if (!failed) co_return std::unexpected(failed.error());
        co_return std::unexpected(response.error());
    }
    co_return Result<>{};
}

Awaitable<Result<>> OpenCodeSession::interrupt() const {
    auto& inner = *impl_;
    std::string id;
    {
        std::lock_guard lock(inner.mutex);
        if (inner.closed) co_return std::unexpected(AdapterError::Closed{});
        if (inner.mapper.state() == SessionState::Idle) co_return Result<>{};
        id = inner.mapper.native_id();
    }
    auto response = inner.client.request("POST", "/session/" + id + "/abort");
    if (!response) co_return std::unexpected(response.error());
    std::lock_guard lock(inner.mutex);
    DomWriter writer(*inner.sink);
    auto result = inner.mapper.cancel_turn(writer);
    inner.changed.notify_all();
    co_return result;
}

Awaitable<Result<>> OpenCodeSession::answer(loa::core::NodeId decision, Answer answer) const {
    auto& inner = *impl_;
    Reply reply;
    {
        std::lock_guard lock(inner.mutex);
        if (inner.closed) co_return std::unexpected(AdapterError::Closed{});
        auto planned = inner.mapper.plan_reply(decision, answer);
        if (!planned) co_return std::unexpected(planned.error());
        if (*planned) reply = std::move(**planned);
    }
    if (!reply.path.empty()) {
        auto response = inner.client.request("POST", reply.path, reply.body);
        if (!response) co_return std::unexpected(response.error());
    }
    std::lock_guard lock(inner.mutex);
    DomWriter writer(*inner.sink);
    auto result = inner.mapper.record_answer(writer, decision, answer);
    inner.changed.notify_all();
    co_return result;
}

Awaitable<Result<>> OpenCodeSession::set_model(ModelSelection selection) const {
    if (selection.role) co_return std::unexpected(AdapterError::Unsupported{"opencode", "model roles"});
    if (selection.model.find('/') == std::string::npos) co_return std::unexpected(AdapterError::Format{"model must be provider/model"});
    auto& inner = *impl_;
    std::lock_guard lock(inner.mutex);
    if (inner.closed) co_return std::unexpected(AdapterError::Closed{});
    if (inner.mapper.state() != SessionState::Idle) co_return std::unexpected(AdapterError::Busy{});
    DomWriter writer(*inner.sink);
    co_return inner.mapper.set_model(writer, selection.model);
}

Awaitable<Result<std::vector<HarnessModel>>> OpenCodeSession::models() const {
    auto response = impl_->client.request("GET", "/config/providers");
    if (!response) co_return std::unexpected(response.error());
    co_return provider_models(*response);
}

Awaitable<Result<>> OpenCodeSession::set_mode(loa::core::SessionMode mode) const {
    auto& inner = *impl_;
    std::lock_guard lock(inner.mutex);
    if (inner.closed) co_return std::unexpected(AdapterError::Closed{});
    if (inner.mapper.state() != SessionState::Idle) co_return std::unexpected(AdapterError::Busy{});
    inner.mode = mode;
    DomWriter writer(*inner.sink);
    auto changed = writer.set(inner.mapper.root(), loa::core::attr::MODE, loa::core::Value(mode));
    if (!changed) co_return std::unexpected(AdapterError::Commit{changed.error()});
    co_return Result<>{};
}

Awaitable<Result<>> OpenCodeSession::compact(std::optional<std::string> instructions) const {
    if (instructions) co_return std::unexpected(AdapterError::Unsupported{"opencode", "custom compaction instructions"});
    std::string id, model;
    {
        std::lock_guard lock(impl_->mutex);
        if (impl_->closed) co_return std::unexpected(AdapterError::Closed{});
        id = impl_->mapper.native_id();
        model = impl_->mapper.model().value_or("");
    }
    auto slash = model.find('/');
    if (slash == std::string::npos) co_return std::unexpected(AdapterError::Format{"no provider/model is selected"});
    auto response = impl_->client.request("POST", "/session/" + id + "/summarize",
        object({{"providerID", model.substr(0, slash)}, {"modelID", model.substr(slash + 1)}}));
    if (!response) co_return std::unexpected(response.error());
    co_return Result<>{};
}

SessionState OpenCodeSession::state() const {
    std::lock_guard lock(impl_->mutex);
    return impl_->closed ? SessionState::Closed : impl_->mapper.state();
}
Awaitable<Result<>> OpenCodeSession::wait_idle() const {
    // Wait off the event worker's lock; the notification follows every mapped event.
    auto& inner = *impl_;
    auto executor = co_await asio::this_coro::executor;
    asio::steady_timer timer(executor);
    for (;;) {
        {
            std::lock_guard lock(inner.mutex);
            if (inner.closed) co_return std::unexpected(AdapterError::Closed{});
            if (inner.mapper.state() == SessionState::Idle) co_return Result<>{};
        }
        timer.expires_after(std::chrono::milliseconds(20));
        co_await timer.async_wait(asio::use_awaitable);
    }
}
Awaitable<Result<>> OpenCodeSession::shutdown() const {
    impl_->stop();
    co_return Result<>{};
}

OpenCodeAdapter::OpenCodeAdapter(std::string base, std::optional<ServerAuth> auth)
    : base_(std::move(base)), auth_(std::move(auth)) {}

Awaitable<Availability> OpenCodeAdapter::availability() const {
    std::error_code ec;
    auto cwd = std::filesystem::current_path(ec);
    Client client(base_, ec ? std::string{} : cwd.string(), auth_);
    auto probe = client.probe("/global/health", PROBE_TIMEOUT);
    if (!probe) co_return Availability::unavailable(AvailabilityStatus::Unreachable,
        "no OpenCode server at " + base_ + "; start `opencode serve`");
    if (probe->first < 200 || probe->first >= 300) {
        probe = client.probe("/config/providers", PROBE_TIMEOUT);
        if (!probe) co_return Availability::unavailable(AvailabilityStatus::Unreachable,
            "no OpenCode server at " + base_ + "; start `opencode serve`");
        if (probe->first < 200 || probe->first >= 300)
            co_return Availability::unavailable(AvailabilityStatus::Incompatible,
                "the server at " + base_ + " answered HTTP " + std::to_string(probe->first));
    }
    auto version = string_at(probe->second, "version");
    auto ready = Availability::ready(std::nullopt, version.empty() ? std::nullopt : std::optional(version));
    ready.detail = version.empty() ? base_ : version;
    co_return ready;
}

Capabilities OpenCodeAdapter::capabilities() const {
    constexpr Feature features[]{Feature::Fork, Feature::Rewind, Feature::Compaction, Feature::Subagents, Feature::PlanMode};
    constexpr loa::core::DecisionKind decisions[]{loa::core::DecisionKind::Approval, loa::core::DecisionKind::Question};
    constexpr ContextDelivery context[]{ContextDelivery::SystemPrompt};
    return Capabilities::builder().schema(loa::core::SCHEMA_VERSION).features(features)
        .model_switch(ModelSwitch::PerTurn).decisions(decisions).context(context)
        .mcp_servers().import().export_().build();
}

Awaitable<Result<SessionPtr>> OpenCodeAdapter::start(SessionConfig config, std::shared_ptr<PatchSink> sink) const {
    if (auto ok = check_supported(config); !ok) co_return std::unexpected(ok.error());
    Client client(base_, config.cwd.string(), auth_);
    auto events = client.events();
    if (!events) co_return std::unexpected(events.error());
    auto native = client.request("POST", "/session", object({}));
    if (!native) co_return std::unexpected(native.error());
    for (const auto& server : config.mcp_servers) {
        Value transport;
        if (auto* http = std::get_if<McpHttp>(&server.transport)) {
            Object headers;
            for (const auto& [k, v] : http->headers) headers.emplace_back(k, v);
            transport = object({{"type", "remote"}, {"url", http->url}, {"headers", std::move(headers)}});
        } else {
            const auto& stdio = std::get<McpStdio>(server.transport);
            Array command{Value(stdio.command)};
            for (const auto& arg : stdio.args) command.emplace_back(arg);
            Object environment;
            for (const auto& [k, v] : stdio.env) environment.emplace_back(k, v);
            transport = object({{"type", "local"}, {"command", std::move(command)}, {"environment", std::move(environment)}});
        }
        auto attached = client.request("POST", "/mcp", object({{"name", server.name}, {"config", std::move(transport)}}));
        if (!attached) co_return std::unexpected(attached.error());
    }
    DomWriter writer(*sink);
    RootInfo root;
    root.session_id = config.session_id; root.node = config.node; root.cwd = config.cwd.string();
    root.project = config.project; root.model = config.model; root.model_role = config.model_role;
    root.effort = config.effort; root.mode = config.mode; root.native = std::move(*native);
    auto mapper = Mapper::start(writer, std::move(root));
    if (!mapper) co_return std::unexpected(mapper.error());
    co_return SessionPtr(new OpenCodeSession(std::make_unique<OpenCodeSession::Impl>(
        std::move(client), std::move(sink), std::move(*mapper), config.context,
        config.mode.value_or(loa::core::SessionMode::Normal), std::move(*events))));
}

Awaitable<Result<SessionPtr>> OpenCodeAdapter::resume(SessionConfig config, const loa::core::Dom& dom,
                                                       std::shared_ptr<PatchSink> sink) const {
    auto mapper = resumed(dom);
    if (!mapper) co_return std::unexpected(mapper.error());
    Client client(base_, config.cwd.string(), auth_);
    auto events = client.events();
    if (!events) co_return std::unexpected(events.error());
    auto response = client.request("GET", "/session/" + mapper->native_id());
    if (!response) co_return std::unexpected(response.error());
    DomWriter writer(*sink);
    if (auto cancelled = mapper->cancel_turn(writer); !cancelled) co_return std::unexpected(cancelled.error());
    co_return SessionPtr(new OpenCodeSession(std::make_unique<OpenCodeSession::Impl>(
        std::move(client), std::move(sink), std::move(*mapper), config.context,
        config.mode.value_or(loa::core::SessionMode::Normal), std::move(*events))));
}

Awaitable<Result<SessionPtr>> OpenCodeAdapter::fork(SessionConfig config, const loa::core::Dom& dom,
                                                     std::shared_ptr<PatchSink> sink) const {
    auto mapper = resumed(dom);
    if (!mapper) co_return std::unexpected(mapper.error());
    Client client(base_, config.cwd.string(), auth_);
    auto events = client.events();
    if (!events) co_return std::unexpected(events.error());
    auto response = client.request("POST", "/session/" + mapper->native_id() + "/fork", object({}));
    if (!response) co_return std::unexpected(response.error());
    const auto* id = response->find("id");
    if (!id || !id->as_string()) co_return std::unexpected(AdapterError::Protocol{"fork response has no id"});
    DomWriter writer(*sink);
    if (auto changed = mapper->set_native_id(writer, *id->as_string()); !changed) co_return std::unexpected(changed.error());
    if (auto cancelled = mapper->cancel_turn(writer); !cancelled) co_return std::unexpected(cancelled.error());
    co_return SessionPtr(new OpenCodeSession(std::make_unique<OpenCodeSession::Impl>(
        std::move(client), std::move(sink), std::move(*mapper), config.context,
        config.mode.value_or(loa::core::SessionMode::Normal), std::move(*events))));
}

Result<> OpenCodeAdapter::import(const std::filesystem::path& path, PatchSink& sink) const {
    return OpenCodeNative{}.import(path, sink);
}
Result<> OpenCodeAdapter::export_(const loa::core::Dom& dom, const std::filesystem::path& path) const {
    return OpenCodeNative{}.export_(dom, path);
}

void register_factory() {
    registry::register_build("opencode", registry::HarnessBuild{
        [](const registry::HarnessEnv&, std::optional<std::filesystem::path> exe) -> std::shared_ptr<Adapter> {
            return std::make_shared<OpenCodeAdapter>(exe ? exe->string() : std::string(DEFAULT_URL));
        }, {}});
}

} // namespace loa::harness::opencode
