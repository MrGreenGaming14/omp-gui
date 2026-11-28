#include <doctest/doctest.h>

#include <cstdio>
#include <fstream>
#include <functional>
#include <iterator>

#include "loa/core/rewind.hpp"
#include "loa/core/testkit.hpp"
#include "loa/core/writer.hpp"
#include "loa/core/xml.hpp"
#include "loa/runtime/proto.hpp"

using namespace loa::core;
using testkit::DomFixture;

namespace {

constexpr NodeId TURN{2};
constexpr NodeId MSG{3};

// Session 1 > turn 2 > user message 3 "hi".
Dom seeded() {
    auto f = DomFixture::new_session();
    f.message(Role::User, "hi");
    return f.build();
}

std::string a(std::string_view s) { return std::string(s); }

} // namespace

TEST_CASE("apply cases (tests.rs apply_cases)") {
    struct Case {
        const char* name;
        std::function<Patch(const Dom&)> patch;
        int expected; // ApplyError index, or -1 for success
        int schema = -1; // SchemaError index for apply_error::Schema
    };
    using E = ApplyError;
    auto idx = [](auto t) { return static_cast<int>(E(t).index()); };
    const int sequence = idx(apply_error::Sequence{}), missing = idx(apply_error::MissingNode{}),
              reused = idx(apply_error::IdReused{}), root = idx(apply_error::RootExists{}),
              index = idx(apply_error::Index{}), schema = idx(apply_error::Schema{});
    const std::vector<Case> cases = {
        {"append", [](const Dom& d) { return d.prepare(Draft::append(MSG, " there"), 0); }, -1},
        {"set text", [](const Dom& d) { return d.prepare(Draft::set_text(MSG, "new"), 0); }, -1},
        {"clear text", [](const Dom& d) { return d.prepare(Draft::clear(MSG, a(TEXT)), 0); }, -1},
        {"insert at 0",
         [](const Dom& d) {
             return d.prepare(Draft::insert_at(TURN, 0, ElementKind::Message, {{a(attr::ROLE), Role::System}}), 0);
         },
         -1},
        {"index past end",
         [](const Dom& d) {
             return d.prepare(Draft::insert_at(TURN, 9, ElementKind::Message, {{a(attr::ROLE), Role::User}}), 0);
         },
         index},
        {"x- attribute", [](const Dom& d) { return d.prepare(Draft::set(MSG, "x-uuid", "abc"), 0); }, -1},
        {"ext keeps any attr",
         [](const Dom& d) {
             return d.prepare(Draft::insert(MSG, ElementKind::Ext,
                                            {{a(attr::HARNESS), "c"}, {"any", Value::Bytes{1}}}, "{}"),
                              0);
         },
         -1},
        {"remove subtree", [](const Dom& d) { return d.prepare(Draft::remove(TURN), 0); }, -1},
        {"seq gap", [](const Dom& d) { return Patch{d.seq() + 2, TURN, 0, op::Remove{}}; }, sequence},
        {"missing node", [](const Dom& d) { return d.prepare(Draft::append(NodeId{99}, "x"), 0); }, missing},
        {"second root",
         [](const Dom& d) { return d.prepare(Draft::insert(NodeId::DOCUMENT, ElementKind::Session), 0); }, root},
        {"id reused",
         [](const Dom& d) {
             auto p = d.prepare(Draft::insert(TURN, ElementKind::Todo), 0);
             p.node = MSG;
             return p;
         },
         reused},
        {"bad parent", [](const Dom& d) { return d.prepare(Draft::insert(NodeId{1}, ElementKind::Input), 0); }, schema,
         0},
        {"missing required", [](const Dom& d) { return d.prepare(Draft::insert(TURN, ElementKind::Message), 0); },
         schema, 4},
        {"bad enum", [](const Dom& d) { return d.prepare(Draft::set(MSG, a(attr::ROLE), "robot"), 0); }, schema, 3},
        {"bad type", [](const Dom& d) { return d.prepare(Draft::set(TURN, a(attr::STARTED), "yesterday"), 0); },
         schema, 2},
        {"unknown attr", [](const Dom& d) { return d.prepare(Draft::set(TURN, "colour", "red"), 0); }, schema, 1},
        {"clear required", [](const Dom& d) { return d.prepare(Draft::clear(MSG, a(attr::ROLE)), 0); }, schema, 4},
        {"text takes a string", [](const Dom& d) { return d.prepare(Draft::set(MSG, a(TEXT), 1), 0); }, schema, 5},
    };
    for (const auto& c : cases) {
        CAPTURE(c.name);
        Dom dom = seeded();
        const Dom before = dom;
        const Patch patch = c.patch(dom);
        const auto checked = dom.check(patch);
        const auto applied = dom.apply(patch);
        CHECK(checked == applied);
        if (c.expected < 0) {
            REQUIRE(applied.has_value());
            CHECK(dom.seq() == before.seq() + 1);
        } else {
            REQUIRE_FALSE(applied.has_value());
            CHECK(static_cast<int>(applied.error().index()) == c.expected);
            if (c.schema >= 0)
                CHECK(static_cast<int>(std::get<apply_error::Schema>(applied.error()).error.index()) == c.schema);
            CHECK(dom == before);
        }
    }
    // A removed id is never handed out again, and remove takes the subtree.
    Dom dom = seeded();
    REQUIRE(dom.commit(Draft::remove(TURN), 0));
    CHECK(dom.size() == 1);
    CHECK_FALSE(dom.contains(MSG));
    CHECK(dom.prepare(Draft::insert(NodeId{1}, ElementKind::Turn), 0).node > MSG);
    CHECK(to_string(ApplyError(apply_error::Schema{schema_error::Parent{ElementKind::Input, ElementKind::Session}})) ==
          "<input> can't sit under <session>");
}

