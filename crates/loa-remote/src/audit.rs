//! Audit log of every remote action, refused and denied ones included, behind the [`AuditSink`]
//! port: [`SqliteAudit`] for the app, [`MemoryAudit`] for tests.

use loa_util::LockExt;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, params_from_iter, types::Value};
use serde::Serialize;

use crate::error::Result;

loa_util::str_enum! {
    /// What the agent (or, for `Terminal`, the user) asked a host to do.
    pub Action { Read => "read", Write => "write", List => "list", Edit => "edit", Command => "command", Terminal => "terminal" }
}

impl Action {
    /// Whether the action changes the host (writes, edits, commands, terminals).
    pub fn mutates(self) -> bool {
        matches!(self, Action::Write | Action::Edit | Action::Command | Action::Terminal)
    }
}

loa_util::str_enum! {
    /// How an action ended; `Ok` includes a command's non-zero exit.
    pub Outcome { Ok => "ok", Error => "error", Refused => "refused", Denied => "denied", Cancelled => "cancelled", TimedOut => "timed_out" }
}

/// A row to append.
#[derive(Debug, Clone)]
pub struct NewRecord {
    pub at: SystemTime,
    pub host: String,
    pub session_id: String,
    pub action: Action,
    pub target: String,
    pub outcome: Outcome,
    /// Exit status, error text, refusal reason, byte counts.
    pub detail: String,
}

/// A stored row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AuditRecord {
    pub id: i64,
    pub at: SystemTime,
    pub host: String,
    pub session_id: String,
    pub action: Action,
    pub target: String,
    pub outcome: Outcome,
    pub detail: String,
}

/// Filter for [`AuditSink::query`]; rows come newest first, at most `limit` (0 means 200).
#[derive(Debug, Clone, Default)]
pub struct AuditQuery {
    pub host: Option<String>,
    pub session_id: Option<String>,
    pub action: Option<Action>,
    pub outcome: Option<Outcome>,
    pub since: Option<SystemTime>,
    pub until: Option<SystemTime>,
    /// Only rows with a smaller id (paging backwards).
    pub before_id: Option<i64>,
    pub limit: usize,
}

impl AuditQuery {
    fn limit(&self) -> usize {
        if self.limit == 0 { 200 } else { self.limit }
    }

    fn matches(&self, r: &AuditRecord) -> bool {
        self.host.as_ref().is_none_or(|h| *h == r.host)
            && self.session_id.as_ref().is_none_or(|s| *s == r.session_id)
            && self.action.is_none_or(|a| a == r.action)
            && self.outcome.is_none_or(|o| o == r.outcome)
            && self.since.is_none_or(|t| to_ms(r.at) >= to_ms(t))
            && self.until.is_none_or(|t| to_ms(r.at) < to_ms(t))
            && self.before_id.is_none_or(|id| r.id < id)
    }
}

/// Where audit rows go.
pub trait AuditSink: Send + Sync {
    /// Appends a row and returns its id.
    fn record(&self, r: NewRecord) -> Result<i64>;
    fn query(&self, q: &AuditQuery) -> Result<Vec<AuditRecord>>;
    fn last(&self) -> Result<Option<AuditRecord>> {
        Ok(self.query(&AuditQuery { limit: 1, ..AuditQuery::default() })?.pop())
    }
}

/// Rows kept in memory.
#[derive(Default)]
pub struct MemoryAudit(Mutex<Vec<AuditRecord>>);

impl AuditSink for MemoryAudit {
    fn record(&self, r: NewRecord) -> Result<i64> {
        let mut rows = self.0.locked();
        let id = rows.len() as i64 + 1;
        let NewRecord { at, host, session_id, action, target, outcome, detail } = r;
        rows.push(AuditRecord { id, at: from_ms(to_ms(at)), host, session_id, action, target, outcome, detail });
        Ok(id)
    }

    fn query(&self, q: &AuditQuery) -> Result<Vec<AuditRecord>> {
        Ok(self.0.locked().iter().rev().filter(|r| q.matches(r)).take(q.limit()).cloned().collect())
    }
}

/// Rows in the `remote_audit` table of an SQLite database.
pub struct SqliteAudit(Mutex<Connection>);

