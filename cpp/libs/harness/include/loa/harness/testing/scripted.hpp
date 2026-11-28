#pragma once

// A harness without a process for app tests: every prompt is answered at once, and the hooks
// the app drives (host tools, models, effort, modes, decisions, import/export) are switched on
// with builder calls (port of `testing/scripted.rs`).

#include <cstddef>
#include <map>
#include <memory>
#include <mutex>
#include <optional>
#include <span>
#include <string>
#include <string_view>
#include <utility>
#include <vector>

#include "loa/contracts/harness.hpp"

namespace loa::harness::testing {

// The fields of the `ask form` decision.
inline constexpr std::string_view FORM =
    R"({"type":"object","properties":{"name":{"type":"string","description":"Release name"},"channel":{"enum":["beta","stable"]}},"required":["name","channel"]})";

// What every session of a `ScriptedAdapter` was asked to do.
struct Seen {
    // Without their host tool handlers.
    std::vector<loa::contracts::SessionConfig> configs;
    std::vector<std::string> prompts;
    std::vector<std::string> models;
    std::vector<std::string> efforts;
    std::vector<loa::core::SessionMode> modes;
    std::vector<std::optional<std::string>> compacts;
    std::size_t interrupts = 0;
    std::size_t resumes = 0;
    std::vector<std::string> subagent_transcripts;
    std::vector<std::pair<std::string, std::string>> subagent_messages;
};

// A model a `ScriptedAdapter` lists: id and accepted efforts.
struct ScriptedModel {
    std::string_view id;
    std::vector<std::string_view> efforts;
};

// The scripted harness. A prompt `ask [question|multi|plan|form]` waits on a decision of that
// kind (an approval otherwise) until it is answered; `tool <name> <json>` calls the session's
// host tool and replies with its output; `work <tool> [title]` leaves a call to `tool` running
// until the turn is interrupted; any other prompt gets the reply `ok`. Copies share what they
// have seen and their subagents.
class ScriptedAdapter final : public loa::contracts::Adapter {
  public:
    explicit ScriptedAdapter(std::string id);

    [[nodiscard]] ScriptedAdapter with_capabilities(loa::contracts::Capabilities capabilities) &&;
    // Advertises `Feature::SubagentMessages`.
    [[nodiscard]] ScriptedAdapter subagent_messages() &&;
    // Registers a scripted subagent transcript DOM and addressability.
    [[nodiscard]] ScriptedAdapter subagent_transcript(std::string id, loa::core::Dom dom, bool addressable) &&;
    // What `availability` reports; ready with version 1.0 by default.
    [[nodiscard]] ScriptedAdapter with_availability(loa::contracts::Availability availability) &&;
    // `start` fails as if the harness executable were missing.
    [[nodiscard]] ScriptedAdapter no_start() &&;
    // `resume` fails as unsupported.
    [[nodiscard]] ScriptedAdapter no_resume() &&;
    // Sessions list these models and switch to any of them.
    [[nodiscard]] ScriptedAdapter with_models(std::span<const ScriptedModel> models) &&;
    // Sessions change their effort mid-session.
    [[nodiscard]] ScriptedAdapter with_effort() &&;

    // Updates or inserts a scripted subagent transcript DOM and addressability.
    void set_subagent_transcript(std::string id, loa::core::Dom dom, bool addressable) const;
    // Configures scripted errors for `subagent_transcript` or `message_subagent`.
    void set_subagent_error(std::string id, std::optional<std::string> transcript_error, std::optional<std::string> message_error) const;
    // The current scripted DOM of subagent `id`, if any.
    [[nodiscard]] std::optional<loa::core::Dom> subagent_dom(std::string_view id) const;
    [[nodiscard]] Seen seen() const;

    [[nodiscard]] std::string_view harness() const override { return id_; }
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::Availability> availability() const override;
    [[nodiscard]] loa::contracts::Capabilities capabilities() const override { return capabilities_; }
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::Result<loa::contracts::SessionPtr>> start(
        loa::contracts::SessionConfig config, std::shared_ptr<loa::core::PatchSink> sink) const override;
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::Result<loa::contracts::SessionPtr>> resume(
        loa::contracts::SessionConfig config, const loa::core::Dom& dom, std::shared_ptr<loa::core::PatchSink> sink) const override;
    // A native file is one user message per line.
    [[nodiscard]] loa::contracts::Result<> import(const std::filesystem::path& native, loa::core::PatchSink& sink) const override;
    [[nodiscard]] loa::contracts::Result<> export_(const loa::core::Dom& dom, const std::filesystem::path& out) const override;

    struct Shared;

  private:
    friend class ScriptedSession;
    [[nodiscard]] loa::contracts::SessionPtr session(loa::contracts::SessionConfig config, std::shared_ptr<loa::core::PatchSink> sink,
                                                     loa::core::NodeId root) const;

    std::string id_;
    loa::contracts::Capabilities capabilities_;
    std::optional<loa::contracts::Availability> availability_;
    bool starts_ = true;
    bool resumes_ = true;
    bool effort_ = false;
    std::vector<loa::contracts::HarnessModel> models_;
    std::shared_ptr<Shared> shared_;
};

} // namespace loa::harness::testing
