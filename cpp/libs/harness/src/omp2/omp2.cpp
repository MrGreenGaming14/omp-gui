#include "loa/harness/omp2/omp2.hpp"

#include <asio.hpp>
#include <asio/experimental/awaitable_operators.hpp>

#include <algorithm>
#include <chrono>
#include <fstream>
#include <format>
#include <iterator>
#include <mutex>
#include <utility>

#include "loa/core/base64.hpp"
#include "loa/harness/kit/ledger.hpp"
#include "loa/harness/kit/mapper.hpp"
#include "loa/harness/kit/rpc.hpp"
#include "loa/harness/registry/registry.hpp"
#include "loa/runtime/log.hpp"
#include "loa/runtime/text.hpp"

namespace loa::harness::omp2 {

namespace {

using namespace loa::contracts;
using namespace loa::core;
using loa::runtime::json::Array;
using loa::runtime::json::Object;
using loa::runtime::json::Value;
using loa::harness::kit::RpcError;
using loa::harness::kit::RpcResult;
using loa::harness::kit::Incoming;
using loa::harness::kit::incoming::Notification;
using loa::harness::kit::incoming::Response;

AdapterError commit_error(CommitError e) { return AdapterError(AdapterError::Commit{std::move(e)}); }

class Frames final : public kit::Framing {
  public:
    Value request(const Value& id, std::string_view method, Value params) const override {
        auto frame = notification(method, std::move(params));
        frame.set("id", id);
        return frame;
    }
    Value notification(std::string_view method, Value params) const override {
        Object frame = params.is_object() ? std::move(*params.as_object()) : Object{};
        if (std::ranges::none_of(frame, [](const auto& field) { return field.first == "type"; }))
            frame.emplace_back("type", method);
        return frame;
    }
    Value response(Value, std::string_view, RpcResult<Value> result) const override {
        return result ? std::move(*result) : Value{};
    }
    std::optional<Incoming> classify(Value msg) const override {
        const Value* type = msg.find("type");
        if (type == nullptr || type->as_string() == nullptr || *type->as_string() != "response")
            return Notification{"", std::move(msg)};
        auto text = [&](std::string_view key) {
            const Value* item = msg.find(key);
            return std::string(item && item->as_string() ? *item->as_string() : "");
        };
        const Value* success = msg.find("success");
        RpcResult<Value> result = success && success->as_bool() && *success->as_bool()
            ? RpcResult<Value>(msg.find("data") ? *msg.find("data") : Value{})
            : RpcResult<Value>(std::unexpected(RpcError(RpcError::Command{
                  text("command"), text("error"), msg.find("code") && msg.find("code")->as_string()
                      ? std::optional<std::string>(*msg.find("code")->as_string()) : std::nullopt})));
        const Value* id = msg.find("id");
        return Response{kit::id_key(id ? *id : Value{}), std::move(result)};
    }
};

// `StdioRpc` keeps this inbound port alive. Its mapper/sink and state are also used by the session.
struct Shared final : kit::Inbound {
    mutable std::mutex mutex;
    mutable Omp2Mapper mapper;
    std::shared_ptr<PatchSink> sink;
    mutable kit::StateWatch state;

    Shared(Omp2Mapper m, std::shared_ptr<PatchSink> s)
        : mapper(std::move(m)), sink(std::move(s)), state(mapper.ledger().state()) {}

    template <typename T, typename F>
    Result<T> with(F&& f) const {
        std::scoped_lock lock(mutex);
        Result<T> result = f(mapper, DomWriter(*sink));
        state.set(mapper.ledger().state());
        return result;
    }

    Result<> keep(const Value& msg, std::string_view kind, std::string_view detail) const {
        return with<void>([&](Omp2Mapper& m, const DomWriter& w) -> Result<> {
            Attrs attrs{{std::string(attr::HARNESS), std::string(HARNESS)},
                        {std::string(attr::TYPE), std::string(kind)},
                        {"x-error", std::string(detail)}};
            auto id = w.insert(m.ledger().session, ElementKind::Ext, std::move(attrs), loa::runtime::json::dump(msg));
            if (!id) return std::unexpected(commit_error(std::move(id).error()));
            return {};
        });
    }

