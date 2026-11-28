// Routing tables (keymap chord → command, command id → owner, `/` name → area) and the app's
// session lifecycle, driven headless through `TestApp` (port of `loa-app/src/app/tests.rs`).
//
// The app's command table and most areas land with their own issues. Checks that need an owner
// which isn't compiled in yet are skipped (doctest `skip`, or a `MESSAGE` for a part of a case),
// so they start running the moment that owner registers.

#include <algorithm>
#include <array>
#include <memory>
#include <string>
#include <vector>

#include <doctest/doctest.h>

#include "loa/app/app.hpp"
#include "loa/app/lifecycle.hpp"
#include "loa/app/registry.hpp"
#include "loa/app/routes.hpp"
#include "loa/app_runtime/session.hpp"
#include "loa/core/patch.hpp"
#include "loa/projects/tools.hpp"
#include "loa/runtime/exec.hpp"
#include "support/test_app.hpp"

using namespace loa::app;
using loa::app::testing::FakeHarness;
using loa::app::testing::TestApp;
namespace palette = loa::ui::shell::palette;
namespace composer = loa::ui::session::composer;
namespace layout = loa::ui::shell::layout;
namespace nav = loa::view::shell::sidebar_intent;
namespace core = loa::core;

namespace {

// Whether area `id` is compiled into `loa-qt`.
bool registered(std::string_view id) {
    return std::ranges::any_of(builtin_areas(), [&](const AreaBoot& boot) { return boot.id == id; });
}

bool app_command(std::string_view id) { return find_command(app_commands(), id).has_value(); }

bool all_areas() {
    return std::ranges::all_of(AREA_ORDER, [](std::string_view id) { return registered(id); });
}

// Whether some table owns command `id`: the app's, else an area's.
bool owned(const TestApp& test, std::string_view id) {
    if (app_command(id)) return true;
    return std::ranges::any_of(test.app->areas.iter(), [&](const auto& area) {
        auto rows = area->commands(test.app->ctx);
        return std::ranges::any_of(rows, [&](const auto& row) { return row.id.as_str() == id; });
    });
}

// A sidebar event, as its row emits it.
Message sidebar(loa::view::shell::SidebarIntent event) {
    return msg::Shell{layout::msg::Emit{loa::view::shell::intent::Sidebar{std::move(event)}}};
}

// `session.new`: the app's command once its table is compiled in, else the sidebar's New
// session, which starts the same lifecycle command.
void new_session(TestApp& test) {
    if (app_command("session.new")) test.command("session.new");
    else test.send(sidebar(nav::NewSession{}));
}

std::optional<std::string> indicator(const TestApp& test) {
    auto i = test.app->ctx.composer.indicator();
    return i ? std::optional<std::string>(*i) : std::nullopt;
}

bool waiting(const App& app) {
    const auto* s = app.ctx.runtime.sessions().active();
    return s && turn_status(s->dom()) == std::optional<std::string_view>("Waiting");
}

bool another_active(const App& app, const std::string& first) {
    const auto& id = app.ctx.runtime.sessions().active_id();
    return id && id->as_str() != first;
}

palette::KeyPress chr(std::string text, palette::Modifiers m) {
    const auto latin = static_cast<char32_t>(text.at(0));
    return palette::KeyPress{std::move(text), latin, m, false};
}
palette::KeyPress named(palette::Named n, palette::Modifiers m) { return palette::KeyPress{n, std::nullopt, m, false}; }

constexpr auto CTRL = palette::Modifiers::ctrl();
constexpr auto ALT = palette::Modifiers::alt();
constexpr auto SHIFT = palette::Modifiers::shift();

} // namespace

