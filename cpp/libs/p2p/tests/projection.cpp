#include <doctest/doctest.h>

#include <format>

#include "loa/bus/bus.hpp"
#include "loa/core/dom.hpp"
#include "loa/p2p/protocols/projection.hpp"
#include "loa/p2p/protocols/session.hpp"
#include "loa/p2p/sessions.hpp"
#include "loa/runtime/json.hpp"

using namespace loa;
using core::Draft;
using core::ElementKind;
using core::NodeId;
using core::Value;
using p2p::Access;
using p2p::BusSessions;
using p2p::LocalSession;
using p2p::Mirror;
using p2p::Projection;
using p2p::Update;
using p2p::draft_from_json;
using p2p::draft_to_json;
namespace attr = core::attr;

namespace {

struct Discard final : bus::Persist {
    std::expected<void, bus::PersistError> append(const core::Patch&) override { return {}; }
};

// View on the whole session (none) or on one turn's subtree.
struct View final : Access {
    explicit View(std::optional<NodeId> t) : turn(t) {}
    std::optional<NodeId> turn;
    bool allows(const bus::NodeKey&, const core::Dom& dom, trust::Action action,
                std::optional<NodeId> node) const override {
        if (action != trust::Action::view) return false;
        if (!turn) return true;
        if (!node) return false;
        if (*node == *turn) return true;
        for (const auto* a : dom.ancestors(*node))
            if (a->id == *turn) return true;
        return false;
    }
};

std::vector<std::string> shape(const core::Dom& dom) {
    std::vector<std::string> out;
    for (const auto& n : dom.iter()) {
        std::string attrs;
        for (const auto& [k, v] : n.attrs) attrs += k + ";";
        out.push_back(std::format("{} {} {}", core::as_str(n.kind), n.text, attrs));
    }
    return out;
}

} // namespace

TEST_CASE("a seeded projection matches the replay and hides other turns") {
    core::Dom source;
    std::vector<core::Patch> patches;
    auto add = [&](Draft draft) {
        auto patch = source.commit(std::move(draft), 1);
        REQUIRE(patch.has_value());
        patches.push_back(*patch);
        return *patch;
    };
    auto root = add(Draft::insert(NodeId::DOCUMENT, ElementKind::Session,
                                  {{std::string(attr::ID), Value("s")},
                                   {std::string(attr::HARNESS), Value("omp")},
                                   {std::string(attr::SCHEMA), Value(1)},
                                   {std::string(attr::NODE), Value("owner")},
                                   {std::string(attr::CREATED), Value(1)},
                                   {std::string(attr::PROJECT), Value("private")}}));
    add(Draft::set(root.node, std::string(attr::TITLE), Value("Plan")));
    auto hidden = add(Draft::insert(root.node, ElementKind::Turn));
    auto shown = add(Draft::insert(root.node, ElementKind::Turn));
    auto message = add(Draft::insert(shown.node, ElementKind::Message, {{std::string(attr::ROLE), Value("user")}}, "hel"));
    add(Draft::append(message.node, "lo"));
    add(Draft::insert(hidden.node, ElementKind::Message, {{std::string(attr::ROLE), Value("user")}}, "secret"));
    const bus::NodeKey peer{{1}};
    for (const std::optional<NodeId> turn : {std::optional(shown.node), std::optional<NodeId>()}) {
        const View access(turn);
        CAPTURE(access.turn.has_value());
        Projection replayed(peer);
        std::vector<Update> updates;
        for (const auto& p : patches) {
            auto u = replayed.accept(p, access);
            REQUIRE(u.has_value());
            updates.push_back(*u);
        }
        CHECK(updates.back().source_seq == source.seq());
        auto seeded = Projection::seeded(peer, source, access);
        REQUIRE(seeded.has_value());
        CHECK(shape(seeded->first.visible_dom()) == shape(replayed.visible_dom()));
        core::Dom mirror;
        for (const auto& p : seeded->second) REQUIRE(mirror.apply(p).has_value());
        CHECK(shape(mirror) == shape(seeded->first.visible_dom()));
        const auto* r = mirror.root();
        REQUIRE(r);
        CHECK_FALSE(r->attrs.contains(attr::PROJECT));
        CHECK(r->attrs.contains(attr::TITLE) == !access.turn.has_value());
        bool secret = false;
        for (const auto& n : mirror.iter()) secret |= n.text == "secret";
        CHECK(secret == !access.turn.has_value());
    }
}

