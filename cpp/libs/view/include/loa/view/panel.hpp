#pragma once

// Right panel panes: diff, files, session tree, agents and tools; their read
// models and what they ask the app to do.

#include <compare>
#include <cstdint>
#include <expected>
#include <filesystem>
#include <optional>
#include <string>
#include <string_view>
#include <variant>
#include <vector>

#include "loa/contracts/harness.hpp"
#include "loa/core/value.hpp"

namespace loa::view::panel {

using loa::contracts::McpServerSpec;
using loa::contracts::McpTransport;

// What the diff pane asks: open this path, the new one, or the old one of a deleted file.
namespace diff_intent {
struct OpenFile {
    std::string path;
    friend bool operator==(const OpenFile&, const OpenFile&) = default;
};
} // namespace diff_intent

using DiffIntent = std::variant<diff_intent::OpenFile>;

// What the files pane asks: open the file at this absolute path.
namespace files_intent {
struct Open {
    std::filesystem::path path;
    friend bool operator==(const Open&, const Open&) = default;
};
} // namespace files_intent

using FilesIntent = std::variant<files_intent::Open>;

// A turn element, the target of rewind and fork, and the sequence that inserted it.
struct TurnInfo {
    loa::core::NodeId node;
    std::uint64_t seq = 0;
    std::string first_line;

    friend bool operator==(const TurnInfo&, const TurnInfo&) = default;
};

// A session of a fork family: `parent` is `parent_session`, `forked_at` the parent's sequence the fork starts after.
struct SessionInfo {
    std::string id;
    std::optional<std::string> parent;
    std::optional<std::uint64_t> forked_at;
    std::string title;
    std::vector<TurnInfo> turns;

    friend bool operator==(const SessionInfo&, const SessionInfo&) = default;
};

// What the session tree asks; `Rewind` rewinds `session` to just before `turn`.
namespace session_tree_intent {
struct Rewind {
    std::string session;
    loa::core::NodeId turn;
    friend bool operator==(const Rewind&, const Rewind&) = default;
};
struct Fork {
    std::string session;
    loa::core::NodeId turn;
    friend bool operator==(const Fork&, const Fork&) = default;
};
struct Open {
    std::string session;
    friend bool operator==(const Open&, const Open&) = default;
};
} // namespace session_tree_intent

using SessionTreeIntent = std::variant<session_tree_intent::Rewind, session_tree_intent::Fork, session_tree_intent::Open>;

// What the agents pane asks. `OpenSubagent` opens a live subagent, by its `<job id>`, as a child
// tab of the conversation; `DetachTranscript` shows a read-only copy of a subagent's transcript
// (a library session id or a native session file); `Cancel` stops a running job, `id` is the
// harness's job id when known.
namespace agents_intent {
struct OpenSubagent {
    std::string job;
    friend bool operator==(const OpenSubagent&, const OpenSubagent&) = default;
};
struct DetachTranscript {
    std::string transcript;
    friend bool operator==(const DetachTranscript&, const DetachTranscript&) = default;
};
struct Cancel {
    loa::core::NodeId node;
    std::optional<std::string> id;
    friend bool operator==(const Cancel&, const Cancel&) = default;
};
} // namespace agents_intent

using AgentsIntent = std::variant<agents_intent::OpenSubagent, agents_intent::DetachTranscript, agents_intent::Cancel>;

// Readiness of one part a tool depends on.
enum class DetailState : std::uint8_t { Ready, NotConfigured, Unavailable };

struct ToolDetail {
    std::string label;
    DetailState state = DetailState::Ready;
    std::string text;

    friend bool operator==(const ToolDetail&, const ToolDetail&) = default;
};

// A tool as listed: a name, what it does, and readiness details (e.g. one per sandbox provider).
struct ToolInfo {
    std::string name;
    std::string description;
    std::vector<ToolDetail> details;

    [[nodiscard]] static ToolInfo from(const loa::contracts::HostToolSpec& spec);

    friend bool operator==(const ToolInfo&, const ToolInfo&) = default;
};

// One tool for toggles and grants: a harness or app tool, or an MCP tool scoped to its server.
namespace tool_id {
struct Builtin {
    std::string name;
    friend auto operator<=>(const Builtin&, const Builtin&) = default;
};
struct Mcp {
    std::string server;
    std::string tool;
    friend auto operator<=>(const Mcp&, const Mcp&) = default;
};
} // namespace tool_id

struct ToolId {
    std::variant<tool_id::Builtin, tool_id::Mcp> value;

    [[nodiscard]] static ToolId builtin(std::string name);
    [[nodiscard]] static ToolId mcp(std::string server, std::string tool);

