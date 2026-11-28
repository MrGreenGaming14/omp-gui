// `SqliteAudit` and `MemoryAudit` filter alike; the `remote_audit` table matches the Rust golden.

#include <doctest/doctest.h>

#include <sqlite3.h>

#include <filesystem>

#include "loa/remote/audit.hpp"
#include "loa/runtime/json.hpp"
#include "loa/runtime/tempfile.hpp"
#include "sshd.hpp"

using namespace loa::remote;
namespace json = loa::runtime::json;
using std::chrono::milliseconds;
using std::chrono::seconds;

namespace {

const std::filesystem::path GOLDEN = std::filesystem::path(LOA_GOLDEN_DIR) / "remote";

// `SELECT * FROM remote_audit ORDER BY id` as the Rust golden writer dumps it.
std::string dump_rows(const std::filesystem::path& db_path) {
    sqlite3* db = nullptr;
    REQUIRE(sqlite3_open(db_path.c_str(), &db) == SQLITE_OK);
    sqlite3_stmt* st = nullptr;
    REQUIRE(sqlite3_prepare_v2(db, "SELECT * FROM remote_audit ORDER BY id", -1, &st, nullptr) == SQLITE_OK);
    json::Array rows;
    while (sqlite3_step(st) == SQLITE_ROW) {
        json::Object row;
        for (int i = 0; i < sqlite3_column_count(st); ++i) {
            const std::string name = sqlite3_column_name(st, i);
            if (sqlite3_column_type(st, i) == SQLITE_INTEGER) {
                row.emplace_back(name, json::Value(static_cast<std::int64_t>(sqlite3_column_int64(st, i))));
            } else {
                row.emplace_back(name, std::string(reinterpret_cast<const char*>(sqlite3_column_text(st, i)),
                                                   static_cast<std::size_t>(sqlite3_column_bytes(st, i))));
            }
        }
        rows.emplace_back(std::move(row));
    }
    sqlite3_finalize(st);
    sqlite3_close(db);
    return json::dump_pretty(json::Value(json::Object{{"remote_audit", json::Value(std::move(rows))}})) + "\n";
}

struct GoldenRow {
    const char* host;
    const char* session;
    Action action;
    const char* target;
    Outcome outcome;
    const char* detail;
};

const GoldenRow GOLDEN_ROWS[] = {
    {"db", "s1", Action::write, "/etc/app.conf", Outcome::ok, "12 bytes"},
    {"db", "s2", Action::command, "rm -rf /", Outcome::refused, "blocked by policy"},
    {"web", "s1", Action::terminal, "", Outcome::denied, "user declined"},
    {"web", "s1", Action::read, "/var/log/na\xC3\xAFve \xE2\x9C\x93.log", Outcome::timed_out, ""},
};

} // namespace