    Awaitable<RpcResult<Value>> on_request(std::string method, Value) const override {
        co_return std::unexpected(RpcError(RpcError::Protocol{std::format("omp2 sent an unexpected request `{}`", method)}));
    }
    Awaitable<void> on_notification(std::string, Value msg) const override {
        auto input = wire::decode_input(msg);
        Result<> result;
        if (!input) result = keep(msg, "unparsed", input.error());
        else {
            result = with<void>([&](Omp2Mapper& m, const DomWriter& w) -> Result<> {
                auto replies = m.apply(std::move(*input), w);
                if (!replies) return std::unexpected(std::move(replies).error());
                return {};
            });
            if (!result && result.error().get<AdapterError::Protocol>())
                result = keep(msg, "untranslated", result.error().message());
        }
        if (!result) loa::runtime::log::error("loa::harness::omp2", result.error().message());
        co_return;
    }
    Awaitable<void> on_closed(std::optional<loa::runtime::exec::ExitStatus>, std::string) const override {
        with<void>([](Omp2Mapper& m, const DomWriter&) -> Result<> { m.ledger_mut().close(); return {}; });
        co_return;
    }
};

// A `RpcError::Command(session_busy)` means Busy; otherwise preserve the generic RPC error mapping.
AdapterError rpc_error(RpcError error) {
    if (error.code() == "session_busy") return AdapterError(AdapterError::Busy{});
    return error.to_adapter_error();
}

} // namespace

struct Omp2Session::Impl {
    std::shared_ptr<Shared> shared;
    kit::StdioRpc<Frames> rpc;