TEST_CASE("visible drafts map to source ids, hidden nodes refuse") {
    core::Dom source;
    auto commit = [&](Draft d) {
        auto p = source.commit(std::move(d), 1);
        REQUIRE(p.has_value());
        return *p;
    };
    auto root = commit(Draft::insert(NodeId::DOCUMENT, ElementKind::Session,
                                     {{std::string(attr::ID), Value("s")},
                                      {std::string(attr::CREATED), Value(1)},
                                      {std::string(attr::HARNESS), Value("omp")},
                                      {std::string(attr::NODE), Value("owner")}}));
    auto hidden = commit(Draft::insert(root.node, ElementKind::Turn));
    auto shown = commit(Draft::insert(root.node, ElementKind::Turn));
    auto first = commit(Draft::insert(shown.node, ElementKind::Message, {{std::string(attr::ROLE), Value("user")}}, "a"));
    auto seeded = Projection::seeded(bus::NodeKey{{2}}, source, View(shown.node));
    REQUIRE(seeded.has_value());
    const auto& projection = seeded->first;
    const auto* visible_root = projection.visible_dom().root();
    REQUIRE(visible_root);
    REQUIRE(visible_root->children.size() == 1);
    const NodeId visible_turn = visible_root->children[0];
    CHECK(projection.source_node(visible_turn) == shown.node);
    const NodeId visible_message = projection.visible_dom().get(visible_turn)->children[0];
    CHECK(projection.source_node(visible_message) == first.node);

    auto appended = projection.source_draft(Draft::append(visible_message, "!"));
    REQUIRE(appended);
    CHECK(*appended == Draft::append(first.node, "!"));
    // After the one visible turn: after `shown`, at source index 2.
    auto inserted = projection.source_draft(Draft::insert_at(visible_root->id, 1, ElementKind::Turn));
    REQUIRE(inserted);
    CHECK(std::get<Draft::Insert>(inserted->v).index == 2u);
    CHECK_FALSE(projection.source_draft(Draft::append(NodeId{99}, "x")));
    (void)hidden;
}

TEST_CASE("mirrors refuse gaps and owners") {
    auto key = bus::NodeKey{{3}};
    auto owned = std::make_shared<bus::SessionWriter>(bus::WriterConfig{"s", key, key, {}, std::make_unique<Discard>(), nullptr});
    auto refused = Mirror::make(owned, 0);
    REQUIRE_FALSE(refused);
    CHECK(refused.error().to_string() == "this node does not own the session");

    auto writer = std::make_shared<bus::SessionWriter>(
        bus::WriterConfig{"s", bus::NodeKey{{4}}, key, {}, std::make_unique<Discard>(), nullptr});
    auto mirror = Mirror::seeded(writer, 5, {});
    REQUIRE(mirror);
    auto gap = mirror->receive(Update{7, std::nullopt});
    REQUIRE_FALSE(gap);
    CHECK(gap.error().to_string() == "patch 7 is not next after 5");
    CHECK(mirror->receive(Update{6, std::nullopt}));
    CHECK(mirror->source_seq() == 6);
}