TEST_CASE("keymap chords run app commands") {
    auto keymap = palette::Keymap::defaults(palette::Platform::Other);
    const std::vector<std::pair<palette::KeyPress, std::string_view>> table{
        {chr("k", CTRL), "palette.toggle"},
        {chr("b", CTRL), "panel.toggle"},
        {chr("a", ALT), "panel.agents"},
        {chr("n", CTRL), "session.new"},
        {chr("l", CTRL), "composer.focus"},
        {chr("/", CTRL), "keymap.show"},
        {chr("f", CTRL | SHIFT), "sidebar.search"},
        {chr("3", CTRL), "session.goto.3"},
        {chr("2", CTRL | ALT), "panel.tab.2"},
        {named(palette::Named::Tab, CTRL), "session.next"},
        {named(palette::Named::Tab, CTRL | SHIFT), "session.previous"},
        {named(palette::Named::PageDown, CTRL), "project.tab.next"},
        {named(palette::Named::Escape, {}), "session.interrupt"},
    };
    for (const auto& [event, id] : table) {
        const auto* resolved = keymap.resolve(event, palette::Scope::Global);
        REQUIRE_MESSAGE(resolved != nullptr, id);
        CHECK(resolved->as_str() == id);
        if (!app_commands().empty()) CHECK_MESSAGE(app_command(id), id << " has no command");
    }
    if (app_commands().empty()) MESSAGE("the app's command table isn't compiled in: owners not checked");
}

TEST_CASE("every listed command has an owner and the palette keeps its order" * doctest::skip(!all_areas() || app_commands().empty())) {
    TestApp test;
    test.session("hello");
    test.send(msg::Palette{palette::msg::Open{}});
    auto commands = test.app->palette.commands();
    for (const auto& command : commands)
        if (command.enabled) CHECK_MESSAGE(owned(test, command.id.as_str()), "nobody runs " << command.id.as_str());
    std::vector<std::string> session;
    for (const auto& c : commands)
        if (c.section == "Session") session.push_back(c.title);
    const std::vector<std::string> expected{"Pin session",       "Import native session…", "Fork session",
                                            "Compact session",   "Enter plan mode",        "Delete session"};
    REQUIRE(session.size() >= expected.size());
    CHECK(std::vector<std::string>(session.begin(), session.begin() + expected.size()) == expected);
    for (std::string_view id : {"panel.toggle", "tab.files", "harness.recheck", "sandbox.settings", "surfaces.browser", "models.settings"}) {
        test.app->ctx.clear_notice();
        test.command(id);
        CHECK_MESSAGE(!test.notice().starts_with("Unknown command"), id << ": " << test.notice());
    }
    test.command("nobody.here");
    CHECK(test.notice() == "Unknown command nobody.here");
}

TEST_CASE("an unknown command is reported") {
    TestApp test;
    test.command("nobody.here");
    CHECK(test.notice() == "Unknown command nobody.here");
}

TEST_CASE("slash names route to their area and list in order" * doctest::skip(app_commands().empty())) {
    const std::vector<std::pair<std::string_view, std::optional<std::string_view>>> table{
        {"/new", "session.new"}, {"/model a/b", std::nullopt}, {"/plan", std::nullopt}, {"/import", std::nullopt}, {"/nope", std::nullopt}};
    for (const auto& [text, id] : table) {
        auto found = find_slash(app_commands(), text);
        CHECK_MESSAGE((found ? std::optional<std::string_view>(found->first->id) : std::nullopt) == id, text);
    }
    if (!registered(area_id::SESSIONS)) {
        MESSAGE("the sessions area isn't compiled in: its `/` names not checked");
        return;
    }
    TestApp test;
    std::vector<std::string> names;
    for (const auto& command : test.app->ctx.slash) names.push_back(command.name);
    CHECK(names == std::vector<std::string>{"new", "model", "role", "compact", "fork", "plan", "normal", "export", "export-xml", "import"});
}

TEST_CASE("a message for the wrong area is reported, not run" * doctest::skip(!registered(area_id::TOOLS))) {
    TestApp test;
    test.send(area_message(area_id::TOOLS, to::harness::Pick{"omp"}));
    CHECK_MESSAGE(test.notice().find("The tools area can't handle") != std::string::npos, test.notice());
}