    // The tool's own name, without its server.
    [[nodiscard]] const std::string& name() const noexcept;

    friend bool operator==(const ToolId&, const ToolId&) = default;
    friend auto operator<=>(const ToolId&, const ToolId&) = default;
};

// The two groups of the Built in section: the active harness's tools and the app's.
enum class BuiltinGroup : std::uint8_t { Harness, App };

// `harness` names the active harness; `harness_note` shows instead of its tools, e.g. while they load.
struct BuiltinTools {
    std::string harness;
    std::vector<ToolInfo> harness_tools;
    std::vector<ToolInfo> app_tools;
    std::optional<std::string> harness_note;

    friend bool operator==(const BuiltinTools&, const BuiltinTools&) = default;
};

namespace server_status {
struct Running {
    friend bool operator==(const Running&, const Running&) = default;
};
struct Starting {
    friend bool operator==(const Starting&, const Starting&) = default;
};
struct Stopped {
    friend bool operator==(const Stopped&, const Stopped&) = default;
};
struct Error {
    std::string message;
    friend bool operator==(const Error&, const Error&) = default;
};
} // namespace server_status

using ServerStatus = std::variant<server_status::Running, server_status::Starting, server_status::Stopped,
                                  server_status::Error>;

// Who registered a server; app hosted ones (`remote`) can't be removed and have no run controls.
enum class ServerKind : std::uint8_t { External, Surface, AppHosted };

// A registered MCP server and the tools it declared.
struct McpServer {
    std::string name;
    McpTransport transport;
    ServerStatus status;
    ServerKind kind = ServerKind::External;
    std::vector<ToolInfo> tools;

    friend bool operator==(const McpServer&, const McpServer&) = default;
};

// A project the toggles are scoped to; shown by `name`.
struct ProjectRef {
    std::string id;
    std::string name;

    friend bool operator==(const ProjectRef&, const ProjectRef&) = default;
};

// The tool config an import candidate was found in.
enum class ImportSource : std::uint8_t { Claude, Cursor, Codex };

// `.claude`, `.cursor`, `.codex`.
[[nodiscard]] std::string_view label(ImportSource source) noexcept;

// A server definition found in another tool's config file.
struct ImportCandidate {
    ImportSource source = ImportSource::Claude;
    std::string path;
    McpServerSpec server;

    friend bool operator==(const ImportCandidate&, const ImportCandidate&) = default;
};

namespace import_outcome {
struct Imported {
    friend bool operator==(const Imported&, const Imported&) = default;
};
struct Skipped {
    std::string reason;
    friend bool operator==(const Skipped&, const Skipped&) = default;
};
struct Failed {
    std::string message;
    friend bool operator==(const Failed&, const Failed&) = default;
};
} // namespace import_outcome

using ImportOutcome = std::variant<import_outcome::Imported, import_outcome::Skipped, import_outcome::Failed>;

// Limits on one SSH host: paths `remote_file` may touch, commands for `remote_bash`, and skipping approval cards.
struct HostLimits {
    std::vector<std::string> allowed_paths;
    bool read_only = false;
    bool commands_allowed = false;
    bool auto_approve = false;

    friend bool operator==(const HostLimits&, const HostLimits&) = default;
};

// An SSH host the `remote` server reaches by its ssh config `alias`.
struct RemoteHost {
    std::string alias;
    std::string hostname;
    std::optional<std::string> user;
    std::optional<std::uint16_t> port;
    std::optional<std::string> proxy_jump;
    HostLimits limits;

    friend bool operator==(const RemoteHost&, const RemoteHost&) = default;
};

// What a logged remote action did; `Terminal` is the user opening a terminal to the host.
enum class AuditAction : std::uint8_t { Read, List, Write, Edit, Command, Terminal };

// `read`, `list`, `write`, `edit`, `command`, `terminal`.
[[nodiscard]] std::string_view label(AuditAction action) noexcept;

// One audit log entry: `when` formatted by the app, `detail` a path or command, `outcome` `ok`, `denied`, ...
struct AuditRow {
    std::string when;
    std::string session;
    AuditAction action = AuditAction::Read;
    std::string detail;
    std::string outcome;

    friend bool operator==(const AuditRow&, const AuditRow&) = default;
};

// A host key to trust, or a write or command the agent wants to run.
enum class PromptKind : std::uint8_t { HostKey, Approval };

// A question from the `remote` server waiting for the user.
struct RemotePrompt {
    std::uint64_t id = 0;
    PromptKind kind = PromptKind::HostKey;
    std::string title;
    std::string body;

