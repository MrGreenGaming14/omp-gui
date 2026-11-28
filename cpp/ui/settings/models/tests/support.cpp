#include "support.hpp"

namespace loa::ui::settings::models::test {

QApplication& app() {
    static int argc = 1;
    static char name[] = "loa_ui_settings_tests";
    static char* argv[] = {name, nullptr};
    static auto* a = qApp ? static_cast<QApplication*>(qApp) : new QApplication(argc, argv);
    return *a;
}

} // namespace loa::ui::settings::models::test
