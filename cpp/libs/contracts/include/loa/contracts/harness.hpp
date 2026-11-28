#pragma once

// The harness port: adapters start sessions that write the harness's activity into the DOM.

#include <expected>
#include <filesystem>
#include <map>
#include <memory>
#include <optional>
#include <string>
#include <string_view>
#include <variant>
#include <vector>

#include "loa/contracts/async.hpp"
#include "loa/contracts/availability.hpp"
#include "loa/contracts/capabilities.hpp"
#include "loa/contracts/output.hpp"
#include "loa/core/dom.hpp"
#include "loa/core/schema.hpp"
#include "loa/core/sink.hpp"
#include "loa/runtime/error.hpp"

namespace loa::contracts {

// Semver of this interface: major on breaking changes, minor on additions.
inline constexpr std::string_view INTERFACE_VERSION = "0.5.0";

// Why an adapter or session call failed; `message()` is the Rust `Display` text.
class AdapterError {
  public:
    struct Unsupported { std::string_view harness; std::string_view what; };
    struct Busy {};
    struct NotInstalled { std::string executable; };
    struct Version { std::string_view harness; std::string detail; };
    struct Closed {};
    struct UnknownDecision { loa::core::NodeId decision; };
    struct Protocol { std::string detail; };
    // The target (a subagent) exists but no longer takes messages; the text says why.
    struct NotAddressable { std::string reason; };
    // The target turned this one request down but still takes others; the text says why.
    struct Rejected { std::string reason; };
    struct Format { std::string detail; };
    struct Commit { loa::core::CommitError error; };
    struct Io { loa::Error error; };

    using Kind = std::variant<Unsupported, Busy, NotInstalled, Version, Closed, UnknownDecision, Protocol, NotAddressable,
                              Rejected, Format, Commit, Io>;

    template <typename K>
        requires std::is_constructible_v<Kind, K&&>
    AdapterError(K&& kind) : kind_(std::forward<K>(kind)) {}

    [[nodiscard]] const Kind& kind() const noexcept { return kind_; }
    template <typename K>
    [[nodiscard]] const K* get() const noexcept { return std::get_if<K>(&kind_); }
    [[nodiscard]] std::string message() const;

  private:
    Kind kind_;
};

template <typename T = void>
using Result = std::expected<T, AdapterError>;

// How an MCP server is reached.
struct McpStdio {
    std::string command;
    std::vector<std::string> args;
    std::map<std::string, std::string> env;
    friend bool operator==(const McpStdio&, const McpStdio&) = default;
};
struct McpHttp {
    std::string url;
    std::map<std::string, std::string> headers;
    friend bool operator==(const McpHttp&, const McpHttp&) = default;
};
using McpTransport = std::variant<McpStdio, McpHttp>;

// An MCP server the harness should connect to for this session.
struct McpServerSpec {
    std::string name;
    McpTransport transport;
    friend bool operator==(const McpServerSpec&, const McpServerSpec&) = default;
};

// A tool the app serves to the harness; `input_schema` is JSON Schema text.
struct HostToolSpec {
    std::string name;
    std::string description;
    std::string input_schema;
    friend bool operator==(const HostToolSpec&, const HostToolSpec&) = default;
};

// Runs host tool calls coming from the harness; an error reaches the model as a tool error.
class HostTools {
  public:
    virtual ~HostTools() = default;
    [[nodiscard]] virtual Awaitable<std::expected<std::string, std::string>> call(
        std::string session_id, std::string tool, std::string args) const = 0;
    // Like `call`, keeping images as image blocks.
    [[nodiscard]] virtual Awaitable<std::expected<std::vector<OutputBlock>, std::string>> call_content(
        std::string session_id, std::string tool, std::string args) const;
};

// Everything needed to start or attach a session.
struct SessionConfig {
    // App-wide session id, written to `<session id>`.
    std::string session_id;
    // Owner node key, written to `<session node>`.
    std::string node;
    std::filesystem::path cwd;
    std::optional<std::string> project;
    std::optional<std::string> model;
    std::optional<std::string> model_role;
    std::optional<std::string> effort;
    std::optional<loa::core::SessionMode> mode;
    // Harness-specific approval mode (omp `--approval-mode`); empty keeps the harness's.
    std::optional<std::string> approval_mode;
    // Project context, delivered as `Capabilities::context_delivery` declares.
    std::optional<std::string> context;
    // Additional system instructions, independent of project context. Only the omp adapter
    // delivers them (`--append-system-prompt`); omp2 refuses them like `context`, the other
    // adapters ignore them.
    std::optional<std::string> instructions;
    std::vector<McpServerSpec> mcp_servers;
    std::vector<HostToolSpec> host_tools;
    // Required when `host_tools` isn't empty.
    std::shared_ptr<const HostTools> host_tool_handler;
    std::vector<std::string> disabled_tools;
    std::map<std::string, std::string> env;
    // Executable to run instead of the one on `PATH` (the updater tests staged versions).
    std::optional<std::filesystem::path> executable;
    // The app names the session itself (forks, rewinds, compactions), so a harness that
    // generates titles leaves it alone.
    bool named = false;
};

// A file or image sent along with a prompt.
struct Attachment {
    std::filesystem::path path;
    std::optional<std::string> mime;
    friend bool operator==(const Attachment&, const Attachment&) = default;
};

struct Prompt {
    std::string text;
    std::vector<Attachment> attachments;
    // Recorded as `<message by>`.
    std::string by;
    friend bool operator==(const Prompt&, const Prompt&) = default;
};

// The response to a `<decision>`.
struct Answer {
    // Picked options, typed text, a `loa::core::answer` constant, or one JSON form object.
    std::vector<std::string> values;
    // Deny reason, reject feedback, or the edited plan.
    std::optional<std::string> reason;
    // Recorded as `<decision by>`.
    std::string by;
    friend bool operator==(const Answer&, const Answer&) = default;
};

struct ModelSelection {
    std::string model;
    // Role to assign (`default`, `smol`, …); empty sets the session's model.
    std::optional<std::string> role;
    friend bool operator==(const ModelSelection&, const ModelSelection&) = default;
};

// A model the running session can switch to.
struct HarnessModel {
    // What `set_model` takes (omp: `provider/id`).
    std::string id;
    std::string name;
    std::string provider;
    // Accepted effort levels, lowest first; empty without an effort setting.
    std::vector<std::string> efforts;
    friend bool operator==(const HarnessModel&, const HarnessModel&) = default;
};

enum class SessionState {
    Idle,
    Running,
    // Blocked on a `<decision>`.
    Waiting,
    Closed,
};

// A subagent's conversation, read through its parent session.
struct SubagentTranscript {
    // The subagent's conversation as a `<session>` tree; empty when the harness has no record.
    loa::core::Dom dom;
    // Whether `HarnessSession::message_subagent` can reach it now.
    bool addressable = false;
};

// One running session; const methods so the UI can interrupt or answer mid turn.
// Defaults answer `AdapterError::Unsupported`.
class HarnessSession {
  public:
    virtual ~HarnessSession() = default;

