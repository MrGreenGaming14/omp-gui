#pragma once

// Audit log of every remote action, refused and denied ones included, behind the `AuditSink`
// port: `SqliteAudit` (table `remote_audit`) for the app, `MemoryAudit` for tests.

#include <array>
#include <chrono>
#include <cstddef>
#include <cstdint>
#include <filesystem>
#include <memory>
#include <mutex>
#include <optional>
#include <string>
#include <string_view>
#include <vector>

#include "loa/remote/error.hpp"

struct sqlite3;

namespace loa::remote {

using Time = std::chrono::system_clock::time_point;

// What the agent (or, for `terminal`, the user) asked a host to do.
enum class Action { read, write, list, edit, command, terminal };
inline constexpr std::array<Action, 6> ALL_ACTIONS{Action::read, Action::write, Action::list,
                                                   Action::edit, Action::command, Action::terminal};
[[nodiscard]] std::string_view to_string(Action action) noexcept;
[[nodiscard]] std::optional<Action> parse_action(std::string_view text) noexcept;
// Whether the action changes the host (writes, edits, commands, terminals).
[[nodiscard]] bool mutates(Action action) noexcept;

// How an action ended; `ok` includes a command's non-zero exit.
enum class Outcome { ok, error, refused, denied, cancelled, timed_out };
[[nodiscard]] std::string_view to_string(Outcome outcome) noexcept;
[[nodiscard]] std::optional<Outcome> parse_outcome(std::string_view text) noexcept;

// A row to append.
struct NewRecord {
    Time at;
    std::string host;
    std::string session_id;
    Action action = Action::read;
    std::string target;
    Outcome outcome = Outcome::ok;
    // Exit status, error text, refusal reason, byte counts.
    std::string detail;
};

// A stored row; `at` has millisecond precision.
struct AuditRecord {
    std::int64_t id = 0;
    Time at;
    std::string host;
    std::string session_id;
    Action action = Action::read;
    std::string target;
    Outcome outcome = Outcome::ok;
    std::string detail;
    friend bool operator==(const AuditRecord&, const AuditRecord&) = default;
};

// Filter for `AuditSink::query`; rows come newest first, at most `limit` (0 means 200).
struct AuditQuery {
    std::optional<std::string> host;
    std::optional<std::string> session_id;
    std::optional<Action> action;
    std::optional<Outcome> outcome;
    std::optional<Time> since;
    std::optional<Time> until;
    // Only rows with a smaller id (paging backwards).
    std::optional<std::int64_t> before_id;
    std::size_t limit = 0;

    [[nodiscard]] std::size_t effective_limit() const noexcept { return limit == 0 ? 200 : limit; }
    [[nodiscard]] bool matches(const AuditRecord& r) const;
};

// Milliseconds since the Unix epoch (0 before it), as the `at_ms` column stores them.
[[nodiscard]] std::int64_t to_ms(Time t) noexcept;
[[nodiscard]] Time from_ms(std::int64_t ms) noexcept;

// Where audit rows go. Thread-safe.
class AuditSink {
  public:
    virtual ~AuditSink() = default;
    // Appends a row and returns its id.
    [[nodiscard]] virtual Result<std::int64_t> record(NewRecord r) = 0;
    [[nodiscard]] virtual Result<std::vector<AuditRecord>> query(const AuditQuery& q) const = 0;
    [[nodiscard]] Result<std::optional<AuditRecord>> last() const;
};

// Rows kept in memory.
class MemoryAudit final : public AuditSink {
  public:
    [[nodiscard]] Result<std::int64_t> record(NewRecord r) override;
    [[nodiscard]] Result<std::vector<AuditRecord>> query(const AuditQuery& q) const override;

  private:
    mutable std::mutex mu_;
    std::vector<AuditRecord> rows_;
};

// The `CREATE TABLE remote_audit` and index statements, byte for byte the Rust `SCHEMA`.
extern const std::string_view AUDIT_SCHEMA;

// Rows in the `remote_audit` table of an SQLite database.
class SqliteAudit final : public AuditSink {
  public:
    // Opens (and creates) the log in the database at `path`.
    [[nodiscard]] static Result<std::unique_ptr<SqliteAudit>> open(const std::filesystem::path& path);
    ~SqliteAudit() override;
    SqliteAudit(const SqliteAudit&) = delete;
    SqliteAudit& operator=(const SqliteAudit&) = delete;

    [[nodiscard]] Result<std::int64_t> record(NewRecord r) override;
    [[nodiscard]] Result<std::vector<AuditRecord>> query(const AuditQuery& q) const override;

  private:
    explicit SqliteAudit(sqlite3* db) : db_(db) {}
    mutable std::mutex mu_;
    sqlite3* db_;
};

} // namespace loa::remote
