// The `terminal` smoke screen: two terminal tabs, the active one showing coloured output while it connects.

#include <filesystem>
#include <string_view>

#include <QVBoxLayout>
#include <QWidget>

#include "loa/ui/app/smoke.hpp"
#include "loa/ui/terminal/view.hpp"

namespace loa::ui::app {

namespace {

namespace t = loa::ui::terminal;

constexpr std::string_view SAMPLE = "\x1b]0;build\x07$ make\r\n\x1b[1;32mok\x1b[0m compiled 3 files\r\n"
                                    "\x1b[31merror\x1b[0m: \x1b[4mmissing.rs\x1b[0m not found\r\n\x1b[7m reversed \x1b[0m $ ";

// The state outlives the view that renders it; no runner, so the tabs stay connecting.
struct Screen : QWidget {
    t::State state{t::default_metrics(), t::LocalShell::login_shell(std::filesystem::current_path())};
};

} // namespace

QWidget* terminal_screen() {
    auto* screen = new Screen;
    screen->setAccessibleName(QStringLiteral("terminal screen"));
    (void)screen->state.open(t::Remote{"example"});
    const auto id = screen->state.open(t::Remote{"build-host"}).first;
    (void)screen->state.update(t::msg::Output{id, t::ChannelRead{t::port::channel_read::Data{t::Bytes(SAMPLE.begin(), SAMPLE.end())}}});
    auto* layout = new QVBoxLayout(screen);
    layout->setContentsMargins(0, 0, 0, 0);
    auto* view = new t::TerminalView;
    layout->addWidget(view);
    view->render(screen->state);
    QObject::connect(view, &t::TerminalView::message, view, [screen, view](const t::Message& m) {
        const auto task = screen->state.update(m);
        view->render(screen->state);
        view->apply(task);
    });
    screen->resize(800, 480);
    return screen;
}

std::string check_terminal_screen(QWidget& screen) {
    auto* canvas = screen.findChild<t::Canvas*>();
    if (!canvas || !canvas->isVisible()) return "missing terminal canvas";
    if (canvas->width() < 100 || canvas->height() < 100) return "terminal canvas too small";
    if (!screen.findChild<QWidget*>(QStringLiteral("terminal tabs"))) return "missing terminal tabs";
    const auto& state = static_cast<Screen&>(screen).state;
    const auto* emulator = state.emulator(*state.active());
    if (!emulator || emulator->screen_text().find("compiled 3 files") == std::string::npos) return "output not shown";
    // The canvas reported its size, which resized the active terminal to fit.
    if (emulator->size().cols < 20) return "grid not resized to the canvas";
    return {};
}

} // namespace loa::ui::app
