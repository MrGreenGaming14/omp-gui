// The `shell` smoke screen: the whole app core (services over a temporary data directory,
// runtime, areas, shell) drawn by `AppView`, its startup tasks left unrun.

#include <memory>

#include <QVBoxLayout>
#include <QWidget>

#include "loa/app/app.hpp"
#include "loa/app/host.hpp"
#include "loa/runtime/tempfile.hpp"
#include "loa/ui/app/smoke.hpp"
#include "loa/ui/shell/layout.hpp"

namespace loa::ui::app {

namespace {

// Holds the app and its data directory for as long as the screen shows.
class AppScreen final : public QWidget {
  public:
    AppScreen(loa::runtime::TempDir dir, std::unique_ptr<loa::app::App> app)
        : dir_(std::move(dir)), app_(std::move(app)) {
        setAccessibleName(QStringLiteral("app shell"));
        auto* layout = new QVBoxLayout(this);
        layout->setContentsMargins(0, 0, 0, 0);
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

QWidget* app_shell_screen() {
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
    auto built = loa::app::App::build(std::move(services), std::move(parts));
    auto* screen = new AppScreen(std::move(*dir), std::move(built.first));
    screen->resize(1280, 800);
    return screen;
}

std::string check_app_shell_screen(QWidget& screen) {
    const auto* top = screen.findChild<QWidget*>(QStringLiteral("top bar"));
    if (!top || top->height() != shell::topbar::HEIGHT) return "top bar height";
    if (!screen.findChild<QWidget*>(QStringLiteral("sidebar"))) return "missing sidebar";
    if (!screen.findChild<QWidget*>(QStringLiteral("panel"))) return "missing panel";
    const auto* start = screen.findChild<QWidget*>(QStringLiteral("Start"));
    if (!start || !start->isVisible()) return "the start screen isn't shown";
    if (!start->findChild<QWidget*>(QStringLiteral("loa-composer-editor"))) return "missing composer on the start screen";
    return {};
}

} // namespace loa::ui::app
