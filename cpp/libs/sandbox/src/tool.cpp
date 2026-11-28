#include "loa/sandbox/tool.hpp"

#include <unistd.h>

#include <asio.hpp>

#include <atomic>
#include <format>

#include "args.hpp"
#include "loa/contracts/schema.hpp"
#include "loa/runtime/tempfile.hpp"
#include "loa/runtime/time.hpp"
#include "loa/sandbox/client.hpp"
#include "loa/sandbox/isolation.hpp"
#include "loa/sandbox/sync.hpp"
#include "loa/sandbox/worker.hpp"
#include "offload.hpp"
#include "process.hpp"

namespace loa::sandbox {

namespace s = contracts::schema;
namespace json = runtime::json;
using contracts::Awaitable;

namespace {

// Run numbers, unique in this process, so containers, remote directories and worker sessions
// of concurrent sessions never collide.
std::atomic<std::uint64_t> NEXT_RUN{1};

// Rust `{:?}`-free variant list for serde's `unknown variant` error.
std::string kind_list() {
    std::string out;
    for (const auto kind : SANDBOX_KINDS) out += std::format("{}`{}`", out.empty() ? "" : ", ", as_str(kind));
    return out;
}

constexpr std::size_t EVENT_CAPACITY = 256;

} // namespace

// ---- arguments -----------------------------------------------------------------------------------

json::Value SandboxOptions::schema() {
    return s::Object()
        .description("Per call options from the `sandbox_task` arguments. Each provider reads the ones that apply to it and "
                     "falls back to its own configuration.")
        .property("image", s::describe(s::nullable(s::string()), "Container image (docker)."), false)
        .property("host", s::describe(s::nullable(s::string()), "SSH host alias or `user@host` (ssh)."), false)
        .property("peer", s::describe(s::nullable(s::string()), "Node key or name of the peer (peer)."), false)
        .property("network", s::describe(s::nullable(s::boolean()), "Allow network access from inside the sandbox (docker, firecracker, vm)."), false)
        .property("memory_mib", s::nullable(s::unsigned_integer("uint32")), false)
        .property("cpus", s::nullable(s::unsigned_integer("uint32")), false)
        .deny_unknown_fields()
        .to_json();
}

std::expected<SandboxOptions, std::string> SandboxOptions::from_json(const json::Value& value) {
    if (auto ok = detail::fields(value, "struct SandboxOptions", {"image", "host", "peer", "network", "memory_mib", "cpus"}); !ok)
        return std::unexpected(ok.error());
    SandboxOptions o;
    auto image = contracts::args::optional_string(value, "image");
    if (!image) return std::unexpected(image.error());
    auto host = contracts::args::optional_string(value, "host");
    if (!host) return std::unexpected(host.error());
    auto peer = contracts::args::optional_string(value, "peer");
    if (!peer) return std::unexpected(peer.error());
    auto network = contracts::args::optional_bool(value, "network");
    if (!network) return std::unexpected(network.error());
    auto memory = detail::optional_u32(value, "memory_mib");
    if (!memory) return std::unexpected(memory.error());
    auto cpus = detail::optional_u32(value, "cpus");
    if (!cpus) return std::unexpected(cpus.error());
    return SandboxOptions{std::move(*image), std::move(*host), std::move(*peer), *network, *memory, *cpus};
}

std::string SandboxTaskArgs::schema() {
    std::vector<std::string_view> kinds;
    for (const auto kind : SANDBOX_KINDS) kinds.push_back(as_str(kind));
    auto object = s::Object()
                      .description("One sandbox task's arguments.")
                      .property("task", s::describe(s::string(), "Complete instructions for the worker"))
                      .property("kind", s::string_enum(kinds))
                      .property("options", s::ref("SandboxOptions"), false)
                      .property("isolation", s::describe(s::with_default(s::boolean(), false), "Work on an isolated checkout and return a patch"), false)
                      .property("agent", s::describe(s::nullable(s::string()), "Instructions of the agent the worker plays"), false)
                      .property("model_role", s::nullable(s::string()), false)
                      .deny_unknown_fields();
    return s::Root("SandboxTaskArgs", std::move(object)).define("SandboxOptions", SandboxOptions::schema()).dump();
}

std::expected<SandboxTaskArgs, std::string> SandboxTaskArgs::from_json(const contracts::Json& value) {
    if (auto ok = detail::fields(value, "struct SandboxTaskArgs", {"task", "kind", "options", "isolation", "agent", "model_role"}); !ok)
        return std::unexpected(ok.error());
    SandboxTaskArgs args;
    auto task = contracts::args::string(value, "task");
    if (!task) return std::unexpected(task.error());
    args.task = std::move(*task);
    auto kind = contracts::args::string(value, "kind");
    if (!kind) return std::unexpected(kind.error());
    auto parsed = parse_kind(*kind);
    if (!parsed) return std::unexpected(std::format("unknown variant `{}`, expected one of {}", *kind, kind_list()));
    args.kind = *parsed;
    if (const auto* options = value.find("options"); options && !options->is_null()) {
        auto o = SandboxOptions::from_json(*options);
        if (!o) return std::unexpected(o.error());
        args.options = std::move(*o);
    }
    auto isolation = contracts::args::optional_bool(value, "isolation");
    if (!isolation) return std::unexpected(isolation.error());
    args.isolation = isolation->value_or(false);
    auto agent = contracts::args::optional_string(value, "agent");
    if (!agent) return std::unexpected(agent.error());
    args.agent = std::move(*agent);
    auto role = contracts::args::optional_string(value, "model_role");
    if (!role) return std::unexpected(role.error());
    args.model_role = std::move(*role);
    return args;
}

json::Value SandboxTaskResult::to_json() const {
    const auto opt = [](const std::optional<std::string>& v) { return v ? json::Value(*v) : json::Value(nullptr); };
    return json::Object{{"session_id", session_id}, {"final_message", opt(final_message)}, {"patch", opt(patch)}};
}

std::expected<SandboxTaskResult, std::string> SandboxTaskResult::from_json(const json::Value& value) {
    if (auto ok = contracts::args::object(value, "struct SandboxTaskResult"); !ok) return std::unexpected(ok.error());
    auto id = contracts::args::string(value, "session_id");
    if (!id) return std::unexpected(id.error());
    auto message = contracts::args::optional_string(value, "final_message");
    if (!message) return std::unexpected(message.error());
    auto patch = contracts::args::optional_string(value, "patch");
    if (!patch) return std::unexpected(patch.error());
    return SandboxTaskResult{std::move(*id), std::move(*message), std::move(*patch)};
}

// ---- events ------------------------------------------------------------------------------------

std::optional<SandboxEvent> SandboxEvents::Receiver::recv(std::chrono::milliseconds timeout) {
    std::unique_lock lock(mutex_);
    if (!ready_.wait_for(lock, timeout, [&] { return !queue_.empty(); })) return std::nullopt;
    auto event = std::move(queue_.front());
    queue_.pop_front();
    return event;
}

std::optional<SandboxEvent> SandboxEvents::Receiver::try_recv() { return recv(std::chrono::milliseconds(0)); }

void SandboxEvents::send(const SandboxEvent& event) {
    std::scoped_lock lock(mutex_);
    std::erase_if(receivers_, [](const auto& weak) { return weak.expired(); });
    for (const auto& weak : receivers_) {
        auto receiver = weak.lock();
        if (!receiver) continue;
        {
            std::scoped_lock queue_lock(receiver->mutex_);
            if (receiver->queue_.size() == EVENT_CAPACITY) receiver->queue_.pop_front();
            receiver->queue_.push_back(event);
        }
        receiver->ready_.notify_all();
    }
}

std::shared_ptr<SandboxEvents::Receiver> SandboxEvents::subscribe() {
    auto receiver = std::make_shared<Receiver>();
    std::scoped_lock lock(mutex_);
    receivers_.push_back(receiver);
    return receiver;
}

// ---- the tool ----------------------------------------------------------------------------------

SandboxTask::SandboxTask(std::shared_ptr<const contracts::Adapter> adapter, contracts::SessionConfig templ,
                         std::shared_ptr<const WorkerJournal> journal, std::vector<std::shared_ptr<SandboxProvider>> providers,
                         std::shared_ptr<SandboxEvents> events)
    : adapter_(std::move(adapter)), template_(std::move(templ)), journal_(std::move(journal)), events_(std::move(events)) {
    for (auto& provider : providers) providers_.insert_or_assign(provider->kind(), std::move(provider));
    std::string available;
    for (const auto& [kind, provider] : providers_) {
        available += std::format("{}- {}: {}", available.empty() ? "" : "\n", as_str(kind), provider->describe());
    }
    description_ = std::format(
        "Run a task as a worker agent whose commands and file operations happen inside a sandbox; its model calls stay on "
        "this host. Returns the worker's final message. With isolation (the workspace must be a git repository) the worker "
        "works on an isolated checkout and the result carries `patch`, a unified diff of its changes, while the workspace "
        "stays untouched. Without isolation the worker changes the workspace itself.\nSandboxes available here:\n{}",
        available);
}

std::vector<SandboxKind> SandboxTask::kinds() const {
    std::vector<SandboxKind> out;
    for (const auto& [kind, _] : providers_) out.push_back(kind);
    return out;
}

Awaitable<Result<SandboxTaskResult>> SandboxTask::execute(SandboxTaskArgs args) const {
    const auto blank = args.task.find_first_not_of(" \t\n\r\f\v") == std::string::npos;
    if (blank) co_return std::unexpected(error::args("task must not be empty"));
    const auto found = providers_.find(args.kind);
    if (found == providers_.end()) co_return std::unexpected(error::no_provider(as_str(args.kind)));
    auto provider = found->second;
    const auto run = NEXT_RUN.fetch_add(1, std::memory_order_relaxed);
    // With the start time, ids stay unique across app restarts too, where run numbers start
    // over but earlier worker journals remain.
    const auto started = runtime::time::now_ms();
    auto worker = std::format("{}-sandbox-{:x}-{}", template_.session_id, started, run);
    events_->send(event::Started{SandboxRun{run, template_.session_id, worker, args.kind, args.task, args.isolation, template_.cwd}});
    // Names containers and remote directories: unique across processes and machines sharing a
    // Docker daemon or SSH host.
    auto name = std::format("loa-sbx-{}-{:x}-{}", ::getpid(), started, run);
    auto result = co_await work(std::move(provider), std::move(name), std::move(worker), std::move(args));
    if (result) events_->send(event::Finished{run, result->final_message, result->patch});
    else events_->send(event::Failed{run, result.error().message()});
    co_return result;
}

Awaitable<Result<SandboxTaskResult>> SandboxTask::work(std::shared_ptr<SandboxProvider> provider, std::string name,
                                                      std::string worker, SandboxTaskArgs args) const {
    std::optional<IsolatedCheckout> checkout;
    if (args.isolation) {
        auto created = co_await detail::offload([cwd = template_.cwd] { return IsolatedCheckout::create(cwd); });
        if (!created) co_return std::unexpected(created.error());
        checkout.emplace(std::move(*created));
    }
    const auto workspace = checkout ? checkout->path() : template_.cwd;
    // Never inherit the host adapter's env (which can contain API keys).
    const SandboxSpec spec{std::move(name), workspace, {}, args.options};
    auto started = co_await provider->start(spec);
    if (!started) co_return std::unexpected(started.error());
    auto sandbox = std::move(*started);
    const bool copied = sandbox->workspace_mode() == WorkspaceMode::Copied;

    auto inner = [&]() -> Awaitable<Result<SandboxTaskResult>> {
        auto channel = sandbox->take_channel();
        if (!channel) co_return std::unexpected(error::disconnected());
        auto connected = co_await StubClient::connect(std::move(*channel));
        if (!connected) co_return std::unexpected(connected.error());
        std::shared_ptr<const StubClient> client = std::move(*connected);
        if (copied) {
            auto in = co_await detail::offload([&] { return sync::copy_in(*client, workspace); });
            if (!in) co_return std::unexpected(in.error());
        }
        // The harness needs a host cwd that shows nothing of the host or the checkout.
        auto harness_dir = runtime::TempDir::create();
        if (!harness_dir) co_return std::unexpected(harness_dir.error());
        auto final_message = co_await run_worker(*adapter_, template_, *journal_, sandbox_tools(client), worker,
                                                 harness_dir->path(), std::move(args));
        if (!final_message) co_return std::unexpected(final_message.error());
        if (copied) {
            auto out = co_await detail::offload([&] { return sync::copy_out(*client, workspace); });
            if (!out) co_return std::unexpected(out.error());
        }
        if (auto down = co_await client->shutdown(); !down) co_return std::unexpected(down.error());
        std::optional<std::string> patch;
        if (checkout) {
            auto bytes = co_await detail::offload([&] { return checkout->patch(); });
            if (!bytes) co_return std::unexpected(bytes.error());
            patch = detail::lossy(*bytes);
        }
        co_return SandboxTaskResult{worker, std::move(*final_message), std::move(patch)};
    };
    auto result = co_await inner();
    auto stopped = co_await sandbox->stop();
    if (!result) co_return result;
    if (!stopped) co_return std::unexpected(stopped.error());
    co_return result;
}

Awaitable<contracts::ToolResult> SandboxTask::run(const contracts::CallCtx&, SandboxTaskArgs args) const {
    auto result = co_await execute(std::move(args));
    if (!result) co_return std::unexpected(result.error().message());
    co_return contracts::ToolOutput::text(json::dump(result->to_json()));
}

} // namespace loa::sandbox
