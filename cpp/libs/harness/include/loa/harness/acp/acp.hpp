#pragma once

// Generic Agent Client Protocol adapter (port of `acp/mod.rs` and `acp/session.rs`).
//
// One `AcpAdapter` drives any agent that speaks ACP (JSON-RPC 2.0 over stdio, protocol
// version 1). It is built with the agent's command line; each session runs its own agent
// process. The harness id is `acp`.
//
// Mapping onto the session DOM:
// - `initialize` negotiates the protocol version and records what the agent offers;
//   `AcpAdapter::capabilities` reflects the last negotiation
// - `session/new` starts a session with the config's MCP servers, `session/load` resumes one
//   (its replayed history is skipped, the DOM has it)
// - `session/prompt` runs a turn, `session/cancel` interrupts it
// - `session/update` chunks, tool calls, plans and mode changes become messages, `<call>`
//   elements, `<todo>` elements and session attributes; anything else is kept as
//   `<ext harness="acp">`
// - `session/request_permission` becomes an approval `<decision>`; the answer is sent back as
//   the picked option id
// - `fs/*` and `terminal/*` requests from the agent are served inside the session cwd; paths
//   outside it are refused

#include <filesystem>
#include <memory>
#include <mutex>
#include <optional>
#include <string>
#include <string_view>
#include <vector>

#include "loa/contracts/harness.hpp"
#include "loa/harness/acp/host.hpp"
#include "loa/harness/acp/mapper.hpp"
#include "loa/harness/acp/wire.hpp"

namespace loa::harness::acp {

inline constexpr std::string_view HARNESS = "acp";
using wire::PROTOCOL_VERSION;

// `file://` and the path with every byte outside the unreserved set and `/` percent-encoded.
[[nodiscard]] std::string file_uri(const std::filesystem::path& path);

// The `session/prompt` content blocks of `prompt`: its text, then one `resource_link` per
// attachment.
[[nodiscard]] std::vector<Json> prompt_blocks(const loa::contracts::Prompt& prompt);

// A running ACP session.
class AcpSession final : public loa::contracts::HarnessSession {
  public:
    struct Impl;
    // Kills the agent.
    ~AcpSession() override;
    AcpSession(const AcpSession&) = delete;
    AcpSession& operator=(const AcpSession&) = delete;

    // The agent's session id.
    [[nodiscard]] std::string native_id() const;

    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::Result<>> send(loa::contracts::Prompt prompt) const override;
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::Result<>> interrupt() const override;
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::Result<>> answer(loa::core::NodeId decision,
                                                                            loa::contracts::Answer answer) const override;
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::Result<>> set_mode(loa::core::SessionMode mode) const override;
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::Result<std::vector<loa::contracts::HarnessModel>>> models() const override;
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::Result<>> set_model(loa::contracts::ModelSelection selection) const override;
    [[nodiscard]] std::string_view harness() const override { return HARNESS; }
    [[nodiscard]] loa::contracts::SessionState state() const override;
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::Result<>> wait_idle() const override;
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::Result<>> shutdown() const override;

  private:
    explicit AcpSession(std::unique_ptr<Impl> impl);
    std::unique_ptr<Impl> impl_;
    friend class AcpAdapter;
};

// The ACP adapter. One instance serves one agent command line.
class AcpAdapter final : public loa::contracts::Adapter {
  public:
    // An adapter that runs `program args…` for every session.
    AcpAdapter(std::filesystem::path program, std::vector<std::string> args);
    AcpAdapter(const AcpAdapter& other);

    // Says `hint` instead of "not found" when the program is missing.
    [[nodiscard]] AcpAdapter with_missing_hint(std::string hint) &&;

    // The capabilities every ACP agent is guaranteed to have. Every ACP agent takes stdio MCP
    // servers, so the app can serve project context as an MCP resource.
    [[nodiscard]] static loa::contracts::Capabilities base_capabilities();

    [[nodiscard]] std::string_view harness() const override { return HARNESS; }
    // Starts the agent and runs `initialize`: ready when it speaks this ACP version, with the
    // agent's name and version as the detail.
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::Availability> availability() const override;
    [[nodiscard]] loa::contracts::Capabilities capabilities() const override;
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::Result<loa::contracts::SessionPtr>> start(
        loa::contracts::SessionConfig config, std::shared_ptr<loa::core::PatchSink> sink) const override;
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::Result<loa::contracts::SessionPtr>> resume(
        loa::contracts::SessionConfig config, const loa::core::Dom& dom, std::shared_ptr<loa::core::PatchSink> sink) const override;

  private:
    // What the last `initialize` and `session/new` taught about the agent.
    struct Learned {
        bool plan_mode = false;
        bool models = false;
    };

    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::Result<std::unique_ptr<AcpSession>>> connect(
        const loa::contracts::SessionConfig& config, std::shared_ptr<loa::core::PatchSink> sink, loa::core::NodeId root,
        std::optional<std::string> model) const;
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::Result<wire::AgentCapabilities>> initialize(const AcpSession& session) const;
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::Result<>> create(const loa::contracts::SessionConfig& config,
                                                                             const AcpSession& session) const;
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::Result<>> load(const loa::contracts::SessionConfig& config,
                                                                           const AcpSession& session, std::string native_id) const;
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::Result<>> adopt_models(const loa::contracts::SessionConfig& config,
                                                                                   const AcpSession& session,
                                                                                   std::optional<wire::SessionModelState> models) const;
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::Result<>> adopt_modes(const AcpSession& session,
                                                                                  std::optional<wire::SessionModeState> modes,
                                                                                  std::optional<loa::core::SessionMode> wanted) const;
    template <typename F>
    void learn(F&& update) const;

    std::filesystem::path program_;
    std::vector<std::string> args_;
    mutable std::mutex mutex_;
    mutable std::optional<Learned> learned_;
    // What availability says when the program is missing.
    std::optional<std::string> missing_;
};

// Registers the acp build strategy in `registry::factories()`.
void register_factory();

} // namespace loa::harness::acp

namespace loa::harness {
using acp::AcpAdapter;
using acp::AcpMapper;
using acp::AcpSession;
} // namespace loa::harness
