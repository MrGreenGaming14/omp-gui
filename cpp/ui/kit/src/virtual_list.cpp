#include "loa/ui/kit/widgets/virtual_list.hpp"

#include <algorithm>
#include <bit>
#include <cmath>

#include <QMouseEvent>
#include <QPainter>
#include <QScrollBar>

namespace loa::ui::kit::widgets {
namespace {

std::size_t lowbit(std::size_t i) { return i & (~i + 1); }

} // namespace

std::optional<float> Heights::get(std::size_t index) const {
    if (index >= values_.size()) return std::nullopt;
    return values_[index];
}

double Heights::prefix(std::size_t count) const {
    double sum = 0;
    for (auto i = std::min(count, values_.size()); i > 0; i -= lowbit(i)) sum += tree_[i - 1];
    return sum;
}

void Heights::push(float height) {
    const auto i = values_.size() + 1;
    // Node `i` covers `(i - lowbit(i), i]`: the new value plus the values before it in that span.
    const double covered = prefix(i - 1) - prefix(i - lowbit(i));
    values_.push_back(height);
    tree_.push_back(covered + static_cast<double>(height));
}

void Heights::truncate(std::size_t len) {
    if (len >= values_.size()) return;
    values_.resize(len);
    tree_.resize(len);
}

void Heights::set(std::size_t index, float height) {
    if (index >= values_.size()) return;
    const double delta = static_cast<double>(height) - static_cast<double>(values_[index]);
    values_[index] = height;
    for (auto i = index + 1; i <= tree_.size(); i += lowbit(i)) tree_[i - 1] += delta;
}

void Heights::splice(Range range, std::span<const float> values) {
    if (range.end >= values_.size()) {
        truncate(range.begin);
        for (const float h : values) push(h);
        return;
    }
    values_.erase(values_.begin() + static_cast<std::ptrdiff_t>(range.begin), values_.begin() + static_cast<std::ptrdiff_t>(range.end));
    values_.insert(values_.begin() + static_cast<std::ptrdiff_t>(range.begin), values.begin(), values.end());
    const auto n = values_.size();
    tree_.assign(values_.begin(), values_.end());
    for (std::size_t i = 1; i <= n; ++i)
        if (i + lowbit(i) <= n) tree_[i + lowbit(i) - 1] += tree_[i - 1];
}

std::optional<std::size_t> Heights::find(double y) const {
    const auto n = values_.size();
    if (n == 0) return std::nullopt;
    std::size_t pos = 0;
    double rest = y;
    for (auto step = std::bit_ceil(n); step > 0; step >>= 1) {
        if (pos + step <= n && tree_[pos + step - 1] <= rest) {
            pos += step;
            rest -= tree_[pos - 1];
        }
    }
    return std::min(pos, n - 1);
}

Range Heights::window(float offset, float height, float overscan) const {
    const auto first = find(std::max(static_cast<double>(offset - overscan), 0.0));
    if (!first) return {};
    return {*first, find(static_cast<double>(offset + height + overscan)).value_or(*first) + 1};
}

std::optional<float> scroll_target(float offset, float height, float top, float bottom) {
    if (top < offset) return top;
    if (bottom > offset + height) return bottom - height;
    return std::nullopt;
}

VirtualList::VirtualList(QWidget* parent) : QAbstractScrollArea(parent) {
    setAccessibleName(QStringLiteral("virtual list"));
    setFrameShape(QFrame::NoFrame);
    viewport()->setAutoFillBackground(false);
}

void VirtualList::refresh() {
    const auto total = static_cast<int>(std::ceil(heights_.total()));
    verticalScrollBar()->setRange(0, std::max(0, total - viewport()->height()));
    verticalScrollBar()->setPageStep(viewport()->height());
    viewport()->update();
}

void VirtualList::scroll_to(std::size_t index) {
    if (index >= heights_.size()) return;
    const auto top = static_cast<float>(heights_.prefix(index));
    const auto bottom = static_cast<float>(heights_.prefix(index + 1));
    if (auto t = scroll_target(static_cast<float>(verticalScrollBar()->value()), static_cast<float>(viewport()->height()), top, bottom))
        verticalScrollBar()->setValue(static_cast<int>(std::lround(*t)));
}

Range VirtualList::visible() const {
    return heights_.window(static_cast<float>(verticalScrollBar()->value()), static_cast<float>(viewport()->height()), overscan_);
}

void VirtualList::paintEvent(QPaintEvent*) {
    if (!paint_row_) return;
    QPainter p(viewport());
    const auto offset = static_cast<double>(verticalScrollBar()->value());
    const auto [begin, end] = visible();
    for (auto i = begin; i < end; ++i) {
        const auto top = heights_.prefix(i) - offset;
        const QRect rect(0, static_cast<int>(std::lround(top)), viewport()->width(), static_cast<int>(std::lround(*heights_.get(i))));
        p.save();
        paint_row_(p, i, rect);
        p.restore();
    }
}

void VirtualList::resizeEvent(QResizeEvent* e) {
    QAbstractScrollArea::resizeEvent(e);
    refresh();
}

void VirtualList::mousePressEvent(QMouseEvent* e) {
    if (!row_clicked_ || heights_.empty()) return;
    const double y = e->position().y() + verticalScrollBar()->value();
    if (y < 0 || y >= heights_.total()) return;
    if (auto i = heights_.find(y)) row_clicked_(*i);
}

} // namespace loa::ui::kit::widgets
