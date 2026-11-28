// The sessions area driven headless through `TestApp` (port of
// `loa-app/src/areas/sessions/tests.rs`, its iced render checks left out): branching, plan mode,
// subagent tabs, transfer, and omp's model roles file.

#include <algorithm>
#include <fstream>
#include <array>
#include <iterator>
#include <memory>
#include <string>
#include <vector>

#include <doctest/doctest.h>

#include "areas/sessions/sessions.hpp"
#include "loa/app/app.hpp"
#include "loa/app_runtime/session.hpp"
#include "loa/core/patch.hpp"
#include "loa/core/schema.hpp"
#include "loa/settings/models/config.hpp"
#include "loa/settings/models/selector.hpp"
#include "support/test_app.hpp"

using namespace loa::app;
using loa::app::testing::FakeHarness;
using loa::app::testing::TestApp;
namespace s = loa::app::areas::sessions;
namespace core = loa::core;
namespace attr = loa::core::attr;
namespace agents = loa::ui::panel::agents;
namespace composer = loa::ui::session::composer;
namespace nav = loa::view::shell::sidebar_intent;

namespace {

core::Dom subagent_dom(const std::string& id, const std::string& text) {
    core::Dom dom;
    auto root = dom.commit(core::Draft::insert(core::NodeId::DOCUMENT, core::ElementKind::Session,
                                               {{std::string(attr::ID), id},
                                                {std::string(attr::HARNESS), "fake"},
                                                {std::string(attr::NODE), "testhost"},
                                                {std::string(attr::CREATED), std::int64_t{0}}}),
                           0);
    REQUIRE(root.has_value());
    auto turn = dom.commit(core::Draft::insert(root->node, core::ElementKind::Turn, {{std::string(attr::STATUS), core::TurnStatus::Done}}), 0);
    REQUIRE(turn.has_value());
    REQUIRE(dom.commit(core::Draft::insert(turn->node, core::ElementKind::Message, {{std::string(attr::ROLE), core::Role::Assistant}}, text), 0)
                .has_value());
    return dom;
}

core::NodeId add_job(TestApp& test, const std::string& parent, core::JobKind kind, core::Attrs attrs) {
    const auto* session = test.app->ctx.runtime.sessions().get(parent);
    REQUIRE(session != nullptr);
    auto writer = session->writer();
    const auto root = session->dom().root()->id;
    auto turn = writer->commit(core::Draft::insert(root, core::ElementKind::Turn, {{std::string(attr::STATUS), core::TurnStatus::Running}}));
    REQUIRE(turn.has_value());
    core::Attrs job{{std::string(attr::KIND), kind}, {std::string(attr::STATUS), core::JobStatus::Running}};
    for (auto& [name, value] : attrs) job.insert_or_assign(name, std::move(value));
    auto inserted = writer->commit(core::Draft::insert(turn->node, core::ElementKind::Job, std::move(job)));
    REQUIRE(inserted.has_value());
    return inserted->node;
}

std::vector<std::string> stored_session_ids(const TestApp& test) {
    std::vector<std::string> ids;
    auto listings = test.app->ctx.runtime.library()->store().list();
    REQUIRE(listings.has_value());
    for (const auto& listing : *listings)
        if (const auto* info = std::get_if<loa::bus::SessionInfo>(&listing)) ids.push_back(info->id);
    return ids;
}

std::vector<std::string> recents(const TestApp& test) {
    std::vector<std::string> ids;
    for (const auto* summary : test.app->ctx.shell.sidebar().recents()) ids.push_back(summary->id);
    return ids;
}

s::State& area(const TestApp& test) { return test.area<s::State>(); }
const s::State& area_of(const App& app) { return *app.areas.get<s::State>(); }

template <typename M>
void send_area(TestApp& test, M message) {
    test.send(area_message(area_id::SESSIONS, std::move(message)));
}

std::size_t agent_count(const App& app) { return area_of(app).agents.jobs().size(); }

const s::ChildTab& tab(const TestApp& test, const std::string& parent, const std::string& child) {
    const auto* found = area(test).subagents.tab(parent, child);
    REQUIRE_MESSAGE(found != nullptr, child);
    return *found;
}

std::string assistant_text(const s::ChildTab& tab) {
    if (!tab.dom) return {};
    for (const auto& node : tab.dom->iter())
        if (node.kind == core::ElementKind::Message) return node.text;
    return {};
}

// Starts a new session as the sidebar's New session does.
void new_session_from_sidebar(TestApp& test) {
    test.send(msg::Shell{loa::ui::shell::layout::msg::Emit{loa::view::shell::intent::Sidebar{nav::NewSession{}}}});
}

bool another_active(const App& app, std::initializer_list<std::string_view> not_these) {
    const auto& id = app.ctx.runtime.sessions().active_id();
    return id && std::ranges::none_of(not_these, [&](std::string_view other) { return id->as_str() == other; });
}

} // namespace

