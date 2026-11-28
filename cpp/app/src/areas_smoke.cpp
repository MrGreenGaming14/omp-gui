// The `surfaces` and `updater` smoke screens: the app core over a temporary data directory, its
// startup tasks left unrun, with the Surfaces tab and the Updates screen drawn by their areas.

#include <memory>

#include <QVBoxLayout>
#include <QWidget>

#include "loa/app/app.hpp"
#include "loa/runtime/tempfile.hpp"
#include "loa/ui/app/smoke.hpp"

namespace loa::ui::app {

namespace {

// Holds the app and its data directory for as long as the area's widget shows.
class AreaScreen final : public QWidget {
  public:
    AreaScreen(loa::runtime::TempDir dir, std::unique_ptr<loa::app::App> app) : dir_(std::move(dir)), app_(std::move(app)) {
        auto* layout = new QVBoxLayout(this);
        layout->setContentsMargins(0, 0, 0, 0);
    }
    [[nodiscard]] loa::app::App& app() { return *app_; }

  private:
    loa::runtime::TempDir dir_;
    std::unique_ptr<loa::app::App> app_;
};

AreaScreen* app_screen() {
    auto dir = loa::runtime::TempDir::create();
    if (!dir) return nullptr;
    auto memory_key = [](const std::filesystem::path&) -> trust::Result<std::pair<trust::NodeIdentity, trust::KeySource>> {
        auto identity = trust::NodeIdentity::generate();
        if (!identity) return std::unexpected(identity.error());
        return std::pair{std::move(*identity), trust::KeySource{trust::key_source::Memory{"smoke"}}};
    };
    auto loaded = loa::app::Services::load_in(dir->path() / "loa", memory_key, dir->path().string(), loa::app::Host::system());
    if (!loaded) return nullptr;
    auto services = std::move(loaded->first);
    auto library = loa::app::open_library(services.storage.dirs, services.node);
    if (!library) return nullptr;
    auto parts = loa::app::machine_parts(services, std::move(*library));
    auto built = loa::app::App::build(std::move(services), std::move(parts));
    return new AreaScreen(std::move(*dir), std::move(built.first));
}

QWidget* area_screen(std::string_view area, bool tab) {
    auto* screen = app_screen();
    if (!screen) return nullptr;
    auto& app = screen->app();
    const auto* found = app.areas.find(area);
    QWidget* view = nullptr;
    if (found) {
        auto send = [](loa::app::Message) {};
        view = tab ? found->view_tab(area, app.ctx, send) : found->view_screen(app.ctx, send);
    }
    if (!view) {
        delete screen;
        return nullptr;
    }
    screen->layout()->addWidget(view);
    screen->resize(1100, 800);
    return screen;
}

bool shows(QWidget& screen, const QString& name) {
    for (const auto* w : screen.findChildren<QWidget*>())
        if (w->accessibleName() == name && w->isVisible()) return true;
    return false;
}

} // namespace

QWidget* surfaces_area_screen() { return area_screen("surfaces", true); }

std::string check_surfaces_area_screen(QWidget& screen) {
    if (!shows(screen, QStringLiteral("surfaces"))) return "missing Surfaces tab";
    if (!shows(screen, QStringLiteral("surface launcher"))) return "missing launcher";
    return {};
}

QWidget* updater_area_screen() { return area_screen("updater", false); }

std::string check_updater_area_screen(QWidget& screen) {
    if (!shows(screen, QStringLiteral("updater"))) return "missing Updates screen";
    return {};
}

} // namespace loa::ui::app
