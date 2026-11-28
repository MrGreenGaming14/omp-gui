#pragma once

// Qt views of the surface renderer. Each renders its `const` state and reports input through
// one `message` signal; none mutates state. Port of the iced views in
// `crates/loa-ui-surface/src/{viewer,picker}.rs`, `tab/*.rs` and `panel/{mod,launcher}.rs`.

#include <optional>

#include <QScrollArea>
#include <QWidget>

#include "loa/ui/surface/panel.hpp"
#include "loa/ui/surface/picker.hpp"
#include "loa/ui/surface/tab.hpp"

class QLabel;
class QLineEdit;
class QPushButton;
class QStackedWidget;
class QHBoxLayout;
class QVBoxLayout;

namespace loa::ui::surface {

// The picture of a pixel tab, letterboxed, with the locally drawn cursor; forwards pointer,
// wheel and keys in panel coordinates (`viewer.rs`).
class Viewer : public QWidget {
    Q_OBJECT

  public:
    explicit Viewer(QWidget* parent = nullptr);
    // `mode` must outlive the next `render` (the owner re-renders after every update).
    void render(const PixelMode* mode);

  signals:
    void message(const loa::ui::surface::Message& m);

  protected:
    void paintEvent(QPaintEvent* e) override;
    void resizeEvent(QResizeEvent* e) override;
    void mouseMoveEvent(QMouseEvent* e) override;
    void leaveEvent(QEvent* e) override;
    void mousePressEvent(QMouseEvent* e) override;
    void mouseReleaseEvent(QMouseEvent* e) override;
    void wheelEvent(QWheelEvent* e) override;
    void keyPressEvent(QKeyEvent* e) override;
    void keyReleaseEvent(QKeyEvent* e) override;
    void focusOutEvent(QFocusEvent* e) override;

  private:
    void pointer(PointerInput input);
    void key(QKeyEvent* e, bool pressed);
    [[nodiscard]] Size panel_size() const;

    const PixelMode* mode_ = nullptr;
    bool inside_ = false;
    // Buttons pressed inside and not yet released.
    unsigned dragging_ = 0;
    std::optional<loa::view::surface::Modifiers> modifiers_;
};

// The pages (or image) of a document tab at its zoom; reports scrolling (`document.rs`).
class DocumentView : public QScrollArea {
    Q_OBJECT

  public:
    explicit DocumentView(QWidget* parent = nullptr);
    void render(const DocumentMode& mode);

  signals:
    void message(const loa::ui::surface::Message& m);

  private:
    void scrolled();
    QWidget* content_ = nullptr;
    QVBoxLayout* pages_ = nullptr;
};

// One open surface: the header (name, notice, chips or zoom) over its mode's view (`tab/mod.rs`).
class TabView : public QWidget {
    Q_OBJECT

  public:
    explicit TabView(QWidget* parent = nullptr);
    void render(const State& state);

  signals:
    void message(const loa::ui::surface::Message& m);

  private:
    QLabel* name_ = nullptr;
    QLabel* notice_ = nullptr;
    QHBoxLayout* chips_ = nullptr;
    QStackedWidget* body_ = nullptr;
    Viewer* viewer_ = nullptr;
    DocumentView* document_ = nullptr;
};

// The file picker: up, the path field and the folder's entries (`picker.rs`).
class PickerView : public QWidget {
    Q_OBJECT

  public:
    explicit PickerView(QWidget* parent = nullptr);
    void render(const FilePicker& picker);

  signals:
    void message(const loa::ui::surface::picker::Message& m);

  private:
    QPushButton* up_ = nullptr;
    QLineEdit* path_ = nullptr;
    QVBoxLayout* list_ = nullptr;
    QLabel* error_ = nullptr;
};

// The Surfaces tab: the tab strip over the active tab or the launcher (`panel/*.rs`).
class PanelView : public QWidget {
    Q_OBJECT

  public:
    explicit PanelView(QWidget* parent = nullptr);
    void render(const Panel& panel);

  signals:
    void message(const loa::ui::surface::panel::Message& m);

  private:
    QWidget* build_launcher();
    void render_launcher(const Panel& panel);

    QHBoxLayout* tabs_ = nullptr;
    QPushButton* launcher_button_ = nullptr;
    QStackedWidget* body_ = nullptr;
    TabView* tab_ = nullptr;
    QScrollArea* launcher_ = nullptr;
    // Launcher parts.
    QWidget* status_ = nullptr;
    QLineEdit* url_ = nullptr;
    QPushButton* open_ = nullptr;
    QLabel* browser_note_ = nullptr;
    QPushButton* screencast_ = nullptr;
    QPushButton* virtual_display_ = nullptr;
    QLabel* capture_note_ = nullptr;
#if defined(LOA_REMOTE_VIDEO)
    QPushButton* video_on_ = nullptr;
    QPushButton* video_off_ = nullptr;
    QLabel* video_encoder_ = nullptr;
    QLabel* video_hardware_ = nullptr;
#endif
    PickerView* picker_ = nullptr;
    QLineEdit* address_ = nullptr;
    QLineEdit* password_ = nullptr;
    QPushButton* connect_ = nullptr;
    QVBoxLayout* vms_ = nullptr;
    QVBoxLayout* declared_ = nullptr;
    std::optional<SurfaceUri> shown_tab_;
};

} // namespace loa::ui::surface
