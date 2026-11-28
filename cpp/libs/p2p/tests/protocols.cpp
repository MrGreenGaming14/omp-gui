// Two nodes in one process over the loopback TCP transport (TLS 1.3 + yamux):
// grants over omp/control/1, shared sessions, added protocols and folders
// (port of the protocol scenarios of `crates/loa-p2p/tests/two_nodes.rs`).

#include <doctest/doctest.h>

#include <asio.hpp>

#include <chrono>
#include <filesystem>
#include <fstream>
#include <map>
#include <mutex>
#include <thread>

#include "loa/bus/bus.hpp"
#include "loa/bus/store.hpp"
#include "loa/p2p/auth.hpp"
#include "loa/p2p/protocols/control.hpp"
#include "loa/p2p/protocols/extra.hpp"
#include "loa/p2p/protocols/folder.hpp"
#include "loa/p2p/protocols/session.hpp"
#include "loa/p2p/sessions.hpp"
#include "loa/p2p/tcp.hpp"
#include "loa/runtime/fs.hpp"
#include "loa/runtime/tempfile.hpp"
#include "loa/runtime/time.hpp"

using namespace loa;
using namespace loa::p2p;
using namespace std::chrono_literals;
using core::Draft;
using core::ElementKind;
using core::Value;
using trust::Action;
namespace attr = core::attr;
namespace stdfs = std::filesystem;

namespace {

struct Discard final : bus::Persist {
    std::expected<void, bus::PersistError> append(const core::Patch&) override { return {}; }
};

// A node: grant store, bus sessions and the protocols on a loopback TCP transport.
class TestNode final : public NodeHost, public std::enable_shared_from_this<TestNode> {
  public:
    static std::shared_ptr<TestNode> make(std::string name, std::vector<AddedProtocol> added = {}) {
        auto identity = trust::NodeIdentity::generate();
        REQUIRE(identity);
        auto store = trust::GrantStore::open_in_memory();
        REQUIRE(store);
        auto node = std::shared_ptr<TestNode>(new TestNode(std::move(name), *identity,
                                                           std::make_shared<trust::GrantStore>(std::move(*store))));
        std::weak_ptr<NodeHost> weak = node;
        Router router;
        router.handler(wire::CONTROL, served(weak, control::serve));
        router.handler(wire::SESSION, served(weak, session::serve));
        router.handler(wire::FOLDER, served(weak, folder::serve));
        for (auto& protocol : added)
            router.handler(protocol.label(), served(weak, [protocol](NodeHost& host, const Connection& conn, Stream s) {
                               return extra::serve(host, conn, std::move(s), protocol);
                           }));
        auto tcp = Tcp::bind(identity->secret_bytes(), "127.0.0.1:0", std::move(router));
        REQUIRE(tcp);
        node->tcp_ = *tcp;
        return node;
    }

    ~TestNode() override { tcp_->close(); }

    NodeId key() const override { return grants_.key(); }
    GrantService& grants() const override { return grants_; }
    LocalSessions& local_sessions() const override { return *sessions_; }
    wire::Hello hello() const override {
        wire::Hello hello{"0.4.2", {}, name_};
        for (auto label : wire::LOCAL) hello.protocols.push_back({std::string(label.substr(0, label.rfind('/'))), 1});
        return hello;
    }
    void observe(const NodeId& peer, const wire::Hello& hello, TransportKind) override {
        std::scoped_lock lock(mutex_);
        seen_[peer] = hello;
    }
    void changed() override {
        std::scoped_lock lock(mutex_);
        ++changes_;
    }
    void prompt_requested(PromptRequested request) override {
        std::scoped_lock lock(mutex_);
        prompts_.push_back(std::move(request));
    }
    void answer_requested(AnswerRequested request) override {
        std::scoped_lock lock(mutex_);
        answers_.push_back(std::move(request));
    }
    Result<Connection> dial(const NodeId& peer, std::string_view alpn) override {
        std::vector<SocketAddr> hints;
        {
            std::scoped_lock lock(mutex_);
            if (auto it = addresses_.find(peer); it != addresses_.end()) hints.push_back(it->second);
        }
        return tcp_->dial(peer, hints, alpn);
    }
    Result<Connection> dial_any(const SocketAddr& addr, std::string_view alpn) override { return tcp_->dial_any(addr, alpn); }