TEST_CASE("slash commands branch, switch mode, export and delete sessions") {
    TestApp test;
    const auto first = test.session("hello");
    test.prompt("/fork");
    test.until("the fork is active", [&](const App& app) { return another_active(app, {first}); });
    const auto fork = *test.active();
    const auto* forked = test.app->ctx.runtime.sessions().active()->dom().root();
    CHECK_MESSAGE(forked->str_attr(attr::PARENT_SESSION) == std::optional<std::string_view>(first), "the fork links its parent");

    test.send(msg::OpenRecent{first});
    auto turns = loa::app_runtime::dom_turns(test.app->ctx.runtime.sessions().active()->dom());
    REQUIRE_FALSE(turns.empty());
    send_area(test, s::msg::Chat{loa::view::session::transcript::intent::Rewind{turns.back()->id}});
    test.until("the rewind is active", [&](const App& app) { return another_active(app, {first, fork}); });
    const auto& summary = test.app->ctx.runtime.sessions().active()->summary();
    CHECK_MESSAGE((summary && summary->title.ends_with("(rewound)")), "the rewound branch is named after its source");

    // Tests have no small model to summarize with.
    area(test).small = nullptr;
    test.prompt("/compact");
    CHECK_MESSAGE(test.notice().starts_with("Compaction needs"), test.notice());
    test.prompt("/plan");
    CHECK_MESSAGE(test.notice().find("plan mode") != std::string::npos, "a harness without read-only tools refuses: " << test.notice());
    test.prompt("/model");
    CHECK_MESSAGE(test.notice().starts_with("Name the model"), test.notice());
    test.prompt("/export-xml");
    CHECK_MESSAGE(test.notice().starts_with("Exported to"), test.notice());

    send_area(test, s::msg::Delete{fork});
    CHECK_MESSAGE(test.app->ctx.shell.sidebar().session(fork) == nullptr, "deleted from Recents");
    const auto stored = stored_session_ids(test);
    CHECK(std::ranges::find(stored, fork) == stored.end());
}

TEST_CASE("a compacted session takes over what was typed in its source") {
    TestApp test;
    auto paste = [](TestApp& t, const char* path) { (void)t.app->ctx.composer.attach_image(composer::pasted(path, std::nullopt)); };
    const auto source = test.session("hello");
    test.prompt("/fork");
    test.until("the fork is active", [&](const App& app) { return another_active(app, {source}); });
    const auto compacted = *test.active();
    test.type_text("typed in the fork");
    paste(test, "fork.png");
    test.send(msg::OpenRecent{source});
    test.type_text("keep me");
    paste(test, "source.png");

    // Tests have no small model to summarize with: the fork stands in for the compacted session.
    area(test).compacted.insert_or_assign(compacted, source);
    test.send(msg::OpenRecent{compacted});
    CHECK_MESSAGE(test.app->ctx.runtime.sessions().get(source) == nullptr, "the source closed");
    CHECK_MESSAGE(test.app->ctx.composer.text() == "keep me [Image #2] \n\ntyped in the fork [Image #1] ",
                  "both texts stay, their images numbered apart: " << test.app->ctx.composer.text());
    test.send(msg::Composer{composer::msg::Submit{}});
    test.until("the merged prompt ran", [](const App& app) { return !app.ctx.composer.is_busy(); });
    const auto prompts = test.harness.seen().prompts;
    REQUIRE_FALSE(prompts.empty());
    CHECK(prompts.back() == "keep me [Image #2] \n\ntyped in the fork [Image #1]");
}

