#include "areas/decisions/decisions.hpp"

#include <array>

#include <QGuiApplication>
#include <QHBoxLayout>
#include <QScrollArea>
#include <QVBoxLayout>
#include <QWidget>
#include <QWindow>

#include "areas/tools/tools.hpp"
#include "loa/app/perform.hpp"
#include "loa/app/view/session.hpp"
#include "loa/core/schema.hpp"
#include "loa/runtime/time.hpp"
#include "loa/session/plan.hpp"
#include "loa/ui/app/notifier.hpp"
#include "loa/ui/kit/widgets.hpp"
#include "loa/ui/session/decisions/card.hpp"
#include "loa/ui/session/decisions/view.hpp"

namespace loa::app::areas::decisions {

namespace {

template <class... F>
struct overloaded : F... {
    using F::operator()...;
};

namespace kit = loa::ui::kit;
namespace theme = loa::ui::kit::theme;
namespace widgets = loa::ui::kit::widgets;
using theme::SpaceRole;
using theme::TextRole;

int px(float v) { return static_cast<int>(v + 0.5F); }
int gap(SpaceRole role) { return px(theme::sp(role)); }

constexpr std::string_view ON_TITLE = "Unattended prompts: deny approvals and reject plans after 5 min";
constexpr std::string_view OFF_TITLE = "Unattended prompts: wait for someone";

// Whether the app's window is shown but not the one the user works in. No window (tests, a
// headless run) counts as the foreground.
bool in_background() {
    if (qobject_cast<QGuiApplication*>(QCoreApplication::instance()) == nullptr) return false;
    bool shown = false;
    for (const auto* window : QGuiApplication::topLevelWindows()) shown = shown || window->isVisible();
    return shown && QGuiApplication::applicationState() != Qt::ApplicationActive;
}

desk::KeyPress card_key(const ui::shell::palette::KeyPress& key) {
    namespace palette = ui::shell::palette;
    desk::KeyPress out;
    if (const auto* named = std::get_if<palette::Named>(&key.key)) {
        if (*named == palette::Named::Enter) out.named = desk::KeyPress::Named::Enter;
        else if (*named == palette::Named::Escape) out.named = desk::KeyPress::Named::Escape;
    } else if (const auto* text = std::get_if<std::string>(&key.key)) {
        out.text = *text;
    }
    out.ctrl = key.modifiers.contains(palette::Modifiers::ctrl());
    out.alt = key.modifiers.contains(palette::Modifiers::alt());
    out.logo = key.modifiers.contains(palette::Modifiers::logo());
    return out;
}

Task show_settings(State&, std::string_view, Ctx& ctx) {
    ctx.show_screen(State::ID);
    return Task::none();
}

const std::array<AreaCommand<State>, 3> COMMAND_TABLE{
    AreaCommand<State>{"decisions.settings", "Unattended prompt settings", "Settings", std::nullopt, Listed<State>::always(),
                       show_settings},
    AreaCommand<State>{"decisions.unattended.on", "", "Settings", std::nullopt,
                       Listed<State>::when_([](const State& s, const Ctx&) -> std::optional<std::string_view> {
                           if (s.desk().policy().enabled()) return std::nullopt;
                           return ON_TITLE;
                       }),
                       [](State& s, std::string_view, Ctx& ctx) { return s.set_unattended(true, ctx); }},
    AreaCommand<State>{"decisions.unattended.off", "", "Settings", std::nullopt,
                       Listed<State>::when_([](const State& s, const Ctx&) -> std::optional<std::string_view> {
                           if (!s.desk().policy().enabled()) return std::nullopt;
                           return OFF_TITLE;
                       }),
                       [](State& s, std::string_view, Ctx& ctx) { return s.set_unattended(false, ctx); }},
};

} // namespace

std::span<const AreaCommand<State>> State::COMMANDS() { return COMMAND_TABLE; }

State::State(std::string by, desk::UnattendedView policy, std::shared_ptr<const desk::Notifier> notifier)
    : desk_(std::move(by), std::move(policy), std::move(notifier)) {}

std::pair<std::unique_ptr<State>, Task> State::make(Ctx& ctx) {
    auto config = ctx.read_config("unattended prompt settings");
    auto policy = loa::app::view::unattended_to_view(config ? config->unattended : settings::app::UnattendedSettings{});
    auto state = std::make_unique<State>(ctx.services.node.key, std::move(policy),
                                         std::make_shared<ui::app::DesktopNotifier>("loa"));
    state->desk_.set_local_name(ctx.services.node.label);
    return {std::move(state), Task::none()};
}

loa::app::Message State::dismiss() const { return message(msg::Desk{desk::desk_msg::Key{desk::Shortcut::dismiss()}}); }

std::optional<loa::app::Message> State::shortcut(const ui::shell::palette::KeyPress& key) const {
    if (!desk_.wants_keys()) return std::nullopt;
    auto shortcut = desk::shortcut(card_key(key));
    if (!shortcut || shortcut->kind == desk::Shortcut::Kind::Dismiss) return std::nullopt;
    return message(msg::Desk{desk::desk_msg::Key{*shortcut}});
}

Task State::set_unattended(bool enabled, Ctx& ctx) {
    return update_typed(msg::Desk{desk::desk_msg::Settings{desk::unattended::Enabled{enabled}}}, ctx);
}

Task State::notify(const std::vector<desk::Notice>& notices) const {
    std::vector<Task> tasks;
    for (auto& future : desk_.notify(notices))
        tasks.push_back(perform([future = std::make_shared<desk::NotifyFuture>(std::move(future))]() mutable { return std::move(*future); },
                                [](std::expected<void, desk::NotifyError> result) -> loa::app::Message {
                                    return message(msg::Desk{desk::desk_msg::Notified{std::move(result)}});
                                }));
    return Task::batch(std::move(tasks));
}

Task State::sync(const SessionId& session, Ctx& ctx) {
    const auto* open = ctx.runtime.sessions().get(session.as_str());
    if (!open) return Task::none();
    const bool background = in_background();
    if (desk_.tracker().background != background) (void)desk_.update(desk::desk_msg::Focused{!background});
    return notify(desk_.sync(open->dom(), loa::runtime::time::now_ms()));
}

Task State::answer(std::string session, core::NodeId decision, contracts::Answer answer, Ctx& ctx) {
    const auto* open = ctx.runtime.sessions().get(session);
    if (!open) {
        desk_.answer_failed(session, decision);
        return Task::none();
    }
    // Remote host approvals and host key questions belong to the tools area's `remote` server,
    // not the harness.
    if (tools::is_remote_decision(open->dom(), decision)) {
        ctx.request(request::Forward{area_message(area_id::TOOLS, to::tools::AnswerDecision{open->id(), decision, std::move(answer)})});
        return Task::none();
    }
    if (loa::session::plan::is_app_plan(open->dom(), decision)) {
        ctx.request(request::Forward{area_message(area_id::SESSIONS, to::sessions::AnswerPlan{open->id(), decision, std::move(answer)})});
        return Task::none();
    }
    auto harness = open->harness();
    if (!harness) {
        desk_.answer_failed(session, decision);
        // A timeout in a session nobody runs isn't worth a notice.
        if (answer.by != desk::TIMEOUT_BY)
            ctx.notify("This session's harness isn't running; send a prompt to resume it, then answer");
        return Task::none();
    }
    return perform([harness, decision, answer = std::move(answer)] { return harness->answer(decision, answer); },
                   [session = std::move(session), decision](contracts::Result<> result) -> loa::app::Message {
                       std::expected<void, std::string> out;
                       if (!result) out = std::unexpected(result.error().message());
                       return message(msg::Answered{session, decision, std::move(out)});
                   });
}

Task State::update_typed(Message message, Ctx& ctx) {
    return std::visit(
        overloaded{
            [&](msg::Desk& m) {
                // Keys answer the active session's card only while it shows; Esc closes a screen
                // over it first.
                if (const auto* key = std::get_if<desk::desk_msg::Key>(&m.message); key && ctx.screen()) {
                    if (key->shortcut.kind == desk::Shortcut::Kind::Dismiss) ctx.close_screen();
                    return Task::none();
                }
                std::vector<Task> tasks;
                for (auto& intent : desk_.update(std::move(m.message))) {
                    namespace di = loa::view::session::decisions::desk_intent;
                    std::visit(overloaded{
                                   [&](di::Answer& a) {
                                       tasks.push_back(answer(std::move(a.session), a.decision, std::move(a.answer), ctx));
                                   },
                                   [&](di::OpenLink& l) {
                                       if (!ctx.backends.opener) return;
                                       if (auto opened = ctx.backends.opener->open_url(l.url); !opened) ctx.notify(opened.error());
                                   },
                                   [&](di::PolicyChanged& p) {
                                       (void)ctx.edit_config("unattended prompt settings", [&](settings::app::Config& config) {
                                           config.unattended = loa::app::view::unattended_from_view(std::move(p.policy));
                                       });
                                   },
                                   [&](di::NotifyFailed& f) {
                                       if (notify_failed_) return;
                                       notify_failed_ = true;
                                       ctx.notify("System notifications are unavailable: " + f.message);
                                   },
                               },
                               intent);
                }
                return Task::batch(std::move(tasks));
            },
            [&](msg::Answered& m) {
                if (!m.result) {
                    desk_.answer_failed(m.session, m.decision);
                    ctx.notify("The answer wasn't taken: " + m.result.error());
                }
                return Task::none();
            },
            [&](msg::Answer& m) { return answer(std::move(m.session), m.decision, std::move(m.answer), ctx); },
            [&](msg::ViewOnly& m) {
                desk_.set_view_only(m.session, m.view_only);
                return Task::none();
            },
            [&](msg::Clock&) {
                return update_typed(msg::Desk{desk::desk_msg::Tick{loa::runtime::time::now_ms()}}, ctx);
            },
            [&](msg::CloseSettings&) {
                ctx.close_screen();
                return Task::none();
            },
        },
        message);
}

QWidget* State::view_screen(const Ctx&, const Emit& send) const {
    const auto total = desk_.total();
    std::string waiting = total == 0   ? std::string("No prompt is waiting.")
                          : total == 1 ? std::string("1 prompt is waiting.")
                                       : std::to_string(total) + " prompts are waiting.";
    auto* body = new QWidget;
    body->setMaximumWidth(px(theme::width(theme::LayoutRole::ContentWidth)));
    auto* layout = new QVBoxLayout(body);
    layout->setContentsMargins(0, 0, 0, 0);
    layout->setSpacing(gap(SpaceRole::L));
    auto* done = widgets::text_button("Done", kit::ButtonKind::secondary(), [send] { send(message(msg::CloseSettings{})); });
    layout->addWidget(widgets::section_header("Unattended prompts", {done}));
    auto* settings = new desk::UnattendedSettingsView;
    settings->setAccessibleName(QStringLiteral("unattended settings"));
    settings->render(desk_.policy());
    QObject::connect(settings, &desk::UnattendedSettingsView::message, settings,
                     [send](const desk::unattended::Message& m) { send(message(msg::Desk{desk::desk_msg::Settings{m}})); });
    layout->addWidget(widgets::card(settings));
    layout->addWidget(widgets::label(waiting, TextRole::Caption, kit::Tone::Muted));
    layout->addStretch(1);

    auto* center = new QWidget;
    auto* row = new QHBoxLayout(center);
    row->setContentsMargins(gap(SpaceRole::Xl), gap(SpaceRole::Xl), gap(SpaceRole::Xl), gap(SpaceRole::Xl));
    row->addStretch(1);
    row->addWidget(body, 100);
    row->addStretch(1);
    auto* scroll = new QScrollArea;
    scroll->setAccessibleName(QStringLiteral("unattended prompts"));
    scroll->setWidgetResizable(true);
    scroll->setWidget(center);
    return scroll;
}

QWidget* State::card(std::string_view session, core::NodeId node, const Emit& send) const {
    const auto* cards = desk_.session(session);
    if (!cards) return nullptr;
    const auto* card = cards->card(node);
    if (!card) return nullptr;
    auto* view = new desk::CardView;
    view->render(*card, *cards, desk_.active() == std::optional<std::string>(std::string(session)) && desk_.focused() == node);
    QObject::connect(view, &desk::CardView::message, view, [send, session = std::string(session)](const desk::Message& m) {
        send(message(msg::Desk{desk::desk_msg::Card{session, m}}));
    });
    return view;
}

Task State::on_session_event(const SessionEvent& event, Ctx& ctx) {
    namespace ev = app_runtime::session_event;
    return std::visit(overloaded{
                          [&](const ev::Activated& e) {
                              desk_.set_active(e.id ? std::optional<std::string>(e.id->str()) : std::nullopt);
                              return e.id ? sync(*e.id, ctx) : Task::none();
                          },
                          [&](const ev::Loaded& e) { return sync(e.id, ctx); },
                          [&](const ev::Patch& e) {
                              bool touches = false;
                              std::visit(overloaded{
                                             [&](const core::op::Insert& op) { touches = op.kind == core::ElementKind::Decision; },
                                             [&](const core::op::Set&) {
                                                 const auto* open = ctx.runtime.sessions().get(e.id.as_str());
                                                 const auto* n = open ? open->dom().get(e.patch->node) : nullptr;
                                                 touches = n && n->kind == core::ElementKind::Decision;
                                             },
                                             [&](const core::op::Remove&) { touches = true; },
                                             [&](const auto&) {},
                                         },
                                         e.patch->op);
                              return touches ? sync(e.id, ctx) : Task::none();
                          },
                          [&](const ev::Closed& e) {
                              desk_.forget(e.id.as_str());
                              return Task::none();
                          },
                          [&](const auto&) { return Task::none(); },
                      },
                      event);
}

Subscriptions State::subscription() const {
    Subscriptions subs;
    if (desk_.wants_clock())
        subs.push_back(subscription::Every{"decisions.clock", std::chrono::milliseconds(desk::TICK_MS), message(msg::Clock{})});
    return subs;
}

} // namespace loa::app::areas::decisions
