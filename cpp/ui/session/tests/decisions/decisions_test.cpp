// Port of `crates/loa-ui-session/src/tests/decisions.rs` (state tests; the iced simulator checks
// read the same text out of `header`, `footer` and `preview_rows`).

#include <doctest/doctest.h>

#include <asio/co_spawn.hpp>
#include <asio/detached.hpp>
#include <asio/io_context.hpp>

#include "loa/core/patch.hpp"
#include "loa/core/testkit.hpp"
#include "loa/ui/session/decisions/desk.hpp"

using namespace loa::ui::session::decisions;
using loa::core::Attrs;
using loa::core::Draft;
using loa::core::ElementKind;
using loa::core::Value;
using loa::core::testkit::DomFixture;
namespace ans = loa::core::answer;
namespace vi = loa::view::session::decisions;

namespace {

Value list(std::initializer_list<std::string_view> items) {
    std::vector<std::string> v;
    for (auto s : items) v.emplace_back(s);
    return Value(v);
}

Attrs attrs(std::initializer_list<std::pair<std::string_view, Value>> items) {
    Attrs a;
    for (const auto& [k, v] : items) a.insert_or_assign(std::string(k), v);
    return a;
}

// A session with one decision of `a` under a `bash` call.
std::pair<DomFixture, NodeId> fixture(Attrs a, std::string_view body) {
    auto f = DomFixture::new_session();
    f.turn().call("bash");
    const auto call = f.last;
    const auto id = f.add(call, ElementKind::Decision, std::move(a), body);
    return {std::move(f), id};
}

State state(const DomFixture& f, bool view_only) {
    State s(Viewer{"me", view_only});
    s.sync(f.dom);
    return s;
}

std::vector<std::string> strs(std::initializer_list<std::string_view> items) { return {items.begin(), items.end()}; }

std::optional<Intent> answer_intent(NodeId id, std::vector<std::string> values, std::optional<std::string> reason, std::string by = "me") {
    return vi::intent::Answer{id, Answer{std::move(values), std::move(reason), std::move(by)}};
}

std::string status_of(const State& s, NodeId id) {
    const auto f = s.footer_of(*s.card(id));
    return f ? f->status : std::string();
}

// Labels of the approval buttons, in order.
std::vector<std::string_view> approval_labels(const State& s, NodeId id) {
    std::vector<std::string_view> out;
    for (auto c : s.card(id)->decision.approval_choices()) out.push_back(label_of(c));
    return out;
}

struct NullNotifier : Notifier {
    NotifyFuture notify(const Notice&) const override { co_return std::expected<void, NotifyError>{}; }
};

// Records every notice; the future it returns succeeds.
struct CountingNotifier : Notifier {
    mutable std::vector<Notice> seen;
    mutable int ran = 0;
    NotifyFuture notify(const Notice& n) const override {
        seen.push_back(n);
        return run(this);
    }
    static NotifyFuture run(const CountingNotifier* self) {
        ++self->ran;
        co_return std::expected<void, NotifyError>{};
    }
};

} // namespace

TEST_CASE("shortcuts map digits, Enter and Escape but not chords") {
    const auto key = [](std::string text, bool ctrl = false) { return KeyPress{KeyPress::Named::None, std::move(text), ctrl, false, false}; };
    const auto named = [](KeyPress::Named n, bool alt = false) { return KeyPress{n, {}, false, alt, false}; };
    CHECK(shortcut(key("1")) == Shortcut::pick(0));
    CHECK(shortcut(key("9")) == Shortcut::pick(8));
    CHECK(shortcut(key("0")) == std::nullopt);
    CHECK(shortcut(key("1", true)) == std::nullopt);
    CHECK(shortcut(named(KeyPress::Named::Enter)) == Shortcut::confirm());
    CHECK(shortcut(named(KeyPress::Named::Escape)) == Shortcut::dismiss());
    CHECK(shortcut(named(KeyPress::Named::Escape, true)) == std::nullopt);
    CHECK(shortcut(key("a")) == std::nullopt);
}

