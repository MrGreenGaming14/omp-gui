#include "workflows.hpp"

#include <array>
#include <filesystem>

#include <QWidget>

#include "loa/app/perform.hpp"
#include "loa/app/view/settings/workflows.hpp"
#include "loa/core/schema.hpp"
#include "loa/settings/models/agents.hpp"
#include "loa/ui/settings/models/view.hpp"
#include "loa/ui/settings/workflows/view.hpp"
#include "loa/view/session.hpp"

namespace loa::app::areas::workflows {

namespace {

template <class... F>
struct overloaded : F... {
    using F::operator()...;
};

using composer_view = loa::view::session::composer::WorkflowOption;
using loa::view::session::composer::WorkflowKind;

// The harness new sessions and the active one run on.
std::optional<std::string_view> harness_of(const Ctx& ctx) {
    const auto& sessions = ctx.runtime.sessions();
    if (const auto* active = sessions.active())
        if (auto id = active->harness_id()) return id;
    if (sessions.harness) return std::string_view(*sessions.harness);
    return std::nullopt;
}

// The model role a session's `<session>` records.
std::optional<std::string> recorded_role(const app_runtime::Session& session) {
    const auto* root = session.dom().root();
    if (!root) return std::nullopt;
    auto role = root->str_attr(core::attr::MODEL_ROLE);
    return role ? std::optional<std::string>(*role) : std::nullopt;
}

shown::Error error_to_view(const settings::models::Error& error) {
    if (error.kind == settings::models::Error::Kind::ChangedOnDisk) return shown::error::ChangedOnDisk{};
    return shown::error::Message{error.to_string()};
}

contracts::Awaitable<std::expected<Data, std::string>> read_data(models::Backend backend, std::filesystem::path project) {
    auto files = co_await backend.files();
    if (!files) co_return std::unexpected(files.error());
    auto loaded = co_await backend.load_workflows();
    if (!loaded) co_return std::unexpected(loaded.error().to_string());
    auto listed = settings::models::agents::list(files->agent_dir, project);
    std::vector<shown::AgentInfo> agents;
    agents.reserve(listed.files.size());
    for (const auto& file : listed.files) agents.push_back({std::string(file.definition.name()), file.enabled});
    co_return Data{std::move(loaded->first), std::move(loaded->second), std::move(agents)};
}

contracts::Awaitable<std::expected<void, shown::Error>> save(models::Backend backend, domain::Workflow workflow,
                                                             std::optional<std::string> previous,
                                                             domain::WorkflowSnapshot snapshot) {
    auto previous_role = previous ? std::optional<std::string_view>(*previous) : std::nullopt;
    auto saved = co_await backend.save_workflow(workflow, previous_role, snapshot);
    if (!saved) co_return std::unexpected(error_to_view(saved.error()));
    co_return std::expected<void, shown::Error>{};
}

contracts::Awaitable<std::expected<void, shown::Error>> remove(models::Backend backend, std::string role,
                                                               domain::WorkflowSnapshot snapshot) {
    auto removed = co_await backend.delete_workflow(role, snapshot);
    if (!removed) co_return std::unexpected(error_to_view(removed.error()));
    co_return std::expected<void, shown::Error>{};
}

std::vector<std::pair<std::string, std::string>> each_workflow(const State& s, std::string_view prefix) {
    std::vector<std::pair<std::string, std::string>> rows;
    for (const auto& w : s.workflows()) rows.emplace_back(w.role, std::string(prefix) + w.role);
    return rows;
}

const std::array<AreaCommand<State>, 3> COMMAND_TABLE{
    AreaCommand<State>{"workflows.new", "New workflow", "Workflows", std::nullopt, Listed<State>::always(),
                       [](State& s, std::string_view, Ctx& ctx) { return s.update_typed(msg::New{}, ctx); }},
    AreaCommand<State>{"workflows.edit.", "", "Workflows", std::nullopt,
                       Listed<State>::each_([](const State& s, const Ctx&) { return each_workflow(s, "Edit workflow: "); }),
                       [](State& s, std::string_view role, Ctx& ctx) { return s.update_typed(msg::Edit{std::string(role)}, ctx); }},
    AreaCommand<State>{"workflows.use.", "", "Workflows", std::nullopt,
                       Listed<State>::each_([](const State& s, const Ctx&) { return each_workflow(s, "Use workflow: "); }),
                       [](State& s, std::string_view role, Ctx& ctx) { return s.update_typed(msg::Pick{std::string(role)}, ctx); }},
};

} // namespace

std::span<const AreaCommand<State>> State::COMMANDS() { return COMMAND_TABLE; }

std::pair<std::unique_ptr<State>, Task> State::make(Ctx& ctx) {
    auto state = std::make_unique<State>(ctx.backends.models);
    state->sync(ctx);
    auto task = state->read(ctx, Load{Load::Kind::Refresh, {}});
    return {std::move(state), std::move(task)};
}

Task State::read(const Ctx& ctx, Load load) const {
    return perform([backend = backend_, project = ctx.runtime.cwd()] { return read_data(backend, project); },
                   [load = std::move(load)](std::expected<Data, std::string> result) -> loa::app::Message {
                       return message(msg::Loaded{load, std::move(result)});
                   });
}

Task State::open(Load load, Ctx& ctx) {
    ctx.show_screen(ID);
    if (busy_) return Task::none();
    return reload(ctx, std::move(load));
}

Task State::reload(const Ctx& ctx, Load load) {
    if (load.kind == Load::Kind::Open)
        if (auto role = screen_.selected()) load = Load{Load::Kind::Edit, std::string(*role)};
    busy_ = "Reading workflows…";
    return read(ctx, std::move(load));
}

void State::sync(Ctx& ctx) const {
    const auto harness = harness_of(ctx);
    std::optional<std::string> current;
    if (const auto* active = ctx.runtime.sessions().active()) current = recorded_role(*active);
    else if (const auto* model = ctx.composer.models().second) current = model->role;
    std::vector<composer_view> options;
    options.reserve(workflows_.size());
    for (const auto& workflow : workflows_) {
        composer_view option{workflow.role, workflow.role, WorkflowKind::None, current == workflow.role};
        switch (workflow.rules.kind) {
        case domain::Rules::Kind::Managed:
            option.label = workflow.rules.name;
            option.kind = WorkflowKind::Managed;
            break;
        case domain::Rules::Kind::HandWritten:
        case domain::Rules::Kind::External: option.kind = WorkflowKind::HandWritten; break;
        case domain::Rules::Kind::None: break;
        }
        options.push_back(std::move(option));
    }
    ctx.composer.set_workflows(std::move(options), harness == "omp");
}

void State::pick(std::string_view role, Ctx& ctx) const {
    if (harness_of(ctx) != "omp") {
        ctx.notify("Available with OMP sessions");
        return;
    }
    ctx.request(request::Forward{area_message(area_id::MODELS, to::models::PickRole{std::string(role)})});
}

Task State::intent(shown::Intent intent, Ctx& ctx) {
    return std::visit(
        overloaded{
            [&](shown::intent::Close&) {
                ctx.close_screen();
                return Task::none();
            },
            [&](shown::intent::Open&) { return Task::none(); },
            [&](shown::intent::New&) { return Task::none(); },
            // The form goes back to what is on disk: a Save then can't overwrite the change it missed.
            [&](shown::intent::Reload&) { return reload(ctx, Load{Load::Kind::Open, {}}); },
            [&](shown::intent::Save& s) {
                if (!snapshot_) return Task::none();
                auto workflow = std::move(s.draft.workflow);
                if (!s.previous_role && workflow.role.empty())
                    if (const auto* managed = std::get_if<shown::rules::Managed>(&workflow.rules)) {
                        auto role = domain::role_for(managed->name);
                        if (!role) {
                            screen_.set_error(shown::error::Message{role.error().to_string()});
                            return Task::none();
                        }
                        workflow.role = std::move(*role);
                    }
                auto stored = view::settings::from_view(std::move(workflow));
                if (!stored) {
                    screen_.set_error(shown::error::Message{stored.error().to_string()});
                    return Task::none();
                }
                busy_ = "Saving workflow…";
                auto role = stored->role;
                return perform(
                    [backend = backend_, workflow = std::move(*stored), previous = std::move(s.previous_role),
                     snapshot = *snapshot_] { return save(backend, workflow, previous, snapshot); },
                    [role = std::move(role)](std::expected<void, shown::Error> result) -> loa::app::Message {
                        return message(msg::Saved{role, std::move(result)});
                    });
            },
            [&](shown::intent::Delete& d) {
                if (!snapshot_) return Task::none();
                busy_ = "Deleting workflow…";
                return perform([backend = backend_, role = std::move(d.role), snapshot = *snapshot_] { return remove(backend, role, snapshot); },
                               [](std::expected<void, shown::Error> result) -> loa::app::Message {
                                   return message(msg::Saved{std::nullopt, std::move(result)});
                               });
            },
        },
        intent);
}

Task State::loaded(msg::Loaded loaded, Ctx& ctx) {
    if (loaded.load.kind == Load::Kind::Refresh) {
        // The startup read must not replace a newer editor snapshot.
        if (snapshot_ || busy_) return Task::none();
    } else {
        busy_.reset();
    }
    if (!loaded.result) {
        if (loaded.load.kind != Load::Kind::Refresh) screen_.set_error(shown::error::Message{std::move(loaded.result.error())});
        return Task::none();
    }
    auto& data = *loaded.result;
    workflows_ = std::move(data.workflows);
    snapshot_ = std::move(data.snapshot);
    std::vector<shown::Workflow> entries;
    entries.reserve(workflows_.size());
    for (const auto& workflow : workflows_) entries.push_back(view::settings::to_view(workflow));
    screen_.set_workflows(std::move(entries));
    screen_.set_agents(std::move(data.agents));
    screen_.set_error(std::nullopt);
    if (loaded.load.kind == Load::Kind::New) {
        screen_.new_workflow();
    } else if (loaded.load.kind == Load::Kind::Edit && !screen_.edit(loaded.load.role)) {
        screen_.new_workflow();
        ctx.notify("No workflow " + loaded.load.role);
    }
    sync(ctx);
    return Task::none();
}

Task State::update_typed(Message message, Ctx& ctx) {
    return std::visit(
        overloaded{
            [&](msg::Open&) { return open(Load{Load::Kind::Open, {}}, ctx); },
            [&](msg::New&) { return open(Load{Load::Kind::New, {}}, ctx); },
            [&](msg::Edit& e) { return open(Load{Load::Kind::Edit, std::move(e.role)}, ctx); },
            [&](msg::Pick& p) {
                pick(p.role, ctx);
                return Task::none();
            },
            [&](msg::ShowPage& p) {
                if (p.page != Page::Workflows)
                    ctx.request(request::Forward{area_message(area_id::MODELS, to::models::Show{p.page})});
                return Task::none();
            },
            [&](msg::Screen& s) {
                if (const auto* emitted = std::get_if<shown::Intent>(&s.message);
                    emitted && std::holds_alternative<shown::intent::Close>(*emitted))
                    return intent(*emitted, ctx);
                if (busy_) return Task::none();
                auto emitted = screen_.update(std::move(s.message));
                return emitted ? intent(std::move(*emitted), ctx) : Task::none();
            },
            [&](msg::Loaded& l) { return loaded(std::move(l), ctx); },
            [&](msg::Saved& s) {
                if (!s.result) {
                    busy_.reset();
                    screen_.set_error(std::move(s.result.error()));
                    return Task::none();
                }
                ctx.notify(s.role ? "Workflow saved" : "Workflow deleted");
                return reload(ctx, s.role ? Load{Load::Kind::Edit, std::move(*s.role)} : Load{Load::Kind::New, {}});
            },
        },
        message);
}

// Runs after the models area's pending selection has contributed.
void State::contribute(contracts::SessionConfig& config, const Target& target, const Ctx& ctx) const {
    if (target.harness != "omp") return;
    auto role = config.model_role;
    // A resumed session keeps the role its `<session>` records.
    if (!role && target.resume)
        if (const auto* session = ctx.runtime.sessions().get(config.session_id)) role = recorded_role(*session);
    if (role)
        config.instructions = "The active model role is `" + *role + "`. Follow the RULES.md section for that role.";
}

QWidget* State::view_screen(const Ctx&, const Emit& send) const {
    QWidget* body = nullptr;
    if (busy_) {
        body = ui::settings::models::status(*busy_);
    } else {
        auto* view = new screen::WorkflowsView(false);
        view->render(screen_);
        QObject::connect(view, &screen::WorkflowsView::message, view,
                         [send](const screen::Message& m) { send(message(msg::Screen{m})); });
        body = view;
    }
    using FramePage = ui::settings::models::Page;
    return ui::settings::models::frame(
        FramePage::Workflows, body, [send](FramePage page) { send(message(msg::ShowPage{static_cast<Page>(page)})); },
        [send] { send(message(msg::Screen{shown::Intent{shown::intent::Close{}}})); });
}

Task State::on_session_event(const SessionEvent&, Ctx& ctx) {
    sync(ctx);
    return Task::none();
}

} // namespace loa::app::areas::workflows
