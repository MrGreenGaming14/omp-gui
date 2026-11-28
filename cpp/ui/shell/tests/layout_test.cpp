// Port of the tests in `loa-ui-shell/src/layout.rs` (the narrow render check lives in view_test).

#include <doctest/doctest.h>

#include <variant>
#include <vector>

#include "loa/ui/kit/widgets.hpp"
#include "loa/ui/shell/layout.hpp"

using namespace loa::ui::shell;
using namespace loa::ui::shell::layout;
namespace in = loa::view::shell::intent;
namespace is = loa::view::shell::sidebar_intent;
using loa::view::theme::Appearance;
using loa::view::theme::ThemeRef;

namespace {
std::vector<Intent> send(State& state, std::vector<Message> messages) {
    std::vector<Intent> out;
    for (auto& m : messages)
        if (auto e = state.update(std::move(m)).second) out.push_back(std::move(*e));
    return out;
}
PanelTab tab(const char* id) { return PanelTab{id, id, std::nullopt}; }
} // namespace

TEST_CASE("top bar height and panel detach id match the Rust frame") {
    CHECK(topbar::HEIGHT == 52.0F);
    CHECK(PANEL_DETACH_ID == "loa-panel-detach");
}

TEST_CASE("panes resize within bounds, go full and follow the theme") {
    auto [state, task] = State::make(Appearance{});
    CHECK(task.ops == std::vector<Op>{op::QuerySystemTheme{}});
    state.set_tabs({tab("diff"), tab("files")});
    send(state, {msg::FrameSized{{1600, 900}}, msg::SelectTab{"files"}, msg::SelectTab{"gone"}});
    const float splitter = loa::ui::kit::widgets::splitter_width();
    const float max_panel = 1600.0F - SIDEBAR_WIDTH - MIN_CENTER_WIDTH - 2.0F * splitter;
    struct Drag {
        Pane pane;
        float x, width;
    };
    // The Rust table assumes a 6px splitter; widths derive from the active token.
    const Drag drags[] = {{Pane::Panel, 1100, 1600.0F - 1100.0F - splitter / 2.0F},
                          {Pane::Panel, 10, max_panel},
                          {Pane::Panel, 1590, MIN_PANEL_WIDTH},
                          {Pane::Sidebar, 343, 343.0F - splitter / 2.0F},
                          {Pane::Sidebar, 5000, 480},
                          {Pane::Sidebar, 10, 200}};
    for (const auto& d : drags) {
        CAPTURE(d.x);
        send(state, {msg::DragStart{d.pane}, msg::DragMove{d.x}, msg::DragEnd{}, msg::DragMove{600}});
        CHECK((d.pane == Pane::Panel ? state.panel_width() : state.sidebar_width()) == doctest::Approx(d.width));
    }
    send(state, {msg::TogglePanel{}, msg::TogglePanel{}, msg::ToggleFull{Pane::Panel}, msg::ToggleFull{Pane::Center}});
    CHECK(state.active_tab() == std::optional<std::string_view>("files")); // the panel keeps its tab
    CHECK(state.full() == Pane::Center);                                   // and the chat takes over
    send(state, {msg::ToggleFull{Pane::Panel}, msg::TogglePanel{}});
    CHECK(!state.full()); // closing the panel leaves its full screen
    const auto events = send(state, {msg::SystemTheme{theme::Mode::Light}, msg::SetTheme{ThemeRef{"dark"}}, msg::SetTheme{ThemeRef{"nope"}}});
    REQUIRE(events.size() == 3);
    CHECK(std::holds_alternative<in::ThemeChanged>(events[0]));
    CHECK(std::holds_alternative<in::SetTheme>(events[1]));
    CHECK(std::holds_alternative<in::SetTheme>(events[2]));
    CHECK(state.theme_state().variant() == theme::Variant::Light); // unknown themes fall back to the system
    CHECK(state.theme_state().errors().size() == 1);
    Appearance dark;
    dark.theme = ThemeRef{"dark"};
    const auto changed = state.set_appearance(dark);
    CHECK((changed && std::holds_alternative<in::ThemeChanged>(*changed)));
    CHECK(!state.set_appearance(dark)); // a config change re-applies the appearance once
}

TEST_CASE("breadcrumb follows the active session") {
    auto state = State::make(Appearance{}).first;
    state.sidebar_mut().set_nodes({NodeStatus{"k-box", "buildbox", true, false}});
    SessionSummary s;
    s.id = "s1";
    s.title = "Ship it";
    s.project = "loa";
    s.node = "k-box";
    s.waiting = true;
    s.updated = 1;
    state.sidebar_mut().set_sessions({s});
    state.set_session_running("s1", true);
    CHECK((state.sidebar().is_running("s1") && !state.sidebar().shows_running("s1")));
    CHECK(send(state, {msg::Sidebar{sidebar::msg::OpenSession{"s1"}}}) == std::vector<Intent>{in::Sidebar{is::OpenSession{"s1"}}});
    const auto crumb = state.breadcrumb();
    REQUIRE(crumb);
    CHECK(crumb->first->title == "Ship it");
    CHECK(crumb->second.name == "buildbox");
    CHECK(crumb->first->waiting);
    state.sidebar_mut().remove_session("s1");
    CHECK((!state.breadcrumb() && !state.sidebar().is_running("s1")));
}

TEST_CASE("tabs: add replaces, show opens, reveal snaps and the wheel scrolls") {
    auto state = State::make(Appearance{}).first;
    state.add_tab(tab("a"));
    state.add_tab(tab("b"));
    state.add_tab(PanelTab{"a", "Again", std::nullopt});
    CHECK(state.tabs().size() == 2);
    CHECK(state.tabs()[0].title == "Again");
    state.set_panel_open(false);
    state.show_tab("b");
    CHECK(state.panel_open());
    CHECK(state.reveal_active_tab().ops == std::vector<Op>{op::SnapTo{std::string(PANEL_TABS_ID), 1.0F, std::nullopt}});
    CHECK(wheel_tabs(PANEL_TABS_ID, {ScrollDelta::Unit::Lines, 0, 1}).ops == std::vector<Op>{op::ScrollBy{std::string(PANEL_TABS_ID), -WHEEL_LINE, 0}});
    CHECK(wheel_tabs(PANEL_TABS_ID, {ScrollDelta::Unit::Pixels, 0, 0}).empty());
    CHECK(send(state, {msg::Emit{in::DetachTab{"b"}}}) == std::vector<Intent>{in::DetachTab{"b"}});
}
