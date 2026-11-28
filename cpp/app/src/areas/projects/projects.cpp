#include "projects.hpp"

#include <algorithm>
#include <array>
#include <cerrno>
#include <chrono>
#include <cstring>

#include <fcntl.h>
#include <sys/stat.h>
#include <unistd.h>

#include <asio.hpp>

#include "loa/app/perform.hpp"
#include "loa/app/view/settings/projects.hpp"
#include "loa/app/watch.hpp"
#include "loa/core/schema.hpp"
#include "loa/projects/settings.hpp"
#include "loa/projects/source.hpp"
#include "loa/ui/settings/projects/view.hpp"
#include "loa/ui/shell/sidebar.hpp"

namespace loa::app::areas::projects {

namespace {

namespace lp = loa::projects;
namespace vs = loa::view::settings::projects;
using app::view::settings::overview_to_view;
using app::view::settings::settings_from_view;
using app::view::settings::settings_to_view;

template <class... F>
struct overloaded : F... {
    using F::operator()...;
};

// How often the subscription looks for rebuild outcomes.
constexpr std::chrono::milliseconds OUTCOME_POLL{250};

// Worker threads of the area's own executor, which runs the rebuild driver.
constexpr std::size_t DRIVER_WORKERS = 1;

// Bytes copied per read when importing a terminal session.
constexpr std::size_t COPY_CHUNK = 64 * 1024;

loa::app::Message wrap(projects::Message m) { return State::message(std::move(m)); }

template <class T>
std::expected<T, std::string> text(lp::Result<T> result) {
    if (result) return std::move(*result);
    return std::unexpected(result.error().to_string());
}

std::expected<void, std::string> text(lp::Result<> result) {
    if (result) return {};
    return std::unexpected(result.error().to_string());
}

bool is_dir(const std::filesystem::path& path) {
    std::error_code ec;
    return std::filesystem::is_directory(path, ec);
}

bool exists(const std::filesystem::path& path) {
    std::error_code ec;
    return std::filesystem::exists(path, ec);
}

loa::runtime::Task poll_outcomes(std::shared_ptr<lp::Outcomes> outcomes, Task::Emit send, std::stop_token stop) {
    asio::steady_timer timer(co_await asio::this_coro::executor);
    while (!stop.stop_requested()) {
        while (auto outcome = outcomes->try_recv()) send(wrap(msg::Rebuild{std::move(*outcome)}));
        timer.expires_after(OUTCOME_POLL);
        co_await timer.async_wait(asio::use_awaitable);
    }
}

// Copies the native session file into the staging directory, readable by the owner only.
std::expected<void, std::string> copy_session(const std::filesystem::path& source, const std::filesystem::path& target) {
    auto fail = [](const std::filesystem::path& path) { return std::unexpected(path.string() + ": " + std::strerror(errno)); };
    int in = ::open(source.c_str(), O_RDONLY | O_CLOEXEC);
    if (in < 0) return fail(source);
    int out = ::open(target.c_str(), O_WRONLY | O_CREAT | O_EXCL | O_CLOEXEC, S_IRUSR | S_IWUSR);
    if (out < 0) {
        auto error = fail(target);
        ::close(in);
        return error;
    }
    std::array<char, COPY_CHUNK> buffer{};
    std::expected<void, std::string> result;
    for (;;) {
        auto n = ::read(in, buffer.data(), buffer.size());
        if (n == 0) break;
        if (n < 0) {
            if (errno == EINTR) continue;
            result = fail(source);
            break;
        }
        for (ssize_t done = 0; done < n;) {
            auto w = ::write(out, buffer.data() + done, static_cast<std::size_t>(n - done));
            if (w < 0) {
                if (errno == EINTR) continue;
                result = fail(target);
                break;
            }
            done += w;
        }
        if (!result) break;
    }
    if (result && ::fsync(out) != 0) result = fail(target);
    ::close(out);
    ::close(in);
    return result;
}

} // namespace

// ---- History ---------------------------------------------------------------------------------

bool History::begin_change(std::string_view project) {
    if (busy == project) return false;
    return changing.emplace(project).second;
}

void History::end_change(std::string_view project) {
    if (auto it = changing.find(project); it != changing.end()) changing.erase(it);
}

// ---- State -----------------------------------------------------------------------------------

State::State(std::unique_ptr<loa::runtime::Executor> executor, lp::ProjectService service, lp::LiveSessions sessions,
             std::shared_ptr<lp::Outcomes> outcomes,
             std::pair<std::vector<contracts::HostToolSpec>, std::shared_ptr<const contracts::HostTools>> tools)
    : executor_(std::move(executor)), service_(std::move(service)), sessions_(std::move(sessions)), tools_(std::move(tools)),
      outcomes_(std::move(outcomes)) {}

State::~State() = default;

std::pair<std::unique_ptr<State>, Task> State::make(Ctx& ctx) {
    return with(ctx, ctx.backends.summarizer, lp::ServiceConfig{});
}

std::pair<std::unique_ptr<State>, Task> State::with(Ctx& ctx, std::shared_ptr<const lp::Summarizers> summarizer,
                                                    lp::ServiceConfig config) {
    const auto& library = ctx.runtime.library();
    lp::LiveSessions sessions(library->bus(), library->store());
    auto settings = std::make_shared<lp::FileSettingsStore>(ctx.services.storage.dirs.data / "projects");
    auto executor = std::make_unique<loa::runtime::Executor>(DRIVER_WORKERS);
    auto [service, outcomes] = lp::ProjectService::start(sessions, settings, std::make_shared<lp::LocalOnly>(),
                                                         std::move(summarizer), config, executor->context());
    auto tools = service.host_tools();
    for (const auto& spec : tools.first) ctx.set_app_tool(loa::view::panel::ToolInfo::from(spec));
    std::unique_ptr<State> state(
        new State(std::move(executor), std::move(service), std::move(sessions), std::move(outcomes), std::move(tools)));
    std::vector<Task> tasks;
    tasks.push_back(state->list());
    if (const auto& project = ctx.runtime.project()) tasks.push_back(state->refresh_context(*project));
    return {std::move(state), Task::batch(std::move(tasks))};
}

std::optional<std::string> State::shown() const {
    if (creating_) return std::nullopt;
    if (!screen_) return std::nullopt;
    return screen_->project();
}

std::optional<std::string_view> State::context(std::string_view project) const {
    auto it = contexts_.find(project);
    if (it == contexts_.end() || !it->second) return std::nullopt;
    return std::string_view(*it->second);
}

Task State::list() const {
    return blocking([service = service_] { return text(service.projects()); },
                    [](auto result) { return wrap(msg::Listed{std::move(result)}); });
}

Task State::load_overview(std::string project) {
    auto request = ++overview_request_;
    return blocking(
        [service = service_, project]() -> std::expected<loa::settings::ProjectOverview, std::string> {
            return text(service.overview(project));
        },
        [request, project](auto result) { return wrap(msg::Overview{request, project, std::move(result)}); });
}

Task State::refresh_context(std::string project) const {
    return blocking([service = service_, project] { return text(service.context(project, std::nullopt, "")); },
                    [project](auto result) { return wrap(msg::Context{project, std::move(result)}); });
}

Task State::refresh_contexts() const {
    std::vector<Task> tasks;
    for (const auto& [project, _] : contexts_) tasks.push_back(refresh_context(project));
    return Task::batch(std::move(tasks));
}

Task State::reload_screen() {
    if (!screen_) return Task::none();
    return load_overview(screen_->project());
}

std::optional<std::filesystem::path> State::folder(std::string_view project) const {
    auto it = std::ranges::find(projects_, project, &lp::ProjectSummary::name);
    if (it == projects_.end() || !it->cwd) return std::nullopt;
    std::filesystem::path folder(*it->cwd);
    if (!is_dir(folder)) return std::nullopt;
    return folder;
}

void State::sync_chip(Ctx& ctx) const {
    std::optional<std::string> current;
    if (const auto* session = ctx.runtime.sessions().active()) {
        if (const auto& summary = session->summary()) current = summary->project;
    } else {
        current = ctx.runtime.project();
    }
    ctx.composer.set_projects(names(), std::move(current));
}

Task State::open(std::string project, Ctx& ctx) {
    auto f = folder(project);
    return open_in(std::move(project), std::move(f), ctx);
}

Task State::open_in(std::string project, std::optional<std::filesystem::path> folder, Ctx& ctx) {
    ctx.request(request::ActivateProject{project, std::move(folder)});
    return show(std::move(project), ctx);
}

Task State::show(std::string project, Ctx& ctx) {
    creating_.reset();
    if (!screen_ || screen_->project() != project) {
        screen_.emplace(project);
        ideas_file_.reset();
    }
    ctx.show_screen(ID);
    clear_history();
    std::vector<Task> tasks;
    tasks.push_back(load_overview(project));
    tasks.push_back(refresh_context(std::move(project)));
    return Task::batch(std::move(tasks));
}

Task State::choose_folder(const Ctx& ctx, std::filesystem::path start, Pick pick) {
    auto picker = ctx.services.host.picker;
    return perform([picker, start = std::move(start)]() { return picker->pick(start); },
                   [pick = std::move(pick)](auto result) { return wrap(msg::FolderChosen{pick, std::move(result)}); });
}

Task State::screen_event(screen::Intent event, Ctx& ctx) {
    return std::visit(
        overloaded{
            [&](vs::intent::SaveSettings& s) -> Task {
                auto settings = settings_from_view(std::move(s.settings));
                std::optional<std::pair<std::optional<loa::runtime::fs::Snapshot>, std::string>> notes;
                if (s.ideas) notes.emplace(ideas_file_, std::move(*s.ideas));
                return blocking(
                    [service = service_, project = s.project, settings, notes = std::move(notes)]() {
                        std::optional<std::pair<loa::runtime::fs::Snapshot, std::string_view>> seen;
                        if (notes && notes->first) seen.emplace(*notes->first, std::string_view(notes->second));
                        auto result = service.save_settings(project, settings, std::move(seen));
                        Saving saving;
                        if (result && *result && notes)
                            saving = saving::Stored{loa::settings::Ideas{notes->second, std::move(**result)}};
                        // Notes edited while no file was seen for them, after a reload asked for one.
                        else if ((result && !*result && notes) ||
                                 (!result && result.error().kind == lp::ErrorKind::ChangedOnDisk))
                            saving = saving::IdeasChanged{};
                        else if (result)
                            saving = saving::Stored{};
                        else
                            saving = saving::Failed{result.error().to_string()};
                        return msg::Saved{project, settings, std::move(saving)};
                    },
                    [](msg::Saved saved) { return wrap(std::move(saved)); });
            },
            [&](vs::intent::ReloadIdeas&) -> Task {
                ideas_file_.reset();
                return reload_screen();
            },
            [&](vs::intent::SetExcluded& s) -> Task {
                auto project = screen_ ? screen_->project() : std::string();
                return blocking([service = service_, session = s.session, excluded = s.excluded] {
                    return text(service.set_excluded(session, excluded));
                },
                                [project](auto result) { return wrap(msg::Excluded{project, std::move(result)}); });
            },
            [&](vs::intent::OpenSession& o) -> Task {
                // The active session's chat comes back too.
                ctx.close_screen();
                ctx.request(request::OpenSession{std::move(o.session)});
                return Task::none();
            },
            [&](vs::intent::OpenOmpHistory& o) { return open_history(o.id, ctx); },
            [&](vs::intent::DeleteOmpHistory& d) { return delete_history(d.id, ctx); },
            [&](vs::intent::NewChat& n) -> Task {
                {
                    std::scoped_lock lock(new_chat_mutex_);
                    std::optional<std::filesystem::path> cwd;
                    if (n.cwd) cwd.emplace(*n.cwd);
                    new_chat_ = NewChat{std::move(n.project), std::move(cwd)};
                }
                ctx.request(request::NewSession{});
                return Task::none();
            },
            [&](vs::intent::ChangeFolder& c) -> Task {
                std::filesystem::path start = c.current ? std::filesystem::path(*c.current) : ctx.runtime.cwd();
                return choose_folder(ctx, std::move(start), pick::Project{std::move(c.project)});
            },
            [&](vs::intent::Delete& d) -> Task {
                if (!history_.begin_change(d.project)) {
                    ctx.notify("Wait for the current project operation to finish");
                    return Task::none();
                }
                return blocking([service = service_, project = d.project] { return text(service.remove(project)); },
                                [project = d.project](auto result) {
                                    return wrap(msg::Deleted{project, std::move(result)});
                                });
            },
        },
        event);
}

Task State::form_event(screen::CreateIntent event, Ctx& ctx) {
    return std::visit(
        overloaded{
            [&](vs::create_intent::ChooseFolder& c) {
                return choose_folder(ctx, std::filesystem::path(c.folder), pick::Form{});
            },
            [&](vs::create_intent::Create& c) -> Task {
                auto settings = settings_from_view(std::move(c.settings));
                if (settings.folder && !is_dir(*settings.folder)) {
                    if (creating_) creating_->failed(settings.folder->string() + " isn't a folder");
                    return Task::none();
                }
                return blocking([service = service_, name = c.name, settings] { return text(service.create(name, settings)); },
                                [name = c.name](auto result) { return wrap(msg::Created{name, std::move(result)}); });
            },
            [&](vs::create_intent::Cancel&) -> Task {
                creating_.reset();
                // Back to the project screen it was opened over, if any.
                if (!screen_) ctx.close_screen();
                return Task::none();
            },
        },
        event);
}

std::vector<std::string> State::names() const {
    std::vector<std::string> out;
    out.reserve(projects_.size());
    for (const auto& p : projects_) out.push_back(p.name);
    return out;
}

std::span<const AreaCommand<State>> State::COMMANDS() {
    static const std::array<AreaCommand<State>, 1> commands{
        AreaCommand<State>{"projects.", "", "Projects", std::nullopt,
                           Listed<State>::each_([](const State& state, const Ctx& ctx) { return state.rows(ctx); }),
                           [](State& state, std::string_view arg, Ctx& ctx) { return state.command(arg, ctx); }},
    };
    return commands;
}

std::vector<std::pair<std::string, std::string>> State::rows(const Ctx& ctx) const {
    const auto& pinned = ctx.shell.sidebar().pinned_projects();
    std::vector<std::pair<std::string, std::string>> rows;
    for (const auto& name : names()) {
        auto verb = pinned.contains(name) ? "Unpin" : "Pin";
        rows.emplace_back(std::string(OPEN_COMMAND) + name, "Open project: " + name);
        rows.emplace_back(std::string(PIN_COMMAND) + name, std::string(verb) + " project: " + name);
    }
    return rows;
}

Task State::command(std::string_view arg, Ctx& ctx) {
    if (arg.starts_with(PIN_COMMAND)) {
        ui::shell::sidebar::msg::ToggleProjectPin toggle{std::string(arg.substr(PIN_COMMAND.size()))};
        ctx.request(request::Forward{loa::app::msg::Shell{ui::shell::layout::msg::Sidebar{std::move(toggle)}}});
        return Task::none();
    }
    if (arg.starts_with(OPEN_COMMAND)) return open(std::string(arg.substr(OPEN_COMMAND.size())), ctx);
    ctx.notify("Unknown command projects." + std::string(arg));
    return Task::none();
}

Task State::update_typed(Message message, Ctx& ctx) {
    // A "New chat" that didn't start a session (no harness) is dropped.
    if (!std::holds_alternative<msg::Screen>(message)) {
        std::scoped_lock lock(new_chat_mutex_);
        new_chat_.reset();
    }
    return std::visit(
        overloaded{
            [&](to::projects::Open& o) { return open(std::move(o.project), ctx); },
            [&](msg::Listed& l) -> Task {
                if (!l.result) {
                    ctx.notify("Can't list projects: " + l.result.error());
                    return Task::none();
                }
                projects_ = std::move(*l.result);
                ctx.shell.sidebar_mut().set_projects(names());
                sync_chip(ctx);
                std::vector<Task> tasks;
                for (const auto& p : projects_)
                    if (!contexts_.contains(p.name)) tasks.push_back(refresh_context(p.name));
                return Task::batch(std::move(tasks));
            },
            [&](msg::Overview& o) -> Task {
                if (o.request != overview_request_) return Task::none();
                if (!screen_ || screen_->project() != o.project) return Task::none();
                if (!o.result) {
                    screen_->set_error("Can't load project " + o.project + ": " + o.result.error());
                    return Task::none();
                }
                auto& overview = *o.result;
                std::optional<std::filesystem::path> folder = overview.settings.folder;
                if (folder && !is_dir(*folder)) folder.reset();
                bool reload_ideas = !ideas_file_.has_value();
                bool edited = screen_->ideas_edited();
                screen_->set_overview(overview_to_view(overview), core::now_ms());
                if (reload_ideas) screen_->revert_ideas();
                if (reload_ideas || !edited) {
                    if (overview.ideas) ideas_file_ = overview.ideas->file;
                    else ideas_file_.reset();
                }
                if (!history_.matches_folder(folder)) return scan_history(std::move(folder), ctx);
                return Task::none();
            },
            [&](msg::Screen& s) -> Task {
                if (!screen_) return Task::none();
                auto event = screen_->update(std::move(s.message));
                if (!event) return Task::none();
                return screen_event(std::move(*event), ctx);
            },
            [&](msg::Saved& s) -> Task {
                if (screen_ && screen_->project() == s.project) {
                    auto settings = settings_to_view(s.settings);
                    std::visit(overloaded{
                                   [&](saving::Stored& stored) {
                                       std::optional<std::string> text;
                                       if (stored.ideas) text = stored.ideas->text;
                                       screen_->saved(std::pair{settings, std::move(text)});
                                       if (stored.ideas) ideas_file_ = std::move(stored.ideas->file);
                                   },
                                   [&](saving::IdeasChanged&) { screen_->ideas_changed_outside(settings); },
                                   [&](saving::Failed& f) { screen_->saved(std::unexpected(f.error)); },
                               },
                               s.saving);
                }
                // Also after a failure: the settings may be stored while the notes weren't.
                return refresh_context(s.project);
            },
            [&](msg::Excluded& e) -> Task {
                if (!e.result && screen_) screen_->set_error("Can't change the session's context toggle: " + e.result.error());
                std::vector<Task> tasks;
                tasks.push_back(reload_screen());
                tasks.push_back(refresh_context(e.project));
                return Task::batch(std::move(tasks));
            },
            [&](msg::Context& c) -> Task {
                if (c.result) contexts_.insert_or_assign(c.project, std::move(*c.result));
                else ctx.notify("Can't build the context of project " + c.project + ": " + c.result.error());
                return Task::none();
            },
            [&](msg::Rebuild& r) -> Task {
                return std::visit(overloaded{
                                      [&](lp::rebuild_outcome::Rebuilt&) {
                                          std::vector<Task> tasks;
                                          tasks.push_back(refresh_contexts());
                                          tasks.push_back(reload_screen());
                                          return Task::batch(std::move(tasks));
                                      },
                                      [&](lp::rebuild_outcome::Failed& f) {
                                          ctx.notify("Digest of session " + f.session + " failed: " + f.error);
                                          return reload_screen();
                                      },
                                      [&](lp::rebuild_outcome::Skipped&) { return Task::none(); },
                                  },
                                  r.outcome);
            },
            [&](to::projects::Activate& a) -> Task {
                ctx.request(request::ActivateProject{a.project, folder(a.project)});
                return Task::none();
            },
            [&](to::projects::NewChatIn& n) -> Task {
                {
                    std::scoped_lock lock(new_chat_mutex_);
                    new_chat_ = NewChat{n.project, folder(n.project)};
                }
                ctx.request(request::NewSession{});
                return Task::none();
            },
            [&](to::projects::AskDelete& a) -> Task {
                auto task = show(std::move(a.project), ctx);
                if (screen_) screen_->ask_delete();
                return task;
            },
            [&](msg::Deleted& d) -> Task {
                history_.end_change(d.project);
                if (!d.result) {
                    ctx.notify("Can't delete project " + d.project + ": " + d.result.error());
                    return Task::none();
                }
                if (auto it = contexts_.find(d.project); it != contexts_.end()) contexts_.erase(it);
                if (screen_ && screen_->project() == d.project) {
                    screen_.reset();
                    ctx.close_screen();
                }
                ctx.request(request::ForgetProject{d.project});
                return list();
            },
            [&](to::projects::New&) -> Task {
                creating_.emplace(ctx.runtime.cwd().string());
                ctx.show_screen(ID);
                // The New project view focuses its name input when it first draws.
                return Task::none();
            },
            [&](msg::Form& f) -> Task {
                if (!creating_) return Task::none();
                auto event = creating_->update(std::move(f.message));
                if (!event) return Task::none();
                return form_event(std::move(*event), ctx);
            },
            [&](msg::Created& c) -> Task {
                if (!c.result) {
                    if (creating_) creating_->failed(c.result.error());
                    else ctx.notify("Can't create project " + c.name + ": " + c.result.error());
                    return Task::none();
                }
                auto opened = open_in(c.name, std::move(*c.result), ctx);
                std::vector<Task> tasks;
                tasks.push_back(list());
                tasks.push_back(std::move(opened));
                return Task::batch(std::move(tasks));
            },
            [&](msg::FolderChosen& f) -> Task {
                if (!f.result) {
                    ctx.notify(f.result.error());
                    return Task::none();
                }
                if (!*f.result) return Task::none();
                auto chosen = std::move(**f.result);
                if (std::holds_alternative<pick::Form>(f.pick)) {
                    if (creating_) creating_->set_folder(chosen.string());
                    return Task::none();
                }
                auto project = std::get<pick::Project>(f.pick).project;
                if (!history_.begin_change(project)) {
                    ctx.notify("Wait for the current project operation to finish, then choose the folder again");
                    return Task::none();
                }
                return blocking([service = service_, project, chosen] { return text(service.set_folder(project, chosen)); },
                                [project](auto result) { return wrap(msg::FolderSet{project, std::move(result)}); });
            },
            [&](msg::FolderSet& f) -> Task {
                history_.end_change(f.project);
                if (!f.result) {
                    ctx.notify("Can't change the folder of " + f.project + ": " + f.result.error());
                    return Task::none();
                }
                // New chats in the active project start in its new folder.
                if (ctx.runtime.project() == f.project) ctx.request(request::ActivateProject{f.project, *f.result});
                if (shown() == f.project) clear_history();
                std::vector<Task> tasks;
                tasks.push_back(list());
                tasks.push_back(reload_screen());
                return Task::batch(std::move(tasks));
            },
            [&](to::projects::Move& m) -> Task {
                return blocking(
                    [service = service_, session = m.session, project = m.project] {
                        std::optional<std::string_view> p;
                        if (project) p = *project;
                        return text(service.move_session(session, p));
                    },
                    [](auto result) { return wrap(msg::Moved{std::move(result)}); });
            },
            [&](msg::Moved& m) -> Task {
                if (m.result) return list();
                ctx.notify("Can't move the session: " + m.result.error());
                return Task::none();
            },
            [&](msg::HistoryLoaded& h) -> Task {
                history_loaded(h.scan, std::move(h.result));
                return Task::none();
            },
            [&](msg::HistoryOpened& h) { return history_opened(h.scan, std::move(h.result), ctx); },
            [&](msg::HistoryDeleted& h) { return history_deleted(std::move(h.result), ctx); },
        },
        message);
}

QWidget* State::view_screen(const Ctx&, const Emit& send) const {
    if (creating_) {
        auto* view = new screen::CreateView;
        view->render(*creating_);
        QObject::connect(view, &screen::CreateView::message, view,
                         [send](const create::Message& m) { send(wrap(msg::Form{m})); });
        return view;
    }
    if (!screen_) return nullptr;
    auto* view = new screen::ProjectView;
    view->render(*screen_);
    QObject::connect(view, &screen::ProjectView::message, view,
                     [send](const screen::Message& m) { send(wrap(msg::Screen{m})); });
    return view;
}

void State::contribute(contracts::SessionConfig& config, const Target& target, const Ctx&) const {
    if (target.resume) return;
    std::optional<NewChat> chat;
    {
        std::scoped_lock lock(new_chat_mutex_);
        chat = std::exchange(new_chat_, std::nullopt);
    }
    if (chat) {
        config.project = std::move(chat->project);
        if (chat->cwd) config.cwd = std::move(*chat->cwd);
    }
    if (!config.project) return;
    auto shared = context(*config.project);
    if (!shared) return;
    if (config.context) config.context = *config.context + "\n\n" + std::string(*shared);
    else config.context = std::string(*shared);
}

std::optional<HostToolSet> State::host_tools(const contracts::SessionConfig&, const Target& target, const Ctx&) const {
    if (!target.capabilities || !target.capabilities->host_tools) return std::nullopt;
    return HostToolSet{tools_.first, tools_.second};
}

Task State::on_session_event(const SessionEvent& event, Ctx& ctx) {
    namespace se = app_runtime::session_event;
    return std::visit(
        overloaded{
            [&](const se::TurnEnded& e) -> Task {
                (void)service_.notify(e.session.as_str());
                std::vector<Task> tasks;
                tasks.push_back(list());
                tasks.push_back(reload_screen());
                return Task::batch(std::move(tasks));
            },
            [&](const se::Compaction& e) -> Task {
                (void)service_.notify(e.session.as_str());
                return Task::none();
            },
            [&](const se::Activated&) -> Task {
                sync_chip(ctx);
                return list();
            },
            [&](const se::Closed&) -> Task {
                sync_chip(ctx);
                return list();
            },
            // A session moved between projects.
            [&](const se::Patch& p) -> Task {
                const auto* set = p.patch ? std::get_if<core::op::Set>(&p.patch->op) : nullptr;
                if (!set || set->name != core::attr::PROJECT) return Task::none();
                sync_chip(ctx);
                return list();
            },
            [&](const auto&) { return Task::none(); },
        },
        event);
}

Subscriptions State::subscription() const {
    auto outcomes = outcomes_;
    Subscriptions subs;
    subs.push_back(subscription::Stream{
        ptr_key("projects-outcomes", outcomes), [outcomes](Task::Emit send, std::stop_token stop, loa::runtime::Executor& executor) {
            executor.spawn(poll_outcomes(outcomes, std::move(send), std::move(stop)));
        }});
    return subs;
}

// ---- history.rs ------------------------------------------------------------------------------

void State::clear_history() {
    ++history_.scan;
    history_.folder.reset();
    history_.rows.clear();
    if (screen_) screen_->set_omp_history(std::nullopt);
}

Task State::scan_history(std::optional<std::filesystem::path> folder, const Ctx& ctx) {
    auto scan = ++history_.scan;
    history_.folder = folder;
    history_.rows.clear();
    if (screen_) {
        screen_->set_omp_history(folder ? std::optional<vs::OmpHistory>(vs::omp_history::Loading{}) : std::nullopt);
        screen_->set_omp_history_busy(history_.busy.has_value());
    }
    if (!folder) return Task::none();
    auto backend = ctx.backends.models;
    return perform(
        [backend, sessions = sessions_, folder = std::move(*folder)]()
            -> contracts::Awaitable<std::expected<std::vector<ProjectSession>, std::string>> {
            auto files = co_await backend.files();
            if (!files) co_return std::unexpected(files.error());
            auto rows = loa::harness::omp::native::project_history(files->agent_dir, folder);
            if (!rows) co_return std::unexpected(rows.error().to_string());
            if (!rows->empty()) {
                auto imported = sessions.native_sessions(loa::harness::omp::HARNESS);
                if (!imported) co_return std::unexpected(imported.error().to_string());
                std::vector<ProjectSession> fresh;
                for (auto& row : *rows)
                    if (!imported->contains(row.id)) fresh.push_back(std::move(row));
                *rows = std::move(fresh);
            }
            co_return std::move(*rows);
        },
        [scan](auto result) { return wrap(msg::HistoryLoaded{scan, std::move(result)}); });
}

void State::history_loaded(std::uint64_t scan, std::expected<std::vector<ProjectSession>, std::string> result) {
    if (scan != history_.scan || !screen_) return;
    if (!result) {
        screen_->set_omp_history(vs::OmpHistory(vs::omp_history::Failed{result.error()}));
        return;
    }
    vs::omp_history::Ready ready;
    for (const auto& row : *result) ready.rows.push_back({row.id, row.title, row.modified});
    history_.rows = std::move(*result);
    screen_->set_omp_history(vs::OmpHistory(std::move(ready)));
}

std::optional<std::tuple<std::string, std::filesystem::path, ProjectSession>> State::claim_row(std::string_view native_id,
                                                                                            Ctx& ctx) {
    if (history_.busy || creating_ || ctx.screen() != ID) return std::nullopt;
    if (!screen_ || !history_.folder) return std::nullopt;
    auto row = std::ranges::find(history_.rows, native_id, &ProjectSession::id);
    if (row == history_.rows.end()) return std::nullopt;
    auto project = screen_->project();
    if (history_.changing.contains(project)) {
        ctx.notify("Wait for the current project operation to finish");
        return std::nullopt;
    }
    history_.busy = project;
    screen_->set_omp_history_busy(true);
    return std::tuple{std::move(project), *history_.folder, *row};
}

void State::release_row() {
    history_.busy.reset();
    if (screen_) screen_->set_omp_history_busy(false);
}

Task State::refresh_history(const Ctx& ctx) {
    if (screen_ && ctx.screen() == ID) return scan_history(history_.folder, ctx);
    return Task::none();
}

Task State::open_history(std::string_view native_id, Ctx& ctx) {
    auto claimed = claim_row(native_id, ctx);
    if (!claimed) return Task::none();
    auto [project, folder, row] = std::move(*claimed);
    auto scan = history_.scan;
    return blocking(
        [sessions = sessions_, library = ctx.runtime.library(), owner = ctx.services.node.key, project, folder,
         row]() -> std::expected<std::string, std::string> {
            auto known = sessions.native_sessions(loa::harness::omp::HARNESS);
            if (!known) return std::unexpected(known.error().to_string());
            if (auto it = known->find(row.id); it != known->end()) return it->second;
            auto cwd = loa::runtime::fs::canonicalize(folder);
            if (!cwd) return std::unexpected(cwd.error().to_string());
            auto id = app_runtime::new_session_id();
            auto imported = library->import_staged(
                id, owner, project,
                [&](const std::filesystem::path& staged, const std::filesystem::path& target,
                    core::PatchSink& sink) -> loa::session::Result<> {
                    constexpr std::string_view name = "terminal-history.jsonl";
                    auto copy = staged / name;
                    if (auto copied = copy_session(row.path, copy); !copied)
                        return std::unexpected(loa::session::Error::port(copied.error()));
                    auto session = loa::harness::omp::native::project_session(copy, *cwd);
                    if (!session) return std::unexpected(loa::session::Error::port(session.error().to_string()));
                    if (!*session || (*session)->id != row.id)
                        return std::unexpected(
                            loa::session::Error::port("The terminal session changed since this project was opened"));
                    if (auto r = loa::harness::omp::native::import(copy, sink, owner); !r)
                        return std::unexpected(loa::session::Error::adapter(r.error()));
                    // The OMP importer inserts the session root first.
                    for (auto draft : {core::Draft::set(core::NodeId{1}, std::string(loa::harness::omp::native::X_SESSION_FILE),
                                                        (target / name).string()),
                                       core::Draft::set(core::NodeId{1}, std::string(core::attr::CWD), cwd->string())})
                        if (auto committed = sink.commit(std::move(draft)); !committed)
                            return std::unexpected(loa::session::Error::commit(committed.error()));
                    return {};
                });
            if (!imported) return std::unexpected(imported.error().to_string());
            return id;
        },
        [scan](auto result) { return wrap(msg::HistoryOpened{scan, std::move(result)}); });
}

Task State::history_opened(std::uint64_t scan, std::expected<std::string, std::string> result, Ctx& ctx) {
    release_row();
    if (!result) {
        ctx.notify("Can't open OMP terminal session: " + result.error());
        return Task::none();
    }
    if (scan == history_.scan && shown() && ctx.screen() == ID) {
        ctx.close_screen();
        ctx.request(request::OpenSession{std::move(*result)});
    }
    auto refresh = refresh_history(ctx);
    std::vector<Task> tasks;
    tasks.push_back(list());
    tasks.push_back(std::move(refresh));
    return Task::batch(std::move(tasks));
}

Task State::delete_history(std::string_view native_id, Ctx& ctx) {
    auto claimed = claim_row(native_id, ctx);
    if (!claimed) return Task::none();
    auto [_, folder, row] = std::move(*claimed);
    auto backend = ctx.backends.models;
    return perform(
        [backend, folder, id = row.id]() -> contracts::Awaitable<std::expected<void, std::string>> {
            auto files = co_await backend.files();
            if (!files) co_return std::unexpected(files.error());
            auto removed = loa::harness::omp::native::delete_project_session(files->agent_dir, folder, id);
            if (!removed) co_return std::unexpected(removed.error().to_string());
            co_return std::expected<void, std::string>{};
        },
        [](auto result) { return wrap(msg::HistoryDeleted{std::move(result)}); });
}

Task State::history_deleted(std::expected<void, std::string> result, Ctx& ctx) {
    release_row();
    if (!result) ctx.notify("Can't delete OMP terminal session: " + result.error());
    return refresh_history(ctx);
}

} // namespace loa::app::areas::projects
