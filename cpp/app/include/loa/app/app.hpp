#pragma once

// The application: the shell frame, the chat, composer and decision cards of the active
// session, the palette, the built-in panel tabs and the areas (port of `loa-app/src/app.rs`).
// `App` is plain C++: `update` returns a `Task` the Qt host (`AppHost`, `loa/app/host.hpp`)
// runs, and `AppView` draws it.

#include <chrono>
#include <cstddef>
#include <expected>
#include <filesystem>
#include <functional>
#include <memory>
#include <optional>
#include <span>
#include <string>
#include <string_view>
#include <utility>
#include <vector>

#include "loa/app/area.hpp"
#include "loa/app/lifecycle.hpp"
#include "loa/app/message.hpp"
#include "loa/app/registry.hpp"
#include "loa/app/services.hpp"
#include "loa/app/task.hpp"
#include "loa/app/windows.hpp"
#include "loa/app/workspace.hpp"
#include "loa/app_runtime/runtime.hpp"
#include "loa/models/service.hpp"
#include "loa/ui/panel/diff.hpp"
#include "loa/ui/panel/files.hpp"
#include "loa/ui/shell/palette.hpp"
#include "loa/ui/terminal/state.hpp"

namespace loa::app {

// How often the sidebar's usage meter re-reads usage.
inline constexpr std::chrono::seconds USAGE_REFRESH{300};
// How often running sessions redraw, so elapsed times advance.
inline constexpr std::chrono::seconds TICK{1};
// How many recent sessions the start screen lists.
inline constexpr std::size_t RECENTS = 4;

// What `App::build` starts from besides the services: the runtime's parts, the backends areas
// build on and the areas. `machine_parts` makes them from this machine; tests pass offline ones.
struct AppParts {
    app_runtime::RuntimeParts runtime;
    // The model screens' backend.
    models::Parts models;
    // Writes the projects' digests.
    std::shared_ptr<const projects::Summarizers> summarizer;
    // Home directory: native sessions, omp's rules and config.
    std::filesystem::path home;
    // Opens links clicked in the transcript.
    std::shared_ptr<const contracts::UrlOpener> opener;
    // The areas, booted in `AREA_ORDER`.
    std::vector<AreaBoot> areas = builtin_areas();
};

// The runtime over `library` and `harnesses`: on the project selected last run while its folder
// still exists, else on no project in the workspace, and on the saved approval mode.
[[nodiscard]] app_runtime::RuntimeParts runtime_parts(const Services& services,
                                                      std::shared_ptr<session::Library> library,
                                                      app_runtime::Harnesses harnesses);
// The parts of the app on this machine: every harness the registry knows, on the executables
// `[harness] executables` sets, and the backends areas build on, composed from them.
[[nodiscard]] AppParts machine_parts(const Services& services, std::shared_ptr<session::Library> library);

class App;

// One of the app's own commands (`app/keys.rs`): runs on the app with its argument.
using AppCommand = CommandSpec<App, Task (*)(App&, std::string_view)>;
// The app's commands, in palette order.
[[nodiscard]] std::span<const AppCommand> app_commands();

class App {
  public:
    // The app on this machine's services, in the startup order of the Rust app:
    // `Services::load` → `open_library` → `machine_parts` → `App::build`. `first_arg` is the
    // folder argument of the command line.
    [[nodiscard]] static std::expected<std::pair<std::unique_ptr<App>, Task>, std::string> make(
        std::optional<std::string> first_arg);
    // The app over `services` and `parts`: Recents from the session store, every area registered.
    [[nodiscard]] static std::pair<std::unique_ptr<App>, Task> build(Services services, AppParts parts);

    App(const App&) = delete;
    App& operator=(const App&) = delete;

    // Opens the main window, once, when the app boots; then reads the model catalog.
    Task open_main();

    Task update(Message message);
    // Esc closes the innermost thing open, else interrupts the harness.
    Task escape();
    [[nodiscard]] Subscriptions subscription() const;
    // The title of window `id`.
    [[nodiscard]] std::string title(WindowId id) const;
    // Every panel tab: the built-in ones, then each area's.
    [[nodiscard]] std::vector<PanelTab> tabs() const;
    // Runs palette command `id`: one of `app_commands()`, else the command of the area its prefix names.
    Task command(const ui::shell::palette::CommandId& id);

