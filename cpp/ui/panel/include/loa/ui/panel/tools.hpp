#pragma once

// The tools page: built in tools, MCP servers, remote hosts and peer grants with per project
// toggles. Reads and emits the plain types of `loa::view::panel`. Port of
// `crates/loa-ui-panel/src/tools/{mod,catalog,form}.rs`. Plain C++.

#include <cstddef>
#include <cstdint>
#include <expected>
#include <map>
#include <optional>
#include <set>
#include <span>
#include <string>
#include <string_view>
#include <unordered_map>
#include <utility>
#include <variant>
#include <vector>

#include "loa/ui/kit/widgets/form.hpp"
#include "loa/ui/shell/task.hpp"
#include "loa/view/panel.hpp"

namespace loa::ui::panel::tools {

using Task = loa::ui::shell::Task;
using namespace loa::view::panel;

// ---- catalog -------------------------------------------------------------------------------

// Toggle state of a set of tools.
enum class Toggle : std::uint8_t { On, Off, Mixed };
// `on`, `off`, `mixed`.
[[nodiscard]] std::string_view label(Toggle t) noexcept;

// A listed tool and whether it is on in the selected project.
struct ToolRow {
    ToolId id;
    const ToolInfo* info = nullptr;
    bool enabled = true;
    friend bool operator==(const ToolRow&, const ToolRow&) = default;
};
struct GroupView {
    BuiltinGroup group = BuiltinGroup::Harness;
    std::string_view title;
    std::vector<ToolRow> tools;
    friend bool operator==(const GroupView&, const GroupView&) = default;
};
// A server with its filtered tools; `toggle` is over all its tools, as the toggle sets all of them.
struct ServerView {
    const McpServer* server = nullptr;
    Toggle toggle = Toggle::On;
    std::vector<ToolRow> tools;
    friend bool operator==(const ServerView&, const ServerView&) = default;
};
// The Built in and MCP sections as shown; views into the page's lists.
struct Catalog {
    std::vector<GroupView> builtin;
    std::vector<ServerView> servers;
    friend bool operator==(const Catalog&, const Catalog&) = default;
};
// Tool names per group or server, as tests compare catalogs.
using Groups = std::vector<std::vector<std::string_view>>;
[[nodiscard]] std::pair<Groups, Groups> names(const Catalog& c);

// A lowercase text filter; empty matches everything.
class Filter {
  public:
    Filter() = default;
    explicit Filter(std::string_view text);
    [[nodiscard]] bool empty() const noexcept { return text_.empty(); }
    [[nodiscard]] bool matches(std::string_view text) const;
    // `matches` for text that is already lowercase.
    [[nodiscard]] bool matches_lower(std::string_view lower) const;
    [[nodiscard]] bool tool(const ToolInfo& t) const;
    friend bool operator==(const Filter&, const Filter&) = default;

  private:
    std::string text_;
};

// Lowercased names and descriptions of every tool; entries pair with the builtin and server
// lists by index, so rebuild it whenever those change.
class Search {
  public:
    struct Haystack {
        std::string name, description;
        [[nodiscard]] bool matches(const Filter& f) const { return f.matches_lower(name) || f.matches_lower(description); }
    };
    Search() = default;
    Search(const BuiltinTools& builtin, std::span<const McpServer> servers);

  private:
    friend class Rows;
    std::vector<Haystack> harness_, app_;
    std::vector<std::pair<std::string, std::vector<Haystack>>> servers_;
};

// The Built in and MCP rows, filtered and toggled, as indices into the lists their `Search` was
// built from. Rebuild whenever the filter, the project's disabled tools or those lists change.
class Rows {
  public:
    Rows() = default;
    Rows(const BuiltinTools& builtin, std::span<const McpServer> servers, const Search& search, const std::set<ToolId>& disabled,
         const Filter& filter);
    [[nodiscard]] Catalog catalog(const BuiltinTools& builtin, std::span<const McpServer> servers) const;

    struct Row {
        ToolId id;
        std::size_t index = 0;
        bool enabled = true;
    };

