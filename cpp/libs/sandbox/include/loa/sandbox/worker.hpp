#pragma once

// The worker session of a `sandbox_task` and the tools it works with (Rust
// `loa_sandbox::worker`). The worker runs on the host through a harness `Adapter`, so its model
// calls, API keys and journal never leave the host. Its harness gets no file or exec tools of
// its own and no MCP servers; the only way it can act is through the `sandbox_*` tools here,
// each of which is one request to the stub inside the sandbox.

#include <array>
#include <filesystem>
#include <memory>
#include <optional>
#include <string>
#include <string_view>

#include "loa/contracts/tools.hpp"
#include "loa/core/dom.hpp"
#include "loa/core/sink.hpp"
#include "loa/sandbox/client.hpp"

namespace loa::sandbox {

struct SandboxTaskArgs;

// Tools of the harness itself that act on the host. They are switched off for a worker so
// everything it does goes through the sandbox.
inline constexpr std::array<std::string_view, 19> HOST_TOOLS_TO_DISABLE{
    "bash", "read", "write", "edit", "grep", "find", "glob", "ast_grep", "ast_edit", "eval",
    "lsp",  "debug", "browser", "computer", "task", "fetch", "notebook", "ssh", "ask"};

// Where a worker's session is journaled. `loa-qt` backs it with the session library, so the
// worker's journal lives on the host like any other.
class WorkerJournal {
  public:
    virtual ~WorkerJournal() = default;
    // The sink of a new, empty session.
    [[nodiscard]] virtual Result<std::shared_ptr<core::PatchSink>> open(std::string_view session_id) const = 0;
};

// The text of the last assistant message that isn't thinking.
[[nodiscard]] std::optional<std::string> final_message(const core::Dom& dom);

struct BashArgs {
    // Script for /bin/sh -c (cmd /C on Windows).
    std::string command;
    // Directory relative to the workspace.
    std::optional<std::string> cwd;
    std::optional<std::uint64_t> timeout_ms;
    [[nodiscard]] static std::string schema();
    [[nodiscard]] static std::expected<BashArgs, std::string> from_json(const contracts::Json& json);
};

struct ReadArgs {
    std::string path;
    // Most bytes to read.
    std::optional<std::uint64_t> limit;
    [[nodiscard]] static std::string schema();
    [[nodiscard]] static std::expected<ReadArgs, std::string> from_json(const contracts::Json& json);
};

struct WriteArgs {
    std::string path;
    std::string content;
    [[nodiscard]] static std::string schema();
    [[nodiscard]] static std::expected<WriteArgs, std::string> from_json(const contracts::Json& json);
};

struct ListArgs {
    // Defaults to the workspace root.
    std::optional<std::string> path;
    [[nodiscard]] static std::string schema();
    [[nodiscard]] static std::expected<ListArgs, std::string> from_json(const contracts::Json& json);
};

// `sandbox_bash`: one shell command in the stub.
class SandboxBash final : public contracts::Tool<BashArgs> {
  public:
    explicit SandboxBash(std::shared_ptr<const StubClient> client) : client_(std::move(client)) {}
    [[nodiscard]] std::string_view name() const override { return "sandbox_bash"; }
    [[nodiscard]] std::string_view description() const override;
    [[nodiscard]] contracts::Awaitable<contracts::ToolResult> run(const contracts::CallCtx& ctx, BashArgs args) const override;

  private:
    std::shared_ptr<const StubClient> client_;
};

class SandboxRead final : public contracts::Tool<ReadArgs> {
  public:
    explicit SandboxRead(std::shared_ptr<const StubClient> client) : client_(std::move(client)) {}
    [[nodiscard]] std::string_view name() const override { return "sandbox_read"; }
    [[nodiscard]] std::string_view description() const override;
    [[nodiscard]] contracts::Awaitable<contracts::ToolResult> run(const contracts::CallCtx& ctx, ReadArgs args) const override;

  private:
    std::shared_ptr<const StubClient> client_;
};

class SandboxWrite final : public contracts::Tool<WriteArgs> {
  public:
    explicit SandboxWrite(std::shared_ptr<const StubClient> client) : client_(std::move(client)) {}
    [[nodiscard]] std::string_view name() const override { return "sandbox_write"; }
    [[nodiscard]] std::string_view description() const override;
    [[nodiscard]] contracts::Awaitable<contracts::ToolResult> run(const contracts::CallCtx& ctx, WriteArgs args) const override;

  private:
    std::shared_ptr<const StubClient> client_;
};

class SandboxList final : public contracts::Tool<ListArgs> {
  public:
    explicit SandboxList(std::shared_ptr<const StubClient> client) : client_(std::move(client)) {}
    [[nodiscard]] std::string_view name() const override { return "sandbox_list"; }
    [[nodiscard]] std::string_view description() const override;
    [[nodiscard]] contracts::Awaitable<contracts::ToolResult> run(const contracts::CallCtx& ctx, ListArgs args) const override;

  private:
    std::shared_ptr<const StubClient> client_;
};

// The `sandbox_*` tools, each answered by the stub behind `client`.
[[nodiscard]] std::shared_ptr<contracts::ToolSet> sandbox_tools(const std::shared_ptr<const StubClient>& client);

// Runs the worker session `session_id` to the end of its first turn and returns its final
// message. `templ` carries the host side settings; the worker keeps them, gets `tools` in place
// of any host tools and MCP servers, and has the harness's own host tools switched off. Its
// harness runs in `harness_cwd`, an empty directory. Pending approvals are answered
// `allow_once` by the worker itself.
[[nodiscard]] contracts::Awaitable<Result<std::optional<std::string>>> run_worker(
    const contracts::Adapter& adapter, const contracts::SessionConfig& templ, const WorkerJournal& journal,
    std::shared_ptr<contracts::ToolSet> tools, std::string session_id, std::filesystem::path harness_cwd,
    SandboxTaskArgs args);

} // namespace loa::sandbox
