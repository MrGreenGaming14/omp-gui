// The `tools` smoke screen: the tools area's Tools and Remote shell tabs and the sandbox area's
// Sandbox tab, drawn by the areas of the whole app core over a temporary data directory, its
// startup tasks left unrun.

#include <memory>

#include <QHBoxLayout>
#include <QWidget>

#include "loa/app/app.hpp"
#include "loa/app/routes.hpp"
#include "loa/runtime/tempfile.hpp"
#include "loa/ui/app/smoke.hpp"
#include "loa/ui/panel/tools_view.hpp"
#include "loa/ui/terminal/view.hpp"

namespace loa::ui::app {

namespace {

namespace la = loa::app;

// Holds the app and its data directory for as long as the screen shows.
class ToolsScreen final : public QWidget {
  public:
    ToolsScreen(loa::runtime::TempDir dir, std::unique_ptr<la::App> app) : dir_(std::move(dir)), app_(std::move(app)) {
        setAccessibleName(QStringLiteral("tools"));
        auto* layout = new QHBoxLayout(this);
        const la::Emit ignore = [](la::Message) {};
        const std::pair<std::string_view, std::string_view> tabs[] = {
            {la::area_id::TOOLS, "tools"}, {la::area_id::TOOLS, "remote-shell"}, {la::area_id::SANDBOX, "sandbox"}};
        for (auto [area, tab] : tabs)
            if (const auto* owner = app_->areas.find(area))
                if (auto* view = owner->view_tab(tab, app_->ctx, ignore)) layout->addWidget(view, 1);
    }

  private:
    loa::runtime::TempDir dir_;
    std::unique_ptr<la::App> app_;
};

} // namespace

QWidget* tools_screen() {
    auto dir = loa::runtime::TempDir::create();
    if (!dir) return nullptr;
    auto memory_key = [](const std::filesystem::path&) -> trust::Result<std::pair<trust::NodeIdentity, trust::KeySource>> {
        auto identity = trust::NodeIdentity::generate();
        if (!identity) return std::unexpected(identity.error());
        return std::pair{std::move(*identity), trust::KeySource{trust::key_source::Memory{"smoke"}}};
    };
    auto loaded = la::Services::load_in(dir->path() / "loa", memory_key, dir->path().string(), la::Host::system());
    if (!loaded) return nullptr;
    auto services = std::move(loaded->first);
    auto library = la::open_library(services.storage.dirs, services.node);
    if (!library) return nullptr;
    auto parts = la::machine_parts(services, std::move(*library));
    auto built = la::App::build(std::move(services), std::move(parts));
    auto* screen = new ToolsScreen(std::move(*dir), std::move(built.first));
    screen->resize(1600, 800);
    return screen;
}

std::string check_tools_screen(QWidget& screen) {
    if (!screen.findChild<panel::ToolsView*>()) return "missing the Tools tab";
    if (!screen.findChild<terminal::TerminalView*>()) return "missing the Remote shell tab";
    if (!screen.findChild<QWidget*>(QStringLiteral("Sandbox"))) return "missing the Sandbox tab";
    return {};
}

} // namespace loa::ui::app
