#pragma once

#include <chrono>
#include <filesystem>
#include <memory>
#include <string_view>

#include "loa/contracts/harness.hpp"
#include "loa/harness/omp/launch.hpp"
#include "loa/harness/omp/rpc/client.hpp"

namespace loa::harness::omp {

inline constexpr std::string_view HARNESS = "omp";

struct SessionShared;

// One running omp RPC process and its DOM mapper. Its event reader owns no process reference.
class OmpSession final : public loa::contracts::HarnessSession {
  public:
    OmpSession(rpc::RpcClient client, std::shared_ptr<SessionShared> shared,
               std::chrono::milliseconds shutdown_grace);
    ~OmpSession() override;

    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::Result<>> send(loa::contracts::Prompt prompt) const override;
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::Result<>> interrupt() const override;
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::Result<>> answer(loa::core::NodeId decision,
                                                                             loa::contracts::Answer answer) const override;
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::Result<>> set_model(loa::contracts::ModelSelection selection) const override;
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::Result<std::vector<loa::contracts::HarnessModel>>> models() const override;
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::Result<>> set_effort(std::string effort) const override;
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::Result<>> compact(std::optional<std::string> instructions) const override;
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::Result<loa::contracts::SubagentTranscript>>
    subagent_transcript(std::string id) const override;
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::Result<>> message_subagent(std::string id, std::string text) const override;
    [[nodiscard]] std::string_view harness() const override { return HARNESS; }
    [[nodiscard]] loa::contracts::SessionState state() const override;
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::Result<>> wait_idle() const override;
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::Result<>> shutdown() const override;

    [[nodiscard]] const rpc::RpcClient& client() const noexcept { return client_; }
    [[nodiscard]] std::shared_ptr<SessionShared> shared() const noexcept { return shared_; }

  private:
    [[nodiscard]] loa::contracts::Result<> open() const;
    rpc::RpcClient client_;
    std::shared_ptr<SessionShared> shared_;
    std::chrono::milliseconds shutdown_grace_;
};

class OmpAdapter final : public loa::contracts::Adapter {
  public:
    explicit OmpAdapter(OmpAdapterConfig config) : config(std::move(config)) {}
    OmpAdapterConfig config;

    [[nodiscard]] std::string_view harness() const override { return HARNESS; }
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::Availability> availability() const override;
    [[nodiscard]] loa::contracts::Capabilities capabilities() const override;
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::Result<loa::contracts::SessionPtr>>
    start(loa::contracts::SessionConfig config, std::shared_ptr<loa::core::PatchSink> sink) const override;
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::Result<loa::contracts::SessionPtr>>
    resume(loa::contracts::SessionConfig config, const loa::core::Dom& dom,
           std::shared_ptr<loa::core::PatchSink> sink) const override;
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::Result<loa::contracts::SessionPtr>>
    fork(loa::contracts::SessionConfig config, const loa::core::Dom& dom,
         std::shared_ptr<loa::core::PatchSink> sink) const override;
    [[nodiscard]] loa::contracts::Result<> import(const std::filesystem::path& native,
                                                  loa::core::PatchSink& sink) const override;
    [[nodiscard]] loa::contracts::Result<> export_(const loa::core::Dom& dom,
                                                   const std::filesystem::path& out) const override;

  private:
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::Result<std::unique_ptr<OmpSession>>>
    launch(loa::contracts::SessionConfig config, Target target, std::shared_ptr<loa::core::PatchSink> sink,
           loa::core::NodeId root, bool titled) const;
};

} // namespace loa::harness::omp