TEST_CASE("sessions run on the harness, stream their feed and survive a restart") {
    TestApp test;
    auto id = test.session("hello");
    CHECK(test.harness.seen().prompts == std::vector<std::string>{"hello"});
    CHECK(test.app->ctx.shell.sidebar().active() == std::optional<std::string_view>(id));
    test.prompt("ask");
    test.until("the approval waits", waiting);
    // Esc dismisses the focused card first (the decisions area's), then interrupts the turn.
    if (registered(area_id::DECISIONS)) {
        test.key(palette::Named::Escape);
        CHECK(test.harness.seen().interrupts == 0);
    } else {
        MESSAGE("the decisions area isn't compiled in: no card to dismiss first");
    }
    test.key(palette::Named::Escape);
    CHECK_MESSAGE(test.harness.seen().interrupts == 1, "Esc interrupts the harness");
    auto configs = test.harness.seen().configs;
    REQUIRE_FALSE(configs.empty());
    CHECK(configs.front().session_id == id);
    if (registered(area_id::PROJECTS)) {
        CHECK_MESSAGE(std::ranges::any_of(configs.front().host_tools,
                                          [](const auto& tool) { return tool.name == loa::projects::PROJECT_SEARCH; }),
                      "areas contribute host tools");
    }

    test.reopen();
    std::vector<std::string> recents;
    for (const auto* summary : test.app->ctx.shell.sidebar().recents()) recents.push_back(summary->id);
    CHECK_MESSAGE(std::ranges::find(recents, id) != recents.end(), "Recents reload the session: " << test.notice());
}

TEST_CASE("every session keeps its own unsent text") {
    TestApp test;
    auto first = test.session("hello");
    test.prompt("ask");
    test.until("the approval waits", waiting);
    test.prompt("queued");
    test.type_text("typed");
    CHECK(test.app->ctx.composer.queue().size() == 1);

    new_session(test);
    test.until("the new session is active", [&](const App& app) { return another_active(app, first); });
    auto second = *test.active();
    {
        const auto& c = test.app->ctx.composer;
        CHECK_MESSAGE(c.text().empty(), "a new session starts empty");
        CHECK(c.queue().empty());
        CHECK_FALSE(c.is_busy());
    }
    test.type_text("second");

    test.send(msg::OpenRecent{first});
    CHECK_MESSAGE(test.app->ctx.composer.text() == "queued\n\ntyped", "the queue went back into the first session's own draft");
    CHECK_MESSAGE(test.app->ctx.composer.is_busy(), "its turn still runs");
    test.send(msg::OpenRecent{second});
    CHECK(test.app->ctx.composer.text() == "second");
    test.send(msg::Composer{composer::msg::Undo{}});
    CHECK_MESSAGE(test.app->ctx.composer.text() == "second", "undo can't reach another session's text");

    if (!registered(area_id::SESSIONS)) {
        MESSAGE("the sessions area isn't compiled in: deleting sessions not checked");
        return;
    }
    test.send(area_message(area_id::SESSIONS, to::sessions::Delete{second}));
    CHECK_MESSAGE(!test.active().has_value(), "the center without a session has a draft of its own");
    CHECK(test.app->ctx.composer.text().empty());
    CHECK_MESSAGE(!test.app->sessions.drafts.contains(std::optional<SessionId>(SessionId(second))),
                  "a deleted session's draft is dropped");
    test.type_text("center");
    test.send(msg::OpenRecent{first});
    CHECK(test.app->ctx.composer.text() == "queued\n\ntyped");
    test.send(area_message(area_id::SESSIONS, to::sessions::Delete{first}));
    CHECK(test.app->ctx.composer.text() == "center");
}

TEST_CASE("what is typed while a session starts from the center stays with it") {
    TestApp test;
    test.type_text("hello");
    auto start = test.update(msg::Composer{composer::msg::Submit{}});
    CHECK_MESSAGE((test.app->ctx.composer.is_busy() && !test.active()), "the session is starting");
    test.prompt("queued");
    test.type_text("more");

    test.run(std::move(start));
    CHECK_MESSAGE(test.active().has_value(), "the session runs");
    CHECK_MESSAGE(test.app->ctx.composer.text() == "more", "the started session's composer keeps both");
    CHECK(test.app->ctx.composer.queue().size() == 1);
}

