// The session writer, its subscriptions and the bus registry (port of tests/bus.rs).

#include <doctest/doctest.h>

#include <asio.hpp>

#include <atomic>
#include <format>
#include <string>
#include <thread>
#include <vector>

#include "loa/bus/bus.hpp"
#include "loa/bus/testkit.hpp"

using namespace loa::bus;
using namespace loa::core;

namespace {

NodeKey key(std::uint8_t b) {
    NodeKey k;
    k.bytes.fill(b);
    return k;
}
const NodeKey OWNER = key(1);
const NodeKey PEER = key(2);

// A live session of three patches (session, turn, empty assistant message).
struct Fixture {
    std::unique_ptr<SessionWriter> writer;
    // Every patch persisted after the first three.
    loa::core::testkit::MemoryPersist log;
    NodeId session, turn, message;

    [[nodiscard]] std::vector<std::uint64_t> seqs() const {
        std::scoped_lock lock(log.patches->mutex);
        std::vector<std::uint64_t> out;
        for (const auto& p : log.patches->patches) out.push_back(p.seq);
        return out;
    }
    [[nodiscard]] std::vector<Patch> patches() const {
        std::scoped_lock lock(log.patches->mutex);
        return log.patches->patches;
    }
    [[nodiscard]] std::string text() const {
        return writer->with_dom([&](const Dom& dom) { return dom.get(message)->text; });
    }
};

Fixture fixture_with(NodeKey local, loa::core::testkit::MemoryPersist log, std::shared_ptr<const Authorize> authorize) {
    auto built = loa::core::testkit::DomFixture::new_session();
    NodeId message = built.turn().message(Role::Assistant, "").last;
    Dom dom = built.build();
    NodeId turn = dom.get(message)->parent;
    WriterConfig config{"s1", OWNER, local, std::move(dom), std::make_unique<loa::bus::testkit::MemoryPersist>(log),
                        std::move(authorize)};
    return Fixture{std::make_unique<SessionWriter>(std::move(config)), std::move(log), built.session, turn, message};
}

Fixture fixture() {
    return fixture_with(OWNER, {}, std::make_shared<loa::bus::testkit::AllowAll>());
}

// Lets PEER change nodes under `scope` only.
struct Scoped final : Authorize {
    mutable std::mutex mutex;
    std::optional<NodeId> scope;

    std::expected<void, Denied> authorize(const SessionId&, const NodeKey& who, const Draft& draft,
                                          const Dom& dom) const override {
        NodeId target = std::visit(
            [](const auto& d) {
                if constexpr (std::is_same_v<std::decay_t<decltype(d)>, Draft::Insert>) return d.parent;
                else return d.node;
            },
            draft.v);
        std::optional<NodeId> s;
        {
            std::scoped_lock lock(mutex);
            s = scope;
        }
        if (!s || who != PEER) return std::unexpected(Denied{"no grant"});
        bool inside = target == *s;
        for (const Node* n : dom.ancestors(target)) inside = inside || n->id == *s;
        if (inside) return {};
        return std::unexpected(Denied{"outside the granted turn"});
    }
    void set(std::optional<NodeId> to) {
        std::scoped_lock lock(mutex);
        scope = to;
    }
};

std::pair<Fixture, std::shared_ptr<Scoped>> scoped() {
    auto auth = std::make_shared<Scoped>();
    auto f = fixture_with(OWNER, {}, auth);
    auth->set(f.turn);
    return {std::move(f), auth};
}

std::vector<Event> drain(Subscription& sub) {
    std::vector<Event> out;
    while (auto e = sub.try_recv()) out.push_back(std::move(*e));
    return out;
}

Patch must(std::expected<Patch, CommitError> r) {
    REQUIRE_MESSAGE(r.has_value(), (r ? std::string() : to_string(r.error())));
    return std::move(*r);
}

const std::string& append_text(const Patch& p) { return std::get<op::Append>(p.op).text; }

} // namespace

