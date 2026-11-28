// Qt view of the workflows screen. Port of `State::{view, body, form}` in
// `crates/loa-ui-settings/src/workflows/mod.rs`.

#include "loa/ui/settings/workflows/view.hpp"

#include <array>
#include <format>

#include <QButtonGroup>
#include <QLabel>
#include <QLineEdit>
#include <QPlainTextEdit>
#include <QPushButton>
#include <QRadioButton>
#include <QScrollArea>
#include <QVBoxLayout>

#include "loa/ui/kit/qt.hpp"
#include "settings_widgets.hpp"

namespace loa::ui::settings::workflows {

using namespace sw;
namespace rules = vs::rules;

namespace {

std::string join(const std::vector<std::string>& v) {
    std::string out;
    for (const auto& s : v) out += (out.empty() ? "" : ", ") + s;
    return out;
}

// The first non-blank line, trimmed: sections run long and a list row shows only that.
std::string first_line(std::string_view text) {
    while (!text.empty()) {
        const auto nl = text.find('\n');
        auto line = text.substr(0, nl);
        const auto b = line.find_first_not_of(" \t\r");
        if (b != std::string_view::npos) {
            line.remove_prefix(b);
            return std::string(line.substr(0, line.find_last_not_of(" \t\r") + 1));
        }
        if (nl == std::string_view::npos) break;
        text.remove_prefix(nl + 1);
    }
    return {};
}

} // namespace

WorkflowsView::WorkflowsView(bool titled, QWidget* parent) : QWidget(parent), titled_(titled) {
    auto* l = new QVBoxLayout(this);
    l->setContentsMargins(0, 0, 0, 0);
    setAccessibleName(QStringLiteral("workflows settings"));
}

void WorkflowsView::render(const State& state) {
    namespace l = local;
    auto post = [this](Local m) { emit message(Message{std::move(m)}); };
    auto send = [post](Local m) -> Callback { return [post, m] { post(m); }; };
    auto muted = [](std::string_view s) { return kw::label(s, TextRole::Label, Tone::Muted); };
    const int editor_height = px(theme::width(LayoutRole::ListWidth));

    // The list.
    auto* list = vcol({action("New workflow", ButtonKind::secondary(), send(l::New{}))});
    for (const auto& entry : state.entries()) {
        std::string preview = std::visit(
            [&](const auto& r) -> std::string {
                using T = std::decay_t<decltype(r)>;
                if constexpr (std::is_same_v<T, rules::None>) return "No rules in RULES.md for this role";
                else if constexpr (std::is_same_v<T, rules::Managed>)
                    return std::format("{} \u00b7 Allow: {} \u00b7 Disallow: {} \u00b7 {}", r.name, join(r.allowed), join(r.disallowed), first_line(r.notes));
                else return first_line(r.text);
            },
            entry.rules);
        const auto detail = std::format("{} \u00b7 {}", join(entry.models), preview);
        add(list, kw::two_line_row(entry.role, detail, state.selected() == entry.role, send(l::Open{entry.role})));
        if (!std::holds_alternative<rules::None>(entry.rules))
            add(list, action(std::format("Copy from {}\u2026", entry.role), ButtonKind::ghost(), send(l::CopyFrom{entry.role})));
    }
    stretch(list);
    auto* list_scroll = new QScrollArea;
    list_scroll->setWidgetResizable(true);
    list_scroll->setFrameShape(QFrame::NoFrame);
    list_scroll->setWidget(list);
    list_scroll->setFixedWidth(px(theme::width(LayoutRole::ListWidth)));
    list_scroll->setAccessibleName(QStringLiteral("workflow list"));

    // The editor.
    auto* form = vcol({}, spi(SpaceRole::M));
    form->setMaximumWidth(px(theme::width(LayoutRole::ContentWidth)));
    if (const auto& error = state.error()) {
        const bool changed = std::holds_alternative<vs::error::ChangedOnDisk>(*error);
        add(form, errors({changed ? std::string("Changed outside Loa \u2014 reload before saving") : std::get<vs::error::Message>(*error).message}));
        if (changed)
            add(form, action("Reload", ButtonKind::secondary(), [this] { emit message(Message{Intent{vs::intent::Reload{}}}); }));
    }
    if (!state.errors().empty()) {
        std::vector<std::string> lines;
        for (const auto& e : state.errors()) lines.push_back(std::format("{}: {}", e.field, e.message));
        add(form, errors(lines));
    }
    add(form, muted("Role"));
    const auto& r = state.rules();
    if (std::holds_alternative<rules::External>(r)) {
        add(form, kw::label(state.role(), TextRole::Body, Tone::Normal));
        add(form, muted("Edited outside Loa \u2014 edit the raw Markdown below; this workflow cannot be renamed here."));
    } else {
        add(form, field("ROLE", state.role(), [post](std::string v) { post(l::Role{std::move(v)}); }, "workflow-role"));
    }
    add(form, muted("Models (comma separated)"));
    add(form, field("provider/model", state.models(), [post](std::string v) { post(l::Models{std::move(v)}); }, "workflow-models"));

    if (const auto* m = std::get_if<rules::Managed>(&r)) {
        const auto& fields = m->fields;
        add(form, muted("Name"));
        add(form, field("Research", m->name, [post](std::string v) { post(l::Name{std::move(v)}); }, "workflow-name"));
        add(form, muted("Description"));
        add(form, field("Short description", fields.description, [post](std::string v) { post(l::Description{std::move(v)}); },
                        "workflow-description"));
        auto agents = state.agents();
        for (const auto* names : {&m->allowed, &m->disallowed})
            for (const auto& name : *names)
                if (std::ranges::none_of(agents, [&](const AgentInfo& a) { return a.name == name; })) agents.push_back(AgentInfo{name, false});
        add(form, muted("Agents"));
        for (const auto& agent : agents) {
            const auto caption = agent.enabled ? agent.name : std::format("{} (disabled)", agent.name);
            auto* choices = hrow({kw::label(caption, TextRole::Body, Tone::Normal)});
            auto* group = new QButtonGroup(choices);
            const auto current = state.access(agent.name);
            for (const auto access : std::array{Access::Allow, Access::Disallow, Access::Unspecified}) {
                auto* radio = new QRadioButton(qs(label(access)));
                radio->setFont(kit::qt::font(TextRole::Label));
                radio->setChecked(access == current);
                const auto tip = std::format("{} {}", label(access), agent.name);
                radio->setToolTip(qs(tip));
                radio->setAccessibleName(qs(tip));
                group->addButton(radio);
                QObject::connect(radio, &QRadioButton::clicked, radio, [post, name = agent.name, access] { post(l::SetAccess{name, access}); });
                add(choices, radio);
            }
            stretch(choices);
            add(form, choices);
            if (std::ranges::find(m->allowed, agent.name) != m->allowed.end()) {
                const auto it = std::ranges::find_if(fields.routing, [&](const RoutingNote& n) { return n.agent == agent.name; });
                add(form, muted("Routing note"));
                add(form, field("When to use this agent", it != fields.routing.end() ? it->note : std::string(),
                                [post, name = agent.name](std::string v) { post(l::Routing{name, std::move(v)}); },
                                std::format("workflow-routing-{}", agent.name)));
            }
        }
        add(form, muted("Steps"));
        const auto steps = fields.steps.size();
        for (std::size_t i = 0; i < steps; ++i) {
            auto* row = hrow({field("Step", fields.steps[i], [post, i](std::string v) { post(l::Step{i, std::move(v)}); }, std::format("workflow-step-{}", i)),
                              kw::reorder(i, steps,
                                          [post](std::size_t from, std::size_t to) {
                                              post(to < from ? Local{l::MoveStepUp{from}} : Local{l::MoveStepDown{from}});
                                          }),
                              action("Remove", ButtonKind::danger(), send(l::RemoveStep{i}))});
            add(form, muted(std::format("Step {}", i + 1)));
            add(form, row);
        }
        add(form, action("Add step", ButtonKind::secondary(), send(l::AddStep{})));
        add(form, muted("Rule blocks"));
        for (std::size_t i = 0; i < fields.blocks.size(); ++i) {
            add(form, muted("Heading"));
            add(form, field("Rule block heading", fields.blocks[i].heading, [post, i](std::string v) { post(l::BlockHeading{i, std::move(v)}); },
                            std::format("workflow-block-heading-{}", i)));
            add(form, muted("Markdown"));
            add(form, editor(state.block_text(i), "", [post, i](std::string t) { post(l::BlockText{i, std::move(t)}); },
                             std::format("workflow-block-{}", i), editor_height));
            add(form, action("Remove block", ButtonKind::danger(), send(l::RemoveBlock{i})));
        }
        add(form, action("Add block", ButtonKind::secondary(), send(l::AddBlock{})));
    }
    if (!std::holds_alternative<rules::None>(r)) {
        const bool raw = std::holds_alternative<rules::HandWritten>(r) || std::holds_alternative<rules::External>(r);
        add(form, muted(raw ? "Rules markdown" : "Notes"));
        add(form, editor(state.text(), "", [post](std::string t) { post(l::Text{std::move(t)}); }, "workflow-text", editor_height));
    } else {
        add(form, muted("No rules in RULES.md for this role"));
    }
    auto* actions = hrow({action("Save", ButtonKind::primary(), send(l::Save{}))});
    if (state.selected()) {
        if (state.confirming_delete()) {
            add(actions, action("Confirm delete", ButtonKind::danger(), send(l::ConfirmDelete{})));
            add(actions, action("Cancel", ButtonKind::secondary(), send(l::CancelDelete{})));
        } else {
            add(actions, action("Delete", ButtonKind::danger(), send(l::Delete{})));
        }
    }
    stretch(actions);
    add(form, actions);
    stretch(form);

    auto* form_scroll = new QScrollArea;
    form_scroll->setWidgetResizable(true);
    form_scroll->setFrameShape(QFrame::NoFrame);
    auto* padded = hrow({form}, 0);
    const int pad = spi(SpaceRole::M);
    padded->layout()->setContentsMargins(pad, pad, pad, pad);
    form_scroll->setWidget(padded);
    form_scroll->setAccessibleName(QStringLiteral("workflow editor"));

    auto* body = hrow({list_scroll, form_scroll}, spi(SpaceRole::L));
    QWidget* content = body;
    if (titled_) {
        auto* close = action("Close", ButtonKind::ghost(), [this] { emit message(Message{Intent{vs::intent::Close{}}}); });
        content = vcol({kw::section_header("Workflows", {close}), body}, spi(SpaceRole::M));
        content->layout()->setContentsMargins(pad, pad, pad, pad);
    }
    content_ = swap_content(*this, content_, content);
}

} // namespace loa::ui::settings::workflows
