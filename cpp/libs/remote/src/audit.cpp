#include "loa/remote/audit.hpp"

#include <sqlite3.h>

#include <algorithm>
#include <limits>
#include <utility>
#include <variant>

namespace loa::remote {

namespace {

constexpr std::array<std::string_view, 6> ACTION_NAMES{"read", "write", "list", "edit", "command", "terminal"};
constexpr std::array<std::string_view, 6> OUTCOME_NAMES{"ok", "error", "refused", "denied", "cancelled", "timed_out"};

} // namespace

std::string_view to_string(Action action) noexcept { return ACTION_NAMES[static_cast<std::size_t>(action)]; }

std::optional<Action> parse_action(std::string_view text) noexcept {
    for (std::size_t i = 0; i < ACTION_NAMES.size(); ++i) {
        if (ACTION_NAMES[i] == text) return static_cast<Action>(i);
    }
    return std::nullopt;
}

bool mutates(Action action) noexcept {
    return action == Action::write || action == Action::edit || action == Action::command || action == Action::terminal;
}

std::string_view to_string(Outcome outcome) noexcept { return OUTCOME_NAMES[static_cast<std::size_t>(outcome)]; }

std::optional<Outcome> parse_outcome(std::string_view text) noexcept {
    for (std::size_t i = 0; i < OUTCOME_NAMES.size(); ++i) {
        if (OUTCOME_NAMES[i] == text) return static_cast<Outcome>(i);
    }
    return std::nullopt;
}

std::int64_t to_ms(Time t) noexcept {
    const auto ms = std::chrono::duration_cast<std::chrono::milliseconds>(t.time_since_epoch()).count();
    return ms < 0 ? 0 : ms;
}

Time from_ms(std::int64_t ms) noexcept { return Time{} + std::chrono::milliseconds(ms < 0 ? 0 : ms); }

bool AuditQuery::matches(const AuditRecord& r) const {
    return (!host || *host == r.host) && (!session_id || *session_id == r.session_id) && (!action || *action == r.action) &&
           (!outcome || *outcome == r.outcome) && (!since || to_ms(r.at) >= to_ms(*since)) &&
           (!until || to_ms(r.at) < to_ms(*until)) && (!before_id || r.id < *before_id);
}

Result<std::optional<AuditRecord>> AuditSink::last() const {
    AuditQuery q;
    q.limit = 1;
    auto rows = query(q);
    if (!rows) return std::unexpected(rows.error());
    if (rows->empty()) return std::optional<AuditRecord>{};
    return std::optional<AuditRecord>{std::move(rows->back())};
}

Result<std::int64_t> MemoryAudit::record(NewRecord r) {
    std::lock_guard lock(mu_);
    const auto id = static_cast<std::int64_t>(rows_.size()) + 1;
    rows_.push_back(AuditRecord{id, from_ms(to_ms(r.at)), std::move(r.host), std::move(r.session_id), r.action, std::move(r.target),
                                r.outcome, std::move(r.detail)});
    return id;
}

Result<std::vector<AuditRecord>> MemoryAudit::query(const AuditQuery& q) const {
    std::lock_guard lock(mu_);
    std::vector<AuditRecord> out;
    for (auto it = rows_.rbegin(); it != rows_.rend() && out.size() < q.effective_limit(); ++it) {
        if (q.matches(*it)) out.push_back(*it);
    }
    return out;
}

const std::string_view AUDIT_SCHEMA = R"(CREATE TABLE IF NOT EXISTS remote_audit (
    id INTEGER PRIMARY KEY AUTOINCREMENT, at_ms INTEGER NOT NULL, host TEXT NOT NULL, session_id TEXT NOT NULL,
    action TEXT NOT NULL, target TEXT NOT NULL, outcome TEXT NOT NULL, detail TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS remote_audit_session ON remote_audit (session_id, id);
CREATE INDEX IF NOT EXISTS remote_audit_host ON remote_audit (host, id);)";

namespace {

Error db_error(sqlite3* db) { return Error::audit(sqlite3_errmsg(db)); }

struct Stmt {
    sqlite3_stmt* s = nullptr;
    ~Stmt() { sqlite3_finalize(s); }
};

std::string text_column(sqlite3_stmt* s, int i) {
    const auto* p = reinterpret_cast<const char*>(sqlite3_column_text(s, i));
    return p ? std::string(p, static_cast<std::size_t>(sqlite3_column_bytes(s, i))) : std::string();
}

} // namespace