TEST_CASE("from_parts and MemorySink round trip") {
    const Dom dom = seeded();
    std::vector<Node> nodes;
    for (const Node& n : dom.iter()) nodes.push_back(n);
    CHECK(Dom::from_parts(nodes, dom.next_id(), dom.seq()).value() == dom);
    CHECK(std::holds_alternative<parts_error::NextId>(Dom::from_parts(nodes, MSG, 0).error()));
    auto detached = nodes;
    detached[1].children.clear();
    CHECK(Dom::from_parts(detached, dom.next_id(), 0).error() == PartsError(parts_error::Unreachable{MSG}));

    MemorySink sink;
    const DomWriter w = DomWriter(sink).at(7);
    auto s = w.insert(NodeId::DOCUMENT, ElementKind::Session,
                      {{a(attr::ID), "s"}, {a(attr::HARNESS), "h"}, {a(attr::NODE), "k"}, {a(attr::CREATED), 1}});
    const NodeId turn = w.insert(s.value(), ElementKind::Turn, {{a(attr::STATUS), TurnStatus::Running}}).value();
    REQUIRE(w.close(turn, TurnStatus::Done));
    CHECK_FALSE(w.append(NodeId{77}, "x"));
    Dom replayed;
    for (const Patch& p : sink.patches()) REQUIRE(replayed.apply(p));
    CHECK(replayed == sink.dom());
    CHECK(sink.dom().get(turn)->int_attr(attr::ENDED) == 7);
}

TEST_CASE("xml golden (tests.rs xml_golden)") {
    auto f = DomFixture::new_session();
    f.message(Role::User, "hi").call("bash", {{"x-n", 1.0}}).decision(DecisionKind::Question);
    const NodeId d = f.last;
    f.commit(Draft::set(d, a(attr::OPTIONS), std::vector<std::string>{"Yes", "It's \\ fine"}));
    Attrs raw{{a(attr::HARNESS), "c"},
              {"raw", Value::Bytes{0x00, 0xff, 'h', 'i'}},
              {"weird name", "a"},
              {"_xA", "b"},
              {"x-l", Value::List{Value(1), Value(Value::Bytes{1, 2})}}};
    f.add(f.session, ElementKind::Ext, std::move(raw), "a < b && \"d\"\r");
    const std::string expected = R"(<?xml version="1.0" encoding="UTF-8"?>
<!-- loa session dom: schema 1, seq 7, next nid 7 -->
<session nid="1" created.int="1" harness="omp" id="s1" node="k">
  <turn nid="2">
    <message nid="3" role="user">hi</message>
    <call nid="4" tool="bash" x-n.float="1.0">
      <decision nid="5" kind="question" options.list="['Yes', 'It\'s \\ fine']"/>
    </call>
  </turn>
  <ext nid="6" _x005F_xA="b" harness="c" raw.bytes="AP9oaQ==" weird_x0020_name="a" x-l.list="[1, bytes(AQI=)]">a &lt; b &amp;&amp; "d"&#x0D;</ext>
</session>
)";
    CHECK(xml::to_string(f.dom) == expected);

    // Rust `{:?}` float forms the golden doesn't reach.
    auto g = DomFixture::new_session();
    g.commit(Draft::set(g.session, "x-f", Value::List{1e16, 1.5e-5, 0.25, -0.0}));
    CHECK(xml::to_string(g.dom).find(R"(x-f.list="[1e16, 1.5e-5, 0.25, -0.0]")") != std::string::npos);
}

