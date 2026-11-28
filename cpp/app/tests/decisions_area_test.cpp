// The decisions area (port of `loa-app/src/areas/decisions/tests.rs`): the unattended toggle
// and its settings screen, waiting cards, answers through the harness and unattended timeouts.

#include <algorithm>

#include <doctest/doctest.h>

#include "areas/decisions/decisions.hpp"
#include "areas/tools/tools.hpp"
#include "loa/core/schema.hpp"
#include "loa/runtime/time.hpp"
#include "loa/ui/session/decisions/card.hpp"
#include "support/test_app.hpp"

using namespace loa::app;
using loa::app::testing::TestApp;
namespace d = loa::app::areas::decisions;
namespace attr = loa::core::attr;

namespace {

constexpr std::string_view ON = "Unattended prompts: deny approvals and reject plans after 5 min";
constexpr std::string_view OFF = "Unattended prompts: wait for someone";

bool has_title(TestApp& test, std::string_view title) {
    auto titles = test.palette_titles();
    return std::ranges::find(titles, title) != titles.end();
}

// The first `<decision>` of the active session.
const loa::core::Node* first_decision(const App& app) {
    const auto* active = app.ctx.runtime.sessions().active();
    if (!active) return nullptr;
    const auto& dom = active->dom();
    for (const auto& node : dom.iter())
        if (node.kind == loa::core::ElementKind::Decision) return &node;
    return nullptr;
}

} // namespace

TEST_CASE("decisions: unattended prompts toggle from the palette and are saved") {
    TestApp test;
    CHECK(has_title(test, ON));
    test.command("decisions.unattended.on");
    CHECK(has_title(test, OFF));
    CHECK_FALSE(has_title(test, ON));
    auto saved = test.app->ctx.services.storage.config.load();
    REQUIRE(saved.has_value());
    CHECK_MESSAGE(!(saved->unattended == loa::settings::app::Config{}.unattended), "saved");
    test.command("decisions.settings");
    CHECK(test.app->ctx.screen() == std::optional<std::string_view>("decisions"));
    test.send(d::State::message(d::msg::CloseSettings{}));
    CHECK_FALSE(test.app->ctx.screen().has_value());

    test.session("hello");
    test.prompt("ask question");
    test.until("the card waits", [](const App& app) { return app.areas.get<d::State>()->focused(); });
}

TEST_CASE("decisions: a card answers through the harness") {
    TestApp test;
    test.session("hello");
    test.prompt("ask");
    test.until("the card waits", [](const App& app) { return app.areas.get<d::State>()->focused(); });
    // `1` selects the card's first option (allow once), Enter confirms it.
    loa::ui::shell::palette::KeyPress one{std::string("1"), U'1', {}, false};
    test.key(one);
    CHECK_MESSAGE(test.area<d::State>().focused(), "a digit only selects");
    test.key(loa::ui::shell::palette::Named::Enter);
    const auto answered = [&] {
        const auto* decision = first_decision(*test.app);
        return decision ? decision->attr(attr::ANSWER) : nullptr;
    };
    test.until("the harness took the answer", [&](const App&) { return answered() != nullptr; });
    CHECK(*answered() == loa::core::Value(std::vector<std::string>{std::string(loa::core::answer::ALLOW_ONCE)}));
    CHECK_FALSE(test.area<d::State>().focused());
}

TEST_CASE("decisions: an unattended timeout answers for the user") {
    TestApp test;
    test.command("decisions.unattended.on");
    test.session("hello");
    test.prompt("ask");
    test.until("the card waits", [](const App& app) { return app.areas.get<d::State>()->focused(); });
    CHECK(test.area<d::State>().desk().wants_clock());
    // Before the timeout nothing is answered.
    test.send(d::State::message(d::msg::Desk{loa::ui::session::decisions::desk_msg::Tick{loa::runtime::time::now_ms() + 60'000}}));
    CHECK(test.area<d::State>().focused());
    // Past five minutes the approval is denied, recorded as the timeout's.
    test.send(d::State::message(d::msg::Desk{loa::ui::session::decisions::desk_msg::Tick{loa::runtime::time::now_ms() + 6 * 60'000}}));
    test.until("the timeout answered", [](const App& app) {
        const auto* decision = first_decision(app);
        return decision && decision->str_attr(attr::BY) == std::optional<std::string_view>(loa::ui::session::decisions::TIMEOUT_BY);
    });
    CHECK_FALSE(test.area<d::State>().focused());
}

TEST_CASE("decisions: Esc dismisses the focused card, or closes a screen over it first") {
    TestApp test;
    test.session("hello");
    test.prompt("ask");
    test.until("the card waits", [](const App& app) { return app.areas.get<d::State>()->focused(); });
    test.command("decisions.settings");
    test.send(test.area<d::State>().dismiss());
    CHECK_FALSE(test.app->ctx.screen().has_value());
    CHECK_MESSAGE(test.area<d::State>().focused(), "the card still waits");
    test.send(test.area<d::State>().dismiss());
    test.until("the card was dismissed", [](const App& app) {
        const auto* decision = first_decision(app);
        return decision && decision->str_attr(attr::BY).has_value();
    });
}

TEST_CASE("decisions: remote decisions are told apart by their request id") {
    loa::core::Dom dom;
    using loa::core::Draft;
    using loa::core::ElementKind;
    REQUIRE(dom.commit(Draft::insert(loa::core::NodeId::DOCUMENT, ElementKind::Session,
                                     {{"id", "s"}, {"harness", "omp"}, {"node", "k"}, {"created", 1}}),
                       0));
    REQUIRE(dom.commit(Draft::insert(loa::core::NodeId{1}, ElementKind::Decision, {{"kind", "approval"}, {"request_id", "remote:7"}}), 0));
    REQUIRE(dom.commit(Draft::insert(loa::core::NodeId{1}, ElementKind::Decision, {{"kind", "approval"}, {"request_id", "7"}}), 0));
    CHECK(areas::tools::is_remote_decision(dom, loa::core::NodeId{2}));
    CHECK_FALSE(areas::tools::is_remote_decision(dom, loa::core::NodeId{3}));
    CHECK_FALSE(areas::tools::is_remote_decision(dom, loa::core::NodeId{1}));
}