TEST_CASE("local commits and peer requests persist in one gap free order") {
    auto [f, auth] = scoped();
    auto [dom, sub] = f.writer->subscribe(Policy::every_patch(100));
    {
        std::jthread local([&] {
            for (int i = 0; i < 20; ++i) (void)must(f.writer->commit(Draft::append(f.message, std::format("L{};", i))));
        });
        std::jthread peer([&] {
            for (int i = 0; i < 20; ++i)
                REQUIRE(f.writer->request(PEER, std::nullopt, Draft::append(f.message, std::format("P{};", i)), 0));
        });
    }
    std::vector<std::uint64_t> want;
    for (std::uint64_t s = 4; s <= 43; ++s) want.push_back(s);
    CHECK(f.seqs() == want);
    auto text = f.text();
    for (char who : {'L', 'P'}) {
        std::string mine, expect;
        std::size_t start = 0;
        for (std::size_t end; (end = text.find(';', start)) != std::string::npos; start = end + 1)
            if (text[start] == who) mine += text.substr(start, end - start + 1);
        for (int i = 0; i < 20; ++i) expect += std::format("{}{};", who, i);
        CHECK(mine == expect);
    }
    std::string published;
    for (auto& e : drain(sub)) published += append_text(*std::get<event::Patch>(e).patch);
    CHECK_MESSAGE(published == text, "subscribers saw a different order than the DOM");
}

TEST_CASE("failed persist and invalid drafts change and publish nothing") {
    loa::core::testkit::MemoryPersist failing;
    failing.fail = "disk full";
    auto f = fixture_with(OWNER, failing, std::make_shared<loa::bus::testkit::AllowAll>());
    auto [before, sub] = f.writer->subscribe(Policy::every_patch(16));
    auto err = f.writer->commit(Draft::append(f.message, "lost"));
    CHECK(err.error() == CommitError(commit_error::Journal{"disk full"}));
    CHECK(f.writer->with_dom([](const Dom& d) { return d; }) == before);
    CHECK(sub.try_recv().error() == TryRecvError::Empty);

    auto g = fixture();
    auto missing = g.writer->commit(Draft::append(NodeId{99}, "x"));
    CHECK(missing.error() == CommitError(commit_error::Apply{apply_error::MissingNode{NodeId{99}}}));
    auto bad = g.writer->commit(Draft::set(g.turn, "status", "exploded"));
    REQUIRE(!bad);
    REQUIRE(std::holds_alternative<commit_error::Apply>(bad.error()));
    CHECK(std::holds_alternative<apply_error::Schema>(std::get<commit_error::Apply>(bad.error()).error));
    CHECK(g.seqs().empty());
    CHECK(must(g.writer->commit(Draft::set(g.turn, "status", "done"))).seq == 4);
}

TEST_CASE("mirrors replicate the owner's patches in order only") {
    auto owner = fixture();
    for (const char* text : {"a", "b", "c"}) (void)must(owner.writer->commit(Draft::append(owner.message, text)));
    auto patches = owner.patches();
    auto mirror = fixture_with(PEER, {}, std::make_shared<loa::bus::testkit::AllowAll>());
    auto [dom, sub] = mirror.writer->subscribe(Policy::every_patch(16));
    CHECK_FALSE(mirror.writer->is_writer());
    CHECK(mirror.writer->commit(Draft::append(mirror.message, "x")).error() == CommitError(commit_error::NotWriter{}));
    auto req = mirror.writer->request(PEER, std::nullopt, Draft::append(mirror.message, "x"), 0);
    CHECK(req.error() == RequestError(CommitError(commit_error::NotWriter{})));

    auto gap = mirror.writer->replicate(patches[1]);
    CHECK(gap.error() == ReplicateError(CommitError(commit_error::Apply{apply_error::Sequence{4, 5}})));
    for (const auto& p : patches) REQUIRE(mirror.writer->replicate(p));
    auto copy = [](const Dom& d) { return d; };
    CHECK(mirror.writer->with_dom(copy) == owner.writer->with_dom(copy));
    CHECK(mirror.seqs() == std::vector<std::uint64_t>{4, 5, 6});
    CHECK(sub.pending() == 3);
    CHECK(owner.writer->replicate(patches[0]).error() == ReplicateError(replicate_error::Owner{}));
}