  private:
    std::vector<std::pair<BuiltinGroup, std::vector<Row>>> builtin_{{BuiltinGroup::Harness, {}}, {BuiltinGroup::App, {}}};
    std::vector<std::tuple<std::size_t, Toggle, std::vector<Row>>> servers_;
};

// Title of the app tools group.
inline constexpr std::string_view APP_GROUP_TITLE = "App tools";

// On, off or mixed over every tool of a server; a server without tools is on.
[[nodiscard]] Toggle server_toggle(const McpServer& s, const std::set<ToolId>& disabled);

struct MatrixRow {
    ToolId tool;
    std::vector<bool> allowed;
    friend bool operator==(const MatrixRow&, const MatrixRow&) = default;
};
// Which tools each peer may use: `rows[i].allowed[j]` for `peers[j]`.
struct PeerMatrix {
    std::vector<const PeerTools*> peers;
    std::vector<MatrixRow> rows;
};
// The matrix over every listed tool (filtered as `Rows`); `only_peer` keeps that peer and the tools it may use.
[[nodiscard]] PeerMatrix peer_matrix(const BuiltinTools& builtin, std::span<const McpServer> servers, std::span<const PeerTools> peers,
                                     std::optional<std::string_view> only_peer, const Filter& filter);

// ---- forms ---------------------------------------------------------------------------------

// A field of a tools form, by its key (static strings).
using FieldId = std::string_view;
using Errors = std::map<FieldId, std::string>;

// Why a field was rejected; the text shows under the field.
inline constexpr std::string_view EMPTY = "required";
inline constexpr std::string_view BAD_NAME = "use letters, digits, '-' or '_'";
inline constexpr std::string_view DUPLICATE_SERVER = "a server with this name already exists";
inline constexpr std::string_view DUPLICATE_HOST = "a host with this alias already exists";
inline constexpr std::string_view UNBALANCED = "unbalanced quote";
inline constexpr std::string_view NO_COMMAND = "only environment assignments, no command";
inline constexpr std::string_view BAD_SCHEME = "URL must start with http:// or https://";
inline constexpr std::string_view NO_HOST = "URL has no host";
inline constexpr std::string_view WHITESPACE = "must not contain spaces";
inline constexpr std::string_view BAD_PORT = "port must be a number from 1 to 65535";
inline constexpr std::string_view RELATIVE_PATH = "path must start with '/' or '~'";
inline constexpr std::string_view DUPLICATE_PATH = "path is already allowed";

// Text values by field key, settable for any of `fields`.
class Values {
  public:
    // The trimmed value, or empty.
    [[nodiscard]] std::string_view get(std::string_view key) const;
    [[nodiscard]] std::string raw(std::string_view key) const;
    std::expected<void, std::string> set(std::span<const kit::widgets::Field> fields, std::string_view key, std::string_view value);
    friend bool operator==(const Values&, const Values&) = default;

  private:
    std::map<FieldId, std::string> values_;
};

// Words of a shell command line (`shell_words::split`); error on an unbalanced quote or trailing backslash.
[[nodiscard]] std::optional<std::vector<std::string>> shell_split(std::string_view line);

// How a server being added is reached.
enum class TransportKind : std::uint8_t { Stdio, Http };

// The add-server form; `command` is a stdio command line with optional leading `KEY=value` assignments.
class AddServerForm : public kit::widgets::Form {
  public:
    using Field = FieldId;
    using Output = McpServerSpec;

    TransportKind kind = TransportKind::Stdio;
    Values values;

    [[nodiscard]] std::span<const kit::widgets::Field> fields() const override;
    [[nodiscard]] std::string get(std::string_view key) const override { return values.raw(key); }
    std::expected<void, std::string> set(std::string_view key, std::string_view value) override;
    // Checks the name against `existing` ones and builds the spec.
    [[nodiscard]] std::expected<McpServerSpec, Errors> validate(std::span<const std::string> existing) const;
    friend bool operator==(const AddServerForm& a, const AddServerForm& b) { return a.kind == b.kind && a.values == b.values; }
};

// The add-host form: alias, hostname, and optional user and port.
class AddHostForm : public kit::widgets::Form {
  public:
    using Field = FieldId;
    using Output = RemoteHost;

    Values values;

    [[nodiscard]] std::span<const kit::widgets::Field> fields() const override;
    [[nodiscard]] std::string get(std::string_view key) const override { return values.raw(key); }
    std::expected<void, std::string> set(std::string_view key, std::string_view value) override;
    // A host with the safe default limits: no paths, read only, no commands.
    [[nodiscard]] std::expected<RemoteHost, Errors> validate(std::span<const std::string> existing) const;
    friend bool operator==(const AddHostForm& a, const AddHostForm& b) { return a.values == b.values; }
};

// Edits to one host's limits before they are saved; `new_path` is the add-path input.
struct LimitsDraft {
    std::string host;
    HostLimits limits;
    std::string new_path;
    std::optional<std::string_view> path_error;

