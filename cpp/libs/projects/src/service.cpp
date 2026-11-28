#include "loa/projects/service.hpp"

#include <algorithm>
#include <asio/io_context.hpp>
#include <cerrno>
#include <climits>
#include <cstdlib>
#include <cstring>
#include <map>
#include <set>

#include "loa/core/schema.hpp"
#include "loa/core/sink.hpp"
#include "loa/projects/context.hpp"
#include "loa/projects/digest.hpp"
#include "loa/projects/links.hpp"
#include "loa/projects/rank.hpp"
#include "loa/projects/text.hpp"
#include "loa/projects/tools.hpp"
#include "loa/runtime/log.hpp"

namespace loa::projects {

namespace attr = core::attr;
namespace fs = std::filesystem;
using settings::ProjectOverview;
using settings::ProjectSettings;
using settings::SessionRow;

namespace {

// The project screen's row for one session.
SessionRow session_row(const SessionInfo& info, const core::Dom& dom) {
    const auto* root = dom.root();
    auto digest = digest_of(dom);
    SessionRow row;
    row.id = info.id;
    row.title = info.title;
    if (root)
        if (auto h = root->str_attr(attr::HARNESS)) row.harness = std::string(*h);
    row.created = info.created;
    row.last_active = last_active(dom);
    row.turns = root ? dom.children_of_kind(root->id, core::ElementKind::Turn).size() : 0;
    row.excluded = is_excluded(dom);
    row.digest_state = digest_state(dom);
    if (digest) {
        row.digest_model = digest->model;
        row.digest = std::move(digest->content);
    }
    return row;
}

// A project's folder as a path string, when one is set.
std::optional<std::string> folder_of(const ProjectSettings& settings) {
    if (!settings.folder) return std::nullopt;
    return settings.folder->string();
}

// Updates the `.omgui` links in `folder` when it exists. A failure is logged: the listing matters more.
void sync_links(std::string_view project, const fs::path& folder, const std::set<fs::path>& dirs) {
    std::error_code ec;
    if (!fs::is_directory(folder, ec)) return;
    if (auto r = links::sync(folder, dirs); !r)
        runtime::log::warn("loa_projects", "can't update the .omgui links: project=" + std::string(project) +
                                               " folder=" + folder.string() + " error=" + r.error().to_string());
}

// Removes the `.omgui` links of `folder` when it exists, logging a failure.
void remove_links(std::string_view project, const fs::path& folder) {
    std::error_code ec;
    if (!fs::is_directory(folder, ec)) return;
    if (auto r = links::remove(folder); !r)
        runtime::log::warn("loa_projects", "can't remove the .omgui links: project=" + std::string(project) +
                                               " folder=" + folder.string() + " error=" + r.error().to_string());
}

std::optional<std::string_view> blank_to_none(std::optional<std::string_view> name) {
    if (!name) return std::nullopt;
    auto trimmed = text::trim(*name);
    if (trimmed.empty()) return std::nullopt;
    return trimmed;
}

} // namespace

Result<fs::path> isolated(fs::path folder) {
    const auto temp = fs::temp_directory_path();
    const auto same = [](const fs::path& a, const fs::path& b) {
        if (a == b) return true;
        std::error_code ea, eb;
        auto ca = fs::canonical(a, ea);
        auto cb = fs::canonical(b, eb);
        return !ea && !eb && ca == cb;
    };
    if (!same(folder, temp)) return folder;
    std::string templ = (folder / ".tmpXXXXXX").string();
    if (!::mkdtemp(templ.data()))
        return std::unexpected(Error::io(std::string(std::strerror(errno)) + " (os error " + std::to_string(errno) + ")"));
    return fs::path(templ);
}

std::pair<ProjectService, std::shared_ptr<Outcomes>> ProjectService::start(
    LiveSessions sessions, std::shared_ptr<const SettingsStore> settings, std::shared_ptr<const Access> access,
    std::shared_ptr<const Summarizers> summarizers, ServiceConfig config, asio::io_context& context) {
    auto source = std::make_shared<LiveSessions>(sessions);
    RebuildDriver driver{source, source, Digester(std::move(summarizers), config.limits), config.debounce, settings};
    auto [rebuild, outcomes] = std::move(driver).spawn(context);
    return {ProjectService(std::move(sessions), std::move(settings), std::move(access), std::move(rebuild), config.context_budget),
            std::move(outcomes)};
}

bool ProjectService::notify(std::string_view session) const { return rebuild_.send(std::string(session)); }

Result<std::vector<ProjectSummary>> ProjectService::projects() const {
    std::map<std::string, std::string> folders;
    std::map<std::string, std::pair<ProjectSummary, std::int64_t>> projects;
    auto names = settings_->names();
    if (!names) return std::unexpected(names.error());
    for (const auto& name : *names) {
        auto settings = settings_->load(name);
        if (!settings) return std::unexpected(settings.error());
        if (auto folder = folder_of(*settings)) folders.insert_or_assign(name, std::move(*folder));
        projects.insert_or_assign(name, std::pair{ProjectSummary{name, std::nullopt, 0, 0}, INT64_MIN});
    }
    std::map<std::string, std::set<fs::path>> dirs;
    auto all = sessions_.all();
    if (!all) return std::unexpected(all.error());
    for (const auto& [info, dom] : *all) {
        const auto active = last_active(*dom);
        std::optional<std::string> cwd;
        if (const auto* root = dom->root())
            if (auto c = root->str_attr(attr::CWD)) cwd = std::string(*c);
        if (cwd) dirs[info.project].insert(fs::path(*cwd));
        auto it = projects.find(info.project);
        if (it == projects.end())
            it = projects.emplace(info.project, std::pair{ProjectSummary{info.project, std::nullopt, 0, active}, INT64_MIN}).first;
        auto& [summary, cwd_at] = it->second;
        summary.sessions += 1;
        summary.last_active = std::max(summary.last_active, active);
        if (cwd && active >= cwd_at) {
            summary.cwd = std::move(cwd);
            cwd_at = active;
        }
    }
    for (const auto& [name, folder] : folders) {
        auto d = dirs.find(name);
        sync_links(name, fs::path(folder), d == dirs.end() ? std::set<fs::path>{} : d->second);
    }
    std::vector<ProjectSummary> out;
    for (auto& [name, entry] : projects) {
        auto summary = std::move(entry.first);
        if (auto f = folders.find(name); f != folders.end()) summary.cwd = f->second;
        out.push_back(std::move(summary));
    }
    std::ranges::stable_sort(out, [](const ProjectSummary& a, const ProjectSummary& b) {
        if (a.last_active != b.last_active) return a.last_active > b.last_active;
        return a.name < b.name;
    });
    return out;
}

Result<std::optional<fs::path>> ProjectService::create(std::string_view raw_name, const ProjectSettings& settings) const {
    const auto name = text::trim(raw_name);
    if (name.empty()) return std::unexpected(Error::empty_name());
    auto names = settings_->names();
    if (!names) return std::unexpected(names.error());
    if (std::ranges::find(*names, name) != names->end()) return std::unexpected(Error::project_exists(std::string(name)));
    auto stored = settings;
    if (stored.folder) {
        auto folder = isolated(*stored.folder);
        if (!folder) return std::unexpected(folder.error());
        stored.folder = std::move(*folder);
    }
    if (auto r = settings_->save(name, stored); !r) return std::unexpected(r.error());
    return stored.folder;
}

Result<fs::path> ProjectService::set_folder(std::string_view project, fs::path folder) const {
    auto settings = settings_->load(project);
    if (!settings) return std::unexpected(settings.error());
    if (settings->folder == folder) return folder;
    auto isolated_folder = isolated(std::move(folder));
    if (!isolated_folder) return std::unexpected(isolated_folder.error());
    auto old = std::exchange(settings->folder, *isolated_folder);
    if (auto r = settings_->save(project, *settings); !r) return std::unexpected(r.error());
    if (old) remove_links(project, *old);
    return *isolated_folder;
}

Result<std::size_t> ProjectService::remove(std::string_view project) const {
    auto settings = settings_->load(project);
    if (!settings) return std::unexpected(settings.error());
    std::size_t moved = 0;
    auto all = sessions_.all();
    if (!all) return std::unexpected(all.error());
    for (const auto& [info, dom] : *all) {
        if (info.project != project) continue;
        if (const auto* root = dom->root()) {
            if (auto r = sessions_.commit(info.id, core::Draft::clear(root->id, std::string(attr::PROJECT))); !r)
                return std::unexpected(r.error());
            ++moved;
        }
    }
    if (auto r = settings_->remove(project); !r) return std::unexpected(r.error());
    if (settings->folder) remove_links(project, *settings->folder);
    return moved;
}

Result<> ProjectService::move_session(std::string_view session, std::optional<std::string_view> raw_project) const {
    const auto project = blank_to_none(raw_project);
    auto dom = sessions_.load(session);
    if (!dom) return std::unexpected(dom.error());
    if (!*dom || !(*dom)->root()) return std::unexpected(Error::session_not_found(std::string(session)));
    const auto* root = (*dom)->root();
    if (blank_to_none(root->str_attr(attr::PROJECT)) == project) return {};
    auto draft = project ? core::Draft::set(root->id, std::string(attr::PROJECT), *project)
                         : core::Draft::clear(root->id, std::string(attr::PROJECT));
    if (auto r = sessions_.commit(session, std::move(draft)); !r) return std::unexpected(r.error());
    return {};
}

Result<ProjectOverview> ProjectService::overview(std::string_view project) const {
    auto settings = settings_->load(project);
    if (!settings) return std::unexpected(settings.error());
    auto loaded = settings_->load_ideas(project);
    std::expected<settings::Ideas, std::string> ideas =
        loaded ? std::expected<settings::Ideas, std::string>(std::move(*loaded)) : std::unexpected(loaded.error().to_string());
    std::vector<SessionRow> sessions;
    std::optional<std::pair<std::int64_t, std::string>> cwd;
    auto visible = visible_sessions(sessions_, *access_, project);
    if (!visible) return std::unexpected(visible.error());
    for (const auto& info : *visible) {
        auto dom = sessions_.load(info.id);
        if (!dom) return std::unexpected(dom.error());
        if (!*dom) continue;
        auto row = session_row(info, **dom);
        if (const auto* root = (*dom)->root())
            if (auto dir = root->str_attr(attr::CWD); dir && (!cwd || row.last_active >= cwd->first))
                cwd = std::pair{row.last_active, std::string(*dir)};
        sessions.push_back(std::move(row));
    }
    std::ranges::stable_sort(sessions, [](const SessionRow& a, const SessionRow& b) {
        if (a.last_active != b.last_active) return a.last_active > b.last_active;
        return a.id < b.id;
    });
    auto folder = folder_of(*settings);
    if (!folder && cwd) folder = std::move(cwd->second);
    return ProjectOverview{std::string(project), std::move(folder), std::move(*settings), std::move(ideas), std::move(sessions)};
}

Result<std::optional<runtime::fs::Snapshot>> ProjectService::save_settings(
    std::string_view project, const ProjectSettings& settings,
    std::optional<std::pair<runtime::fs::Snapshot, std::string_view>> ideas) const {
    if (auto r = settings_->save(project, settings); !r) return std::unexpected(r.error());
    if (!ideas) return std::optional<runtime::fs::Snapshot>{};
    auto saved = settings_->save_ideas(project, std::move(ideas->first), ideas->second);
    if (!saved) return std::unexpected(saved.error());
    return std::optional<runtime::fs::Snapshot>(std::move(*saved));
}

Result<bool> ProjectService::set_excluded(std::string_view session, bool excluded) const {
    auto dom = sessions_.load(session);
    if (!dom) return std::unexpected(dom.error());
    if (!*dom) return std::unexpected(Error::session_not_found(std::string(session)));
    auto draft = exclude_draft(**dom, excluded);
    if (!draft) return false;
    if (auto r = sessions_.commit(session, std::move(*draft)); !r) return std::unexpected(r.error());
    if (!excluded) notify(session);
    return true;
}

Result<std::optional<std::string>> ProjectService::context(std::string_view project, std::optional<std::string_view> session,
                                                           std::string_view query) const {
    const ContextRequest request{project, session, query, context_budget_, core::now_ms()};
    auto packed = build_context(sessions_, *access_, *settings_, request);
    if (!packed) return std::unexpected(packed.error());
    const bool empty = packed->digests.empty() && text::trim(packed->settings.instructions).empty() &&
                       packed->settings.pinned.empty() && !packed->ideas;
    if (empty) return std::optional<std::string>{};
    return std::optional<std::string>(render_markdown(*packed));
}

std::pair<std::vector<contracts::HostToolSpec>, std::shared_ptr<const contracts::HostTools>> ProjectService::host_tools() const {
    auto tools = tool_set(std::make_shared<LiveSessions>(sessions_), access_);
    auto specs = tools->specs();
    return {std::move(specs), std::move(tools)};
}

} // namespace loa::projects
