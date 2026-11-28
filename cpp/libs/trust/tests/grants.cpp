#include <doctest/doctest.h>

#include <asio.hpp>
#include <sqlite3.h>

#include <algorithm>
#include <cstdlib>
#include <filesystem>
#include <fstream>
#include <iterator>
#include <optional>
#include <string>
#include <vector>

#include "loa/runtime/json.hpp"
#include "loa/trust/renew.hpp"
#include "loa/trust/scope.hpp"
#include "loa/trust/store.hpp"
#include "loa/trust/token.hpp"

using namespace loa::trust;
using loa::core::NodeId;
namespace fs = std::filesystem;
namespace json = loa::runtime::json;

namespace {

constexpr std::int64_t NOW = 1'800'000'000'000;
constexpr std::int64_t TTL = 60'000;
constexpr auto View = Action::view, Suggest = Action::suggest, Drive = Action::drive, Fork = Action::fork,
               Read = Action::read, Write = Action::write, Exec = Action::exec;

NodeIdentity key(std::uint8_t byte) {
    std::array<std::uint8_t, 32> seed{};
    seed.fill(byte);
    return NodeIdentity::from_secret_bytes(seed);
}

Scope scope(Resource resource, Action action) {
    auto s = Scope::make(std::move(resource), {action});
    REQUIRE(s.has_value());
    return *s;
}

GrantStore store() {
    auto s = GrantStore::open_in_memory();
    REQUIRE(s.has_value());
    return std::move(*s);
}

SessionFacts session(std::string id, std::optional<std::string> project, std::int64_t created_ms) {
    return SessionFacts{std::move(id), created_ms, std::move(project)};
}

// An absolute path for the platform: `/srv/share` stays as is on Unix and
// becomes `C:\srv\share` on Windows. Relative paths pass through.
std::string abs(std::string path) {
#ifdef _WIN32
    if (path.starts_with('/')) {
        std::ranges::replace(path, '/', '\\');
        return "C:" + path;
    }
#endif
    return path;
}

Resource folder(std::string path) { return resource::Folder{abs(std::move(path))}; }

std::expected<VerifiedGrant, Denied> verify(const NodeIdentity& root, const GrantStore& store, const NodeIdentity& holder,
                                            std::span<const std::uint8_t> token, std::int64_t now) {
    Challenge challenge;
    challenge.bytes.fill(19);
    const auto proof = prove(holder, challenge, token);
    return Verifier(root.public_key(), store).verify(Presented{token, &proof}, holder.public_key(), challenge, now);
}

IssuedGrant issue(const NodeIdentity& root, const GrantStore& store, const NodeIdentity& holder, std::vector<Scope> scopes,
                  bool reshare) {
    auto issued = store.issue(root, holder.public_key(), GrantSpec{std::move(scopes), TTL, reshare, std::nullopt}, NOW);
    REQUIRE(issued.has_value());
    return std::move(*issued);
}

std::vector<Scope> sorted(std::vector<Scope> scopes) {
    std::ranges::sort(scopes, {}, [](const Scope& s) { return json::dump(scope_to_json(s)); });
    return scopes;
}

template <class T>
bool denied_as(const std::expected<T, Denied>& result, Denied::Kind kind) {
    return !result.has_value() && result.error().kind == kind;
}

template <class T>
bool fails_with(const Result<T>& result, TrustError::Kind kind) {
    return !result.has_value() && result.error().kind() == kind;
}

std::vector<std::uint8_t> slurp(const fs::path& path) {
    std::ifstream in(path, std::ios::binary);
    return {std::istreambuf_iterator<char>(in), {}};
}

std::string slurp_text(const fs::path& path) {
    const auto bytes = slurp(path);
    return {bytes.begin(), bytes.end()};
}

template <class T>
T block_on(Awaitable<T> task) {
    asio::io_context io;
    std::optional<T> out;
    asio::co_spawn(
        io, [](Awaitable<T> inner, std::optional<T>& out) -> Awaitable<void> { out.emplace(co_await std::move(inner)); }(std::move(task), out),
        asio::detached);
    io.run();
    REQUIRE(out.has_value());
    return std::move(*out);
}

} // namespace

