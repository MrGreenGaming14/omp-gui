#pragma once

// `loa-qt --smoke <screen>`: builds one screen offscreen, shows it, spins the event loop once,
// runs the screen's checks and exits. UI modules register their screens here.

#include <functional>
#include <map>
#include <string>
#include <string_view>
#include <vector>

class QWidget;

namespace loa::ui::app {

class Smoke {
  public:
    // Builds the screen; returns null to fail.
    using Build = std::function<QWidget*()>;
    // Checks the shown screen; returns an error message, or empty when it passed.
    using Check = std::function<std::string(QWidget&)>;

    // The process-wide registry, with the builtin `kit` screen.
    [[nodiscard]] static Smoke& registry();

    void add(std::string name, Build build, Check check = {});
    [[nodiscard]] std::vector<std::string> names() const;

    // 0 when `screen` builds, shows and passes its checks; 1 on failure; 2 when unknown.
    // Needs a running `QApplication`.
    [[nodiscard]] int run(std::string_view screen) const;

  private:
    struct Entry {
        Build build;
        Check check;
    };
    std::map<std::string, Entry, std::less<>> screens_;
};

// The `kit` screen: every icon, tone, text role, button kind, surface and shared widget.
[[nodiscard]] QWidget* kit_gallery();
// Checks the gallery: every icon resource renders, and every widget has an accessible name.
[[nodiscard]] std::string check_kit_gallery(QWidget& gallery);
// The `panel` screen: the diff, files (over `LocalFs` at the working directory) and session tree panes.
[[nodiscard]] QWidget* panel_screen();
// Checks the panes rendered: diff rows painted, files listed, session rows shown.
[[nodiscard]] std::string check_panel_screen(QWidget& screen);
// The `agents` screen: the agents and background jobs panes over a sample session, beside the tools page.
[[nodiscard]] QWidget* agents_screen();
// Checks the panes rendered: the subagent rows, the job row and the tools tabs.
[[nodiscard]] std::string check_agents_screen(QWidget& screen);
// The `models` screen: the models frame on its sign in page, beside the model settings page and the picker.
[[nodiscard]] QWidget* models_screen();
// Checks the pages rendered: the frame tabs, the sign in rows, the settings sections and the picker rows.
[[nodiscard]] std::string check_models_screen(QWidget& screen);

} // namespace loa::ui::app
