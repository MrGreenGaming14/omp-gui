#pragma once

// The terminal tab's Qt view: a tab bar (titles, close, +), and the active terminal drawn on `Canvas` with its
// status line. Renders `const State&`, reports input through one `message` signal, never mutates the state.

#include <chrono>
#include <optional>
#include <string_view>

#include <QWidget>

#include "loa/ui/terminal/palette.hpp"
#include "loa/ui/terminal/state.hpp"
#include "loa/view/theme.hpp"

class QFont;
class QVBoxLayout;

namespace loa::ui::terminal {

// The kit `PanelPane` of the terminal tab.
struct PanelPane {
    std::string_view id;
    std::string_view title;
    loa::view::theme::Icon icon;
};
inline constexpr PanelPane TERMINAL_PANE{"terminal", "Terminal", loa::view::theme::Icon::Terminal};

// The grid metrics of a monospaced `font` (cell width from a sample run, line height 1.25).
[[nodiscard]] Metrics measure(const QFont& font);
// `measure` of the kit `Mono` font.
[[nodiscard]] Metrics default_metrics();

// One terminal's grid, drawn from its emulator; mouse and keys become `CanvasEvent`s.
class Canvas : public QWidget {
    Q_OBJECT

  public:
    explicit Canvas(QWidget* parent = nullptr);
    void render(TabId tab, const Emulator* emulator, Metrics metrics);

  signals:
    void input(const loa::ui::terminal::Message& m);

  protected:
    void paintEvent(QPaintEvent* e) override;
    void resizeEvent(QResizeEvent* e) override;
    void keyPressEvent(QKeyEvent* e) override;
    void mousePressEvent(QMouseEvent* e) override;
    void mouseMoveEvent(QMouseEvent* e) override;
    void mouseReleaseEvent(QMouseEvent* e) override;
    void wheelEvent(QWheelEvent* e) override;
    void focusInEvent(QFocusEvent* e) override;
    void focusOutEvent(QFocusEvent* e) override;
    bool focusNextPrevChild(bool next) override;

  private:
    void publish(CanvasEvent e);
    [[nodiscard]] GridSize grid() const;

    TabId tab_;
    const Emulator* emulator_ = nullptr;
    Metrics metrics_;
    bool selecting_ = false;
    std::optional<GridSize> sent_grid_;
    struct Click {
        std::chrono::steady_clock::time_point at;
        ViewportCell cell;
        int count;
    };
    std::optional<Click> last_click_;
    float scroll_remainder_ = 0;
};

class TerminalView : public QWidget {
    Q_OBJECT

  public:
    explicit TerminalView(QWidget* parent = nullptr);
    void render(const State& state);
    // Runs the clipboard operations of an update; `ReadClipboard` answers with a `Paste` message.
    void apply(const Task& task);

  signals:
    void message(const loa::ui::terminal::Message& m);

  private:
    QVBoxLayout* layout_ = nullptr;
    QWidget* bar_ = nullptr;
    QWidget* body_ = nullptr;
    Canvas* canvas_ = nullptr;
};

} // namespace loa::ui::terminal