TEST_CASE("rewind cases (tests.rs rewind_cases)") {
    auto running = [](DomFixture& f) {
        f.turn();
        const NodeId turn = f.last;
        f.commit(Draft::set(turn, a(attr::STATUS), TurnStatus::Running));
        f.call("bash", {{a(attr::STATUS), CallStatus::Running}});
        return std::pair{turn, f.last};
    };
    {
        // Rewinding past running work cancels it (deepest first) and removes it.
        auto f = DomFixture::new_session();
        const Dom target = f.build();
        auto [turn, call] = running(f);
        f.commit(Draft::set(f.session, a(attr::MODEL), "m2"));
        const auto plan = rewind::plan(f.dom, target);
        CHECK(plan.cancel == std::vector{call, turn});
        CHECK(plan.resume.empty());
        CHECK(plan.apply ==
              rewind::Apply(rewind::Drafts{{Draft::remove(turn), Draft::clear(f.session, a(attr::MODEL))}}));
    }
    {
        // Rewinding into unfinished work resumes it; grown text replays as an append.
        auto f = DomFixture::new_session();
        auto [turn, call] = running(f);
        f.commit(Draft::set_text(call, "Hel"));
        const Dom target = f.build();
        f.commit(Draft::append(call, "lo")).commit(Draft::set(call, a(attr::STATUS), CallStatus::Done));
        f.commit(Draft::set(turn, a(attr::STATUS), TurnStatus::Done));
        const auto plan = rewind::plan(f.dom, target);
        CHECK(plan.cancel.empty());
        CHECK(plan.resume == std::vector{turn, call});
        const auto& drafts = std::get<rewind::Drafts>(plan.apply).drafts;
        for (const Draft& d : drafts) f.commit(d);
        CHECK(rewind::drafts(f.dom, target).value().empty());
    }
    {
        // Restoring removed nodes needs a fork at the target's position.
        auto f = DomFixture::new_session();
        auto [turn, call] = running(f);
        const Dom target = f.build();
        f.commit(Draft::remove(turn));
        CHECK(rewind::plan(f.dom, target).apply ==
              rewind::Apply(rewind::Fork{target.seq(), rewind::inexpressible::Added{turn}}));
    }
    {
        // An <ext> attribute named `#text` that changed can't be set back.
        auto f = DomFixture::new_session();
        const NodeId ext = f.add(f.session, ElementKind::Ext, {{a(attr::HARNESS), "c"}, {a(TEXT), "x"}});
        const Dom with = f.build();
        std::vector<Node> nodes;
        for (const Node& n : with.iter()) nodes.push_back(n);
        nodes[1].attrs.insert_or_assign(a(TEXT), Value("y"));
        const Dom other = Dom::from_parts(nodes, with.next_id(), with.seq()).value();
        CHECK(rewind::drafts(with, other).error() == rewind::Inexpressible(rewind::inexpressible::TextAttr{ext}));
    }
}

// --- Golden: bus/patch-NN.pb replayed must equal bus/dom.pb (Rust prost output).

