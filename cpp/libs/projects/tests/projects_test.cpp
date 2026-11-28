// Port of `crates/loa-projects/src/tests.rs` (access, tools, search, read, rank, digests) and the
// `rank.rs` unit test.

#include <doctest/doctest.h>

#include <cmath>
#include <thread>

#include "loa/runtime/tempfile.hpp"

#include "loa/projects/context.hpp"
#include "loa/projects/digest.hpp"
#include "loa/projects/digester.hpp"
#include "loa/projects/rank.hpp"
#include "loa/projects/read.hpp"
#include "loa/projects/rebuild.hpp"
#include "loa/projects/search.hpp"
#include "loa/projects/settings.hpp"
#include "loa/projects/tools.hpp"
#include "loa/runtime/json.hpp"
#include "testing.hpp"

using namespace loa::projects;
using namespace loa::projects::test;
namespace json = loa::runtime::json;
using loa::settings::DigestState;

namespace {

struct Fixture {
    Builder a{"a", "loa", 100};
    Builder b{"b", "loa", 200};
    Builder c{"c", "web", 300};
    std::shared_ptr<MemorySource> source;

    // a: an older session with a todo and a cargo call; b: newer, digested; c: another project.
    Fixture() {
        auto t = a.turn("Set up the Iroh endpoint", "Bound it to port 31415.", 110);
        a.call(t, "bash", "cargo test -p net", "ok: 12 passed");
        a.commit(Draft::insert(a.root, K::Todo, attrs({{attr::STATE, Value("pending")}}), "write docs"));
        b.turn("Theme the sidebar", "Dark tokens done.", 210);
        b.digest("Themed the sidebar with dark tokens.");
        c.turn("iroh elsewhere", "no", 310);
        source = std::make_shared<MemorySource>(std::initializer_list<const Builder*>{&a, &b, &c});
    }
};

std::vector<std::string> ids(const MemorySource& source, const Access& access) {
    std::vector<std::string> out;
    const auto visible = visible_sessions(source, access, "loa").value();
    for (const auto& s : visible) out.push_back(s.id);
    return out;
}

bool contains(const std::vector<std::string>& v, std::string_view x) { return std::ranges::find(v, x) != v.end(); }

} // namespace

TEST_CASE("access_decides_what_every_path_sees") {
    Fixture f;
    const LocalOnly local;
    const Only only_a({"a"});
    const Only none({});
    const std::pair<const Access*, std::vector<std::string>> cases[] = {
        {&local, {"b", "a"}}, {&only_a, {"a"}}, {&none, {}}};
    auto store_dir = loa::runtime::TempDir::create().value();
    const FileSettingsStore store(store_dir.path());
    for (const auto& [access, visible] : cases) {
        CHECK(ids(*f.source, *access) == visible);
        SearchQuery query;
        query.text = "iroh";
        auto hits = search(*f.source, *access, "loa", query).value().hits;
        for (const auto& h : hits) CHECK(contains(visible, h.session));
        auto read = read_subtree(*f.source, *access, "loa", ReadRequest{"b", std::nullopt, 0, 0});
        CHECK(read.has_value() == contains(visible, "b"));
        // Context only offers digests of visible sessions.
        auto packed = build_context(*f.source, *access, store, ContextRequest{"loa", std::nullopt, "", 4000, 1000}).value();
        for (const auto& d : packed.digests) CHECK(contains(visible, d.candidate.session));
        CHECK(packed.digests.size() == (contains(visible, "b") ? 1u : 0u));
    }
    auto other = read_subtree(*f.source, local, "loa", ReadRequest{"c", std::nullopt, 0, 0});
    REQUIRE_FALSE(other.has_value());
    CHECK(other.error().kind == ErrorKind::SessionNotFound);
    // Missing, other-project and denied sessions read the same.
    auto missing = read_subtree(*f.source, local, "loa", ReadRequest{"zz", std::nullopt, 0, 0});
    auto denied = read_subtree(*f.source, only_a, "loa", ReadRequest{"b", std::nullopt, 0, 0});
    CHECK(other.error().to_string() == "session c not found");
    CHECK(missing.error().kind == ErrorKind::SessionNotFound);
    CHECK(denied.error() == Error::session_not_found("b"));
}