TEST_CASE("a session opened while the center starts one shows its own draft" * doctest::skip(!registered(area_id::SESSIONS))) {
    TestApp test;
    auto first = test.session("hello");
    test.type_text("first notes");
    new_session(test);
    test.until("the new session is active", [&](const App& app) { return another_active(app, first); });
    auto second = *test.active();
    test.send(area_message(area_id::SESSIONS, to::sessions::Delete{second}));
    CHECK_MESSAGE(!test.active().has_value(), "the center shows");

    test.type_text("center prompt");
    auto start = test.update(msg::Composer{composer::msg::Submit{}});
    CHECK_MESSAGE(test.app->ctx.runtime.sessions().starting().has_value(), "the session is starting");
    test.send(msg::OpenRecent{first});
    CHECK_MESSAGE(test.app->ctx.composer.text() == "first notes", "the opened session shows its own draft");
    test.type_text(" and more");

    test.run(std::move(start));
    test.until("the started session is active", [&](const App& app) { return another_active(app, first); });
    test.send(msg::OpenRecent{first});
    CHECK_MESSAGE(test.app->ctx.composer.text() == "first notes and more", "the draft survived the start");
}

TEST_CASE("the working indicator follows the shown session's turn") {
    TestApp test;
    test.type_text("hello");
    auto start = test.update(msg::Composer{composer::msg::Submit{}});
    CHECK(indicator(test) == std::optional<std::string>("Starting session…"));
    test.run(std::move(start));
    test.until("the first turn ends", [](const App& app) {
        const auto* s = app.ctx.runtime.sessions().active();
        return s && !s->is_busy();
    });
    auto first = *test.active();
    CHECK_MESSAGE(!indicator(test), "an idle session shows none");

    auto reading = [](const App& app) {
        return app.ctx.composer.indicator() == std::optional<std::string_view>("Reading the config…");
    };
    test.prompt("work read Reading the config");
    test.until("the call runs", reading);
    new_session(test);
    test.until("the new session is active", [&](const App& app) { return another_active(app, first); });
    CHECK_MESSAGE(!indicator(test), "the shown session is idle");
    test.send(msg::OpenRecent{first});
    CHECK_MESSAGE(indicator(test) == std::optional<std::string>("Reading the config…"), "switching back shows the running call");

    test.key(palette::Named::Escape);
    test.until("the turn is interrupted", [](const App& app) { return !app.ctx.composer.is_busy(); });
    CHECK_MESSAGE(!indicator(test), "an interrupted turn shows none");
    test.prompt("ask");
    test.until("the approval waits", waiting);
    CHECK_MESSAGE((test.app->ctx.composer.is_busy() && !indicator(test)), "the card asks the user instead");
}

TEST_CASE("a turn left running by a crash shows no working indicator") {
    TestApp test;
    test.prompt("work read Reading the config");
    test.until("the call runs", [](const App& app) {
        return app.ctx.composer.indicator() == std::optional<std::string_view>("Reading the config…");
    });
    auto id = *test.active();

    test.reopen();
    test.send(msg::OpenRecent{id});
    const auto* session = test.app->ctx.runtime.sessions().active();
    REQUIRE_MESSAGE(session != nullptr, "the session reopened");
    CHECK_MESSAGE(turn_status(session->dom()) == std::optional<std::string_view>("Running"), "its DOM still calls the turn running");
    CHECK(session->harness() == nullptr);
    const auto& c = test.app->ctx.composer;
    CHECK_MESSAGE((!c.is_busy() && !c.indicator()), "no harness runs it");
}