TEST_CASE("every kind maps clicks, keys and text to answers") {
    using Step = Message (*)(NodeId);
    using Expected = std::optional<std::pair<std::vector<std::string>, std::optional<std::string>>>;
    const std::string form = R"({"properties":{"tag":{"type":"string"},"env":{"enum":["dev","prod"]}},"required":["tag"]})";
    const auto session_approval = [] { return attrs({{"kind", "approval"}, {"options", list({ans::ALLOW_ONCE, ans::ALLOW_SESSION, ans::DENY})}}); };
    const auto once_deny = [] { return attrs({{"kind", "approval"}, {"options", list({ans::ALLOW_ONCE, ans::DENY})}}); };
    const auto approval = [] { return attrs({{"kind", "approval"}}); };
    const auto pick = [](std::size_t i) { return Message(msg::Key{Shortcut::pick(i)}); };
    struct Case {
        Attrs a;
        std::string body;
        std::vector<Step> steps;
        Expected expected;
    };
    const auto ok = [](std::vector<std::string> v, std::optional<std::string> r = std::nullopt) { return Expected{{std::move(v), std::move(r)}}; };
    std::vector<Case> cases;
    cases.push_back({session_approval(), "ls", {[](NodeId d) -> Message { return msg::Choose{d, 1}; }}, ok(strs({ans::ALLOW_SESSION}))});
    cases.push_back({session_approval(), "ls",
                     {[](NodeId d) -> Message { return msg::Text{d, " prod "}; }, [](NodeId d) -> Message { return msg::Choose{d, 2}; }},
                     ok(strs({ans::DENY}), "prod")});
    cases.push_back({approval(), "ls",
                     {[](NodeId d) -> Message { return msg::Text{d, " prod "}; }, [](NodeId d) -> Message { return msg::Choose{d, 1}; }},
                     ok(strs({ans::DENY}), "prod")});
    cases.push_back({approval(), "ls", {[](NodeId d) -> Message { return msg::Choose{d, 2}; }}, std::nullopt});
    cases.push_back({once_deny(), "ls",
                     {[](NodeId) -> Message { return msg::Key{Shortcut::pick(1)}; }, [](NodeId) -> Message { return msg::Key{Shortcut::confirm()}; }},
                     ok(strs({ans::DENY}))});
    cases.push_back({once_deny(), "ls",
                     {[](NodeId) -> Message { return msg::Key{Shortcut::pick(2)}; }, [](NodeId) -> Message { return msg::Key{Shortcut::confirm()}; }},
                     std::nullopt});
    cases.push_back({approval(), "ls",
                     {[](NodeId) -> Message { return msg::Key{Shortcut::pick(0)}; }, [](NodeId) -> Message { return msg::Key{Shortcut::confirm()}; }},
                     ok(strs({ans::ALLOW_ONCE}))});
    cases.push_back({approval(), "ls",
                     {[](NodeId d) -> Message { return msg::Text{d, "  "}; }, [](NodeId) -> Message { return msg::Key{Shortcut::dismiss()}; }},
                     ok(strs({ans::DENY}))});
    cases.push_back({attrs({{"kind", "question"}, {"options", list({"red", "blue"})}}), "Color?",
                     {[](NodeId d) -> Message { return msg::Choose{d, 1}; }}, ok(strs({"blue"}))});
    cases.push_back({attrs({{"kind", "question"}, {"options", list({"a", "b", "c"})}, {"multi", true}}), "?",
                     {[](NodeId d) -> Message { return msg::Choose{d, 2}; }, [](NodeId) -> Message { return msg::Key{Shortcut::pick(0)}; },
                      [](NodeId d) -> Message { return msg::Submit{d}; }},
                     ok(strs({"a", "c"}))});
    cases.push_back({attrs({{"kind", "question"}}), "?", {[](NodeId d) -> Message { return msg::SubmitText{d}; }}, std::nullopt});
    cases.push_back({attrs({{"kind", "question"}}), "?",
                     {[](NodeId d) -> Message { return msg::Text{d, "green"}; }, [](NodeId d) -> Message { return msg::SubmitText{d}; }},
                     ok(strs({"green"}))});
    cases.push_back({attrs({{"kind", "question"}}), "?", {[](NodeId) -> Message { return msg::Key{Shortcut::dismiss()}; }}, ok({})});
    cases.push_back({attrs({{"kind", "plan"}}), "1. Do", {[](NodeId d) -> Message { return msg::Choose{d, 0}; }}, ok(strs({ans::APPROVE}))});
    cases.push_back({attrs({{"kind", "plan"}}), "1. Do",
                     {[](NodeId d) -> Message { return msg::Text{d, "split"}; }, [](NodeId d) -> Message { return msg::Choose{d, 2}; }},
                     ok(strs({ans::REJECT}), "split")});
    cases.push_back({attrs({{"kind", "plan"}}), "1. Do",
                     {[](NodeId d) -> Message { return msg::Choose{d, 1}; }, [](NodeId) -> Message { return msg::Key{Shortcut::dismiss()}; },
                      [](NodeId d) -> Message { return msg::Choose{d, 1}; }},
                     std::nullopt});
    cases.push_back({attrs({{"kind", "plan"}}), "1. Do",
                     {[](NodeId d) -> Message { return msg::Choose{d, 1}; }, [](NodeId d) -> Message { return msg::EditPlan{d, "1. Do\n2. Test"}; },
                      [](NodeId d) -> Message { return msg::Submit{d}; }},
                     ok(strs({ans::EDIT}), "1. Do\n2. Test")});
    cases.push_back({attrs({{"kind", "form"}, {"fields", form}}), "", {[](NodeId d) -> Message { return msg::Submit{d}; }}, std::nullopt});
    cases.push_back({attrs({{"kind", "form"}, {"fields", form}}), "",
                     {[](NodeId d) -> Message { return msg::Field{d, 0, field_input::Text{"v1"}}; },
                      [](NodeId d) -> Message { return msg::Field{d, 1, field_input::Choice{1}}; }, [](NodeId d) -> Message { return msg::Submit{d}; }},
                     ok(strs({R"({"tag":"v1","env":"prod"})"}))});
    (void)pick;
    for (std::size_t n = 0; n < cases.size(); ++n) {
        CAPTURE(n);
        auto& c = cases[n];
        auto [f, id] = fixture(std::move(c.a), c.body);
        auto s = state(f, false);
        std::vector<Intent> events;
        for (auto step : c.steps)
            if (auto i = s.update(step(id)).second) events.push_back(*i);
        std::vector<Intent> expected;
        if (c.expected) expected.push_back(*answer_intent(id, c.expected->first, c.expected->second));
        CHECK(events == expected);
    }
}

