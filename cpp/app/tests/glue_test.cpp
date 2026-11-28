// The app modules around the core shell: `omp usage --json` parsing and the Usage window's
// state (`app/usage.rs`), the right panel pane registry (`app/panes.rs`) and the app's own
// commands (`app/keys.rs`).

#include <algorithm>

#include <doctest/doctest.h>

#include "loa/app/panes.hpp"
#include "loa/app/usage_window.hpp"
#include <fstream>
#include <sstream>

#include "support/test_app.hpp"

using namespace loa::app;
using loa::app::testing::TestApp;
namespace palette = loa::ui::shell::palette;
namespace fs = std::filesystem;

namespace {

std::string read_file(const std::filesystem::path& path) {
    std::ifstream in(path, std::ios::binary);
    std::stringstream text;
    text << in.rdbuf();
    return text.str();
}

std::vector<std::string> titles(const App& app) {
    std::vector<std::string> out;
    for (const auto& command : app.palette.commands()) out.push_back(command.title);
    return out;
}

const palette::Command* find_row(const App& app, std::string_view id) {
    const auto& commands = app.palette.commands();
    auto it = std::ranges::find_if(commands, [&](const auto& c) { return c.id.as_str() == id; });
    return it == commands.end() ? nullptr : &*it;
}

} // namespace

// ---- usage -------------------------------------------------------------------------------------

TEST_CASE("usage: groups accounts by provider and reads limits (golden)") {
    auto parsed = usage::parse(read_file(fs::path(LOA_GOLDEN_DIR) / "app/usage.json"));
    REQUIRE(parsed.has_value());
    const auto& u = *parsed;
    CHECK(u.generated_at == 1000);
    REQUIRE(u.providers.size() == 2);
    CHECK(u.providers[0].name == "Anthropic");
    CHECK(u.providers[1].name == "OpenAI Codex");
    const auto& anthropic = u.providers[0];
    REQUIRE(anthropic.accounts.size() == 2);
    CHECK(anthropic.accounts[0].label == "Personal");
    CHECK(anthropic.accounts[0].reset_target == std::optional<std::string>("anthropic/c1"));
    CHECK(anthropic.accounts[0].resets == std::pair<std::uint64_t, std::uint64_t>{3, 1});
    CHECK(anthropic.accounts[0].limits[0] == usage::Limit{"Claude 5 Hour", "5h", 0.03F, 5000});
    CHECK(anthropic.accounts[1].label == "work@example.com");
    CHECK_FALSE(anthropic.accounts[1].reset_target.has_value());
    CHECK_FALSE(anthropic.accounts[1].limits[0].fraction.has_value());
    CHECK_FALSE(anthropic.accounts[1].limits[0].resets_at.has_value());
    CHECK(anthropic.capacity == std::optional<std::string>("7d 0.70 of 2 used"));
    CHECK(anthropic.peak() == std::optional<float>(0.51F));
    CHECK(u.providers[1].accounts[0].resets == std::pair<std::uint64_t, std::uint64_t>{0, 0});
    CHECK_FALSE(u.providers[1].capacity.has_value());
    CHECK(u.meter() == std::optional<loa::view::shell::UsageMeter>(loa::view::shell::UsageMeter{0.51F, "7d"}));
}

TEST_CASE("usage: names, defaults and invalid JSON") {
    auto parsed = usage::parse(R"({"reports":[{"provider":"mistral","limits":[{}]},{}],
        "capacity":{"mistral":[{"window":"5h","meter":"tokens","accounts":3,"usedAccounts":1.234}]}})");
    REQUIRE(parsed.has_value());
    REQUIRE(parsed->providers.size() == 2);
    const auto& p = parsed->providers[0];
    CHECK(p.name == "Mistral");
    CHECK(p.accounts[0].label == "Account");
    CHECK(p.accounts[0].limits[0] == usage::Limit{"Limit", "", std::nullopt, std::nullopt});
    CHECK(p.capacity == std::optional<std::string>("5h tokens 1.23 of 3 used"));
    CHECK(parsed->providers[1].id == "unknown");
    CHECK(parsed->generated_at == 0);
    CHECK_FALSE(parsed->meter().has_value());
    auto broken = usage::parse("not json");
    REQUIRE_FALSE(broken.has_value());
    CHECK(broken.error().starts_with("omp usage printed invalid JSON: "));
}

