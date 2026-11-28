// Qt views of the models screen: the page frame (`mod.rs`) and the `view` halves of
// `picker.rs`, `settings.rs` and `signin.rs` in `crates/loa-ui-settings/src/models/`.

#include "loa/ui/settings/models/view.hpp"

#include <cmath>
#include <format>
#include <initializer_list>

#include <QApplication>
#include <QBoxLayout>
#include <QCheckBox>
#include <QComboBox>
#include <QEvent>
#include <QKeyEvent>
#include <QLabel>
#include <QLineEdit>
#include <QPushButton>
#include <QScrollArea>
#include <QScrollBar>

#include "loa/ui/kit/qt.hpp"
#include "loa/ui/kit/style.hpp"
#include "loa/ui/kit/theme.hpp"
#include "loa/ui/kit/widgets.hpp"

namespace loa::ui::settings::models {

namespace {

namespace theme = kit::theme;
namespace kw = kit::widgets;
namespace vm = loa::view::settings::models;
using kit::ButtonKind;
using kit::Surface;
using kit::Tone;
using theme::LayoutRole;
using theme::SpaceRole;
using theme::TextRole;

QString qs(std::string_view s) { return QString::fromUtf8(s.data(), static_cast<qsizetype>(s.size())); }
int px(float f) { return static_cast<int>(std::lround(f)); }
int spi(SpaceRole r) { return px(theme::sp(r)); }

QWidget* box(QBoxLayout::Direction dir, std::initializer_list<QWidget*> items, int spacing) {
    auto* w = new QWidget;
    auto* l = new QBoxLayout(dir, w);
    l->setContentsMargins(0, 0, 0, 0);
    l->setSpacing(spacing);
    for (auto* i : items)
        if (i) l->addWidget(i);
    return w;
}
QWidget* hrow(std::initializer_list<QWidget*> items, int spacing = spi(SpaceRole::S)) { return box(QBoxLayout::LeftToRight, items, spacing); }
QWidget* vcol(std::initializer_list<QWidget*> items, int spacing = spi(SpaceRole::Xs)) { return box(QBoxLayout::TopToBottom, items, spacing); }
void add(QWidget* parent, QWidget* child, int stretch = 0) {
    if (child) static_cast<QBoxLayout*>(parent->layout())->addWidget(child, stretch);
}
void stretch(QWidget* parent) { static_cast<QBoxLayout*>(parent->layout())->addStretch(1); }

// The crate's `action`: a text button in `kind`, disabled without `on`.
QPushButton* action(std::string_view s, ButtonKind kind, std::function<void()> on) {
    auto* b = kw::text_button(s, kind, std::move(on));
    b->setAccessibleName(qs(s));
    return b;
}

// A small status light in `tone`.
QWidget* dot(Tone tone) { return kw::label("\u25CF", TextRole::Caption, tone); }

// A text input in the kit look; `name` identifies it across renders so focus survives a rebuild.
QLineEdit* field(std::string_view name, std::string_view placeholder, std::string_view value, std::function<void(std::string)> on_input,
                 std::function<void()> on_submit = {}) {
    auto* e = new QLineEdit(qs(value));
    e->setObjectName(qs(name));
    e->setAccessibleName(qs(name));
    e->setPlaceholderText(qs(placeholder));
    e->setFont(kit::qt::font(TextRole::Body));
    QObject::connect(e, &QLineEdit::textEdited, e, [on_input = std::move(on_input)](const QString& t) { on_input(t.toStdString()); });
    if (on_submit) QObject::connect(e, &QLineEdit::returnPressed, e, std::move(on_submit));
    return e;
}

// A check box in the kit look.
QCheckBox* check(bool on, std::string_view s, std::function<void(bool)> toggle) {
    auto* c = new QCheckBox(qs(s));
    c->setAccessibleName(qs(s));
    c->setChecked(on);
    c->setFont(kit::qt::font(TextRole::Label));
    QObject::connect(c, &QCheckBox::toggled, c, std::move(toggle));
    return c;
}

// A pick list over `options`; `selected` empty shows `placeholder`.
QComboBox* pick(std::span<const std::string> options, std::optional<std::string_view> selected, std::string_view placeholder,
                std::function<void(std::string)> on) {
    auto* c = new QComboBox;
    c->setAccessibleName(qs(placeholder.empty() ? "pick" : placeholder));
    c->setFont(kit::qt::font(TextRole::Label));
    c->setPlaceholderText(qs(placeholder));
    for (const auto& o : options) c->addItem(qs(o));
    c->setCurrentIndex(-1);
    if (selected)
        for (std::size_t i = 0; i < options.size(); ++i)
            if (options[i] == *selected) c->setCurrentIndex(static_cast<int>(i));
    QObject::connect(c, &QComboBox::activated, c, [c, on = std::move(on)](int i) {
        if (i >= 0) on(c->itemText(i).toStdString());
    });
    return c;
}

// `content` after a muted label in the label column.
QWidget* labeled(std::string_view name, QWidget* content) {
    auto* l = kw::label(name, TextRole::Label, Tone::Muted);
    l->setFixedWidth(px(theme::width(LayoutRole::LabelColumn)));
    auto* r = hrow({l, content}, spi(SpaceRole::M));
    static_cast<QBoxLayout*>(r->layout())->setStretch(1, 1);
    return r;
}

// A headed section of a screen.
QWidget* section(std::string_view title, QWidget* body) { return vcol({kw::section_header(title, {}), body}, spi(SpaceRole::S)); }

// `w` in a container of surface `s` with padding `pad`.
QWidget* surface(QWidget* w, Surface s, int pad, std::string_view name) {
    auto* c = new QWidget;
    // Scopes the surface stylesheet, so it must be a plain identifier; `name` is for accessibility.
    c->setObjectName(QStringLiteral("loa-models-surface"));
    c->setAccessibleName(qs(name));
    c->setAttribute(Qt::WA_StyledBackground, true);
    auto* l = new QVBoxLayout(c);
    l->setContentsMargins(pad, pad, pad, pad);
    l->addWidget(w);
    kit::qt::style_container(*c, s);
    return c;
}

// Problems in a danger box, one per line.
QWidget* errors(const std::vector<std::string>& lines) {
    auto* col = vcol({});
    for (const auto& line : lines) {
        auto* l = kw::label(line, TextRole::Label, Tone::Danger);
        l->setWordWrap(true);
        add(col, l);
    }
    return surface(col, Surface::ErrorBox, spi(SpaceRole::S), "errors");
}

// Replaces what `scroll` shows with `content`, keeping the scroll position and the focused input.
void show_in(QScrollArea* scroll, QWidget* content) {
    QString focused;
    int cursor = 0;
    if (auto* f = qobject_cast<QLineEdit*>(QApplication::focusWidget()); f && scroll->isAncestorOf(f) && !f->objectName().isEmpty()) {
        focused = f->objectName();
        cursor = f->cursorPosition();
    }
    const int y = scroll->verticalScrollBar()->value();
    if (auto* old = scroll->takeWidget()) {
        old->hide();
        old->deleteLater();
    }
    scroll->setWidget(content);
    scroll->verticalScrollBar()->setValue(y);
    if (!focused.isEmpty())
        if (auto* f = content->findChild<QLineEdit*>(focused)) {
            f->setFocus();
            f->setCursorPosition(cursor);
        }
}

QScrollArea* make_scroll(QWidget* owner, std::string_view name) {
    auto* l = new QVBoxLayout(owner);
    l->setContentsMargins(0, 0, 0, 0);
    auto* s = new QScrollArea(owner);
    s->setWidgetResizable(true);
    s->setFrameShape(QFrame::NoFrame);
    s->setAccessibleName(qs(name));
    l->addWidget(s);
    return s;
}

// A screen body: padded column.
QWidget* body_column(int spacing) {
    auto* w = vcol({}, spacing);
    w->layout()->setContentsMargins(spi(SpaceRole::M), spi(SpaceRole::M), spi(SpaceRole::M), spi(SpaceRole::M));
    return w;
}

} // namespace

// ---- Frame ---------------------------------------------------------------------------------

QWidget* frame(Page page, QWidget* body, std::function<void(Page)> select, std::function<void()> close, QWidget* parent) {
    auto* root = new QWidget(parent);
    root->setAccessibleName(QStringLiteral("models"));
    std::vector<kw::Tab> tabs;
    for (const auto p : loa::core::all<Page>()) tabs.push_back({loa::core::as_str(p), std::nullopt, p == page, [select, p] { select(p); }, nullptr});
    auto* head = hrow({kw::label("Models", TextRole::Heading, Tone::Normal), kw::tab_strip(tabs)}, spi(SpaceRole::M));
    stretch(head);
    add(head, kw::icon_button(loa::view::theme::Icon::X, "Close", std::move(close)));
    head->layout()->setContentsMargins(spi(SpaceRole::M), spi(SpaceRole::S), spi(SpaceRole::M), spi(SpaceRole::S));
    auto* rule = new QFrame;
    rule->setFrameShape(QFrame::HLine);
    rule->setAccessibleName(QStringLiteral("rule"));
    auto* l = new QVBoxLayout(root);
    l->setContentsMargins(0, 0, 0, 0);
    l->setSpacing(0);
    l->addWidget(head);
    l->addWidget(rule);
    l->addWidget(body, 1);
    return root;
}

QWidget* status(std::string_view message, QWidget* parent) {
    auto* w = new QWidget(parent);
    auto* l = new QVBoxLayout(w);
    auto* label = kw::label(message, TextRole::Label, Tone::Muted);
    l->addWidget(label, 0, Qt::AlignCenter);
    w->setAccessibleName(qs(message));
    return w;
}

// ---- Picker --------------------------------------------------------------------------------

PickerView::PickerView(QWidget* parent) : QWidget(parent) {
    setAccessibleName(QStringLiteral("model picker"));
    setAttribute(Qt::WA_StyledBackground, true);
    setObjectName(QStringLiteral("loa-model-picker"));
    kit::qt::style_container(*this, Surface::Popup);
    setFixedWidth(px(theme::width(LayoutRole::PopupWidth)));
    auto* l = new QVBoxLayout(this);
    const int pad = spi(SpaceRole::S);
    l->setContentsMargins(pad, pad, pad, pad);
    l->setSpacing(pad);
    heading_ = new QWidget;
    new QHBoxLayout(heading_);
    heading_->layout()->setContentsMargins(0, 0, 0, 0);
    search_ = field(picker::SEARCH_ID, "Search models", "", [this](std::string q) { emit message(picker::Message::local(picker::local::Query{std::move(q)})); },
                    [this] { emit message(picker::Message::local(picker::local::Submit{})); });
    search_->installEventFilter(this);
    list_ = new QScrollArea;
    list_->setWidgetResizable(true);
    list_->setFrameShape(QFrame::NoFrame);
    list_->setObjectName(qs(picker::LIST_ID));
    list_->setAccessibleName(QStringLiteral("models"));
    list_->setFixedHeight(px(theme::width(LayoutRole::ListWidth)));
    l->addWidget(heading_);
    l->addWidget(search_);
    l->addWidget(list_);
}

bool PickerView::eventFilter(QObject* watched, QEvent* event) {
    if (watched == search_ && event->type() == QEvent::KeyPress)
        if (auto m = picker::State::key(static_cast<QKeyEvent*>(event)->key())) {
            emit message(*m);
            return true;
        }
    return QWidget::eventFilter(watched, event);
}

void PickerView::render(const picker::State& state) {
    namespace vt = vm::picker::target;
    const auto heading = std::holds_alternative<vt::Session>(state.target())
                             ? std::string("Model for this session")
                             : std::format("Model for role {}", std::get<vt::Role>(state.target()).role);
    auto* hl = static_cast<QBoxLayout*>(heading_->layout());
    while (auto* item = hl->takeAt(0)) {
        if (auto* w = item->widget()) w->deleteLater();
        delete item;
    }
    hl->addWidget(kw::label(heading, TextRole::Body, Tone::Normal));
    hl->addStretch(1);
    hl->addWidget(kw::icon_button(loa::view::theme::Icon::X, "Close", [this] { emit message(picker::Message::event(vm::picker::intent::Close{})); }));
    if (search_->text().toStdString() != state.query()) search_->setText(qs(state.query()));

    auto* list = vcol({}, spi(SpaceRole::S));
    const auto& catalog = state.catalog();
    const auto selected = state.selected();
    for (const auto& g : state.groups()) {
        auto* col = vcol({kw::label(g.title, TextRole::Caption, Tone::Muted)}, px(theme::sp(SpaceRole::Xs) / 2.0f));
        for (std::size_t o = 0; o < g.rows.size(); ++o) {
            const auto index = g.start + o;
            const auto row = g.rows[o];
            std::string title, detail;
            if (row.kind == picker::Row::Kind::Role) {
                title = catalog.roles[row.index].role;
                detail = catalog.roles[row.index].model.selector();
            } else {
                const auto& info = catalog.models[row.index];
                std::string roles;
                for (auto r : state.roles_of(info.model)) roles += (roles.empty() ? "" : ", ") + std::string(r);
                const std::string_view current = state.current() == info.model ? " · current" : "";
                title = info.display_name();
                detail = roles.empty() ? std::format("{}{}", info.model.id, current) : std::format("{} · {}{}", info.model.id, roles, current);
            }
            auto* b = kw::two_line_row(title, detail, selected == index, [this, index] { emit message(picker::Message::local(picker::local::Pick{index})); });
            b->setAccessibleName(qs(title));
            add(col, b);
        }
        add(list, col);
    }
    if (state.rows().empty()) add(list, kw::label("No models match", TextRole::Label, Tone::Muted));
    stretch(list);
    if (auto* old = list_->takeWidget()) {
        old->hide();
        old->deleteLater();
    }
    list_->setWidget(list);
}

void PickerView::apply(const shell::Task& task) {
    for (const auto& op : task.ops) {
        if (const auto* f = std::get_if<shell::op::Focus>(&op); f && f->id == picker::SEARCH_ID) search_->setFocus();
        if (const auto* s = std::get_if<shell::op::SnapTo>(&op); s && s->id == picker::LIST_ID && s->y) {
            auto* bar = list_->verticalScrollBar();
            bar->setValue(bar->minimum() + static_cast<int>(std::lround(*s->y * static_cast<float>(bar->maximum() - bar->minimum()))));
        }
    }
}

// ---- Settings ------------------------------------------------------------------------------

SettingsView::SettingsView(QWidget* parent) : QWidget(parent) {
    setAccessibleName(QStringLiteral("model settings"));
    scroll_ = make_scroll(this, "model settings body");
}

void SettingsView::render(const settings::State& s) {
    namespace l = settings::local;
    using settings::Message;
    auto send = [this](settings::Local m) { return [this, m] { emit message(Message::local(m)); }; };
    const auto& d = s.draft();
    const bool dirty = s.is_dirty();
    auto when_dirty = [&](settings::Local m) -> std::function<void()> { return dirty ? std::function<void()>(send(std::move(m))) : nullptr; };

    auto* root = body_column(spi(SpaceRole::L));
    auto* head = hrow({kw::label("Model settings", TextRole::Body, Tone::Normal)});
    stretch(head);
    if (dirty) add(head, kw::label("Unsaved changes", TextRole::Caption, Tone::Muted));
    add(head, action("Revert", ButtonKind::ghost(), when_dirty(l::Revert{})));
    add(head, action("Save", ButtonKind::primary(), when_dirty(l::Save{})));
    add(root, head);
    if (!s.errors().empty()) {
        std::vector<std::string> lines;
        for (const auto& e : s.errors()) lines.push_back(std::format("{}: {}", e.location, e.message));
        add(root, errors(lines));
    }
    if (!s.local_errors().empty()) add(root, errors({s.local_errors().begin(), s.local_errors().end()}));

    auto card = [](QWidget* content, std::string_view name) { return surface(content, Surface::Card, spi(SpaceRole::S), name); };

    // Model roles.
    auto* roles = vcol({}, spi(SpaceRole::M));
    for (std::size_t i = 0; i < d.base.roles.size(); ++i) {
        const auto& role = d.base.roles[i];
        auto* name = hrow({field(std::format("role-{}-name", i), "Role name", role.role, [this, i](std::string n) { emit message(Message::local(l::RenameRole{i, std::move(n)})); }),
                           action("Remove role", ButtonKind::danger(), send(l::RemoveRole{i}))});
        auto* hint = kw::label("The first model is the primary, the rest its fallbacks in order; type provider/model[:thinking] to add one", TextRole::Caption, Tone::Faint);
        hint->setWordWrap(true);
        const std::string_view input = i < s.chain_inputs().size() ? std::string_view(s.chain_inputs()[i]) : std::string_view();
        auto* chain = kw::list_editor(
            role.chain, [this, i](std::size_t j) { emit message(Message::local(l::RemoveChain{i, j})); },
            [this, i](std::size_t from, std::size_t to) { emit message(Message::local(l::MoveChain{i, from, to})); }, input,
            [this, i](std::string v) { emit message(Message::local(l::ChainInput{i, std::move(v)})); },
            [this, i] { emit message(Message::local(l::AddChain{i, std::nullopt})); });
        auto* add_model = pick(s.known(), std::nullopt, "Add model", [this, i](std::string sel) { emit message(Message::local(l::AddChain{i, std::move(sel)})); });
        add(roles, card(vcol({name, hint, chain, add_model}, spi(SpaceRole::S)), std::format("role {}", role.role)));
    }
    add(roles, hrow({field("new-role", "New role name", s.new_role(), [this](std::string n) { emit message(Message::local(l::NewRole{std::move(n)})); },
                           send(l::AddRole{})),
                     action("Add role", ButtonKind::secondary(), send(l::AddRole{}))}));
    add(root, section("Model roles", roles));

    // Enabled models.
    QWidget* models = nullptr;
    if (d.base.models.empty()) models = kw::label("No models available", TextRole::Label, Tone::Muted);
    else {
        models = vcol({});
        for (std::size_t i = 0; i < d.base.models.size(); ++i)
            add(models, check(d.base.models[i].enabled, d.base.models[i].selector, [this, i](bool on) { emit message(Message::local(l::ModelToggled{i, on})); }));
    }
    add(root, section("Enabled models", models));

    // Provider order.
    QWidget* providers = nullptr;
    if (d.base.providers.empty()) providers = kw::label("No providers available", TextRole::Label, Tone::Muted);
    else {
        providers = vcol({});
        const auto n = d.base.providers.size();
        for (std::size_t i = 0; i < n; ++i) {
            const auto& p = d.base.providers[i];
            const auto name = p.name.empty() ? p.id : std::format("{} ({})", p.name, p.id);
            auto* r = hrow({check(p.enabled, name, [this, i](bool on) { emit message(Message::local(l::ProviderToggled{i, on})); })});
            stretch(r);
            add(r, kw::reorder(i, n, [this](std::size_t from, std::size_t to) { emit message(Message::local(l::MoveProvider{from, to})); }));
            add(providers, r);
        }
    }
    add(root, section("Providers (order)", providers));

    // Model overrides.
    auto* overrides = vcol({}, spi(SpaceRole::M));
    for (std::size_t i = 0; i < d.overrides.size(); ++i) {
        const auto& o = d.overrides[i];
        auto* oh = hrow({kw::label(o.selector, TextRole::Mono, Tone::Normal)});
        stretch(oh);
        add(oh, action("Remove override", ButtonKind::danger(), send(l::RemoveOverride{i})));
        auto* form = kw::form_view(o, o.parsed().second, [this, i](std::string_view k, std::string v) { emit message(Message::local(l::OverrideEdit{i, std::string(k), std::move(v)})); }, nullptr);
        add(overrides, card(vcol({oh, form}, spi(SpaceRole::S)), std::format("override {}", o.selector)));
    }
    add(overrides, hrow({pick(s.known(), std::nullopt, "Override a model", [this](std::string sel) { emit message(Message::local(l::AddOverride{std::move(sel)})); }),
                         field("new-override", "or type provider/model", s.new_override(), [this](std::string v) { emit message(Message::local(l::NewOverride{std::move(v)})); },
                               send(l::AddOverride{std::nullopt})),
                         action("Add", ButtonKind::secondary(), send(l::AddOverride{std::nullopt}))}));
    add(root, section("Model overrides", overrides));

    // Custom providers.
    auto* custom = vcol({}, spi(SpaceRole::M));
    const auto provider_errors = d.provider_errors();
    static const std::vector<std::string> AUTH_MODES = [] {
        std::vector<std::string> out;
        for (auto a : loa::core::all<vm::AuthMode>()) out.emplace_back(loa::core::as_str(a));
        return out;
    }();
    for (std::size_t i = 0; i < d.custom.size(); ++i) {
        const auto& p = d.custom[i];
        auto* form = kw::form_view(p, provider_errors[i], [this, i](std::string_view k, std::string v) { emit message(Message::local(l::ProviderEdit{i, std::string(k), std::move(v)})); }, nullptr);
        auto* auth = labeled("Auth", pick(AUTH_MODES, loa::core::as_str(p.auth), "", [this, i](std::string a) {
                                 if (auto mode = loa::core::parse<vm::AuthMode>(a)) emit message(Message::local(l::ProviderAuth{i, *mode}));
                             }));
        add(custom, card(vcol({form, auth, action("Remove provider", ButtonKind::danger(), send(l::RemoveProvider{i}))}, spi(SpaceRole::S)), std::format("custom provider {}", i + 1)));
    }
    add(custom, action("Add custom provider", ButtonKind::secondary(), send(l::AddProvider{})));
    add(root, section("Custom providers", custom));

    // Other harnesses.
    QWidget* harnesses = nullptr;
    if (d.base.harnesses.empty()) harnesses = kw::label("No other harness settings found", TextRole::Label, Tone::Muted);
    else {
        harnesses = vcol({}, spi(SpaceRole::M));
        for (std::size_t i = 0; i < d.base.harnesses.size(); ++i) {
            const auto& h = d.base.harnesses[i];
            std::vector<std::string> efforts{std::string(settings::HARNESS_DEFAULT)};
            efforts.insert(efforts.end(), h.efforts.begin(), h.efforts.end());
            const std::string effort = h.effort.empty() ? std::string(settings::HARNESS_DEFAULT) : h.effort;
            auto* name = hrow({kw::label(h.name, TextRole::Body, Tone::Normal), kw::label(h.config_path, TextRole::Mono, Tone::Muted)});
            stretch(name);
            auto* model = labeled("Model", field(std::format("harness-{}-model", i), "harness default", h.model, [this, i](std::string m) { emit message(Message::local(l::HarnessModel{i, std::move(m)})); }));
            auto* eff = labeled("Effort", pick(efforts, effort, "", [this, i](std::string e) { emit message(Message::local(l::HarnessEffort{i, std::move(e)})); }));
            add(harnesses, card(vcol({name, model, eff}, spi(SpaceRole::S)), std::format("harness {}", h.name)));
        }
    }
    add(root, section("Other harnesses", harnesses));
    stretch(root);
    show_in(scroll_, root);
}

// ---- Sign in -------------------------------------------------------------------------------

SignInView::SignInView(QWidget* parent) : QWidget(parent) {
    setAccessibleName(QStringLiteral("sign in"));
    scroll_ = make_scroll(this, "sign in body");
}

void SignInView::render(const signin::State& state) {
    namespace l = signin::local;
    namespace ls = vm::login_state;
    namespace so = signin::sign_out;
    using signin::Message;
    auto send = [this](signin::Message m) { return [this, m] { emit message(m); }; };
    auto local = [&](signin::Local m) { return send(Message::local(std::move(m))); };
    auto note = [](std::string_view s, Tone tone) {
        auto* w = kw::label(s, TextRole::Label, tone);
        w->setWordWrap(true);
        return w;
    };

    auto* root = body_column(spi(SpaceRole::L));
    if (state.providers().empty()) add(root, kw::label("No providers", TextRole::Label, Tone::Muted));
    std::vector<std::string_view> harnesses;
    for (const auto& e : state.providers())
        if (harnesses.empty() || harnesses.back() != e.harness_name) harnesses.push_back(e.harness_name);

    for (const auto h : harnesses) {
        auto* rows = vcol({}, spi(SpaceRole::M));
        for (const auto& entry : state.providers()) {
            if (entry.harness_name != h) continue;
            const auto& key = entry.key;
            const auto& login = state.login_state(key);
            const auto* sign_out = state.sign_out(key);
            const bool sign_out_open = sign_out && !std::holds_alternative<so::Done>(*sign_out);

            bool on = false;
            std::string status;
            std::optional<std::string> detail;
            const auto* as_login = std::get_if<vm::auth::Login>(&entry.auth);
            if (as_login) {
                on = as_login->signed_in;
                status = as_login->account ? std::format("Signed in as {}", *as_login->account) : (on ? "Signed in" : "Not signed in");
            } else if (const auto* k = std::get_if<vm::auth::ApiKey>(&entry.auth)) {
                on = k->configured_via.has_value();
                status = k->configured_via ? std::format("Configured via {}", *k->configured_via) : "Not configured";
                detail = k->instructions;
            } else {
                status = "Unavailable";
                detail = std::get<vm::auth::Unavailable>(entry.auth).reason;
            }
            const bool idle = std::holds_alternative<ls::Idle>(login.value) || std::holds_alternative<ls::Cancelled>(login.value);
            std::optional<std::string_view> sign_in;
            if (as_login) {
                if (std::holds_alternative<ls::Failed>(login.value)) sign_in = "Retry";
                else if (std::holds_alternative<ls::Succeeded>(login.value) || (as_login->signed_in && idle)) sign_in = "Sign in again";
                else if (idle) sign_in = "Sign in";
            }
            // Both wait while the row's sign out is open; only a signed in row signs out.
            auto gated = [&](signin::Local m, bool enabled) -> std::function<void()> {
                return enabled && !sign_out_open ? std::function<void()>(local(std::move(m))) : nullptr;
            };
            auto* head = hrow({dot(on ? Tone::Ok : Tone::Faint), kw::label(entry.name, TextRole::Body, Tone::Normal),
                               kw::label(status, TextRole::Caption, on ? Tone::Ok : Tone::Muted)});
            if (sign_in) {
                add(head, action(*sign_in, ButtonKind::secondary(), gated(l::SignIn{key}, true)));
                add(head, action("Sign out", ButtonKind::danger(), gated(l::SignOut{key}, as_login && as_login->signed_in)));
            }
            stretch(head);
            auto* row = vcol({head});
            if (detail) add(row, kw::label(*detail, TextRole::Caption, Tone::Muted));

            // The login flow part of the row; nothing while idle.
            QWidget* flow = nullptr;
            std::visit(
                [&](const auto& st) {
                    using T = std::decay_t<decltype(st)>;
                    if constexpr (std::is_same_v<T, ls::Idle>) {
                        return;
                    } else if constexpr (std::is_same_v<T, ls::Starting>) {
                        flow = vcol({note("Waiting for the sign in URL\u2026", Tone::Muted), st.progress ? note(*st.progress, Tone::Muted) : nullptr,
                                     action("Cancel", ButtonKind::ghost(), local(l::Cancel{key}))});
                    } else if constexpr (std::is_same_v<T, ls::Browser>) {
                        namespace vi = vm::signin::intent;
                        auto* open = hrow({action("Copy", ButtonKind::ghost(), send(Message::event(vi::CopyUrl{st.copy_url}))),
                                           action("Open browser", ButtonKind::primary(), send(Message::event(vi::OpenUrl{st.url})))});
                        stretch(open);
                        flow = vcol({kw::mono_block(st.url), open, st.instructions ? note(*st.instructions, Tone::Normal) : nullptr,
                                     st.progress ? note(*st.progress, Tone::Muted) : nullptr,
                                     note("Waiting for sign in to finish in the browser\u2026", Tone::Muted)});
                        if (st.prompt) {
                            const auto& p = *st.prompt;
                            const bool can_submit = p.input.find_first_not_of(" \t\n\r\f\v") != std::string::npos;
                            std::function<void()> submit = can_submit ? std::function<void()>(local(l::Submit{key})) : nullptr;
                            auto* input = field(std::format("login-{}-{}", key.harness, key.provider), p.placeholder.value_or(""), p.input,
                                                [this, key](std::string v) { emit message(Message::local(l::Input{key, std::move(v)})); }, submit);
                            add(flow, vcol({note(p.message, Tone::Normal), hrow({input, action("Submit", ButtonKind::primary(), submit)})}));
                        }
                        add(flow, action("Cancel", ButtonKind::ghost(), local(l::Cancel{key})));
                    } else if constexpr (std::is_same_v<T, ls::Succeeded>) {
                        flow = vcol({note(st.account ? std::format("Signed in as {}", *st.account) : std::string("Signed in"), Tone::Ok),
                                     action("Dismiss", ButtonKind::ghost(), local(l::Dismiss{key}))});
                    } else if constexpr (std::is_same_v<T, ls::Failed>) {
                        flow = vcol({note(std::format("Sign in failed: {}", st.message), Tone::Danger), action("Dismiss", ButtonKind::ghost(), local(l::Dismiss{key}))});
                    } else {
                        flow = vcol({note("Sign in cancelled", Tone::Muted), action("Dismiss", ButtonKind::ghost(), local(l::Dismiss{key}))});
                    }
                },
                login.value);
            if (flow) {
                // A plain column's buttons would stretch; keep them at their natural width.
                for (int i = 0; i < flow->layout()->count(); ++i)
                    if (auto* w = qobject_cast<QPushButton*>(flow->layout()->itemAt(i)->widget())) flow->layout()->setAlignment(w, Qt::AlignLeft);
                add(row, flow);
            }

            // The sign out part: the stored sign ins to pick from, its progress or its outcome.
            if (sign_out) {
                auto cancel = [&] { return action("Cancel", ButtonKind::ghost(), local(l::CancelSignOut{key})); };
                QWidget* part = nullptr;
                std::visit(
                    [&](const auto& st) {
                        using T = std::decay_t<decltype(st)>;
                        if constexpr (std::is_same_v<T, so::Reading>) {
                            part = vcol({note("Reading the stored sign ins\u2026", Tone::Muted), cancel()});
                        } else if constexpr (std::is_same_v<T, so::Confirm>) {
                            part = vcol({note("Remove which sign in?", Tone::Normal)});
                            for (const auto& c : st.credentials) {
                                auto* about = vcol({kw::label(c.label, TextRole::Body, Tone::Normal), kw::label(c.detail, TextRole::Caption, Tone::Muted)}, 0);
                                auto* r = hrow({about});
                                static_cast<QBoxLayout*>(r->layout())->setStretch(0, 1);
                                if (c.active) add(r, kw::label("In use", TextRole::Caption, Tone::Ok));
                                add(r, action("Remove", ButtonKind::danger(), local(l::Remove{key, c.id})));
                                add(part, r);
                            }
                            add(part, cancel());
                        } else if constexpr (std::is_same_v<T, so::Removing>) {
                            part = vcol({note("Signing out\u2026", Tone::Muted)});
                        } else {
                            part = vcol({note(st.message, st.tone), action("Dismiss", ButtonKind::ghost(), local(l::Dismiss{key}))});
                        }
                    },
                    *sign_out);
                add(row, part);
            }
            add(rows, row);
        }
        add(root, section(h, rows));
    }
    stretch(root);
    show_in(scroll_, root);
}

} // namespace loa::ui::settings::models
