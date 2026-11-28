#include <doctest/doctest.h>

#include "loa/updater/decision.hpp"
#include "loa/updater/peers.hpp"

using namespace loa::updater;
using loa::settings::CheckResult;
using loa::settings::GateCheck;
using loa::settings::GateReport;
using loa::settings::HoldChoice;
using loa::settings::PeerInfo;
using loa::settings::Standing;

namespace {

Version v(std::string_view s) { return *Version::parse(s); }
GateReport report(std::string_view to, bool clean) {
    const auto check = clean ? CheckResult::pass(GateCheck::Changelog, {}) : CheckResult::harmful(GateCheck::Changelog, {});
    return GateReport{"omp", v("1.0.0"), v(to), {check}};
}
PeerInfo peer(std::string node, std::optional<Version> version, std::vector<std::string> alpns) {
    return PeerInfo{std::move(node), std::nullopt, std::move(version), std::move(alpns)};
}

} // namespace

TEST_CASE("decisions") {
    UpdaterState state;
    CHECK(decision::record(state, report("1.1.0", true), 5));
    CHECK(state.pending_swap.at("omp") == v("1.1.0"));
    CHECK_FALSE(decision::record(state, report("1.1.0", false), 6));
    CHECK(state.pending_swap.empty());
    CHECK(state.held.at("omp").held_at_unix == 6);

    struct Case {
        HoldChoice choice;
        std::optional<Version> swap, pin, wait;
    };
    const std::vector<Case> cases{
        {HoldChoice::UpdateAnyway, v("1.1.0"), {}, {}},
        {HoldChoice::PinCurrent, {}, v("1.0.0"), {}},
        {HoldChoice::Wait, {}, v("0.9.0"), v("1.1.0")},
    };
    for (const auto& c : cases) {
        auto copy = state;
        copy.pins["omp"] = v("0.9.0");
        CHECK(decision::apply_choice(copy, "omp", v("1.0.0"), c.choice) == c.swap);
        CHECK((copy.pins.contains("omp") ? std::optional(copy.pins["omp"]) : std::nullopt) == c.pin);
        CHECK((copy.waits.contains("omp") ? std::optional(copy.waits["omp"]) : std::nullopt) == c.wait);
        CHECK(copy.held.empty());
        CHECK((copy.pending_swap.contains("omp") ? std::optional(copy.pending_swap["omp"]) : std::nullopt) == c.swap);
    }
    UpdaterState empty;
    CHECK_FALSE(decision::apply_choice(empty, "omp", v("1.0.0"), HoldChoice::Wait).has_value());
    state.waits["omp"] = v("1.1.0");
    CHECK_FALSE(decision::offer_allowed(state, "omp", v("1.1.0")));
    CHECK(decision::offer_allowed(state, "omp", v("1.2.0")));
    state.pins["omp"] = v("1.0.0");
    CHECK_FALSE(decision::offer_allowed(state, "omp", v("1.2.0")));
}

TEST_CASE("ALPN labels parse") {
    CHECK(peers::parse_alpn("omp/session/2") == std::optional(std::pair<std::string_view, std::uint32_t>{"session", 2}));
    CHECK(peers::parse_alpn("omp/control/10") == std::optional(std::pair<std::string_view, std::uint32_t>{"control", 10}));
    for (const auto* bad : {"h3", "omp/session", "omp/session/x", "omp/session/+1", "omp//1", "other/session/1"})
        CHECK_FALSE(peers::parse_alpn(bad).has_value());
}

TEST_CASE("peers sort by standing and shared protocols") {
    const std::vector<std::string> local{"omp/control/1", "omp/session/1", "omp/session/2", "omp/surface/1"};
    const std::vector<std::string> all{"omp/control/1", "omp/session/1", "omp/surface/1"};
    const std::vector<PeerInfo> input{
        peer("z-current", v("2.3.4"), all),
        peer("y-patch", v("2.3.1"), all),
        peer("x-major", v("1.9.9"), all),
        peer("w-mute", v("2.3.4"), {"omp/control/1"}),
        peer("s-newer", v("3.0.0"), {"omp/control/1", "omp/session/3", "omp/surface/1"}),
        peer("v-minor", v("2.1.0"), all),
        peer("u-unknown", {}, all),
        peer("t-ahead", v("3.0.0"), all),
    };
    const auto rows = peers::classify(v("2.3.4"), local, input);
    std::vector<std::pair<std::string, bool>> order;
    for (const auto& row : rows) order.emplace_back(row.node, row.can_talk);
    CHECK(order == std::vector<std::pair<std::string, bool>>{{"w-mute", false}, {"s-newer", false}, {"x-major", true}, {"v-minor", true},
                                                              {"y-patch", true}, {"u-unknown", true}, {"z-current", true}, {"t-ahead", true}});
    CHECK(rows[2].standing == Standing{Standing::Kind::Behind, 1, 0, 0});
    CHECK(rows[3].standing == Standing{Standing::Kind::Behind, 0, 2, 0});
    CHECK(rows[4].standing == Standing{Standing::Kind::Behind, 0, 0, 3});
    const auto& session = *std::ranges::find_if(rows[2].protocols, [](const auto& p) { return p.family == "session"; });
    CHECK(session.shared == 1);
    CHECK(session.local == 2);
    CHECK(session.peer == 1);
    const auto& newer = *std::ranges::find_if(rows[1].protocols, [](const auto& p) { return p.family == "session"; });
    CHECK_FALSE(newer.shared);
    CHECK(newer.peer == 3);
}

TEST_CASE("multiple advertised versions require an exact label") {
    const std::vector<std::string> local{"omp/control/1", "omp/session/1", "omp/surface/1"};
    const std::vector<PeerInfo> input{
        peer("overlap", {}, {"omp/control/1", "omp/session/1", "omp/session/2", "omp/surface/1"}),
        peer("no_match", {}, {"omp/control/1", "omp/session/2", "omp/session/3", "omp/surface/1"}),
        peer("different_spelling", {}, {"omp/control/1", "omp/session/01", "omp/surface/1"}),
    };
    const auto rows = peers::classify(v("1.4.0"), local, input);
    const auto& overlap = *std::ranges::find_if(rows, [](const auto& r) { return r.node == "overlap"; });
    CHECK(overlap.can_talk);
    const auto& session = *std::ranges::find_if(overlap.protocols, [](const auto& p) { return p.family == "session"; });
    CHECK(session.shared == 1);
    CHECK(session.local == 1);
    CHECK(session.peer == 2);
    for (const auto* node : {"no_match", "different_spelling"}) {
        const auto& row = *std::ranges::find_if(rows, [=](const auto& r) { return r.node == node; });
        CHECK_FALSE(row.can_talk);
        CHECK_FALSE(std::ranges::find_if(row.protocols, [](const auto& p) { return p.family == "session"; })->shared.has_value());
    }
}