TEST_CASE("peer requests are scoped by grant and revision") {
    auto [f, auth] = scoped();
    auto& w = *f.writer;
    auto patch = w.request(PEER, std::nullopt, Draft::append(f.message, "peer"), 7);
    REQUIRE(patch);
    CHECK(patch->seq == 4);
    CHECK(patch->ts == 7);
    auto outside = w.request(PEER, std::nullopt, Draft::set(f.session, "title", "renamed"), 8);
    CHECK(outside.error() == RequestError(Denied{"outside the granted turn"}));
    auto by_owner = w.request(OWNER, std::nullopt, Draft::append(f.message, "x"), 9);
    CHECK(std::holds_alternative<Denied>(by_owner.error()));

    // A request made against a revision a local commit has since moved past.
    (void)must(w.commit(Draft::append(f.message, " local")));
    auto stale = w.request(PEER, 4, Draft::append(f.message, " stale"), 9);
    CHECK(stale.error() == RequestError(request_error::Conflict{4, 5}));
    REQUIRE(w.request(PEER, 5, Draft::append(f.message, " fresh"), 9));
    CHECK(f.text() == "peer local fresh");

    // The grant is checked against the tree at the moment of ordering.
    auth->set(std::nullopt);
    auto revoked = w.request(PEER, 6, Draft::append(f.message, "x"), 9);
    CHECK(std::holds_alternative<Denied>(revoked.error()));
    CHECK(f.seqs() == std::vector<std::uint64_t>{4, 5, 6});
    auto denied = fixture_with(OWNER, {}, std::make_shared<loa::bus::testkit::DenyAll>());
    CHECK_FALSE(denied.writer->request(PEER, std::nullopt, Draft::append(denied.message, "x"), 0));
}

TEST_CASE("closing drains subscriptions and frees the id") {
    Bus bus;
    auto f = fixture();
    auto config = [&] {
        return WriterConfig{"a",
                            OWNER,
                            OWNER,
                            f.writer->with_dom([](const Dom& d) { return d; }),
                            std::make_unique<loa::bus::testkit::MemoryPersist>(),
                            std::make_shared<loa::bus::testkit::AllowAll>()};
    };
    auto writer = bus.open(config());
    REQUIRE(writer);
    CHECK(bus.open(config()).error() == BusError(bus_error::AlreadyOpen{"a"}));
    auto [d1, sub] = (*writer)->subscribe(Policy::every_patch(16));
    auto [d2, lagging] = (*writer)->subscribe(Policy::every_patch(1));
    for (const char* text : {"a", "b"}) (void)must(bus.get("a")->commit(Draft::append(f.message, text)));
    CHECK(bus.close("a") != nullptr);
    CHECK(bus.get("a") == nullptr);
    CHECK(bus.ids().empty());
    CHECK((*writer)->commit(Draft::append(f.message, "late")).error() == CommitError(commit_error::Closed{}));

    asio::io_context io;
    std::vector<std::uint64_t> seqs;
    asio::co_spawn(
        io,
        [&]() -> asio::awaitable<void> {
            while (auto e = co_await sub.recv()) {
                auto* p = std::get_if<event::Patch>(&*e);
                if (!p) break;
                seqs.push_back(p->patch->seq);
            }
        },
        asio::detached);
    io.run();
    CHECK(seqs == std::vector<std::uint64_t>{4, 5});
    auto lagged = drain(lagging);
    REQUIRE(lagged.size() == 2);
    CHECK(std::holds_alternative<event::Patch>(lagged[0]));
    CHECK(lagged[1] == Event(event::Lagged{5}));
    auto [d3, late] = (*writer)->subscribe(Policy::every_patch(16));
    CHECK(late.try_recv().error() == TryRecvError::Closed);
    CHECK(bus.open(config()));
}

TEST_CASE("recv wakes on a commit from another thread") {
    auto f = fixture();
    auto [dom, sub] = f.writer->subscribe(Policy::every_patch(16));
    asio::io_context io;
    std::optional<std::uint64_t> got;
    asio::co_spawn(
        io,
        [&]() -> asio::awaitable<void> {
            auto e = co_await sub.recv();
            if (e) got = std::get<event::Patch>(*e).patch->seq;
        },
        asio::detached);
    auto guard = asio::make_work_guard(io);
    std::jthread committer([&] {
        std::this_thread::sleep_for(std::chrono::milliseconds(20));
        (void)f.writer->commit(Draft::append(f.message, "x"));
        guard.reset();
    });
    io.run();
    CHECK(got == 4);
}

