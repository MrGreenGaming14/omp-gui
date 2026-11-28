// The `sessions` smoke screen: the app core over a temporary data directory and home, showing
// the sessions area's import and export screen as `sessions.import` opens it.

#include <memory>

#include <QVBoxLayout>
#include <QWidget>

#include "loa/app/app.hpp"
#include "loa/app/host.hpp"
#include "loa/runtime/tempfile.hpp"
#include "loa/ui/app/smoke.hpp"

namespace loa::ui::app {

namespace {

// Holds the app and its directories for as long as the screen shows.
class SessionsScreen final : public QWidget {
  public:
    SessionsScreen(loa::runtime::TempDir dir, std::unique_ptr<loa::app::App> app)
        : dir_(std::move(dir)), app_(std::move(app)) {
        setAccessibleName(QStringLiteral("sessions"));
        auto* layout = new QVBoxLayout(this);
        layout->setContentsMargins(0, 0, 0, 0);
        // What the command asks of the app (a native session scan) ran with it; nothing is left.
        (void)app_->command(shell::palette::CommandId("sessions.import"));
        view_ = new loa::app::AppView([](loa::app::Message) {}, this);
        layout->addWidget(view_);
        view_->render(*app_);
    }

  private:
    loa::runtime::TempDir dir_;
    std::unique_ptr<loa::app::App> app_;
    loa::app::AppView* view_ = nullptr;
};

} // namespace

QWidget* sessions_screen() {
    auto dir = loa::runtime::TempDir::create();
    if (!dir) return nullptr;
    auto memory_key = [](const std::filesystem::path&) -> trust::Result<std::pair<trust::NodeIdentity, trust::KeySource>> {
        auto identity = trust::NodeIdentity::generate();
        if (!identity) return std::unexpected(identity.error());
        return std::pair{std::move(*identity), trust::KeySource{trust::key_source::Memory{"smoke"}}};
    };
    auto loaded = loa::app::Services::load_in(dir->path() / "loa", memory_key, dir->path().string(),
                                              loa::app::Host::system());
    if (!loaded) return nullptr;
    auto services = std::move(loaded->first);
    auto library = loa::app::open_library(services.storage.dirs, services.node);
    if (!library) return nullptr;
    auto parts = loa::app::machine_parts(services, std::move(*library));
    // Native sessions are looked for in an empty home, not the developer's.
    parts.home = dir->path() / "home";
    auto built = loa::app::App::build(std::move(services), std::move(parts));
    auto* screen = new SessionsScreen(std::move(*dir), std::move(built.first));
    screen->resize(1280, 800);
    return screen;
}

std::string check_sessions_screen(QWidget& screen) {
    const auto* page = screen.findChild<QWidget*>(QStringLiteral("Import and export sessions"));
    if (!page || !page->isVisible()) return "the import and export screen isn't shown";
    if (!page->findChild<QWidget*>(QStringLiteral("import path"))) return "missing the import path field";
    if (!page->findChild<QWidget*>(QStringLiteral("native sessions"))) return "missing the native sessions list";
    return {};
}

} // namespace loa::ui::app
