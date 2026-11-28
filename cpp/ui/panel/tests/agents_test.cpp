// Port of the inline tests of `crates/loa-ui-panel/src/agents.rs`.

#include <doctest/doctest.h>

#include <format>
#include <sstream>

#include "loa/ui/panel/agents.hpp"

using namespace loa::ui::panel::agents;
namespace core = loa::core;
namespace attr = loa::core::attr;
namespace intent = loa::view::panel::agents_intent;
using core::Draft;
using core::ElementKind;
using core::Value;

namespace {

core::Attrs attrs(std::initializer_list<std::pair<std::string_view, Value>> list) {
    core::Attrs out;
    for (const auto& [k, v] : list) out.insert_or_assign(std::string(k), v);
    return out;
}

NodeId commit(core::Dom& dom, std::initializer_list<State*> states, Draft draft) {
    auto patch = dom.commit(std::move(draft), 0);
    REQUIRE(patch);
    for (auto* s : states) s->apply(dom, *patch);
    return patch->node;
}

// A session with a running turn; returns the turn.
NodeId session_turn(core::Dom& dom, std::initializer_list<State*> states) {
    const auto session = commit(dom, states,
                                Draft::insert(NodeId::DOCUMENT, ElementKind::Session,
                                              attrs({{attr::ID, "s1"}, {attr::HARNESS, "omp"}, {attr::NODE, "key"}, {attr::CREATED, std::int64_t{0}}})));
    return commit(dom, states, Draft::insert(session, ElementKind::Turn, attrs({{attr::STATUS, "running"}})));
}

} // namespace

TEST_CASE("patches track output, waits and events") {
    core::Dom dom;
    auto state = State::background_jobs();
    const auto turn = session_turn(dom, {&state});
    const auto a = commit(dom, {&state},
                          Draft::insert(turn, ElementKind::Job,
                                        attrs({{attr::KIND, JobKind::Shell}, {attr::COMMAND, "cargo build"}, {attr::STARTED, std::int64_t{1000}}})));
    for (auto [chunk, last] : {std::pair{"ab", "ab"}, {"c\nde", "de"}, {"f\n", "def"}, {"  \n\n", "def"}}) {
        commit(dom, {&state}, Draft::append(a, chunk));
        CHECK(state.job(a)->last_line() == last);
    }
    (void)state.update(msg::Tick{66'000});
    const auto wait = commit(dom, {&state}, Draft::insert(turn, ElementKind::Call, attrs({{attr::TOOL, WAIT_TOOL}, {attr::STATUS, "pending"}})));
    CHECK(state.job(a)->label() == "cargo build");
    CHECK(state.elapsed_ms(a) == 65'000);
    CHECK(state.is_awaited(a));
    CHECK(state.update(msg::Cancel{a}).second == Event{intent::Cancel{a, std::nullopt}});
    commit(dom, {&state}, Draft::set(wait, std::string(attr::STATUS), core::CallStatus::Done));
    commit(dom, {&state}, Draft::set(a, std::string(attr::STATUS), JobStatus::Done));
    CHECK_FALSE(state.waiting());
    CHECK_FALSE(state.update(msg::Cancel{a}).second);
    CHECK_FALSE(state.update(msg::Open{a}).second);
    CHECK(state.jobs() == State::from_dom(dom, PaneKind::Jobs).jobs()); // incremental equals a fresh scan
}

TEST_CASE("toggle expands a job and survives a reset") {
    core::Dom dom;
    auto state = State::background_jobs();
    const auto turn = session_turn(dom, {&state});
    const auto a = commit(dom, {&state}, Draft::insert(turn, ElementKind::Job, attrs({{attr::KIND, JobKind::Shell}, {attr::COMMAND, "ls"}})));
    CHECK_FALSE(state.update(msg::Toggle{a}).second);
    CHECK(state.job(a)->expanded());
    state.reset(dom);
    CHECK(state.job(a)->expanded());
    (void)state.update(msg::Toggle{a});
    CHECK_FALSE(state.job(a)->expanded());
    (void)state.update(msg::Toggle{NodeId{999}});
    CHECK(state.job(NodeId{999}) == nullptr);
}

TEST_CASE("agents and jobs split by kind and emit open or transcript") {
    core::Dom dom;
    auto agents = State::agents();
    auto jobs = State::background_jobs();
    const auto turn = session_turn(dom, {&agents, &jobs});
    const auto shell = commit(dom, {&agents, &jobs}, Draft::insert(turn, ElementKind::Job, attrs({{attr::KIND, JobKind::Shell}, {attr::COMMAND, "cargo test"}})));
    const auto alpha = commit(dom, {&agents, &jobs},
                              Draft::insert(turn, ElementKind::Job, attrs({{attr::KIND, JobKind::Subagent}, {attr::ID, "a1"}, {attr::TITLE, "Alpha"}})));
    const auto beta = commit(dom, {&agents, &jobs},
                             Draft::insert(turn, ElementKind::Job,
                                           attrs({{attr::KIND, JobKind::Subagent}, {attr::ID, "b1"}, {attr::TITLE, "Beta"}, {attr::SESSION, "/tmp/b1.jsonl"}})));
    std::vector<NodeId> got;
    for (const auto& [n, _] : agents.jobs()) got.push_back(n);
    CHECK(got == std::vector{alpha, beta});
    got.clear();
    for (const auto& [n, _] : jobs.jobs()) got.push_back(n);
    CHECK(got == std::vector{shell});
    CHECK(agents.update(msg::Open{alpha}).second == Event{intent::OpenSubagent{"a1"}});
    CHECK_FALSE(agents.update(msg::Detach{alpha}).second);
    CHECK(agents.update(msg::Detach{beta}).second == Event{intent::DetachTranscript{"/tmp/b1.jsonl"}});
}

TEST_CASE("long output shows only its tail until expanded in full") {
    core::Dom dom;
    auto state = State::background_jobs();
    const auto turn = session_turn(dom, {&state});
    const auto job = commit(dom, {&state}, Draft::insert(turn, ElementKind::Job, attrs({{attr::KIND, JobKind::Shell}, {attr::COMMAND, "make"}})));
    std::string text;
    for (int i = 1; i <= 250; ++i) text += std::format("line {}\n", i);
    commit(dom, {&state}, Draft::append(job, text));
    CHECK(state.job(job)->line_count() == 250);
    const std::string shown = state.job(job)->output().substr(state.job(job)->tail_start());
    CHECK(shown.starts_with("line 51\n"));
    CHECK(static_cast<std::size_t>(std::ranges::count(shown, '\n')) == TAIL_LINES);
    (void)state.update(msg::ToggleFull{job});
    CHECK(state.is_full(job));
}
