#include "loa/ui/kit/widgets.hpp"

#include <algorithm>
#include <cmath>

#include <QHBoxLayout>
#include <QLabel>
#include <QLineEdit>
#include <QPainter>
#include <QPushButton>
#include <QVBoxLayout>

#include "loa/ui/kit/qt.hpp"

namespace loa::ui::kit::widgets {
namespace {

using theme::SpaceRole;
using theme::TextRole;

int spi(SpaceRole r) { return static_cast<int>(std::lround(theme::sp(r))); }

QString qs(std::string_view s) { return QString::fromUtf8(s.data(), static_cast<qsizetype>(s.size())); }

void apply_label_style(QLabel& l, TextRole r, Tone tone) {
    l.setFont(qt::font(r));
    QPalette p = l.palette();
    p.setColor(QPalette::WindowText, qt::qcolor(kit::color(tone)));
    l.setPalette(p);
}

QLabel* icon_label(Icon i, TextRole size, Tone tone, QWidget* parent = nullptr) {
    auto* l = new QLabel(parent);
    l->setPixmap(tinted(i, size, tone));
    l->setAccessibleName(qs(icon_file(i)));
    return l;
}

void connect_click(QPushButton* b, Callback on) {
    b->setEnabled(static_cast<bool>(on));
    if (on) QObject::connect(b, &QPushButton::clicked, b, [on = std::move(on)] { on(); });
}

// A row button with arbitrary content (`row_button` in Rust).
QPushButton* row_button(QLayout* content, bool selected, Callback on, QWidget* parent) {
    auto* b = new QPushButton(parent);
    b->setLayout(content);
    content->setContentsMargins(spi(SpaceRole::S), spi(SpaceRole::Xs), spi(SpaceRole::S), spi(SpaceRole::Xs));
    b->setSizePolicy(QSizePolicy::Expanding, QSizePolicy::Fixed);
    b->setMinimumHeight(content->sizeHint().height());
    qt::style_button(*b, ButtonKind::row(selected));
    connect_click(b, std::move(on));
    return b;
}

class Meter final : public QWidget {
  public:
    Meter(float fraction, QWidget* parent) : QWidget(parent), fraction_(std::clamp(fraction, 0.0F, 1.0F)) {
        setAccessibleName(QStringLiteral("meter"));
        setFixedHeight(static_cast<int>(std::lround(theme::sp(SpaceRole::Xs) + 2.0F)));
        setSizePolicy(QSizePolicy::Expanding, QSizePolicy::Fixed);
    }

  protected:
    void paintEvent(QPaintEvent*) override {
        const auto t = theme::tokens();
        const qreal r = std::min<qreal>(t->radius(theme::RadiusRole::Dot), height() / 2.0);
        QPainter p(this);
        p.setRenderHint(QPainter::Antialiasing);
        p.setPen(Qt::NoPen);
        p.setBrush(qt::qcolor(t->color(theme::ColorRole::LineStrong)));
        p.drawRoundedRect(rect(), r, r);
        p.setBrush(qt::qcolor(t->color(meter_role(fraction_))));
        p.drawRoundedRect(QRectF(0, 0, std::max<qreal>(1.0, width() * fraction_), height()), r, r);
    }

