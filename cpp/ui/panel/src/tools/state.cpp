// The tools page state. Port of `crates/loa-ui-panel/src/tools/mod.rs`.

#include <algorithm>

#include "loa/ui/panel/tools.hpp"

namespace loa::ui::panel::tools {

namespace ti = loa::view::panel::tools_intent;

namespace {

const std::set<ToolId>& no_tools() {
    static const std::set<ToolId> empty;
    return empty;
}

template <typename F> void edit(kit::widgets::FormState<F>& form, std::string_view key, std::string_view value) {
    (void)form.spec.set(key, value);
    if (const auto it = form.errors.find(key); it != form.errors.end()) form.errors.erase(it);
}

std::vector<std::string> server_names(const std::vector<McpServer>& servers) {
    std::vector<std::string> out;
    for (const auto& s : servers) out.push_back(s.name);
    return out;
}

} // namespace

void State::set_builtin(BuiltinTools builtin) {
    builtin_ = std::move(builtin);
    rebuild_search();
}

void State::select_project(std::string id) {
    project_ = std::move(id);
    rebuild();
}

void State::set_disabled_tools(std::string project, std::set<ToolId> tools) {
    disabled_.insert_or_assign(std::move(project), std::move(tools));
    rebuild();
}

void State::set_servers(std::vector<McpServer> servers) {
    servers_ = std::move(servers);
    if (imports_)
        for (auto& e : *imports_)
            e.exists = std::ranges::any_of(servers_, [&](const McpServer& s) { return s.name == e.candidate.server.name; });
    rebuild_search();
}

void State::set_projects(std::vector<ProjectRef> projects) {
    if (project_ && std::ranges::none_of(projects, [&](const ProjectRef& p) { return p.id == *project_; })) project_.reset();
    projects_ = std::move(projects);
    rebuild();
}

void State::set_import_candidates(std::vector<ImportCandidate> candidates) {
    std::vector<ImportEntry> entries;
    for (auto& c : candidates) {
        const bool exists = std::ranges::any_of(servers_, [&](const McpServer& s) { return s.name == c.server.name; });
        entries.push_back({std::move(c), !exists, exists, std::nullopt});
    }
    imports_ = std::move(entries);
    import_scanning_ = false;
}

void State::set_import_outcome(ImportSource source, std::string_view name, ImportOutcome outcome) {
    if (!imports_) return;
    for (auto& e : *imports_)
        if (e.candidate.source == source && e.candidate.server.name == name) {
            e.selected = e.selected && !std::holds_alternative<import_outcome::Imported>(outcome);
            e.outcome = std::move(outcome);
            return;
        }
}

void State::set_peers(std::vector<PeerTools> peers) {
    if (peer_filter_ && std::ranges::none_of(peers, [&](const PeerTools& p) { return p.key == *peer_filter_; })) peer_filter_.reset();
    peers_ = std::move(peers);
}

const std::set<ToolId>* State::disabled_tools() const {
    if (!project_) return nullptr;
    const auto it = disabled_.find(*project_);
    return it == disabled_.end() ? nullptr : &it->second;
}

bool State::block_flag(std::string_view key, bool fallback) const {
    const auto it = open_blocks_.find(key);
    return it == open_blocks_.end() ? fallback : it->second;
}

bool State::block_open(std::string_view key) const { return !filter_.empty() || block_flag(key, true); }

std::vector<const RemoteHost*> State::visible_hosts() const {
    std::vector<const RemoteHost*> out;
    for (const auto& h : hosts_)
        if (filter_.matches(h.alias) || filter_.matches(h.hostname)) out.push_back(&h);
    return out;
}

PeerMatrix State::peer_matrix() const {
    return tools::peer_matrix(builtin_, servers_, peers_,
                              peer_filter_ ? std::optional<std::string_view>(*peer_filter_) : std::nullopt, filter_);
}

void State::rebuild_search() {
    search_ = Search(builtin_, servers_);
    rebuild();
}

void State::rebuild() {
    const auto* d = disabled_tools();
    rows_ = Rows(builtin_, servers_, search_, d ? *d : no_tools(), filter_);
}

std::pair<Task, std::optional<Event>> State::update(Message message) { return {Task::none(), handle(std::move(message))}; }

std::optional<Event> State::set_enabled(std::vector<ToolId> tools, bool enabled) {
    if (!project_ || tools.empty()) return std::nullopt;
    auto& disabled = disabled_[*project_];
    for (const auto& t : tools) {
        if (enabled) disabled.erase(t);
        else disabled.insert(t);
    }
    rebuild();
    return ti::SetToolsEnabled{*project_, std::move(tools), enabled};
}

std::optional<Event> State::handle(Message message) {
    return std::visit(
        [&](auto& m) -> std::optional<Event> {
            using M = std::decay_t<decltype(m)>;
            if constexpr (std::is_same_v<M, msg::SectionSelected>) {
                section_ = m.section;
            } else if constexpr (std::is_same_v<M, msg::FilterChanged>) {
                filter_ = Filter(m.text);
                filter_text_ = std::move(m.text);
                rebuild();
            } else if constexpr (std::is_same_v<M, msg::ProjectSelected>) {
                if (project_ != m.project.id) {
                    project_ = m.project.id;
                    rebuild();
                    return ti::ProjectSelected{std::move(m.project.id)};
                }
            } else if constexpr (std::is_same_v<M, msg::ToolToggled>) {
                return set_enabled({std::move(m.tool)}, m.on);
            } else if constexpr (std::is_same_v<M, msg::ServerToggled>) {
                const auto s = std::ranges::find(servers_, m.name, &McpServer::name);
                if (s == servers_.end()) return std::nullopt;
                const auto* d = disabled_tools();
                const bool on = server_toggle(*s, d ? *d : no_tools()) != Toggle::On;
                std::vector<ToolId> ids;
                for (const auto& t : s->tools) ids.push_back(ToolId::mcp(s->name, t.name));
                return set_enabled(std::move(ids), on);
            } else if constexpr (std::is_same_v<M, msg::Server>) {
                const auto s = std::ranges::find(servers_, m.name, &McpServer::name);
                if (s == servers_.end()) return std::nullopt;
                if (m.action == ServerAction::Remove && s->kind == ServerKind::AppHosted) return std::nullopt;
                return ti::Server{std::move(m.name), m.action};
            } else if constexpr (std::is_same_v<M, msg::FormOpen>) {
                if (m.form == FormId::Server && !add_server_) add_server_.emplace();
                if (m.form == FormId::Host && !add_host_) add_host_.emplace();
            } else if constexpr (std::is_same_v<M, msg::FormEdit>) {
                switch (m.form) {
                case FormId::Server:
                    if (add_server_) edit(*add_server_, m.key, m.value);
                    break;
                case FormId::Host:
                    if (add_host_) edit(*add_host_, m.key, m.value);
                    break;
                case FormId::Limits:
                    if (limits_) {
                        limits_->new_path = std::move(m.value);
                        limits_->path_error.reset();
                    }
                    break;
                }
            } else if constexpr (std::is_same_v<M, msg::TransportPicked>) {
                if (add_server_) add_server_->spec.kind = m.kind;
            } else if constexpr (std::is_same_v<M, msg::FormSubmit>) {
                switch (m.form) {
                case FormId::Server: {
                    if (!add_server_) return std::nullopt;
                    const auto names = server_names(servers_);
                    auto spec = add_server_->submit(names);
                    if (!spec) return std::nullopt;
                    add_server_.reset();
                    return ti::AddServer{std::move(*spec)};
                }
                case FormId::Host: {
                    if (!add_host_) return std::nullopt;
                    std::vector<std::string> aliases;
                    for (const auto& h : hosts_) aliases.push_back(h.alias);
                    auto host = add_host_->submit(aliases);
                    if (!host) return std::nullopt;
                    add_host_.reset();
                    return ti::AddHost{std::move(*host)};
                }
                case FormId::Limits: {
                    if (!limits_) return std::nullopt;
                    auto d = std::move(*limits_);
                    limits_.reset();
                    return ti::SaveHostLimits{std::move(d.host), std::move(d.limits)};
                }
                }
            } else if constexpr (std::is_same_v<M, msg::FormCancel>) {
                if (m.form == FormId::Server) add_server_.reset();
                else if (m.form == FormId::Host) add_host_.reset();
                else limits_.reset();
            } else if constexpr (std::is_same_v<M, msg::ImportScan>) {
                imports_.emplace();
                import_scanning_ = true;
                return ti::ScanImports{};
            } else if constexpr (std::is_same_v<M, msg::ImportSelect>) {
                if (imports_ && m.index < imports_->size()) (*imports_)[m.index].selected = m.on;
            } else if constexpr (std::is_same_v<M, msg::ImportSubmit>) {
                if (!imports_) return std::nullopt;
                std::vector<ImportCandidate> chosen;
                for (const auto& e : *imports_)
                    if (e.selected) chosen.push_back(e.candidate);
                if (chosen.empty()) return std::nullopt;
                return ti::ImportServers{std::move(chosen)};
            } else if constexpr (std::is_same_v<M, msg::ImportClose>) {
                imports_.reset();
                import_scanning_ = false;
            } else if constexpr (std::is_same_v<M, msg::ImportSshHosts>) {
                return ti::ImportSshHosts{};
            } else if constexpr (std::is_same_v<M, msg::RemoveHost>) {
                if (limits_ && limits_->host == m.alias) limits_.reset();
                return ti::RemoveHost{std::move(m.alias)};
            } else if constexpr (std::is_same_v<M, msg::EditLimits>) {
                const auto h = std::ranges::find(hosts_, m.alias, &RemoteHost::alias);
                if (h != hosts_.end()) limits_ = LimitsDraft::of(*h);
            } else if constexpr (std::is_same_v<M, msg::LimitSet>) {
                if (!limits_) return std::nullopt;
                auto& l = limits_->limits;
                (m.limit == Limit::ReadOnly ? l.read_only : m.limit == Limit::Commands ? l.commands_allowed : l.auto_approve) = m.on;
            } else if constexpr (std::is_same_v<M, msg::LimitsAddPath>) {
                if (limits_) limits_->add_path();
            } else if constexpr (std::is_same_v<M, msg::LimitsRemovePath>) {
                if (limits_ && m.index < limits_->limits.allowed_paths.size())
                    limits_->limits.allowed_paths.erase(limits_->limits.allowed_paths.begin() + static_cast<std::ptrdiff_t>(m.index));
            } else if constexpr (std::is_same_v<M, msg::OpenTerminal>) {
                return ti::OpenTerminal{std::move(m.alias)};
            } else if constexpr (std::is_same_v<M, msg::ShowAudit>) {
                audit_ = std::pair{m.alias, std::vector<AuditRow>{}};
                return ti::LoadAudit{std::move(m.alias)};
            } else if constexpr (std::is_same_v<M, msg::CloseAudit>) {
                audit_.reset();
            } else if constexpr (std::is_same_v<M, msg::AnswerPrompt>) {
                const auto p = std::ranges::find(prompts_, m.id, &RemotePrompt::id);
                if (p == prompts_.end()) return std::nullopt;
                prompts_.erase(p);
                return ti::AnswerPrompt{m.id, m.answer};
            } else if constexpr (std::is_same_v<M, msg::PeerFilter>) {
                peer_filter_ = std::move(m.choice.key);
            } else if constexpr (std::is_same_v<M, msg::BlockOpen>) {
                open_blocks_.insert_or_assign(std::move(m.key), m.open);
            } else if constexpr (std::is_same_v<M, msg::ToolExpanded>) {
                if (expanded_tool_ == m.tool) expanded_tool_.reset();
                else expanded_tool_ = std::move(m.tool);
            }
            return std::nullopt;
        },
        message);
}

} // namespace loa::ui::panel::tools
