#pragma once

// Virtual lists: item heights with logarithmic prefix sums, so a list finds the items under a
// scroll offset and its spacer sizes without walking every item; plus the scroll math and a
// Qt scroll area that paints only the visible window.

#include <cstddef>
#include <functional>
#include <optional>
#include <span>
#include <vector>

#include <QAbstractScrollArea>

class QPainter;
class QRect;

namespace loa::ui::kit::widgets {

// A half-open index range.
struct Range {
    std::size_t begin = 0, end = 0;
    friend bool operator==(const Range&, const Range&) = default;
};

// Heights of every item, in order, backed by a Fenwick tree. Sums are `double` so a long list
// (millions of pixels) keeps sub-pixel precision.
class Heights {
  public:
    [[nodiscard]] std::size_t size() const { return values_.size(); }
    [[nodiscard]] bool empty() const { return values_.empty(); }
    [[nodiscard]] std::optional<float> get(std::size_t index) const;

    // Sum of the first `count` heights.
    [[nodiscard]] double prefix(std::size_t count) const;
    [[nodiscard]] double total() const { return prefix(values_.size()); }

    void push(float height);
    // Keeps the first `len` heights.
    void truncate(std::size_t len);
    // Replaces the height at `index`; out of range is ignored.
    void set(std::size_t index, float height);
    // Replaces `[begin, end)` with `values`: a tail splice costs only the new items, a middle one a rebuild.
    void splice(Range range, std::span<const float> values);

    // Index of the item containing position `y`, clamped to the last; `nullopt` when empty.
    [[nodiscard]] std::optional<std::size_t> find(double y) const;
    // The items overlapping `offset..offset + height`, widened by `overscan` on both sides.
    [[nodiscard]] Range window(float offset, float height, float overscan) const;

  private:
    std::vector<float> values_;
    std::vector<double> tree_; // 1-based Fenwick: tree_[i - 1] covers items (i - lowbit(i), i]
};

// The scroll offset that brings `top..bottom` into a viewport of `height` scrolled to `offset`,
// or `nullopt` when it is already visible.
[[nodiscard]] std::optional<float> scroll_target(float offset, float height, float top, float bottom);

// A vertical list that paints only the rows in view. Rows are drawn by `paint_row` and sized by
// `heights()`; clicks report the row index.
class VirtualList : public QAbstractScrollArea {
  public:
    using PaintRow = std::function<void(QPainter&, std::size_t index, const QRect& rect)>;
    using RowClicked = std::function<void(std::size_t index)>;

    explicit VirtualList(QWidget* parent = nullptr);

    [[nodiscard]] Heights& heights() { return heights_; }
    [[nodiscard]] const Heights& heights() const { return heights_; }
    void set_paint_row(PaintRow f) { paint_row_ = std::move(f); }
    void set_row_clicked(RowClicked f) { row_clicked_ = std::move(f); }
    void set_overscan(float px) { overscan_ = px; }

    // Call after changing `heights()`.
    void refresh();
    // Scrolls just enough to show row `index`.
    void scroll_to(std::size_t index);
    // The rows painted at the current scroll position.
    [[nodiscard]] Range visible() const;

  protected:
    void paintEvent(QPaintEvent* e) override;
    void resizeEvent(QResizeEvent* e) override;
    void mousePressEvent(QMouseEvent* e) override;

  private:
    Heights heights_;
    PaintRow paint_row_;
    RowClicked row_clicked_;
    float overscan_ = 0;
};

} // namespace loa::ui::kit::widgets