TEST_CASE("usage: the window's state") {
    usage::State state(nullptr);
    auto [task, event] = state.update(usage::msg::Loaded{read_file(fs::path(LOA_GOLDEN_DIR) / "app/usage.json")});
    CHECK(task.empty());
    REQUIRE(event.has_value());
    const auto* meter = std::get_if<usage::event::Meter>(&*event);
    REQUIRE(meter != nullptr);
    CHECK(meter->meter == std::optional<loa::view::shell::UsageMeter>(loa::view::shell::UsageMeter{0.51F, "7d"}));
    REQUIRE(state.usage().has_value());
    CHECK(state.usage()->has_value());

    CHECK_FALSE(state.is_open("anthropic"));
    (void)state.update(usage::msg::Toggle{"anthropic"});
    CHECK(state.is_open("anthropic"));
    (void)state.update(usage::msg::Toggle{"anthropic"});
    CHECK_FALSE(state.is_open("anthropic"));

    auto spend = state.update(usage::msg::Spend{"anthropic/c1"});
    CHECK(spend.first.jobs.size() == 1);
    CHECK(state.spending() == std::optional<std::string>("anthropic/c1"));
    auto spent = state.update(usage::msg::Spent{std::unexpected(std::string("no resets left"))});
    CHECK_FALSE(state.spending().has_value());
    CHECK(state.loading());
    CHECK(spent.first.jobs.size() == 1);
    REQUIRE(state.usage().has_value());
    CHECK(state.usage()->error() == "no resets left");

    auto failed = state.update(usage::msg::Loaded{std::unexpected(std::string("Couldn't run omp usage"))});
    CHECK_FALSE(state.loading());
    REQUIRE(failed.second.has_value());
    CHECK_FALSE(std::get<usage::event::Meter>(*failed.second).meter.has_value());

    auto close = state.update(usage::msg::Close{});
    REQUIRE(close.second.has_value());
    CHECK(std::holds_alternative<usage::event::Close>(*close.second));
}

TEST_CASE("usage: a read sets the sidebar meter and Close leaves the window") {
    TestApp test;
    auto& app = *test.app;
    app.windows.overlay = overlay::Usage{};
    test.send(msg::Usage{usage::msg::Loaded{read_file(fs::path(LOA_GOLDEN_DIR) / "app/usage.json")}});
    CHECK(app.ctx.shell.sidebar().usage() == std::optional<loa::view::shell::UsageMeter>(loa::view::shell::UsageMeter{0.51F, "7d"}));
    test.send(msg::Usage{usage::msg::Close{}});
    CHECK_FALSE(app.windows.overlay.has_value());
}

// ---- panes -------------------------------------------------------------------------------------

TEST_CASE("panes: the registry holds diff, files and terminal in tab order") {
    TestApp test;
    const auto& app = *test.app;
    std::vector<std::string> ids;
    for (const auto& pane : panes::REGISTRY) {
        auto tab = pane.tab(app);
        CHECK(tab.shown);
        ids.push_back(tab.id);
    }
    CHECK(ids == std::vector<std::string>{"diff", "files", "terminal"});
    REQUIRE(panes::find(app, "files") != nullptr);
    CHECK(panes::find(app, "files")->kind == panes::Kind::Files);
    CHECK(panes::find(app, "terminal")->kind == panes::Kind::Terminal);
    CHECK(panes::find(app, "tools") == nullptr);
    const auto tabs = app.tabs();
    REQUIRE(tabs.size() >= 3);
    CHECK(tabs[0].title == "Diff");
    CHECK(tabs[1].title == "Files");
    CHECK(tabs[2].title == "Terminal");
}

// ---- keys --------------------------------------------------------------------------------------

TEST_CASE("keys: command ids route to their command with the argument") {
    auto routed = [](std::string_view id) -> std::pair<std::string, std::string> {
        auto found = find_command(app_commands(), id);
        if (!found) return {};
        return {std::string(found->first->id), std::string(found->second)};
    };
    CHECK(routed("session.new") == std::pair<std::string, std::string>{"session.new", ""});
    CHECK(routed("theme.dark") == std::pair<std::string, std::string>{"theme.", "dark"});
    CHECK(routed("tab.files") == std::pair<std::string, std::string>{"tab.", "files"});
    CHECK(routed("session.goto.3") == std::pair<std::string, std::string>{"session.goto.", "3"});
    CHECK(routed("panel.tab.2") == std::pair<std::string, std::string>{"panel.tab.", "2"});
    CHECK(routed("composer.model.0") == std::pair<std::string, std::string>{"composer.model.", "0"});
    CHECK(routed("workflows.new").first.empty());
    auto slash = find_slash(app_commands(), "/new");
    REQUIRE(slash.has_value());
    CHECK(slash->first->id == "session.new");
}

