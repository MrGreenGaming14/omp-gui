// Qt view of the agents screen. Port of `State::{view, body, form}` in
// `crates/loa-ui-settings/src/agents/mod.rs`.

#include "loa/ui/settings/agents/view.hpp"

#include <algorithm>
#include <format>

#include <QCheckBox>
#include <QLabel>
#include <QLineEdit>
#include <QPlainTextEdit>
#include <QPushButton>
#include <QScrollArea>
#include <QVBoxLayout>

#include "settings_widgets.hpp"

namespace loa::ui::settings::agents {

using namespace sw;

AgentsView::AgentsView(bool titled, QWidget* parent) : QWidget(parent), titled_(titled) {
    auto* l = new QVBoxLayout(this);
    l->setContentsMargins(0, 0, 0, 0);
    setAccessibleName(QStringLiteral("agents settings"));
}

void AgentsView::render(const State& state) {
    namespace l = local;
    auto post = [this](Local m) { emit message(Message{std::move(m)}); };
    auto send = [post](Local m) -> Callback { return [post, m] { post(m); }; };

    // The list.
    auto* list = vcol({action("New agent", ButtonKind::secondary(), send(l::New{}))});
    if (state.entries().empty()) add(list, kw::label("No agents yet", TextRole::Label, Tone::Muted));
    for (std::size_t i = 0; i < state.entries().size(); ++i) {
        const auto& e = state.entries()[i];
        const std::string source = e.source == AgentSource::User ? "User" : "Project";
        const auto detail = e.enabled ? source : std::format("{}  \u00b7  disabled", source);
        const bool selected = state.editing() == e.path;
        add(list, kw::two_line_row(e.fields.name, detail, selected, send(l::Edit{i})));
    }
    stretch(list);
    auto* list_scroll = new QScrollArea;
    list_scroll->setWidgetResizable(true);
    list_scroll->setFrameShape(QFrame::NoFrame);
    list_scroll->setWidget(list);
    list_scroll->setFixedWidth(px(theme::width(LayoutRole::NavWidth)));
    list_scroll->setAccessibleName(QStringLiteral("agent list"));

    // The form.
    auto* form = vcol({}, spi(SpaceRole::M));
    form->setMaximumWidth(px(theme::width(LayoutRole::ContentWidth)));
    auto field_errors = [&](std::string_view field) {
        std::vector<QWidget*> out;
        for (const auto& e : state.errors())
            if (e.field == field) out.push_back(kw::label(e.message, TextRole::Caption, Tone::Danger));
        return out;
    };
    // A labelled field with its errors below.
    auto labelled = [&](std::string_view name, std::string_view field, QWidget* input) {
        auto* c = vcol({kw::label(name, TextRole::Label, Tone::Muted), input});
        for (auto* e : field_errors(field)) add(c, e);
        c->setAccessibleName(qs(name));
        return c;
    };
    auto checks = [&](const std::vector<std::string>& names, const std::vector<std::string>& selected, bool tools) {
        auto* c = vcol({});
        std::vector<std::string> all = names;
        for (const auto& s : selected)
            if (std::ranges::find(names, s) == names.end()) all.push_back(s);
        for (const auto& n : all) {
            const bool on = std::ranges::find(selected, n) != selected.end();
            add(c, check(on, n, [post, n, tools](bool v) {
                    post(tools ? Local{l::Tool{n, v}} : Local{l::Spawn{n, v}});
                }));
        }
        return c;
    };
    auto picker = [&](PickerKind which, const std::string& current, const std::vector<std::string>& options,
                      std::function<Local(std::size_t)> pick) {
        std::vector<kw::PickerRow> rows;
        for (const auto& o : options) rows.push_back({o, std::nullopt, o == current});
        auto* p = kw::picker_view(current, state.open_picker() == which, rows, [post, which, pick = std::move(pick)](kw::PickerMsg m) {
            if (m.kind == kw::PickerMsg::Kind::Toggle) post(l::TogglePicker{which});
            else post(pick(m.index));
        });
        return p;
    };

    if (const auto question = state.pending_question()) {
        auto* row = hrow({kw::label(*question, TextRole::Body, Tone::Normal),
                          action(*state.pending_yes(), ButtonKind::danger(), send(l::Confirm{})),
                          action("Cancel", ButtonKind::secondary(), send(l::Cancel{}))});
        stretch(row);
        add(form, row);
    }
    if (const auto& path = state.editing()) {
        const bool enabled = state.is_enabled(*path).value_or(true);
        auto* row = hrow({action(enabled ? "Disable" : "Enable", ButtonKind::secondary(), send(l::Toggle{})),
                          action("Delete", ButtonKind::danger(), send(l::Delete{}))});
        stretch(row);
        add(form, row);
    }
    std::vector<std::string> form_errors;
    for (const auto& e : state.errors())
        if (e.field == "form") form_errors.push_back(e.message);
    if (!form_errors.empty()) add(form, errors(form_errors));

    const auto templates = state.templates();
    std::vector<std::string> template_names;
    for (const auto& t : templates) template_names.push_back(t.label());
    add(form, labelled("Template", "template",
                       picker(PickerKind::Template, state.current_template().label(), template_names,
                              [templates](std::size_t i) { return Local{l::UseTemplate{templates.at(i)}}; })));
    add(form, labelled("Name", "name", field("my-agent", state.name(), [post](std::string v) { post(l::Name{std::move(v)}); }, "agent-name")));
    add(form, labelled("Description", "description",
                       field("What the agent is for", state.description(), [post](std::string v) { post(l::Description{std::move(v)}); },
                             "agent-description")));
    const auto models = state.model_options();
    add(form, labelled("Model", "model",
                       picker(PickerKind::Model, state.model_label(), models, [models](std::size_t i) { return Local{l::Model{models.at(i)}}; })));
    const auto levels = state.thinking_options();
    add(form, labelled("Thinking level", "thinkingLevel",
                       picker(PickerKind::Thinking, state.thinking().value_or(std::string(THINKING_DEFAULT)), levels,
                              [levels](std::size_t i) { return Local{l::Thinking{levels.at(i)}}; })));

    auto* tools = vcol({checks(state.choices().tools, state.tools_on(), true)});
    if (state.choices().tools_note) add(tools, kw::label(*state.choices().tools_note, TextRole::Caption, Tone::Muted));
    if (state.tools_on().empty()) add(tools, kw::label("None ticked: the agent gets omp's default tools", TextRole::Caption, Tone::Muted));
    add(form, labelled("Tools", "tools", tools));

    const auto others = state.other_agents();
    QWidget* spawns = others.empty() && state.spawns_on().empty() ? static_cast<QWidget*>(kw::label("No other agents to spawn", TextRole::Caption, Tone::Muted))
                                                                   : checks(others, state.spawns_on(), false);
    add(form, labelled("May spawn", "spawns", spawns));
    add(form, labelled("Prompt", "body",
                       editor(state.body(), "The agent's system prompt", [post](std::string t) { post(l::Body{std::move(t)}); }, "agent-prompt",
                              px(theme::width(LayoutRole::ListWidth)))));
    auto* save = hrow({action("Save", ButtonKind::primary(), send(l::Save{}))});
    stretch(save);
    add(form, save);
    stretch(form);

    auto* form_scroll = new QScrollArea;
    form_scroll->setWidgetResizable(true);
    form_scroll->setFrameShape(QFrame::NoFrame);
    auto* padded = hrow({form}, 0);
    padded->layout()->setContentsMargins(spi(SpaceRole::M), 0, spi(SpaceRole::M), 0);
    form_scroll->setWidget(padded);
    form_scroll->setAccessibleName(QStringLiteral("agent form"));

    auto* body = hrow({list_scroll, form_scroll}, spi(SpaceRole::L));
    QWidget* content = body;
    if (titled_) {
        auto* close = action("Close", ButtonKind::ghost(), [this] { emit message(Message{Intent{vs::intent::Close{}}}); });
        content = vcol({kw::section_header("Agents", {close}), body}, spi(SpaceRole::M));
        const int m = spi(SpaceRole::M);
        content->layout()->setContentsMargins(m, m, m, m);
    }
    content_ = swap_content(*this, content_, content);
}

} // namespace loa::ui::settings::agents
