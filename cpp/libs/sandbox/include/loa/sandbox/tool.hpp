#pragma once

// The app-hosted `sandbox_task` tool and the worker/provider lifecycle (Rust `loa_sandbox::tool`).

#include <chrono>
#include <condition_variable>
#include <cstdint>
#include <deque>
#include <filesystem>
#include <map>
#include <memory>
#include <mutex>
#include <optional>
#include <string>
#include <variant>
#include <vector>

#include "loa/contracts/tools.hpp"
#include "loa/sandbox/provider.hpp"

namespace loa::sandbox {

class WorkerJournal;

// Name registered with an adapter; it does not replace omp's own `task`.
inline constexpr std::string_view TASK_TOOL = "sandbox_task";

// One sandbox task's arguments.
struct SandboxTaskArgs {
    // Complete instructions for the worker.
    std::string task;
    SandboxKind kind = SandboxKind::Local;
    SandboxOptions options;
    // Work on an isolated checkout and return a patch.
    bool isolation = false;
    // Instructions of the agent the worker plays.
    std::optional<std::string> agent;
    std::optional<std::string> model_role;

    [[nodiscard]] static std::string schema();
    [[nodiscard]] static std::expected<SandboxTaskArgs, std::string> from_json(const contracts::Json& json);
};

// The worker's answer and, for an isolated run, a unified diff of its checkout against its
// initial state, to apply deliberately.
struct SandboxTaskResult {
    std::string session_id;
    std::optional<std::string> final_message;
    std::optional<std::string> patch;

    [[nodiscard]] runtime::json::Value to_json() const;
    [[nodiscard]] static std::expected<SandboxTaskResult, std::string> from_json(const runtime::json::Value& json);
    friend bool operator==(const SandboxTaskResult&, const SandboxTaskResult&) = default;
};

// A `sandbox_task` call as it starts.
struct SandboxRun {
    std::uint64_t run = 0;
    // Session that called the tool.
    std::string parent;
    // The worker's session id.
    std::string worker;
    SandboxKind kind = SandboxKind::Local;
    std::string task;
    bool isolated = false;
    // The workspace the task works on (the original, not the checkout).
    std::filesystem::path workspace;
    friend bool operator==(const SandboxRun&, const SandboxRun&) = default;
};

namespace event {
struct Started {
    SandboxRun run;
    friend bool operator==(const Started&, const Started&) = default;
};
struct Finished {
    std::uint64_t run = 0;
    std::optional<std::string> final_message;
    std::optional<std::string> patch;
    friend bool operator==(const Finished&, const Finished&) = default;
};
struct Failed {
    std::uint64_t run = 0;
    std::string error;
    friend bool operator==(const Failed&, const Failed&) = default;
};
} // namespace event

// What happens to `sandbox_task` calls, for the app to show.
using SandboxEvent = std::variant<event::Started, event::Finished, event::Failed>;

// A broadcast of `SandboxEvent`s (Rust `tokio::sync::broadcast`, capacity 256): every receiver
// sees every event sent after it subscribed; a slow one loses the oldest.
class SandboxEvents {
  public:
    class Receiver {
      public:
        // The next event, waiting up to `timeout` for one.
        [[nodiscard]] std::optional<SandboxEvent> recv(std::chrono::milliseconds timeout = std::chrono::seconds(30));
        [[nodiscard]] std::optional<SandboxEvent> try_recv();

      private:
        friend class SandboxEvents;
        std::mutex mutex_;
        std::condition_variable ready_;
        std::deque<SandboxEvent> queue_;
    };

    void send(const SandboxEvent& event);
    [[nodiscard]] std::shared_ptr<Receiver> subscribe();

  private:
    std::mutex mutex_;
    std::vector<std::weak_ptr<Receiver>> receivers_;
};

// The host-owned tool. The adapter, credentials and journal never enter a `SandboxSpec`.
class SandboxTask final : public contracts::Tool<SandboxTaskArgs> {
  public:
    // The tool for sessions like `templ`, whose runs go to `events`.
    SandboxTask(std::shared_ptr<const contracts::Adapter> adapter, contracts::SessionConfig templ,
                std::shared_ptr<const WorkerJournal> journal, std::vector<std::shared_ptr<SandboxProvider>> providers,
                std::shared_ptr<SandboxEvents> events);

    // The kinds with a provider.
    [[nodiscard]] std::vector<SandboxKind> kinds() const;

    // Runs the host worker and always stops its sandbox, including on worker errors.
    [[nodiscard]] contracts::Awaitable<Result<SandboxTaskResult>> execute(SandboxTaskArgs args) const;

    [[nodiscard]] std::string_view name() const override { return TASK_TOOL; }
    [[nodiscard]] std::string_view description() const override { return description_; }
    [[nodiscard]] contracts::Awaitable<contracts::ToolResult> run(const contracts::CallCtx& ctx, SandboxTaskArgs args) const override;

  private:
    [[nodiscard]] contracts::Awaitable<Result<SandboxTaskResult>> work(std::shared_ptr<SandboxProvider> provider, std::string name,
                                                                        std::string worker, SandboxTaskArgs args) const;

    std::shared_ptr<const contracts::Adapter> adapter_;
    contracts::SessionConfig template_;
    std::shared_ptr<const WorkerJournal> journal_;
    std::map<SandboxKind, std::shared_ptr<SandboxProvider>> providers_;
    std::shared_ptr<SandboxEvents> events_;
    std::string description_;
};

} // namespace loa::sandbox
