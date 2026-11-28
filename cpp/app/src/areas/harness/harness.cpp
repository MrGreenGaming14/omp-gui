#include "areas/harness/harness.hpp"

#include <algorithm>
#include <array>
#include <filesystem>

#include <QHBoxLayout>
#include <QLabel>
#include <QLineEdit>
#include <QPushButton>
#include <QScrollArea>
#include <QVBoxLayout>
#include <QWidget>

#include "loa/app/perform.hpp"
#include "loa/ui/kit/icons.hpp"
#include "loa/ui/kit/widgets.hpp"

namespace loa::app::areas::harness {

namespace {

template <class... F>
struct overloaded : F... {
    using F::operator()...;
};

namespace kit = loa::ui::kit;
namespace theme = loa::ui::kit::theme;
namespace widgets = loa::ui::kit::widgets;
using contracts::AvailabilityStatus;
using loa::view::theme::Icon;
using theme::SpaceRole;
using theme::TextRole;

int px(float v) { return static_cast<int>(v + 0.5F); }
int gap(SpaceRole role) { return px(theme::sp(role)); }

QHBoxLayout* row(SpaceRole role) {
    auto* layout = new QHBoxLayout;
    layout->setContentsMargins(0, 0, 0, 0);
    layout->setSpacing(gap(role));
    return layout;
}

std::string_view trim(std::string_view s) {
    const auto* ws = " \t\r\n";
    const auto first = s.find_first_not_of(ws);
    if (first == std::string_view::npos) return {};
    return s.substr(first, s.find_last_not_of(ws) - first + 1);
}

// Points the composer's harness picker at the runtime's harnesses.
void refresh_picker(Ctx& ctx) {
    const auto& current = ctx.runtime.sessions().harness;
    ctx.composer.set_harnesses(loa::app::harness::options(ctx.runtime.harnesses()),
                               current ? std::optional<std::string_view>(*current) : std::nullopt);
}

std::vector<std::pair<std::string, std::string>> each_available(const State&, const Ctx& ctx) {
    std::vector<std::pair<std::string, std::string>> rows;
    for (const auto& harness : ctx.runtime.harnesses().list)
        if (harness.available) rows.emplace_back(harness.id, "Switch harness: " + harness.label);
    return rows;
}

const std::array<AreaCommand<State>, 2> COMMAND_TABLE{
    AreaCommand<State>{"harness.", "", "Harness", std::nullopt, Listed<State>::each_(each_available),
                       [](State& s, std::string_view id, Ctx& ctx) { return s.switch_to(std::string(id), ctx); }},
    AreaCommand<State>{RECHECK, "Recheck harness availability", "Harness", std::nullopt, Listed<State>::always(),
                       [](State& s, std::string_view, Ctx& ctx) {
                           ctx.open_tab(std::string(TAB));
                           return s.probe_all(ctx);
                       }},
};

} // namespace

std::string_view status_word(std::optional<AvailabilityStatus> status) {
    if (!status) return "Checking…";
    switch (*status) {
    case AvailabilityStatus::Ready: return "Ready";
    case AvailabilityStatus::NotInstalled: return "Not installed";
    case AvailabilityStatus::Unreachable: return "Not running";
    case AvailabilityStatus::Incompatible: return "Incompatible";
    case AvailabilityStatus::Failed: return "Failed";
    case AvailabilityStatus::Unknown: return "Unknown";
    }
    return "Unknown";
}

std::span<const AreaCommand<State>> State::COMMANDS() { return COMMAND_TABLE; }

std::pair<std::unique_ptr<State>, Task> State::make(Ctx& ctx) {
    auto state = std::make_unique<State>();
    if (auto config = ctx.read_config("harness settings")) state->apply(config->harness, ctx);
    auto task = state->probe_all(ctx);
    return {std::move(state), std::move(task)};
}

std::string_view State::draft(std::string_view id) const {
    const auto it = drafts_.find(id);
    return it == drafts_.end() ? std::string_view{} : std::string_view(it->second);
}

void State::apply(const settings::app::HarnessSettings& settings, Ctx& ctx) {
    for (const auto& id : loa::app::harness::set_executables(ctx.runtime.harnesses_mut(), settings.executables,
                                                             ctx.services.node.key, ctx.services.storage.dirs.omp))
        drafts_.insert_or_assign(id, settings.executables.at(id).string());
    default_ = settings.default_;
    const auto* harness = ctx.runtime.harnesses().get(settings.default_);
    if (ctx.runtime.sessions().active() == nullptr && harness && harness->available)
        ctx.runtime.sessions_mut().harness = settings.default_;
    refresh_picker(ctx);
}

Task State::probe_all(const Ctx& ctx) {
    std::vector<std::string> ids;
    for (const auto& harness : ctx.runtime.harnesses().list) ids.push_back(harness.id);
    std::vector<Task> tasks;
    for (auto& id : ids) tasks.push_back(probe(std::move(id), ctx));
    return Task::batch(std::move(tasks));
}

Task State::probe(std::string id, const Ctx& ctx) {
    auto adapter = ctx.runtime.harnesses().registry.get(id);
    if (!adapter) return Task::none();
    const auto probe = ++next_probe_;
    probes_.insert_or_assign(id, probe);
    if (std::ranges::find(probing_, id) == probing_.end()) probing_.push_back(id);
    return perform([adapter] { return adapter->availability(); },
                   [id = std::move(id), probe](contracts::Availability availability) -> loa::app::Message {
                       return message(msg::Probed{id, probe, std::move(availability)});
                   });
}

void State::probed(const std::string& id, std::uint64_t probe, contracts::Availability availability, Ctx& ctx) {
    const auto it = probes_.find(id);
    if (it == probes_.end() || it->second != probe) return;
    std::erase(probing_, id);
    ctx.runtime.harnesses_mut().set_availability(id, std::move(availability));
    const auto& harnesses = ctx.runtime.harnesses();
    auto runnable = [&](std::string_view h) {
        const auto* harness = harnesses.get(h);
        return harness && harness->available;
    };
    const auto& sessions = ctx.runtime.sessions();
    if (sessions.active() == nullptr && !sessions.starting() && !(sessions.harness && runnable(*sessions.harness))) {
        std::optional<std::string> next;
        if (default_ && runnable(*default_)) next = default_;
        else next = harnesses.default_id();
        ctx.runtime.sessions_mut().harness = std::move(next);
    }
    refresh_picker(ctx);
}

Task State::switch_to(std::string id, Ctx& ctx) {
    const auto& harnesses = ctx.runtime.harnesses();
    const auto* harness = harnesses.get(id);
    if (!harness || !harness->available) {
        ctx.notify(std::string(harnesses.label(id)) + " is unavailable");
        return Task::none();
    }
    if (ctx.runtime.sessions().starting()) {
        ctx.notify("A session is already starting");
        ctx.show_harness();
        return Task::none();
    }
    ctx.runtime.sessions_mut().harness = id;
    ctx.composer.set_harnesses(loa::app::harness::options(ctx.runtime.harnesses()), std::string_view(id));
    if (default_ != id && ctx.edit_config("harness settings", [&](settings::app::Config& config) { config.harness.default_ = id; }))
        default_ = id;
    const auto* session = ctx.runtime.sessions().active();
    if (session == nullptr) {
        ctx.request(request::NewSession{});
        return Task::none();
    }
    if (session->harness_id() == std::optional<std::string_view>(id)) return Task::none();
    if (!session->is_busy() && !session->has_turns()) {
        const std::string current = session->id().str();
        ctx.replace_session(current, std::nullopt, true);
    }
    ctx.request(request::NewSession{});
    return Task::none();
}

Task State::save_executable(std::string id, Ctx& ctx) {
    const auto draft = trim(this->draft(id));
    std::optional<std::filesystem::path> executable;
    if (!draft.empty()) executable = std::filesystem::path(std::string(draft));
    const bool saved = ctx.edit_config("harness settings", [&](settings::app::Config& config) {
        if (executable) config.harness.executables.insert_or_assign(id, *executable);
        else config.harness.executables.erase(id);
    });
    if (!saved) return Task::none();
    if (!loa::app::harness::set_executable(ctx.runtime.harnesses_mut(), id, std::move(executable), ctx.services.node.key,
                                           ctx.services.storage.dirs.omp))
        return Task::none();
    refresh_picker(ctx);
    return probe(std::move(id), ctx);
}

Task State::update_typed(Message message, Ctx& ctx) {
    return std::visit(overloaded{
                          [&](msg::Pick& m) { return switch_to(std::move(m.id), ctx); },
                          [&](msg::Recheck&) { return probe_all(ctx); },
                          [&](msg::Probed& m) {
                              probed(m.id, m.probe, std::move(m.availability), ctx);
                              return Task::none();
                          },
                          [&](msg::EditExecutable& m) {
                              drafts_.insert_or_assign(std::move(m.id), std::move(m.value));
                              return Task::none();
                          },
                          [&](msg::SaveExecutable& m) { return save_executable(std::move(m.id), ctx); },
                      },
                      message);
}

std::vector<PanelTab> State::panel_tabs() const { return {PanelTab{std::string(TAB), "Harnesses", std::nullopt, false}}; }

Task State::on_session_event(const SessionEvent& event, Ctx& ctx) {
    if (const auto* activated = std::get_if<app_runtime::session_event::Activated>(&event); activated && activated->id)
        ctx.show_harness();
    return Task::none();
}

QWidget* State::view_tab(std::string_view id, const Ctx& ctx, const Emit& send) const {
    if (id != TAB) return nullptr;
    const bool checking = !probing_.empty();
    widgets::Callback recheck;
    if (!checking) recheck = [send] { send(message(msg::Recheck{})); };
    auto* button = widgets::text_button(checking ? "Checking…" : "Recheck", kit::ButtonKind::secondary(), recheck);

    auto* body = new QWidget;
    auto* layout = new QVBoxLayout(body);
    layout->setContentsMargins(gap(SpaceRole::M), gap(SpaceRole::M), gap(SpaceRole::M), gap(SpaceRole::M));
    layout->setSpacing(gap(SpaceRole::M));
    layout->addWidget(widgets::section_header("Harnesses", {button}));
    auto* rows = new QVBoxLayout;
    rows->setSpacing(gap(SpaceRole::S));
    const auto& selected = ctx.runtime.sessions().harness;
    for (const auto& harness : ctx.runtime.harnesses().list)
        rows->addWidget(harness_row(harness, selected ? std::optional<std::string_view>(*selected) : std::nullopt, send));
    layout->addLayout(rows);
    layout->addStretch(1);

    auto* scroll = new QScrollArea;
    scroll->setAccessibleName(QStringLiteral("harnesses"));
    scroll->setWidgetResizable(true);
    scroll->setWidget(body);
    return scroll;
}

QWidget* State::harness_row(const app_runtime::Harness& harness, std::optional<std::string_view> selected,
                            const Emit& send) const {
    const std::string id = harness.id;
    const bool probing = std::ranges::find(probing_, id) != probing_.end();
    std::optional<AvailabilityStatus> status;
    if (harness.availability && !probing) status = harness.availability->status;
    Icon icon = Icon::TriangleAlert;
    kit::Tone tone = kit::Tone::Danger;
    if (status == AvailabilityStatus::Ready) {
        icon = Icon::Check;
        tone = kit::Tone::Ok;
    } else if (!status || *status == AvailabilityStatus::Unknown) {
        icon = Icon::CircleDot;
        tone = kit::Tone::Muted;
    }

    auto* body = new QWidget;
    auto* layout = new QVBoxLayout(body);
    layout->setContentsMargins(0, 0, 0, 0);
    layout->setSpacing(gap(SpaceRole::Xs));

    auto* title = row(SpaceRole::S);
    auto* glyph = new QLabel;
    glyph->setPixmap(kit::tinted(icon, TextRole::Body, tone));
    title->addWidget(glyph);
    title->addWidget(widgets::label(harness.label, TextRole::Body, kit::Tone::Normal));
    title->addWidget(widgets::label(status_word(status), TextRole::Label, tone));
    title->addStretch(1);
    if (selected == std::string_view(id))
        title->addWidget(widgets::label("New sessions", TextRole::Label, kit::Tone::Accent));
    else if (harness.available)
        title->addWidget(widgets::text_button("Use", kit::ButtonKind::secondary(), [send, id] { send(message(msg::Pick{id})); }));
    layout->addLayout(title);

    auto note = [](std::string_view s) { return widgets::label(s, TextRole::Label, kit::Tone::Muted); };
    std::optional<std::string_view> version;
    if (harness.availability && harness.availability->version) version = *harness.availability->version;
    if (harness.detail && std::optional<std::string_view>(*harness.detail) != version) layout->addWidget(note(*harness.detail));
    if (version) layout->addWidget(note("Version " + std::string(*version)));

    if (harness.traits.server_url) {
        layout->addWidget(note("Server " + std::string(harness.program)));
    } else {
        if (harness.executable)
            layout->addWidget(widgets::label(harness.executable->string(), TextRole::Mono, kit::Tone::Muted));
        const std::string draft(this->draft(id));
        auto* field = new QLineEdit(QString::fromStdString(draft));
        field->setPlaceholderText(QString::fromUtf8(harness.program.data(), static_cast<int>(harness.program.size())));
        field->setAccessibleName(QString::fromStdString("executable of " + id));
        QObject::connect(field, &QLineEdit::textEdited, field,
                         [send, id](const QString& value) { send(message(msg::EditExecutable{id, value.toStdString()})); });
        QObject::connect(field, &QLineEdit::returnPressed, field, [send, id] { send(message(msg::SaveExecutable{id})); });
        const std::string configured = harness.configured ? harness.configured->string() : std::string();
        widgets::Callback save;
        if (configured != trim(draft)) save = [send, id] { send(message(msg::SaveExecutable{id})); };
        auto* edit = row(SpaceRole::Xs);
        edit->addWidget(field, 1);
        edit->addWidget(widgets::text_button("Save", kit::ButtonKind::secondary(), save));
        layout->addLayout(edit);
    }
    auto* card = widgets::card(body);
    card->setAccessibleName(QString::fromStdString(harness.label));
    return card;
}

} // namespace loa::app::areas::harness
