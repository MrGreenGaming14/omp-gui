#pragma once

// omp2 RPC v1 adapter: one child per session; `patch@1` notifications drive the app DOM.

#include <array>
#include <filesystem>
#include <memory>
#include <optional>
#include <string_view>
#include <utility>

#include "loa/contracts/harness.hpp"
#include "loa/harness/omp2/mapper.hpp"
#include "loa/runtime/exec.hpp"

namespace loa::harness::omp2 {

inline constexpr std::string_view HARNESS = "omp2";
inline constexpr std::string_view EXECUTABLE = "omp";
inline constexpr std::array<std::string_view, 6> THINKING_LEVELS{"off", "minimal", "low", "medium", "high", "xhigh"};

// One running omp2 session.
class Omp2Session final : public loa::contracts::HarnessSession {
  public:
    struct Impl;
    ~Omp2Session() override;
    Omp2Session(const Omp2Session&) = delete;
    Omp2Session& operator=(const Omp2Session&) = delete;

    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::Result<>> send(loa::contracts::Prompt prompt) const override;
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::Result<>> interrupt() const override;
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::Result<>> answer(loa::core::NodeId decision,
                                                                            loa::contracts::Answer answer) const override;
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::Result<>> set_model(loa::contracts::ModelSelection selection) const override;
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::Result<std::vector<loa::contracts::HarnessModel>>> models() const override;
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::Result<>> set_effort(std::string effort) const override;
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::Result<>> compact(std::optional<std::string> instructions) const override;
    [[nodiscard]] std::string_view harness() const override { return HARNESS; }
    [[nodiscard]] loa::contracts::SessionState state() const override;
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::Result<>> wait_idle() const override;
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::Result<>> shutdown() const override;

  private:
    explicit Omp2Session(std::unique_ptr<Impl> impl);
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::Result<loa::runtime::json::Value>> call(
        std::string method, loa::runtime::json::Value params) const;
    [[nodiscard]] loa::contracts::Result<> set_root(std::string_view name, std::string_view value) const;
    std::unique_ptr<Impl> impl_;
    friend class Omp2Adapter;
};

// Starts omp2 and maps its DOM onto the app's.
class Omp2Adapter final : public loa::contracts::Adapter {
  public:
    explicit Omp2Adapter(std::filesystem::path executable = EXECUTABLE);
    [[nodiscard]] static Omp2Adapter with_executable(std::filesystem::path executable) { return Omp2Adapter(std::move(executable)); }

    [[nodiscard]] std::string_view harness() const override { return HARNESS; }
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::Availability> availability() const override;
    [[nodiscard]] loa::contracts::Capabilities capabilities() const override;
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::Result<loa::contracts::SessionPtr>> start(
        loa::contracts::SessionConfig config, std::shared_ptr<loa::core::PatchSink> sink) const override;
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::Result<loa::contracts::SessionPtr>> resume(
        loa::contracts::SessionConfig config, const loa::core::Dom& dom, std::shared_ptr<loa::core::PatchSink> sink) const override;

  private:
    [[nodiscard]] loa::contracts::Result<loa::runtime::exec::Command> command(
        const loa::contracts::SessionConfig& config, std::optional<std::string_view> resume) const;
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::Result<std::pair<std::unique_ptr<Omp2Session>, std::optional<std::string>>>> launch(
        const loa::contracts::SessionConfig& config, std::shared_ptr<loa::core::PatchSink> sink, Omp2Mapper mapper,
        std::optional<std::string_view> resume) const;
    std::filesystem::path executable_;
};

// Registers the omp2 build strategy in `registry::factories()`.
void register_factory();

} // namespace loa::harness::omp2

namespace loa::harness {
using omp2::Omp2Adapter;
using omp2::Omp2Mapper;
using omp2::Omp2Session;
} // namespace loa::harness