TEST_CASE("each decision kind maps to its card kind") {
    CHECK(kind_for(DecisionKind::Approval).settings_label() == "Approvals");
    CHECK(kind_for(DecisionKind::Question).settings_label() == "Questions");
    CHECK(kind_for(DecisionKind::Plan).settings_label() == "Plan reviews");
    CHECK(kind_for(DecisionKind::Form).settings_label() == "Forms");
    CHECK(kind_for(DecisionKind::Approval).notice() == "needs approval");
    CHECK(kind_for(DecisionKind::Question).notice() == "has a question");
    CHECK(kind_for(DecisionKind::Plan).notice() == "has a plan to review");
    CHECK(kind_for(DecisionKind::Form).notice() == "needs a form filled in");
    CHECK(answer_of(ApprovalChoice::AllowOnce) == "allow_once");
    CHECK(answer_of(ApprovalChoice::AllowSession) == "allow_session");
    CHECK(answer_of(ApprovalChoice::Deny) == "deny");
    CHECK(answer_of(PlanChoice::Approve) == "approve");
    CHECK(answer_of(PlanChoice::Edit) == "edit");
    CHECK(answer_of(PlanChoice::Reject) == "reject");
    CHECK(answer_summary(DecisionKind::Plan, strs({"edit"})) == "Approved with edits");
    CHECK(answer_summary(DecisionKind::Question, strs({"a", "b"})) == "a, b");
    CHECK(answer_summary(DecisionKind::Question, {}) == "Skipped");
}

