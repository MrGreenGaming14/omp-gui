#include "loa/sandbox/worker.hpp"

#include <asio.hpp>
#include <asio/experimental/awaitable_operators.hpp>
#include <asio/experimental/concurrent_channel.hpp>

#include <format>
#include <mutex>

#include "args.hpp"
#include "loa/contracts/schema.hpp"
#include "loa/sandbox/tool.hpp"
#include "process.hpp"

namespace loa::sandbox {

namespace s = contracts::schema;
using contracts::Awaitable;
using contracts::ToolOutput;
using contracts::ToolResult;

namespace {

// Most bytes of output returned to the model per stream.
constexpr std::size_t OUTPUT_LIMIT = 64 * 1024;

// Instructions every worker gets ahead of its task.
constexpr std::string_view PREAMBLE =
    "You are working inside a sandbox. Use sandbox_bash, sandbox_read, sandbox_write and sandbox_list for every "
    "command and file operation; paths are relative to the workspace root. End with a short report of what you did.";

using Approvals = asio::experimental::concurrent_channel<void(asio::error_code, core::NodeId)>;

// Forwards commits to the journal sink and keeps a copy of the DOM, so the worker's final
// message can be read whatever the journal is. Approval requests the worker's harness raises go
// to `approvals`: nobody watches a worker, and its tools only reach the sandbox the user already
// approved with the `sandbox_task` call.
class Mirror final : public core::PatchSink {
  public:
    Mirror(std::shared_ptr<core::PatchSink> inner, std::shared_ptr<Approvals> approvals)
        : inner_(std::move(inner)), approvals_(std::move(approvals)) {}

    std::expected<core::Patch, core::CommitError> commit_at(core::Draft draft, std::int64_t ts) override {
        // Held across the commit so patches reach the mirror in sequence.
        std::scoped_lock lock(mutex_);
        auto patch = inner_->commit_at(std::move(draft), ts);
        if (!patch) return patch;
        if (auto applied = dom_.apply(*patch); !applied) return std::unexpected(core::commit_error::Apply{applied.error()});
        const auto* insert = std::get_if<core::op::Insert>(&patch->op);
        const auto* set = std::get_if<core::op::Set>(&patch->op);
        const bool touches_status = (insert && insert->kind == core::ElementKind::Decision) || (set && set->name == core::attr::STATUS);
        if (touches_status) {
            const auto* node = dom_.get(patch->node);
            if (node && node->kind == core::ElementKind::Decision &&
                node->str_attr(core::attr::KIND) == core::as_str(core::DecisionKind::Approval) &&
                node->str_attr(core::attr::STATUS) == core::as_str(core::DecisionStatus::Waiting)) {
                (void)approvals_->try_send(asio::error_code{}, patch->node);
            }
        }
        return patch;
    }

    std::optional<std::string> final() const {
        std::scoped_lock lock(mutex_);
        return final_message(dom_);
    }