TEST_CASE("authorize_matrix_covers_every_kind_and_prefix_traps") {
    const auto root = key(1), alice = key(2);
    const auto s = store();
    const std::vector<Scope> given = {
        scope(resource::Session{"one"}, Suggest),
        scope(resource::Prompt{"two", NodeId{3}}, View),
        scope(resource::Project{"work"}, Fork),
        scope(resource::FutureSessions{NOW}, Drive),
        scope(resource::Ssh{"box"}, Read),
        scope(resource::Surface{"surface://browser/tab"}, View),
        scope(resource::Sandbox{}, Exec),
        scope(folder("/srv/share"), Write),
    };
    const auto issued = issue(root, s, alice, given, false);
    auto grant = verify(root, s, alice, issued.token, NOW);
    REQUIRE(grant.has_value());
    const auto one = session("one", std::nullopt, 0), two = session("two", std::nullopt, 0),
               onex = session("one-more", std::nullopt, 0);
    const auto work = session("w", "work", 0), fresh = session("n", std::nullopt, NOW), old = session("o", "workshop", NOW - 1);
    const std::string paths[] = {abs("/srv/share/docs/deep"), abs("/srv/share"), abs("/srv/shared"),
                                 abs("/srv"), abs("srv/share"), abs("/srv/share/../etc")};
    const std::pair<Request, bool> rows[] = {
        {Request::session(Suggest, one), true},
        {Request::session(View, one), true},
        {Request::session(Drive, one), false},
        {Request::session(View, onex), false},
        {Request::turn(View, two, NodeId{3}), true},
        {Request::turn(View, two, NodeId{4}), false},
        {Request::session(View, two), false},
        {Request::session(Fork, work), true},
        {Request::session(Drive, work), false},
        {Request::session(Drive, fresh), true},
        {Request::session(View, old), false},
        {Request::ssh(Read, "box"), true},
        {Request::ssh(Exec, "box"), false},
        {Request::ssh(Read, "box2"), false},
        {Request::surface(View, "surface://browser/tab"), true},
        {Request::surface(Drive, "surface://browser/tab"), false},
        {Request::surface(View, "surface://browser/tab2"), false},
        {Request::sandbox(Exec), true},
        {Request::folder(Write, paths[0]), true},
        {Request::folder(Read, paths[1]), true},
        {Request::folder(Read, paths[2]), false},
        {Request::folder(Read, paths[3]), false},
        {Request::folder(Read, paths[4]), false},
        {Request::folder(Read, paths[5]), false},
        {Request::ssh(Read, "/srv/share"), false},
    };
    for (const auto& [request, allowed] : rows) {
        const auto result = grant->authorize(request, NOW);
        INFO(request.debug(), " -> ", result ? std::string("ok") : result.error().to_string());
        CHECK((allowed ? result.has_value() : denied_as(result, Denied::Kind::not_granted)));
    }
    CHECK(denied_as(grant->authorize(Request::sandbox(Exec), NOW + TTL), Denied::Kind::expired));
    // The holder reads the scopes back out of the token, implied actions included.
    auto scopes = token_scopes(issued.token, root.public_key());
    REQUIRE(scopes.has_value());
    CHECK(sorted(*scopes) == sorted(given));

    auto bad_action = Scope::make(resource::Ssh{"b"}, {View});
    REQUIRE(fails_with(bad_action, TrustError::Kind::action));
    CHECK(bad_action.error().message() == "view");
    CHECK_FALSE(Scope::make(resource::Sandbox{}, {View}).has_value());
    CHECK(fails_with(Scope::make(resource::Project{"p"}, std::span<const Action>{}), TrustError::Kind::no_actions));
    auto surface = Scope::make(resource::Surface{"s"}, {Drive});
    REQUIRE(surface.has_value());
    CHECK(surface->actions == ActionSet{View, Drive});
    for (const char* bad : {"relative", "/a/../b", "/a/", "/a//b", ""}) {
        INFO(bad);
        CHECK(fails_with(Scope::make(folder(bad), {Read}), TrustError::Kind::folder_path));
    }
}