TEST_CASE("cards lock while sending and read back who answered") {
    const std::string body = "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-pub fn old() {}\n+pub fn new() {}\n";
    auto [f, id] = fixture(attrs({{"kind", "approval"}}), body);
    auto s = state(f, false);
    const auto h = header(*s.card(id));
    CHECK(h.lead == "omp wants to run");
    CHECK(h.tool == std::optional<std::string>("bash"));
    REQUIRE(s.card(id)->diff);
    const auto rows = preview_rows(s.card(id)->diff->first, false);
    CHECK(std::ranges::any_of(rows, [](const auto& r) { return r.text == "+pub fn new() {}"; }));
    CHECK(std::ranges::any_of(rows, [](const auto& r) { return r.text == "-pub fn old() {}"; }));
    CHECK(approval_labels(s, id).front() == "Allow once");
    CHECK(s.update(msg::Choose{id, 0}).second.has_value());
    CHECK_FALSE(s.update(msg::Choose{id, 0}).second.has_value()); // locked until the DOM records it
    CHECK(status_of(s, id) == "Sending answer…");
    s.answer_failed(id);
    CHECK(s.focused() == id);
    const std::string key(40, 'k');
    f.commit(Draft::set(id, "status", "answered")).commit(Draft::set(id, "answer", list({ans::DENY}))).commit(Draft::set(id, "by", key));
    s.sync(f.dom);
    CHECK(status_of(s, id) == "Denied by kkkkkkkk");
    CHECK(s.focused() == std::nullopt);
    s.set_local_name("laptop");
    s.set_viewer(Viewer{key, false});
    CHECK(status_of(s, id) == "Denied by laptop");
    auto [f2, id2] = fixture(attrs({{"kind", "approval"}}), "ls");
    auto peer = state(f2, true);
    CHECK_FALSE(peer.update(msg::Choose{id2, 0}).second.has_value());
    CHECK(peer.focused() == std::nullopt);
    CHECK(status_of(peer, id2) == "View only: you can't answer this prompt");
    CHECK(node_name("timeout", "me", std::nullopt) == TIMEOUT_BY);
}

TEST_CASE("diff previews fold at PREVIEW_LINES") {
    CHECK(PREVIEW_LINES == 300);
    std::string body = "--- a/big.txt\n+++ b/big.txt\n@@ -0,0 +1,400 @@\n";
    for (int i = 0; i < 400; ++i) body += "+line " + std::to_string(i) + "\n";
    auto [f, id] = fixture(attrs({{"kind", "approval"}}), body);
    auto s = state(f, false);
    const auto* c = s.card(id);
    REQUIRE(c->diff);
    CHECK(c->diff->second == 403);
    const auto rows = preview_rows(c->diff->first, false);
    CHECK(rows.size() == PREVIEW_LINES);
    CHECK(rows.front().path);
    CHECK(rows.back().text == "+line 298");
    CHECK(preview_toggle(*c) == std::optional<std::string>("Show all 403 lines"));
    (void)s.update(msg::ToggleDiff{id});
    CHECK(preview_rows(s.card(id)->diff->first, s.card(id)->show_full).size() == 401);
    CHECK(preview_toggle(*s.card(id)) == std::optional<std::string>("Show fewer lines"));
    auto [f2, id2] = fixture(attrs({{"kind", "approval"}}), "--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n+b\n");
    CHECK(preview_toggle(*state(f2, false).card(id2)) == std::nullopt);
    auto [f3, id3] = fixture(attrs({{"kind", "approval"}}), "ls -la");
    CHECK_FALSE(state(f3, false).card(id3)->diff.has_value());
}

TEST_CASE("the tracker counts, notifies and fires each timeout once") {
    auto f = DomFixture::new_session();
    f.turn();
    const auto turn = f.last;
    const auto own = f.add(turn, ElementKind::Decision,
                           attrs({{"kind", "approval"}, {"default", list({ans::DENY})}, {"timeout_ms", 5000}, {"created", 1000}}), "rm -rf /tmp/x");
    const auto question = f.add(turn, ElementKind::Decision, attrs({{"kind", "question"}, {"options", list({"red", "green"})}}), "Color?");
    f.add(turn, ElementKind::Decision, attrs({{"kind", "plan"}, {"default", list({ans::EDIT})}, {"timeout_ms", 1}}), "# P");
    Tracker tracker;
    CHECK(tracker.observe(f.dom, 4000).empty()); // no notices in the foreground
    CHECK(tracker.badge("s1") == 3);
    CHECK(tracker.next_deadline() == 6000);
    CHECK(tracker.tick(5999).empty());
    const auto fired = tracker.tick(6000);
    CHECK(fired == std::vector<Expired>{{"s1", own, Answer{strs({ans::DENY}), std::nullopt, std::string(TIMEOUT_BY)}}});
    CHECK(tracker.tick(100000).empty()); // each default fires once; a misfit default never
    tracker.policy = Unattended{UnattendedView{std::nullopt, DecisionDefaultView{60, "GREEN", std::nullopt}, std::nullopt, std::nullopt}};
    CHECK(tracker.next_deadline() == 64000); // a policy set later counts from when the wait began
    const auto fired2 = tracker.tick(64000);
    REQUIRE(fired2.size() == 1);
    CHECK(fired2[0].decision == question);
    CHECK(fired2[0].answer.values == strs({"green"}));
    CHECK(tracker.tick(INT64_MAX).empty());
    tracker.background = true;
    const auto call = f.add(turn, ElementKind::Call, attrs({{"tool", "bash"}}), "");
    f.add(call, ElementKind::Decision, attrs({{"kind", "approval"}}), "cargo publish\n");
    const auto notices = tracker.observe(f.dom, 70000);
    REQUIRE(notices.size() == 1);
    CHECK(notices[0].heading == "A session needs approval");
    CHECK(notices[0].summary == "bash: cargo publish");
    CHECK(tracker.observe(f.dom, 70001).empty());
    f.commit(Draft::set(own, "status", "answered"));
    (void)tracker.observe(f.dom, 70002);
    CHECK(tracker.total() == 3);
    tracker.forget("s1");
    CHECK(tracker.total() == 0);
}