TEST_CASE("both sinks filter the same and sqlite survives reopen") {
    const auto dir = *loa::runtime::TempDir::create();
    const auto path = dir.path() / "audit.db";
    const Time t0 = Time{} + seconds(1'700'000'000);
    MemoryAudit memory;
    const std::pair<const char*, Outcome> rows[] = {{"s1", Outcome::ok}, {"s2", Outcome::refused}, {"s1", Outcome::denied}};
    for (std::size_t i = 0; i < 3; ++i) {
        NewRecord row{t0 + seconds(i), "db", rows[i].first, Action::write, "/etc/app.conf", rows[i].second, ""};
        auto sqlite = SqliteAudit::open(path);
        REQUIRE(sqlite);
        REQUIRE((*sqlite)->record(row));
        REQUIRE(memory.record(row));
    }
    auto sqlite = SqliteAudit::open(path);
    REQUIRE(sqlite);
    for (const AuditSink* sink : {static_cast<const AuditSink*>(sqlite->get()), static_cast<const AuditSink*>(&memory)}) {
        AuditQuery by_session;
        by_session.session_id = "s1";
        const auto s1 = sink->query(by_session);
        REQUIRE(s1);
        REQUIRE(s1->size() == 2);
        CHECK((*s1)[0].outcome == Outcome::denied);
        CHECK((*s1)[0].at == t0 + seconds(2));
        CHECK((*s1)[1].outcome == Outcome::ok);
        CHECK((*s1)[1].at == t0);
        AuditQuery refused;
        refused.outcome = Outcome::refused;
        refused.since = t0 + seconds(1);
        CHECK(sink->query(refused)->at(0).session_id == "s2");
        AuditQuery page;
        page.before_id = (*s1)[0].id;
        page.limit = 1;
        CHECK(sink->query(page)->at(0).session_id == "s2");
        CHECK((*sink->last())->id == (*s1)[0].id);
    }
}

TEST_CASE("remote_audit rows written here match the Rust golden byte for byte") {
    const auto dir = *loa::runtime::TempDir::create();
    const auto path = dir.path() / "audit.db";
    auto audit = SqliteAudit::open(path);
    REQUIRE(audit);
    const Time t0 = Time{} + milliseconds(1'700'000'000'123);
    for (std::size_t i = 0; i < std::size(GOLDEN_ROWS); ++i) {
        const auto& g = GOLDEN_ROWS[i];
        const auto id = (*audit)->record(NewRecord{t0 + milliseconds(i * 1001), g.host, g.session, g.action, g.target, g.outcome, g.detail});
        REQUIRE(id);
        CHECK(*id == static_cast<std::int64_t>(i) + 1);
    }
    CHECK((*audit)->query({})->size() == 4);
    CHECK(dump_rows(path) == test::slurp(GOLDEN / "remote_audit.rows.json"));
    CHECK(AUDIT_SCHEMA == test::slurp(GOLDEN / "remote_audit.sql"));
}

TEST_CASE("a remote_audit database the Rust schema built reads back identically") {
    const auto dir = *loa::runtime::TempDir::create();
    const auto path = dir.path() / "rust.db";
    {
        // Rebuild the Rust database from its schema and rows.
        sqlite3* db = nullptr;
        REQUIRE(sqlite3_open(path.c_str(), &db) == SQLITE_OK);
        REQUIRE(sqlite3_exec(db, test::slurp(GOLDEN / "remote_audit.sql").c_str(), nullptr, nullptr, nullptr) == SQLITE_OK);
        const auto rows = json::parse(test::slurp(GOLDEN / "remote_audit.rows.json"));
        REQUIRE(rows);
        for (const auto& row : *rows->find("remote_audit")->as_array()) {
            sqlite3_stmt* st = nullptr;
            REQUIRE(sqlite3_prepare_v2(db, "INSERT INTO remote_audit VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)", -1, &st, nullptr) == SQLITE_OK);
            int i = 1;
            for (const auto& [name, value] : *row.as_object()) {
                if (const auto* s = value.as_string()) sqlite3_bind_text(st, i, s->c_str(), static_cast<int>(s->size()), SQLITE_TRANSIENT);
                else sqlite3_bind_int64(st, i, *value.as_i64());
                ++i;
            }
            REQUIRE(sqlite3_step(st) == SQLITE_DONE);
            sqlite3_finalize(st);
        }
        sqlite3_close(db);
    }
    auto audit = SqliteAudit::open(path);
    REQUIRE(audit);
    const auto rows = (*audit)->query({});
    REQUIRE(rows);
    REQUIRE(rows->size() == 4);
    const Time t0 = Time{} + milliseconds(1'700'000'000'123);
    for (std::size_t i = 0; i < 4; ++i) {
        const auto& got = (*rows)[3 - i];
        const auto& g = GOLDEN_ROWS[i];
        CHECK(got == AuditRecord{static_cast<std::int64_t>(i) + 1, t0 + milliseconds(i * 1001), g.host, g.session, g.action, g.target, g.outcome,
                                 g.detail});
    }
    // Appending keeps the ids going and leaves the file as Rust would.
    CHECK(*(*audit)->record(NewRecord{t0, "db", "s3", Action::list, "/", Outcome::ok, "0 entries"}) == 5);
    CHECK(dump_rows(path).find("\"id\": 5") != std::string::npos);
}

TEST_CASE("actions and outcomes keep their Rust names") {
    CHECK(to_string(Action::command) == "command");
    CHECK(parse_action("terminal") == Action::terminal);
    CHECK_FALSE(parse_action("Command"));
    CHECK(to_string(Outcome::timed_out) == "timed_out");
    CHECK(parse_outcome("cancelled") == Outcome::cancelled);
    CHECK(mutates(Action::terminal));
    CHECK_FALSE(mutates(Action::list));
}