Result<std::unique_ptr<SqliteAudit>> SqliteAudit::open(const std::filesystem::path& path) {
    sqlite3* db = nullptr;
    const int rc = sqlite3_open_v2(path.c_str(), &db, SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE | SQLITE_OPEN_FULLMUTEX, nullptr);
    if (rc != SQLITE_OK) {
        auto e = db ? db_error(db) : Error::audit(sqlite3_errstr(rc));
        sqlite3_close(db);
        return std::unexpected(std::move(e));
    }
    if (sqlite3_exec(db, std::string(AUDIT_SCHEMA).c_str(), nullptr, nullptr, nullptr) != SQLITE_OK) {
        auto e = db_error(db);
        sqlite3_close(db);
        return std::unexpected(std::move(e));
    }
    return std::unique_ptr<SqliteAudit>(new SqliteAudit(db));
}

SqliteAudit::~SqliteAudit() { sqlite3_close(db_); }

Result<std::int64_t> SqliteAudit::record(NewRecord r) {
    std::lock_guard lock(mu_);
    static constexpr const char* SQL =
        "INSERT INTO remote_audit (at_ms, host, session_id, action, target, outcome, detail) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)";
    Stmt st;
    if (sqlite3_prepare_v2(db_, SQL, -1, &st.s, nullptr) != SQLITE_OK) return std::unexpected(db_error(db_));
    const auto bind = [&](int i, std::string_view v) { sqlite3_bind_text(st.s, i, v.data(), static_cast<int>(v.size()), SQLITE_TRANSIENT); };
    sqlite3_bind_int64(st.s, 1, to_ms(r.at));
    bind(2, r.host);
    bind(3, r.session_id);
    bind(4, to_string(r.action));
    bind(5, r.target);
    bind(6, to_string(r.outcome));
    bind(7, r.detail);
    if (sqlite3_step(st.s) != SQLITE_DONE) return std::unexpected(db_error(db_));
    return sqlite3_last_insert_rowid(db_);
}

Result<std::vector<AuditRecord>> SqliteAudit::query(const AuditQuery& q) const {
    std::string sql = "SELECT id, at_ms, host, session_id, action, target, outcome, detail FROM remote_audit WHERE 1=1";
    std::vector<std::variant<std::string, std::int64_t>> args;
    const auto filter = [&](const char* clause, auto value) {
        sql += clause;
        args.emplace_back(std::move(value));
    };
    if (q.host) filter(" AND host = ?", *q.host);
    if (q.session_id) filter(" AND session_id = ?", *q.session_id);
    if (q.action) filter(" AND action = ?", std::string(to_string(*q.action)));
    if (q.outcome) filter(" AND outcome = ?", std::string(to_string(*q.outcome)));
    if (q.since) filter(" AND at_ms >= ?", to_ms(*q.since));
    if (q.until) filter(" AND at_ms < ?", to_ms(*q.until));
    if (q.before_id) filter(" AND id < ?", *q.before_id);
    sql += " ORDER BY id DESC LIMIT ?";
    const auto limit = q.effective_limit();
    args.emplace_back(limit > static_cast<std::size_t>(std::numeric_limits<std::int64_t>::max())
                          ? std::numeric_limits<std::int64_t>::max()
                          : static_cast<std::int64_t>(limit));

    std::lock_guard lock(mu_);
    Stmt st;
    if (sqlite3_prepare_v2(db_, sql.c_str(), -1, &st.s, nullptr) != SQLITE_OK) return std::unexpected(db_error(db_));
    for (std::size_t i = 0; i < args.size(); ++i) {
        const int index = static_cast<int>(i) + 1;
        if (const auto* s = std::get_if<std::string>(&args[i])) {
            sqlite3_bind_text(st.s, index, s->data(), static_cast<int>(s->size()), SQLITE_TRANSIENT);
        } else {
            sqlite3_bind_int64(st.s, index, std::get<std::int64_t>(args[i]));
        }
    }
    std::vector<AuditRecord> out;
    for (;;) {
        const int rc = sqlite3_step(st.s);
        if (rc == SQLITE_DONE) break;
        if (rc != SQLITE_ROW) return std::unexpected(db_error(db_));
        const auto action_text = text_column(st.s, 4);
        const auto outcome_text = text_column(st.s, 6);
        const auto action = parse_action(action_text);
        if (!action) {
            return std::unexpected(Error::audit("Conversion error from type Text at index: 4, unknown Action `" + action_text + "`"));
        }
        const auto outcome = parse_outcome(outcome_text);
        if (!outcome) {
            return std::unexpected(Error::audit("Conversion error from type Text at index: 6, unknown Outcome `" + outcome_text + "`"));
        }
        out.push_back(AuditRecord{sqlite3_column_int64(st.s, 0), from_ms(sqlite3_column_int64(st.s, 1)), text_column(st.s, 2),
                                  text_column(st.s, 3), *action, text_column(st.s, 5), *outcome, text_column(st.s, 7)});
    }
    return out;
}

} // namespace loa::remote