TEST_CASE("the notifier fires once per new decision") {
    auto f = DomFixture::new_session();
    f.turn();
    const auto turn = f.last;
    auto notifier = std::make_shared<CountingNotifier>();
    Desk desk("me", UnattendedView{}, notifier);
    (void)desk.update(desk_msg::Focused{false});
    const auto first = f.add(turn, ElementKind::Decision, attrs({{"kind", "question"}, {"title", "Pick"}}), "Color?");
    asio::io_context io;
    const auto raise = [&](std::vector<Notice> notices) {
        int ok = 0;
        for (auto& fut : desk.notify(notices))
            asio::co_spawn(io, std::move(fut), [&](std::exception_ptr, std::expected<void, NotifyError> r) { ok += r.has_value(); });
        io.restart();
        io.run();
        return ok;
    };
    CHECK(raise(desk.sync(f.dom, 10)) == 1);
    CHECK(raise(desk.sync(f.dom, 20)) == 0);
    const auto second = f.add(turn, ElementKind::Decision, attrs({{"kind", "plan"}}), "# Plan");
    CHECK(raise(desk.sync(f.dom, 30)) == 1);
    REQUIRE(notifier->seen.size() == 2);
    CHECK(notifier->ran == 2);
    CHECK(notifier->seen[0].decision == first);
    CHECK(notifier->seen[0].summary == "Pick: Color?");
    CHECK(notifier->seen[1].decision == second);
    CHECK(notifier->seen[1].heading == "A session has a plan to review");
    (void)desk.update(desk_msg::Focused{true});
    (void)f.add(turn, ElementKind::Decision, attrs({{"kind", "question"}}), "Again?");
    CHECK(desk.sync(f.dom, 40).empty()); // foreground: no notice
    CHECK(desk.badge("s1") == 3);
}

TEST_CASE("unattended presets are the six timeouts and schedule Expired at that duration") {
    CHECK(TIMEOUTS == std::array<std::uint64_t, 6>{30, 60, 120, 300, 900, 1800});
    for (auto secs : TIMEOUTS) {
        CAPTURE(secs);
        auto f = DomFixture::new_session();
        f.turn();
        const auto id = f.add(f.last, ElementKind::Decision, attrs({{"kind", "approval"}, {"created", 1000}}), "ls");
        Desk desk("me", UnattendedView{}, std::make_shared<NullNotifier>());
        (void)desk.update(desk_msg::Settings{unattended::Answer{DecisionKind::Approval, std::string(ans::DENY)}});
        const auto changed = desk.update(desk_msg::Settings{unattended::Timeout{DecisionKind::Approval, Duration{secs}}});
        CHECK(changed.size() == (secs == DEFAULT_TIMEOUT ? 0u : 1u));
        (void)desk.sync(f.dom, 1000);
        const auto due = 1000 + static_cast<std::int64_t>(secs) * 1000;
        CHECK(desk.tracker().next_deadline() == due);
        CHECK(desk.wants_clock());
        CHECK(desk.update(desk_msg::Tick{due - 1}).empty());
        const auto events = desk.update(desk_msg::Tick{due});
        REQUIRE(events.size() == 1);
        const auto& a = std::get<vi::desk_intent::Answer>(events[0]);
        CHECK(a.decision == id);
        CHECK(a.answer.values == strs({ans::DENY}));
        CHECK(a.answer.reason == "Nobody answered within " + Duration{secs}.to_string());
        CHECK(a.answer.by == TIMEOUT_BY);
    }
    const auto rows = settings_rows(Unattended{UnattendedView{DecisionDefaultView{45, "deny", std::nullopt}, {}, {}, {}}});
    REQUIRE(rows.size() == 4);
    CHECK(rows[0].timeouts.size() == 7); // the six presets and the hand-edited 45 s
    CHECK(rows[0].timeout == Duration{45});
    CHECK(rows[0].current->label == "Deny");
    CHECK(rows[1].choices.empty()); // questions take typed text
    CHECK(rows[2].current->label == "Wait for someone");
    CHECK(rows[2].timeouts.empty());
    CHECK(rows[3].choices.back().label == "Submit the form's defaults");
}

