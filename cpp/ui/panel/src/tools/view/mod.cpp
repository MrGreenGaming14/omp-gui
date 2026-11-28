// The tools page view and its shared pieces. Port of `crates/loa-ui-panel/src/tools/view/mod.rs`.

#include <format>

#include <QCheckBox>
#include <QComboBox>
#include <QFrame>
#include <QGridLayout>
#include <QLineEdit>

#include "loa/ui/panel/tools_view.hpp"
#include "view.hpp"

namespace loa::ui::panel::tools::view {

using namespace pw;

QLabel* bold(std::string_view s, TextRole role) {
    auto* l = kw::label(s, role, Tone::Normal);
    auto f = l->font();
    f.setBold(true);
    l->setFont(f);
    return l;
}

QWidget* card(const std::vector<QWidget*>& rows) {
    auto* content = new QWidget;
    auto* l = new QVBoxLayout(content);
    l->setContentsMargins(0, 0, 0, 0);
    l->setSpacing(0);
    for (std::size_t i = 0; i < rows.size(); ++i) {
        if (i > 0) {
            auto* rule = new QFrame;
            rule->setFrameShape(QFrame::HLine);
            rule->setAccessibleName(QStringLiteral("divider"));
            l->addWidget(rule);
        }
        l->addWidget(rows[i]);
    }
    return surface(content, Surface::Card, spi(SpaceRole::Xs));
}

QWidget* note_row(std::string_view s) {
    auto* w = hrow({kw::label(s, TextRole::Label, Tone::Muted)});
    w->layout()->setContentsMargins(spi(SpaceRole::M), spi(SpaceRole::S), spi(SpaceRole::M), spi(SpaceRole::S));
    return w;
}

QPushButton* row_button(QWidget* content, std::string_view name, std::function<void()> on) {
    return content_button(content, name, ButtonKind::row(false), std::move(on));
}

kit::Tone detail_tone(DetailState s) {
    static constexpr std::array<Tone, 3> T{Tone::Ok, Tone::Muted, Tone::Danger};
    return T[static_cast<std::size_t>(s)];
}

QWidget* status_dot(kit::Tone tone) {
    auto* d = new QWidget;
    const int size = px(theme::sp(SpaceRole::Xs) * 1.5F);
    d->setFixedSize(size, size);
    d->setAttribute(Qt::WA_StyledBackground, true);
    d->setStyleSheet(QString::fromStdString(std::format("background: {}; border-radius: {}px;", kit::qt::css(kit::color(tone)), size / 2)));
    return d;
}

QPushButton* primary(std::string_view s, std::function<void()> on) {
    auto* b = kw::text_button(s, ButtonKind::primary(), std::move(on));
    b->setAccessibleName(qs(s));
    return b;
}

QWidget* form_buttons(std::string_view submit, FormId form, const Emit& send) {
    auto* r = hrow({primary(submit, view::on(send, msg::FormSubmit{form})), action("Cancel", view::on(send, msg::FormCancel{form}))}, spi(SpaceRole::S));
    stretch(r);
    return r;
}

QWidget* form_card(std::string_view title, QWidget* trailing, QWidget* content, std::string_view submit, FormId form, const Emit& send) {
    auto* head = hrow({bold(title, TextRole::Label)});
    stretch(head);
    add(head, trailing);
    return kw::card(vcol({head, content, form_buttons(submit, form, send)}, spi(SpaceRole::S)));
}

QWidget* inset(QWidget* content) {
    auto* w = surface(content, Surface::Well, spi(SpaceRole::M));
    auto* outer = vcol({w});
    outer->layout()->setContentsMargins(0, 0, 0, spi(SpaceRole::Xs));
    return outer;
}

QWidget* grid(const std::vector<QWidget*>& items, bool wide) {
    if (!wide) {
        auto* c = vcol({}, spi(SpaceRole::L));
        for (auto* i : items) add(c, i);
        return c;
    }
    auto* w = new QWidget;
    auto* g = new QGridLayout(w);
    g->setContentsMargins(0, 0, 0, 0);
    g->setSpacing(spi(SpaceRole::L));
    for (std::size_t i = 0; i < items.size(); ++i)
        g->addWidget(items[i], static_cast<int>(i / 2), static_cast<int>(i % 2), Qt::AlignTop);
    g->setColumnStretch(0, 1);
    g->setColumnStretch(1, 1);
    return w;
}

QWidget* block_header(const State& s, const std::string& key, std::string_view title, std::size_t count, QWidget* trailing, const Emit& send) {
    const bool open = s.block_open(key), filtering = !s.filter().empty();
    auto* name = hrow({}, spi(SpaceRole::Xs));
    if (!filtering) {
        auto* chevron = new QLabel;
        chevron->setPixmap(kit::tinted(open ? Icon::ChevronDown : Icon::ChevronRight, TextRole::Caption, Tone::Muted));
        add(name, chevron);
    }
    add(name, kw::label(title, TextRole::Caption, Tone::Normal));
    add(name, kw::label(std::to_string(count), TextRole::Caption, Tone::Faint));
    auto* toggle = content_button(name, title, ButtonKind::flat(), filtering ? std::function<void()>{} : view::on(send, msg::BlockOpen{key, !open}));
    auto* r = hrow({toggle}, spi(SpaceRole::S));
    stretch(r);
    add(r, trailing);
    return r;
}

namespace {

// An open tool: its whole description and its details.
QWidget* tool_details(const ToolInfo& info) {
    auto* c = vcol({kw::label(info.description, TextRole::Caption, Tone::Muted)}, spi(SpaceRole::Xs));
    for (const auto& d : info.details) {
        auto* name = kw::label(d.label, TextRole::Caption, Tone::Normal);
        add(c, hrow({status_dot(detail_tone(d.state)), name, kw::label(d.text, TextRole::Caption, Tone::Muted)}, spi(SpaceRole::S)));
    }
    c->layout()->setContentsMargins(spi(SpaceRole::Xl) + spi(SpaceRole::S), 0, spi(SpaceRole::M), spi(SpaceRole::M));
    return c;
}

// Checkbox, name, clipped description and detail dots; a click opens the details.
QWidget* tool_row(const State& s, const ToolRow& t, const Emit& send) {
    const bool expanded = s.expanded_tool() == t.id;
    auto* check = new QCheckBox;
    check->setChecked(t.enabled);
    check->setAccessibleName(qs(t.info->name));
    check->setEnabled(s.project().has_value());
    QObject::connect(check, &QCheckBox::toggled, check, [send, id = t.id](bool on) { send(msg::ToolToggled{id, on}); });
    auto* desc = kw::label(t.info->description, TextRole::Caption, Tone::Muted);
    desc->setMinimumWidth(0);
    desc->setSizePolicy(QSizePolicy::Ignored, QSizePolicy::Preferred);
    auto* line = hrow({check, kw::label(t.info->name, TextRole::Label, Tone::Normal), desc}, spi(SpaceRole::S));
    static_cast<QBoxLayout*>(line->layout())->setStretchFactor(desc, 1);
    if (!t.info->details.empty()) {
        auto* dots = hrow({}, spi(SpaceRole::Xs));
        for (const auto& d : t.info->details) add(dots, status_dot(detail_tone(d.state)));
        add(line, dots);
    }
    auto* chevron = new QLabel;
    chevron->setPixmap(kit::tinted(expanded ? Icon::ChevronUp : Icon::ChevronDown, TextRole::Caption, Tone::Muted));
    add(line, chevron);
    auto* c = vcol({row_button(line, t.info->name, view::on(send, msg::ToolExpanded{t.id}))});
    if (expanded) add(c, tool_details(*t.info));
    return c;
}

} // namespace

std::vector<QWidget*> tool_rows(const State& s, const std::string& key, const std::vector<ToolRow>& tools, const Emit& send) {
    const std::size_t total = tools.size();
    const std::string all_key = key + ":all";
    const bool all = !s.filter().empty() || total <= COLLAPSE_AT || s.block_flag(all_key, false);
    std::vector<QWidget*> rows;
    for (std::size_t i = 0; i < (all ? total : std::min(total, PREVIEW)); ++i) rows.push_back(tool_row(s, tools[i], send));
    if (total > COLLAPSE_AT && s.filter().empty()) {
        const auto more = all ? std::string("Show fewer") : std::format("Show all {}", total);
        rows.push_back(row_button(kw::label(more, TextRole::Caption, Tone::Muted), more, view::on(send, msg::BlockOpen{all_key, !all})));
    }
    return rows;
}

std::map<std::string, std::string, std::less<>> form_errors(const Errors& errors) {
    std::map<std::string, std::string, std::less<>> out;
    for (const auto& [k, v] : errors) out.emplace(std::string(k), v);
    return out;
}

namespace {

QWidget* group_view(const State& s, const GroupView& group, const Emit& send) {
    std::string key;
    std::string title;
    if (group.group == BuiltinGroup::Harness) {
        key = "harness";
        title = group.title.empty() ? std::string("Harness tools") : std::format("Harness tools \u00b7 {}", group.title);
    } else {
        key = "app";
        title = std::string(group.title);
    }
    auto* header = block_header(s, key, title, group.tools.size(), nullptr, send);
    if (!s.block_open(key)) return header;
    std::vector<QWidget*> rows;
    if (group.group == BuiltinGroup::Harness && s.builtin().harness_note) rows.push_back(note_row(*s.builtin().harness_note));
    else if (group.tools.empty()) rows.push_back(note_row("No tools"));
    else rows = tool_rows(s, key, group.tools, send);
    return vcol({header, card(rows)}, spi(SpaceRole::Xs));
}

} // namespace

} // namespace loa::ui::panel::tools::view