    // Starts a turn once the harness accepts it; `Busy` if it can't queue.
    [[nodiscard]] virtual Awaitable<Result<>> send(Prompt prompt) const = 0;
    // Stops the running turn; no-op when idle.
    [[nodiscard]] virtual Awaitable<Result<>> interrupt() const = 0;
    // Answers the waiting `<decision>` and records the answer.
    [[nodiscard]] virtual Awaitable<Result<>> answer(loa::core::NodeId decision, Answer answer) const = 0;

    [[nodiscard]] virtual Awaitable<Result<>> set_model(ModelSelection selection) const;
    [[nodiscard]] virtual Awaitable<Result<>> set_mode(loa::core::SessionMode mode) const;
    // Models this session can switch to, listed by the harness without spending tokens.
    [[nodiscard]] virtual Awaitable<Result<std::vector<HarnessModel>>> models() const;
    // Sets the effort for following turns and records it on the session.
    [[nodiscard]] virtual Awaitable<Result<>> set_effort(std::string effort) const;
    // Compacts the context, recording a `<compaction>`.
    [[nodiscard]] virtual Awaitable<Result<>> compact(std::optional<std::string> instructions) const;
    // The conversation of subagent `id` (the `<job id>` of a `kind="subagent"` job) as its own
    // `<session>` DOM, and whether it still accepts `message_subagent`.
    [[nodiscard]] virtual Awaitable<Result<SubagentTranscript>> subagent_transcript(std::string id) const;
    // Sends `text` to running subagent `id` as its user: it steers a running turn or starts the
    // next one, and is recorded in the subagent's own conversation, not this session's.
    // `NotAddressable` once the subagent no longer accepts messages; `Rejected` when it turned
    // down this message but stays reachable.
    [[nodiscard]] virtual Awaitable<Result<>> message_subagent(std::string id, std::string text) const;

    [[nodiscard]] virtual std::string_view harness() const = 0;
    [[nodiscard]] virtual SessionState state() const = 0;
    // Resolves when no turn is running or waiting.
    [[nodiscard]] virtual Awaitable<Result<>> wait_idle() const = 0;
    // Stops the harness process; the session can be resumed later.
    [[nodiscard]] virtual Awaitable<Result<>> shutdown() const = 0;
};

using SessionPtr = std::unique_ptr<HarnessSession>;

// One harness: starts, resumes and forks sessions and converts native session files.
// Defaults answer `AdapterError::Unsupported`.
class Adapter {
  public:
    virtual ~Adapter() = default;

    // Stable id written to `<session harness>` (`omp`, `claude-code`, `codex`, …).
    [[nodiscard]] virtual std::string_view harness() const = 0;
    [[nodiscard]] virtual Awaitable<Availability> availability() const;
    [[nodiscard]] virtual Capabilities capabilities() const = 0;

    // Starts a session, inserting the `<session>` root into the empty `sink`.
    [[nodiscard]] virtual Awaitable<Result<SessionPtr>> start(SessionConfig config, std::shared_ptr<loa::core::PatchSink> sink) const = 0;
    // Reattaches to a session whose journal loaded into `dom`; `sink` continues it.
    // `dom` must outlive the returned coroutine.
    [[nodiscard]] virtual Awaitable<Result<SessionPtr>> resume(SessionConfig config, const loa::core::Dom& dom,
                                                               std::shared_ptr<loa::core::PatchSink> sink) const = 0;
    // Starts the harness side of a fork of `dom`, already cut at the fork point.
    [[nodiscard]] virtual Awaitable<Result<SessionPtr>> fork(SessionConfig config, const loa::core::Dom& dom,
                                                             std::shared_ptr<loa::core::PatchSink> sink) const;
    // Reads a native session into the empty `sink`, keeping original timestamps.
    [[nodiscard]] virtual Result<> import(const std::filesystem::path& native, loa::core::PatchSink& sink) const;
    // Writes `dom` as a native session that imports back to an equal DOM.
    [[nodiscard]] virtual Result<> export_(const loa::core::Dom& dom, const std::filesystem::path& out) const;
};

// Turns `input` into a short text following `instructions` (digests, changelogs, titles).
class Summarizer {
  public:
    virtual ~Summarizer() = default;
    [[nodiscard]] virtual Awaitable<std::expected<std::string, std::string>> summarize(std::string instructions, std::string input) const = 0;
};

} // namespace loa::contracts
