#include "loa/app/app.hpp"

#include <algorithm>

#include "loa/app/adapters/models.hpp"
#include "loa/app/adapters/summarizer.hpp"
#include "loa/app/perform.hpp"
#include "loa/app/routes.hpp"
#include "loa/app/view/theme.hpp"
#include "loa/app/watch.hpp"
#include "loa/harness/account_support/opener.hpp"
#include "loa/harness/account_support/process.hpp"
#include "loa/harness/omp/account.hpp"
#include "loa/harness/omp/oneshot.hpp"
#include "loa/harness/omp/validate.hpp"
#include "loa/harness/registry/registry.hpp"
#include "loa/runtime/fs.hpp"
#include "loa/settings/models/keys.hpp"
#include "loa/ui/app/local_fs.hpp"
#include "loa/ui/kit/theme.hpp"
#include "loa/ui/panel/view.hpp"
#include "loa/ui/terminal/view.hpp"

namespace loa::app {

namespace {

namespace rt = app_runtime;
// `harness` alone names the app's harness helpers (`services.hpp`).
namespace omp = loa::harness::omp;
namespace account_support = loa::harness::account_support;
namespace shell = ui::shell;
namespace palette = ui::shell::palette;
namespace nav = loa::view::shell::sidebar_intent;
namespace shell_intent = loa::view::shell::intent;
namespace composer_intent = loa::view::session::composer::intent;

template <class... F>
struct overloaded : F... {
    using F::operator()...;
};

std::optional<std::string_view> opt_view(const std::optional<std::string>& s) {
    return s ? std::optional<std::string_view>(*s) : std::nullopt;
}

// What a shell or composer event asks of the app.
namespace intent {
struct Start {};
struct OpenSession {
    std::string id;
};
struct Submit {
    contracts::Prompt prompt;
};
struct Interrupt {};
struct Access {
    std::string id;
};
struct Files {};
struct Tab {
    std::string_view id;
};
struct Folder {
    std::filesystem::path path;
};
struct Browse {};
struct Usage {};
struct Notice {
    std::string_view text;
};
// Paste the clipboard's image into the composer.
struct PasteImage {};
// The composer's project chip: moves the shown session, or picks the next one's project.
struct ProjectPicked {
    std::optional<std::string> project;
};
struct CloseProjectTab {
    std::string project;
};
// A theme was picked (saved) or the active one changed.
struct Theme {
    std::optional<loa::view::theme::ThemeRef> picked;
};
struct SavePins {};
struct SaveProjectPins {};
// Runs this message.
struct Forward {
    Message message;
};
} // namespace intent

using IntentV = std::variant<intent::Start, intent::OpenSession, intent::Submit, intent::Interrupt, intent::Access,
                             intent::Files, intent::Tab, intent::Folder, intent::Browse, intent::Usage, intent::Notice,
                             intent::PasteImage, intent::ProjectPicked, intent::CloseProjectTab, intent::Theme,
                             intent::SavePins, intent::SaveProjectPins, intent::Forward>;

Message window(WindowMessage message) { return msg::Window{std::move(message)}; }

// The route table from shell events to intents.
std::optional<IntentV> route(loa::view::shell::Intent event) {
    auto forward = [](Message m) -> IntentV { return intent::Forward{std::move(m)}; };
    return std::visit(
        overloaded{
            [&](shell_intent::Sidebar& s) -> std::optional<IntentV> {
                return std::visit(
                    overloaded{
                        [&](nav::NewSession&) -> IntentV { return intent::Start{}; },
                        [&](nav::OpenUsage&) -> IntentV { return intent::Usage{}; },
                        [&](nav::OpenConfigs&) { return forward(window(window_msg::OpenConfigs{ConfigsTab::Models})); },
                        [&](nav::OpenSettings&) {
                            return forward(window(window_msg::OpenSettings{SettingsTab::General}));
                        },
                        [&](nav::OpenPeers&) { return forward(window(window_msg::OpenSettings{SettingsTab::Peers})); },
                        [&](nav::OpenTools&) { return forward(area_message(area_id::TOOLS, to::tools::Open{})); },
                        [&](nav::OpenUpdates&) { return forward(area_message(area_id::UPDATER, to::updater::Open{})); },
                        [&](nav::OpenSession& o) -> IntentV {
                            if (peers::is_remote(o.id))
                                return forward(area_message(area_id::PEERS, to::peers::OpenRecent{std::move(o.id)}));
                            return intent::OpenSession{std::move(o.id)};
                        },
                        [&](nav::DeleteSession& d) -> IntentV {
                            if (peers::is_remote(d.id))
                                return intent::Notice{"A peer's session is deleted on its own node; revoke or let the "
                                                      "grant end to hide it"};
                            return forward(area_message(area_id::SESSIONS, to::sessions::Delete{std::move(d.id)}));
                        },
                        [&](nav::OpenProject& o) {
                            return forward(area_message(area_id::PROJECTS, to::projects::Open{std::move(o.project)}));
                        },
                        [&](nav::NewProject&) { return forward(area_message(area_id::PROJECTS, to::projects::New{})); },
                        [&](nav::NewSessionIn& n) {
                            return forward(
                                area_message(area_id::PROJECTS, to::projects::NewChatIn{std::move(n.project)}));
                        },
                        [&](nav::DeleteProject& d) {
                            return forward(
                                area_message(area_id::PROJECTS, to::projects::AskDelete{std::move(d.project)}));
                        },
                        [&](nav::MoveSession& m) {
                            return forward(area_message(area_id::PROJECTS,
                                                        to::projects::Move{std::move(m.id), std::move(m.project)}));
                        },
                        [&](nav::SetPinned&) -> IntentV { return intent::SavePins{}; },
                        [&](nav::SetProjectPinned&) -> IntentV { return intent::SaveProjectPins{}; },
                    },
                    s.intent);
            },
            [&](shell_intent::DetachTab& d) -> std::optional<IntentV> {
                return forward(window(window_msg::Detach{std::move(d.tab)}));
            },
            [&](shell_intent::ShowDetached& d) -> std::optional<IntentV> {
                return forward(window(window_msg::Show{std::move(d.tab)}));
            },
            [&](shell_intent::ReattachTab& d) -> std::optional<IntentV> {
                return forward(window(window_msg::ReattachTab{std::move(d.tab)}));
            },
            [&](shell_intent::CloseProjectTab& c) -> std::optional<IntentV> {
                return intent::CloseProjectTab{std::move(c.project)};
            },
            [&](shell_intent::SetTheme& t) -> std::optional<IntentV> { return intent::Theme{std::move(t.theme)}; },
            [&](shell_intent::ThemeChanged&) -> std::optional<IntentV> { return intent::Theme{std::nullopt}; },
        },
        event);
}

// The route table from composer intents to app intents.
std::optional<IntentV> compose(loa::view::session::composer::Intent event) {
    auto area = [](std::string_view id, auto m) -> std::optional<IntentV> {
        return intent::Forward{area_message(id, std::move(m))};
    };
    return std::visit(
        overloaded{
            [&](composer_intent::Submit& s) -> std::optional<IntentV> { return intent::Submit{std::move(s.prompt)}; },
            [&](composer_intent::Stop&) -> std::optional<IntentV> { return intent::Interrupt{}; },
            [&](composer_intent::HarnessChanged& h) {
                return area(area_id::HARNESS, to::harness::Pick{std::move(h.harness)});
            },
            [&](composer_intent::ModelChanged& m) { return area(area_id::MODELS, to::models::Model{std::move(m.model)}); },
            [&](composer_intent::ModelAction& a) { return area(area_id::MODELS, to::models::Action{std::move(a.id)}); },
            [&](composer_intent::EffortChanged& e) {
                return area(area_id::MODELS, to::models::Effort{std::move(e.effort)});
            },
            [&](composer_intent::WorkflowPicked& w) {
                return area(area_id::WORKFLOWS, to::workflows::Pick{std::move(w.role)});
            },
            [&](composer_intent::OpenRules&) { return area(area_id::WORKFLOWS, to::workflows::Open{}); },
            [&](composer_intent::ModeChanged& m) { return area(area_id::SESSIONS, to::sessions::Mode{m.mode}); },
            [&](composer_intent::AccessChanged& a) -> std::optional<IntentV> {
                return intent::Access{std::move(a.access)};
            },
            [&](composer_intent::FilesRequested&) -> std::optional<IntentV> { return intent::Files{}; },
            [&](composer_intent::OpenWorkspace&) -> std::optional<IntentV> { return intent::Tab{"files"}; },
            [&](composer_intent::FolderPicked& f) -> std::optional<IntentV> {
                return intent::Folder{std::filesystem::path(f.folder)};
            },
            [&](composer_intent::BrowseFolders&) -> std::optional<IntentV> { return intent::Browse{}; },
            [&](composer_intent::PasteImage&) -> std::optional<IntentV> { return intent::PasteImage{}; },
            [&](composer_intent::ProjectPicked& p) -> std::optional<IntentV> {
                return intent::ProjectPicked{std::move(p.project)};
            },
            [&](composer_intent::QueueChanged&) -> std::optional<IntentV> { return std::nullopt; },
        },
        event);
}

} // namespace

struct App::Intent {
    IntentV value;
};

// ---- parts -----------------------------------------------------------------------------------

rt::RuntimeParts runtime_parts(const Services& services, std::shared_ptr<session::Library> library,
                               rt::Harnesses harnesses) {
    auto saved = services.storage.config.load().value_or(settings::app::Config{});
    std::optional<std::string> project;
    std::filesystem::path cwd = services.workspace;
    std::error_code ec;
    if (saved.active_project && std::filesystem::is_directory(saved.active_project->cwd, ec)) {
        project = saved.active_project->name;
        cwd = saved.active_project->cwd;
    }
    return rt::RuntimeParts{std::move(library), std::move(harnesses), services.node.key,
                            std::move(cwd),     std::move(project),   saved.harness.approval_mode};
}

AppParts machine_parts(const Services& services, std::shared_ptr<session::Library> library) {
    const auto& node = services.node.key;
    const auto& omp_dir = services.storage.dirs.omp;
    auto harnesses = harness::detect(node, omp_dir);
    // The backends below run the configured executables.
    if (auto config = services.storage.config.load())
        (void)harness::set_executables(harnesses, config->harness.executables, node, omp_dir);
    auto runtime = runtime_parts(services, std::move(library), std::move(harnesses));
    auto executable = [&](std::string_view id) -> std::optional<std::filesystem::path> {
        if (const auto* h = runtime.harnesses.get(id)) return h->executable;
        return std::nullopt;
    };
    omp::OmpLaunch launch;
    launch.executable = executable(runtime.harnesses.roles_id()).value_or(omp::OmpLaunch{}.executable);
    launch.cwd = runtime.cwd;
    auto models = adapters::models_parts(launch, runtime.harnesses, loa::harness::factories({node, omp_dir}));
    // Digests come from omp's `smol` role.
    auto summarizer = std::make_shared<adapters::OmpSummarizers>(
        omp::OneShot(executable("omp").value_or("omp"), services.storage.dirs.data));
    auto home = loa::runtime::fs::home().value_or(".");
    return AppParts{std::move(runtime), std::move(models), std::move(summarizer), std::move(home),
                    std::make_shared<account_support::SystemUrlOpener>()};
}

// ---- construction ----------------------------------------------------------------------------

std::expected<std::pair<std::unique_ptr<App>, Task>, std::string> App::make(std::optional<std::string> first_arg) {
    auto loaded = Services::load(std::move(first_arg));
    if (!loaded) return std::unexpected(loaded.error());
    auto [services, warnings] = std::move(*loaded);
    auto library = open_library(services.storage.dirs, services.node);
    if (!library) return std::unexpected(library.error());
    auto parts = machine_parts(services, std::move(*library));
    auto built = build(std::move(services), std::move(parts));
    if (!warnings.empty()) {
        std::string text;
        for (const auto& w : warnings) text += (text.empty() ? "" : "; ") + w;
        built.first->ctx.notify(std::move(text));
    }
    return built;
}

App::App(Services services, rt::RuntimeParts runtime, Backends backends, shell::layout::State shell_state)
    : ctx(std::move(services), rt::Runtime(std::move(runtime)), std::move(backends), std::move(shell_state),
          ui::session::composer::State{}, shell::Keymap::defaults(palette::current_platform())),
      palette(palette::current_platform()),
      files(ctx.runtime.cwd(), std::make_shared<ui::app::LocalFs>()),
      terminal(ui::terminal::Metrics{}, ui::terminal::LocalShell::login_shell(ctx.runtime.cwd())) {}

std::pair<std::unique_ptr<App>, Task> App::build(Services services, AppParts parts) {
    auto saved = services.storage.config.load().value_or(settings::app::Config{});
    auto [shell_state, setup] = shell::layout::State::make(view::to_view(saved.appearance));
    Backends backends{models::Backend(std::move(parts.models)), std::move(parts.summarizer), std::move(parts.home),
                      std::move(parts.opener)};
    std::unique_ptr<App> app(
        new App(std::move(services), std::move(parts.runtime), std::move(backends), std::move(shell_state)));
    auto task = app->start(std::move(parts.areas), std::move(setup));
    return {std::move(app), std::move(task)};
}

Task App::start(std::vector<AreaBoot> boots, shell::Task setup) {
    auto saved = ctx.services.storage.config.load().value_or(settings::app::Config{});
    auto& sessions = ctx.runtime.sessions_mut();
    sessions.harness = ctx.runtime.harnesses().default_id();
    ctx.composer.set_harnesses(harness::options(ctx.runtime.harnesses()), opt_view(ctx.runtime.sessions().harness));
    std::string problems;
    for (const auto& error : ctx.shell.theme_state().errors())
        problems += (problems.empty() ? "Theme: " : "; Theme: ") + error.to_string();
    if (!problems.empty()) ctx.notify(problems);
    ctx.shell.set_project_tabs(saved.project_tabs);
    // The project selected last run, while its folder still exists, is the active one from the start.
    std::error_code ec;
    if (saved.active_project && std::filesystem::is_directory(saved.active_project->cwd, ec))
        ctx.shell.set_active_project(saved.active_project->name);
    else ctx.shell.set_active_project(std::nullopt);
    ctx.shell.sidebar_mut().set_pins({saved.pinned.begin(), saved.pinned.end()});
    ctx.shell.sidebar_mut().set_pinned_projects({saved.pinned_projects.begin(), saved.pinned_projects.end()});
    auto library = ctx.runtime.library();
    auto [registry, area_tasks] = Registry::make(ctx, std::move(boots));
    areas = std::move(registry);
    ctx.slash = slash_rows(app_commands());
    for (auto& row : areas.slash_rows()) ctx.slash.push_back(std::move(row));
    ctx.composer.set_commands(ctx.slash);
    std::vector<loa::view::shell::PanelTab> shown;
    for (auto& tab : tabs())
        if (tab.shown) shown.push_back({std::move(tab.id), std::move(tab.title), tab.icon});
    ctx.shell.set_tabs(std::move(shown));
    const auto cwd = ctx.runtime.cwd();
    ctx.composer.set_workspace(chip(*ctx.services.host.git, cwd));
    remember(cwd);
    refresh_commands();
    std::vector<Task> tasks;
    tasks.push_back(Task::pane(ui_op::Pane::Target::Shell, setup));
    tasks.push_back(std::move(area_tasks));
    tasks.push_back(working_diff());
    // The sidebar fills in once the sessions on disk have been read.
    tasks.push_back(blocking([library] { return rt::recents(*library); },
                             [](rt::Recents recents) -> Message {
                                 return msg::Recents{std::move(recents.summaries), std::move(recents.problems),
                                                     std::move(recents.empty)};
                             }));
    tasks.push_back(settle());
    tasks.push_back(Task::done(msg::Usage{usage::msg::Refresh{}}));
    return Task::batch(std::move(tasks));
}

Task App::open_main() {
    const auto id = windows.next++;
    windows.main = id;
    ctx.composer.set_window(id);
    using ui::kit::theme::LayoutRole;
    using ui::kit::theme::width;
    std::vector<Task> tasks;
    tasks.push_back(Task::op(ui_op::OpenWindow{id, width(LayoutRole::MainWidth), width(LayoutRole::MainHeight)}));
    if (auto* models = areas.port<ModelsPort>()) tasks.push_back(models->read_catalog());
    return Task::batch(std::move(tasks));
}

// ---- tabs and commands -----------------------------------------------------------------------

std::vector<PanelTab> App::tabs() const {
    std::vector<PanelTab> out;
    for (const auto* pane : {&ui::panel::DIFF_PANE, &ui::panel::FILES_PANE})
        out.push_back({std::string(pane->id), std::string(pane->title), pane->icon, true});
    out.push_back({std::string(ui::terminal::TERMINAL_PANE.id), std::string(ui::terminal::TERMINAL_PANE.title),
                   ui::terminal::TERMINAL_PANE.icon, true});
    for (const auto& area : areas.iter())
        for (auto& tab : area->panel_tabs()) out.push_back(std::move(tab));
    return out;
}

bool App::list_tab(std::string_view id) {
    const auto& listed = ctx.shell.tabs();
    if (std::ranges::any_of(listed, [&](const auto& tab) { return tab.id == id; })) return true;
    auto all = tabs();
    auto it = std::ranges::find_if(all, [&](const auto& tab) { return tab.id == id; });
    if (it == all.end()) {
        ctx.notify("There is no " + std::string(id) + " tab");
        return false;
    }
    ctx.shell.add_tab({std::move(it->id), std::move(it->title), it->icon});
    return true;
}

Task App::open_tab(std::string_view id) {
    if (!list_tab(id)) return Task::none();
    ctx.shell.show_tab(id);
    return Task::pane(ui_op::Pane::Target::Shell, ctx.shell.reveal_active_tab());
}

Task App::reveal_tab(std::string_view id) {
    if (!ctx.shell.panel_open()) return open_tab(id);
    list_tab(id);
    return Task::none();
}

void App::refresh_commands() {
    std::vector<palette::Command> commands;
    for (const auto& spec : app_commands()) {
        auto rows = spec.palette(*this, ctx);
        commands.insert(commands.end(), std::make_move_iterator(rows.begin()), std::make_move_iterator(rows.end()));
    }
    for (const auto& area : areas.iter()) {
        auto rows = area->commands(ctx);
        commands.insert(commands.end(), std::make_move_iterator(rows.begin()), std::make_move_iterator(rows.end()));
    }
    for (auto& command : commands)
        if (!command.shortcut) command.shortcut = ctx.keymap.chord_for(command.id);
    if (palette.commands() != commands) palette.set_commands(std::move(commands));
}

// ---- lifecycle and settle --------------------------------------------------------------------

Task App::lifecycle(const std::function<Task(SessionController&, Ctx&, Registry&)>& call) {
    auto task = call(sessions, ctx, areas);
    std::vector<Task> tasks;
    tasks.push_back(std::move(task));
    tasks.push_back(after());
    return Task::batch(std::move(tasks));
}

Task App::after() {
    std::vector<Task> tasks;
    auto pending = std::exchange(sessions.after, {});
    for (auto& each : pending) {
        tasks.push_back(std::visit(
            overloaded{
                [&](lifecycle::after::Present& p) { return follow(std::move(p.cwd)); },
                [&](lifecycle::after::Diff&) { return working_diff(); },
                [&](lifecycle::after::RefreshFiles&) {
                    files.refresh();
                    return Task::none();
                },
                [&](lifecycle::after::TurnEnded&) {
                    // A role switch due for the session goes before its queued prompts.
                    if (const auto& id = ctx.runtime.sessions().active_id())
                        if (auto* port = areas.port<SessionsPort>(); port && port->hold(id->as_str())) return Task::none();
                    // A queued `/` command runs like a typed one.
                    auto next = ctx.composer.turn_ended();
                    return composer_event(std::move(next));
                },
                [&](lifecycle::after::Continue& c) {
                    std::vector<Task> both;
                    if (c.folder) both.push_back(set_folder(std::move(*c.folder)));
                    auto prompt = std::move(c.prompt);
                    both.push_back(lifecycle([&](SessionController& s, Ctx& x, Registry& a) {
                        return s.run(x, a, rt::command::Start{std::move(prompt)});
                    }));
                    return Task::batch(std::move(both));
                },
            },
            each));
    }
    tasks.push_back(settle());
    return Task::batch(std::move(tasks));
}

Task App::settle() {
    std::vector<Task> tasks;
    for (;;) {
        if (!ctx.transitions().empty()) {
            auto transitions = std::exchange(ctx.transitions(), {});
            for (auto& transition : transitions)
                tasks.push_back(lifecycle([&](SessionController& s, Ctx& x, Registry& a) {
                    return s.apply(x, a, std::move(transition));
                }));
            continue;
        }
        if (ctx.requests().empty()) break;
        auto requests = std::exchange(ctx.requests(), {});
        for (auto& request : requests) {
            tasks.push_back(std::visit(
                overloaded{
                    [&](request::NewSession&) {
                        return lifecycle([](SessionController& s, Ctx& x, Registry& a) {
                            return s.run(x, a, rt::command::Start{std::nullopt});
                        });
                    },
                    [&](request::OpenSession& r) {
                        return lifecycle([&](SessionController& s, Ctx& x, Registry& a) {
                            return s.run(x, a, rt::command::Open{SessionId(r.id)});
                        });
                    },
                    [&](request::DetachTranscript& r) {
                        return detach_transcript(std::move(r.key), std::move(r.title), std::move(r.dom));
                    },
                    [&](request::Prompt& r) {
                        return lifecycle([&](SessionController& s, Ctx& x, Registry& a) {
                            return s.send(x, a, std::move(r.prompt));
                        });
                    },
                    [&](request::ReleaseRoleQueue& r) {
                        // A turn that started meanwhile releases the queue when it ends.
                        const auto* active = ctx.runtime.sessions().active();
                        if (active && active->id() == std::string_view(r.id) && !active->is_busy())
                            return composer_event(ctx.composer.turn_ended());
                        return Task::none();
                    },
                    [&](request::OpenTab& r) { return open_tab(r.id); },
                    [&](request::RevealTab& r) { return reveal_tab(r.id); },
                    [&](request::Spawn& r) {
                        return lifecycle([&](SessionController& s, Ctx& x, Registry& a) {
                            return s.run(x, a, std::move(r.spawn));
                        });
                    },
                    [&](request::ActivateProject& r) { return activate_project(std::move(r.project), std::move(r.cwd)); },
                    [&](request::ForgetProject& r) { return forget_project(r.project); },
                    [&](request::Forward& r) { return update(std::move(r.message)); },
                },
                request));
        }
    }
    return Task::batch(std::move(tasks));
}

Task App::command(const palette::CommandId& command_id) {
    const auto& id = command_id.as_str();
    if (auto found = find_command(app_commands(), id)) return found->first->run(*this, found->second);
    Area* area = nullptr;
    if (auto dot = id.find('.'); dot != std::string::npos) area = areas.find(std::string_view(id).substr(0, dot));
    std::optional<Task> task;
    if (area) task = area->run(id, ctx);
    if (!task) {
        ctx.notify("Unknown command " + id);
        return Task::none();
    }
    std::vector<Task> tasks;
    tasks.push_back(std::move(*task));
    tasks.push_back(settle());
    return Task::batch(std::move(tasks));
}

std::optional<Task> App::slash(std::string_view text) {
    if (auto found = find_slash(app_commands(), text)) return found->first->run(*this, found->second);
    for (auto& area : areas.iter_mut()) {
        if (auto task = area->slash(text, ctx)) {
            std::vector<Task> tasks;
            tasks.push_back(std::move(*task));
            tasks.push_back(settle());
            return Task::batch(std::move(tasks));
        }
    }
    return std::nullopt;
}

Task App::composer_event(std::optional<loa::view::session::composer::Intent> event) {
    if (!event) return Task::none();
    auto routed = compose(std::move(*event));
    if (!routed) return Task::none();
    return intent(Intent{std::move(*routed)});
}

Task App::intent(Intent i) {
    return std::visit(
        overloaded{
            [&](intent::Start&) {
                return lifecycle([](SessionController& s, Ctx& x, Registry& a) {
                    return s.run(x, a, rt::command::Start{std::nullopt});
                });
            },
            [&](intent::OpenSession& o) {
                return lifecycle([&](SessionController& s, Ctx& x, Registry& a) {
                    return s.run(x, a, rt::command::Open{SessionId(o.id)});
                });
            },
            [&](intent::Submit& s) {
                if (auto task = slash(s.prompt.text)) {
                    // Not a turn: the composer is ready again, and a message queued behind the
                    // command goes out now.
                    std::vector<Task> tasks;
                    tasks.push_back(std::move(*task));
                    tasks.push_back(composer_event(ctx.composer.turn_ended()));
                    return Task::batch(std::move(tasks));
                }
                if (!ctx.runtime.sessions().active())
                    return lifecycle([&](SessionController& c, Ctx& x, Registry& a) {
                        return c.run(x, a, rt::command::Start{std::move(s.prompt)});
                    });
                return lifecycle(
                    [&](SessionController& c, Ctx& x, Registry& a) { return c.send(x, a, std::move(s.prompt)); });
            },
            [&](intent::Interrupt&) {
                return lifecycle([](SessionController& s, Ctx& x, Registry& a) {
                    return s.run(x, a, rt::command::Interrupt{});
                });
            },
            [&](intent::Access& acc) {
                return lifecycle(
                    [&](SessionController& s, Ctx& x, Registry& a) { return s.set_access(x, a, std::move(acc.id)); });
            },
            [&](intent::Files&) {
                files.refresh();
                std::vector<std::string> paths;
                for (const auto& path : files.visible()) paths.push_back(path.string());
                ctx.composer.set_files(std::move(paths));
                return Task::none();
            },
            [&](intent::Tab& t) { return open_tab(t.id); },
            [&](intent::Folder& f) { return switch_folder(std::move(f.path)); },
            [&](intent::Browse&) { return browse(); },
            [&](intent::Usage&) {
                windows.overlay = overlay::Usage{};
                return Task::done(msg::Usage{usage::msg::Refresh{}});
            },
            [&](intent::Notice& n) {
                ctx.notify(std::string(n.text));
                return Task::none();
            },
            [&](intent::PasteImage&) { return paste_image(); },
            [&](intent::ProjectPicked& p) { return project_picked(std::move(p.project)); },
            [&](intent::CloseProjectTab& c) {
                close_project_tab(c.project);
                return Task::none();
            },
            [&](intent::Theme& t) {
                // The kit tokens are global: every view restyles from them, so only the choice is saved.
                if (t.picked)
                    ctx.edit_config("settings", [&](settings::app::Config& config) {
                        config.appearance.theme = view::theme_from_view(*t.picked);
                    });
                return Task::none();
            },
            [&](intent::SavePins&) {
                const auto& pins = ctx.shell.sidebar().pins();
                ctx.edit_config("settings",
                                [&](settings::app::Config& config) { config.pinned = {pins.begin(), pins.end()}; });
                return Task::none();
            },
            [&](intent::SaveProjectPins&) {
                const auto& pins = ctx.shell.sidebar().pinned_projects();
                ctx.edit_config("settings", [&](settings::app::Config& config) {
                    config.pinned_projects = {pins.begin(), pins.end()};
                });
                return Task::none();
            },
            [&](intent::Forward& f) { return update(std::move(f.message)); },
        },
        i.value);
}

// ---- update ----------------------------------------------------------------------------------

Task App::update(Message message) {
    auto task = handle(std::move(message));
    // The sidebar and top bar follow a project page in the center.
    std::optional<std::string> shown;
    if (ctx.screen() == area_id::PROJECTS)
        if (auto* projects = areas.port<ProjectsPort>()) shown = projects->shown();
    ctx.shell.set_shown_project(std::move(shown));
    sessions.sync_access(ctx);
    sync_project_tabs();
    return task;
}

Task App::sync_after_area(std::string_view id) {
    if (id == area_id::MODELS || id == area_id::HARNESS)
        if (auto* workflows = areas.port<WorkflowsPort>()) workflows->sync(ctx);
    return settle();
}

Task App::handle(Message message) {
    using Target = ui_op::Pane::Target;
    return std::visit(
        overloaded{
            [&](msg::Shell& m) {
                auto [task, event] = ctx.shell.update(std::move(m.message));
                if (event)
                    if (const auto* s = std::get_if<shell_intent::Sidebar>(&*event))
                        if (std::holds_alternative<nav::NewSession>(s->intent) ||
                            std::holds_alternative<nav::OpenSession>(s->intent) ||
                            std::holds_alternative<nav::OpenProject>(s->intent))
                            windows.overlay.reset();
                std::vector<Task> tasks;
                tasks.push_back(Task::pane(Target::Shell, task));
                if (event)
                    if (auto routed = route(std::move(*event))) tasks.push_back(intent(Intent{std::move(*routed)}));
                return Task::batch(std::move(tasks));
            },
            [&](msg::Chat& m) {
                if (std::holds_alternative<ui::session::transcript::msg::Copy>(m.message) &&
                    (palette.is_open() || windows.overlay))
                    return Task::none();
                auto [task, event] = sessions.chat.update(std::move(m.message));
                std::vector<Task> tasks;
                tasks.push_back(Task::chat(std::move(task)));
                if (event)
                    tasks.push_back(update(area_message(area_id::SESSIONS, to::sessions::Chat{std::move(*event)})));
                return Task::batch(std::move(tasks));
            },
            [&](msg::Composer& m) {
                auto [task, event] = ctx.composer.update(std::move(m.message));
                std::vector<Task> tasks;
                tasks.push_back(Task::composer(task));
                tasks.push_back(composer_event(std::move(event)));
                return Task::batch(std::move(tasks));
            },
            [&](msg::Files& m) {
                auto [task, event] = files.update(std::move(m.message));
                if (event)
                    if (const auto* open = std::get_if<loa::view::panel::files_intent::Open>(&*event))
                        ctx.notify(open->path.string());
                return Task::pane(Target::Files, task);
            },
            [&](msg::Diff& m) { return Task::pane(Target::Diff, diff.update(std::move(m.message)).first); },
            [&](msg::Terminal& m) { return Task::terminal(terminal.update(std::move(m.message))); },
            [&](msg::RunCommand& m) { return command(palette::CommandId(std::move(m.id))); },
            [&](msg::Palette& m) {
                const bool opening = std::holds_alternative<palette::msg::Open>(m.message) ||
                                     std::holds_alternative<palette::msg::OpenShortcuts>(m.message) ||
                                     (std::holds_alternative<palette::msg::Toggle>(m.message) && !palette.is_open());
                if (opening) refresh_commands();
                auto [task, event] = palette.update(std::move(m.message));
                std::vector<Task> tasks;
                tasks.push_back(Task::pane(Target::Palette, task));
                if (event)
                    if (const auto* run = std::get_if<palette::event::Run>(&*event)) tasks.push_back(command(run->id));
                return Task::batch(std::move(tasks));
            },
            [&](msg::Keyboard& m) {
                const auto* named = std::get_if<palette::Named>(&m.key.key);
                const auto mods = m.key.modifiers;
                if (named && *named == palette::Named::Escape && !mods.contains(palette::Modifiers::ctrl()) &&
                    !mods.contains(palette::Modifiers::alt()) && !mods.contains(palette::Modifiers::logo()))
                    return escape();
                // The palette is modal: only its own bindings apply while it shows.
                const auto scope = palette.is_open() ? palette::Scope::Palette : palette::Scope::Global;
                if (const auto* id = ctx.keymap.resolve(m.key, scope)) return command(palette::CommandId(*id));
                return Task::none();
            },
            [&](msg::Tick&) { return Task::none(); },
            [&](msg::Usage& m) {
                return std::visit(
                    overloaded{
                        [&](usage::msg::Refresh&) {
                            auto source = ctx.services.host.usage;
                            return perform([source] { return source->fetch(); },
                                           [](std::expected<std::string, std::string> json) -> Message {
                                               return msg::Usage{usage::msg::Loaded{std::move(json)}};
                                           });
                        },
                        [&](usage::msg::Loaded& l) {
                            if (l.json) usage = std::move(*l.json);
                            else if (windows.overlay && std::holds_alternative<overlay::Usage>(*windows.overlay))
                                ctx.notify(l.json.error());
                            return Task::none();
                        },
                        [&](usage::msg::Spend& s) {
                            auto source = ctx.services.host.usage;
                            return perform([source, target = std::move(s.target)] { return source->spend(target); },
                                           [](std::expected<void, std::string> result) -> Message {
                                               return msg::Usage{usage::msg::Spent{std::move(result)}};
                                           });
                        },
                        [&](usage::msg::Spent& s) {
                            if (!s.result) {
                                ctx.notify(s.result.error());
                                return Task::none();
                            }
                            return Task::done(msg::Usage{usage::msg::Refresh{}});
                        },
                        [&](usage::msg::Toggle&) { return Task::none(); },
                        [&](usage::msg::Close&) {
                            windows.overlay.reset();
                            return Task::none();
                        },
                    },
                    m.message);
            },
            [&](msg::Window& m) { return window_update(std::move(m.message)); },
            [&](msg::FolderChosen& m) {
                if (!m.folder) {
                    ctx.notify(m.folder.error());
                    return Task::none();
                }
                if (!*m.folder) return Task::none();
                return switch_folder(std::move(**m.folder));
            },
            [&](msg::DetachedChat& m) {
                auto it = sessions.detached_transcripts.find(m.session);
                if (it == sessions.detached_transcripts.end()) return Task::none();
                auto [task, event] = it->second.chat.update(std::move(m.message));
                std::vector<Task> tasks;
                tasks.push_back(Task::chat(std::move(task), m.session));
                // Rewinds act on the active session, so only links pass on.
                if (event && std::holds_alternative<loa::view::session::transcript::intent::OpenLink>(*event))
                    tasks.push_back(update(area_message(area_id::SESSIONS, to::sessions::Chat{std::move(*event)})));
                return Task::batch(std::move(tasks));
            },
            [&](msg::OpenRecent& m) {
                return lifecycle([&](SessionController& s, Ctx& x, Registry& a) {
                    return s.run(x, a, rt::command::Open{SessionId(m.id)});
                });
            },
            [&](msg::Recents& m) {
                sessions.recents_loaded(ctx, std::move(m.summaries), std::move(m.problems), std::move(m.empty));
                return Task::none();
            },
            [&](msg::ImagePasted& m) { return image_pasted(std::move(m.pasted)); },
            [&](msg::FileDropped& m) { return file_dropped(m.path); },
            [&](msg::Session& m) {
                return lifecycle([&](SessionController& s, Ctx& x, Registry& a) {
                    return s.update(x, a, std::move(m.message));
                });
            },
            [&](msg::WorkingDiff& m) { return show_diff(std::move(m.diff)); },
            [&](msg::Area& m) {
                // The models screen's own close button, drawn inside Configs or Settings.
                if (m.message.is<to::models::Close>() && windows.overlay &&
                    (std::holds_alternative<overlay::Settings>(*windows.overlay) ||
                     std::holds_alternative<overlay::Configs>(*windows.overlay))) {
                    windows.overlay.reset();
                    ctx.close_screen();
                    return Task::none();
                }
                std::vector<Task> tasks;
                tasks.push_back(areas.update(m.id, std::move(m.message), ctx));
                tasks.push_back(sync_after_area(m.id));
                return Task::batch(std::move(tasks));
            },
        },
        message);
}

Task App::escape() {
    if (palette.is_open()) return update(msg::Palette{palette::msg::Close{}});
    // A file's full screen leaves before the Configs window it is drawn in.
    if (windows.docs.full && windows.overlay && std::holds_alternative<overlay::Configs>(*windows.overlay)) {
        windows.docs.full = false;
        return Task::none();
    }
    if (windows.overlay) {
        windows.overlay.reset();
        return Task::none();
    }
    if (auto pane = ctx.shell.full()) return update(msg::Shell{shell::layout::msg::ToggleFull{*pane}});
    if (auto* decisions = areas.port<DecisionsPort>(); decisions && decisions->focused())
        return update(decisions->dismiss());
    if (ctx.screen()) {
        ctx.close_screen();
        return Task::none();
    }
    // Interrupt the harness itself, even before the turn-start patch arrives.
    return lifecycle(
        [](SessionController& s, Ctx& x, Registry& a) { return s.run(x, a, rt::command::Interrupt{}); });
}

Subscriptions App::subscription() const {
    const auto* active = ctx.runtime.sessions().active();
    const bool busy = active && active->is_busy();
    Subscriptions subs = loa::app::sessions(ctx.runtime.sessions());
    if (busy) subs.push_back(subscription::Every{"tick", TICK, msg::Tick{}});
    subs.push_back(subscription::Every{"usage", USAGE_REFRESH, msg::Usage{usage::msg::Refresh{}}});
    for (const auto& area : areas.iter())
        for (auto& sub : area->subscription()) subs.push_back(std::move(sub));
    return subs;
}

std::string App::title(WindowId id) const {
    for (const auto& [window, what] : windows.detached)
        if (window == id) return detached_title(what) + " — Loa";
    return "Loa";
}

} // namespace loa::app
