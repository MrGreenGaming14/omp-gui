// The `App` side of `loa-app/src/app/workspace.rs`: the folder and project the app works in.

#include <algorithm>

#include "loa/app/app.hpp"
#include "loa/app/perform.hpp"
#include "loa/app/routes.hpp"
#include "loa/core/schema.hpp"
#include "loa/ui/app/local_fs.hpp"

namespace loa::app {

namespace fs = std::filesystem;
namespace rt = app_runtime;

Task App::switch_folder(fs::path path) {
    std::error_code ec;
    if (auto canonical = fs::canonical(path, ec); !ec) path = std::move(canonical);
    if (!fs::is_directory(path, ec)) {
        ctx.notify(path.string() + " isn't a folder");
        return Task::none();
    }
    if (path == ctx.runtime.cwd()) return Task::none();
    std::vector<Task> tasks;
    tasks.push_back(set_folder(std::move(path)));
    if (ctx.runtime.sessions().active())
        tasks.push_back(lifecycle([](SessionController& s, Ctx& x, Registry& a) {
            return s.run(x, a, rt::command::Start{std::nullopt});
        }));
    return Task::batch(std::move(tasks));
}

Task App::set_folder(fs::path path) {
    ctx.runtime.set_cwd(path);
    (void)diff.set_note("Reading changes…");
    files = ui::panel::files::State(path, std::make_shared<ui::app::LocalFs>());
    remember(path);
    ctx.notify("Working in " + chip(*ctx.services.host.git, path).path);
    std::vector<Task> tasks;
    tasks.push_back(working_diff());
    tasks.push_back(follow(std::move(path)));
    return Task::batch(std::move(tasks));
}

void App::remember(const fs::path& path) {
    auto& recent = workspace.recent;
    std::erase(recent, path);
    recent.insert(recent.begin(), path);
    if (recent.size() > RECENT_FOLDERS) recent.resize(RECENT_FOLDERS);
    std::vector<loa::view::session::composer::Workspace> folders;
    for (const auto& folder : recent) folders.push_back(chip(*ctx.services.host.git, folder));
    ctx.composer.set_folders(std::move(folders));
}

Task App::follow(fs::path cwd) {
    ctx.composer.set_workspace(chip(*ctx.services.host.git, cwd));
    terminal.set_new_tab(ui::terminal::LocalShell::login_shell(cwd));
    if (terminal.is_empty()) return Task::none();
    std::optional<ui::terminal::TabId> here;
    for (auto id : terminal.tabs()) {
        const auto* target = terminal.target(id);
        const auto* local = target ? std::get_if<ui::terminal::LocalShell>(target) : nullptr;
        if (local && local->cwd == cwd) {
            here = id;
            break;
        }
    }
    ui::terminal::Message message = here ? ui::terminal::Message(ui::terminal::msg::Select{*here})
                                         : ui::terminal::Message(ui::terminal::msg::NewTab{});
    return Task::terminal(terminal.update(std::move(message)));
}

Task App::activate_project(std::string project, std::optional<fs::path> cwd) {
    ctx.shell.set_active_project(project);
    add_project_tab(project);
    const auto folder = cwd.value_or(ctx.runtime.cwd());
    if (ctx.runtime.project() == project && ctx.runtime.cwd() == folder) return Task::none();
    ctx.runtime.set_active(project, folder);
    files.set_root(folder);
    remember(folder);
    settings::app::ActiveProject saved{project, folder};
    ctx.edit_config("settings", [&](settings::app::Config& config) { config.active_project = saved; });
    std::vector<Task> tasks;
    for (auto& area : areas.iter_mut()) tasks.push_back(area->project_activated(project, folder, ctx));
    tasks.push_back(follow(folder));
    tasks.push_back(working_diff());
    tasks.push_back(settle());
    return Task::batch(std::move(tasks));
}

Task App::project_picked(std::optional<std::string> project) {
    if (const auto* session = ctx.runtime.sessions().active())
        return update(area_message(area_id::PROJECTS, to::projects::Move{session->id().str(), std::move(project)}));
    if (project) return update(area_message(area_id::PROJECTS, to::projects::Activate{std::move(*project)}));
    clear_project();
    return Task::none();
}

void App::clear_project() {
    ctx.runtime.clear_project();
    ctx.shell.set_active_project(std::nullopt);
    ctx.edit_config("settings", [](settings::app::Config& config) { config.active_project.reset(); });
}

Task App::forget_project(std::string_view project) {
    close_project_tab(project);
    auto pins = ctx.shell.sidebar().pinned_projects();
    if (auto it = pins.find(project); it != pins.end()) {
        pins.erase(it);
        ctx.shell.sidebar_mut().set_pinned_projects({pins.begin(), pins.end()});
        ctx.edit_config("settings",
                        [&](settings::app::Config& config) { config.pinned_projects = {pins.begin(), pins.end()}; });
    }
    if (ctx.runtime.project() == project) clear_project();
    return Task::none();
}

Task App::working_diff() const {
    auto git = ctx.services.host.git;
    auto cwd = ctx.runtime.cwd();
    return perform([git, cwd] { return git->diff(cwd); },
                   [](std::optional<std::string> patch) -> Message {
                       if (!patch) return msg::WorkingDiff{std::nullopt};
                       auto parsed = ui::panel::diff::parse_unified(*patch);
                       if (!parsed) return msg::WorkingDiff{std::unexpected(parsed.error().message())};
                       return msg::WorkingDiff{std::move(*parsed)};
                   });
}

Task App::show_diff(std::optional<std::expected<std::vector<ui::panel::diff::FileDiff>, std::string>> parsed) {
    using Target = ui_op::Pane::Target;
    // Not a git repository (or no commit yet): say so instead of keeping another folder's changes.
    if (!parsed) {
        auto folder = chip(*ctx.services.host.git, ctx.runtime.cwd()).path;
        return Task::pane(Target::Diff, diff.set_note("No git changes to show: " + folder +
                                                      " isn't a git repository with a commit"));
    }
    if (!*parsed) {
        ctx.notify("Can't read the working tree diff: " + parsed->error());
        return Task::none();
    }
    (void)diff.set_note(std::nullopt);
    return Task::pane(Target::Diff, diff.set_files(std::move(**parsed)));
}

Task App::browse() const {
    auto picker = ctx.services.host.picker;
    auto cwd = ctx.runtime.cwd();
    return perform([picker, cwd] { return picker->pick(cwd); },
                   [](std::expected<std::optional<fs::path>, std::string> chosen) -> Message {
                       return msg::FolderChosen{std::move(chosen)};
                   });
}

void App::add_project_tab(std::string_view project) {
    auto& open = workspace.open_projects;
    if (std::ranges::find(open, project) != open.end()) return;
    open.emplace_back(project);
    ctx.shell.set_open_projects(open);
}

void App::close_project_tab(std::string_view project) {
    std::erase(workspace.open_projects, project);
    ctx.shell.set_open_projects(workspace.open_projects);
}

void App::sync_project_tabs() {
    std::vector<std::pair<std::string, std::optional<std::string>>> seen;
    for (const auto& [id, session] : ctx.runtime.sessions().iter()) {
        if (workspace.tabbed.contains(id.as_str())) continue;
        // A session whose journal has no root yet is looked at again next time.
        const auto& dom = session.dom();
        const auto* root = dom.root();
        if (!root) continue;
        std::optional<std::string> project;
        if (auto p = root->str_attr(core::attr::PROJECT)) project = std::string(*p);
        seen.emplace_back(id.str(), std::move(project));
    }
    for (auto& [id, project] : seen) {
        workspace.tabbed.insert(std::move(id));
        if (project && !project->empty()) add_project_tab(*project);
    }
}

Task App::step_project_tab(bool forward) {
    if (!ctx.shell.project_tabs()) {
        ctx.notify("Project tabs are off; turn them on from the palette");
        return Task::none();
    }
    const auto& tabs = workspace.open_projects;
    if (tabs.empty()) {
        ctx.notify("No project tabs are open");
        return Task::none();
    }
    const std::size_t last = tabs.size() - 1;
    const auto& active = ctx.runtime.project();
    auto at = active ? std::ranges::find(tabs, *active) : tabs.end();
    std::size_t index = 0;
    if (at != tabs.end()) {
        const auto i = static_cast<std::size_t>(at - tabs.begin());
        index = forward ? (i == last ? 0 : i + 1) : (i == 0 ? last : i - 1);
    } else {
        index = forward ? 0 : last;
    }
    return update(area_message(area_id::PROJECTS, to::projects::Open{tabs[index]}));
}

} // namespace loa::app