    [[nodiscard]] static LimitsDraft of(const RemoteHost& host) { return {host.alias, host.limits, {}, std::nullopt}; }
    // Moves `new_path` into the allowed paths, or records why it can't.
    void add_path();
    friend bool operator==(const LimitsDraft&, const LimitsDraft&) = default;
};

// ---- page state ----------------------------------------------------------------------------

enum class Section : std::uint8_t { Builtin, Servers, Hosts, Peers };
// The page's forms; `Limits` edits the limits of the host being edited.
enum class FormId : std::uint8_t { Server, Host, Limits };
// A host limit switch.
enum class Limit : std::uint8_t { ReadOnly, Commands, AutoApprove };

// An import candidate; `exists` when a server with its name is registered already.
struct ImportEntry {
    ImportCandidate candidate;
    bool selected = false;
    bool exists = false;
    std::optional<ImportOutcome> outcome;
    friend bool operator==(const ImportEntry&, const ImportEntry&) = default;
};

// The peers view's filter: all peers, or one.
struct PeerChoice {
    std::optional<std::string> key;
    std::string name;
    [[nodiscard]] static PeerChoice all() { return {}; }
    [[nodiscard]] static PeerChoice peer(std::string key, std::string name) { return {std::move(key), std::move(name)}; }
    // The peer's name, or `All peers`.
    [[nodiscard]] std::string label() const { return key ? name : "All peers"; }
    friend bool operator==(const PeerChoice&, const PeerChoice&) = default;
};

// `BlockOpen` opens or closes a block (tool group, server tools, hosts) by key; `ToolExpanded` toggles details.
namespace msg {
struct SectionSelected { Section section; };
struct FilterChanged { std::string text; };
struct ProjectSelected { ProjectRef project; };
struct ToolToggled { ToolId tool; bool on = false; };
struct ServerToggled { std::string name; };
struct Server { std::string name; ServerAction action; };
struct TransportPicked { TransportKind kind; };
struct FormOpen { FormId form; };
struct FormEdit { FormId form; std::string key; std::string value; };
struct FormSubmit { FormId form; };
struct FormCancel { FormId form; };
struct ImportScan {};
struct ImportSelect { std::size_t index = 0; bool on = false; };
struct ImportSubmit {};
struct ImportClose {};
struct ImportSshHosts {};
struct RemoveHost { std::string alias; };
struct EditLimits { std::string alias; };
struct LimitSet { Limit limit; bool on = false; };
struct LimitsAddPath {};
struct LimitsRemovePath { std::size_t index = 0; };
struct OpenTerminal { std::string alias; };
struct ShowAudit { std::string alias; };
struct CloseAudit {};
struct AnswerPrompt { std::uint64_t id = 0; PromptAnswer answer; };
struct PeerFilter { PeerChoice choice; };
struct BlockOpen { std::string key; bool open = false; };
struct ToolExpanded { ToolId tool; };
} // namespace msg

using Message = std::variant<msg::SectionSelected, msg::FilterChanged, msg::ProjectSelected, msg::ToolToggled, msg::ServerToggled,
                             msg::Server, msg::TransportPicked, msg::FormOpen, msg::FormEdit, msg::FormSubmit, msg::FormCancel,
                             msg::ImportScan, msg::ImportSelect, msg::ImportSubmit, msg::ImportClose, msg::ImportSshHosts,
                             msg::RemoveHost, msg::EditLimits, msg::LimitSet, msg::LimitsAddPath, msg::LimitsRemovePath,
                             msg::OpenTerminal, msg::ShowAudit, msg::CloseAudit, msg::AnswerPrompt, msg::PeerFilter,
                             msg::BlockOpen, msg::ToolExpanded>;
using Event = ToolsIntent;

using ServerFormState = kit::widgets::FormState<AddServerForm>;
using HostFormState = kit::widgets::FormState<AddHostForm>;

class State {
  public:
    void set_builtin(BuiltinTools builtin);
    void set_hosts(std::vector<RemoteHost> hosts) { hosts_ = std::move(hosts); }
    void set_audit(std::string host, std::vector<AuditRow> rows) { audit_ = std::pair{std::move(host), std::move(rows)}; }
    void set_prompts(std::vector<RemotePrompt> prompts) { prompts_ = std::move(prompts); }
    // Selects a project without emitting `ProjectSelected`.
    void select_project(std::string id);
    // The tools switched off in a project, as stored in the config.
    void set_disabled_tools(std::string project, std::set<ToolId> tools);
    void set_servers(std::vector<McpServer> servers);
    // Projects for the selector; the selection stays while it exists.
    void set_projects(std::vector<ProjectRef> projects);
    void set_import_candidates(std::vector<ImportCandidate> candidates);
    // Records how importing one candidate went; imported ones are deselected.
    void set_import_outcome(ImportSource source, std::string_view name, ImportOutcome outcome);
    // The peer filter resets when its peer disappears.
    void set_peers(std::vector<PeerTools> peers);

