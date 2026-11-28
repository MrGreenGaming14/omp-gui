#include <toml++/toml.hpp>

#include <algorithm>
#include <charconv>
#include <format>

#include "loa/view/panel.hpp"
#include "loa/view/session.hpp"
#include "loa/view/settings_agents.hpp"
#include "loa/view/settings_models.hpp"
#include "loa/view/settings_peers.hpp"
#include "loa/view/settings_updates.hpp"
#include "loa/view/shell.hpp"
#include "loa/view/terminal.hpp"
#include "loa/view/theme.hpp"

namespace loa::view {

namespace theme {

Overrides::Overrides() : table_(std::make_unique<toml::table>()) {}
Overrides::Overrides(toml::table table) : table_(std::make_unique<toml::table>(std::move(table))) {}
Overrides::Overrides(const Overrides& other) : table_(std::make_unique<toml::table>(*other.table_)) {}
Overrides::Overrides(Overrides&&) noexcept = default;
Overrides& Overrides::operator=(const Overrides& other) {
    if (this != &other) table_ = std::make_unique<toml::table>(*other.table_);
    return *this;
}
Overrides& Overrides::operator=(Overrides&&) noexcept = default;
Overrides::~Overrides() = default;

bool operator==(const Overrides& a, const Overrides& b) {
    if (!a.table_ || !b.table_) return !a.table_ && !b.table_;
    return *a.table_ == *b.table_;
}

} // namespace theme

namespace shell {

bool NodeFilter::matches(std::string_view key) const noexcept { return !node || *node == key; }

} // namespace shell

namespace session::composer {

std::string ModelOption::to_string() const {
    auto name_part = std::format("{} ({})", name, provider);
    return role ? std::format("{} · {}", *role, name_part) : name_part;
}

} // namespace session::composer

namespace session::decisions {

const std::optional<DecisionDefaultView>& UnattendedView::for_kind(loa::core::DecisionKind kind) const noexcept {
    return const_cast<UnattendedView*>(this)->for_kind(kind);
}

std::optional<DecisionDefaultView>& UnattendedView::for_kind(loa::core::DecisionKind kind) noexcept {
    switch (kind) {
    case loa::core::DecisionKind::Approval: return approval;
    case loa::core::DecisionKind::Question: return question;
    case loa::core::DecisionKind::Plan: return plan;
    case loa::core::DecisionKind::Form: return form;
    }
    return form;
}

} // namespace session::decisions

namespace session::port {

std::string NotifyError::message() const { return "system notification failed: " + reason; }

} // namespace session::port

namespace panel {

ToolInfo ToolInfo::from(const loa::contracts::HostToolSpec& spec) { return {spec.name, spec.description, {}}; }

ToolId ToolId::builtin(std::string name) { return {tool_id::Builtin{std::move(name)}}; }
ToolId ToolId::mcp(std::string server, std::string tool) { return {tool_id::Mcp{std::move(server), std::move(tool)}}; }

const std::string& ToolId::name() const noexcept {
    if (const auto* b = std::get_if<tool_id::Builtin>(&value)) return b->name;
    return std::get<tool_id::Mcp>(value).tool;
}

std::string_view label(ImportSource source) noexcept {
    static constexpr std::string_view labels[] = {".claude", ".cursor", ".codex"};
    return labels[static_cast<std::size_t>(source)];
}

std::string_view label(AuditAction action) noexcept {
    static constexpr std::string_view labels[] = {"read", "list", "write", "edit", "command", "terminal"};
    return labels[static_cast<std::size_t>(action)];
}

bool ToolRule::allows(const ToolId& tool) const noexcept {
    if (std::holds_alternative<tool_rule::All>(value)) return true;
    if (std::holds_alternative<tool_rule::AllBuiltin>(value)) return std::holds_alternative<tool_id::Builtin>(tool.value);
    if (const auto* s = std::get_if<tool_rule::Server>(&value)) {
        const auto* mcp = std::get_if<tool_id::Mcp>(&tool.value);
        return mcp && mcp->server == s->name;
    }
    return std::get<tool_rule::Tool>(value).id == tool;
}

bool PeerTools::allows(const ToolId& tool) const noexcept {
    return std::ranges::any_of(rules, [&](const ToolRule& r) { return r.allows(tool); });
}

} // namespace panel

namespace terminal::port {

Error Error::io(std::string message) { return {Kind::Io, std::move(message)}; }
Error Error::spawn(std::string message) { return {Kind::Spawn, std::move(message)}; }
Error Error::not_a_directory(const std::filesystem::path& dir) { return {Kind::NotADirectory, dir.string()}; }
Error Error::remote(std::string host, std::string message) { return {Kind::Remote, std::move(message), std::move(host)}; }
Error Error::no_remote() { return {Kind::NoRemote, {}}; }
Error Error::closed() { return {Kind::Closed, {}}; }

std::string Error::message() const {
    switch (kind_) {
    case Kind::Io: return "terminal i/o: " + detail_;
    case Kind::Spawn: return "cannot start shell: " + detail_;
    case Kind::NotADirectory: return std::format("working directory {} is not a directory", detail_);
    case Kind::Remote: return std::format("remote host {}: {}", host_, detail_);
    case Kind::NoRemote: return "no remote transport is configured";
    case Kind::Closed: return "terminal channel is closed";
    }
    return detail_;
}

} // namespace terminal::port

namespace settings::agents {

std::span<const std::string> ModelSpec::entries() const noexcept {
    if (const auto* one = std::get_if<std::string>(&value)) return {one, 1};
    return std::get<std::vector<std::string>>(value);
}

} // namespace settings::agents

namespace settings::models {

std::string ModelRef::selector() const { return provider + "/" + id; }

const std::string& ModelInfo::display_name() const noexcept { return name.empty() ? model.id : name; }
const std::string& ModelInfo::provider_display_name() const noexcept {
    return provider_name.empty() ? model.provider : provider_name;
}

bool LoginState::is_running() const noexcept {
    return std::holds_alternative<login_state::Starting>(value) || std::holds_alternative<login_state::Browser>(value);
}

namespace {
bool is_upper(char c) noexcept { return c >= 'A' && c <= 'Z'; }
bool is_digit(char c) noexcept { return c >= '0' && c <= '9'; }
} // namespace

bool is_env_var_name(std::string_view name) noexcept {
    if (name.empty() || !(is_upper(name[0]) || name[0] == '_')) return false;
    return std::ranges::all_of(name.substr(1), [](char c) { return is_upper(c) || is_digit(c) || c == '_'; });
}

bool is_provider_id(std::string_view id) noexcept {
    return !id.empty() && std::ranges::all_of(id, [](char c) {
        return is_upper(c) || is_digit(c) || (c >= 'a' && c <= 'z') || c == '-' || c == '_' || c == '.';
    });
}

} // namespace settings::models

namespace settings::peers {

namespace {

std::optional<VersionParts> version_parts(std::string_view value) {
    while (value.starts_with('v')) value.remove_prefix(1);
    std::uint64_t parts[3];
    for (auto& part : parts) {
        const auto end = value.find_first_of(".-+");
        const auto digits = value.substr(0, end);
        const auto [ptr, ec] = std::from_chars(digits.data(), digits.data() + digits.size(), part);
        if (digits.empty() || ec != std::errc{} || ptr != digits.data() + digits.size()) return std::nullopt;
        if (end == std::string_view::npos) {
            if (&part != &parts[2]) return std::nullopt;
            value = {};
        } else {
            value.remove_prefix(end + 1);
        }
    }
    return VersionParts{parts[0], parts[1], parts[2]};
}

// "45s", "5m", "3h", "2d": rounded `up` for countdowns, down for ages.
std::string duration(std::uint64_t seconds, bool up) {
    if (seconds < 60) return std::format("{}s", seconds);
    const auto [unit, name] = seconds < 3600    ? std::pair{std::uint64_t{60}, 'm'}
                              : seconds < 86400 ? std::pair{std::uint64_t{3600}, 'h'}
                                                : std::pair{std::uint64_t{86400}, 'd'};
    return std::format("{}{}", up ? (seconds + unit - 1) / unit : seconds / unit, name);
}

} // namespace

std::string_view Peer::short_key() const noexcept {
    // The first 12 UTF-8 characters.
    std::size_t chars = 0;
    for (std::size_t i = 0; i < key.size(); ++i) {
        if ((static_cast<unsigned char>(key[i]) & 0xC0) == 0x80) continue;
        if (chars++ == 12) return std::string_view(key).substr(0, i);
    }
    return key;
}

std::optional<VersionParts> Peer::version() const { return version_parts(app_version); }

VersionRelation Peer::version_relation(std::string_view local) const {
    const auto remote = version();
    const auto mine = version_parts(local);
    if (!remote || !mine) return VersionRelation::Unknown;
    if (*remote < *mine) return VersionRelation::Behind;
    if (*remote > *mine) return VersionRelation::Ahead;
    return VersionRelation::Current;
}

std::string Peer::last_seen_text(std::uint64_t now) const {
    if (presence == Presence::Online) return "now";
    if (!last_seen) return "never";
    return duration(now > *last_seen ? now - *last_seen : 0, false) + " ago";
}

bool allowed_for(Permission permission, GrantScope scope) noexcept {
    switch (scope) {
    case GrantScope::Sandbox: return permission == Permission::Drive;
    case GrantScope::Surface:
    case GrantScope::SshHost:
    case GrantScope::Folder: return permission == Permission::View || permission == Permission::Drive;
    default: return true;
    }
}

std::string Grant::expiry_text(std::uint64_t now) const {
    if (ended) return direction == GrantDirection::Given ? "revoked" : "revoked by its issuer";
    std::string expiry = !expires_at          ? "no end date"
                         : *expires_at <= now ? "expired"
                                              : "expires in " + duration(*expires_at - now, true);
    if (!renews_at) return expiry;
    if (*renews_at > now) return std::format("{}; renews in {}", expiry, duration(*renews_at - now, true));
    return expiry + "; renewal due";
}

} // namespace settings::peers

namespace settings::updates {

std::string_view label(GateCheck check) noexcept {
    switch (check) {
    case GateCheck::ToolSchemas: return "Tool schemas";
    case GateCheck::SessionFormat: return "Session format version";
    case GateCheck::ConfigSchema: return "Config schema";
    case GateCheck::RpcProtocol: return "RPC protocol version";
    case GateCheck::ContractTests: return "Adapter contract tests";
    case GateCheck::Changelog: return "Changelog";
    }
    return {};
}

std::vector<const CheckResult*> GateReport::harmful() const {
    std::vector<const CheckResult*> out;
    for (const auto& c : checks)
        if (c.verdict == Verdict::Harmful) out.push_back(&c);
    return out;
}

} // namespace settings::updates

} // namespace loa::view
