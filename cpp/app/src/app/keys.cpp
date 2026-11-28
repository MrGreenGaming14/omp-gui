// The app's own commands (#78, #82): session and view actions, panel tabs, themes, the
// shortcuts the keymap binds and a palette command for every sidebar and composer control, so
// nothing needs a pointer (port of `loa-app/src/app/keys.rs`).

#include <algorithm>
#include <array>
#include <charconv>
#include <string>

#include "loa/app/app.hpp"
#include "loa/app/routes.hpp"
#include "loa/core/schema.hpp"

namespace loa::app {

namespace palette = ui::shell::palette;
namespace ids = ui::shell::palette::ids;
namespace composer = ui::session::composer;
namespace layout = ui::shell::layout;
namespace sidebar = ui::shell::sidebar;

namespace {

// Section of the rows that list keys which aren't commands.
constexpr std::string_view KEYS_SECTION = "Keyboard shortcuts";
// Prefix of the `Open X tab` commands; the rest is the tab id.
constexpr std::string_view TAB_COMMAND = "tab.";

using Rows = std::vector<std::pair<std::string, std::string>>;

// `text` as a whole decimal number.
std::optional<std::size_t> parse_index(std::string_view text) {
    std::size_t n = 0;
    const auto* end = text.data() + text.size();
    auto [ptr, ec] = std::from_chars(text.data(), end, n);
    if (ec != std::errc{} || ptr != end || text.empty()) return std::nullopt;
    return n;
}

constexpr AppCommand command(std::string_view id, std::string_view title, std::string_view section, Task (*run)(App&, std::string_view)) {
    return AppCommand{id, title, section, std::nullopt, Listed<App>::always(), run};
}

} // namespace

// The app's private calls the command table makes.
struct AppKeys {
    static Task start(App& app) {
        return app.lifecycle([](SessionController& s, Ctx& ctx, Registry& areas) {
            return s.run(ctx, areas, app_runtime::command::Start{std::nullopt});
        });
    }
    static Task open_tab(App& app, std::string_view tab) { return app.open_tab(tab); }
};

namespace {

const auto APP_COMMANDS = std::to_array<AppCommand>({
    command("session.new", "New session", "Commands", [](App& app, std::string_view) { return AppKeys::start(app); })
        .with_slash("new", "Start a new session"),
    command("panel.toggle", "Toggle right panel", "Commands",
            [](App& app, std::string_view) {
                app.ctx.shell.set_panel_open(!app.ctx.shell.panel_open());
                return Task::none();
            }),
    command("panel.agents", "Open Agents tab", "Commands", [](App& app, std::string_view) { return AppKeys::open_tab(app, "agents"); }),
    command("session.interrupt", "Interrupt session", "Commands",
            [](App& app, std::string_view) { return app.update(msg::Composer{composer::msg::Interrupt{}}); }),
    command("project.tabs.toggle", "", "Commands",
            [](App& app, std::string_view) {
                const bool enabled = !app.ctx.shell.project_tabs();
                app.ctx.shell.set_project_tabs(enabled);
                app.ctx.edit_config("settings", [&](settings::app::Config& config) { config.project_tabs = enabled; });
                return Task::none();
            })
        .with_listed(Listed<App>::when_([](const App& app, const Ctx&) -> std::optional<std::string_view> {
            return app.ctx.shell.project_tabs() ? "Hide project tabs" : "Show project tabs";
        })),
    command("palette.toggle", "Command palette", "Commands",
            [](App& app, std::string_view) { return app.update(msg::Palette{palette::msg::Toggle{}}); }),
    command(TAB_COMMAND, "", "Commands", [](App& app, std::string_view tab) { return AppKeys::open_tab(app, tab); })
        .with_listed(Listed<App>::each_([](const App& app, const Ctx&) {
            Rows rows;
            for (auto& tab : app.tabs())
                if (tab.id != "agents") rows.emplace_back(tab.id, "Open " + tab.title + " tab");
            return rows;
        })),
    command("theme.", "", "Theme",
            [](App& app, std::string_view theme) {
                const auto themes = app.ctx.shell.theme_state().themes();
                if (std::ranges::none_of(themes, [&](const auto& entry) { return entry.first == theme; }))
                    return app.unknown("theme.", theme);
                return app.update(msg::Shell{layout::msg::SetTheme{loa::view::theme::ThemeRef{std::string(theme)}}});
            })
        .with_listed(Listed<App>::each_([](const App& app, const Ctx&) {
            Rows rows;
            for (auto& [key, label] : app.ctx.shell.theme_state().themes()) rows.emplace_back(key, "Theme: " + label);
            return rows;
        })),
    command("session.next", "Next session", "Commands", [](App& app, std::string_view) { return app.step_session(true); }),
    command("session.previous", "Previous session", "Commands", [](App& app, std::string_view) { return app.step_session(false); }),
    command("project.tab.next", "Next project tab", "Commands", [](App& app, std::string_view) { return app.step_project_tab(true); }),
    command("project.tab.previous", "Previous project tab", "Commands",
            [](App& app, std::string_view) { return app.step_project_tab(false); }),
    command("composer.focus", "Focus the composer", "Commands", [](App& app, std::string_view) { return app.focus_composer(); }),
    command("sidebar.search", "Search sessions", "Commands",
            [](App&, std::string_view) {
                return Task::pane(ui_op::Pane::Target::Shell, ui::shell::Task{{ui::shell::op::Focus{std::string(sidebar::SEARCH_ID)}}});
            }),
    command("focus.next", "Focus the next field", "Commands", [](App&, std::string_view) { return Task::op(ui_op::FocusNext{true}); }),
    command("focus.previous", "Focus the previous field", "Commands",
            [](App&, std::string_view) { return Task::op(ui_op::FocusNext{false}); }),
    command("keymap.show", "Keyboard shortcuts", "Commands",
            [](App& app, std::string_view) { return app.update(msg::Palette{palette::msg::OpenShortcuts{}}); }),
    // The sidebar's Tools button and Peers rows.
    command("sidebar.tools", "Show tools", "Commands",
            [](App& app, std::string_view) { return app.update(area_message(area_id::TOOLS, to::tools::Open{})); }),
    command("sidebar.peers", "Show peers", "Commands",
            [](App& app, std::string_view) { return app.update(area_message(area_id::PEERS, to::peers::Open{})); }),
    command(ids::SESSION_GOTO, "", "Sessions",
            [](App& app, std::string_view n) {
                auto index = parse_index(n);
                if (!index) return app.unknown(ids::SESSION_GOTO, n);
                auto order = app.session_order();
                if (*index == 0 || *index > order.size()) {
                    app.ctx.notify("There is no session " + std::string(n) + " in the sidebar");
                    return Task::none();
                }
                return app.show_session(std::move(order[*index - 1]));
            })
        .with_listed(Listed<App>::each_([](const App& app, const Ctx&) {
            const auto& bar = app.ctx.shell.sidebar();
            Rows rows;
            auto order = app.session_order();
            for (std::size_t i = 0; i < order.size(); ++i) {
                const auto* summary = bar.session(order[i]);
                rows.emplace_back(std::to_string(i + 1), "Go to session: " + (summary ? summary->title : order[i]));
            }
            return rows;
        })),
    // The hover-only Pin / Unpin button of the active session's sidebar row.
    command("sidebar.pin", "", "Session",
            [](App& app, std::string_view) {
                const auto& active = app.ctx.runtime.sessions().active_id();
                if (!active) {
                    app.ctx.notify("Open a session to pin it");
                    return Task::none();
                }
                return app.update(msg::Shell{layout::msg::Sidebar{sidebar::msg::TogglePin{active->str()}}});
            })
        .with_listed(Listed<App>::when_([](const App& app, const Ctx& ctx) -> std::optional<std::string_view> {
            const auto& active = ctx.runtime.sessions().active_id();
            if (!active) return std::nullopt;
            const auto* summary = app.ctx.shell.sidebar().session(active->as_str());
            if (!summary) return std::nullopt;
            return summary->pinned ? "Unpin session" : "Pin session";
        })),
    // The composer's pickers: the mode, the effort, the model by its index.
    command("composer.mode.", "", "Composer",
            [](App& app, std::string_view mode) { return app.pick(composer::PickerId::Mode, "composer.mode.", mode); })
        .with_listed(Listed<App>::each_([](const App&, const Ctx& ctx) {
            Rows rows;
            if (!ctx.runtime.sessions().active()) return rows;
            for (auto mode : core::StrEnum<core::SessionMode>::values) rows.emplace_back(mode, "Mode: " + std::string(mode));
            return rows;
        })),
    command("composer.effort.", "", "Composer",
            [](App& app, std::string_view effort) { return app.pick(composer::PickerId::Effort, "composer.effort.", effort); })
        .with_listed(Listed<App>::each_([](const App&, const Ctx& ctx) {
            Rows rows;
            if (!ctx.runtime.sessions().active()) return rows;
            for (auto& effort : ctx.composer.efforts().first) rows.emplace_back(effort, "Effort: " + effort);
            return rows;
        })),
    command("composer.model.", "", "Composer",
            [](App& app, std::string_view index) {
                auto n = parse_index(index);
                if (!n || *n >= app.ctx.composer.models().first.size()) return app.unknown("composer.model.", index);
                return app.update(msg::Composer{composer::msg::Pick{composer::PickerId::Model, *n}});
            })
        .with_listed(Listed<App>::each_([](const App&, const Ctx& ctx) {
            Rows rows;
            if (!ctx.runtime.sessions().active()) return rows;
            const auto& models = ctx.composer.models().first;
            for (std::size_t i = 0; i < models.size(); ++i) rows.emplace_back(std::to_string(i), "Model: " + models[i].to_string());
            return rows;
        })),
    // Ctrl/Cmd+Alt+1…9: the n-th tab in the panel.
    command(ids::PANEL_TAB, "", "Commands",
            [](App& app, std::string_view n) {
                auto index = parse_index(n);
                if (!index) return app.unknown(ids::PANEL_TAB, n);
                const auto& tabs = app.ctx.shell.tabs();
                if (*index == 0 || *index > tabs.size()) {
                    app.ctx.notify("There is no panel tab " + std::string(n));
                    return Task::none();
                }
                return AppKeys::open_tab(app, std::string(tabs[*index - 1].id));
            })
        .with_listed(Listed<App>::never()),
});

} // namespace

std::span<const AppCommand> app_commands() { return APP_COMMANDS; }

std::vector<palette::Command> App::reference_keys() const {
    using palette::Chord;
    using palette::Modifiers;
    using palette::Named;
    const auto platform = ctx.keymap.platform();
    const auto primary = palette::primary(platform);
    const auto none = Modifiers::none();
    auto label = [&](Named key, Modifiers modifiers) { return Chord::named(key, modifiers).label(platform); };
    auto hint = [&](std::string_view id, std::string fallback) {
        auto chord = ctx.keymap.chord_for(palette::CommandId(id));
        return chord ? chord->label(platform) : fallback;
    };
    const auto escape = label(Named::Escape, none), enter = label(Named::Enter, none), tab = label(Named::Tab, none);
    const auto up = label(Named::ArrowUp, none), down = label(Named::ArrowDown, none);
    auto range = [&](Modifiers modifiers) { return Chord::character(U'1', modifiers).label(platform) + "–9"; };
    const std::array<std::array<std::string, 3>, 10> rows{{
        {"close", "Close the palette or a screen, dismiss the focused prompt", escape},
        {"prompt.pick", "Prompt card: pick option 1 to 9", "1–9"},
        {"prompt.confirm", "Prompt card: confirm the pick", enter},
        {"composer.send", "Composer: send", hint(ids::COMPOSER_SUBMIT, enter)},
        {"composer.newline", "Composer: new line", hint(ids::COMPOSER_NEWLINE, label(Named::Enter, Modifiers::shift()))},
        {"composer.undo", "Composer: undo, redo",
         Chord::character(U'z', primary).label(platform) + " " + Chord::character(U'z', primary | Modifiers::shift()).label(platform)},
        {"composer.popup", "Composer: move in the / and @ lists, accept", up + " " + down + " " + tab},
        {"palette", "Palette: move, run, close", up + " " + down + " " + enter + " " + escape},
        {"session.goto", "Go to session 1 to 9 in the sidebar", range(primary)},
        {"panel.tab", "Show right panel tab 1 to 9", range(primary | Modifiers::alt())},
    }};
    std::vector<palette::Command> out;
    out.reserve(rows.size());
    for (const auto& [id, title, keys] : rows)
        out.push_back(palette::Command(palette::CommandId("keys." + id), title, std::string(KEYS_SECTION)).with_keys(keys).with_enabled(false));
    return out;
}

void App::hint_commands(std::vector<palette::Command>& commands) const {
    const auto& shown = ctx.shell.tabs();
    for (auto& command : commands) {
        if (command.shortcut || command.keys) continue;
        const auto& id = command.id.as_str();
        std::optional<palette::CommandId> bound;
        if (std::string_view(id).starts_with(TAB_COMMAND)) {
            const auto tab = std::string_view(id).substr(TAB_COMMAND.size());
            auto it = std::ranges::find_if(shown, [&](const auto& listed) { return listed.id == tab; });
            if (it != shown.end()) bound = ids::panel_tab(static_cast<std::size_t>(it - shown.begin()) + 1);
        } else {
            bound = command.id;
        }
        if (bound) command.shortcut = ctx.keymap.chord_for(*bound);
    }
}

Task App::unknown(std::string_view prefix, std::string_view arg) {
    ctx.notify("Unknown command " + std::string(prefix) + std::string(arg));
    return Task::none();
}

Task App::pick(composer::PickerId picker, std::string_view prefix, std::string_view key) {
    if (auto message = ctx.composer.pick_key(picker, key)) return update(msg::Composer{std::move(*message)});
    return unknown(prefix, key);
}

std::vector<std::string> App::session_order() const {
    const auto& bar = ctx.shell.sidebar();
    std::vector<std::string> order;
    for (const auto* summary : bar.pinned()) order.push_back(summary->id);
    for (const auto* summary : bar.recents()) order.push_back(summary->id);
    return order;
}

Task App::step_session(bool forward) {
    auto order = session_order();
    if (order.empty()) {
        ctx.notify("No sessions yet");
        return Task::none();
    }
    std::optional<std::size_t> current;
    if (const auto& active = ctx.shell.sidebar().active())
        if (auto it = std::ranges::find(order, *active); it != order.end()) current = static_cast<std::size_t>(it - order.begin());
    const auto len = order.size();
    std::size_t index = 0;
    if (current) index = forward ? (*current + 1) % len : (*current + len - 1) % len;
    else index = forward ? 0 : len - 1;
    return show_session(std::move(order[index]));
}

Task App::show_session(std::string session) {
    return update(msg::Shell{layout::msg::Sidebar{sidebar::msg::OpenSession{std::move(session)}}});
}

Task App::focus_composer() {
    if (ctx.screen()) ctx.close_screen();
    if (!ctx.runtime.sessions().active()) {
        ctx.notify("Start or open a session to type a prompt");
        return Task::none();
    }
    return Task::composer(composer::Task::focus());
}

} // namespace loa::app