    Impl(std::shared_ptr<Shared> s, kit::StdioRpc<Frames> r) : shared(std::move(s)), rpc(std::move(r)) {}
};

Omp2Session::Omp2Session(std::unique_ptr<Impl> impl) : impl_(std::move(impl)) {}
Omp2Session::~Omp2Session() = default;

Awaitable<Result<Value>> Omp2Session::call(std::string method, Value params) const {
    auto result = co_await impl_->rpc.request_as(Value(impl_->rpc.next_id()), std::move(method), std::move(params));
    if (!result) co_return std::unexpected(rpc_error(std::move(result).error()));
    co_return *result;
}

Result<> Omp2Session::set_root(std::string_view name, std::string_view value) const {
    return impl_->shared->with<void>([&](Omp2Mapper& m, const DomWriter& w) -> Result<> {
        auto result = w.set(m.ledger().session, name, std::string(value));
        if (!result) return std::unexpected(commit_error(std::move(result).error()));
        return {};
    });
}

Awaitable<Result<>> Omp2Session::send(Prompt prompt) const {
    switch (state()) {
    case SessionState::Closed: co_return std::unexpected(AdapterError(AdapterError::Closed{}));
    case SessionState::Idle: break;
    default: co_return std::unexpected(AdapterError(AdapterError::Busy{}));
    }
    Object params{{"message", prompt.text}};
    Array images;
    for (const auto& attachment : prompt.attachments) {
        std::optional<std::string_view> mime = attachment.mime ? std::optional<std::string_view>(*attachment.mime)
                                                                : loa::runtime::text::mime_of(attachment.path);
        if (!mime || !mime->starts_with("image/"))
            co_return std::unexpected(AdapterError(AdapterError::Unsupported{HARNESS, "attachments other than images"}));
        std::ifstream file(attachment.path, std::ios::binary);
        if (!file) co_return std::unexpected(AdapterError(AdapterError::Io{
            loa::Error(ErrorCode::io, std::format("cannot read {}", attachment.path.string()))}));
        std::string bytes(std::istreambuf_iterator<char>{file}, std::istreambuf_iterator<char>{});
        if (file.bad()) co_return std::unexpected(AdapterError(AdapterError::Io{
            loa::Error(ErrorCode::io, std::format("cannot read {}", attachment.path.string()))}));
        auto encoded = loa::core::base64::encode(std::span(reinterpret_cast<const std::uint8_t*>(bytes.data()), bytes.size()));
        images.emplace_back(Object{{"data", std::move(encoded)}, {"mimeType", *mime}});
    }
    if (!images.empty()) params.emplace_back("images", std::move(images));
    auto begun = impl_->shared->with<NodeId>([&](Omp2Mapper& m, const DomWriter& w) { return m.begin(w, prompt); });
    if (!begun) co_return std::unexpected(std::move(begun).error());
    auto reply = co_await call("prompt", std::move(params));
    if (!reply) {
        auto finished = impl_->shared->with<void>([](Omp2Mapper& m, const DomWriter& w) { return m.finish(w, TurnStatus::Error); });
        if (!finished) co_return std::unexpected(std::move(finished).error());
        co_return std::unexpected(std::move(reply).error());
    }
    co_return Result<>{};
}

Awaitable<Result<>> Omp2Session::interrupt() const {
    if (state() == SessionState::Running || state() == SessionState::Waiting) {
        auto result = co_await call("abort", Object{});
        if (!result) co_return std::unexpected(std::move(result).error());
    }
    co_return Result<>{};
}

Awaitable<Result<>> Omp2Session::answer(NodeId decision, Answer answer) const {
    auto reply = impl_->shared->with<std::optional<wire::Reply>>([&](Omp2Mapper& m, const DomWriter& w) {
        return m.answer(decision, answer, w);
    });
    if (!reply) co_return std::unexpected(std::move(reply).error());
    if (!*reply) co_return Result<>{};
    auto frame = wire::to_json(**reply);
    if (std::holds_alternative<wire::reply::Approve>(**reply)) {
        auto sent = co_await call("approve", std::move(frame));
        if (!sent) co_return std::unexpected(std::move(sent).error());
    } else {
        auto sent = impl_->rpc.send(frame);
        if (!sent) co_return std::unexpected(sent.error().to_adapter_error());
    }
    co_return Result<>{};
}

Awaitable<Result<>> Omp2Session::set_model(ModelSelection selection) const {
    if (selection.role)
        co_return std::unexpected(AdapterError(AdapterError::Unsupported{HARNESS, "setting a model role over RPC"}));
    auto slash = selection.model.find('/');
    if (slash == std::string::npos)
        co_return std::unexpected(AdapterError(AdapterError::Protocol{
            std::format("omp2 models are `provider/model`, got `{}`", selection.model)}));
    Object params;
    params.emplace_back("provider", selection.model.substr(0, slash));
    params.emplace_back("modelId", selection.model.substr(slash + 1));
    auto result = co_await call("set_model", Value(std::move(params)));
    if (!result) co_return std::unexpected(std::move(result).error());
    auto updated = set_root(attr::MODEL, selection.model);
    co_return updated;
}

Awaitable<Result<std::vector<HarnessModel>>> Omp2Session::models() const {
    auto data = co_await call("get_available_models", Object{});
    if (!data) co_return std::unexpected(std::move(data).error());
    const Value* raw = data->find("models");
    auto models = wire::decode_models(raw ? *raw : Value{});
    if (!models) co_return std::unexpected(AdapterError(AdapterError::Protocol{"get_available_models: " + models.error()}));
    std::vector<HarnessModel> out;
    for (auto& m : *models) {
        HarnessModel entry;
        entry.id = m.provider + "/" + m.id;
        entry.name = m.name.empty() ? m.id : m.name;
        entry.provider = std::move(m.provider);
        if (m.reasoning) for (auto level : THINKING_LEVELS) entry.efforts.emplace_back(level);
        out.push_back(std::move(entry));
    }
    co_return out;
}

Awaitable<Result<>> Omp2Session::set_effort(std::string effort) const {
    Object params;
    params.emplace_back("level", effort);
    auto result = co_await call("set_thinking_level", Value(std::move(params)));
    if (!result) co_return std::unexpected(std::move(result).error());
    auto updated = set_root(attr::EFFORT, effort);
    co_return updated;
}

Awaitable<Result<>> Omp2Session::compact(std::optional<std::string> instructions) const {
    Object params;
    if (instructions) params.emplace_back("customInstructions", std::move(*instructions));
    auto result = co_await call("compact", std::move(params));
    if (!result) co_return std::unexpected(std::move(result).error());
    co_return Result<>{};
}

SessionState Omp2Session::state() const { return impl_->shared->state.get(); }

Awaitable<Result<>> Omp2Session::wait_idle() const {
    auto state = co_await impl_->shared->state.wait_settled();
    if (state == SessionState::Closed) co_return std::unexpected(AdapterError(AdapterError::Closed{}));
    co_return Result<>{};
}

Awaitable<Result<>> Omp2Session::shutdown() const {
    (void)impl_->rpc.send(Object{{"type", "quit"}, {"id", "loa-quit"}});
    (void)co_await impl_->rpc.shutdown(std::chrono::seconds(5));
    co_return impl_->shared->with<void>([](Omp2Mapper& m, const DomWriter&) -> Result<> {
        m.ledger_mut().close();
        return {};
    });
}

Omp2Adapter::Omp2Adapter(std::filesystem::path executable) : executable_(std::move(executable)) {}

Result<loa::runtime::exec::Command> Omp2Adapter::command(const SessionConfig& config, std::optional<std::string_view> resume) const {
    auto refuse = [](std::string_view what) -> Result<loa::runtime::exec::Command> {
        return std::unexpected(AdapterError(AdapterError::Unsupported{HARNESS, what}));
    };
    if (!config.mcp_servers.empty() || !config.host_tools.empty() || !config.disabled_tools.empty())
        return refuse("configuring tools over omp2 RPC");
    if (config.context) return refuse("injecting project context over omp2 RPC");
    if (config.instructions) return refuse("system instructions over omp2 RPC");
    if (config.model_role || (config.mode && *config.mode != SessionMode::Normal))
        return refuse("model roles or mode over omp2 RPC");
    auto cmd = loa::runtime::exec::command(config.executable.value_or(executable_));
    cmd.args = {"--mode", "rpc"};
    cmd.current_dir = config.cwd;
    cmd.env.assign(config.env.begin(), config.env.end());
    if (resume) { cmd.args.emplace_back("--resume"); cmd.args.emplace_back(*resume); }
    if (config.model) { cmd.args.emplace_back("--model"); cmd.args.push_back(*config.model); }
    if (config.effort) { cmd.args.emplace_back("--thinking"); cmd.args.push_back(*config.effort); }
    return cmd;
}

Awaitable<Result<std::pair<std::unique_ptr<Omp2Session>, std::optional<std::string>>>> Omp2Adapter::launch(
    const SessionConfig& config, std::shared_ptr<PatchSink> sink, Omp2Mapper mapper,
    std::optional<std::string_view> resume) const {
    auto cmd = command(config, resume);
    if (!cmd) co_return std::unexpected(std::move(cmd).error());
    auto shared = std::make_shared<Shared>(std::move(mapper), std::move(sink));
    auto executor = co_await asio::this_coro::executor;
    auto rpc = kit::StdioRpc<Frames>::spawn(executor, *cmd, Frames{}, shared);
    if (!rpc) co_return std::unexpected(rpc_error(std::move(rpc).error()));
    auto session = std::unique_ptr<Omp2Session>(new Omp2Session(std::make_unique<Omp2Session::Impl>(shared, std::move(*rpc))));
    Value subscription_params(Object{{"enabled", true}});
    auto subscribed = co_await session->impl_->rpc.request_as(
        Value(session->impl_->rpc.next_id()), "set_dom_subscription", std::move(subscription_params));
    if (!subscribed) {
        AdapterError error = [&] {
            if (const auto* refused = subscribed.error().get<RpcError::Command>())
                return AdapterError(AdapterError::Version{HARNESS,
                    refused->message.empty() ? "`set_dom_subscription` was refused" : refused->message});
            return rpc_error(std::move(subscribed).error());
        }();
        (void)co_await session->shutdown();
        co_return std::unexpected(std::move(error));
    }
    Value empty_state_params(Object{});
    auto state = co_await session->call("get_state", std::move(empty_state_params));
    if (!state) {
        auto error = std::move(state).error();
        (void)co_await session->shutdown();
        co_return std::unexpected(std::move(error));
    }
    std::optional<std::string> native;
    if (const Value* id = state->find("sessionId"); id && id->as_string()) native = *id->as_string();
    std::pair<std::unique_ptr<Omp2Session>, std::optional<std::string>> launched{std::move(session), std::move(native)};
    co_return std::move(launched);
}

Awaitable<Availability> Omp2Adapter::availability() const {
    auto found = Availability::probe(executable_, {"--version"});
    if (!found.is_ready() || !found.executable) co_return found;
    std::error_code ec;
    auto cwd = std::filesystem::current_path(ec);
    if (ec) cwd = "/";
    SessionConfig config;
    config.cwd = std::move(cwd);
    config.executable = found.executable;
    config.session_id = "availability-probe";
    auto probe = [this, config = std::move(config)]() -> Awaitable<Result<>> {
        auto sink = std::make_shared<MemorySink>();
        auto root = kit::insert_root(DomWriter(*sink), HARNESS, config);
        if (!root) co_return std::unexpected(std::move(root).error());
        auto launched = co_await launch(config, sink, Omp2Mapper(*root), std::nullopt);
        if (!launched) co_return std::unexpected(std::move(launched).error());
        co_return co_await launched->first->shutdown();
    };
    auto timeout = []() -> Awaitable<bool> {
        asio::steady_timer timer(co_await asio::this_coro::executor, Availability::PROBE_TIMEOUT);
        co_await timer.async_wait(asio::use_awaitable);
        co_return true;
    };
    using namespace asio::experimental::awaitable_operators;
    auto outcome = co_await (probe() || timeout());
    const std::string shown = found.executable->string();
    if (std::holds_alternative<bool>(outcome))
        co_return Availability::unavailable(AvailabilityStatus::Failed, shown + " didn't answer in RPC mode");
    auto result = std::move(std::get<Result<>>(outcome));
    if (result) co_return found;
    if (const auto* version = result.error().get<AdapterError::Version>()) {
        auto incompatible = Availability::unavailable(AvailabilityStatus::Incompatible,
            shown + " doesn't stream omp2's DOM: " + version->detail);
        incompatible.executable = found.executable;
        incompatible.version = found.version;
        co_return incompatible;
    }
    co_return Availability::unavailable(AvailabilityStatus::Failed, shown + ": " + result.error().message());
}

Capabilities Omp2Adapter::capabilities() const {
    constexpr std::array features{Feature::Compaction, Feature::Subagents, Feature::OmpTools, Feature::AdvisorRules};
    constexpr std::array decisions{DecisionKind::Approval, DecisionKind::Question};
    return Capabilities::builder().schema(SCHEMA_VERSION).features(features).model_switch(ModelSwitch::PerTurn)
        .decisions(decisions).effort().build();
}

Awaitable<Result<SessionPtr>> Omp2Adapter::start(SessionConfig config, std::shared_ptr<PatchSink> sink) const {
    if (auto cmd = command(config, std::nullopt); !cmd) co_return std::unexpected(std::move(cmd).error());
    DomWriter w(*sink);
    auto root = kit::insert_root(w, HARNESS, config);
    if (!root) co_return std::unexpected(std::move(root).error());
    auto launched = co_await launch(config, sink, Omp2Mapper(*root), std::nullopt);
    if (!launched) co_return std::unexpected(std::move(launched).error());
    if (launched->second) {
        auto set = w.set(*root, attr::NATIVE_ID, *launched->second);
        if (!set) co_return std::unexpected(commit_error(std::move(set).error()));
    }
    co_return SessionPtr(std::move(launched->first));
}

Awaitable<Result<SessionPtr>> Omp2Adapter::resume(SessionConfig config, const Dom& dom,
                                                   std::shared_ptr<PatchSink> sink) const {
    const auto* root = dom.root();
    auto native = root ? root->str_attr(attr::NATIVE_ID) : std::nullopt;
    if (!native) co_return std::unexpected(AdapterError(AdapterError::Format{"missing omp2 native session id"}));
    auto launched = co_await launch(config, std::move(sink), Omp2Mapper::from_dom(dom), *native);
    if (!launched) co_return std::unexpected(std::move(launched).error());
    co_return SessionPtr(std::move(launched->first));
}

void register_factory() {
    registry::register_build(HARNESS, registry::HarnessBuild{
        [](const registry::HarnessEnv&, std::optional<std::filesystem::path> exe) -> std::shared_ptr<Adapter> {
            return std::make_shared<Omp2Adapter>(exe.value_or(std::filesystem::path(EXECUTABLE)));
        },
        {}});
}

} // namespace loa::harness::omp2