TEST_CASE("agents and jobs filter by kind and a transcript avoids recents and runtime sessions") {
    TestApp test(FakeHarness("fake").subagent_messages());
    const auto file = test.dir.path() / "alpha-child.txt";
    std::ofstream(file) << "first child line\nsecond child line\n";
    const auto parent = test.session("hello");

    add_job(test, parent, core::JobKind::Shell, {{std::string(attr::COMMAND), "cargo test"}});
    const auto alpha = add_job(test, parent, core::JobKind::Subagent,
                               {{std::string(attr::ID), "alpha"}, {std::string(attr::TITLE), "Alpha"}, {std::string(attr::SESSION), file.string()}});
    add_job(test, parent, core::JobKind::Subagent, {{std::string(attr::ID), "beta"}, {std::string(attr::TITLE), "Beta"}});
    test.until("jobs split into Agents and Background Jobs", [](const App& app) {
        return area_of(app).agents.jobs().size() == 2 && area_of(app).jobs.jobs().size() == 1;
    });
    std::vector<std::string> shells, subagents;
    for (const auto& [_, job] : area(test).jobs.jobs()) shells.emplace_back(job.label());
    for (const auto& [_, job] : area(test).agents.jobs()) subagents.emplace_back(job.label());
    CHECK(shells == std::vector<std::string>{"cargo test"});
    CHECK(subagents == std::vector<std::string>{"Alpha", "Beta"});
    std::vector<std::string> tabs;
    for (const auto& t : area(test).panel_tabs()) tabs.push_back(t.id);
    CHECK(tabs == std::vector<std::string>{"agents", "jobs", "session"});

    send_area(test, s::msg::Agents{agents::msg::Detach{alpha}});
    const bool detached = std::ranges::any_of(test.app->windows.detached, [&](const auto& entry) {
        const auto* t = std::get_if<detached::Transcript>(&entry.second);
        return t && t->key == file.string();
    });
    CHECK_MESSAGE(detached, "Transcript opens a detached read-only window; notice: " << test.notice());
    CHECK_MESSAGE(test.app->ctx.runtime.sessions().iter().size() == 1, "no runtime session created for the child");
    CHECK_MESSAGE(stored_session_ids(test) == std::vector<std::string>{parent}, "no library session created for the child");
    CHECK_MESSAGE(recents(test) == std::vector<std::string>{parent}, "no recents entry added for the child");

    test.reopen();
    CHECK_MESSAGE(recents(test) == std::vector<std::string>{parent}, "the child never appears in recents after a restart");
}