TEST_CASE("the sidebar tracks a background session while it runs and clears when it ends") {
    TestApp test;
    test.prompt("work read Reading the config");
    test.until("the call runs", [](const App& app) {
        return app.ctx.composer.indicator() == std::optional<std::string_view>("Reading the config…");
    });
    auto first = *test.active();
    const auto& sidebar_of = [&]() -> const auto& { return test.app->ctx.shell.sidebar(); };
    CHECK(sidebar_of().is_running(first));
    CHECK(sidebar_of().shows_running(first));
    CHECK(test.app->ctx.shell.session_status() == std::optional<std::string_view>("Running"));

    // Switch to a new idle session: the background session stays running in the sidebar while
    // the top bar tracks the active idle session.
    new_session(test);
    test.until("the second session is active", [&](const App& app) { return another_active(app, first); });
    auto second = *test.active();
    CHECK_MESSAGE(sidebar_of().is_running(first), "background session stays marked running");
    CHECK(sidebar_of().shows_running(first));
    CHECK_MESSAGE(!sidebar_of().is_running(second), "idle active session is not running");
    CHECK_MESSAGE(!test.app->ctx.shell.session_status(), "top bar follows the idle active session");

    // End the background turn with Error: the sidebar clears without disturbing the active top bar.
    const auto* session = test.app->ctx.runtime.sessions().get(first);
    REQUIRE_MESSAGE(session != nullptr, "first still open");
    auto writer = session->writer();
    const auto root = session->dom().root()->id;
    auto turns = loa::app_runtime::dom_turns(session->dom());
    REQUIRE_FALSE(turns.empty());
    const auto turn = turns.back()->id;
    REQUIRE(writer->commit(core::Draft::set(turn, std::string(core::attr::STATUS), core::TurnStatus::Error)).has_value());
    test.until("the background turn ends", [&](const App& app) { return !app.ctx.shell.sidebar().is_running(first); });
    CHECK_FALSE(sidebar_of().shows_running(first));
    CHECK_MESSAGE(!test.app->ctx.shell.session_status(), "background status never overwrites the active top bar");
    CHECK(test.active() == second);

    // A background turn starting again marks the sidebar running without changing the active top bar.
    auto inserted = writer->commit(core::Draft::insert(
        root, core::ElementKind::Turn, {{std::string(core::attr::STATUS), core::Value(core::TurnStatus::Running)}}, ""));
    REQUIRE(inserted.has_value());
    const auto next_turn = inserted->node;
    test.until("the background turn starts", [&](const App& app) { return app.ctx.shell.sidebar().shows_running(first); });
    CHECK_MESSAGE(!test.app->ctx.shell.session_status(), "background running turn leaves active top bar alone");

    // A waiting decision in the background takes precedence over the running indicator.
    REQUIRE(writer
                ->commit(core::Draft::insert(next_turn, core::ElementKind::Decision,
                                             {{std::string(core::attr::KIND), core::Value(core::DecisionKind::Approval)},
                                              {std::string(core::attr::STATUS), core::Value(core::DecisionStatus::Waiting)}},
                                             ""))
                .has_value());
    REQUIRE(writer->commit(core::Draft::set(next_turn, std::string(core::attr::STATUS), core::TurnStatus::Waiting)).has_value());
    test.until("the background decision waits", [&](const App& app) {
        const auto* s = app.ctx.shell.sidebar().session(first);
        return s && s->waiting;
    });
    CHECK(sidebar_of().is_running(first));
    CHECK_MESSAGE(!sidebar_of().shows_running(first), "waiting wins over running");
    CHECK_FALSE(test.app->ctx.shell.session_status());

    if (!registered(area_id::SESSIONS)) {
        MESSAGE("the sessions area isn't compiled in: deleting sessions not checked");
        return;
    }
    // Deleting the background session clears its sidebar running state.
    test.send(area_message(area_id::SESSIONS, to::sessions::Delete{first}));
    CHECK_FALSE(sidebar_of().is_running(first));
    CHECK_FALSE(sidebar_of().shows_running(first));
}