TEST_CASE("host_tools_search_and_read_respect_session_boundaries") {
    Fixture f;
    loa::runtime::Executor exec(1);
    auto tools = tool_set(f.source, std::make_shared<LocalOnly>());
    const auto call = [&](const loa::contracts::ToolSet& set, std::string tool, std::string args) {
        return block_on(exec, set.call("b", std::move(tool), std::move(args)));
    };

    auto result = call(*tools, "project_search", R"({"query":"iroh"})").value();
    auto page = json::parse(result).value();
    const auto& hits = *page.find("hits")->as_array();
    REQUIRE(hits.size() == 1);
    CHECK(*hits[0].find("session")->as_string() == "a");
    const auto args = json::dump(json::Object{{"session", *hits[0].find("session")}, {"node", *hits[0].find("node")}});
    auto text = call(*tools, "project_read", args).value();
    CHECK(text.find("Iroh endpoint") != std::string::npos);

    auto own = call(*tools, "project_search", R"({"query":"dark tokens"})").value();
    CHECK(json::parse(own).value().find("total")->as_u64() == 0u);
    own = call(*tools, "project_search", R"({"query":"dark tokens","sessions":["b"]})").value();
    CHECK(json::parse(own).value().find("total")->as_u64() > 0u);

    auto restricted = tool_set(f.source, std::make_shared<Only>(std::vector<std::string>{"b"}));
    auto denied = call(*restricted, "project_search", R"({"query":"iroh"})").value();
    CHECK(json::parse(denied).value().find("total")->as_u64() == 0u);
    CHECK_FALSE(call(*restricted, "project_read", args).has_value());
    CHECK_FALSE(call(*tools, "project_read", R"({"session":"c"})").has_value());
    const std::pair<std::string, std::string> bad[] = {
        {"project_search", "{"},
        {"project_search", R"({"element":"not-an-element"})"},
        {"project_search", R"({"unexpected":true})"},
        {"project_read", R"({"node":1})"},
        {"project_read", R"({"session":"a","node":"wrong"})"},
    };
    for (const auto& [tool, a] : bad) {
        INFO(tool << ": " << a);
        CHECK_FALSE(call(*tools, tool, a).has_value());
    }
}

TEST_CASE("search_cases") {
    Fixture f;
    const auto q = [](std::optional<std::string> text, TextMode mode, std::optional<K> kind,
                      std::vector<AttrFilter> filters = {}) {
        SearchQuery query;
        query.text = std::move(text);
        query.mode = mode;
        query.kind = kind;
        query.attrs = std::move(filters);
        return query;
    };
    const auto phrase = TextMode::Phrase, all = TextMode::AllWords;
    using Hits = std::vector<std::pair<std::string, K>>;
    const std::pair<SearchQuery, Hits> cases[] = {
        {q("IROH endpoint", phrase, std::nullopt), {{"a", K::Message}}},
        {q("port endpoint", phrase, std::nullopt), {}},
        {q("31415 bound", all, std::nullopt), {{"a", K::Message}}},
        {q("12 passed", phrase, K::Call), {{"a", K::Call}}},
        {q(std::nullopt, phrase, K::Todo), {{"a", K::Todo}}},
        {q(std::nullopt, phrase, std::nullopt, {{"tool", "BA"}}), {{"a", K::Call}}},
        {q("dark tokens", phrase, K::Digest), {{"b", K::Digest}}},
    };
    for (const auto& [query, want] : cases) {
        auto page = search(*f.source, LocalOnly{}, "loa", query).value();
        Hits got;
        for (const auto& h : page.hits) got.emplace_back(h.session, h.kind);
        INFO(query.text.value_or("<none>"));
        CHECK(got == want);
    }
    auto blank = search(*f.source, LocalOnly{}, "loa", q("  ", phrase, std::nullopt));
    REQUIRE_FALSE(blank.has_value());
    CHECK(blank.error().kind == ErrorKind::Arguments);
    // Pages cover every hit exactly once.
    auto query = q(std::nullopt, phrase, K::Message);
    query.limit = 3;
    std::vector<std::pair<std::string, NodeId>> seen;
    int pages = 0;
    for (;;) {
        auto page = search(*f.source, LocalOnly{}, "loa", query).value();
        CHECK(page.total == 4);
        for (auto& h : page.hits) seen.emplace_back(h.session, h.node);
        ++pages;
        if (!page.next_cursor) break;
        query.cursor = page.next_cursor;
    }
    seen.erase(std::unique(seen.begin(), seen.end()), seen.end());
    CHECK(seen.size() == 4);
    CHECK(pages == 2);
}