  private:
    std::shared_ptr<core::PatchSink> inner_;
    std::shared_ptr<Approvals> approvals_;
    mutable std::mutex mutex_;
    core::Dom dom_;
};

loa::Error adapter_error(const contracts::AdapterError& e) { return loa::Error(ErrorCode::unknown, e.message()); }

} // namespace

std::optional<std::string> final_message(const core::Dom& dom) {
    std::optional<std::string> last;
    for (const auto& n : dom.iter()) {
        if (n.kind == core::ElementKind::Message && n.str_attr(core::attr::ROLE) == core::as_str(core::Role::Assistant) &&
            n.str_attr(core::attr::KIND) != core::as_str(core::MessageKind::Thinking)) {
            last = n.text;
        }
    }
    return last;
}

// ---- tool arguments ----------------------------------------------------------------------------

std::string BashArgs::schema() {
    return s::Root("BashArgs", s::Object()
                                   .property("command", s::describe(s::string(), "Script for /bin/sh -c (cmd /C on Windows)"))
                                   .property("cwd", s::describe(s::nullable(s::string()), "Directory relative to the workspace"), false)
                                   .property("timeout_ms", s::nullable(s::unsigned_integer("uint64")), false)
                                   .deny_unknown_fields())
        .dump();
}

std::expected<BashArgs, std::string> BashArgs::from_json(const contracts::Json& json) {
    if (auto ok = detail::fields(json, "struct BashArgs", {"command", "cwd", "timeout_ms"}); !ok) return std::unexpected(ok.error());
    auto command = contracts::args::string(json, "command");
    if (!command) return std::unexpected(command.error());
    auto cwd = contracts::args::optional_string(json, "cwd");
    if (!cwd) return std::unexpected(cwd.error());
    auto timeout = contracts::args::optional_u64(json, "timeout_ms");
    if (!timeout) return std::unexpected(timeout.error());
    return BashArgs{std::move(*command), std::move(*cwd), *timeout};
}

std::string ReadArgs::schema() {
    return s::Root("ReadArgs", s::Object()
                                   .property("path", s::string())
                                   .property("limit", s::describe(s::nullable(s::unsigned_integer("uint64")), "Most bytes to read"), false)
                                   .deny_unknown_fields())
        .dump();
}

std::expected<ReadArgs, std::string> ReadArgs::from_json(const contracts::Json& json) {
    if (auto ok = detail::fields(json, "struct ReadArgs", {"path", "limit"}); !ok) return std::unexpected(ok.error());
    auto path = contracts::args::string(json, "path");
    if (!path) return std::unexpected(path.error());
    auto limit = contracts::args::optional_u64(json, "limit");
    if (!limit) return std::unexpected(limit.error());
    return ReadArgs{std::move(*path), *limit};
}

std::string WriteArgs::schema() {
    return s::Root("WriteArgs", s::Object().property("path", s::string()).property("content", s::string()).deny_unknown_fields()).dump();
}

std::expected<WriteArgs, std::string> WriteArgs::from_json(const contracts::Json& json) {
    if (auto ok = detail::fields(json, "struct WriteArgs", {"path", "content"}); !ok) return std::unexpected(ok.error());
    auto path = contracts::args::string(json, "path");
    if (!path) return std::unexpected(path.error());
    auto content = contracts::args::string(json, "content");
    if (!content) return std::unexpected(content.error());
    return WriteArgs{std::move(*path), std::move(*content)};
}

std::string ListArgs::schema() {
    return s::Root("ListArgs", s::Object()
                                   .property("path", s::describe(s::nullable(s::string()), "Defaults to the workspace root"), false)
                                   .deny_unknown_fields())
        .dump();
}

std::expected<ListArgs, std::string> ListArgs::from_json(const contracts::Json& json) {
    if (auto ok = detail::fields(json, "struct ListArgs", {"path"}); !ok) return std::unexpected(ok.error());
    auto path = contracts::args::optional_string(json, "path");
    if (!path) return std::unexpected(path.error());
    return ListArgs{std::move(*path)};
}

// ---- tools -------------------------------------------------------------------------------------

std::string_view SandboxBash::description() const {
    return "Runs a shell command inside the sandbox, in the workspace. Returns stdout, stderr and the exit code.";
}

namespace {
// What `sandbox_bash` tells the model: each non-empty stream (cut at `OUTPUT_LIMIT`), then how it ended.
std::string bash_report(const ExecOutput& out) {
    std::string text;
    const std::pair<std::string_view, const std::string*> streams[] = {{"stdout", &out.stdout_data}, {"stderr", &out.stderr_data}};
    for (const auto& [name, bytes] : streams) {
        if (bytes->empty()) continue;
        const auto cut = std::min(bytes->size(), OUTPUT_LIMIT);
        text += std::format("<{}>\n{}\n</{}>\n", name, detail::lossy(std::string_view(*bytes).substr(0, cut)), name);
        if (cut < bytes->size()) text += std::format("[{} cut at {} of {} bytes]\n", name, OUTPUT_LIMIT, bytes->size());
    }
    const auto& status = out.status;
    if (status.timed_out) text += "killed: timed out";
    else if (status.code) text += std::format("exit code: {}", *status.code);
    else if (status.signal) text += std::format("killed by signal {}", *status.signal);
    else text += "exit status unknown";
    return text;
}
} // namespace

Awaitable<ToolResult> SandboxBash::run(const contracts::CallCtx&, BashArgs args) const {
    auto exec = proto::ExecRequest::shell(args.command);
    exec.cwd = std::move(args.cwd);
    exec.timeout_ms = args.timeout_ms;
    auto out = co_await client_->exec(std::move(exec));
    if (!out) co_return std::unexpected(out.error().message());
    co_return ToolOutput::text(bash_report(*out));
}

std::string_view SandboxRead::description() const { return "Reads a file in the sandbox workspace."; }

Awaitable<ToolResult> SandboxRead::run(const contracts::CallCtx&, ReadArgs args) const {
    const auto script = args.limit ? std::format("head -c {} -- \"$1\"", *args.limit) : std::string("cat -- \"$1\"");
    auto exec = proto::ExecRequest::shell(script, std::vector<std::string>{args.path});
    auto data = co_await client_->run(std::move(exec));
    if (!data) co_return std::unexpected(data.error().message());
    co_return ToolOutput::text(detail::lossy(*data));
}

std::string_view SandboxWrite::description() const { return "Writes a file in the sandbox workspace, creating parent directories."; }

Awaitable<ToolResult> SandboxWrite::run(const contracts::CallCtx&, WriteArgs args) const {
    const auto len = args.content.size();
    auto exec = proto::ExecRequest::shell(R"sh(mkdir -p -- "$(dirname -- "$1")" && cat > "$1")sh", {args.path});
    exec.stdin_data = std::move(args.content);
    auto done = co_await client_->run(std::move(exec));
    if (!done) co_return std::unexpected(done.error().message());
    co_return ToolOutput::text(std::format("wrote {} bytes to {}", len, args.path));
}

std::string_view SandboxList::description() const { return "Lists a directory in the sandbox workspace."; }

Awaitable<ToolResult> SandboxList::run(const contracts::CallCtx&, ListArgs args) const {
    auto exec = proto::ExecRequest::shell(R"(ls -Al -- "$1")", std::vector<std::string>{args.path.value_or(".")});
    auto listing = co_await client_->run(std::move(exec));
    if (!listing) co_return std::unexpected(listing.error().message());
    co_return ToolOutput::text(detail::lossy(*listing));
}

std::shared_ptr<contracts::ToolSet> sandbox_tools(const std::shared_ptr<const StubClient>& client) {
    auto tools = std::make_shared<contracts::ToolSet>();
    tools->add(std::make_shared<SandboxBash>(client)).add(std::make_shared<SandboxRead>(client));
    tools->add(std::make_shared<SandboxWrite>(client)).add(std::make_shared<SandboxList>(client));
    return tools;
}

// ---- the worker --------------------------------------------------------------------------------

Awaitable<Result<std::optional<std::string>>> run_worker(const contracts::Adapter& adapter, const contracts::SessionConfig& templ,
                                                        const WorkerJournal& journal, std::shared_ptr<contracts::ToolSet> tools,
                                                        std::string session_id, std::filesystem::path harness_cwd,
                                                        SandboxTaskArgs args) {
    auto config = templ;
    config.session_id = session_id;
    config.cwd = std::move(harness_cwd);
    if (args.model_role) config.model_role = args.model_role;
    const auto extra = args.agent ? args.agent : templ.context;
    config.context = extra ? std::format("{}\n\n{}", PREAMBLE, *extra) : std::string(PREAMBLE);
    config.mcp_servers.clear();
    config.host_tools = tools->specs();
    config.host_tool_handler = tools;
    for (const auto tool : HOST_TOOLS_TO_DISABLE) {
        if (std::ranges::find(config.disabled_tools, tool) == config.disabled_tools.end()) config.disabled_tools.emplace_back(tool);
    }

    auto executor = co_await asio::this_coro::executor;
    auto approvals = std::make_shared<Approvals>(executor, 1024);
    auto inner = journal.open(session_id);
    if (!inner) co_return std::unexpected(inner.error());
    auto mirror = std::make_shared<Mirror>(std::move(*inner), approvals);
    auto started = co_await adapter.start(std::move(config), mirror);
    if (!started) co_return std::unexpected(adapter_error(started.error()));
    const std::shared_ptr<contracts::HarnessSession> session = std::move(*started);
    const auto by = templ.node;
    auto task = std::move(args.task);

    auto run = [&]() -> Awaitable<contracts::Result<>> {
        if (auto sent = co_await session->send(contracts::Prompt{task, {}, by}); !sent) co_return sent;
        co_return co_await session->wait_idle();
    };
    // The mirror keeps the sender, so this only ends on a failed answer (or when `run` wins).
    auto allow = [&]() -> Awaitable<contracts::Result<>> {
        for (;;) {
            auto [ec, decision] = co_await approvals->async_receive(asio::as_tuple(asio::use_awaitable));
            if (ec) co_return contracts::Result<>{};
            contracts::Answer answer{{std::string(core::answer::ALLOW_ONCE)}, std::nullopt, by};
            if (auto answered = co_await session->answer(decision, std::move(answer)); !answered) co_return answered;
        }
    };
    using namespace asio::experimental::awaitable_operators;
    auto first = co_await (run() || allow());
    auto result = std::visit([](auto r) { return r; }, std::move(first));
    auto shutdown = co_await session->shutdown();
    if (!result) co_return std::unexpected(adapter_error(result.error()));
    if (!shutdown) co_return std::unexpected(adapter_error(shutdown.error()));
    co_return mirror->final();
}

} // namespace loa::sandbox