TEST_CASE("a stale crash DOM with no harness is never running in the sidebar") {
    TestApp test;
    test.prompt("work read Reading the config");
    test.until("the call runs", [](const App& app) {
        return app.ctx.composer.indicator() == std::optional<std::string_view>("Reading the config…");
    });
    auto id = *test.active();
    CHECK(test.app->ctx.shell.sidebar().is_running(id));
    CHECK(test.app->ctx.shell.sidebar().shows_running(id));

    test.reopen();
    CHECK_MESSAGE(test.app->ctx.shell.sidebar().session(id) != nullptr, "reopened recents list the crashed session");
    CHECK_MESSAGE(!test.app->ctx.shell.sidebar().is_running(id), "disk recents are never marked running");
    CHECK_FALSE(test.app->ctx.shell.sidebar().shows_running(id));

    test.send(msg::OpenRecent{id});
    const auto* session = test.app->ctx.runtime.sessions().active();
    REQUIRE_MESSAGE(session != nullptr, "the session reopened");
    CHECK_MESSAGE((session->is_busy() && session->harness() == nullptr), "stale DOM is busy with no harness");
    CHECK_MESSAGE(!session_running(test.app->ctx.runtime.sessions(), session->id()), "no harness or start means not running");
    CHECK_MESSAGE(!test.app->ctx.shell.sidebar().is_running(id), "reopening a stale crash DOM does not mark it running");
    CHECK_FALSE(test.app->ctx.shell.sidebar().shows_running(id));

    // Peer sessions are never in the runtime's sessions and never running.
    CHECK_FALSE(session_running(test.app->ctx.runtime.sessions(), SessionId("peer:node:remote")));

    // While this busy session is starting or resuming, it counts as running until starting clears.
    SessionId session_id(id);
    test.app->ctx.runtime.sessions_mut().set_starting(session_id);
    CHECK(session_running(test.app->ctx.runtime.sessions(), session_id));
    test.app->ctx.runtime.sessions_mut().set_starting(std::nullopt);
    CHECK_FALSE(session_running(test.app->ctx.runtime.sessions(), session_id));
}

TEST_CASE("shell events open the overlays and the folder dialog") {
    TestApp test;
    test.send(sidebar(nav::OpenSettings{}));
    REQUIRE(test.app->windows.overlay.has_value());
    CHECK(std::get<overlay::Settings>(*test.app->windows.overlay).tab == SettingsTab::General);
    test.send(sidebar(nav::OpenConfigs{}));
    REQUIRE(test.app->windows.overlay.has_value());
    CHECK(std::get<overlay::Configs>(*test.app->windows.overlay).tab == ConfigsTab::Models);
    test.key(palette::Named::Escape);
    CHECK_MESSAGE(!test.app->windows.overlay.has_value(), "Esc closes Configs");
    test.run(test.app->browse());
    CHECK(test.notice() == "no folder dialog in tests");
}

TEST_CASE("usage reads its source and spends a saved reset") {
    TestApp test;
    test.send(sidebar(nav::OpenUsage{}));
    REQUIRE(test.app->windows.overlay.has_value());
    CHECK(std::holds_alternative<overlay::Usage>(*test.app->windows.overlay));
    test.send(msg::Usage{usage::msg::Toggle{"anthropic"}});
    // What the "Use a reset" button sends.
    test.send(msg::Usage{usage::msg::Spend{"anthropic/c1"}});
    CHECK(test.usage->spent() == std::vector<std::string>{"anthropic/c1"});
    test.send(msg::Usage{usage::msg::Close{}});
    CHECK_FALSE(test.app->windows.overlay.has_value());
}

TEST_CASE("the backends run the configured harness executables") {
    auto bin = loa::runtime::TempDir::create();
    REQUIRE(bin.has_value());
    auto omp = bin->path() / "my-omp";
    REQUIRE(loa::runtime::exec::write_executable(omp, "#!/bin/sh\n").has_value());
    // On Windows the script lands next to a launcher; the harness runs whichever `resolve` finds.
    omp = loa::runtime::exec::resolve(omp);
    TestApp test(std::make_shared<loa::runtime::Executor>(2), FakeHarness("fake"), "testhost",
                 [&](loa::settings::app::Config& config) { config.harness.executables["omp"] = omp; });
    auto parts = machine_parts(test.app->ctx.services, test.app->ctx.runtime.library());
    // omp runs the model screens' RPC and the project digests.
    const auto* omp_harness = parts.runtime.harnesses.get("omp");
    REQUIRE_MESSAGE(omp_harness != nullptr, "omp is registered");
    CHECK(omp_harness->configured == std::optional(omp));
    CHECK(omp_harness->executable == std::optional(omp));
}
