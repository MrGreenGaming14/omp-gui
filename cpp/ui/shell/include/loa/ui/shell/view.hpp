#pragma once

// Qt views of the shell: the app frame (`ShellView`) and the command palette (`PaletteView`).
// They render `const State&` and report user input as Messages through one `message` signal.

#include <optional>

#include <QWidget>

#include "loa/ui/shell/keymap.hpp"
#include "loa/ui/shell/layout.hpp"
#include "loa/ui/shell/palette.hpp"

class QKeyEvent;
class QLineEdit;
class QVBoxLayout;
class QHBoxLayout;
class QScrollArea;

namespace loa::ui::shell {

// The `KeyPress` of a Qt key event (Cmd is `Logo` on macOS, where Qt reports it as Control).
[[nodiscard]] std::optional<palette::KeyPress> to_key_press(const QKeyEvent& event);

// The frame: sidebar, top bar over the center slot, and the right panel with its tabs.
class ShellView : public QWidget {
    Q_OBJECT

  public:
    explicit ShellView(QWidget* parent = nullptr);

    // The widget drawn in the center slot, and the body of the active panel tab (owned).
    void set_center(QWidget* center);
    void set_panel_body(QWidget* body);

    void render(const layout::State& state);

  signals:
    void message(const loa::ui::shell::layout::Message& m);

  protected:
    void resizeEvent(QResizeEvent* event) override;

  private:
    QWidget* build_sidebar(const layout::State& state);
    QWidget* build_topbar(const layout::State& state);
    QWidget* build_project_tabs(const layout::State& state);
    QWidget* build_panel(const layout::State& state);
    QWidget* edge(layout::Pane pane);
    void send(layout::Message m) { emit message(m); }
    void send_sidebar(sidebar::Message m) { emit message(layout::Message{layout::msg::Sidebar{std::move(m)}}); }

    QHBoxLayout* frame_ = nullptr;
    QLineEdit* search_ = nullptr;
    QWidget* center_ = nullptr;
    QWidget* center_slot_ = nullptr;
    QWidget* panel_body_ = nullptr;
    QWidget* panel_slot_ = nullptr;
};

// The palette card: a search field over the ranked rows.
class PaletteView : public QWidget {
    Q_OBJECT

  public:
    explicit PaletteView(QWidget* parent = nullptr);

    void render(const palette::State& state);
    // Runs the widget operations of an update (focus the field, scroll the list).
    void apply(const Task& task);

  signals:
    void message(const loa::ui::shell::palette::Message& m);

  protected:
    bool eventFilter(QObject* watched, QEvent* event) override;

  private:
    QLineEdit* input_ = nullptr;
    QWidget* list_ = nullptr;
    QVBoxLayout* rows_ = nullptr;
    QScrollArea* scroll_ = nullptr;
};

} // namespace loa::ui::shell