TEST_CASE("unattended policy answers per kind and the desk applies it") {
    auto f = DomFixture::new_session();
    f.turn();
    const auto turn = f.last;
    const std::string fields = R"({"properties":{"env":{"enum":["dev","prod"],"default":"prod"},"n":{"type":"integer"}},"required":["env"]})";
    std::vector<NodeId> ids;
    for (auto a : {attrs({{"kind", "approval"}, {"options", list({ans::ALLOW_ONCE, ans::ALLOW_SESSION, ans::DENY})}}),
                   attrs({{"kind", "question"}, {"options", list({"red"})}}), attrs({{"kind", "plan"}}),
                   attrs({{"kind", "form"}, {"fields", fields}})})
        ids.push_back(f.add(turn, ElementKind::Decision, a, ""));
    const auto decision = [&](std::size_t i) { return *Decision::from_dom(f.dom, ids[i]); };
    Unattended policy;
    policy.update(unattended::Enabled{true});
    const auto deny = *policy.answer(decision(0), "t");
    CHECK(deny.values == strs({ans::DENY}));
    CHECK(deny.reason == "Nobody answered within 5 min");
    CHECK(policy.answer(decision(2), "t")->values == strs({ans::REJECT}));
    CHECK_FALSE(policy.answer(decision(1), "t"));
    CHECK_FALSE(policy.answer(decision(3), "t"));
    const std::array<std::tuple<DecisionKind, std::string, std::string, std::size_t>, 4> edits{{
        {DecisionKind::Approval, std::string(ans::ALLOW_SESSION), std::string(ans::ALLOW_SESSION), 0},
        {DecisionKind::Question, "RED", "red", 1},
        {DecisionKind::Question, "blue", "blue", 1},
        {DecisionKind::Form, R"({"n":3})", R"({"env":"prod","n":3})", 3},
    }};
    for (const auto& [kind, configured, expected, i] : edits) {
        CAPTURE(configured);
        policy.update(unattended::Answer{kind, configured});
        CHECK(policy.answer(decision(i), "t")->values == std::vector<std::string>{expected});
    }
    policy.view.plan = DecisionDefaultView{5, std::string(ans::EDIT), std::nullopt};
    CHECK_FALSE(policy.answer(decision(2), "t")); // hand-edited misfits are not applied
    CHECK(Duration{30}.to_string() == "30 s");
    CHECK(Duration{120}.to_string() == "2 min");
    CHECK(Duration{90}.to_string() == "90 s");

    Desk desk("me", UnattendedView{}, std::make_shared<NullNotifier>());
    auto events = desk.update(desk_msg::Settings{unattended::Answer{DecisionKind::Approval, std::string(ans::ALLOW_ONCE)}});
    REQUIRE(events.size() == 1);
    CHECK(Unattended{std::get<vi::desk_intent::PolicyChanged>(events[0]).policy}.timeout_ms(DecisionKind::Approval) == 300000);
    (void)desk.update(desk_msg::Settings{unattended::Timeout{DecisionKind::Approval, Duration{30}}});
    CHECK(desk.update(desk_msg::Settings{unattended::Timeout{DecisionKind::Approval, Duration{30}}}).empty()); // no change
    (void)desk.sync(f.dom, 1000);
    desk.set_active("s1");
    CHECK(desk.badge("s1") == 4);
    CHECK(desk.focused() == ids[0]);
    CHECK(desk.update(desk_msg::Tick{30999}).empty());
    events = desk.update(desk_msg::Tick{31000});
    CHECK(events == std::vector<DeskIntent>{vi::desk_intent::Answer{"s1", ids[0], Answer{strs({ans::ALLOW_ONCE}), std::nullopt, std::string(TIMEOUT_BY)}}});
    CHECK(desk.focused() == ids[1]); // the sent card is locked
    events = desk.update(desk_msg::Card{"s1", msg::Choose{ids[1], 0}});
    REQUIRE(events.size() == 1);
    const auto& a = std::get<vi::desk_intent::Answer>(events[0]);
    CHECK(a.session == "s1");
    CHECK(a.answer.values == strs({"red"}));
    CHECK(a.answer.by == "me");
    CHECK(desk.update(desk_msg::Notified{std::unexpected(NotifyError{"no daemon"})}) ==
          std::vector<DeskIntent>{vi::desk_intent::NotifyFailed{"system notification failed: no daemon"}});
}