namespace loa::ui::panel {

using namespace pw;
namespace tv = tools::view;

ToolsView::ToolsView(QWidget* parent) : QWidget(parent) {
    setAccessibleName(QStringLiteral("tools"));
    auto* l = new QVBoxLayout(this);
    l->setContentsMargins(spi(SpaceRole::M), spi(SpaceRole::S), spi(SpaceRole::Xs) / 2, 0);
    l->setSpacing(spi(SpaceRole::M));
    tabs_ = new QWidget;
    search_ = new QLineEdit;
    search_->setPlaceholderText(QStringLiteral("Filter tools, servers and hosts"));
    search_->setAccessibleName(QStringLiteral("tools filter"));
    connect(search_, &QLineEdit::textEdited, this, [this](const QString& t) { emit message(tools::msg::FilterChanged{t.toStdString()}); });
    controls_ = new QWidget;
    body_ = new QWidget;
    auto* header = vcol({tabs_, hrow({search_, controls_}, spi(SpaceRole::S))}, spi(SpaceRole::S));
    l->addWidget(header);
    l->addWidget(body_, 1);
}

void ToolsView::render(const tools::State& state) {
    using tools::Section;
    namespace m = tools::msg;
    tools::view::Emit emit_fn = [this](tools::Message msg) { emit message(msg); };
    const bool wide = width() >= tv::TWO_COLUMNS;
    const auto catalog = state.catalog();

    std::size_t builtin_count = 0;
    for (const auto& g : catalog.builtin) builtin_count += g.tools.size();
    const auto hosts_title = state.prompts().empty() ? std::string("Remote hosts") : std::format("Remote hosts ({})", state.prompts().size());
    const std::array<std::tuple<Section, std::string, std::size_t>, 4> sections{{
        {Section::Builtin, "Built in", builtin_count},
        {Section::Servers, "MCP servers", catalog.servers.size()},
        {Section::Hosts, hosts_title, state.visible_hosts().size()},
        {Section::Peers, "Peers", state.peers().size()},
    }};
    auto* tabs = hrow({}, spi(SpaceRole::L));
    for (const auto& [section, name, count] : sections) {
        const bool active = section == state.section();
        const auto tone = active ? Tone::Normal : (section == Section::Hosts && !state.prompts().empty()) ? Tone::Warn : Tone::Muted;
        auto* content = hrow({kw::label(name, TextRole::Label, tone)}, spi(SpaceRole::Xs));
        if (count > 0) add(content, kw::label(std::to_string(count), TextRole::Caption, Tone::Faint));
        add(tabs, content_button(content, name, ButtonKind::tab(active), tv::on(emit_fn, m::SectionSelected{section})));
    }
    stretch(tabs);
    replace(tabs_, tabs);

    if (search_->text().toStdString() != state.filter_text()) search_->setText(QString::fromStdString(state.filter_text()));

    const bool toggles = state.section() == Section::Builtin || state.section() == Section::Servers;
    auto* controls = hrow({});
    if (toggles) {
        auto* combo = new QComboBox;
        combo->setAccessibleName(QStringLiteral("project"));
        combo->setPlaceholderText(QStringLiteral("Choose a project"));
        int current = -1;
        for (std::size_t i = 0; i < state.projects().size(); ++i) {
            combo->addItem(QString::fromStdString(state.projects()[i].name));
            if (state.project() == state.projects()[i].id) current = static_cast<int>(i);
        }
        combo->setCurrentIndex(current);
        connect(combo, &QComboBox::activated, combo, [emit_fn, projects = state.projects()](int i) {
            if (i >= 0 && static_cast<std::size_t>(i) < projects.size()) emit_fn(m::ProjectSelected{projects[static_cast<std::size_t>(i)]});
        });
        add(controls, combo);
    }
    replace(controls_, controls);

    auto* content = vcol({}, spi(SpaceRole::S));
    if (toggles && !state.project()) add(content, kw::label("Choose a project to switch tools on or off", TextRole::Caption, Tone::Faint));
    switch (state.section()) {
    case Section::Builtin: {
        std::vector<QWidget*> groups;
        for (const auto& g : catalog.builtin) groups.push_back(tv::group_view(state, g, emit_fn));
        if (groups.empty()) add(content, kw::label("No built in tool matches the filter", TextRole::Label, Tone::Muted));
        else add(content, tv::grid(groups, wide));
        break;
    }
    case Section::Servers: add(content, new tv::Servers(state, catalog, wide, emit_fn)); break;
    case Section::Hosts: add(content, new tv::Hosts(state, wide, emit_fn)); break;
    case Section::Peers: add(content, new tv::Peers(state, emit_fn)); break;
    }
    stretch(content);
    content->layout()->setContentsMargins(0, spi(SpaceRole::Xs), spi(SpaceRole::S), spi(SpaceRole::M));
    replace(body_, pw::scroll(content, "tools body"));
}

} // namespace loa::ui::panel