TEST_CASE("keys: panel and view commands") {
    TestApp test;
    auto& app = *test.app;
    const bool open = app.ctx.shell.panel_open();
    test.command("panel.toggle");
    CHECK(app.ctx.shell.panel_open() == !open);
    test.command("tab.files");
    CHECK(app.ctx.shell.panel_open());
    CHECK(app.ctx.shell.active_tab() == std::optional<std::string_view>("files"));
    test.command("panel.tab.3");
    CHECK(app.ctx.shell.active_tab() == std::optional<std::string_view>("terminal"));
    test.command("panel.tab.9");
    CHECK(test.notice() == "There is no panel tab 9");
    test.command("panel.tab.x");
    CHECK(test.notice() == "Unknown command panel.tab.x");
    test.command("theme.nope");
    CHECK(test.notice() == "Unknown command theme.nope");
    test.command("session.next");
    CHECK(test.notice() == "No sessions yet");
    test.command("session.goto.1");
    CHECK(test.notice() == "There is no session 1 in the sidebar");
    test.command("composer.focus");
    CHECK(test.notice() == "Start or open a session to type a prompt");
    test.command("sidebar.pin");
    CHECK(test.notice() == "Open a session to pin it");
    test.command("composer.model.99");
    CHECK(test.notice() == "Unknown command composer.model.99");
    test.command("project.tabs.toggle");
    CHECK(app.ctx.shell.project_tabs());
    auto focus = app.command(palette::CommandId("focus.previous"));
    REQUIRE(focus.ops.size() == 1);
    CHECK(std::get<ui_op::FocusNext>(focus.ops[0]).forward == false);
    test.command("palette.toggle");
    CHECK(app.palette.is_open());
}

TEST_CASE("keys: the palette lists app commands, reference keys and shortcut hints") {
    TestApp test;
    auto& app = *test.app;
    test.send(msg::Palette{palette::msg::Open{}});
    const auto listed = titles(app);
    for (const auto* title : {"New session", "Toggle right panel", "Open Agents tab", "Show project tabs", "Open Files tab",
                              "Open Terminal tab", "Keyboard shortcuts", "Show tools", "Show peers"})
        CHECK_MESSAGE(std::ranges::find(listed, title) != listed.end(), title);
    // Rows that need a session stay out without one; the panel tab command is run by key only.
    CHECK(std::ranges::none_of(listed, [](const auto& t) { return t.starts_with("Mode: ") || t.starts_with("Go to session: "); }));
    CHECK(find_row(app, "panel.tab.") == nullptr);
    CHECK(std::ranges::none_of(listed, [](const auto& t) { return t == "Open agents tab"; }));
    CHECK(std::ranges::any_of(listed, [](const auto& t) { return t.starts_with("Theme: "); }));

    const auto* close = find_row(app, "keys.close");
    REQUIRE(close != nullptr);
    CHECK_FALSE(close->enabled);
    CHECK(close->section == "Keyboard shortcuts");
    CHECK(close->keys.has_value());
    const auto* session_goto = find_row(app, "keys.session.goto");
    REQUIRE(session_goto != nullptr);
    CHECK(session_goto->keys->ends_with("–9"));

    // `Open X tab` takes the hint of `panel.tab.<n>` by the tab's place in the panel.
    const auto& shown = app.ctx.shell.tabs();
    auto at = std::ranges::find_if(shown, [](const auto& t) { return t.id == "files"; });
    REQUIRE(at != shown.end());
    const auto* files = find_row(app, "tab.files");
    REQUIRE(files != nullptr);
    CHECK(files->shortcut == app.ctx.keymap.chord_for(palette::ids::panel_tab(static_cast<std::size_t>(at - shown.begin()) + 1)));
    const auto* toggle = find_row(app, "palette.toggle");
    REQUIRE(toggle != nullptr);
    CHECK(toggle->shortcut == app.ctx.keymap.chord_for(palette::CommandId("palette.toggle")));
}
