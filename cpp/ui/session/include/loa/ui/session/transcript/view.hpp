#pragma once

// The transcript's Qt view: a keyed virtual list of rows over a scroll area, the "Jump to
// latest" overlay, selectable rich text and copy buttons. Renders `const State&` with the
// session DOM and reports input through one `message` signal.
// Port of the view halves of `crates/loa-ui-session/src/transcript/{mod,prose,virtual_list,
// selection,copy_button}.rs`.

#include <cstddef>
#include <functional>
#include <memory>
#include <string>
#include <unordered_map>
#include <vector>

#include <QPushButton>
#include <QWidget>

#include "loa/ui/session/transcript/rows.hpp"
#include "loa/ui/session/transcript/selection.hpp"
#include "loa/ui/session/transcript/state.hpp"

class QScrollArea;
class QTextLayout;

namespace loa::ui::session::transcript {

// A ghost caption button that copies what `value` returns when clicked.
class CopyButton : public QPushButton {
  public:
    CopyButton(std::string_view label, std::function<std::string()> value, Emit post, QWidget* parent = nullptr);
};

class SelectableText;

// One message's selectable blocks: a press, drag and Ctrl+C here select and copy across every
// block in document order; a click on a link opens it.
class SelectionArea : public QWidget {
  public:
    SelectionArea(Emit post, QWidget* parent = nullptr);

    // A block of `text` with inline formats; `spans` partitions `text` as md spans do.
    SelectableText* text(const md::Text& spans, float size, float line_height, bool mono, QWidget* parent);
    [[nodiscard]] const Selection& selection() const { return selection_; }
    // Copies the selection as Ctrl+C does: emits `Copy` unless nothing is selected.
    bool copy();

  protected:
    void mousePressEvent(QMouseEvent* e) override;
    void mouseMoveEvent(QMouseEvent* e) override;
    void mouseReleaseEvent(QMouseEvent* e) override;
    void keyPressEvent(QKeyEvent* e) override;
    void focusOutEvent(QFocusEvent* e) override;

  private:
    [[nodiscard]] std::optional<Endpoint> hit(const QPoint& at, bool clamp) const;
    void repaint_blocks();

    Emit emit_;
    Selection selection_;
    std::vector<SelectableText*> blocks_;
};

// Rendered markdown of one assistant message.
[[nodiscard]] QWidget* prose_widget(const std::vector<md::Block>& blocks, const std::string& source, const Emit& post);
// Plain selectable text (user messages, unparsed replies).
[[nodiscard]] QWidget* selectable(std::string_view text, const Emit& post);

// A column of keyed rows, `top` and `bottom` pixels from the ends of the content. A row keeps
// its widget while its key stays in the window; rows outside the window take no widgets.
class VirtualList : public QWidget {
  public:
    using Build = std::function<QWidget*(NodeId key)>;
    using Measured = std::function<void(NodeId key, float height)>;

    explicit VirtualList(QWidget* parent = nullptr);

    // Shows `keys` in order: reuses the widgets of known keys whose `version` is unchanged and
    // builds the others. Reports each row's height when it is first laid out or resized.
    void sync(float top, float bottom, const std::vector<NodeId>& keys, const std::vector<std::size_t>& versions,
              const Build& build, const Measured& measured);
    [[nodiscard]] QWidget* row(NodeId key) const;
    [[nodiscard]] std::size_t rows() const { return order_.size(); }

  private:
    struct Row {
        QWidget* widget = nullptr;
        std::size_t version = 0;
        float reported = -1;
    };
    void relayout(const Measured& measured);

    float top_ = 0, bottom_ = 0;
    std::vector<NodeId> order_;
    std::unordered_map<NodeId, Row> rows_;
};

class TranscriptView : public QWidget {
    Q_OBJECT

  public:
    // Renders the decision card of a slot.
    using SlotView = std::function<QWidget*(Slot)>;

    explicit TranscriptView(QWidget* parent = nullptr);

    void set_slot(SlotView slot) { slot_ = std::move(slot); }
    void render(const State& state, const loa::core::Dom& dom);
    // Runs the widget operations of an update (scroll, clipboard).
    void apply(const Task& task);

    [[nodiscard]] VirtualList* list() const { return list_; }
    [[nodiscard]] QScrollArea* scroll_area() const { return scroll_; }

  signals:
    void message(const loa::ui::session::transcript::Message& m);

  protected:
    void resizeEvent(QResizeEvent* e) override;

  private:
    void send(Message m) { emit message(m); }
    void report_scroll();
    void place_jump();

    QScrollArea* scroll_ = nullptr;
    VirtualList* list_ = nullptr;
    QPushButton* jump_ = nullptr;
    SlotView slot_;
    float bottom_inset_ = 0;
    bool rendering_ = false;
    ScrollMetrics reported_{-1, -1, -1};
};

} // namespace loa::ui::session::transcript