TEST_CASE("coalescing merges adjacent appends to one node only") {
    auto f = fixture();
    auto& w = *f.writer;
    Attrs user{{std::string(attr::ROLE), Role::User}};
    NodeId other = must(w.commit(Draft::insert(f.turn, ElementKind::Message, std::move(user)))).node;
    auto [mirror, ui] = w.subscribe(Policy::coalescing(8));
    auto [pd, peer] = w.subscribe(Policy::every_patch(1000));
    for (int i = 0; i < 500; ++i) (void)must(w.commit(Draft::append(f.message, std::format("{} ", i))));
    (void)must(w.commit(Draft::append(other, "c")));
    (void)must(w.commit(Draft::append(f.message, "d")));
    (void)must(w.commit(Draft::set(f.message, "status", "done")));
    for (const char* t : {"e", "f"}) (void)must(w.commit(Draft::append(f.message, t)));

    auto events = drain(ui);
    std::vector<std::size_t> lens;
    for (const auto& e : events) {
        if (const auto* a = std::get_if<Appends>(&e)) lens.push_back(a->text.size());
        else if (const auto* p = std::get_if<event::Patch>(&e)) {
            const auto* ap = std::get_if<op::Append>(&p->patch->op);
            lens.push_back(ap ? ap->text.size() : 0);
        } else FAIL("lagged");
    }
    CHECK(lens == std::vector<std::size_t>{1890, 1, 1, 0, 2});
    const auto* first = std::get_if<Appends>(&events[0]);
    REQUIRE(first);
    CHECK(first->first_seq == 5);
    CHECK(first->last_seq == 504);
    for (auto& e : events) CHECK(apply_to(std::move(e), mirror).value());
    CHECK(mirror == w.with_dom([](const Dom& d) { return d; }));
    CHECK_MESSAGE(drain(peer).size() == 505, "peers get every patch on its own");
}

TEST_CASE("subscribing while another thread commits leaves no gap") {
    auto f = fixture();
    std::atomic<bool> stop = false;
    {
        std::jthread committer([&] {
            while (!stop.load(std::memory_order_relaxed)) (void)must(f.writer->commit(Draft::append(f.message, "t")));
        });
        auto first = [](Subscription& sub) {
            for (;;) {
                if (auto e = sub.try_recv())
                    if (auto* p = std::get_if<event::Patch>(&*e)) return p->patch->seq;
                std::this_thread::yield();
            }
        };
        for (int i = 0; i < 20; ++i) {
            auto [mirror, sub] = f.writer->subscribe(Policy::every_patch(1 << 20));
            auto [seq, by_seq] = f.writer->subscribe_seq(Policy::every_patch(1 << 20));
            CHECK(first(sub) == mirror.seq() + 1);
            CHECK(first(by_seq) == seq + 1);
        }
        stop = true;
    }
    // A subscriber that never reads did not hold up the committer.
    auto [d, stuck] = f.writer->subscribe(Policy::every_patch(1));
    for (int i = 0; i < 5; ++i) (void)must(f.writer->commit(Draft::append(f.message, "x")));
}

TEST_CASE("node keys") {
    std::array<std::uint8_t, 32> seed{};
    auto k = NodeKey::from_seed(seed);
    REQUIRE(k);
    // The ed25519 public key of the all-zero seed.
    CHECK(k->debug() == "NodeKey(3b6a27bc…)");
    CHECK(OWNER.debug() == "NodeKey(01010101…)");
    CHECK(OWNER != PEER);
}

TEST_CASE("Ed25519 matches RFC 8032 test vector 1") {
    constexpr std::array<std::uint8_t, 32> seed{
        0x9d, 0x61, 0xb1, 0x9d, 0xef, 0xfd, 0x5a, 0x60, 0xba, 0x84, 0x4a, 0xf4, 0x92, 0xec, 0x2c, 0xc4,
        0x44, 0x49, 0xc5, 0x69, 0x7b, 0x32, 0x69, 0x19, 0x70, 0x3b, 0xac, 0x03, 0x1c, 0xae, 0x7f, 0x60};
    constexpr NodeKey::Signature signature{
        0xe5, 0x56, 0x43, 0x00, 0xc3, 0x60, 0xac, 0x72, 0x90, 0x86, 0xe2, 0xcc, 0x80, 0x6e, 0x82, 0x8a,
        0x84, 0x87, 0x7f, 0x1e, 0xb8, 0xe5, 0xd9, 0x74, 0xd8, 0x73, 0xe0, 0x65, 0x22, 0x49, 0x01, 0x55,
        0x5f, 0xb8, 0x82, 0x15, 0x90, 0xa3, 0x3b, 0xac, 0xc6, 0x1e, 0x39, 0x70, 0x1c, 0xf9, 0xb4, 0x6b,
        0xd2, 0x5b, 0xf5, 0xf0, 0x59, 0x5b, 0xbe, 0x24, 0x65, 0x51, 0x41, 0x43, 0x8e, 0x7a, 0x10, 0x0b};
    const auto key = NodeKey::from_seed(seed);
    REQUIRE(key);
    CHECK(NodeKey::sign(seed, {}).value() == signature);
    CHECK(key->verify({}, signature).value());
    CHECK_FALSE(key->verify(std::array<std::uint8_t, 1>{0}, signature).value());
}