const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS remote_audit (
    id INTEGER PRIMARY KEY AUTOINCREMENT, at_ms INTEGER NOT NULL, host TEXT NOT NULL, session_id TEXT NOT NULL,
    action TEXT NOT NULL, target TEXT NOT NULL, outcome TEXT NOT NULL, detail TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS remote_audit_session ON remote_audit (session_id, id);
CREATE INDEX IF NOT EXISTS remote_audit_host ON remote_audit (host, id);";

fn to_ms(t: SystemTime) -> i64 {
    t.duration_since(UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

fn from_ms(ms: i64) -> SystemTime {
    UNIX_EPOCH + Duration::from_millis(u64::try_from(ms).unwrap_or(0))
}

impl SqliteAudit {
    /// Opens (and creates) the log in the database at `path`.
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self(Mutex::new(conn)))
    }
}

fn column<T: std::str::FromStr<Err = String>>(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<T> {
    let text: String = row.get(index)?;
    text.parse().map_err(|e: String| rusqlite::Error::FromSqlConversionFailure(index, rusqlite::types::Type::Text, e.into()))
}

impl AuditSink for SqliteAudit {
    fn record(&self, r: NewRecord) -> Result<i64> {
        let conn = self.0.locked();
        let sql = "INSERT INTO remote_audit (at_ms, host, session_id, action, target, outcome, detail) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)";
        conn.prepare_cached(sql)?.execute((to_ms(r.at), r.host, r.session_id, r.action.as_str(), r.target, r.outcome.as_str(), r.detail))?;
        Ok(conn.last_insert_rowid())
    }

    fn query(&self, q: &AuditQuery) -> Result<Vec<AuditRecord>> {
        let mut sql = String::from("SELECT id, at_ms, host, session_id, action, target, outcome, detail FROM remote_audit WHERE 1=1");
        let mut args: Vec<Value> = Vec::new();
        let filters = [
            (" AND host = ?", q.host.clone().map(Value::Text)),
            (" AND session_id = ?", q.session_id.clone().map(Value::Text)),
            (" AND action = ?", q.action.map(|a| Value::Text(a.as_str().into()))),
            (" AND outcome = ?", q.outcome.map(|o| Value::Text(o.as_str().into()))),
            (" AND at_ms >= ?", q.since.map(|t| Value::Integer(to_ms(t)))),
            (" AND at_ms < ?", q.until.map(|t| Value::Integer(to_ms(t)))),
            (" AND id < ?", q.before_id.map(Value::Integer)),
        ];
        for (clause, value) in filters.into_iter().filter_map(|(c, v)| Some((c, v?))) {
            sql.push_str(clause);
            args.push(value);
        }
        sql.push_str(" ORDER BY id DESC LIMIT ?");
        args.push(Value::Integer(i64::try_from(q.limit()).unwrap_or(i64::MAX)));
        let conn = self.0.locked();
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(args), |row| {
            Ok(AuditRecord {
                id: row.get(0)?,
                at: from_ms(row.get(1)?),
                host: row.get(2)?,
                session_id: row.get(3)?,
                action: column(row, 4)?,
                target: row.get(5)?,
                outcome: column(row, 6)?,
                detail: row.get(7)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_sinks_filter_the_same_and_sqlite_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.db");
        let t0 = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let memory = MemoryAudit::default();
        for (i, (session, outcome)) in [("s1", Outcome::Ok), ("s2", Outcome::Refused), ("s1", Outcome::Denied)].into_iter().enumerate() {
            let row = NewRecord {
                at: t0 + Duration::from_secs(i as u64),
                host: "db".into(),
                session_id: session.into(),
                action: Action::Write,
                target: "/etc/app.conf".into(),
                outcome,
                detail: String::new(),
            };
            SqliteAudit::open(&path).unwrap().record(row.clone()).unwrap();
            memory.record(row).unwrap();
        }
        let sqlite = SqliteAudit::open(&path).unwrap();
        for sink in [&sqlite as &dyn AuditSink, &memory] {
            let s1 = sink.query(&AuditQuery { session_id: Some("s1".into()), ..Default::default() }).unwrap();
            assert_eq!(s1.iter().map(|r| (r.outcome, r.at)).collect::<Vec<_>>(), [(Outcome::Denied, t0 + Duration::from_secs(2)), (Outcome::Ok, t0)]);
            let refused = sink.query(&AuditQuery { outcome: Some(Outcome::Refused), since: Some(t0 + Duration::from_secs(1)), ..Default::default() });
            assert_eq!(refused.unwrap()[0].session_id, "s2");
            let page = sink.query(&AuditQuery { before_id: Some(s1[0].id), limit: 1, ..Default::default() }).unwrap();
            assert_eq!(page[0].session_id, "s2");
            assert_eq!(sink.last().unwrap().unwrap().id, s1[0].id);
        }
    }
}
