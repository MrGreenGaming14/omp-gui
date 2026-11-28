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
// The `shell` screen: the app core (`loa::app::App`) over a temporary data directory, drawn by `AppView`.
[[nodiscard]] QWidget* app_shell_screen();
// Checks the frame (top bar, sidebar, panel) and the start screen with its composer.
[[nodiscard]] std::string check_app_shell_screen(QWidget& screen);
// The `sessions` screen: the app core with the sessions area's import and export screen open.
[[nodiscard]] QWidget* sessions_screen();
// Checks the import and export screen is shown with its path field and native sessions list.
[[nodiscard]] std::string check_sessions_screen(QWidget& screen);
// The `terminal` screen: two terminal tabs, the active one showing coloured output while it connects.
[[nodiscard]] QWidget* terminal_screen();
// Checks the canvas is shown, sized, and resized the active terminal's grid.
[[nodiscard]] std::string check_terminal_screen(QWidget& screen);
// The `agents` screen: the agents and background jobs panes over a sample session, beside the tools page.
[[nodiscard]] QWidget* agents_screen();
// Checks the panes rendered: the subagent rows, the job row and the tools tabs.
[[nodiscard]] std::string check_agents_screen(QWidget& screen);
// The `models` screen: the models frame on its sign in page, beside the model settings page and the picker.
[[nodiscard]] QWidget* models_screen();
// Checks the pages rendered: the frame tabs, the sign in rows, the settings sections and the picker rows.
[[nodiscard]] std::string check_models_screen(QWidget& screen);
// The `transcript` screen: a sample session with every row kind and a streamed markdown reply.
[[nodiscard]] QWidget* transcript_screen();
// Checks the rows were built: the code block, its copy button and the decision slot.
[[nodiscard]] std::string check_transcript_screen(QWidget& screen);
// The `surface` screen: the Surfaces tab on a pixel tab with a sample frame, beside one on its launcher.
[[nodiscard]] QWidget* surface_screen();
// Checks the pixel viewer, the launcher and its file picker are shown.
[[nodiscard]] std::string check_surface_screen(QWidget& screen);
// The `projects` screen: a project's settings, sessions and terminal history, over the New project form.
[[nodiscard]] QWidget* settings_projects_screen();
// Checks the project sections and the New project form with its `loa-new-project-name` input.
[[nodiscard]] std::string check_settings_projects_screen(QWidget& screen);
// The agent definitions screen (shown by `--smoke agents` beside the panes): the list and a copied agent's form.
[[nodiscard]] QWidget* settings_agents_screen();
// Checks the list, the template, model and thinking pickers and the form fields.
[[nodiscard]] std::string check_settings_agents_screen(QWidget& screen);
// The `workflows` screen: the workflow list and a managed workflow's structured editor.
[[nodiscard]] QWidget* settings_workflows_screen();
// Checks the list, the agent access rows, steps and rule blocks.
[[nodiscard]] std::string check_settings_workflows_screen(QWidget& screen);
// The `peers` screen: this node, a peer and a nearby node, and the peer's grant form asking for SSH consent.
[[nodiscard]] QWidget* settings_peers_screen();
// Checks the node card, peer rows, grants and the SSH confirmation.
[[nodiscard]] std::string check_settings_peers_screen(QWidget& screen);
// The `updates` screen: a held omp update with its gate report open, and an app swap pending.
[[nodiscard]] QWidget* settings_updates_screen();
// Checks the status lines, hold choices, gate report and peer standing.
[[nodiscard]] std::string check_settings_updates_screen(QWidget& screen);
// The `decisions` screen: a card of every decision kind on a desk, and the unattended settings.
[[nodiscard]] QWidget* decisions_screen();
// Checks one card per kind rendered, with the approval's diff preview, and the settings.
[[nodiscard]] std::string check_decisions_screen(QWidget& screen);
// The `surfaces` and `updater` screens: the app core's Surfaces tab and Updates screen, drawn by
// their areas over a temporary data directory.
[[nodiscard]] QWidget* surfaces_area_screen();
[[nodiscard]] std::string check_surfaces_area_screen(QWidget& screen);
[[nodiscard]] QWidget* updater_area_screen();
[[nodiscard]] std::string check_updater_area_screen(QWidget& screen);

} // namespace loa::ui::app
