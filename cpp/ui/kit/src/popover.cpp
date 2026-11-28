#include "loa/ui/kit/widgets/popover.hpp"

#include <algorithm>
#include <cmath>

#include <QApplication>
#include <QMouseEvent>
#include <QVBoxLayout>

namespace loa::ui::kit::widgets {

Placement popover_limits(Rect anchor, float bounds_width, float bounds_height, float gap, bool fit_anchor) {
    const float above = std::max(anchor.y - gap, 0.0F);
    const float below = std::max(bounds_height - anchor.y - anchor.height - gap, 0.0F);
    return {fit_anchor ? anchor.width : bounds_width, std::max(above, below)};
}

Rect popover_place(Rect anchor, float bounds_width, float bounds_height, float gap, bool fit_anchor, float width, float height) {
    const float above = std::max(anchor.y - gap, 0.0F);
    const float below = std::max(bounds_height - anchor.y - anchor.height - gap, 0.0F);
    const float y = height <= above || above >= below ? anchor.y - gap - height : anchor.y + anchor.height + gap;
    const float x = fit_anchor ? anchor.x : std::max(std::min(anchor.x, bounds_width - width), 0.0F);
    return {x, std::max(y, 0.0F), width, height};
}

Popover::Popover(QWidget* anchor, QWidget* popup, QWidget* parent) : QWidget(parent), anchor_(anchor), popup_(popup) {
    setAccessibleName(QStringLiteral("popover"));
    auto* layout = new QVBoxLayout(this);
    layout->setContentsMargins(0, 0, 0, 0);
    layout->addWidget(anchor_);
    if (popup_) {
        popup_->setParent(this);
        popup_->hide();
        qApp->installEventFilter(this);
    }
}

Popover::~Popover() {
    if (popup_ && popup_->parent() != this) delete popup_;
}

Popover& Popover::own_width() {
    fit_anchor_ = false;
    reposition();
    return *this;
}

Popover& Popover::on_dismiss(std::function<void()> f) {
    on_dismiss_ = std::move(f);
    return *this;
}

void Popover::reposition() {
    if (!popup_ || !isVisible()) return;
    QWidget* win = window();
    if (popup_->parentWidget() != win) {
        popup_->setParent(win);
        win->installEventFilter(this);
    }
    const QPoint at = anchor_->mapTo(win, QPoint(0, 0));
    const Rect a{static_cast<float>(at.x()), static_cast<float>(at.y()), static_cast<float>(anchor_->width()),
                 static_cast<float>(anchor_->height())};
    const auto bw = static_cast<float>(win->width()), bh = static_cast<float>(win->height());
    const auto lim = popover_limits(a, bw, bh, gap_, fit_anchor_);
    popup_->setMaximumSize(static_cast<int>(lim.max_width), static_cast<int>(lim.max_height));
    const QSize hint = popup_->sizeHint().boundedTo(popup_->maximumSize());
    const float w = fit_anchor_ ? lim.max_width : static_cast<float>(hint.width());
    const auto r = popover_place(a, bw, bh, gap_, fit_anchor_, w, static_cast<float>(hint.height()));
    popup_->setGeometry(static_cast<int>(std::lround(r.x)), static_cast<int>(std::lround(r.y)), static_cast<int>(std::lround(r.width)),
                        static_cast<int>(std::lround(r.height)));
    popup_->show();
    popup_->raise();
}

bool Popover::eventFilter(QObject* watched, QEvent* event) {
    if (popup_ && watched == window() && (event->type() == QEvent::Resize || event->type() == QEvent::Move)) reposition();
    if (on_dismiss_ && popup_ && popup_->isVisible() && event->type() == QEvent::MouseButtonPress) {
        const auto* me = static_cast<QMouseEvent*>(event);
        const QPoint g = me->globalPosition().toPoint();
        const bool in_popup = popup_->rect().contains(popup_->mapFromGlobal(g));
        const bool in_anchor = anchor_->rect().contains(anchor_->mapFromGlobal(g));
        if (!in_popup && !in_anchor) on_dismiss_();
    }
    return QWidget::eventFilter(watched, event);
}

void Popover::showEvent(QShowEvent* e) {
    QWidget::showEvent(e);
    reposition();
}

void Popover::moveEvent(QMoveEvent* e) {
    QWidget::moveEvent(e);
    reposition();
}

void Popover::resizeEvent(QResizeEvent* e) {
    QWidget::resizeEvent(e);
    reposition();
}

} // namespace loa::ui::kit::widgets