TEST_CASE("reshare_chain_narrows_needs_the_holder_key_and_stops_at_revocation") {
    const auto root = key(6), alice = key(7), bob = key(8), carol = key(9);
    const auto issuer = store(), alice_store = store(), bob_store = store(), carol_store = store();
    const Resource one = resource::Session{"one"};
    const auto parent = issue(root, issuer, alice, {scope(one, Drive), scope(folder("/srv"), Write)}, true);
    REQUIRE(alice_store.receive(alice.public_key(), root.public_key(), parent.token).has_value());
    Narrowing narrowing{std::vector<Scope>{scope(one, View), scope(folder("/srv/docs"), Read)}, 2 * TTL, true};
    auto child = alice_store.reshare(alice, parent.id, bob.public_key(), narrowing, NOW);
    REQUIRE(child.has_value());
    CHECK(child->expires_ms == parent.expires_ms);

    // Only the last holder, signing this challenge, can use the token.
    Challenge nonce;
    nonce.bytes.fill(17);
    const Verifier verifier(root.public_key(), issuer);
    const auto stolen_proof = prove(carol, nonce, child->token);
    const Presented stolen{child->token, &stolen_proof};
    CHECK(denied_as(verifier.verify(stolen, carol.public_key(), nonce, NOW), Denied::Kind::not_holder));
    CHECK(denied_as(verifier.verify(stolen, bob.public_key(), nonce, NOW), Denied::Kind::bad_proof));

    auto grant = verify(root, issuer, bob, child->token, NOW);
    REQUIRE(grant.has_value());
    CHECK(grant->chain().size() == 2);
    const auto s1 = session("one", std::nullopt, NOW);
    CHECK(grant->authorize(Request::session(View, s1), NOW).has_value());
    const auto docs_x = abs("/srv/docs/x"), srv = abs("/srv"), docs = abs("/srv/docs");
    CHECK(grant->authorize(Request::folder(Read, docs_x), NOW).has_value());
    for (const auto& denied : {Request::session(Drive, s1), Request::folder(Read, srv), Request::folder(Write, docs)}) {
        const auto result = grant->authorize(denied, NOW);
        INFO(denied.debug());
        CHECK(denied_as(result, Denied::Kind::narrowed));
        if (!result) CHECK(result.error().block == 1);
    }

    REQUIRE(bob_store.receive(bob.public_key(), root.public_key(), child->token).has_value());
    Narrowing last{std::vector<Scope>{scope(folder("/srv/docs"), Read)}, TTL, false};
    auto grandchild = bob_store.reshare(bob, child->id, carol.public_key(), last, NOW);
    REQUIRE(grandchild.has_value());
    auto carols = verify(root, issuer, carol, grandchild->token, NOW);
    REQUIRE(carols.has_value());
    CHECK(carols->authorize(Request::folder(Read, docs), NOW).has_value());
    CHECK_FALSE(carols->authorize(Request::session(View, s1), NOW).has_value());
    REQUIRE(carol_store.receive(carol.public_key(), root.public_key(), grandchild->token).has_value());
    Narrowing again{std::nullopt, TTL, false};
    CHECK(fails_with(carol_store.reshare(carol, grandchild->id, alice.public_key(), again, NOW),
                     TrustError::Kind::reshare_forbidden));

    // Revoking the root grant denies every token cut from it.
    REQUIRE(issuer.revoke(root.public_key(), parent.id, NOW + 1).has_value());
    CHECK(denied_as(verify(root, issuer, carol, grandchild->token, NOW + 2), Denied::Kind::revoked));
    CHECK(fails_with(issuer.renew(root, parent.id, alice.public_key(), NOW + 2), TrustError::Kind::revoked));
}

namespace {

struct LocalTransport final : RenewTransport {
    const GrantStore& store;
    const NodeIdentity& issuer;
    PublicKey requester;

    LocalTransport(const GrantStore& s, const NodeIdentity& i, PublicKey r) : store(s), issuer(i), requester(r) {}