    friend bool operator==(const RemotePrompt&, const RemotePrompt&) = default;
};

enum class PromptAnswer : std::uint8_t { Trust, Reject, AllowOnce, AllowSession, Deny };

// One clause of a peer's grant that allows tools.
namespace tool_rule {
struct All {
    friend bool operator==(const All&, const All&) = default;
};
struct AllBuiltin {
    friend bool operator==(const AllBuiltin&, const AllBuiltin&) = default;
};
struct Server {
    std::string name;
    friend bool operator==(const Server&, const Server&) = default;
};
struct Tool {
    ToolId id;
    friend bool operator==(const Tool&, const Tool&) = default;
};
} // namespace tool_rule

struct ToolRule {
    std::variant<tool_rule::All, tool_rule::AllBuiltin, tool_rule::Server, tool_rule::Tool> value;

    [[nodiscard]] bool allows(const ToolId& tool) const noexcept;

    friend bool operator==(const ToolRule&, const ToolRule&) = default;
};

// A peer and its grant's tool clauses; anything no rule allows is denied.
struct PeerTools {
    std::string key;
    std::string name;
    std::vector<ToolRule> rules;

    [[nodiscard]] bool allows(const ToolId& tool) const noexcept;

    friend bool operator==(const PeerTools&, const PeerTools&) = default;
};

// A run control on a registered MCP server.
enum class ServerAction : std::uint8_t { Start, Stop, Restart, Remove };

// What the tools page asks; the app answers through the page's setters (disabled tools, imports, hosts, audit).
namespace tools_intent {
struct ProjectSelected {
    std::string project;
    friend bool operator==(const ProjectSelected&, const ProjectSelected&) = default;
};
struct SetToolsEnabled {
    std::string project;
    std::vector<ToolId> tools;
    bool enabled = false;
    friend bool operator==(const SetToolsEnabled&, const SetToolsEnabled&) = default;
};
struct Server {
    std::string name;
    ServerAction action = ServerAction::Start;
    friend bool operator==(const Server&, const Server&) = default;
};
struct AddServer {
    McpServerSpec server;
    friend bool operator==(const AddServer&, const AddServer&) = default;
};
struct AddHost {
    RemoteHost host;
    friend bool operator==(const AddHost&, const AddHost&) = default;
};
struct SaveHostLimits {
    std::string host;
    HostLimits limits;
    friend bool operator==(const SaveHostLimits&, const SaveHostLimits&) = default;
};
struct ImportServers {
    std::vector<ImportCandidate> candidates;
    friend bool operator==(const ImportServers&, const ImportServers&) = default;
};
struct AnswerPrompt {
    std::uint64_t id = 0;
    PromptAnswer answer = PromptAnswer::Reject;
    friend bool operator==(const AnswerPrompt&, const AnswerPrompt&) = default;
};
struct ScanImports {
    friend bool operator==(const ScanImports&, const ScanImports&) = default;
};
struct ImportSshHosts {
    friend bool operator==(const ImportSshHosts&, const ImportSshHosts&) = default;
};
struct RemoveHost {
    std::string host;
    friend bool operator==(const RemoveHost&, const RemoveHost&) = default;
};
struct OpenTerminal {
    std::string host;
    friend bool operator==(const OpenTerminal&, const OpenTerminal&) = default;
};
struct LoadAudit {
    std::string host;
    friend bool operator==(const LoadAudit&, const LoadAudit&) = default;
};
} // namespace tools_intent

using ToolsIntent =
    std::variant<tools_intent::ProjectSelected, tools_intent::SetToolsEnabled, tools_intent::Server,
                 tools_intent::AddServer, tools_intent::AddHost, tools_intent::SaveHostLimits,
                 tools_intent::ImportServers, tools_intent::AnswerPrompt, tools_intent::ScanImports,
                 tools_intent::ImportSshHosts, tools_intent::RemoveHost, tools_intent::OpenTerminal,
                 tools_intent::LoadAudit>;

} // namespace loa::view::panel

// Ports the panel panes need and the app implements.
namespace loa::view::panel::port {

// A directory entry the files pane lists.
struct Entry {
    std::string name;
    bool is_dir = false;

    friend bool operator==(const Entry&, const Entry&) = default;
};

// Port the files tree reads directories through.
class FileSystem {
  public:
    virtual ~FileSystem() = default;
    // The direct children of `dir`, already filtered; the tree sorts them. The error is shown under the directory.
    [[nodiscard]] virtual std::expected<std::vector<Entry>, std::string> list(const std::filesystem::path& dir) const = 0;
};

} // namespace loa::view::panel::port