  private:
    float fraction_;
};

} // namespace

QLabel* label(std::string_view s, TextRole r, Tone tone, QWidget* parent) {
    auto* l = new QLabel(qs(s), parent);
    apply_label_style(*l, r, tone);
    return l;
}

QWidget* card(QWidget* content, QWidget* parent) {
    auto* c = new QWidget(parent);
    c->setAccessibleName(QStringLiteral("card"));
    auto* layout = new QVBoxLayout(c);
    const int m = spi(SpaceRole::M);
    layout->setContentsMargins(m, m, m, m);
    layout->addWidget(content);
    qt::style_container(*c, Surface::Card);
    return c;
}

QWidget* badge(std::string_view s, Tone tone, QWidget* parent) {
    auto* c = new QWidget(parent);
    c->setAccessibleName(qs(s));
    auto* layout = new QHBoxLayout(c);
    layout->setContentsMargins(spi(SpaceRole::Xs), 0, spi(SpaceRole::Xs), 0);
    layout->addWidget(label(s, TextRole::Caption, tone));
    qt::style_container(*c, Surface::Badge);
    return c;
}

QPushButton* text_button(std::string_view s, ButtonKind kind, Callback on, QWidget* parent) {
    auto* b = new QPushButton(qs(s), parent);
    b->setFont(qt::font(TextRole::Label));
    qt::style_button(*b, kind);
    connect_click(b, std::move(on));
    return b;
}

QPushButton* icon_button(Icon icon, std::string_view tip, Callback on, QWidget* parent) {
    auto* b = new QPushButton(parent);
    b->setIcon(qicon(icon, TextRole::Body, Tone::Muted));
    const int s = static_cast<int>(std::lround(theme::text_size(TextRole::Body)));
    b->setIconSize(QSize(s, s));
    b->setToolTip(qs(tip));
    b->setAccessibleName(qs(tip));
    qt::style_button(*b, ButtonKind::ghost());
    connect_click(b, std::move(on));
    return b;
}

QPushButton* menu_row(std::optional<Icon> icon, std::string_view s, bool selected, Callback on, QWidget* parent) {
    auto* r = new QHBoxLayout;
    r->setSpacing(spi(SpaceRole::S));
    if (icon) r->addWidget(icon_label(*icon, TextRole::Body, Tone::Muted));
    r->addWidget(label(s, TextRole::Body, Tone::Normal), 1);
    auto* b = row_button(r, selected, std::move(on), parent);
    b->setAccessibleName(qs(s));
    return b;
}

QPushButton* two_line_row(std::string_view title, std::string_view detail, bool selected, Callback on, QWidget* parent) {
    auto* c = new QVBoxLayout;
    c->setSpacing(0);
    c->addWidget(label(title, TextRole::Body, Tone::Normal));
    c->addWidget(label(detail, TextRole::Caption, Tone::Faint));
    auto* b = row_button(c, selected, std::move(on), parent);
    b->setAccessibleName(qs(title));
    return b;
}

QWidget* mono_block(std::string_view s, QWidget* parent) {
    auto* c = new QWidget(parent);
    c->setAccessibleName(QStringLiteral("mono block"));
    auto* layout = new QVBoxLayout(c);
    const int m = spi(SpaceRole::S);
    layout->setContentsMargins(m, m, m, m);
    auto* l = label(s, TextRole::Mono, Tone::Normal);
    l->setTextInteractionFlags(Qt::TextSelectableByMouse);
    layout->addWidget(l);
    qt::style_container(*c, Surface::Well);
    return c;
}

theme::ColorRole meter_role(float fraction) {
    const float f = std::clamp(fraction, 0.0F, 1.0F);
    if (f >= 0.8F) return theme::ColorRole::Danger;
    if (f >= 0.5F) return theme::ColorRole::Warn;
    return theme::ColorRole::Accent;
}

QWidget* meter(float fraction, QWidget* parent) { return new Meter(fraction, parent); }

QWidget* section_header(std::string_view title, const std::vector<QWidget*>& actions, QWidget* parent) {
    auto* w = new QWidget(parent);
    w->setAccessibleName(qs(title));
    auto* r = new QHBoxLayout(w);
    r->setContentsMargins(0, 0, 0, 0);
    r->setSpacing(spi(SpaceRole::S));
    r->addWidget(label(title, TextRole::Heading, Tone::Normal));
    r->addStretch(1);
    for (auto* a : actions) r->addWidget(a);
    return w;
}

QWidget* status_row(Icon icon, std::string_view name, std::string_view status, Tone tone, std::optional<std::string_view> detail,
                    QWidget* parent) {
    auto* w = new QWidget(parent);
    w->setAccessibleName(qs(name));
    auto* r = new QHBoxLayout(w);
    r->setContentsMargins(0, 0, 0, 0);
    r->setSpacing(spi(SpaceRole::S));
    r->addWidget(icon_label(icon, TextRole::Body, Tone::Muted));
    r->addWidget(label(name, TextRole::Body, Tone::Normal));
    r->addStretch(1);
    if (detail) r->addWidget(label(*detail, TextRole::Caption, Tone::Faint));
    r->addWidget(label(status, TextRole::Label, tone));
    return w;
}

QWidget* empty_state(Icon icon, std::string_view title, std::string_view hint, QWidget* parent) {
    auto* w = new QWidget(parent);
    w->setAccessibleName(qs(title));
    auto* c = new QVBoxLayout(w);
    c->setSpacing(spi(SpaceRole::S));
    c->addStretch(1);
    c->addWidget(icon_label(icon, TextRole::Title, Tone::Faint), 0, Qt::AlignHCenter);
    c->addWidget(label(title, TextRole::Heading, Tone::Normal), 0, Qt::AlignHCenter);
    c->addWidget(label(hint, TextRole::Label, Tone::Muted), 0, Qt::AlignHCenter);
    c->addStretch(1);
    return w;
}

QWidget* collapsible(std::string_view title, bool open, Callback toggle, QWidget* body, QWidget* parent) {
    auto* w = new QWidget(parent);
    w->setAccessibleName(qs(title));
    auto* c = new QVBoxLayout(w);
    c->setContentsMargins(0, 0, 0, 0);
    c->setSpacing(spi(SpaceRole::Xs));
    auto* r = new QHBoxLayout;
    r->setContentsMargins(0, 0, 0, 0);
    r->setSpacing(spi(SpaceRole::Xs));
    r->addWidget(icon_label(open ? Icon::ChevronDown : Icon::ChevronRight, TextRole::Caption, Tone::Muted));
    r->addWidget(label(title, TextRole::Label, Tone::Muted), 1);
    auto* head = new QPushButton;
    head->setLayout(r);
    head->setMinimumHeight(r->sizeHint().height());
    head->setAccessibleName(qs(title));
    qt::style_button(*head, ButtonKind::flat());
    connect_click(head, std::move(toggle));
    c->addWidget(head);
    if (body) {
        c->addWidget(body);
        body->setVisible(open);
    }
    return w;
}

QWidget* tab_strip(const std::vector<Tab>& tabs, QWidget* parent) {
    auto* w = new QWidget(parent);
    w->setAccessibleName(QStringLiteral("tabs"));
    auto* r = new QHBoxLayout(w);
    r->setContentsMargins(0, 0, 0, 0);
    r->setSpacing(spi(SpaceRole::Xs));
    for (const auto& t : tabs) {
        auto* pill = new QPushButton(qs(t.label));
        pill->setFont(qt::font(TextRole::Label));
        if (t.icon) {
            pill->setIcon(qicon(*t.icon, TextRole::Label, t.active ? Tone::Normal : Tone::Muted));
            const int s = static_cast<int>(std::lround(theme::text_size(TextRole::Label)));
            pill->setIconSize(QSize(s, s));
        }
        qt::style_button(*pill, ButtonKind::tab(t.active));
        connect_click(pill, t.on);
        r->addWidget(pill);
        if (t.trailing) r->addWidget(t.trailing);
    }
    r->addStretch(1);
    return w;
}

float splitter_width() { return theme::sp(SpaceRole::Xs) + 2.0F; }

// ---- form.hpp ------------------------------------------------------------------------------

QWidget* form_view(const Form& form, const std::map<std::string, std::string, std::less<>>& errors,
                   std::function<void(std::string_view, std::string)> on_edit, std::function<void()> on_submit, QWidget* parent) {
    const auto t = theme::tokens();
    auto* w = new QWidget(parent);
    w->setAccessibleName(QStringLiteral("form"));
    auto* col = new QVBoxLayout(w);
    col->setContentsMargins(0, 0, 0, 0);
    col->setSpacing(spi(SpaceRole::S));
    auto edit = std::make_shared<decltype(on_edit)>(std::move(on_edit));
    auto submit = std::make_shared<std::function<void()>>(std::move(on_submit));
    for (const auto& f : form.fields()) {
        auto* row = new QHBoxLayout;
        row->setSpacing(spi(SpaceRole::M));
        auto* name = label(f.label, TextRole::Label, Tone::Muted);
        name->setFixedWidth(static_cast<int>(std::lround(t->layout_size(theme::LayoutRole::LabelColumn))));
        row->addWidget(name);
        auto* field = new QVBoxLayout;
        field->setSpacing(0);
        auto* input = new QLineEdit(QString::fromStdString(form.get(f.key)));
        input->setPlaceholderText(qs(f.placeholder));
        input->setAccessibleName(qs(f.label));
        const std::string_view key = f.key;
        QObject::connect(input, &QLineEdit::textEdited, input, [edit, key](const QString& v) {
            if (*edit) (*edit)(key, v.toStdString());
        });
        QObject::connect(input, &QLineEdit::returnPressed, input, [submit] {
            if (*submit) (*submit)();
        });
        field->addWidget(input);
        if (const auto e = errors.find(f.key); e != errors.end()) field->addWidget(label(e->second, TextRole::Caption, Tone::Danger));
        row->addLayout(field, 1);
        col->addLayout(row);
    }
    return w;
}

QWidget* reorder(std::size_t index, std::size_t len, const std::function<void(std::size_t, std::size_t)>& on_move, QWidget* parent) {
    auto* w = new QWidget(parent);
    auto* r = new QHBoxLayout(w);
    r->setContentsMargins(0, 0, 0, 0);
    auto arrow = [&](Icon icon, std::string_view tip, std::optional<std::size_t> to) {
        Callback cb;
        if (to && on_move) cb = [on_move, index, to = *to] { on_move(index, to); };
        r->addWidget(icon_button(icon, tip, std::move(cb)));
    };
    arrow(Icon::ChevronUp, "Move up", index > 0 ? std::optional(index - 1) : std::nullopt);
    arrow(Icon::ChevronDown, "Move down", index + 1 < len ? std::optional(index + 1) : std::nullopt);
    return w;
}

QWidget* list_editor(std::span<const std::string> items, std::function<void(std::size_t)> on_remove,
                     std::function<void(std::size_t, std::size_t)> on_move, std::string_view draft,
                     std::function<void(std::string)> on_draft, std::function<void()> on_add, QWidget* parent) {
    auto* w = new QWidget(parent);
    w->setAccessibleName(QStringLiteral("list editor"));
    auto* col = new QVBoxLayout(w);
    col->setContentsMargins(0, 0, 0, 0);
    col->setSpacing(spi(SpaceRole::Xs));
    for (std::size_t i = 0; i < items.size(); ++i) {
        auto* row = new QHBoxLayout;
        row->addWidget(label(items[i], TextRole::Body, Tone::Normal), 1);
        if (on_move) row->addWidget(reorder(i, items.size(), on_move));
        Callback remove;
        if (on_remove) remove = [on_remove, i] { on_remove(i); };
        row->addWidget(icon_button(Icon::X, "Remove", std::move(remove)));
        col->addLayout(row);
    }
    auto* add_row = new QHBoxLayout;
    add_row->setSpacing(spi(SpaceRole::S));
    auto* input = new QLineEdit(qs(draft));
    input->setAccessibleName(QStringLiteral("new item"));
    auto add = std::make_shared<std::function<void()>>(std::move(on_add));
    if (on_draft) QObject::connect(input, &QLineEdit::textEdited, input, [on_draft = std::move(on_draft)](const QString& v) { on_draft(v.toStdString()); });
    QObject::connect(input, &QLineEdit::returnPressed, input, [add] {
        if (*add) (*add)();
    });
    add_row->addWidget(input, 1);
    auto* add_button = new QPushButton;
    add_button->setIcon(qicon(Icon::Plus, TextRole::Body, Tone::Normal));
    add_button->setAccessibleName(QStringLiteral("Add"));
    qt::style_button(*add_button, ButtonKind::secondary());
    connect_click(add_button, [add] {
        if (*add) (*add)();
    });
    add_row->addWidget(add_button);
    col->addLayout(add_row);
    return w;
}

QWidget* picker_view(std::string_view current, bool open, const std::vector<PickerRow>& rows, std::function<void(PickerMsg)> on,
                     QWidget* parent) {
    const auto t = theme::tokens();
    auto shared = std::make_shared<std::function<void(PickerMsg)>>(std::move(on));
    auto send = [shared](PickerMsg m) -> Callback {
        if (!*shared) return {};
        return [shared, m] { (*shared)(m); };
    };
    auto* hr = new QHBoxLayout;
    hr->setSpacing(spi(SpaceRole::Xs));
    hr->addWidget(label(current, TextRole::Label, Tone::Normal));
    hr->addWidget(icon_label(Icon::ChevronDown, TextRole::Caption, Tone::Muted));
    auto* head = new QPushButton;
    head->setLayout(hr);
    hr->setContentsMargins(spi(SpaceRole::S), spi(SpaceRole::Xs), spi(SpaceRole::S), spi(SpaceRole::Xs));
    head->setMinimumSize(hr->sizeHint());
    head->setAccessibleName(qs(current));
    qt::style_button(*head, ButtonKind::ghost());
    connect_click(head, send(PickerMsg::toggle()));

    QWidget* menu = nullptr;
    if (open) {
        menu = new QWidget;
        menu->setAccessibleName(QStringLiteral("picker menu"));
        auto* col = new QVBoxLayout(menu);
        const int pad = spi(SpaceRole::Xs);
        col->setContentsMargins(pad, pad, pad, pad);
        col->setSpacing(static_cast<int>(std::lround(theme::sp(SpaceRole::Xs) / 2.0F)));
        for (std::size_t i = 0; i < rows.size(); ++i) col->addWidget(menu_row(rows[i].icon, rows[i].text, rows[i].selected, send(PickerMsg::pick(i))));
        menu->setFixedWidth(static_cast<int>(std::lround(t->layout_size(theme::LayoutRole::PopupWidth) / 2.0F)));
        qt::style_container(*menu, Surface::Popup);
    }
    auto* p = new Popover(head, menu, parent);
    p->own_width();
    return p;
}

} // namespace loa::ui::kit::widgets