TEST_CASE("form schemas parse, validate and answer typed JSON") {
    auto parsed = Form::parse(
        R"({"properties":{"b":{"type":"string","default":"main"},"r":{"type":"integer"},"x":{"type":"number"},"f":{"type":"boolean"},"t":{"type":"array","items":{"enum":["a","b"]},"default":["b"]},"n":{"type":["string","null"]}},"required":["b","x","t"]})");
    REQUIRE(parsed);
    auto& form = *parsed;
    CHECK(form.fields[0].ty.kind == FieldType::Kind::String);
    CHECK(form.fields[1].ty.kind == FieldType::Kind::Integer);
    CHECK(form.fields[2].ty.kind == FieldType::Kind::Number);
    CHECK(form.fields[3].ty.kind == FieldType::Kind::Boolean);
    CHECK(form.fields[4].ty.kind == FieldType::Kind::Multi);
    CHECK(form.fields[5].ty.kind == FieldType::Kind::String);
    const std::array<std::tuple<std::size_t, std::string, FieldError>, 3> cases{{
        {0, " ", FieldError::Required},
        {1, "2.5", FieldError::NotInteger},
        {2, "abc", FieldError::NotNumber},
    }};
    for (const auto& [i, value, error] : cases) {
        CAPTURE(i);
        form.set(i, form::value::Text{value});
        CHECK(form.submit() == std::nullopt);
        CHECK(form.errors[i] == error);
    }
    CHECK(form::message(FieldError::Required) == "Required");
    CHECK(form::message(FieldError::NotNumber) == "Enter a number");
    CHECK(form::message(FieldError::NotInteger) == "Enter a whole number");
    for (const auto& [i, v] : std::array<std::pair<std::size_t, std::string>, 3>{{{0, "dev"}, {1, "5"}, {2, "0.5"}}})
        form.set(i, form::value::Text{v});
    form.toggle(4, 0);
    CHECK(form.submit() == std::optional<std::string>(R"({"b":"dev","r":5,"x":0.5,"f":false,"t":["a","b"]})"));
    const auto bad = Form::parse("not json");
    REQUIRE_FALSE(bad);
    CHECK(bad.error().kind == FormError::Kind::Json);
    CHECK(Form::parse(R"({"type":"string"})").error() == FormError::not_object());
    CHECK(Form::parse(R"({"properties":{"t":{"type":"array"}}})").error() == FormError::unsupported("t", "array"));
}