    SocketAddr address() const { return tcp_->local_addrs().at(0); }
    void meet(const TestNode& other) {
        std::scoped_lock lock(mutex_);
        addresses_[other.key()] = other.address();
    }
    std::optional<wire::Hello> seen(const NodeId& peer) {
        std::scoped_lock lock(mutex_);
        auto it = seen_.find(peer);
        return it == seen_.end() ? std::nullopt : std::optional(it->second);
    }
    std::vector<PromptRequested> prompts() {
        std::scoped_lock lock(mutex_);
        return prompts_;
    }

    // Issues a grant to `guest` and delivers it over control.
    trust::GrantId grant(TestNode& guest, trust::Resource resource, Action action) {
        auto scope = trust::Scope::make(std::move(resource), {action});
        REQUIRE(scope);
        auto issued = grants_.store->issue(grants_.identity, guest.key(),
                                           trust::GrantSpec{{*scope}, trust::DEFAULT_TTL_MS, false, std::nullopt},
                                           runtime::time::now_ms());
        REQUIRE(issued);
        auto delivered = control::call_peer(*this, guest.key(), wire::GrantDelivery{issued->token, {}});
        REQUIRE_MESSAGE(delivered, delivered.error().to_string());
        return issued->id;
    }

    void revoke(TestNode& guest, const trust::GrantId& id) {
        REQUIRE(grants_.store->revoke(key(), id, runtime::time::now_ms()));
        const auto& text = id.as_str();
        auto told = control::call_peer(*this, guest.key(), wire::GrantRevocation{Bytes(text.begin(), text.end())});
        REQUIRE_MESSAGE(told, told.error().to_string());
    }

    std::vector<trust::GrantId> given() const {
        std::vector<trust::GrantId> out;
        for (auto& g : grants_.infos())
            if (g.direction == GrantDirection::given && !g.ended) out.push_back(g.id);
        return out;
    }

    // A live session on this node's bus with a prompt and an answer; the writer and the answer.
    std::pair<std::shared_ptr<bus::SessionWriter>, core::NodeId> session(const std::string& id) {
        const auto me = trust::to_bus(key());
        auto writer = bus_->open(bus::WriterConfig{bus::SessionId(id), me, me, {}, std::make_unique<Discard>(), grants_.access});
        REQUIRE(writer);
        const auto ts = runtime::time::now_ms();
        auto root = (*writer)->commit_at(Draft::insert(core::NodeId::DOCUMENT, ElementKind::Session,
                                                       {{std::string(attr::ID), Value(id)},
                                                        {std::string(attr::HARNESS), Value("omp")},
                                                        {std::string(attr::NODE), Value(key().to_string())},
                                                        {std::string(attr::CREATED), Value(ts)},
                                                        {std::string(attr::SCHEMA), Value(1)},
                                                        {std::string(attr::TITLE), Value("Fix the parser")},
                                                        {std::string(attr::PROJECT), Value("/home/owner/secret-project")}}),
                                         ts);
        REQUIRE(root);
        auto turn = (*writer)->commit_at(Draft::insert(root->node, ElementKind::Turn), ts);
        REQUIRE(turn);
        REQUIRE((*writer)->commit_at(
            Draft::insert(turn->node, ElementKind::Message, {{std::string(attr::ROLE), Value("user")}}, "Why does it fail?"), ts));
        auto answer = (*writer)->commit_at(
            Draft::insert(turn->node, ElementKind::Message, {{std::string(attr::ROLE), Value("assistant")}}, "Because"), ts);
        REQUIRE(answer);
        return {*writer, answer->node};
    }

