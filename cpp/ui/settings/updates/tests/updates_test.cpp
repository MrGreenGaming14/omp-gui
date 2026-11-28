// Port of the tests in `crates/loa-ui-settings/src/updates.rs` (render tests not ported).

#include <doctest/doctest.h>

#include "loa/ui/settings/updates/state.hpp"

namespace updates_tests {

using namespace loa::ui::settings::updates;
namespace in = vu::intent;
namespace pk = vu::product_kind;
using vu::CheckResult;
using vu::GateCheck;
using vu::GateReport;
using vu::Verdict;

Product product(std::string id, vu::ProductKind kind, std::optional<std::string> current) {
    Product p;
    p.id = id;
    p.label = id;
    p.kind = std::move(kind);
    p.source = "GitHub can1357/oh-my-pi";
    p.current = std::move(current);
    p.installed = vu::installed::Running{};
    p.last_check_unix = 100;
    return p;
}

GateReport report() {
    return GateReport{"19.0.0", "19.1.0",
                      {CheckResult{GateCheck::ToolSchemas, Verdict::Harmful, {"- edit: tool removed"}}, CheckResult{GateCheck::ContractTests, Verdict::Harmful, {}},
                       CheckResult{GateCheck::RpcProtocol, Verdict::Pass, {}}}};
}

// omp held by a harmful report, the app with a pending swap and a staged offer, Codex up to
// date with a rollback, Claude Code not installed.
Data data() {
    auto omp = product("omp", pk::Omp{}, "19.0.0");
    omp.report = report();
    omp.held = vu::HeldUpdate{"19.1.0", report()};
    omp.changelog = "- faster startup";
    auto app = product("app", pk::App{}, "0.1.0");
    app.pending_swap = "0.2.0";
    app.offer = "0.2.0";
    app.staged = {"0.2.0"};
    app.signature = vu::signature_status::Verified{};
    auto codex = product("codex", pk::Harness{"codex"}, "1.0.0");
    codex.active = vu::Active{"1.0.0", "0.9.0"};
    auto claude = product("claude", pk::Harness{"claude"}, std::nullopt);
    claude.installed = vu::installed::Missing{"not on PATH"};
    claude.newest = "2.0.14";
    claude.offer = "2.0.14";
    std::vector<PeerStanding> peers{PeerStanding{"laptop", "laptop", "0.1.0", vu::standing::Behind{0, 1, 0}, {{"control", 1}, {"session", 1}}, true}};
    return Data{{omp, app, codex, claude}, 42, peers, false, 400};
}

TEST_CASE("status lines follow the first matching rule") {
    const auto d = data();
    std::vector<std::pair<Tone, std::string>> lines;
    for (const auto& p : d.products) lines.push_back(status(p, false));
    CHECK(lines == std::vector<std::pair<Tone, std::string>>{
                       {Tone::Danger, "19.1.0 held: 2 harmful checks. Update anyway, pin or wait."},
                       {Tone::Ok, "Checks passed; ready to swap to 0.2.0"},
                       {Tone::Ok, "Up to date"},
                       {Tone::Muted, "Not installed; latest is 2.0.14"},
                   });
    CHECK(status(d.products[1], true).second == "Checks passed; 0.2.0 swaps in when the running turn ends");
    auto claude = d.products[3];
    claude.current = "2.0.0";
    claude.staged = {"2.0.14"};
    CHECK(status(claude, false).second == "2.0.14 downloaded; run the gate checks");
    claude.restart_required = true;
    CHECK(status(claude, false) == std::pair{Tone::Warn, std::string("Installed; restart the app to use it")});
    auto idle = product("x", pk::Omp{}, "1.0.0");
    idle.last_check_unix.reset();
    CHECK(status(idle, false).second == "Not checked yet");
    CHECK(standing(d.peers[0]) == std::pair{Tone::Warn, std::string("behind by 1 minor")});
}

TEST_CASE("every status rule in order") {
    auto p = product("p", pk::Omp{}, "1.0.0");
    p.restart_required = true;
    p.active = vu::Active{"1.0.0", std::nullopt};
    CHECK(status(p, false) == std::pair{Tone::Warn, std::string("Installed 1.0.0; restart the app to use it")});
    p = product("p", pk::Omp{}, "1.0.0");
    auto one = report();
    one.checks.erase(one.checks.begin());
    p.held = vu::HeldUpdate{"2.0.0", one};
    CHECK(status(p, false) == std::pair{Tone::Danger, std::string("2.0.0 held: 1 harmful check. Update anyway, pin or wait.")});
    p = product("p", pk::Omp{}, std::nullopt);
    p.installed = vu::installed::Unknown{};
    CHECK(status(p, false) == std::pair{Tone::Muted, std::string("Looking for the installed version\u2026")});
    p.installed = vu::installed::Running{};
    CHECK(status(p, false) == std::pair{Tone::Muted, std::string("Not installed")});
    p = product("p", pk::Omp{}, "1.0.0");
    p.offer = "1.1.0";
    CHECK(status(p, false) == std::pair{Tone::Warn, std::string("Update available: 1.1.0")});
    p.offer.reset();
    p.pin = "1.0.0";
    CHECK(status(p, false) == std::pair{Tone::Muted, std::string("Pinned to 1.0.0")});
    p.pin.reset();
    p.waiting_past = "1.1.0";
    CHECK(status(p, false) == std::pair{Tone::Muted, std::string("Waiting for a version after 1.1.0")});
    CHECK(STATUS_RULES.size() == 12);
    CHECK(STATUS_RULES.back().line(p, false) == std::optional<std::string>("Up to date"));
}

TEST_CASE("standing lines") {
    PeerStanding p{"n", std::nullopt, std::nullopt, vu::standing::Behind{2, 0, 0}, {}, true};
    CHECK(standing(p).second == "behind by 2 majors");
    p.standing = vu::standing::Behind{0, 0, 0};
    CHECK(standing(p).second == "behind (prerelease)");
    p.standing = vu::standing::Ahead{};
    CHECK(standing(p) == std::pair{Tone::Muted, std::string("ahead of this app")});
    p.can_talk = false;
    CHECK(standing(p) == std::pair{Tone::Danger, std::string("can't talk: no shared protocol")});
}

TEST_CASE("steps mark products busy until finished") {
    auto start = [](Intent e) { return Message::local(local::Start{std::move(e)}); };
    auto choose = [](std::string id, HoldChoice c) { return Message::event(in::Choose{std::move(id), c}); };
    const std::vector<std::pair<std::vector<Message>, std::vector<Intent>>> cases{
        {{start(in::Swap{"app"}), start(in::Check{"app"}), start(in::Rollback{"codex"}), choose("app", HoldChoice::Wait)}, {in::Swap{"app"}, in::Rollback{"codex"}}},
        {{start(in::Gate{"omp"}), start(in::CheckAll{}), start(in::CheckAll{})}, {in::Gate{"omp"}, in::CheckAll{}}},
        {{choose("omp", HoldChoice::PinCurrent), Message::event(in::Unpin{"nope"}), Message::event(in::Swap{"app"}), start(in::Unpin{"omp"})},
         {in::Choose{"omp", HoldChoice::PinCurrent}}},
    };
    for (std::size_t i = 0; i < cases.size(); ++i) {
        CAPTURE(i);
        State s(data());
        std::vector<Intent> events;
        for (const auto& m : cases[i].first)
            if (auto e = s.update(m)) events.push_back(std::move(*e));
        CHECK(events == cases[i].second);
    }
    State s(data());
    (void)s.update(start(in::Gate{"omp"}));
    (void)s.update(start(in::CheckAll{}));
    CHECK(s.busy("omp") == Step::RunningChecks);
    CHECK(s.busy("codex") == Step::Checking);
    CHECK(!s.update(choose("omp", HoldChoice::Wait)));
    s.finish("omp");
    CHECK(s.update(choose("omp", HoldChoice::Wait)) == std::optional<Intent>(in::Choose{"omp", HoldChoice::Wait}));
    CHECK(s.update(start(in::Stage{"omp"})) == std::optional<Intent>(in::Stage{"omp"}));
    s.finish_all();
    CHECK(!s.busy("codex"));
}

TEST_CASE("toggle and snapshots keep the open product") {
    State s(data());
    (void)s.update(Message::local(local::Toggle{"omp"}));
    CHECK(s.selected == std::optional<std::string>("omp"));
    auto d = data();
    s.set_data(d);
    CHECK(s.selected == std::optional<std::string>("omp"));
    d.products.erase(d.products.begin());
    s.set_data(d);
    CHECK(!s.selected);
    (void)s.update(Message::local(local::Toggle{"app"}));
    (void)s.update(Message::local(local::Toggle{"app"}));
    CHECK(!s.selected);
}

} // namespace updates_tests
