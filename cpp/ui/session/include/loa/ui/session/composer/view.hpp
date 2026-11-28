#pragma once

// The composer's Qt view: renders `const State&` and reports input as Messages through one
// `message` signal. The editor is a QTextDocument-backed `QPlainTextEdit` whose keys run
// through `bind`, so the state stays the only owner of the text.

#include <optional>

#include <QPlainTextEdit>
#include <QWidget>

#include "loa/ui/session/composer/state.hpp"

class QKeyEvent;
class QLabel;
class QTimer;
class QVBoxLayout;

namespace loa::ui::session::composer {

class ComposerHighlighter;

// The `KeyPress` of a Qt key event.
[[nodiscard]] std::optional<KeyPress> to_key_press(const QKeyEvent& event);

// The text editor: shows the state's text and cursor, and reports keys, clicks and IME input.
class ComposerEditor : public QPlainTextEdit {
    Q_OBJECT

  public:
    explicit ComposerEditor(QWidget* parent = nullptr);

    void render(const State& state);

  signals:
    void message(const loa::ui::session::composer::Message& m);

  protected:
    void keyPressEvent(QKeyEvent* event) override;
    void inputMethodEvent(QInputMethodEvent* event) override;
    void mousePressEvent(QMouseEvent* event) override;
    void mouseMoveEvent(QMouseEvent* event) override;
    void mouseReleaseEvent(QMouseEvent* event) override;
    void mouseDoubleClickEvent(QMouseEvent* event) override;
    bool canInsertFromMimeData(const QMimeData* source) const override;
    void insertFromMimeData(const QMimeData* source) override;

  private:
    [[nodiscard]] Position position_at(const QPoint& point) const;
    void send(Message m) { emit message(m); }
    void fit_height();

    KeyContext ctx_;
    std::string selection_;
    bool rendering_ = false;
    ComposerHighlighter* highlighter_ = nullptr;
};

class ComposerView : public QWidget {
    Q_OBJECT

  public:
    explicit ComposerView(QWidget* parent = nullptr);

    void render(const State& state);
    // Runs the widget operations of an update (focus the editor).
    void apply(const Task& task);
    [[nodiscard]] ComposerEditor* editor() const { return editor_; }

  signals:
    void message(const loa::ui::session::composer::Message& m);

  protected:
    void changeEvent(QEvent* event) override;

  private:
    QWidget* build_queue(const State& state);
    QWidget* build_bar(const State& state);
    QWidget* build_working(const State& state);
    QWidget* build_images(const State& state);
    QWidget* build_controls(const State& state);
    QWidget* build_popup(const State& state);
    QWidget* chip(const State& state, const Chip& chip, bool compact);
    QWidget* toggle(const State& state, PickerId picker, QWidget* content, bool open, bool enabled, bool compact);
    QWidget* menu(const State& state, PickerId picker);
    QWidget* effort_card(const State& state);
    void send(Message m) { emit message(m); }

    QVBoxLayout* root_ = nullptr;
    QWidget* frame_ = nullptr;
    QVBoxLayout* frame_layout_ = nullptr;
    ComposerEditor* editor_ = nullptr;
    QWidget* dynamic_top_ = nullptr;   // queue and bar, above the frame
    QWidget* dynamic_head_ = nullptr;  // working indicator and image cards, above the editor
    QWidget* dynamic_tail_ = nullptr;  // controls, under the editor
    QWidget* popup_ = nullptr;         // the `/` and `@` popup
    QTimer* ticker_ = nullptr;
};

} // namespace loa::ui::session::composer