TEST_CASE("read_pages_cover_the_rendering_exactly") {
    Builder a("a", "loa", 1);
    std::string user;
    for (int i = 0; i < 40; ++i) user += "héllo wörld ✓ ";
    auto t = a.turn(user, "ok", 2);
    MemorySource source{&a};
    const auto whole = render_subtree(a.dom(), t);
    for (std::size_t max_bytes : {1u, 7u, 64u, 0u, 100000u}) {
        std::string got;
        std::optional<std::size_t> offset = 0;
        while (offset) {
            auto page = read_subtree(source, LocalOnly{}, "loa", ReadRequest{"a", t, *offset, max_bytes}).value();
            CHECK(page.offset == *offset);
            CHECK(page.total_bytes == whole.size());
            CHECK(!page.text.empty());
            CHECK(page.text.size() <= std::max<std::size_t>(read_pages::PAGES.size(max_bytes), 4));
            got += page.text;
            offset = page.next_offset;
        }
        INFO("max_bytes " << max_bytes);
        CHECK(got == whole);
    }
    auto missing = read_subtree(source, LocalOnly{}, "loa", ReadRequest{"a", NodeId{9999}, 0, 0});
    REQUIRE_FALSE(missing.has_value());
    CHECK(missing.error().kind == ErrorKind::NodeNotFound);
    CHECK(missing.error().to_string() == "node 9999 not found in session a");
}

namespace {

Candidate candidate(std::string_view session, std::string_view summary, std::int64_t last, bool excluded) {
    Builder b(session, "loa", last);
    b.turn("x", "y", last);
    b.digest(summary);
    auto c = Candidate::from_dom(b.dom()).value();
    c.excluded = excluded;
    return c;
}

} // namespace

TEST_CASE("rank_cases") {
    const std::int64_t day = 24LL * 3600 * 1000;
    const auto pool = [&] {
        return std::vector<Candidate>{
            candidate("old", "Configured the iroh relay and iroh endpoint", 0, false),
            candidate("new", "Styled buttons", 10 * day, false),
            candidate("off", "iroh iroh iroh", 10 * day, true),
        };
    };
    const auto order = [&](std::string_view query, double weight, std::size_t budget) {
        auto params = RankParams::make(10 * day, budget);
        params.relevance_weight = weight;
        std::vector<std::string> out;
        for (auto& r : rank_and_pack(pool(), query, params)) out.push_back(r.candidate.session);
        return out;
    };
    CHECK(order("iroh", 1.0, 10000) == std::vector<std::string>{"old", "new"});
    CHECK(order("iroh", 0.0, 10000) == std::vector<std::string>{"new", "old"});
    CHECK(order("", 0.7, 10000) == std::vector<std::string>{"new", "old"});
    CHECK(order("iroh", 1.0, 1).empty());
    const auto c = candidate("x", "iroh", 0, false);
    const std::vector<std::string> q{"iroh"};
    std::vector<std::pair<double, std::unique_ptr<Scorer>>> parts;
    parts.emplace_back(0.5, std::make_unique<Bm25>(Bm25::fit({c}, q)));
    parts.emplace_back(0.5, std::make_unique<Recency>(day, day));
    const Weighted mix(std::move(parts));
    CHECK(std::abs(mix.score(q, c) - 0.75) < 1e-9);
}

