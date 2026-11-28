// Port of the inline tests of `crates/loa-ui-settings/src/models/picker.rs`.

#include <doctest/doctest.h>

#include <format>
#include <string>
#include <vector>

#include <Qt>

#include "loa/ui/settings/models/view.hpp"
#include "support.hpp"

using namespace loa::ui::settings::models;
using namespace loa::ui::settings::models::picker;
namespace vm = loa::view::settings::models;
namespace vi = vm::picker::intent;
namespace vt = vm::picker::target;

namespace {

Catalog catalog() {
    auto info = [](std::string provider, std::string provider_name, std::string id, std::string name) {
        return vm::ModelInfo{{std::move(provider), std::move(id)}, std::move(name), std::move(provider_name)};
    };
    auto role = [](std::string role, std::string id) { return vm::RoleAssignment{std::move(role), {"anthropic", std::move(id)}}; };
    return {{info("anthropic", "Anthropic", "claude-sonnet", "Claude Sonnet"), info("openai", "OpenAI", "gpt-5", "GPT-5"),
             info("anthropic", "Anthropic", "claude-haiku", "Claude Haiku"), info("openai", "OpenAI", "gpt-5-mini", "GPT-5 mini")},
            {role("default", "claude-sonnet"), role("smol", "claude-haiku")}};
}

using Groups = std::vector<std::pair<std::string, std::vector<std::string>>>;

Groups titles(const State& s) {
    Groups out;
    for (const auto& g : s.groups()) {
        std::vector<std::string> rows;
        for (const auto& r : g.rows) rows.push_back(r.kind == Row::Kind::Role ? s.catalog().roles[r.index].role : s.catalog().models[r.index].model.id);
        out.emplace_back(std::string(g.title), std::move(rows));
    }
    return out;
}

std::optional<Intent> session(std::string provider, std::string id, std::optional<std::string> role) {
    return vi::SetSessionModel{{{std::move(provider), std::move(id)}, std::move(role)}};
}

Message loc(Local l) { return Message::local(std::move(l)); }

} // namespace

TEST_CASE("search groups by provider and finds roles") {
    const Groups all{{"Roles", {"default", "smol"}}, {"Anthropic", {"claude-sonnet", "claude-haiku"}}, {"OpenAI", {"gpt-5", "gpt-5-mini"}}};
    const std::vector<std::tuple<Target, std::string, Groups>> cases{
        {vt::Session{}, "", all},
        {vt::Session{}, "OPENAI mini", {{"OpenAI", {"gpt-5-mini"}}}},
        {vt::Role{"plan"}, "smol", {{"Anthropic", {"claude-haiku"}}}},
    };
    for (const auto& [target, query, want] : cases) {
        auto [s, task] = State::open(catalog(), target, std::nullopt);
        CHECK(task == loa::ui::shell::Task::of(loa::ui::shell::op::Focus{std::string(SEARCH_ID)}));
        (void)s.update(loc(local::Query{query}));
        CHECK_MESSAGE(titles(s) == want, query);
    }
}

TEST_CASE("keys and picks reduce to model changes") {
    struct Case {
        Target target;
        std::optional<ModelRef> current;
        std::vector<Message> messages;
        std::optional<std::size_t> selected;
        std::optional<Intent> event;
    };
    std::vector<Message> downs(10, loc(local::Down{}));
    downs.push_back(loc(local::Submit{}));
    const std::vector<Case> cases{
        {vt::Session{}, std::nullopt, {loc(local::Up{})}, 0, std::nullopt},
        {vt::Session{}, std::nullopt, downs, 5, session("openai", "gpt-5-mini", std::nullopt)},
        {vt::Session{}, std::nullopt, {loc(local::Pick{1})}, 0, session("anthropic", "claude-haiku", "smol")},
        {vt::Role{"plan"}, ModelRef{"openai", "gpt-5"}, {loc(local::Submit{})}, 2, vi::SetRoleModel{"plan", {"openai", "gpt-5"}}},
        {vt::Session{}, ModelRef{"openai", "gpt-5-mini"}, {loc(local::Query{"gpt"}), loc(local::Submit{})}, 0, session("openai", "gpt-5", std::nullopt)},
        {vt::Session{}, std::nullopt, {loc(local::Query{"nothing like this"}), loc(local::Submit{})}, std::nullopt, std::nullopt},
        {vt::Session{}, std::nullopt, {Message::event(vi::Close{})}, 0, vi::Close{}},
    };
    for (std::size_t i = 0; i < cases.size(); ++i) {
        const auto& c = cases[i];
        auto [s, _] = State::open(catalog(), c.target, c.current);
        std::optional<Intent> last;
        for (auto m : c.messages) last = s.update(m);
        CHECK_MESSAGE(s.selected() == c.selected, "case " << i);
        CHECK_MESSAGE(last == c.event, "case " << i);
    }
}

TEST_CASE("arrow keys and escape map to messages") {
    CHECK(State::key(Qt::Key_Up) == loc(local::Up{}));
    CHECK(State::key(Qt::Key_Down) == loc(local::Down{}));
    CHECK(State::key(Qt::Key_Escape) == Message::event(vi::Close{}));
    CHECK_FALSE(State::key(Qt::Key_A).has_value());
    // Closing passes the guard; picks only come from the reducer.
    auto [s, _] = State::open(catalog(), vt::Session{}, std::nullopt);
    CHECK_FALSE(s.update(Message::event(vi::SetRoleModel{"plan", {"openai", "gpt-5"}})).has_value());
}

TEST_CASE("the selection scrolls the list") {
    auto [s, _] = State::open(catalog(), vt::Session{}, ModelRef{"openai", "gpt-5-mini"});
    CHECK(s.selected() == 5);
    CHECK(s.scroll() == loa::ui::shell::Task::of(loa::ui::shell::op::SnapTo{std::string(LIST_ID), std::nullopt, 1.0f}));
}

TEST_CASE("clicking a model switches the session") {
    auto [s, _] = State::open(catalog(), vt::Session{}, std::nullopt);
    (void)test::app();
    PickerView view;
    std::vector<Intent> events;
    for (auto m : test::click<PickerView, State, Message>(view, s, "GPT-5 mini"))
        if (auto e = s.update(m)) events.push_back(*e);
    REQUIRE(events.size() == 1);
    CHECK(events[0] == *session("openai", "gpt-5-mini", std::nullopt));
}