    Ctx ctx;
    Registry areas;
    SessionController sessions;
    Workspace workspace;
    ui::shell::palette::State palette;
    ui::panel::files::State files;
    ui::panel::diff::State diff;
    ui::terminal::State terminal;
    Windows windows;
    // The last `omp usage --json` read, for the Usage window.
    std::optional<std::string> usage;
    // Reads the clipboard's image into a file under the directory given; set by the Qt host.
    std::function<std::expected<std::optional<Pasted>, std::string>(const std::filesystem::path&)> read_clipboard_image;
    // The pixel size of an image file; set by the Qt host.
    std::function<std::optional<std::pair<std::uint32_t, std::uint32_t>>(const std::filesystem::path&)> image_size;

    // ---- workspace (`app/workspace.rs`) --------------------------------------------------
    // Works in `path` from now on; an open session is replaced by a fresh one there.
    Task switch_folder(std::filesystem::path path);
    // Points Files, Changes, the terminal, the composer and new sessions at `path`.
    Task set_folder(std::filesystem::path path);
    // Puts `path` first among the recent folders the composer lists.
    void remember(const std::filesystem::path& path);
    // The composer's folder chip and the terminal follow `cwd`.
    Task follow(std::filesystem::path cwd);
    // Makes `project` the active one, with its folder `cwd` when it has one.
    Task activate_project(std::string project, std::optional<std::filesystem::path> cwd);
    // The composer's project chip: the shown session moves to `project`; with none shown, new
    // sessions join it.
    Task project_picked(std::optional<std::string> project);
    // Drops every trace of the deleted `project`.
    Task forget_project(std::string_view project);
    // Reads and parses the working tree diff of the current folder, off the UI thread.
    [[nodiscard]] Task working_diff() const;
    Task show_diff(std::optional<std::expected<std::vector<ui::panel::diff::FileDiff>, std::string>> parsed);
    // Asks for a folder with the desktop's folder dialog.
    [[nodiscard]] Task browse() const;
    // Drops the tab of `project`; it gets one again only when activated again.
    void close_project_tab(std::string_view project);
    // Gives a tab to the project of every session opened since the last call.
    void sync_project_tabs();
    // Activates the project tab after (or before) the active project's, wrapping around.
    Task step_project_tab(bool forward);

    // ---- windows (`app/windows.rs`) -------------------------------------------------------
    Task window_update(WindowMessage message);
    // Shows `what` in its own window, or brings its window forward.
    Task detach(Detached what);
    // Shows a read-only DOM transcript in its own detached window without creating a session.
    Task detach_transcript(std::string key, std::string title, core::Dom dom);
    [[nodiscard]] std::string detached_title(const Detached& what) const;

    // ---- media (`app/media.rs`) -----------------------------------------------------------
    Task paste_image();
    Task image_pasted(std::expected<std::optional<Pasted>, std::string> pasted);
    Task file_dropped(const std::filesystem::path& path);

  private:
    App(Services services, app_runtime::RuntimeParts runtime, Backends backends, ui::shell::layout::State shell);

    Task start(std::vector<AreaBoot> boots, ui::shell::Task setup);
    bool list_tab(std::string_view id);
    Task open_tab(std::string_view id);
    Task reveal_tab(std::string_view id);
    void refresh_commands();
    // Runs a session lifecycle call with the context and the areas, then what it asked of the app.
    Task lifecycle(const std::function<Task(SessionController&, Ctx&, Registry&)>& call);
    Task after();
    Task settle();
    std::optional<Task> slash(std::string_view text);
    Task composer_event(std::optional<loa::view::session::composer::Intent> event);
    struct Intent;
    Task intent(Intent intent);
    Task handle(Message message);
    void clear_project();
    void add_project_tab(std::string_view project);
    [[nodiscard]] std::optional<WindowId> detached_window(const Detached& what) const;
    std::optional<Detached> forget_window(WindowId id);
    void sync_detached();
    Task sync_after_area(std::string_view id);

    friend struct AppAccess;
};

} // namespace loa::app