TEST_CASE("bm25_orders_multiple_terms_across_sessions") {
    const auto bare = [](std::string_view session, std::string_view summary) {
        Candidate c;
        c.session = std::string(session);
        c.digest.content.summary = std::string(summary);
        return c;
    };
    std::vector<Candidate> candidates{bare("unrelated", "unrelated"), bare("common-once", "common"),
                                      bare("common-twice", "common common"), bare("both", "common rare")};
    auto params = RankParams::make(0, SIZE_MAX);
    params.relevance_weight = 1.0;
    auto ranked = rank_and_pack(candidates, "common rare", params);
    std::vector<std::string> sessions;
    for (auto& r : ranked) sessions.push_back(r.candidate.session);
    CHECK(sessions == std::vector<std::string>{"both", "common-twice", "common-once", "unrelated"});
    for (std::size_t i = 1; i < ranked.size(); ++i) CHECK(ranked[i - 1].score > ranked[i].score);
}

TEST_CASE("digest_staleness_and_rebuilds") {
    Builder s("s", "loa", 1);
    CHECK(decide(s.dom()) == Decision::Empty);
    CHECK(digest_state(s.dom()) == DigestState::Missing);
    s.turn("Fix the build", "Done.", 2);
    CHECK(decide(s.dom()) == Decision::Rebuild);
    s.digest("Fixed it.");
    CHECK(decide(s.dom()) == Decision::UpToDate);
    CHECK(digest_state(s.dom()) == DigestState::Fresh);
    s.turn("More", "Sure.", 3);
    CHECK(digest_state(s.dom()) == DigestState::Stale);
    s.commit(*exclude_draft(s.dom(), true));
    CHECK(decide(s.dom()) == Decision::Excluded);
    s.commit(*exclude_draft(s.dom(), false));

    // A burst of triggers costs one call; a failure keeps the old digest, flagged stale.
    for (bool fail : {false, true}) {
        asio::io_context context;
        auto source = std::make_shared<MemorySource>(std::initializer_list<const Builder*>{&s});
        auto fake = std::make_shared<Fake>(fail);
        RebuildDriver driver{source, source, Digester(fake, PromptLimits{}), std::chrono::milliseconds(20), nullptr};
        auto [tx, out] = std::move(driver).spawn(context);
        std::thread runner([&] { context.run(); });
        for (int i = 0; i < 3; ++i) CHECK(tx.send("s"));
        auto outcome = out->recv(std::chrono::seconds(10));
        REQUIRE(outcome.has_value());
        CHECK(fake->calls.load() == 1);
        CHECK(std::holds_alternative<rebuild_outcome::Failed>(*outcome) == fail);
        auto digest = digest_of(s.dom()).value();
        CHECK(digest.content.summary == "digest 1");
        CHECK(digest_state(s.dom()) == (fail ? DigestState::Stale : DigestState::Fresh));
        if (!fail) {
            CHECK(digest.model == std::optional<std::string>("smol"));
            s.turn("Again", "Ok.", 4);
        }
        { auto drop = std::move(tx); }
        CHECK_FALSE(out->recv(std::chrono::seconds(10)).has_value());
        runner.join();
    }
}

TEST_CASE("digest_answers_are_validated") {
    Builder s("s", "loa", 1);
    auto t1 = s.turn("one", "a", 2);
    s.turn("two", "b", 3);
    auto prompt = build_prompt(s.dom(), PromptLimits{}).value();
    CHECK(prompt.text.find("### T2") != std::string::npos);
    const auto answer = [](std::string_view summary, std::string_view pointers) {
        return "x {\"summary\":\"" + std::string(summary) +
               "\",\"decisions\":[\" d \"],\"files\":[],\"open_items\":[],\"pointers\":[" + std::string(pointers) + "]} y";
    };
    auto ok = parse_answer(answer("did it", R"("T1","t1")"), prompt).value();
    CHECK(ok.pointers == std::vector<NodeId>{t1});
    CHECK(ok.decisions == std::vector<std::string>{"d"});
    for (const auto& bad : {answer("", ""), answer("s", R"("T3")"), answer("s", R"("X1")"), std::string("no json")}) {
        INFO(bad);
        auto r = parse_answer(bad, prompt);
        REQUIRE_FALSE(r.has_value());
        CHECK(r.error().kind == ErrorKind::InvalidAnswer);
    }
    Builder empty("e", "loa", 1);
    CHECK(build_prompt(empty.dom(), PromptLimits{}).error().kind == ErrorKind::NothingToDigest);
}
