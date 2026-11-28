#pragma once

#include <functional>
#include <memory>
#include <mutex>
#include <optional>
#include <string>
#include <string_view>
#include <utility>
#include <vector>

#include "loa/contracts/harness.hpp"
#include "loa/harness/codex/mapper.hpp"
#include "loa/harness/kit/jsonrpc.hpp"

namespace loa::harness::codex {

class CodexAdapter;

struct Open {
    enum class Kind { Start, Resume, Fork };
    Kind kind = Kind::Start;
    std::string thread_id;
    std::optional<std::string> last_turn_id;

    static Open start() { return {}; }
    static Open resume(std::string thread) { return {Kind::Resume, std::move(thread), std::nullopt}; }
    static Open fork(std::string thread, std::optional<std::string> turn = std::nullopt) {
        return {Kind::Fork, std::move(thread), std::move(turn)};
    }
};

// The Codex-specific command-line configuration and app-server request payloads.
[[nodiscard]] std::vector<std::string> mcp_overrides(const loa::contracts::SessionConfig& config);
[[nodiscard]] std::pair<std::string_view, loa::runtime::json::Value> open_request(
    const loa::contracts::SessionConfig& config, const Open& open);
[[nodiscard]] loa::runtime::json::Value turn_params(const loa::contracts::Prompt& prompt, std::string_view thread_id,
                                                     std::optional<std::string_view> model,
                                                     std::optional<std::string_view> effort,
                                                     std::optional<loa::core::SessionMode> mode);

class CodexSession final : public loa::contracts::HarnessSession {
  public:
    using Attach = std::function<loa::contracts::Result<Mapper>(std::string_view, std::optional<std::string_view>,
                                                                 loa::core::DomWriter&)>;
    [[nodiscard]] static loa::contracts::Awaitable<loa::contracts::Result<loa::contracts::SessionPtr>> open(
        const CodexAdapter& adapter, loa::contracts::SessionConfig config,
        std::shared_ptr<loa::core::PatchSink> sink, Open open, Attach attach);

    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::Result<>> send(loa::contracts::Prompt prompt) const override;
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::Result<>> interrupt() const override;
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::Result<>> answer(loa::core::NodeId decision,
                                                                             loa::contracts::Answer answer) const override;
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::Result<>> set_model(loa::contracts::ModelSelection selection) const override;
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::Result<>> set_mode(loa::core::SessionMode mode) const override;
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::Result<std::vector<loa::contracts::HarnessModel>>> models() const override;
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::Result<>> set_effort(std::string effort) const override;
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::Result<>> compact(std::optional<std::string> instructions) const override;
    [[nodiscard]] std::string_view harness() const override;
    [[nodiscard]] loa::contracts::SessionState state() const override;
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::Result<>> wait_idle() const override;
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::Result<>> shutdown() const override;

  private:
    struct Shared;
    struct TurnSettings {
        std::optional<std::string> model;
        std::optional<std::string> effort;
        std::optional<loa::core::SessionMode> mode;
    };
    CodexSession(std::shared_ptr<Shared> shared, kit::StdioRpc<kit::JsonRpc2> rpc, std::string thread_id,
                 TurnSettings settings);
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::Result<kit::Json>> request(std::string method,
                                                                                        kit::Json params) const;
    std::shared_ptr<Shared> shared_;
    kit::StdioRpc<kit::JsonRpc2> rpc_;
    std::string thread_id_;
    mutable std::mutex settings_mutex_;
    mutable TurnSettings settings_;
};

} // namespace loa::harness::codex
