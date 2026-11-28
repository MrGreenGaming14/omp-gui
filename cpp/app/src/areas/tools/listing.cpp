#include "areas/tools/listing.hpp"

#include <algorithm>
#include <functional>
#include <map>

#include "loa/mcp/central.hpp"
#include "loa/remote/tools.hpp"
#include "loa/trust/token.hpp"

namespace loa::app::areas::tools::listing {

namespace {

namespace vp = view::panel;
namespace imp = mcp::import;

std::string_view trim(std::string_view s) {
    constexpr std::string_view SPACE = " \t\r\n\v\f";
    const auto start = s.find_first_not_of(SPACE);
    if (start == std::string_view::npos) return {};
    return s.substr(start, s.find_last_not_of(SPACE) - start + 1);
}

template <class... F>
struct overloaded : F... {
    using F::operator()...;
};

} // namespace

std::string short_description(std::string_view text) {
    std::string_view line;
    while (!text.empty()) {
        const auto end = text.find('\n');
        auto candidate = trim(text.substr(0, end));
        text = end == std::string_view::npos ? std::string_view{} : text.substr(end + 1);
        if (!candidate.empty()) {
            line = candidate;
            break;
        }
    }
    // The byte offset of character `DESCRIPTION_CHARS`, when the line has more.
    std::size_t chars = 0;
    for (std::size_t i = 0; i < line.size(); ++i) {
        if ((static_cast<unsigned char>(line[i]) & 0xC0U) == 0x80U) continue;
        if (chars == DESCRIPTION_CHARS) return std::string(line.substr(0, i)) + "\u2026";
        ++chars;
    }
    return std::string(line);
}

vp::McpServer server(const mcp::ServerView& view) {
    vp::ServerStatus status;
    switch (view.status.kind) {
    case mcp::Status::Kind::Starting: status = vp::server_status::Starting{}; break;
    case mcp::Status::Kind::Running: status = vp::server_status::Running{}; break;
    case mcp::Status::Kind::Stopped: status = vp::server_status::Stopped{}; break;
    case mcp::Status::Kind::Error: status = vp::server_status::Error{view.status.message}; break;
    }
    const bool surface =
        std::ranges::any_of(view.tools, [](const mcp::ToolView& tool) { return tool.tool.name.starts_with(SURFACE_TOOL_PREFIX); });
    std::vector<vp::ToolInfo> tools;
    tools.reserve(view.tools.size());
    for (const auto& tool : view.tools)
        tools.push_back({tool.tool.name, short_description(tool.tool.description.value_or("")), {}});
    return {view.spec.name, mcp::central::transport(view.spec), std::move(status),
            surface ? vp::ServerKind::Surface : vp::ServerKind::External, std::move(tools)};
}

vp::McpServer app_server(std::string_view name, contracts::McpTransport transport, vp::ServerStatus status,
                         std::vector<vp::ToolInfo> tools) {
    return {std::string(name), std::move(transport), std::move(status), vp::ServerKind::AppHosted, std::move(tools)};
}

vp::ImportSource import_source(imp::App app) {
    switch (app) {
    case imp::App::Claude: return vp::ImportSource::Claude;
    case imp::App::Cursor: return vp::ImportSource::Cursor;
    case imp::App::Codex: return vp::ImportSource::Codex;
    }
    return vp::ImportSource::Claude;
}

std::vector<vp::ImportCandidate> candidates(std::span<const imp::Found> found) {
    std::vector<vp::ImportCandidate> out;
    for (const auto& entry : found)
        if (entry.spec) out.push_back({import_source(entry.app), entry.file.string(), mcp::central::session_spec(*entry.spec)});
    return out;
}

std::vector<imp::Found> picked(std::span<const imp::Found> found, std::span<const vp::ImportCandidate> chosen) {
    std::vector<imp::Found> out;
    for (const auto& entry : found) {
        const bool pick = std::ranges::any_of(chosen, [&](const vp::ImportCandidate& candidate) {
            return candidate.source == import_source(entry.app) && candidate.path == entry.file.string() &&
                   candidate.server.name == entry.name;
        });
        if (pick) out.push_back(entry);
    }
    return out;
}

std::vector<std::tuple<vp::ImportSource, std::string, vp::ImportOutcome>> outcomes(const imp::ImportReport& report) {
    std::vector<std::tuple<vp::ImportSource, std::string, vp::ImportOutcome>> out;
    for (const auto& entry : report.entries) {
        vp::ImportOutcome outcome = std::visit(
            overloaded{
                [](const imp::outcome::Imported&) -> vp::ImportOutcome { return vp::import_outcome::Imported{}; },
                [](const imp::outcome::Duplicate& d) -> vp::ImportOutcome {
                    return vp::import_outcome::Skipped{"already registered as " + d.existing};
                },
                [](const imp::outcome::Unsupported& u) -> vp::ImportOutcome { return vp::import_outcome::Failed{u.reason}; },
            },
            entry.outcome);
        out.emplace_back(import_source(entry.app), entry.name, std::move(outcome));
    }
    return out;
}

std::vector<vp::PeerTools> peer_tools(std::span<const trust::IssuedRecord> issued, std::span<const vp::McpServer> servers,
                                      std::span<const std::string> hosts, std::int64_t now_ms) {
    std::map<std::string, std::vector<const trust::Scope*>> scopes;
    for (const auto& record : issued) {
        const bool live = !record.revoked_ms && (!record.until_ms || *record.until_ms > now_ms);
        if (!live || !record.scopes) continue;
        auto& held = scopes[record.holder.to_string()];
        for (const auto& scope : *record.scopes) held.push_back(&scope);
    }
    std::vector<vp::PeerTools> out;
    for (auto& [key, held] : scopes) {
        auto may = [&](trust::Action action, const std::function<bool(const trust::Resource&)>& resource) {
            return std::ranges::any_of(held, [&](const trust::Scope* scope) {
                return scope->actions.contains(action) && resource(scope->resource);
            });
        };
        const bool drive = may(trust::Action::drive, [](const trust::Resource& resource) {
            return std::holds_alternative<trust::resource::Session>(resource) || std::holds_alternative<trust::resource::Prompt>(resource) ||
                   std::holds_alternative<trust::resource::Project>(resource) ||
                   std::holds_alternative<trust::resource::FutureSessions>(resource);
        });
        std::vector<vp::ToolRule> rules;
        if (drive) {
            rules.push_back({vp::tool_rule::AllBuiltin{}});
            for (const auto& server : servers) {
                for (const auto& tool : server.tools) {
                    bool allowed = true;
                    if (server.name == trust::REMOTE_SERVER) {
                        const auto action = tool.name == remote::REMOTE_BASH ? trust::Action::exec : trust::Action::read;
                        allowed = may(action, [&](const trust::Resource& resource) {
                            const auto* ssh = std::get_if<trust::resource::Ssh>(&resource);
                            return ssh && std::ranges::find(hosts, ssh->host) != hosts.end();
                        });
                    } else if (tool.name.starts_with(SURFACE_TOOL_PREFIX)) {
                        const auto action = tool.name == "surface_screenshot" ? trust::Action::view : trust::Action::drive;
                        const std::string prefix = "surface://" + server.name + "/";
                        allowed = may(action, [&](const trust::Resource& resource) {
                            const auto* surface = std::get_if<trust::resource::Surface>(&resource);
                            return surface && surface->uri.starts_with(prefix);
                        });
                    }
                    if (allowed) rules.push_back({vp::tool_rule::Tool{vp::ToolId::mcp(server.name, tool.name)}});
                }
            }
        }
        std::string name = key.substr(0, std::min<std::size_t>(key.size(), 8));
        out.push_back({key, std::move(name), std::move(rules)});
    }
    return out;
}

} // namespace loa::app::areas::tools::listing
