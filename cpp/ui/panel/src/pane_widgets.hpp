#pragma once

// Small Qt building blocks the agents and tools views share (iced `row!`/`column!`, the crate's
// `action` and `ghost` buttons). Internal to `loa_ui_panel`.

#include <cmath>
#include <functional>
#include <initializer_list>
#include <optional>
#include <string_view>

#include <QBoxLayout>
#include <QLabel>
#include <QPushButton>
#include <QScrollArea>
#include <QWidget>

#include "loa/ui/kit/icons.hpp"
#include "loa/ui/kit/qt.hpp"
#include "loa/ui/kit/style.hpp"
#include "loa/ui/kit/theme.hpp"
#include "loa/ui/kit/widgets.hpp"

namespace loa::ui::panel::pw {

namespace theme = loa::ui::kit::theme;
namespace kw = loa::ui::kit::widgets;
using loa::ui::kit::ButtonKind;
using loa::ui::kit::Surface;
using loa::ui::kit::Tone;
using loa::view::theme::Icon;
using theme::SpaceRole;
using theme::TextRole;

inline QString qs(std::string_view s) { return QString::fromUtf8(s.data(), static_cast<qsizetype>(s.size())); }
inline int px(float f) { return static_cast<int>(std::lround(f)); }
inline int spi(SpaceRole r) { return px(theme::sp(r)); }

// A box of `items` (null entries skipped) with `spacing`.
inline QWidget* box(QBoxLayout::Direction dir, std::initializer_list<QWidget*> items, int spacing = 0, QWidget* into = nullptr) {
    auto* w = into ? into : new QWidget;
    auto* l = new QBoxLayout(dir, w);
    l->setContentsMargins(0, 0, 0, 0);
    l->setSpacing(spacing);
    for (auto* i : items)
        if (i) l->addWidget(i);
    return w;
}
inline QWidget* hrow(std::initializer_list<QWidget*> items, int spacing = 0) { return box(QBoxLayout::LeftToRight, items, spacing); }
inline QWidget* vcol(std::initializer_list<QWidget*> items, int spacing = 0) { return box(QBoxLayout::TopToBottom, items, spacing); }
inline void add(QWidget* parent, QWidget* child, int stretch = 0) {
    if (child) static_cast<QBoxLayout*>(parent->layout())->addWidget(child, stretch);
}
inline void stretch(QWidget* parent) { static_cast<QBoxLayout*>(parent->layout())->addStretch(1); }

// A ghost caption button with an optional icon; disabled without `on`.
inline QPushButton* ghost(std::string_view label, std::optional<Icon> icon, std::function<void()> on) {
    auto* b = kw::text_button(label, ButtonKind::ghost(), std::move(on));
    if (icon) b->setIcon(kit::qicon(*icon, TextRole::Caption, Tone::Normal));
    b->setAccessibleName(qs(label));
    return b;
}
inline QPushButton* action(std::string_view label, std::function<void()> on) { return ghost(label, std::nullopt, std::move(on)); }

// A button holding `content`, named `name` for accessibility; disabled without `on`.
inline QPushButton* content_button(QWidget* content, std::string_view name, ButtonKind kind, std::function<void()> on) {
    auto* b = new QPushButton;
    b->setAccessibleName(qs(name));
    b->setFlat(true);
    auto* l = new QHBoxLayout(b);
    l->setContentsMargins(spi(SpaceRole::S), spi(SpaceRole::Xs), spi(SpaceRole::S), spi(SpaceRole::Xs));
    l->addWidget(content);
    b->setMinimumHeight(content->sizeHint().height() + 2 * spi(SpaceRole::Xs));
    kit::qt::style_button(*b, kind);
    if (on) QObject::connect(b, &QPushButton::clicked, b, std::move(on));
    else b->setEnabled(false);
    return b;
}

// `w` in a container of surface `s` with padding `pad`.
inline QWidget* surface(QWidget* w, Surface s, int pad) {
    auto* c = new QWidget;
    c->setAttribute(Qt::WA_StyledBackground, true);
    auto* l = new QVBoxLayout(c);
    l->setContentsMargins(pad, pad, pad, pad);
    l->addWidget(w);
    kit::qt::style_container(*c, s);
    return c;
}

// A scroll area showing `content`.
inline QScrollArea* scroll(QWidget* content, std::string_view name) {
    auto* s = new QScrollArea;
    s->setWidgetResizable(true);
    s->setFrameShape(QFrame::NoFrame);
    s->setAccessibleName(qs(name));
    s->setWidget(content);
    return s;
}

// Replaces whatever `owner` showed with `content` (deferred deletion: a render can run inside Qt's own walk).
inline void replace(QWidget* owner, QWidget* content) {
    auto* l = owner->layout();
    if (!l) {
        l = new QVBoxLayout(owner);
        l->setContentsMargins(0, 0, 0, 0);
    }
    while (auto* item = l->takeAt(0)) {
        if (auto* w = item->widget()) {
            w->hide();
            w->deleteLater();
        }
        delete item;
    }
    l->addWidget(content);
}

} // namespace loa::ui::panel::pw
