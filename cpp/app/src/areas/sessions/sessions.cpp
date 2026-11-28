// The sessions area's state, commands and session reactions (port of
// `loa-app/src/areas/sessions/mod.rs`).

#include "sessions.hpp"

#include <algorithm>
#include <array>

#include <asio/steady_timer.hpp>
#include <asio/this_coro.hpp>

#include <QWidget>

#include "loa/app/perform.hpp"
#include "loa/contracts/session.hpp"
#include "loa/core/schema.hpp"
#include "loa/runtime/exec.hpp"
#include "loa/runtime/json.hpp"
#include "loa/runtime/time.hpp"
#include "loa/settings/models/config.hpp"
#include "loa/ui/panel/agents_view.hpp"
#include "loa/ui/panel/view.hpp"

namespace loa::app::areas::sessions {

namespace {

template <class... F>
struct overloaded : F... {
    using F::operator()...;
};

using composer_view = loa::view::session::composer::Command;
namespace attr = core::attr;
namespace ev = app_runtime::session_event;

std::string join(const std::vector<std::string_view>& parts, std::string_view sep) {
    std::string out;
    for (const auto& part : parts) {
        if (!out.empty()) out += sep;
        out += part;
    }
    return out;
}

std::vector<std::string_view> names_of(const Roles& roles) {
    std::vector<std::string_view> names;
    names.reserve(roles.size());
    for (const auto& [name, _] : roles) names.emplace_back(name);
    return names;
}

const std::pair<std::string, std::vector<std::string>>* find_role(const Roles& roles, std::string_view role) {
    auto it = std::ranges::find_if(roles, [&](const auto& r) { return r.first == role; });
    return it == roles.end() ? nullptr : &*it;
}

// A harness call's outcome as `msg::Done`.
contracts::Awaitable<std::expected<void, std::string>> interrupt_call(app_runtime::HarnessPtr harness) {
    auto result = co_await harness->interrupt();
    if (!result) co_return std::unexpected(result.error().message());
    co_return std::expected<void, std::string>{};
}

contracts::Awaitable<std::expected<void, std::string>> answer_call(app_runtime::HarnessPtr harness, NodeId decision,
                                                                   contracts::Answer answer) {
    auto result = co_await harness->answer(decision, std::move(answer));
    if (!result) co_return std::unexpected(result.error().message());
    co_return std::expected<void, std::string>{};
}

contracts::Awaitable<std::expected<std::pair<std::string, session::RoleSelected>, std::string>> select_role(
    models::Backend backend, app_runtime::HarnessPtr harness, contracts::Capabilities capabilities,
    std::shared_ptr<bus::SessionWriter> writer, NodeId root, std::string role) {
    auto files = co_await backend.files();
    if (!files) co_return std::unexpected(files.error());
    auto roles = model_roles(files->config());
    if (!roles) co_return std::unexpected(roles.error());
    const auto* found = find_role(*roles, role);
    if (!found)
        co_return std::unexpected("omp has no model role " + role + "; its roles: " + join(names_of(*roles), ", "));
    auto selected = co_await session::select_role(*harness, capabilities, *writer, root, role, found->second);
    if (!selected) co_return std::unexpected(selected.error().to_string());
    co_return std::pair{std::move(role), std::move(*selected)};
}

contracts::Awaitable<std::expected<std::filesystem::path, std::string>> locate(models::Backend backend) {
    auto files = co_await backend.files();
    if (!files) co_return std::unexpected(files.error());
    co_return files->config();
}

Task locate_config(const Ctx& ctx) {
    return perform([backend = ctx.backends.models] { return locate(backend); },
                   [](std::expected<std::filesystem::path, std::string> result) -> loa::app::Message {
                       return State::message(msg::ConfigLocated{std::move(result)});
                   });
}

loa::runtime::Task tick(Task::Emit send, std::stop_token stop) {
    asio::steady_timer timer(co_await asio::this_coro::executor);
    while (!stop.stop_requested()) {
        timer.expires_after(TICK);
        co_await timer.async_wait(asio::use_awaitable);
        if (stop.stop_requested()) co_return;
        send(State::message(msg::Tick{loa::runtime::time::now_ms()}));
    }
}

std::optional<std::string> active_id(const Ctx& ctx) {
    const auto& id = ctx.runtime.sessions().active_id();
    return id ? std::optional<std::string>(id->str()) : std::nullopt;
}

std::vector<std::pair<std::string, std::string>> each_role(const State& state, const Ctx& ctx) {
    std::vector<std::pair<std::string, std::string>> rows;
    if (!ctx.runtime.sessions().active()) return rows;
    auto roles = state.roles();
    if (!roles || !*roles) return rows;
    for (const auto& [role, models] : **roles) {
        auto title = "Model role: " + role + " (" + (models.empty() ? std::string("no model") : models.front()) + ")";
        rows.emplace_back(role, std::move(title));
    }
    return rows;
}

std::optional<std::string_view> plan_title(const State& state, const Ctx& ctx) {
    const auto* session = ctx.runtime.sessions().active();
    if (!session) return std::nullopt;
    const bool plan = state.in_plan(ctx) || detail::mode(session->dom()) == core::SessionMode::Plan;
    return plan ? std::string_view("Leave plan mode") : std::string_view("Enter plan mode");
}

using L = Listed<State>;
using C = AreaCommand<State>;

Task model_command(State&, std::string_view model, Ctx& ctx) {
    if (model.empty()) {
        ctx.notify("Name the model: /model <provider/model>, or pick one in the model picker");
    } else {
        loa::view::session::composer::ModelOption option{std::string(model), std::string(model), std::string(), std::nullopt};
        ctx.request(request::Forward{area_message(area_id::MODELS, to::models::Model{std::move(option)})});
    }
    return Task::none();
}

Task import_command(State& state, std::string_view, Ctx& ctx) {
    state.show_import(ctx);
    return Task::none();
}

Task compact_command(State& state, std::string_view instructions, Ctx& ctx) { return state.compact(ctx, instructions); }

Task role_command(State& state, std::string_view role, Ctx& ctx) { return state.role(ctx, std::string(role)); }

// Entries sharing an id run the same command, listed once in the palette's and once in the `/`
// list's order.
const std::array<C, 14> COMMAND_TABLE{
    C{"sessions.model", "", "", Slash{"model", "Switch this session's model: /model <provider/model>"}, L::never(),
      model_command},
    C{"sessions.role", "", "", Slash{"role", "Switch to one of omp's model roles: /role <name>"}, L::never(), role_command},
    C{"sessions.import", "Import native session…", "Session", std::nullopt, L::always(), import_command},
    C{"sessions.compact", "", "", Slash{"compact", "Summarize the session and continue with the summary"}, L::never(),
      compact_command},
    C{"sessions.fork", "Fork session", "Session", Slash{"fork", "Fork the session into a new one"}, L::session(),
      [](State& state, std::string_view, Ctx& ctx) {
          state.fork_active(ctx);
          return Task::none();
      }},
    C{"sessions.compact", "Compact session", "Session", std::nullopt, L::session(), compact_command},
    C{"sessions.plan", "", "Session", std::nullopt, L::when_(plan_title),
      [](State& state, std::string_view, Ctx& ctx) {
          const auto next = state.in_plan(ctx) ? core::SessionMode::Normal : core::SessionMode::Plan;
          return state.pick_mode(next, ctx);
      }},
    C{"sessions.mode.plan", "", "", Slash{"plan", "Plan mode: read only until you approve a plan"}, L::never(),
      [](State& state, std::string_view, Ctx& ctx) { return state.pick_mode(core::SessionMode::Plan, ctx); }},
    C{"sessions.mode.normal", "", "", Slash{"normal", "Leave plan mode"}, L::never(),
      [](State& state, std::string_view, Ctx& ctx) { return state.pick_mode(core::SessionMode::Normal, ctx); }},
    C{"sessions.delete", "Delete session", "Session", std::nullopt, L::session(),
      [](State& state, std::string_view, Ctx& ctx) {
          if (auto id = active_id(ctx)) return state.remove(ctx, *id);
          return Task::none();
      }},
    C{"sessions.export", "Export session as native file", "Session", Slash{"export", "Export the session as a native file"},
      L::session(),
      [](State& state, std::string_view, Ctx& ctx) {
          state.export_session(ctx, false, std::nullopt);
          return Task::none();
      }},
    C{"sessions.export-xml", "Export session as XML (debug)", "Session",
      Slash{"export-xml", "Export the session as XML for debugging"}, L::session(),
      [](State& state, std::string_view, Ctx& ctx) {
          state.export_session(ctx, true, std::nullopt);
          return Task::none();
      }},
    C{"sessions.import", "", "", Slash{"import", "Import a native session"}, L::never(), import_command},
    C{"sessions.role.", "", "Model roles", std::nullopt, L::each_(each_role), role_command},
};

} // namespace

// ---- helpers -------------------------------------------------------------------------------

namespace detail {

core::SessionMode mode(const core::Dom& dom) {
    const auto* root = dom.root();
    if (!root) return core::SessionMode::Normal;
    auto value = root->str_attr(attr::MODE);
    if (!value) return core::SessionMode::Normal;
    return core::parse<core::SessionMode>(*value).value_or(core::SessionMode::Normal);
}

std::string title_of(const core::Dom& dom) {
    auto summary = contracts::SessionSummary::from_dom(dom, false);
    return summary ? summary->title : std::string("session");
}

contracts::Capabilities capabilities(const Ctx& ctx, std::string_view harness) {
    auto adapter = ctx.runtime.harnesses().registry.get(harness);
    return adapter ? adapter->capabilities() : contracts::Capabilities{};
}

bool asks_first(const Ctx& ctx, std::string_view harness) {
    return !ctx.runtime.harnesses().traits(harness).approval_modes.empty();
}

std::vector<composer_view> composer_commands(const std::vector<composer_view>& app, const core::Dom* dom) {
    auto commands = app;
    if (!dom) return commands;
    const core::Node* listed = nullptr;
    for (const auto& node : dom->iter())
        if (node.kind == core::ElementKind::Ext && node.str_attr(attr::TYPE) == std::optional(chat::COMMANDS_EXT) &&
            (!listed || listed->id < node.id))
            listed = &node;
    if (!listed) return commands;
    const std::string harness(listed->str_attr(attr::HARNESS).value_or("harness"));
    auto json = loa::runtime::json::parse(listed->text);
    if (!json) return commands;
    // omp's `commands`, ACP's `availableCommands`.
    const auto* list = json->find("commands");
    if (!list) list = json->find("availableCommands");
    const auto* array = list ? list->as_array() : nullptr;
    if (!array) return commands;
    for (const auto& command : *array) {
        const auto* name_value = command.find("name");
        const auto* name = name_value ? name_value->as_string() : nullptr;
        if (!name) continue;
        if (std::ranges::any_of(commands, [&](const auto& known) { return known.name == *name; })) continue;
        const auto* described = command.find("description");
        const auto* description = described ? described->as_string() : nullptr;
        commands.push_back({*name, harness + ": " + (description ? *description : std::string())});
    }
    return commands;
}

} // namespace detail

std::expected<Roles, std::string> model_roles(const std::filesystem::path& path) {
    auto config = settings::models::config::OmpConfig::load(path);
    if (!config) return std::unexpected("Can't read " + path.string() + ": " + config.error().to_string());
    auto roles = config->model_roles();
    if (!roles) return std::unexpected("Can't read the model roles in " + path.string() + ": " + roles.error().to_string());
    Roles out;
    out.reserve(roles->size());
    for (const auto& role : *roles) {
        std::vector<std::string> candidates;
        candidates.reserve(role.candidates.size());
        for (const auto& candidate : role.candidates) candidates.push_back(candidate.to_string());
        out.emplace_back(role.role, std::move(candidates));
    }
    return out;
}

// ---- the area ------------------------------------------------------------------------------

State::State(std::filesystem::path home, std::shared_ptr<const projects::Summarizers> small_model,
             std::shared_ptr<const contracts::UrlOpener> opener)
    : small(std::move(small_model)), home_(std::move(home)), opener_(std::move(opener)) {}

std::span<const AreaCommand<State>> State::COMMANDS() { return COMMAND_TABLE; }

std::pair<std::unique_ptr<State>, Task> State::make(Ctx& ctx) {
    // The small model runs through omp: none when it isn't installed.
    std::shared_ptr<const projects::Summarizers> small_model;
    if (loa::runtime::exec::which("omp")) small_model = ctx.backends.summarizer;
    auto state = std::make_unique<State>(ctx.backends.home, std::move(small_model), ctx.backends.opener);
    return {std::move(state), locate_config(ctx)};
}

Task State::reload(Ctx& ctx) {
    static const core::Dom empty;
    const auto* active = ctx.runtime.sessions().active();
    const auto& dom = active ? active->dom() : empty;
    agents.reset(dom);
    jobs.reset(dom);
    const auto now = loa::runtime::time::now_ms();
    (void)agents.update(agents::msg::Tick{now});
    (void)jobs.update(agents::msg::Tick{now});
    sync_active_parent(ctx);
    ctx.composer.set_mode(detail::mode(dom));
    ctx.composer.set_commands(detail::composer_commands(ctx.slash, active ? &active->dom() : nullptr));
    std::vector<Task> tasks;
    tasks.push_back(tree.refresh(ctx));
    restore_plan(ctx);
    tasks.push_back(refresh_selected_subagent_on_activate(ctx));
    return Task::batch(std::move(tasks));
}

void State::on_append(const SessionId& id, const core::Patch& patch, Ctx& ctx) {
    const auto& sessions = ctx.runtime.sessions();
    if (!sessions.is_active(id.as_str())) return;
    if (const auto* session = sessions.get(id.as_str())) {
        agents.apply(session->dom(), patch);
        jobs.apply(session->dom(), patch);
    }
}

bool State::role_switch_pending(std::string_view id) const {
    return pending_roles_.contains(id) || switching_roles_.contains(id);
}

bool State::hold(std::string_view id) {
    const bool due = role_switch_pending(id);
    if (due) held_.emplace(id);
    return due;
}

void State::release(Ctx& ctx, const std::string& id) {
    if (auto it = held_.find(id); it != held_.end()) {
        held_.erase(it);
        ctx.request(request::ReleaseRoleQueue{id});
    }
}

std::optional<std::expected<Roles, std::string>> State::roles() const {
    if (!config_) return std::nullopt;
    if (!*config_) return std::unexpected("Can't find omp's config: " + config_->error());
    const auto& path = **config_;
    std::error_code error;
    auto mtime = std::filesystem::last_write_time(path, error);
    const bool dated = !error;
    if (dated && roles_cache_ && roles_cache_->first == mtime) return roles_cache_->second;
    auto roles = model_roles(path);
    if (dated && roles) roles_cache_ = std::pair{mtime, *roles};
    return roles;
}

Task State::remove(Ctx& ctx, const std::string& id) {
    tree.forget(id);
    plans_.erase(id);
    {
        std::lock_guard lock(hooks_mutex_);
        hooks_.erase(id);
    }
    subagents.remove_parent(id);
    if (active_parent_ == id) {
        active_parent_.reset();
        active_subagent_messages_ = false;
    }
    if (ctx.runtime.sessions().get(id)) {
        ctx.discard_session(id);
    } else if (auto removed = ctx.runtime.library()->remove(id); removed) {
        ctx.shell.sidebar_mut().remove_session(id);
    } else {
        ctx.notify("Can't delete session " + id + ": " + removed.error().to_string());
    }
    if (!ctx.shell.sidebar().session(id)) {
        // Drops the session from the sidebar's pins and the saved ones.
        if (ctx.shell.sidebar().pins().contains(id)) {
            std::vector<std::string> pins;
            for (const auto& pin : ctx.shell.sidebar().pins())
                if (pin != id) pins.push_back(pin);
            ctx.shell.sidebar_mut().set_pins(std::move(pins));
        }
        ctx.edit_config("pins", [&](settings::app::Config& config) { config.pinned.erase(id); });
    }
    return Task::none();
}

Task State::role(Ctx& ctx, std::string role) {
    const auto* session = ctx.runtime.sessions().active();
    if (!session) {
        ctx.notify("Open a session to switch its model role");
        return Task::none();
    }
    std::string id = session->id().str();
    const bool busy = session->is_busy();
    if (!session->harness()) {
        ctx.notify("Model roles switch a running session: send a message to resume it first");
        return Task::none();
    }
    auto known = roles();
    if (known && !*known) {
        ctx.notify(known->error());
        return Task::none();
    }
    if (known && !find_role(**known, role)) {
        auto names = names_of(**known);
        ctx.notify(role.empty() ? "Name a model role: /role <" + join(names, "|") + ">"
                                : "omp has no model role " + role + "; its roles: " + join(names, ", "));
        return Task::none();
    }
    if (!known && role.empty()) {
        ctx.notify("Name a model role: /role <name>");
        return Task::none();
    }
    if (switching_roles_.contains(id) || busy) {
        pending_roles_.insert_or_assign(std::move(id), std::move(role));
        return Task::none();
    }
    return switch_role(ctx, std::move(id), std::move(role));
}

Task State::switch_role(Ctx& ctx, std::string id, std::string role) {
    const auto* session = ctx.runtime.sessions().get(id);
    const auto* root = session ? session->dom().root() : nullptr;
    if (!session || !session->harness() || !root) {
        ctx.notify("Can't switch to model role " + role + ": the session no longer runs");
        release(ctx, id);
        return Task::none();
    }
    auto harness = session->harness();
    auto writer = session->writer();
    const auto root_id = root->id;
    // Prompts sent while the switch runs wait for it, so the first of them carries the role note.
    if (ctx.runtime.sessions().is_active(id) && !ctx.composer.is_busy()) {
        ctx.composer.turn_started();
        held_.insert(id);
    }
    auto capabilities = detail::capabilities(ctx, harness->harness());
    switching_roles_.insert(id);
    return perform(
        [backend = ctx.backends.models, harness, capabilities, writer, root_id, role] {
            return select_role(backend, harness, capabilities, writer, root_id, role);
        },
        [id](std::expected<std::pair<std::string, session::RoleSelected>, std::string> result) -> loa::app::Message {
            return message(msg::RoleSet{id, std::move(result)});
        });
}

void State::open_link(Ctx& ctx, std::string_view link) const {
    if (!(link.starts_with("https://") || link.starts_with("http://"))) {
        ctx.notify("Only web links open in the browser: " + std::string(link));
    } else if (auto opened = opener_->open_url(link); !opened) {
        ctx.notify("Can't open " + std::string(link) + ": " + opened.error());
    }
}

Task State::interrupt_job(Ctx& ctx) const {
    // Harnesses stop jobs only with the turn that runs them.
    const auto* session = ctx.runtime.sessions().active();
    if (!session || !session->harness()) return Task::none();
    ctx.notify("Interrupting the turn that runs this job");
    return perform([harness = session->harness()] { return interrupt_call(harness); },
                   [](std::expected<void, std::string> result) -> loa::app::Message {
                       return message(msg::Done{std::move(result)});
                   });
}

Task State::agents_event(std::optional<agents::Event> event, bool jobs_pane, Ctx& ctx) {
    if (!event) return Task::none();
    namespace in = loa::view::panel::agents_intent;
    return std::visit(overloaded{
                          [&](in::OpenSubagent& open) {
                              return jobs_pane ? Task::none() : open_child_subtab(ctx, std::move(open.job));
                          },
                          [&](in::DetachTranscript& detach) {
                              if (!jobs_pane) detach_child_transcript(ctx, detach.transcript);
                              return Task::none();
                          },
                          [&](in::Cancel&) { return interrupt_job(ctx); },
                      },
                      *event);
}

Task State::update_typed(Message message, Ctx& ctx) {
    return std::visit(
        overloaded{
            // The panes' own tasks carry no widget operations.
            [&](msg::Agents& m) { return agents_event(agents.update(std::move(m.message)).second, false, ctx); },
            [&](msg::Jobs& m) { return agents_event(jobs.update(std::move(m.message)).second, true, ctx); },
            [&](msg::SubagentSelect& m) { return select_child_subtab(ctx, std::move(m.child)); },
            [&](msg::SubagentClose& m) {
                close_child_subtab(ctx, m.child);
                return Task::none();
            },
            [&](msg::SubagentChat& m) { return update_child_chat(ctx, m.parent, m.child, std::move(m.message)); },
            [&](msg::SubagentDraft& m) {
                set_child_draft(ctx, m.parent, m.child, std::move(m.text));
                return Task::none();
            },
            [&](msg::SubagentSend& m) { return send_child_message(ctx, m.parent, m.child); },
            [&](msg::SubagentTranscriptLoaded& m) { return on_transcript_loaded(ctx, std::move(m)); },
            [&](msg::SubagentMessageSent& m) { return on_message_sent(ctx, std::move(m)); },
            [&](msg::Tree& m) {
                auto [task, event] = tree.view.update(std::move(m.message));
                (void)task;
                if (!event) return Task::none();
                namespace in = loa::view::panel::session_tree_intent;
                std::visit(overloaded{
                               [&](in::Open& open) { ctx.request(request::OpenSession{std::move(open.session)}); },
                               [&](in::Fork& fork) { fork_at(ctx, fork.session, fork.turn); },
                               [&](in::Rewind& back) { rewind(ctx, back.session, back.turn); },
                           },
                           *event);
                return Task::none();
            },
            [&](msg::Chat& m) {
                namespace in = loa::view::session::transcript::intent;
                std::visit(overloaded{
                               [&](in::Rewind& back) {
                                   if (auto id = active_id(ctx)) rewind(ctx, *id, back.message);
                               },
                               [&](in::OpenLink& link) { open_link(ctx, link.url); },
                           },
                           m.intent);
                return Task::none();
            },
            [&](msg::Mode& m) { return pick_mode(m.mode, ctx); },
            [&](msg::Delete& m) { return remove(ctx, m.id); },
            [&](msg::AnswerPlan& m) { return answer_plan(ctx, m.session, m.decision, std::move(m.answer)); },
            [&](msg::PlanAnswered& m) {
                plan_answered(ctx, m.session, m.decision, std::move(m.result));
                return Task::none();
            },
            [&](msg::Advised& m) {
                advised(ctx, m.session, m.turn, std::move(m.result));
                return Task::none();
            },
            [&](msg::ConfigLocated& m) {
                config_ = std::move(m.result);
                return Task::none();
            },
            [&](msg::RoleSet& m) {
                switching_roles_.erase(m.session);
                if (auto set = ctx.report("Can't switch the model role", std::move(m.result))) {
                    auto& [role, selected] = *set;
                    role_notes_.insert_or_assign(m.session, role);
                    ctx.notify(selected.effort_error
                                   ? "Model role " + role + ": " + selected.model + "; " + *selected.effort_error
                                   : "Model role " + role + ": " + selected.model);
                }
                if (auto it = pending_roles_.find(m.session); it != pending_roles_.end()) {
                    auto role = std::move(it->second);
                    pending_roles_.erase(it);
                    return switch_role(ctx, m.session, std::move(role));
                }
                release(ctx, m.session);
                return Task::none();
            },
            [&](msg::Transfer& m) {
                transfer_update(ctx, std::move(m.message));
                return Task::none();
            },
            [&](msg::Tick& m) {
                (void)agents.update(agents::msg::Tick{m.now});
                (void)jobs.update(agents::msg::Tick{m.now});
                return poll_selected_subagent(ctx);
            },
            [&](msg::Done& m) {
                if (!m.result) ctx.notify(m.result.error());
                return Task::none();
            },
            [&](msg::TreeLoaded& m) {
                if (m.loaded) tree.merge(*m.loaded);
                return tree.refresh(ctx);
            },
        },
        message);
}

std::vector<PanelTab> State::panel_tabs() const {
    std::vector<PanelTab> tabs;
    for (const auto* pane : {&ui::panel::pane(agents), &ui::panel::pane(jobs), &ui::panel::pane(tree.view)})
        tabs.push_back({std::string(pane->id), std::string(pane->title), pane->icon, false});
    return tabs;
}

QWidget* State::view_tab(std::string_view id, const Ctx&, const Emit& send) const {
    if (id == "agents" || id == "jobs") {
        const bool is_jobs = id == "jobs";
        auto* view = new ui::panel::AgentsView;
        view->render(is_jobs ? jobs : agents);
        QObject::connect(view, &ui::panel::AgentsView::message, view, [send, is_jobs](const agents::Message& m) {
            send(is_jobs ? message(msg::Jobs{m}) : message(msg::Agents{m}));
        });
        return view;
    }
    if (id == "session") {
        auto* view = new ui::panel::SessionTreeView;
        view->render(tree.view);
        QObject::connect(view, &ui::panel::SessionTreeView::message, view,
                         [send](const session_tree::Message& m) { send(message(msg::Tree{m})); });
        return view;
    }
    return nullptr;
}

QWidget* State::view_screen(const Ctx& ctx, const Emit& send) const {
    std::optional<std::pair<std::string_view, std::string_view>> active;
    if (const auto* session = ctx.runtime.sessions().active())
        active = std::pair{session->id().as_str(), session->harness_id().value_or("unknown")};
    return transfer::view(transfer, active, loa::runtime::time::now_ms(),
                          [send](transfer::Message m) { send(message(msg::Transfer{std::move(m)})); });
}

void State::contribute(contracts::SessionConfig& config, const Target& target, const Ctx& ctx) const {
    if (detail::asks_first(ctx, target.harness)) gate_->enforce_through_approvals(config.session_id);
    start_hooks(config, target);
}

void State::before_send(const SessionId& session, contracts::Prompt& prompt, Ctx& ctx) {
    if (auto it = role_notes_.find(session.as_str()); it != role_notes_.end()) {
        prompt.text = "The active model role is now `" + it->second + "`.\n\n" + prompt.text;
        role_notes_.erase(it);
    }
    hook_prompt(ctx, session, prompt);
    if (gate_->active(session.as_str()) && prompt.text.find(session::plan::PLAN_STEP) == std::string::npos)
        prompt.text = session::plan::wrap_prompt(prompt.text);
}

Task State::on_session_event(const SessionEvent& event, Ctx& ctx) {
    return std::visit(
        overloaded{
            [&](const ev::Activated& e) {
                if (e.id)
                    if (auto it = compacted.find(e.id->as_str()); it != compacted.end()) {
                        auto old = std::move(it->second);
                        compacted.erase(it);
                        // The compacted session's harness already stopped: closing must not stop it again.
                        if (auto* source = ctx.runtime.sessions_mut().get_mut(old)) (void)source->detach();
                        ctx.replace_session(old, e.id, false);
                    }
                if (e.id && branching_ == e.id->as_str()) {
                    branching_.reset();
                    ctx.reveal_tab("session");
                }
                std::vector<Task> tasks;
                if (!(config_ && *config_)) tasks.push_back(locate_config(ctx));
                tasks.push_back(reload(ctx));
                return Task::batch(std::move(tasks));
            },
            [&](const ev::Loaded& e) {
                return ctx.runtime.sessions().is_active(e.id.as_str()) ? reload(ctx) : Task::none();
            },
            [&](const ev::Patch& e) {
                const auto& sessions = ctx.runtime.sessions();
                const auto& patch = *e.patch;
                const auto* session = sessions.get(e.id.as_str());
                if (const auto* insert = std::get_if<core::op::Insert>(&patch.op);
                    insert && insert->kind == core::ElementKind::Turn && session && session->dom().root() &&
                    session->dom().root()->id == insert->parent)
                    tree.record_turn(e.id.as_str(), patch.node, patch.seq);
                auto job_task = on_job_patch(ctx, e.id.as_str(), patch.node);
                if (!session || !sessions.is_active(e.id.as_str())) return job_task;
                const auto& dom = session->dom();
                agents.apply(dom, patch);
                jobs.apply(dom, patch);
                if (const auto* insert = std::get_if<core::op::Insert>(&patch.op)) {
                    const auto* node = dom.get(patch.node);
                    if (insert->kind == core::ElementKind::Job && node) {
                        auto kind = node->str_attr(attr::KIND);
                        auto job = kind ? core::parse<core::JobKind>(*kind) : std::nullopt;
                        if (job == core::JobKind::Shell) ctx.reveal_tab("jobs");
                        else if (job == core::JobKind::Subagent) ctx.reveal_tab("agents");
                    } else if (insert->kind == core::ElementKind::Ext && node &&
                               node->str_attr(attr::TYPE) == std::optional(chat::COMMANDS_EXT)) {
                        ctx.composer.set_commands(detail::composer_commands(ctx.slash, &dom));
                    }
                } else if (const auto* set = std::get_if<core::op::Set>(&patch.op);
                           set && set->name == attr::MODE && dom.root() && dom.root()->id == patch.node) {
                    ctx.composer.set_mode(detail::mode(dom));
                }
                return job_task;
            },
            [&](const ev::TurnStarted& e) {
                if (!ctx.runtime.sessions().is_active(e.session.as_str())) return Task::none();
                sync_active_parent(ctx);
                auto refresh = refresh_selected_subagent_on_activate(ctx);
                std::vector<Task> tasks;
                tasks.push_back(tree.refresh(ctx));
                tasks.push_back(std::move(refresh));
                return Task::batch(std::move(tasks));
            },
            [&](const ev::DecisionWaiting& e) {
                const auto* open = ctx.runtime.sessions().get(e.session.as_str());
                if (!open) return Task::none();
                const auto* node = open->dom().get(e.decision);
                if (!node || !open->harness()) return Task::none();
                auto answer = gate_->verdict(e.session.as_str(), *node);
                if (!answer) return Task::none();
                return perform([harness = open->harness(), decision = e.decision,
                                answer = std::move(*answer)] { return answer_call(harness, decision, answer); },
                               [](std::expected<void, std::string> result) -> loa::app::Message {
                                   return message(msg::Done{std::move(result)});
                               });
            },
            [&](const ev::TurnEnded& e) {
                std::vector<Task> tasks;
                if (auto it = pending_roles_.find(e.session.as_str()); it != pending_roles_.end()) {
                    auto role = std::move(it->second);
                    pending_roles_.erase(it);
                    tasks.push_back(switch_role(ctx, e.session.str(), std::move(role)));
                }
                if (e.status == core::TurnStatus::Done) {
                    if (gate_->active(e.session.as_str())) submit_plan(ctx, e.session, e.turn);
                    tasks.push_back(advise(ctx, e.session, e.turn));
                }
                return Task::batch(std::move(tasks));
            },
            [&](const ev::Closed& e) {
                const auto id = e.id.as_str();
                if (auto it = plans_.find(id); it != plans_.end()) plans_.erase(it);
                {
                    std::lock_guard lock(hooks_mutex_);
                    if (auto it = hooks_.find(id); it != hooks_.end()) hooks_.erase(it);
                }
                if (auto it = pending_roles_.find(id); it != pending_roles_.end()) pending_roles_.erase(it);
                if (auto it = switching_roles_.find(id); it != switching_roles_.end()) switching_roles_.erase(it);
                if (auto it = held_.find(id); it != held_.end()) held_.erase(it);
                if (auto it = role_notes_.find(id); it != role_notes_.end()) role_notes_.erase(it);
                subagents.remove_parent(id);
                if (active_parent_ == id) {
                    active_parent_.reset();
                    active_subagent_messages_ = false;
                }
                return Task::none();
            },
            [&](const auto&) { return Task::none(); },
        },
        event);
}

Subscriptions State::subscription() const {
    auto running = [](const agents::State& pane) {
        return std::ranges::any_of(pane.jobs(), [](const auto& entry) { return entry.second.running(); });
    };
    Subscriptions subs;
    if (running(agents) || running(jobs) || should_poll_subagent())
        subs.push_back(subscription::Stream{
            "sessions.tick", [](Task::Emit send, std::stop_token stop, loa::runtime::Executor& executor) {
                executor.spawn(tick(std::move(send), std::move(stop)));
            }});
    return subs;
}

} // namespace loa::app::areas::sessions
