#include "loa/app/lifecycle.hpp"

#include <algorithm>

#include "loa/app/perform.hpp"
#include "loa/app/registry.hpp"
#include "loa/app/routes.hpp"
#include "loa/core/schema.hpp"
#include "loa/ui/kit/theme.hpp"

namespace loa::app {

namespace {

namespace rt = app_runtime;
namespace ev = app_runtime::runtime_event;
namespace se = app_runtime::session_event;
using ui::session::composer::Activity;

template <class... F>
struct overloaded : F... {
    using F::operator()...;
};

// Runs a harness call of the runtime; its completion goes back to the runtime.
Task perform_effect(rt::Effect effect) {
    return perform(std::move(effect), [](rt::Completion completion) -> Message {
        return msg::Session{lifecycle::msg::Completed{std::move(completion)}};
    });
}

// What `session`'s agent is doing; nothing while no harness runs it.
Activity activity(const rt::Session& session) {
    if (!session.harness()) return Activity::idle();
    return Activity::of(session.dom());
}

// Brings the sidebar's running flag for session `id` in line with the runtime.
void sync_running(Ctx& ctx, const SessionId& id) {
    ctx.shell.set_session_running(id.as_str(), session_running(ctx.runtime.sessions(), id));
}

std::optional<std::string_view> opt_view(const std::optional<std::string>& s) {
    return s ? std::optional<std::string_view>(*s) : std::nullopt;
}

// Adds every area's contribution to `config`, the runtime's base config of a session on `harness`.
void prepare(Ctx& ctx, const Registry& areas, contracts::SessionConfig& config, std::string_view harness, bool resume) {
    contracts::Capabilities capabilities{};
    if (auto adapter = ctx.runtime.harnesses().registry.get(harness)) capabilities = adapter->capabilities();
    const Target target{harness, &capabilities, resume};
    for (const auto& area : areas.iter()) area->contribute(config, target, ctx);
    contracts::HostToolRouter router;
    std::vector<std::string> duplicates;
    for (const auto& area : areas.iter()) {
        auto set = area->host_tools(config, target, ctx);
        if (!set) continue;
        for (auto& spec : set->specs)
            if (auto added = router.add(std::move(spec), set->handler); !added) duplicates.push_back(added.error().name);
    }
    std::move(router).install(config);
    if (!duplicates.empty()) {
        std::string list;
        for (const auto& d : duplicates) list += (list.empty() ? "" : ", ") + d;
        ctx.notify("Host tools offered twice, first one kept: " + list);
    }
}

} // namespace

SessionController::SessionController() {
    chat.set_bottom_inset(ui::kit::theme::width(ui::kit::theme::LayoutRole::ComposerInset));
}

Task SessionController::run(Ctx& ctx, Registry& areas, rt::Command command) {
    const auto before_starting = ctx.runtime.sessions().starting();
    auto transition = ctx.runtime.dispatch(std::move(command));
    auto task = apply(ctx, areas, std::move(transition));
    // Some failed starts and resumes clear `starting` without an event naming the session.
    if (before_starting && ctx.runtime.sessions().starting() != before_starting) sync_running(ctx, *before_starting);
    return task;
}

Task SessionController::apply(Ctx& ctx, Registry& areas, rt::Transition transition) {
    std::vector<Task> tasks;
    for (auto& effect : transition.effects) tasks.push_back(perform_effect(std::move(effect)));
    for (auto& event : transition.events) tasks.push_back(react(ctx, areas, std::move(event)));
    return Task::batch(std::move(tasks));
}

Task SessionController::send(Ctx& ctx, Registry& areas, contracts::Prompt prompt) {
    const auto* active = ctx.runtime.sessions().active();
    if (active && active->harness()) {
        const SessionId id = active->id();
        for (auto& area : areas.iter_mut()) area->before_send(id, prompt, ctx);
    }
    return run(ctx, areas, rt::command::Send{std::move(prompt)});
}

Task SessionController::update(Ctx& ctx, Registry& areas, lifecycle::Message message) {
    return std::visit(overloaded{
                          [&](lifecycle::msg::Feed& feed) {
                              auto batch = ctx.runtime.begin_feed(std::move(feed.id), std::move(feed.feed));
                              std::vector<Task> tasks;
                              while (auto transition = ctx.runtime.advance_feed(batch))
                                  tasks.push_back(apply(ctx, areas, std::move(*transition)));
                              return Task::batch(std::move(tasks));
                          },
                          [&](lifecycle::msg::Completed& completed) {
                              return run(ctx, areas, rt::command::Completed{std::move(completed.completion)});
                          },
                      },
                      message);
}

Task SessionController::react(Ctx& ctx, Registry& areas, rt::RuntimeEvent event) {
    return std::visit(
        overloaded{
            [&](ev::PrepareSession& e) {
                sync_running(ctx, SessionId(e.config.session_id));
                if (!e.resume) ctx.clear_notice();
                prepare(ctx, areas, e.config, e.harness, e.resume);
                return run(ctx, areas, rt::command::Prepared{e.operation, std::move(e.config)});
            },
            [&](ev::Session& e) {
                if (const auto* activated = std::get_if<se::Activated>(&e.event); activated && activated->id)
                    ctx.close_screen();
                return dispatch(ctx, areas, e.event);
            },
            [&](ev::Notice& e) {
                ctx.notify(std::move(e.text));
                return Task::none();
            },
            [&](ev::StartFailed& e) {
                sync_running(ctx, e.id);
                ctx.show_harness();
                (void)ctx.composer.turn_ended();
                if (carried_) {
                    auto draft = std::move(*carried_);
                    carried_.reset();
                    hand_over(ctx.composer, std::nullopt, std::move(draft));
                }
                return Task::none();
            },
            [&](ev::ResumeFailed& e) {
                sync_running(ctx, e.id);
                (void)ctx.composer.turn_ended();
                return Task::none();
            },
            [&](ev::ContinueRequested& e) {
                after.emplace_back(lifecycle::after::Continue{std::move(e.folder), std::move(e.prompt)});
                return Task::none();
            },
            [&](ev::SessionStarted& e) {
                ctx.close_screen();
                sync_running(ctx, e.id);
                if (carried_) {
                    auto draft = std::move(*carried_);
                    carried_.reset();
                    hand_over(ctx.composer, e.id, std::move(draft));
                }
                after.emplace_back(lifecycle::after::RefreshFiles{});
                after.emplace_back(lifecycle::after::Diff{});
                return Task::none();
            },
            [&](ev::TurnStarted& e) {
                ctx.composer.turn_started();
                sync_running(ctx, e.id);
                return Task::none();
            },
            [&](ev::TurnEnded& e) {
                sync_running(ctx, e.id);
                after.emplace_back(lifecycle::after::Diff{});
                after.emplace_back(lifecycle::after::TurnEnded{});
                return Task::none();
            },
            [&](ev::SummaryChanged& e) {
                if (const auto* session = ctx.runtime.sessions().get(e.id.as_str()); session && session->summary())
                    ctx.shell.sidebar_mut().upsert_session(*session->summary());
                return Task::none();
            },
            [&](ev::StatusChanged& e) {
                sync_running(ctx, e.id);
                if (ctx.runtime.sessions().is_active(e.id.as_str())) {
                    std::optional<std::string> status;
                    if (const auto* session = ctx.runtime.sessions().get(e.id.as_str()))
                        if (auto s = turn_status(session->dom())) status = std::string(*s);
                    ctx.shell.set_session_status(std::move(status));
                }
                return Task::none();
            },
            [&](ev::PromptReturned& e) { return Task::composer(ctx.composer.give_back(e.prompt.text)); },
            [&](ev::SendRequested& e) {
                sync_running(ctx, e.id);
                return send(ctx, areas, std::move(e.prompt));
            },
        },
        event);
}

void SessionController::present(Ctx& ctx) {
    const auto* active = ctx.runtime.sessions().active();
    const bool starting = ctx.runtime.sessions().starting().has_value();
    const bool busy = active && active->is_busy() && active->harness();
    follow(ctx.composer, active ? std::optional<SessionId>(active->id()) : std::nullopt, busy, starting);
    ctx.composer.set_activity(active ? activity(*active) : Activity::idle());
    if (!active) {
        chat.load(core::Dom{});
        after.emplace_back(lifecycle::after::Present{ctx.runtime.cwd()});
        return;
    }
    chat.load(active->dom());
    const SessionId id = active->id();
    auto summary = active->summary();
    std::optional<std::string> status;
    if (auto s = turn_status(active->dom())) status = std::string(*s);
    auto cwd = app_runtime::session_cwd(active->dom()).value_or(ctx.runtime.cwd());
    auto& sidebar = ctx.shell.sidebar_mut();
    if (summary) sidebar.upsert_session(std::move(*summary));
    sidebar.set_active(id.str());
    sync_running(ctx, id);
    ctx.shell.set_session_status(std::move(status));
    after.emplace_back(lifecycle::after::Present{std::move(cwd)});
}

void SessionController::follow(ui::session::composer::State& composer, std::optional<SessionId> shown, bool busy,
                               bool starting) {
    if (shown != shown_) {
        auto left = std::exchange(shown_, std::move(shown));
        if (left || !composer.is_busy() || starting) {
            composer.follow_session(false);
            auto draft = composer.take_draft();
            if (!draft.is_empty()) drafts.insert_or_assign(left, std::move(draft));
            if (auto it = drafts.find(shown_); it != drafts.end()) {
                auto mine = std::move(it->second);
                drafts.erase(it);
                composer.restore_draft(std::move(mine));
            }
        }
    }
    composer.follow_session(busy);
}

void SessionController::hand_over(ui::session::composer::State& composer, std::optional<SessionId> to,
                                  ui::session::composer::Draft draft) {
    if (shown_ == to) {
        composer.prepend_draft(std::move(draft));
        return;
    }
    if (auto it = drafts.find(to); it != drafts.end()) {
        it->second.prepend(std::move(draft));
        return;
    }
    drafts.emplace(std::move(to), std::move(draft));
}

Task SessionController::dispatch(Ctx& ctx, Registry& areas, const SessionEvent& event) {
    if (std::holds_alternative<se::Activated>(event)) {
        present(ctx);
    } else if (const auto* loaded = std::get_if<se::Loaded>(&event)) {
        if (ctx.runtime.sessions().is_active(loaded->id.as_str())) {
            present(ctx);
        } else {
            if (const auto* session = ctx.runtime.sessions().get(loaded->id.as_str()); session && session->summary())
                ctx.shell.sidebar_mut().upsert_session(*session->summary());
            sync_running(ctx, loaded->id);
        }
    } else if (const auto* patch = std::get_if<se::Patch>(&event)) {
        apply_to_chats(ctx, patch->id, *patch->patch);
        if (std::holds_alternative<core::op::Append>(patch->patch->op)) {
            if (auto* sessions = areas.port<SessionsPort>()) sessions->on_append(patch->id, *patch->patch, ctx);
            return Task::none();
        }
        if (const auto* active = ctx.runtime.sessions().active(); active && active->id() == patch->id)
            ctx.composer.set_activity(activity(*active));
    } else if (const auto* closed = std::get_if<se::Closed>(&event)) {
        sync_running(ctx, closed->id);
        std::optional<ui::session::composer::Draft> draft;
        if (auto it = drafts.find(std::optional<SessionId>(closed->id)); it != drafts.end()) {
            draft = std::move(it->second);
            drafts.erase(it);
        }
        auto& successors = ctx.successors();
        if (auto it = successors.find(closed->id); it != successors.end()) {
            auto successor = std::move(it->second);
            successors.erase(it);
            if (draft) {
                if (successor) {
                    hand_over(ctx.composer, std::move(successor), std::move(*draft));
                } else {
                    if (carried_) draft->prepend(std::move(*carried_));
                    carried_ = std::move(*draft);
                }
            }
        }
    }
    std::vector<Task> tasks;
    for (auto& area : areas.iter_mut()) tasks.push_back(area->on_session_event(event, ctx));
    return Task::batch(std::move(tasks));
}

void SessionController::apply_to_chats(const Ctx& ctx, const SessionId& id, const core::Patch& patch) {
    const auto* session = ctx.runtime.sessions().get(id.as_str());
    if (!session) return;
    if (ctx.runtime.sessions().is_active(id.as_str())) chat.apply(patch, session->dom());
}

void SessionController::recents_loaded(Ctx& ctx, std::vector<loa::view::shell::SessionSummary> summaries,
                                       std::vector<std::string> problems, std::vector<std::string> empty) {
    for (auto& problem : ctx.runtime.delete_empty(std::move(empty))) problems.push_back(std::move(problem));
    std::vector<SessionId> live;
    for (const auto& [id, session] : ctx.runtime.sessions().iter()) {
        live.push_back(id);
        if (!session.summary()) continue;
        const auto& summary = *session.summary();
        auto it = std::ranges::find_if(summaries, [&](const auto& each) { return each.id == summary.id; });
        if (it != summaries.end()) *it = summary;
        else summaries.push_back(summary);
    }
    auto& sidebar = ctx.shell.sidebar_mut();
    for (auto& summary : summaries) sidebar.upsert_session(std::move(summary));
    for (const auto& id : live) sync_running(ctx, id);
    if (!problems.empty()) {
        std::string text;
        for (const auto& p : problems) text += (text.empty() ? "" : "; ") + p;
        ctx.notify(std::move(text));
    }
}

void SessionController::sync_access(Ctx& ctx) const {
    const auto& sessions = ctx.runtime.sessions();
    std::optional<std::string_view> harness;
    if (const auto* active = sessions.active()) harness = active->harness_id();
    if (!harness) harness = opt_view(sessions.harness);
    std::vector<loa::view::session::composer::AccessOption> options;
    if (harness) {
        const auto traits = ctx.runtime.harnesses().traits(*harness);
        for (const auto& mode : rt::ACCESS)
            if (std::ranges::find(traits.approval_modes, mode.id) != traits.approval_modes.end())
                options.push_back({std::string(mode.id), std::string(mode.label), std::string(mode.detail)});
    }
    std::optional<std::string> current;
    if (!options.empty())
        current = ctx.runtime.access() ? *ctx.runtime.access() : std::string(rt::ACCESS_DEFAULT);
    ctx.composer.set_access(std::move(options), opt_view(current));
}

Task SessionController::set_access(Ctx& ctx, Registry& areas, std::string id) {
    const std::string_view current = ctx.runtime.access() ? std::string_view(*ctx.runtime.access()) : rt::ACCESS_DEFAULT;
    if (id == current) return Task::none();
    ctx.edit_config("settings", [&](settings::app::Config& config) { config.harness.approval_mode = id; });
    return run(ctx, areas, rt::command::SetAccess{std::move(id)});
}

bool session_running(const app_runtime::Sessions& sessions, const SessionId& id) {
    const auto* session = sessions.get(id.as_str());
    return session && session->is_busy() && (session->harness() || sessions.starting() == id);
}

std::optional<std::string_view> turn_status(const core::Dom& dom) {
    auto turns = app_runtime::dom_turns(dom);
    if (turns.empty()) return std::nullopt;
    auto text = turns.back()->str_attr(core::attr::STATUS);
    if (!text) return std::nullopt;
    auto status = core::parse<core::TurnStatus>(*text);
    if (!status) return std::nullopt;
    switch (*status) {
    case core::TurnStatus::Running: return "Running";
    case core::TurnStatus::Waiting: return "Waiting";
    case core::TurnStatus::Cancelled: return "Interrupted";
    case core::TurnStatus::Error: return "Error";
    case core::TurnStatus::Done: return std::nullopt;
    }
    return std::nullopt;
}

} // namespace loa::app