TEST_CASE("local sessions read turns with 60 character previews") {
    core::Dom dom;
    auto commit = [&](Draft d) {
        auto p = dom.commit(std::move(d), 1);
        REQUIRE(p.has_value());
        return *p;
    };
    CHECK_FALSE(LocalSession::from_dom(dom));
    auto root = commit(Draft::insert(NodeId::DOCUMENT, ElementKind::Session,
                                     {{std::string(attr::ID), Value("s1")},
                                      {std::string(attr::CREATED), Value(7)},
                                      {std::string(attr::HARNESS), Value("omp")},
                                      {std::string(attr::NODE), Value("owner")}}));
    auto turn = commit(Draft::insert(root.node, ElementKind::Turn));
    const std::string long_text = "  " + std::string(59, 'a') + "\xc3\xa9" + "tail  ";
    commit(Draft::insert(turn.node, ElementKind::Message, {{std::string(attr::ROLE), Value("user")}}, long_text));
    auto short_turn = commit(Draft::insert(root.node, ElementKind::Turn));
    commit(Draft::insert(short_turn.node, ElementKind::Message, {{std::string(attr::ROLE), Value("user")}}, " hi "));
    auto session = LocalSession::from_dom(dom);
    REQUIRE(session);
    CHECK(session->facts.id == "s1");
    CHECK(session->harness == "omp");
    CHECK(session->last_seq == dom.seq());
    REQUIRE(session->turns.size() == 2);
    CHECK(session->turns[0].preview == std::string(59, 'a') + "\xc3\xa9\u2026");
    CHECK(session->turns[1].preview == "hi");
    CHECK(session->label() == session->turns[0].preview);
    session->title = "Title";
    CHECK(session->label() == "Title");
}

TEST_CASE("bus sessions list only written sessions, newest first") {
    auto bus = std::make_shared<bus::Bus>();
    const bus::NodeKey me{{5}}, other{{6}};
    auto open = [&](std::string id, bus::NodeKey owner, std::int64_t created) {
        auto writer = bus->open(bus::WriterConfig{bus::SessionId(id), owner, me, {}, std::make_unique<Discard>(), nullptr});
        REQUIRE(writer);
        if (owner == me)
            REQUIRE((*writer)->commit_at(Draft::insert(NodeId::DOCUMENT, ElementKind::Session,
                                                       {{std::string(attr::ID), Value(id)},
                                                        {std::string(attr::CREATED), Value(created)},
                                                        {std::string(attr::HARNESS), Value("omp")},
                                                        {std::string(attr::NODE), Value("me")}}),
                                         created));
    };
    open("old", me, 1);
    open("new", me, 2);
    open("mirror", other, 3);
    BusSessions sessions(bus);
    auto listed = sessions.sessions();
    REQUIRE(listed.size() == 2);
    CHECK(listed[0].facts.id == "new");
    CHECK(listed[1].facts.id == "old");
    CHECK(sessions.writer("old"));
    CHECK_FALSE(sessions.writer("mirror"));
    CHECK_FALSE(sessions.writer("missing"));
}

TEST_CASE("drafts use the serde JSON form") {
    auto insert = Draft::insert_at(NodeId{3}, 1, ElementKind::Message,
                                   {{"role", Value("user")}, {"n", Value(2)}, {"l", Value(std::vector<std::string>{"a"})}},
                                   "hi");
    CHECK(runtime::json::dump(draft_to_json(insert)) ==
          R"({"Insert":{"parent":3,"index":1,"kind":"Message","attrs":{"l":{"List":[{"Str":"a"}]},"n":{"Int":2},"role":{"Str":"user"}},"text":"hi"}})");
    CHECK(runtime::json::dump(draft_to_json(Draft::clear(NodeId{4}, "title"))) ==
          R"({"Set":{"node":4,"name":"title","value":null}})");
    CHECK(runtime::json::dump(draft_to_json(Draft::remove(NodeId{5}))) == R"({"Remove":{"node":5}})");
    for (const auto& draft : {insert, Draft::append(NodeId{2}, "x"), Draft::set(NodeId{1}, "ok", Value(true)),
                              Draft::set(NodeId{1}, "b", Value(Value::Bytes{1, 255})), Draft::remove(NodeId{9})}) {
        auto back = draft_from_json(draft_to_json(draft));
        REQUIRE(back);
        CHECK(*back == draft);
    }
    auto parsed = runtime::json::parse(R"({"Explode":{"node":1}})");
    REQUIRE(parsed);
    CHECK_FALSE(draft_from_json(*parsed));
}
