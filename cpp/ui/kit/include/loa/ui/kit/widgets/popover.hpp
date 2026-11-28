#pragma once

// A popover: content drawn as an overlay above its anchor (below when there is no room), so
// opening a menu never moves the page. The popover is as wide as the anchor or, with
// `own_width`, as wide as itself from the anchor's left edge.

#include <functional>

#include <QWidget>

namespace loa::ui::kit::widgets {

struct Rect {
    float x = 0, y = 0, width = 0, height = 0;
    friend bool operator==(const Rect&, const Rect&) = default;
};

// Where a popup goes: `max_width`/`max_height` limit its layout, `x`/`y` place it.
struct Placement {
    float max_width = 0, max_height = 0;
};

// The layout limits for a popup over `anchor` inside `bounds` (width, height).
[[nodiscard]] Placement popover_limits(Rect anchor, float bounds_width, float bounds_height, float gap, bool fit_anchor);
// The popup rectangle for a laid-out size `width` x `height`.
[[nodiscard]] Rect popover_place(Rect anchor, float bounds_width, float bounds_height, float gap, bool fit_anchor, float width,
                                 float height);

// `anchor` with `popup` floating above it, inside the anchor's window.
class Popover : public QWidget {
  public:
    // Takes ownership of both; `popup` may be null (closed).
    Popover(QWidget* anchor, QWidget* popup, QWidget* parent = nullptr);
    ~Popover() override;

    // Lays the popup out at its own width, shifted left as needed to stay inside the window.
    Popover& own_width();
    // Calls `f` when a mouse button is pressed outside both the open popup and its anchor.
    Popover& on_dismiss(std::function<void()> f);

    [[nodiscard]] QWidget* anchor() const { return anchor_; }
    [[nodiscard]] QWidget* popup() const { return popup_; }
    // Re-places the popup (after the anchor moved or the window resized).
    void reposition();

  protected:
    bool eventFilter(QObject* watched, QEvent* event) override;
    void showEvent(QShowEvent* e) override;
    void moveEvent(QMoveEvent* e) override;
    void resizeEvent(QResizeEvent* e) override;

  private:
    QWidget* anchor_;
    QWidget* popup_;
    float gap_ = 6;
    bool fit_anchor_ = true;
    std::function<void()> on_dismiss_;
};

} // namespace loa::ui::kit::widgets