    Awaitable<std::expected<std::vector<std::uint8_t>, RenewFailure>> renew(PublicKey, GrantId id) const override {
        auto renewed = store.renew(issuer, id, requester, NOW + 40'000);
        if (!renewed) co_return std::unexpected(RenewFailure::from_issuer(renewed.error()));
        co_return std::move(renewed->token);
    }
};

} // namespace

TEST_CASE("renewal_driver_refreshes_honours_the_end_and_stops_after_refusal") {
    const auto root = key(15), alice = key(16);
    const auto issuer = store(), holder = store();
    const auto issued = issue(root, issuer, alice, {scope(resource::Session{"s"}, View)}, false);
    REQUIRE(holder.receive(alice.public_key(), root.public_key(), issued.token).has_value());
    CHECK(fails_with(issuer.renew(root, issued.id, key(99).public_key(), NOW), TrustError::Kind::not_holder));
    const Renewer driver(alice.public_key(), 30'000);
    const LocalTransport transport(issuer, root, alice.public_key());
    CHECK(driver.next_due(holder) == Result<std::optional<std::int64_t>>(NOW + 30'000));
    auto early = block_on(driver.renew_due(holder, transport, NOW + 20'000));
    REQUIRE(early.has_value());
    CHECK(early->empty());
    auto renewed = block_on(driver.renew_due(holder, transport, NOW + 40'000));
    REQUIRE(renewed.has_value());
    REQUIRE(renewed->size() == 1);
    CHECK(std::holds_alternative<renew_outcome::Renewed>((*renewed)[0]));

    // An end caps tokens and stops renewal; extending restarts it.
    const auto until = NOW + TTL / 2;
    auto extended = issuer.extend(root, issued.id, until, NOW);
    REQUIRE(extended.has_value());
    CHECK(extended->expires_ms == until);
    CHECK(fails_with(issuer.renew(root, issued.id, alice.public_key(), until), TrustError::Kind::expired));
    REQUIRE(issuer.extend(root, issued.id, std::nullopt, until).has_value());
    auto records = issuer.issued();
    REQUIRE(records.has_value());
    REQUIRE(records->size() == 1);
    CHECK((*records)[0].holder == alice.public_key());
    CHECK_FALSE((*records)[0].until_ms.has_value());

    REQUIRE(issuer.revoke(root.public_key(), issued.id, NOW + 41'000).has_value());
    CHECK((*issuer.issued())[0].revoked_ms.has_value());
    auto refused = block_on(driver.renew_due(holder, transport, NOW + 80'000));
    REQUIRE(refused.has_value());
    REQUIRE(refused->size() == 1);
    CHECK(std::holds_alternative<renew_outcome::Refused>((*refused)[0]));
    CHECK(driver.next_due(holder) == Result<std::optional<std::int64_t>>(std::nullopt));
}

TEST_CASE("renew failures from the issuer: final ones refuse, others retry") {
    for (const auto& e : {TrustError::revoked("g"), TrustError::expired("g"), TrustError::unknown_grant("g"),
                          TrustError::not_holder("g"), TrustError::reshare_forbidden("g")})
        CHECK(RenewFailure::from_issuer(e) == RenewFailure::refused(e.to_string()));
    CHECK(RenewFailure::from_issuer(TrustError::sql("x")).kind == RenewFailure::Kind::unreachable);
    CHECK(RenewFailure::refused("grant g is revoked").to_string() == "the issuer refused to renew: grant g is revoked");
}

// A token minted and re-shared by the Rust code before the kind table (root
// seed 1 to seed 2, re-shared to seed 3 narrowed to session `one` view and
// `/srv/share/docs` read; valid until 5.8e12).
TEST_CASE("tokens_minted_before_the_kind_table_still_verify") {
    const auto token = slurp(fs::path(LOA_GOLDEN_DIR) / "trust" / "v1_reshared_token.bin");
    REQUIRE(token.size() == 1376);
    const auto root = key(1);
    const auto s = store();
    auto grant = verify(root, s, key(3), token, NOW);
    REQUIRE(grant.has_value());
    CHECK(grant->chain().size() == 2);
    const auto one = session("one", std::nullopt, 0);
    CHECK(grant->authorize(Request::session(View, one), NOW).has_value());
#ifndef _WIN32
    CHECK(grant->authorize(Request::folder(Read, "/srv/share/docs/a"), NOW).has_value());
#endif
    CHECK(denied_as(grant->authorize(Request::session(Drive, one), NOW), Denied::Kind::narrowed));
    CHECK(denied_as(grant->authorize(Request::sandbox(Exec), NOW), Denied::Kind::narrowed));
    auto scopes = token_scopes(token, root.public_key());
    REQUIRE(scopes.has_value());
    CHECK(scopes->size() == KINDS.size());
    CHECK(std::ranges::find(*scopes, scope(resource::Prompt{"two", NodeId{3}}, View)) != scopes->end());
    CHECK(std::ranges::find(*scopes, scope(resource::FutureSessions{NOW}, Suggest)) != scopes->end());

    // The golden key file names the keys it verifies with.
    const auto keys = slurp_text(fs::path(LOA_GOLDEN_DIR) / "trust" / "v1_reshared_token.keys");
    CHECK(keys == "root " + root.public_key().to_string() + "\nholder " + key(3).public_key().to_string() + "\n");
}

TEST_CASE("rules and the token Datalog are generated from the kind table") {
    CHECK(rules() ==
          "granted($a) <- session_right($v0, $a), session($v0);\n"
          "granted($a) <- prompt_right($v0, $v1, $a), session($v0), turn($v1);\n"
          "granted($a) <- project_right($v0, $a), project($v0);\n"
          "granted($a) <- future_right($v0, $a), created($c), $c >= $v0;\n"
          "granted($a) <- ssh_right($v0, $a), ssh_host($v0);\n"
          "granted($a) <- surface_right($v0, $a), surface($v0);\n"
          "granted($a) <- sandbox_right($v0, $a), sandbox($v0);\n"
          "granted($a) <- folder_right($v0, $a), folder($v0);\n"
          "allow if action($a), granted($a);");
    auto ancestors = folder_ancestors("/a/./b/");
    REQUIRE(ancestors.has_value());
    CHECK(*ancestors == std::vector<std::string>{"/", "/a", "/a/b"});
    CHECK_FALSE(folder_ancestors("/a/../b").has_value());
    CHECK_FALSE(folder_ancestors("a/b").has_value());
}

namespace {

// Every row of `tables` as the Rust golden writer dumps them.
std::string dump(sqlite3* db, std::initializer_list<const char*> tables) {
    json::Object all;
    for (const char* table : tables) {
        sqlite3_stmt* st = nullptr;
        const auto sql = std::string("SELECT * FROM ") + table + " ORDER BY rowid";
        REQUIRE(sqlite3_prepare_v2(db, sql.c_str(), -1, &st, nullptr) == SQLITE_OK);
        json::Array rows;
        while (sqlite3_step(st) == SQLITE_ROW) {
            json::Object row;
            for (int i = 0; i < sqlite3_column_count(st); ++i) {
                json::Value value;
                switch (sqlite3_column_type(st, i)) {
                    case SQLITE_INTEGER: value = std::int64_t{sqlite3_column_int64(st, i)}; break;
                    case SQLITE_FLOAT: value = sqlite3_column_double(st, i); break;
                    case SQLITE_TEXT: value = std::string(reinterpret_cast<const char*>(sqlite3_column_text(st, i))); break;
                    case SQLITE_BLOB: {
                        const auto* p = static_cast<const std::uint8_t*>(sqlite3_column_blob(st, i));
                        static constexpr char HEX[] = "0123456789abcdef";
                        std::string hex;
                        for (int j = 0; j < sqlite3_column_bytes(st, i); ++j) {
                            hex += HEX[p[j] >> 4];
                            hex += HEX[p[j] & 15];
                        }
                        value = json::Object{{"hex", hex}};
                        break;
                    }
                    default: break;
                }
                row.emplace_back(sqlite3_column_name(st, i), std::move(value));
            }
            rows.emplace_back(std::move(row));
        }
        sqlite3_finalize(st);
        all.emplace_back(table, std::move(rows));
    }
    return json::dump_pretty(all) + "\n";
}

} // namespace

TEST_CASE("grants.sqlite schema and rows match the Rust golden files") {
    const fs::path sqlite_dir = fs::path(LOA_GOLDEN_DIR) / "sqlite";
    CHECK(GrantStore::SCHEMA == slurp_text(sqlite_dir / "grants.sql"));

    const auto root = key(1), holder = key(3);
    const auto s = store();
    auto session_view = Scope::make(resource::Session{"one"}, {View});
    REQUIRE(session_view.has_value());
    const GrantSpec spec{{*session_view}, 60'000, true, NOW + 3'600'000};
    const IssuedGrant issued{GrantId{"00112233445566778899aabbccddeeff"}, holder.public_key(), {}, NOW + 60'000};
    REQUIRE(s.insert_issued(issued, spec, nullptr, NOW).has_value());
    REQUIRE(s.revoke(root.public_key(), issued.id, NOW + 1).has_value());
    REQUIRE(s.record_revocation(GrantId{"ffeeddccbbaa99887766554433221100"}, root.public_key(), NOW + 2).has_value());
    const auto token = slurp(fs::path(LOA_GOLDEN_DIR) / "trust" / "v1_reshared_token.bin");
    auto received = s.receive(holder.public_key(), root.public_key(), token);
    REQUIRE(received.has_value());
    REQUIRE(s.mark_refused(received->id, NOW + 3).has_value());
    auto records = s.issued();
    REQUIRE(records.has_value());
    CHECK(records->size() == 1);
    CHECK((*records)[0].scopes == std::optional(std::vector<Scope>{*session_view}));
    CHECK((*records)[0].until_ms == NOW + 3'600'000);
    auto all = s.received_all();
    REQUIRE(all.has_value());
    REQUIRE(all->size() == 1);
    CHECK((*all)[0].refused_ms == NOW + 3);
    // The token is stored byte for byte.
    CHECK((*all)[0].token == token);

    CHECK(dump(s.connection(), {"grants_issued", "grants_received", "grants_revocations"}) ==
          slurp_text(sqlite_dir / "grants.rows.json"));
}

TEST_CASE("grant definitions round-trip through their serde JSON") {
    const std::string root_text =
        R"({"kind":"root","scopes":[{"resource":{"kind":"session","id":"one"},"actions":["view"]}],"ttl_ms":60000,"reshare":true,"until_ms":1800003600000})";
    auto root = definition_from_json(root_text);
    REQUIRE(root.has_value());
    CHECK(definition_to_json(*root) == root_text);

    std::vector<Scope> every = {
        scope(resource::Prompt{"two", NodeId{3}}, Drive), scope(resource::Project{"p"}, Fork),
        scope(resource::FutureSessions{5}, View), scope(resource::Ssh{"h"}, Write),
        scope(resource::Surface{"surface://a/b"}, Drive), scope(resource::Sandbox{}, Exec),
        scope(folder("/x"), Read)};
    const Definition reshare = Narrowing{every, 10, false};
    auto parsed = definition_from_json(definition_to_json(reshare));
    REQUIRE(parsed.has_value());
    CHECK(*parsed == reshare);
    CHECK(definition_to_json(Narrowing{std::nullopt, 10, true}) == R"({"kind":"reshare","scopes":null,"ttl_ms":10,"reshare":true})");
    CHECK(json::dump(scope_to_json(every[0])) ==
          R"({"resource":{"kind":"prompt","session":"two","turn":3},"actions":["view","suggest","drive"]})");
    // `until_ms` defaults to none when absent.
    auto old = definition_from_json(R"({"kind":"root","scopes":[],"ttl_ms":1,"reshare":false})");
    REQUIRE(old.has_value());
    CHECK_FALSE(std::get<GrantSpec>(*old).until_ms.has_value());
}

TEST_CASE("a token minted here round-trips for the Rust verifier") {
    // With LOA_TRUST_WRITE_TOKEN set, writes a token root seed 21 issued to
    // seed 22 (session `x` view, ttl 1e12 ms) for the Rust side to verify.
    const auto root = key(21), holder = key(22);
    const auto s = store();
    auto issued = s.issue(root, holder.public_key(), GrantSpec{{scope(resource::Session{"x"}, View)}, 1'000'000'000'000, true, std::nullopt}, NOW);
    REQUIRE(issued.has_value());
    if (const char* path = std::getenv("LOA_TRUST_WRITE_TOKEN")) {
        std::ofstream(path, std::ios::binary)
            .write(reinterpret_cast<const char*>(issued->token.data()), static_cast<std::streamsize>(issued->token.size()));
    }
    auto grant = verify(root, s, holder, issued->token, NOW);
    REQUIRE(grant.has_value());
    CHECK(grant->chain()[0].grant == issued->id);
    CHECK(grant->chain()[0].reshare);
    // With LOA_TRUST_READ_TOKEN set, verifies a token Rust minted the same way.
    if (const char* path = std::getenv("LOA_TRUST_READ_TOKEN")) {
        const auto rust = slurp(path);
        auto theirs = verify(root, s, holder, rust, NOW);
        REQUIRE(theirs.has_value());
        const auto x = session("x", std::nullopt, 0);
        CHECK(theirs->authorize(Request::session(View, x), NOW).has_value());
        CHECK(denied_as(theirs->authorize(Request::session(Drive, x), NOW), Denied::Kind::not_granted));
    }
}