TEST_CASE("subagent tabs isolate alpha and beta messages, polling and errors") {
    auto harness = FakeHarness("fake")
                       .subagent_messages()
                       .subagent_transcript("alpha", subagent_dom("alpha", "Alpha initial"), true)
                       .subagent_transcript("beta", subagent_dom("beta", "Beta initial"), true);
    TestApp test(std::move(harness));
    const auto parent = test.session("hello");
    const auto alpha_node = add_job(test, parent, core::JobKind::Subagent, {{std::string(attr::ID), "alpha"}, {std::string(attr::TITLE), "Alpha"}});
    const auto beta_node = add_job(test, parent, core::JobKind::Subagent, {{std::string(attr::ID), "beta"}, {std::string(attr::TITLE), "Beta"}});
    test.until("subagent jobs registered", [](const App& app) { return agent_count(app) == 2; });

    send_area(test, s::msg::Agents{agents::msg::Open{alpha_node}});
    CHECK(area(test).subagents.selected_of(parent) == std::optional<std::string_view>("alpha"));
    CHECK(assistant_text(tab(test, parent, "alpha")) == "Alpha initial");
    CHECK(tab(test, parent, "alpha").tab_label == "Alpha · alpha");
    CHECK(test.harness.seen().subagent_transcripts == std::vector<std::string>{"alpha"});

    send_area(test, s::msg::SubagentDraft{parent, "alpha", "draft for alpha"});
    send_area(test, s::msg::SubagentSelect{std::nullopt});
    CHECK_FALSE(area(test).subagents.selected_of(parent).has_value());
    send_area(test, s::msg::Agents{agents::msg::Open{beta_node}});
    CHECK(area(test).subagents.selected_of(parent) == std::optional<std::string_view>("beta"));
    CHECK(assistant_text(tab(test, parent, "beta")) == "Beta initial");
    CHECK(tab(test, parent, "beta").draft.empty());

    send_area(test, s::msg::SubagentDraft{parent, "beta", "hello beta"});
    send_area(test, s::msg::SubagentSend{parent, "beta"});
    CHECK(test.harness.seen().subagent_messages ==
          std::vector<std::pair<std::string, std::string>>{{"beta", "hello beta"}});
    CHECK(tab(test, parent, "beta").draft.empty());
    CHECK(tab(test, parent, "alpha").draft == "draft for alpha");

    send_area(test, s::msg::SubagentSelect{std::string("alpha")});
    CHECK(area(test).subagents.selected_of(parent) == std::optional<std::string_view>("alpha"));

    const auto before_tick = test.harness.seen().subagent_transcripts.size();
    send_area(test, s::msg::Tick{1'000});
    const auto after_tick = test.harness.seen().subagent_transcripts;
    CHECK_MESSAGE(std::vector<std::string>(after_tick.begin() + static_cast<std::ptrdiff_t>(before_tick), after_tick.end()) ==
                      std::vector<std::string>{"alpha"},
                  "a poll fetches only the selected addressable subagent");

    // A terminal job status fetches the transcript again and stops polling once not addressable.
    test.harness.set_subagent_transcript("alpha", subagent_dom("alpha", "Alpha finished"), false);
    auto writer = test.app->ctx.runtime.sessions().get(parent)->writer();
    REQUIRE(writer->commit(core::Draft::set(alpha_node, std::string(attr::STATUS), core::JobStatus::Done)).has_value());
    test.until("a terminal status fetches alpha's transcript",
               [&](const App& app) { return !area_of(app).subagents.tab(parent, "alpha")->addressable; });
    CHECK(assistant_text(tab(test, parent, "alpha")) == "Alpha finished");
    CHECK(tab(test, parent, "alpha").status == core::JobStatus::Done);
    const auto after_terminal = test.harness.seen().subagent_transcripts.size();
    send_area(test, s::msg::Tick{2'000});
    CHECK_MESSAGE(test.harness.seen().subagent_transcripts.size() == after_terminal, "a non-addressable selected tab is not polled");
    CHECK(area(test).subscription().empty() == false);

    // A failed delivery restores the draft and shows the error.
    send_area(test, s::msg::SubagentSelect{std::string("beta")});
    test.harness.set_subagent_error("beta", std::nullopt, std::string("delivery failed"));
    send_area(test, s::msg::SubagentDraft{parent, "beta", "retry beta"});
    send_area(test, s::msg::SubagentSend{parent, "beta"});
    const auto& beta = tab(test, parent, "beta");
    REQUIRE(beta.send_error.has_value());
    CHECK(beta.send_error->find("delivery failed") != std::string::npos);
    CHECK(beta.draft == "retry beta");

    CHECK(test.app->ctx.runtime.sessions().iter().size() == 1);
    CHECK(stored_session_ids(test) == std::vector<std::string>{parent});
}

TEST_CASE("a subagent tab explains disabled and unsupported states") {
    // The harness can't message subagents.
    {
        TestApp unsupported(FakeHarness("fake"));
        const auto p1 = unsupported.session("hello");
        const auto alpha = add_job(unsupported, p1, core::JobKind::Subagent, {{std::string(attr::ID), "alpha"}, {std::string(attr::TITLE), "Alpha"}});
        unsupported.until("alpha appears", [](const App& app) { return agent_count(app) == 1; });
        send_area(unsupported, s::msg::Agents{agents::msg::Open{alpha}});
        CHECK(area(unsupported).subagents.selected_of(p1) == std::optional<std::string_view>("alpha"));
        CHECK(unsupported.harness.seen().subagent_transcripts.empty());
        send_area(unsupported, s::msg::SubagentDraft{p1, "alpha", "hi"});
        CHECK(tab(unsupported, p1, "alpha").draft.empty());
        send_area(unsupported, s::msg::SubagentSend{p1, "alpha"});
        CHECK(unsupported.harness.seen().subagent_messages.empty());
    }

    // The parent session isn't running (a reopened historical session).
    TestApp with_cap(FakeHarness("fake").subagent_messages());
    const auto p2 = with_cap.session("hello");
    const auto alpha = add_job(with_cap, p2, core::JobKind::Subagent, {{std::string(attr::ID), "alpha"}, {std::string(attr::TITLE), "Alpha"}});
    with_cap.until("alpha appears", [](const App& app) { return agent_count(app) == 1; });
    with_cap.reopen();
    with_cap.send(msg::OpenRecent{p2});
    REQUIRE(with_cap.app->ctx.runtime.sessions().active() != nullptr);
    CHECK_FALSE(with_cap.app->ctx.runtime.sessions().active()->harness());
    send_area(with_cap, s::msg::Agents{agents::msg::Open{alpha}});
    CHECK(area(with_cap).subagents.selected_of(p2) == std::optional<std::string_view>("alpha"));
    CHECK(with_cap.harness.seen().subagent_transcripts.empty());
    send_area(with_cap, s::msg::SubagentSend{p2, "alpha"});
    CHECK(with_cap.harness.seen().subagent_messages.empty());
}

TEST_CASE("child selection survives switching sessions and is cleaned up on delete") {
    auto harness = FakeHarness("fake")
                       .subagent_messages()
                       .subagent_transcript("alpha", subagent_dom("alpha", "Alpha DOM"), true)
                       .subagent_transcript("beta", subagent_dom("beta", "Beta DOM"), true);
    TestApp test(std::move(harness));
    const auto p1 = test.session("one");
    const auto alpha = add_job(test, p1, core::JobKind::Subagent, {{std::string(attr::ID), "alpha"}, {std::string(attr::TITLE), "Alpha"}});
    const auto beta = add_job(test, p1, core::JobKind::Subagent, {{std::string(attr::ID), "beta"}, {std::string(attr::TITLE), "Beta"}});
    test.until("two subagents", [](const App& app) { return agent_count(app) == 2; });

    send_area(test, s::msg::Agents{agents::msg::Open{alpha}});
    send_area(test, s::msg::SubagentSelect{std::nullopt});
    send_area(test, s::msg::Agents{agents::msg::Open{beta}});
    send_area(test, s::msg::SubagentSelect{std::string("alpha")});
    CHECK(area(test).subagents.selected_of(p1) == std::optional<std::string_view>("alpha"));
    CHECK(area(test).subagents.parents.at(p1).order == std::vector<std::string>{"alpha", "beta"});

    // Another parent session has its own tabs and keeps p1's selection intact.
    new_session_from_sidebar(test);
    test.until("a second session is active", [&](const App& app) { return another_active(app, {p1}); });
    const auto p2 = *test.active();
    CHECK_FALSE(area(test).subagents.parents.contains(p2));
    CHECK(area(test).agents.jobs().empty());

    test.send(msg::OpenRecent{p1});
    CHECK(area(test).subagents.selected_of(p1) == std::optional<std::string_view>("alpha"));
    CHECK(assistant_text(tab(test, p1, "alpha")) == "Alpha DOM");

    // Deleting p1 drops all of its child subtabs.
    send_area(test, s::msg::Delete{p1});
    CHECK_FALSE(area(test).subagents.parents.contains(p1));
}

TEST_CASE("a failed first read keeps polling and a closed tab ignores late results") {
    auto harness = FakeHarness("fake").subagent_messages().subagent_transcript("alpha", subagent_dom("alpha", "Alpha live"), true);
    harness.set_subagent_error("alpha", std::string("rpc hiccup"), std::nullopt);
    TestApp test(std::move(harness));
    const auto parent = test.session("hello");
    const auto alpha = add_job(test, parent, core::JobKind::Subagent, {{std::string(attr::ID), "alpha"}, {std::string(attr::TITLE), "Alpha"}});
    test.until("the subagent job registered", [](const App& app) { return agent_count(app) == 1; });

    // The first read fails, but the job still runs, so the tab keeps polling instead of calling
    // the subagent finished.
    send_area(test, s::msg::Agents{agents::msg::Open{alpha}});
    test.until("the failed read settles",
               [&](const App& app) { return area_of(app).subagents.tab(parent, "alpha")->fetch_error.has_value(); });
    CHECK(tab(test, parent, "alpha").live());
    test.harness.set_subagent_error("alpha", std::nullopt, std::nullopt);
    send_area(test, s::msg::Tick{1'000});
    test.until("the next poll reads the transcript",
               [&](const App& app) { return area_of(app).subagents.tab(parent, "alpha")->addressable; });
    CHECK(assistant_text(tab(test, parent, "alpha")) == "Alpha live");

    // A send result from a closed tab never lands on the tab reopened for the same subagent.
    const auto stale = tab(test, parent, "alpha").generation;
    send_area(test, s::msg::SubagentClose{"alpha"});
    send_area(test, s::msg::Agents{agents::msg::Open{alpha}});
    test.until("the reopened tab loads", [&](const App& app) { return area_of(app).subagents.tab(parent, "alpha")->dom.has_value(); });
    send_area(test, s::msg::SubagentMessageSent{parent, "alpha", stale, "old draft",
                                               std::unexpected(s::SendError{s::SendError::Kind::NotAddressable, "gone"})});
    const auto& reopened = tab(test, parent, "alpha");
    CHECK(reopened.generation != stale);
    CHECK(reopened.draft.empty());
    CHECK(reopened.addressable);
    CHECK_FALSE(reopened.send_error.has_value());
}

TEST_CASE("app plan mode gates a harness that asks first until its plan is approved") {
    TestApp test;
    // Without approval modes the gate can't be enforced: plan mode refuses.
    const auto first = test.session("first");
    test.prompt("/plan");
    CHECK_MESSAGE(test.notice().starts_with("Can't enter plan mode"), test.notice());
    CHECK_FALSE(area(test).in_plan(test.app->ctx));

    // A harness that asks before tools that write or execute can be gated.
    static constexpr std::array<std::string_view, 1> MODES{"ask"};
    for (auto& harness : test.app->ctx.runtime.harnesses_mut().list)
        if (harness.id == "fake") harness.traits.approval_modes = MODES;
    new_session_from_sidebar(test);
    test.until("the second session is active", [&](const App& app) { return another_active(app, {first}); });
    const auto id = test.session("hello");
    test.prompt("/plan");
    CHECK_MESSAGE(test.notice().starts_with("Plan mode:"), test.notice());
    REQUIRE(area(test).in_plan(test.app->ctx));
    test.until("the session records plan mode", [](const App& app) {
        const auto* root = app.ctx.runtime.sessions().active()->dom().root();
        return root->str_attr(attr::MODE) == std::optional<std::string_view>("plan");
    });

    // Prompts carry the plan step; the finished turn's plan goes up for review.
    test.prompt("look around");
    test.until("the plan turn ended", [](const App& app) { return !app.ctx.composer.is_busy(); });
    const auto prompts = test.harness.seen().prompts;
    REQUIRE_FALSE(prompts.empty());
    CHECK(prompts.back().starts_with("look around"));
    CHECK(prompts.back().find(loa::session::plan::PLAN_STEP) != std::string::npos);
    std::optional<core::NodeId> review;
    test.until("the plan review waits", [&](const App& app) {
        const auto& dom = app.ctx.runtime.sessions().get(id)->dom();
        for (const auto& node : dom.iter())
            if (node.kind == core::ElementKind::Decision && loa::session::plan::is_app_plan(dom, node.id)) review = node.id;
        return review.has_value();
    });

    // Approving ends plan mode and sends the plan to carry out.
    send_area(test, s::msg::AnswerPlan{loa::bus::SessionId(id), *review, {{std::string(core::answer::APPROVE)}, std::nullopt, ""}});
    test.until("the approved plan runs", [&](const App& app) {
        return !area_of(app).in_plan(app.ctx) && test.harness.seen().prompts.size() > prompts.size();
    });
    CHECK_FALSE(area(test).in_plan(test.app->ctx));

    // A plan answer for a session out of plan mode changes nothing here.
    send_area(test, s::msg::AnswerPlan{loa::bus::SessionId(id), *review, {{std::string(core::answer::APPROVE)}, std::nullopt, ""}});
    CHECK_FALSE(area(test).in_plan(test.app->ctx));
}

TEST_CASE("model roles read from omp's config file, which round-trips byte-identically") {
    auto dir = loa::runtime::TempDir::create();
    REQUIRE(dir.has_value());
    const auto golden = std::filesystem::path(LOA_GOLDEN_DIR) / "settings" / "config.yml";
    const auto path = dir->path() / "config.yml";
    std::filesystem::copy_file(golden, path);
    auto read = [](const std::filesystem::path& p) {
        std::ifstream in(p, std::ios::binary);
        return std::string(std::istreambuf_iterator<char>(in), {});
    };
    const auto original = read(golden);

    auto roles = s::model_roles(path);
    REQUIRE_MESSAGE(roles.has_value(), roles.error());
    const s::Roles expected{{"default", {"anthropic/claude-x:high"}}, {"smol", {"@default"}}, {"plan", {"openai/gpt-y:low"}}};
    CHECK(*roles == expected);
    CHECK_MESSAGE(read(path) == original, "reading the roles leaves the file alone");

    // Writing back what was read changes no byte.
    auto config = loa::settings::models::config::OmpConfig::load(path);
    REQUIRE(config.has_value());
    for (const auto& [role, candidates] : *roles) {
        std::vector<loa::settings::models::selector::Selector> selectors;
        for (const auto& candidate : candidates) {
            auto parsed = loa::settings::models::selector::Selector::parse(candidate);
            REQUIRE(parsed.has_value());
            selectors.push_back(*parsed);
        }
        REQUIRE(config->set_model_role(role, selectors).has_value());
    }
    REQUIRE(config->save().has_value());
    CHECK(read(path) == original);

    auto missing = s::model_roles(dir->path() / "nope" / "config.yml");
    CHECK((!missing.has_value() || missing->empty()));
}
