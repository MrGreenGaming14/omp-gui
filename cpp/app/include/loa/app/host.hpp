#pragma once

// The Qt side of the app (iced's `daemon` runtime for `App`): runs the `Task`s `App::update`
// returns (messages, executor jobs, widget and window operations), keeps the app's
// subscriptions running, and draws the app in its windows (`AppView`).

#include <map>
#include <memory>
#include <optional>
#include <stop_token>
#include <string>

#include <QObject>
#include <QPointer>
#include <QWidget>

#include "loa/app/app.hpp"

class QLabel;
class QStackedWidget;
class QTimer;
class QVBoxLayout;

namespace loa::ui::app {
class AsioBridge;
}
namespace loa::ui::shell {
class ShellView;
class PaletteView;
} // namespace loa::ui::shell
namespace loa::ui::session::composer {
class ComposerView;
}
namespace loa::ui::session::transcript {
class TranscriptView;
}
namespace loa::ui::panel {
class DiffView;
class FilesView;
} // namespace loa::ui::panel
namespace loa::ui::terminal {
class TerminalView;
class Runner;
} // namespace loa::ui::terminal

namespace loa::app {

// The main window's content: the shell frame with the center (an area's screen, the active
// session's transcript and composer, or the start screen), the panel tabs, the overlay
// windows and the palette.
class AppView : public QWidget {
  public:
    AppView(Emit send, QWidget* parent = nullptr);
    ~AppView() override;

    void render(const App& app);
    // Runs the widget operations of a task on the views they name.
    void apply(const UiOp& op);

    [[nodiscard]] ui::shell::ShellView* shell() const { return shell_; }
    [[nodiscard]] ui::session::composer::ComposerView* composer() const { return composer_; }

  protected:
    void resizeEvent(QResizeEvent* event) override;
    void keyPressEvent(QKeyEvent* event) override;

  private:
    void render_center(const App& app);
    void render_start(const App& app);
    void render_panel(const App& app);
    void render_overlay(const App& app);
    void place_overlays();

    Emit emit_;
    ui::shell::ShellView* shell_ = nullptr;
    ui::shell::PaletteView* palette_ = nullptr;
    QWidget* center_ = nullptr;
    QStackedWidget* pages_ = nullptr;
    QWidget* screen_page_ = nullptr;
    QWidget* screen_ = nullptr;
    QWidget* session_page_ = nullptr;
    QVBoxLayout* session_layout_ = nullptr;
    QWidget* child_strip_ = nullptr;
    // The picked subagent's tab, shown in place of the conversation.
    QWidget* child_body_ = nullptr;
    QWidget* start_page_ = nullptr;
    QVBoxLayout* start_layout_ = nullptr;
    QWidget* start_body_ = nullptr;
    QLabel* notice_ = nullptr;
    ui::session::transcript::TranscriptView* transcript_ = nullptr;
    ui::session::composer::ComposerView* composer_ = nullptr;
    QStackedWidget* panel_ = nullptr;
    ui::panel::DiffView* diff_ = nullptr;
    ui::panel::FilesView* files_ = nullptr;
    ui::terminal::TerminalView* terminal_ = nullptr;
    QWidget* area_tab_ = nullptr;
    QWidget* scrim_ = nullptr;
    QWidget* overlay_ = nullptr;
};

// Runs an `App` in Qt windows.
class AppHost : public QObject {
  public:
    AppHost(std::unique_ptr<App> app, ui::app::AsioBridge& bridge);
    ~AppHost() override;

    // Runs the first task (`App::build`'s, then `App::open_main`'s) and draws the app.
    void start(Task task);
    // Runs `message` through `App::update` and does what it asks.
    void dispatch(Message message);

    [[nodiscard]] App& app() { return *app_; }
    // The main window, once `open_main` ran.
    [[nodiscard]] QWidget* main_window() const { return main_; }
    [[nodiscard]] AppView* view() const { return view_; }

  private:
    void run(Task task);
    void render();
    void resubscribe();
    void open_window(const ui_op::OpenWindow& op);
    void render_detached();
    [[nodiscard]] Emit emitter();

    std::unique_ptr<App> app_;
    ui::app::AsioBridge& bridge_;
    std::unique_ptr<ui::terminal::Runner> runner_;
    QPointer<QWidget> main_;
    AppView* view_ = nullptr;
    std::map<WindowId, QPointer<QWidget>> detached_;
    struct Running {
        QPointer<QTimer> timer;
        std::stop_source stop;
    };
    std::map<std::string, Running> subscriptions_;
    bool rendering_ = false;
    bool queued_render_ = false;
    std::shared_ptr<bool> alive_ = std::make_shared<bool>(true);
};

} // namespace loa::app