TEST_CASE("approval session option and navigation follow canonical options") {
    // Absent OPTIONS, allow_once/deny-only OPTIONS, and non-canonical labels hide allow_session.
    for (auto a : {attrs({{"kind", "approval"}}), attrs({{"kind", "approval"}, {"options", list({ans::ALLOW_ONCE, ans::DENY})}}),
                   attrs({{"kind", "approval"}, {"options", list({"Allow", "Always allow", "Reject"})}})}) {
        auto [f, id] = fixture(a, "ls");
        auto s = state(f, false);
        CHECK(approval_labels(s, id) == std::vector<std::string_view>{"Allow once", "Deny"});
        CHECK_FALSE(s.update(msg::Key{Shortcut::pick(2)}).second);
        CHECK_FALSE(s.update(msg::Key{Shortcut::confirm()}).second);
        CHECK_FALSE(s.update(msg::Choose{id, 2}).second);
        CHECK_FALSE(s.update(msg::Key{Shortcut::pick(1)}).second);
        CHECK(s.update(msg::Key{Shortcut::confirm()}).second == answer_intent(id, strs({ans::DENY}), std::nullopt));
    }
    // Canonical ALLOW_SESSION in OPTIONS shows all 3 choices with matching indices 1/2/3.
    auto [f, id] = fixture(attrs({{"kind", "approval"}, {"options", list({ans::ALLOW_ONCE, ans::ALLOW_SESSION, ans::DENY})}}), "ls");
    auto s = state(f, false);
    CHECK(approval_labels(s, id) == std::vector<std::string_view>{"Allow once", "Allow for this session", "Deny"});
    CHECK_FALSE(s.update(msg::Key{Shortcut::pick(1)}).second);
    CHECK(s.update(msg::Key{Shortcut::confirm()}).second == answer_intent(id, strs({ans::ALLOW_SESSION}), std::nullopt));
}

TEST_CASE("unsupported allow_session policy and default fall back to allow_once") {
    auto f = DomFixture::new_session();
    f.turn();
    const auto turn = f.last;
    const auto supported = f.add(turn, ElementKind::Decision,
                                 attrs({{"kind", "approval"}, {"options", list({ans::ALLOW_ONCE, ans::ALLOW_SESSION, ans::DENY})},
                                        {"default", list({ans::ALLOW_SESSION})}, {"timeout_ms", 1000}, {"created", 100}}),
                                 "ls");
    const auto no_options = f.add(turn, ElementKind::Decision,
                                  attrs({{"kind", "approval"}, {"default", list({ans::ALLOW_SESSION})}, {"timeout_ms", 1000}, {"created", 100}}), "ls");
    const auto once_only = f.add(turn, ElementKind::Decision,
                                 attrs({{"kind", "approval"}, {"options", list({ans::ALLOW_ONCE, ans::DENY})}, {"created", 100}}), "ls");
    const auto non_canonical = f.add(turn, ElementKind::Decision,
                                     attrs({{"kind", "approval"}, {"options", list({"Allow", "Always allow", "Reject"})},
                                            {"default", list({ans::ALLOW_SESSION})}, {"timeout_ms", 1000}, {"created", 100}}),
                                     "ls");
    const auto d = [&](NodeId id) { return *Decision::from_dom(f.dom, id); };
    CHECK(d(supported).default_answer("t")->values == strs({ans::ALLOW_SESSION}));
    CHECK(d(no_options).default_answer("t")->values == strs({ans::ALLOW_ONCE}));
    CHECK(d(non_canonical).default_answer("t")->values == strs({ans::ALLOW_ONCE}));

    const Unattended policy{UnattendedView{DecisionDefaultView{30, std::string(ans::ALLOW_SESSION), std::nullopt}, {}, {}, {}}};
    CHECK(policy.answer(d(supported), "t")->values == strs({ans::ALLOW_SESSION}));
    CHECK(policy.answer(d(no_options), "t")->values == strs({ans::ALLOW_ONCE}));
    CHECK(policy.answer(d(once_only), "t")->values == strs({ans::ALLOW_ONCE}));
    CHECK(policy.answer(d(non_canonical), "t")->values == strs({ans::ALLOW_ONCE}));

    Tracker tracker;
    tracker.policy = policy;
    (void)tracker.observe(f.dom, 100);
    const auto by_id = [](const std::vector<Expired>& fired, NodeId id) {
        return std::ranges::find_if(fired, [&](const auto& e) { return e.decision == id; })->answer.values;
    };
    const auto own_fired = tracker.tick(1100);
    CHECK(by_id(own_fired, supported) == strs({ans::ALLOW_SESSION}));
    CHECK(by_id(own_fired, no_options) == strs({ans::ALLOW_ONCE}));
    CHECK(by_id(own_fired, non_canonical) == strs({ans::ALLOW_ONCE}));
    const auto policy_fired = tracker.tick(30100);
    CHECK(by_id(policy_fired, once_only) == strs({ans::ALLOW_ONCE}));
}