  private:
    TestNode(std::string name, trust::NodeIdentity identity, std::shared_ptr<trust::GrantStore> store)
        : name_(std::move(name)), grants_(std::move(store), std::move(identity), 3 * 60'000),
          bus_(std::make_shared<bus::Bus>()), sessions_(std::make_shared<BusSessions>(bus_)) {}

    std::string name_;
    mutable GrantService grants_;
    std::shared_ptr<bus::Bus> bus_;
    std::shared_ptr<BusSessions> sessions_;
    std::shared_ptr<Tcp> tcp_;
    std::mutex mutex_;
    std::map<NodeId, SocketAddr> addresses_;
    std::map<NodeId, wire::Hello> seen_;
    std::vector<PromptRequested> prompts_;
    std::vector<AnswerRequested> answers_;
    int changes_ = 0;
};

std::pair<std::shared_ptr<TestNode>, std::shared_ptr<TestNode>> pair(std::string owner, std::string guest,
                                                                     std::vector<AddedProtocol> added = {}) {
    auto a = TestNode::make(std::move(owner), std::move(added));
    auto b = TestNode::make(std::move(guest));
    a->meet(*b);
    b->meet(*a);
    return {a, b};
}

template <class F>
void eventually(const char* what, F check) {
    for (int i = 0; i < 400; ++i) {
        if (check()) return;
        std::this_thread::sleep_for(25ms);
    }
    FAIL("timed out waiting for " << what);
}

std::optional<std::string> text_of(const core::Dom& dom, std::string_view role) {
    for (const auto& n : dom.iter())
        if (n.kind == ElementKind::Message && n.str_attr(attr::ROLE) == role) return n.text;
    return std::nullopt;
}

core::NodeId first_message(const core::Dom& dom) {
    for (const auto& n : dom.iter())
        if (n.kind == ElementKind::Message) return n.id;
    FAIL("no message");
    return {};
}

std::optional<std::string> view_text(const RemoteView& view, std::string_view role) {
    return view.writer()->with_dom([&](const core::Dom& dom) { return text_of(dom, role); });
}

} // namespace

TEST_CASE("hello validation") {
    CHECK(control::valid_hello(wire::Hello{"1", {{"omp/control", 1}}, ""}));
    CHECK_FALSE(control::valid_hello(wire::Hello{"", {}, ""}));
    CHECK_FALSE(control::valid_hello(wire::Hello{"1", {{"", 1}}, ""}));
    CHECK_FALSE(control::valid_hello(wire::Hello{"1", {{"omp/control", 0}}, ""}));
}

TEST_CASE("a grant proved against a fresh challenge counts; a replayed proof does not") {
    auto [owner, guest] = pair("owner", "guest");
    owner->grant(*guest, trust::resource::Session{"s"}, Action::view);
    const auto guest_key = trust::to_bus(guest->key());

    // Fresh: the guest answers this very challenge.
    {
        auto [a, b] = duplex(64 * 1024);
        std::thread requester([&, b = std::move(b)]() mutable { REQUIRE(guest->grants().present(owner->key(), b)); });
        REQUIRE(owner->grants().challenge(guest->key(), a));
        requester.join();
        CHECK(owner->grants().access->live_grants(guest_key, runtime::time::now_ms()).size() == 1);
    }

    // Replayed: a proof captured from an earlier challenge, presented to a new one.
    auto received = guest->grants().store->received_all();
    REQUIRE(received);
    REQUIRE(received->size() == 1);
    const auto token = received->front().token;
    auto old = trust::Challenge::random();
    REQUIRE(old);
    const auto stale = trust::prove(guest->grants().identity, *old, token);
    auto [a, b] = duplex(64 * 1024);
    std::thread replayer([&, b = std::move(b)]() mutable {
        auto challenge = wire::expect<wire::Frame>(*b.recv);
        REQUIRE(challenge);
        wire::Present present{{{token, Bytes(stale.bytes.begin(), stale.bytes.end())}}};
        REQUIRE(wire::send(*b.send, wire::Frame{wire::Body(present)}));
    });
    REQUIRE(owner->grants().challenge(guest->key(), a));
    replayer.join();
    CHECK(owner->grants().access->live_grants(guest_key, runtime::time::now_ms()).empty());
}

TEST_CASE("a shared session is viewed, driven, prompted, forked and revoked") {
    auto [owner, guest] = pair("owner-box", "guest-box");
    auto [writer, answer] = owner->session("s-shared");
    for (auto action : {Action::drive, Action::fork}) owner->grant(*guest, trust::resource::Session{"s-shared"}, action);

    auto hello = guest->seen(owner->key());
    REQUIRE(hello);
    CHECK(hello->app_version == "0.4.2");
    CHECK(hello->name == "owner-box");
    std::vector<GrantInfo> received;
    for (auto& g : guest->grants().infos())
        if (g.direction == GrantDirection::received) received.push_back(g);
    REQUIRE(received.size() == 2);
    for (auto& g : received) CHECK((g.peer == owner->key() && g.renews_at_ms.has_value()));

    auto listed = session::list(*guest, owner->key());
    REQUIRE_MESSAGE(listed, listed.error().to_string());
    REQUIRE(listed->size() == 1);
    CHECK((*listed)[0].title == "Fix the parser");
    CHECK(std::ranges::find((*listed)[0].actions, Action::drive) != (*listed)[0].actions.end());
    CHECK(std::ranges::find((*listed)[0].actions, Action::fork) != (*listed)[0].actions.end());

    auto opened = session::open(*guest, owner->key(), "s-shared");
    REQUIRE_MESSAGE(opened, opened.error().to_string());
    auto& view = **opened;
    CHECK((view.can(Action::drive) && view.can(Action::fork)));
    CHECK(view.source_seq() == writer->seq());
    view.writer()->with_dom([](const core::Dom& dom) {
        CHECK_FALSE(dom.root()->attrs.contains(attr::PROJECT));
        CHECK(text_of(dom, "assistant") == "Because");
    });

    REQUIRE(writer->commit_at(Draft::append(answer, " the lexer drops tokens"), runtime::time::now_ms()));
    eventually("the owner's patch on the view", [&] { return view_text(view, "assistant") == "Because the lexer drops tokens"; });

    // Drive goes through the owner, the single writer; a stale base is refused.
    auto visible = view.writer()->with_dom([](const core::Dom& dom) {
        for (const auto& n : dom.iter())
            if (n.str_attr(attr::ROLE) == "assistant") return n.id;
        return core::NodeId{};
    });
    const auto before = view.source_seq();
    auto driven = view.drive(Draft::append(visible, "."));
    REQUIRE_MESSAGE(driven, driven.error().to_string());
    CHECK(*driven == DriveOutcome(drive_outcome::Applied{before + 1}));
    CHECK(writer->with_dom([](const core::Dom& dom) { return text_of(dom, "assistant"); }) == "Because the lexer drops tokens.");
    eventually("the driven patch back on the view", [&] { return view.source_seq() == before + 1; });
    REQUIRE(writer->commit_at(Draft::append(answer, "!"), runtime::time::now_ms()));
    auto stale = view.drive_at(before, Draft::append(visible, "?"));
    REQUIRE(stale);
    CHECK(*stale == DriveOutcome(drive_outcome::Conflict{before + 2}));

    REQUIRE(view.prompt("Now write a test"));
    eventually("the prompt request", [&] { return !owner->prompts().empty(); });
    CHECK(owner->prompts()[0] == PromptRequested{guest->key(), "s-shared", "Now write a test"});
    CHECK_FALSE(view.prompt("   "));

    // A fork copies the mirror into a journal this node owns.
    eventually("the view to catch up", [&] { return view.source_seq() == writer->seq(); });
    auto dir = runtime::TempDir::create();
    REQUIRE(dir);
    auto store = bus::Store::make(dir->path() / "sessions");
    REQUIRE(store);
    const auto patches = view.patches();
    auto forked = fork_mirror_to_own_node(*store, patches, view.writer()->seq(), "s-fork", guest->key().to_string());
    REQUIRE_MESSAGE(forked, forked.error().to_string());
    auto dom = forked->load();
    REQUIRE(dom);
    CHECK(dom->root()->str_attr(attr::NODE) == guest->key().to_string());
    CHECK(dom->root()->str_attr(attr::PARENT_SESSION) == "s-shared");
    CHECK(text_of(*dom, "user") == "Why does it fail?");

    // Revoking stops the view at the owner's next patch and tells the guest.
    for (const auto& id : owner->given()) owner->revoke(*guest, id);
    REQUIRE(writer->commit_at(Draft::append(answer, " after revoke"), runtime::time::now_ms()));
    eventually("the view to end", [&] { return view.ended().has_value(); });
    CHECK(view_text(view, "assistant")->find("after revoke") == std::string::npos);
    for (auto& g : guest->grants().infos())
        if (g.direction == GrantDirection::received) CHECK(g.ended);
    CHECK_FALSE(session::open(*guest, owner->key(), "s-shared"));
}

TEST_CASE("view-only grants refuse drive and hide other prompts") {
    auto [owner, guest] = pair("owner", "guest");
    auto [writer, unused] = owner->session("s-view");
    const auto root = writer->with_dom([](const core::Dom& dom) { return dom.root()->id; });
    auto second = writer->commit_at(Draft::insert(root, ElementKind::Turn), runtime::time::now_ms());
    REQUIRE(second);
    REQUIRE(writer->commit_at(
        Draft::insert(second->node, ElementKind::Message, {{std::string(attr::ROLE), Value("user")}}, "only this one"),
        runtime::time::now_ms()));

    owner->grant(*guest, trust::resource::Prompt{"s-view", second->node}, Action::view);
    auto listed = session::list(*guest, owner->key());
    REQUIRE(listed);
    REQUIRE(listed->size() == 1);
    CHECK((*listed)[0].partial);
    CHECK((*listed)[0].actions.empty());
    CHECK((*listed)[0].title.empty());

    auto opened = session::open(*guest, owner->key(), "s-view");
    REQUIRE_MESSAGE(opened, opened.error().to_string());
    auto& view = **opened;
    auto messages = view.writer()->with_dom([](const core::Dom& dom) {
        std::vector<std::string> out;
        for (const auto& n : dom.iter())
            if (n.kind == ElementKind::Message) out.push_back(n.text);
        return out;
    });
    CHECK(messages == std::vector<std::string>{"only this one"});
    CHECK(view.writer()->with_dom([](const core::Dom& dom) { return dom.root()->attrs.contains(attr::TITLE); }) == false);

    const auto message = view.writer()->with_dom(first_message);
    auto drive = view.drive(Draft::append(message, "!"));
    REQUIRE_FALSE(drive);
    CHECK(drive.error().to_string() == "the peer refused: no drive grant on s-view");
    auto prompt = view.prompt("hi");
    REQUIRE_FALSE(prompt);
    CHECK(prompt.error().to_string() == "the peer refused: the peer refused: the grant does not allow driving this session");
    auto direct = session::prompt(*guest, owner->key(), "s-view", "hi");
    REQUIRE_FALSE(direct);

    // The owner's own drive authorizer refuses the peer the same way.
    auto request = writer->request(trust::to_bus(guest->key()), std::nullopt, Draft::append(core::NodeId{5}, "!"), 1);
    REQUIRE_FALSE(request);
    CHECK(bus::to_string(request.error()).find("no drive grant on s-view for this peer") != std::string::npos);
}

TEST_CASE("sessions without a grant are not listed or opened") {
    auto [owner, guest] = pair("owner", "guest");
    owner->session("s-private");
    auto listed = session::list(*guest, owner->key());
    REQUIRE(listed);
    CHECK(listed->empty());
    auto opened = session::open(*guest, owner->key(), "s-private");
    REQUIRE_FALSE(opened);
    CHECK(opened.error().to_string() == "the peer refused: no view grant on s-private");
    auto missing = session::open(*guest, owner->key(), "nope");
    REQUIRE_FALSE(missing);
    CHECK(missing.error().to_string() == "the peer refused: no such session");
}

TEST_CASE("a subscriber that falls 4096 events behind is cut off") {
    auto [owner, guest] = pair("owner", "guest");
    auto [writer, answer] = owner->session("s-slow");
    owner->grant(*guest, trust::resource::Session{"s-slow"}, Action::view);
    auto conn = guest->dial(owner->key(), wire::SESSION);
    REQUIRE(conn);
    auto stream = (*conn)->open_stream();
    REQUIRE(stream);
    REQUIRE(wire::send(*stream->send, wire::Frame{wire::Body(wire::Subscribe{"s-slow", 0})}));
    REQUIRE(guest->grants().present(owner->key(), *stream));
    auto seed = wire::expect<wire::Frame>(*stream->recv);
    REQUIRE(seed);
    REQUIRE(std::holds_alternative<wire::Seed>(*seed->body));
    // The owner blocks on the unread stream while its bus queue overflows.
    const std::string chunk(4096, 'x');
    for (std::size_t i = 0; i < SUBSCRIBER_QUEUE * 3; ++i)
        REQUIRE(writer->commit_at(Draft::append(answer, chunk), runtime::time::now_ms()));
    std::optional<std::string> last;
    std::size_t updates = 0;
    while (true) {
        auto frame = wire::recv<wire::Frame>(*stream->recv);
        REQUIRE(frame);
        if (!*frame) break;
        if (auto* ack = std::get_if<wire::Ack>(&*(*frame)->body)) last = ack->error;
        else ++updates;
    }
    CHECK(last == "fell behind; open the session again");
    CHECK(updates < SUBSCRIBER_QUEUE * 3);
    (*conn)->close(0, "done");
}

TEST_CASE("grants renew over control and stop after revocation") {
    auto [owner, guest] = pair("owner", "guest");
    auto id = owner->grant(*guest, trust::resource::Session{"s"}, Action::view);
    auto held = guest->grants().store->received(id);
    REQUIRE(held);
    REQUIRE(*held);
    const auto first = (**held).expires_ms;
    std::this_thread::sleep_for(5ms);

    control::RenewVia via(guest);
    asio::io_context io;
    std::optional<std::expected<std::vector<std::uint8_t>, trust::RenewFailure>> renewed;
    asio::co_spawn(io, [&]() -> asio::awaitable<void> { renewed = co_await via.renew(owner->key(), id); }, asio::detached);
    io.run();
    REQUIRE(renewed);
    REQUIRE_MESSAGE(*renewed, renewed->error().to_string());
    auto stored = guest->grants().store->receive_renewal(guest->key(), owner->key(), id, **renewed);
    REQUIRE(stored);
    CHECK(stored->expires_ms > first);

    owner->revoke(*guest, id);
    CHECK(guest->grants().infos().at(0).ended);
    CHECK_FALSE(guest->grants().holds_from(owner->key()));
    io.restart();
    renewed.reset();
    asio::co_spawn(io, [&]() -> asio::awaitable<void> { renewed = co_await via.renew(owner->key(), id); }, asio::detached);
    io.run();
    REQUIRE(renewed);
    REQUIRE_FALSE(*renewed);
    CHECK(renewed->error().kind == trust::RenewFailure::Kind::refused);
}

TEST_CASE("only a grant's issuer may deliver or revoke it") {
    auto [owner, guest] = pair("owner", "guest");
    auto third = TestNode::make("third");
    third->meet(*guest);
    guest->meet(*third);
    auto scope = trust::Scope::make(trust::resource::Session{"s"}, {Action::view});
    REQUIRE(scope);
    auto issued = owner->grants().store->issue(owner->grants().identity, guest->key(),
                                               trust::GrantSpec{{*scope}, trust::DEFAULT_TTL_MS, false, std::nullopt},
                                               runtime::time::now_ms());
    REQUIRE(issued);
    // Forwarded by a third node, naming the owner as root.
    const auto& root = owner->key().bytes;
    auto forwarded = control::call_peer(*third, guest->key(), wire::GrantDelivery{issued->token, Bytes(root.begin(), root.end())});
    REQUIRE_FALSE(forwarded);
    CHECK(forwarded.error().to_string() == "the peer refused: the peer refused: only a grant's issuer may deliver it");

    auto id = owner->grant(*guest, trust::resource::Session{"s2"}, Action::view);
    const auto& text = id.as_str();
    auto revoked = control::call_peer(*third, guest->key(), wire::GrantRevocation{Bytes(text.begin(), text.end())});
    REQUIRE_FALSE(revoked);
    CHECK(revoked.error().to_string() == "the peer refused: the peer refused: only a grant's issuer may revoke it");
    CHECK(guest->grants().holds_from(owner->key()));

    control::Probe probe(guest);
    CHECK(probe.probe(owner->address()) == owner->key());
}

namespace {

// Echoes every byte back on the stream.
struct Echo final : PeerProtocol {
    void serve(const NodeId&, Stream stream) override {
        std::uint8_t buf[256];
        while (true) {
            auto n = stream.recv->read(buf);
            if (!n || *n == 0) break;
            if (!stream.send->write_all(std::span(buf, *n))) break;
        }
        (void)stream.send->shutdown();
    }
};

} // namespace

TEST_CASE("added protocols serve only peers with a covering grant") {
    const AddedProtocol echo{"loa-echo", 1, {trust::resource::Sandbox{}, Action::exec}, std::make_shared<Echo>()};
    auto [owner, guest] = pair("owner-box", "guest-box", {echo});
    auto refused = extra::open(*guest, owner->key(), "loa-echo", 1);
    REQUIRE_FALSE(refused);
    CHECK(refused.error().kind() == NodeError::Kind::refused);
    owner->grant(*guest, trust::resource::Ssh{"box"}, Action::exec);
    CHECK_FALSE(extra::open(*guest, owner->key(), "loa-echo", 1));

    auto id = owner->grant(*guest, trust::resource::Sandbox{}, Action::exec);
    auto stream = extra::open(*guest, owner->key(), "loa-echo", 1);
    REQUIRE_MESSAGE(stream, stream.error().to_string());
    const std::string ping = "ping";
    REQUIRE(stream->send->write_all(std::span(reinterpret_cast<const std::uint8_t*>(ping.data()), ping.size())));
    REQUIRE(stream->send->shutdown());
    std::string echoed;
    std::uint8_t buf[64];
    while (true) {
        auto n = stream->recv->read(buf);
        REQUIRE(n);
        if (*n == 0) break;
        echoed.append(reinterpret_cast<const char*>(buf), *n);
    }
    CHECK(echoed == "ping");

    owner->revoke(*guest, id);
    CHECK_FALSE(extra::open(*guest, owner->key(), "loa-echo", 1));
}

TEST_CASE("folder grants list, read and write only inside the folder") {
    auto [owner, guest] = pair("owner-box", "guest-box");
    auto files = runtime::TempDir::create();
    REQUIRE(files);
    auto base = runtime::fs::canonicalize(files->path());
    REQUIRE(base);
    const auto share = *base / "share";
    stdfs::create_directories(share / "docs");
    auto write = [](const stdfs::path& p, std::string_view bytes) { std::ofstream(p, std::ios::binary) << bytes; };
    auto read = [](const stdfs::path& p) {
        std::ifstream in(p, std::ios::binary);
        return std::string(std::istreambuf_iterator<char>(in), {});
    };
    write(*base / "secret.txt", "outside");
    // More than one 512 KiB frame.
    std::string big(1'300'000, '\0');
    for (std::size_t i = 0; i < big.size(); ++i) big[i] = static_cast<char>(i % 251);
    write(share / "docs/big.bin", big);
    write(share / "notes.txt", "hello");
    const auto folder = share.string();
    auto as_bytes = [](std::string_view s) { return std::span(reinterpret_cast<const std::uint8_t*>(s.data()), s.size()); };
    auto& g = *guest;
    const auto key = owner->key();

    auto refused = folder::list_remote(g, key, folder, "");
    REQUIRE_FALSE(refused);
    CHECK(refused.error().kind() == NodeError::Kind::refused);

    // A read grant: list, read (several frames), no writes, nothing outside.
    auto id = owner->grant(g, trust::resource::Folder{folder}, Action::read);
    auto entries = folder::list_remote(g, key, folder, "");
    REQUIRE_MESSAGE(entries, entries.error().to_string());
    REQUIRE(entries->size() == 2);
    CHECK(((*entries)[0].name == "docs" && (*entries)[0].dir));
    CHECK(((*entries)[1].name == "notes.txt" && !(*entries)[1].dir));
    auto got = folder::read_remote(g, key, folder, "docs/big.bin");
    REQUIRE(got);
    CHECK(std::string(got->begin(), got->end()) == big);
    auto docs = folder::list_remote(g, key, folder + "/docs", "");
    REQUIRE(docs);
    CHECK(docs->at(0).size == big.size());
    for (auto escape : {"../secret.txt", "/etc/hostname", "docs/../../secret.txt"}) {
        CAPTURE(escape);
        CHECK_FALSE(folder::read_remote(g, key, folder, escape));
    }
    CHECK_FALSE(folder::list_remote(g, key, base->string(), ""));
    auto denied = folder::write_remote(g, key, folder, "notes.txt", as_bytes("changed"));
    REQUIRE_FALSE(denied);
    CHECK(denied.error().kind() == NodeError::Kind::refused);
    CHECK(denied.error().message().find("no grant") != std::string::npos);

    // A write grant writes, in several frames too.
    owner->grant(g, trust::resource::Folder{folder}, Action::write);
    REQUIRE(folder::write_remote(g, key, folder, "docs/copy.bin", as_bytes(big)));
    CHECK(read(share / "docs/copy.bin") == big);
    REQUIRE(folder::write_remote(g, key, folder, "empty.txt", {}));
    CHECK(read(share / "empty.txt").empty());
    REQUIRE(folder::write_remote(g, key, folder, "notes.txt", as_bytes("updated")));
    CHECK(read(share / "notes.txt") == "updated");
    stdfs::create_symlink("notes.txt", share / "link.txt");
    CHECK_FALSE(folder::write_remote(g, key, folder, "link.txt", as_bytes("changed")));
    CHECK(read(share / "notes.txt") == "updated");
    CHECK(stdfs::is_symlink(stdfs::symlink_status(share / "link.txt")));
    CHECK_FALSE(folder::write_remote(g, key, folder, "../escaped.txt", as_bytes("x")));
    CHECK_FALSE(stdfs::exists(*base / "escaped.txt"));

    // Revoking the read grant leaves the write grant working; revoking it too ends access.
    owner->revoke(g, id);
    CHECK(folder::list_remote(g, key, folder, ""));
    for (const auto& given : owner->given()) owner->revoke(g, given);
    CHECK_FALSE(folder::list_remote(g, key, folder, ""));
}
