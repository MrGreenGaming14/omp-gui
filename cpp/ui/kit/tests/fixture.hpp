#pragma once

#include <QApplication>

// One offscreen QApplication for every test that touches fonts, pixmaps or widgets.
inline QApplication& qt_app() {
    static int argc = 1;
    static char name[] = "loa_ui_kit_tests";
    static char* argv[] = {name, nullptr};
    if (qEnvironmentVariableIsEmpty("QT_QPA_PLATFORM")) qputenv("QT_QPA_PLATFORM", "offscreen");
    // Leaked: destroying a QApplication during static destruction crashes in Qt's own teardown.
    static auto* app = new QApplication(argc, argv);
    return *app;
}
