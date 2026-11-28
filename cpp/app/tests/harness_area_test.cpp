// The harness area (port of `loa-app/src/areas/harness/tests.rs`): switching from the palette,
// rechecking availability, and switching the harness of an empty session.

#include <algorithm>

#include <doctest/doctest.h>

#include "areas/harness/harness.hpp"
#include "support/test_app.hpp"

using namespace loa::app;
using loa::app::testing::FakeHarness;
using loa::app::testing::TestApp;
namespace h = loa::app::areas::harness;

namespace {

bool has_title(TestApp& test, std::string_view title) {
    auto titles = test.palette_titles();
    return std::ranges::find(titles, title) != titles.end();
}

void add_other(TestApp& test, FakeHarness harness) {
    harness::add(test.app->ctx.runtime.harnesses_mut(), std::make_shared<FakeHarness>(std::move(harness)), "Other", true,
                 std::nullopt, std::nullopt);
}

std::optional<std::string> active_harness(const App& app) {
    const auto* active = app.ctx.runtime.sessions().active();
    if (!active) return std::nullopt;
    auto id = active->harness_id();
    return id ? std::optional<std::string>(*id) : std::nullopt;
}

} // namespace

TEST_CASE("harness: switches from the palette and rechecks in its tab") {
    TestApp test;
    CHECK(has_title(test, "Switch harness: fake"));
    test.command("harness.fake");
    CHECK(test.app->ctx.runtime.sessions().harness == std::optional<std::string>("fake"));
    test.command("harness.recheck");
    const auto& tabs = test.app->ctx.shell.tabs();
    CHECK_MESSAGE(std::ranges::any_of(tabs, [](const auto& tab) { return tab.id == h::TAB; }), "Recheck opens the Harnesses tab");
    test.until("the probe answered", [](const App& app) {
        const auto* harness = app.ctx.runtime.harnesses().get("fake");
        return harness && harness->available && harness->availability.has_value();
    });
    CHECK(test.area<h::State>().probing().empty());
}

TEST_CASE("harness: an unavailable harness is refused") {
    TestApp test;
    harness::add(test.app->ctx.runtime.harnesses_mut(), std::make_shared<FakeHarness>(FakeHarness("other")), "Other", false,
                 std::string("not installed"), std::nullopt);
    CHECK_FALSE(has_title(test, "Switch harness: Other"));
    test.send(h::State::message(h::msg::Pick{"other"}));
    CHECK(test.notice() == "Other is unavailable");
}

TEST_CASE("harness: switching the harness of an empty session keeps what was typed in it") {
    TestApp test;
    add_other(test, FakeHarness("other"));
    test.type_text("center");
    test.command("session.new");
    test.until("the new session is active", [](const App& app) { return app.ctx.runtime.sessions().active_id().has_value(); });
    const auto empty = *test.active();
    test.type_text("for the other harness");

    test.command("harness.other");
    test.until("a session runs on the other harness", [](const App& app) { return active_harness(app) == "other"; });
    CHECK_MESSAGE(test.app->ctx.runtime.sessions().get(empty) == nullptr, "the empty session was discarded");
    CHECK_MESSAGE(test.app->ctx.composer.text() == "for the other harness", "its replacement took over its text");
    const auto replacement = *test.active();
    test.send(area_message(area_id::SESSIONS, to::sessions::Delete{replacement}));
    CHECK_MESSAGE(test.app->ctx.composer.text() == "center", "the center kept its own");
}

TEST_CASE("harness: a harness switch that fails to start gives the typed text back") {
    TestApp test;
    add_other(test, FakeHarness("other").no_start());
    test.type_text("center");
    test.command("session.new");
    test.until("the new session is active", [](const App& app) { return app.ctx.runtime.sessions().active_id().has_value(); });
    test.type_text("for the other harness");

    test.command("harness.other");
    test.until("the start failed", [](const App& app) {
        return !app.ctx.runtime.sessions().active_id() && !app.ctx.runtime.sessions().starting();
    });
    CHECK_MESSAGE(test.app->ctx.composer.text() == "for the other harness\n\ncenter", "the text is back, ahead of the center's own");

    test.command("harness.fake");
    test.until("a session runs on the fake harness", [](const App& app) { return app.ctx.runtime.sessions().active() != nullptr; });
    CHECK_MESSAGE(test.app->ctx.composer.text().empty(), "the next session doesn't get the text again");
}

TEST_CASE("harness: status words") {
    using S = loa::contracts::AvailabilityStatus;
    CHECK(h::status_word(std::nullopt) == "Checking…");
    CHECK(h::status_word(S::Ready) == "Ready");
    CHECK(h::status_word(S::NotInstalled) == "Not installed");
    CHECK(h::status_word(S::Unreachable) == "Not running");
    CHECK(h::status_word(S::Incompatible) == "Incompatible");
    CHECK(h::status_word(S::Failed) == "Failed");
    CHECK(h::status_word(S::Unknown) == "Unknown");
}