    [[nodiscard]] Section section() const noexcept { return section_; }
    [[nodiscard]] const std::string& filter_text() const noexcept { return filter_text_; }
    [[nodiscard]] const Filter& filter() const noexcept { return filter_; }
    [[nodiscard]] const BuiltinTools& builtin() const noexcept { return builtin_; }
    [[nodiscard]] const std::vector<McpServer>& servers() const noexcept { return servers_; }
    [[nodiscard]] const std::vector<ProjectRef>& projects() const noexcept { return projects_; }
    [[nodiscard]] const std::optional<std::string>& project() const noexcept { return project_; }
    [[nodiscard]] const std::vector<RemotePrompt>& prompts() const noexcept { return prompts_; }
    [[nodiscard]] const std::vector<RemoteHost>& hosts() const noexcept { return hosts_; }
    [[nodiscard]] const std::vector<PeerTools>& peers() const noexcept { return peers_; }
    [[nodiscard]] const std::optional<std::string>& peer_filter() const noexcept { return peer_filter_; }
    [[nodiscard]] const std::optional<std::pair<std::string, std::vector<AuditRow>>>& audit() const noexcept { return audit_; }
    [[nodiscard]] const std::optional<std::vector<ImportEntry>>& imports() const noexcept { return imports_; }
    [[nodiscard]] bool import_scanning() const noexcept { return import_scanning_; }
    [[nodiscard]] const std::optional<ServerFormState>& add_server() const noexcept { return add_server_; }
    [[nodiscard]] const std::optional<HostFormState>& add_host() const noexcept { return add_host_; }
    [[nodiscard]] const std::optional<LimitsDraft>& limits() const noexcept { return limits_; }
    [[nodiscard]] const std::optional<ToolId>& expanded_tool() const noexcept { return expanded_tool_; }
    // The tools switched off in the selected project.
    [[nodiscard]] const std::set<ToolId>* disabled_tools() const;
    // Whether the block under `key` shows; every block shows while filtering.
    [[nodiscard]] bool block_open(std::string_view key) const;
    // Whether the long list `key` shows every row (`<key>:all`, closed by default).
    [[nodiscard]] bool block_flag(std::string_view key, bool fallback) const;
    // Hosts matching the filter by alias or hostname.
    [[nodiscard]] std::vector<const RemoteHost*> visible_hosts() const;
    // Built in and MCP sections as currently shown.
    [[nodiscard]] Catalog catalog() const { return rows_.catalog(builtin_, servers_); }
    [[nodiscard]] PeerMatrix peer_matrix() const;

    std::pair<Task, std::optional<Event>> update(Message message);

  private:
    std::optional<Event> handle(Message message);
    void rebuild_search();
    void rebuild();
    std::optional<Event> set_enabled(std::vector<ToolId> tools, bool enabled);

    Section section_ = Section::Builtin;
    std::string filter_text_;
    Filter filter_;
    BuiltinTools builtin_;
    std::vector<McpServer> servers_;
    std::vector<ProjectRef> projects_;
    std::optional<std::string> project_;
    std::map<std::string, std::set<ToolId>, std::less<>> disabled_;
    std::optional<ServerFormState> add_server_;
    std::optional<std::vector<ImportEntry>> imports_;
    bool import_scanning_ = false;
    std::vector<RemoteHost> hosts_;
    std::optional<HostFormState> add_host_;
    std::optional<LimitsDraft> limits_;
    std::optional<std::pair<std::string, std::vector<AuditRow>>> audit_;
    std::vector<RemotePrompt> prompts_;
    std::vector<PeerTools> peers_;
    std::optional<std::string> peer_filter_;
    std::map<std::string, bool, std::less<>> open_blocks_;
    std::optional<ToolId> expanded_tool_;
    Search search_;
    Rows rows_;
};

} // namespace loa::ui::panel::tools