namespace {
namespace pb = loa::runtime::proto;
using Span = std::span<const std::uint8_t>;

pb::Bytes slurp(const std::string& path) {
    std::ifstream in(path, std::ios::binary);
    return {std::istreambuf_iterator<char>(in), {}};
}
std::string str(Span s) { return {s.begin(), s.end()}; }

Value decode_value(Span data) {
    pb::Reader r(data);
    std::optional<Value> v;
    while (!r.done()) {
        auto t = r.tag().value();
        switch (t.field) {
        case 1: v = Value(r.varint().value() != 0); break;
        case 2: v = Value(pb::zigzag_decode(r.varint().value())); break;
        case 3: v = Value(std::bit_cast<double>(r.fixed64().value())); break;
        case 4: v = Value(str(r.len().value())); break;
        case 5: {
            auto b = r.len().value();
            v = Value(Value::Bytes(b.begin(), b.end()));
            break;
        }
        case 6: {
            pb::Reader l(r.len().value());
            Value::List items;
            while (!l.done()) {
                REQUIRE(l.tag().value().field == 1);
                items.push_back(decode_value(l.len().value()));
            }
            v = Value(std::move(items));
            break;
        }
        default: REQUIRE(r.skip(t));
        }
    }
    REQUIRE(v);
    return *v;
}

std::pair<std::string, std::optional<Value>> decode_attr(Span data) {
    pb::Reader r(data);
    std::pair<std::string, std::optional<Value>> out;
    while (!r.done()) {
        auto t = r.tag().value();
        if (t.field == 1) out.first = str(r.len().value());
        else if (t.field == 2) out.second = decode_value(r.len().value());
        else REQUIRE(r.skip(t));
    }
    return out;
}

ElementKind kind_of(Span s) {
    auto k = parse<ElementKind>(str(s));
    REQUIRE_MESSAGE(k, str(s));
    return *k;
}

Op decode_op(Span data) {
    pb::Reader r(data);
    auto t = r.tag().value();
    pb::Reader m(r.len().value());
    switch (t.field) {
    case 1: {
        op::Insert o{};
        while (!m.done()) {
            auto f = m.tag().value();
            switch (f.field) {
            case 1: o.parent = NodeId{m.varint().value()}; break;
            case 2: o.index = static_cast<std::uint32_t>(m.varint().value()); break;
            case 3: o.kind = kind_of(m.len().value()); break;
            case 4: {
                auto [k, v] = decode_attr(m.len().value());
                o.attrs.insert_or_assign(k, *v);
                break;
            }
            case 5: o.text = str(m.len().value()); break;
            default: REQUIRE(m.skip(f));
            }
        }
        return o;
    }
    case 2: {
        op::Set o;
        while (!m.done()) {
            auto f = m.tag().value();
            if (f.field == 1) o.name = str(m.len().value());
            else if (f.field == 2) o.value = decode_value(m.len().value());
            else REQUIRE(m.skip(f));
        }
        return o;
    }
    case 3: {
        op::Append o;
        while (!m.done()) {
            auto f = m.tag().value();
            if (f.field == 1) o.text = str(m.len().value());
            else REQUIRE(m.skip(f));
        }
        return o;
    }
    default: return op::Remove{};
    }
}

Patch decode_patch(Span data) {
    pb::Reader r(data);
    Patch p;
    while (!r.done()) {
        auto t = r.tag().value();
        switch (t.field) {
        case 1: p.seq = r.varint().value(); break;
        case 2: p.node = NodeId{r.varint().value()}; break;
        case 3: p.ts = static_cast<std::int64_t>(r.varint().value()); break;
        case 4: p.op = decode_op(r.len().value()); break;
        default: REQUIRE(r.skip(t));
        }
    }
    return p;
}

Dom decode_dom(Span data) {
    pb::Reader r(data);
    std::uint64_t seq = 0, next = 0;
    std::vector<Node> nodes;
    while (!r.done()) {
        auto t = r.tag().value();
        if (t.field == 1) seq = r.varint().value();
        else if (t.field == 2) next = r.varint().value();
        else if (t.field == 3) {
            pb::Reader m(r.len().value());
            Node n{};
            while (!m.done()) {
                auto f = m.tag().value();
                switch (f.field) {
                case 1: n.id = NodeId{m.varint().value()}; break;
                case 2: n.kind = kind_of(m.len().value()); break;
                case 3: n.parent = NodeId{m.varint().value()}; break;
                case 4:
                    if (f.type == pb::WireType::len) {
                        pb::Reader c(m.len().value());
                        while (!c.done()) n.children.push_back(NodeId{c.varint().value()});
                    } else {
                        n.children.push_back(NodeId{m.varint().value()});
                    }
                    break;
                case 5: {
                    auto [k, v] = decode_attr(m.len().value());
                    n.attrs.insert_or_assign(k, *v);
                    break;
                }
                case 6: n.text = str(m.len().value()); break;
                default: REQUIRE(m.skip(f));
                }
            }
            nodes.push_back(std::move(n));
        } else REQUIRE(r.skip(t));
    }
    return Dom::from_parts(std::move(nodes), NodeId{next}, seq).value();
}
} // namespace

TEST_CASE("golden: Rust patches replay to the Rust DOM; every Op kind and enum string decodes") {
    const std::string dir = LOA_GOLDEN_DIR "/bus/";
    Dom replayed;
    std::array<bool, 4> ops{};
    bool insert_at = false, clear = false;
    for (int i = 1; i <= 8; ++i) {
        char name[32];
        std::snprintf(name, sizeof name, "patch-%02d.pb", i);
        const auto bytes = slurp(dir + name);
        REQUIRE_MESSAGE(!bytes.empty(), name);
        const Patch p = decode_patch(bytes);
        ops[p.op.index()] = true;
        if (auto* ins = std::get_if<op::Insert>(&p.op)) insert_at |= ins->index.has_value();
        if (auto* set = std::get_if<op::Set>(&p.op)) clear |= !set->value;
        const auto ok = replayed.apply(p);
        REQUIRE_MESSAGE(ok, name, ": ", ok ? "" : to_string(ok.error()));
    }
    CHECK(ops == std::array{true, true, true, true});
    CHECK(insert_at);
    CHECK(clear);
    const Dom golden = decode_dom(slurp(dir + "dom.pb"));
    CHECK(replayed == golden);
    // Every attribute that names a schema enum holds a value the C++ schema accepts.
    for (const Node& n : golden.iter()) {
        std::optional<ElementKind> parent;
        if (const Node* p = golden.get(n.parent)) parent = p->kind;
        CHECK(validate_insert(parent, n.kind, n.attrs).has_value());
    }
}
