#pragma once

// The app-hosted `remote` server: the SSH service every session reaches (over loopback HTTP or
// as host tools), its host key and approval questions as decision cards, the Remote hosts list
// kept in the central config, the audit log and the SSH shell behind the Remote shell tab (port
// of `loa-app/src/areas/tools/remote.rs`).

#include <chrono>
#include <cstdint>
#include <expected>
#include <filesystem>
#include <map>
#include <memory>
#include <optional>
#include <set>
#include <string>
#include <string_view>
#include <utility>
#include <vector>

#include "loa/app_runtime/session.hpp"
#include "loa/contracts/harness.hpp"
#include "loa/contracts/tools.hpp"
#include "loa/core/schema.hpp"
#include "loa/core/value.hpp"
#include "loa/remote/audit.hpp"
#include "loa/remote/http.hpp"
#include "loa/remote/ports.hpp"
#include "loa/remote/resolve.hpp"
#include "loa/remote/service.hpp"
#include "loa/view/panel.hpp"

namespace loa::app {
class Ctx;
}

namespace loa::app::areas::tools {

// Prefix of the `request_id` of the cards this module raises.
inline constexpr std::string_view REQUEST_PREFIX = "remote:";
// Options of a host key card, as the user picks them.
inline constexpr std::string_view TRUST = "Trust";
inline constexpr std::string_view REJECT = "Reject";
// Audit rows a host's log view shows.
inline constexpr std::size_t AUDIT_ROWS = 200;
// The audit log under the data directory.
inline constexpr std::string_view AUDIT_FILE = "remote-audit.db";

// A decision card raised in a session for a waiting question.
struct Card {
    app_runtime::SessionId session;
    core::NodeId decision;
};

class Remote {
  public:
    // The service over `config` (the machine's `known_hosts` and `ssh-agent`), with its audit log
    // at `<data>/remote-audit.db`; the warning says why it logs in memory instead.
    [[nodiscard]] static std::pair<Remote, std::optional<std::string>> make(const std::filesystem::path& data,
                                                                            remote::RemoteServiceConfig config);

    std::shared_ptr<remote::RemoteService> service;
    std::shared_ptr<remote::PromptQueue> queue;
    // `remote_bash` and `remote_file` over the service, as host tools and over HTTP.
    std::shared_ptr<contracts::ToolSet> tools;
    // The loopback HTTP server harnesses with MCP support connect to.
    std::shared_ptr<remote::RemoteHttp> http;
    // Why the HTTP server isn't serving, once it failed.
    std::optional<std::string> http_error;

    // A session ended: its waiting approvals are denied, its HTTP token revoked and its "allow
    // for session" answers forgotten.
    void forget(std::string_view session) const;
    // Brings the cards in line with the waiting questions: raises a card for each new one,
    // cancels the cards of questions that went away. Returns the panel's prompts and whether any is new.
    std::pair<std::vector<view::panel::RemotePrompt>, bool> sync(const Ctx& ctx);
    // Answers question `id` from the panel; its card records the answer.
    bool answer_panel(std::uint64_t id, view::panel::PromptAnswer answer, const Ctx& ctx);
    // Answers the question behind card `decision` of `session`.
    std::expected<void, std::string> answer_card(const app_runtime::SessionId& session, core::NodeId decision,
                                                 const contracts::Answer& answer, const Ctx& ctx);
    // Cancels waiting remote cards of `session` this run didn't raise: leftovers of an earlier
    // run, whose question died with it.
    void cancel_stale(const app_runtime::SessionId& session, const Ctx& ctx) const;
    // The audit log of `host`, newest first, as the panel lists it (blocking).
    [[nodiscard]] static std::expected<std::vector<view::panel::AuditRow>, std::string> audit(
        const std::shared_ptr<remote::AuditSink>& audit, std::string host);

    [[nodiscard]] const std::map<std::uint64_t, Card>& cards() const noexcept { return cards_; }

  private:
    // Cards raised in sessions, by question id.
    std::map<std::uint64_t, Card> cards_;
    // Questions already seen, to tell new ones.
    std::set<std::uint64_t> seen_;
};

// Decision kind and options for a remote host prompt.
[[nodiscard]] std::pair<core::DecisionKind, core::Value> decision_options(const remote::Prompt& prompt);
// One line for the card header.
[[nodiscard]] std::string title(const remote::Prompt& prompt);
// The card body: the fingerprint and keys on file, or the target and its detail.
[[nodiscard]] std::string body(const remote::Prompt& prompt);
[[nodiscard]] view::panel::RemotePrompt panel_prompt(const remote::PromptView& prompt);
[[nodiscard]] view::panel::AuditRow audit_row(const remote::AuditRecord& record);
// `YYYY-MM-DD HH:MM:SS UTC`.
[[nodiscard]] std::string utc(std::chrono::system_clock::time_point at);

// `$HOME` and how Remote hosts resolve (`ssh -G` over `~/.ssh/config`).
struct SshPaths {
    std::filesystem::path home;
    std::shared_ptr<const remote::SshResolver> resolver;

    [[nodiscard]] static SshPaths from_env();
};

} // namespace loa::app::areas::tools
