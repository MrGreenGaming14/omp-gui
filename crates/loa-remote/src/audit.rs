//! Audit log of every remote action (#101).
//!
//! One row per remote read, write, list, edit and command, including the ones
//! the limits refused or the user denied. The table belongs to this crate.

use loa_util::LockExt;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, params_from_iter, types::Value};
use serde::{Deserialize, Serialize};

use crate::error::Result;

/// What the agent (or, for [`Action::Terminal`], the user) asked a host to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Read,
    Write,
    List,
    Edit,
    Command,
    /// The user opened an interactive terminal to the host.
    Terminal,
}

impl Action {
    pub fn as_str(self) -> &'static str {
        match self {
            Action::Read => "read",
            Action::Write => "write",
            Action::List => "list",
            Action::Edit => "edit",
            Action::Command => "command",
            Action::Terminal => "terminal",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "read" => Action::Read,
            "write" => Action::Write,
            "list" => Action::List,
            "edit" => Action::Edit,
            "command" => Action::Command,
            "terminal" => Action::Terminal,
            _ => return None,
        })
    }

    /// Whether the action changes the host (writes, edits, commands, terminals).
    pub fn mutates(self) -> bool {
        matches!(self, Action::Write | Action::Edit | Action::Command | Action::Terminal)
    }
}

/// How an action ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// Ran. For commands this includes a non-zero exit status.
    Ok,
    /// Tried and failed (connection, auth, I/O).
    Error,
    /// Stopped by the host limits before anything was sent.
    Refused,
    /// The user denied the approval.
    Denied,
    Cancelled,
    TimedOut,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Ok => "ok",
            Outcome::Error => "error",
            Outcome::Refused => "refused",
            Outcome::Denied => "denied",
            Outcome::Cancelled => "cancelled",
            Outcome::TimedOut => "timed_out",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "ok" => Outcome::Ok,
            "error" => Outcome::Error,
            "refused" => Outcome::Refused,
            "denied" => Outcome::Denied,
            "cancelled" => Outcome::Cancelled,
            "timed_out" => Outcome::TimedOut,
            _ => return None,
        })
    }
}

/// A row to append.
#[derive(Debug, Clone, Copy)]
pub struct NewRecord<'a> {
    pub at: SystemTime,
    pub host: &'a str,
    pub session_id: &'a str,
    pub action: Action,
    pub target: &'a str,
    pub outcome: Outcome,
    /// Exit status, error text, refusal reason, byte counts.
    pub detail: &'a str,
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

/// Filter for [`AuditLog::query`]. Rows come newest first.
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
    /// At most this many rows; 0 means 200.
    pub limit: usize,
}

/// SQLite backed audit log.
pub struct AuditLog {
    conn: Mutex<Connection>,
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS remote_audit (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    at_ms INTEGER NOT NULL,
    host TEXT NOT NULL,
    session_id TEXT NOT NULL,
    action TEXT NOT NULL,
    target TEXT NOT NULL,
    outcome TEXT NOT NULL,
    detail TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS remote_audit_session ON remote_audit (session_id, id);
CREATE INDEX IF NOT EXISTS remote_audit_host ON remote_audit (host, id);
";

fn to_ms(t: SystemTime) -> i64 {
    t.duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

fn from_ms(ms: i64) -> SystemTime {
    UNIX_EPOCH + Duration::from_millis(u64::try_from(ms).unwrap_or(0))
}

impl AuditLog {
    /// Opens (and creates) the log in the database at `path`.
    pub fn open(path: &Path) -> Result<Self> {
        Self::init(Connection::open(path)?)
    }

    /// A log that lives only as long as this value.
    pub fn in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Appends a row and returns its id.
    pub fn record(&self, r: &NewRecord<'_>) -> Result<i64> {
        let conn = self.conn.locked();
        let mut stmt = conn.prepare_cached(
            "INSERT INTO remote_audit (at_ms, host, session_id, action, target, outcome, detail)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )?;
        stmt.execute((
            to_ms(r.at),
            r.host,
            r.session_id,
            r.action.as_str(),
            r.target,
            r.outcome.as_str(),
            r.detail,
        ))?;
        Ok(conn.last_insert_rowid())
    }

    /// Rows matching `q`, newest first.
    pub fn query(&self, q: &AuditQuery) -> Result<Vec<AuditRecord>> {
        let mut sql = String::from(
            "SELECT id, at_ms, host, session_id, action, target, outcome, detail FROM remote_audit WHERE 1=1",
        );
        let mut args: Vec<Value> = Vec::new();
        let mut filter = |clause: &str, v: Value| {
            sql.push_str(clause);
            args.push(v);
        };
        if let Some(h) = &q.host {
            filter(" AND host = ?", Value::Text(h.clone()));
        }
        if let Some(s) = &q.session_id {
            filter(" AND session_id = ?", Value::Text(s.clone()));
        }
        if let Some(a) = q.action {
            filter(" AND action = ?", Value::Text(a.as_str().into()));
        }
        if let Some(o) = q.outcome {
            filter(" AND outcome = ?", Value::Text(o.as_str().into()));
        }
        if let Some(t) = q.since {
            filter(" AND at_ms >= ?", Value::Integer(to_ms(t)));
        }
        if let Some(t) = q.until {
            filter(" AND at_ms < ?", Value::Integer(to_ms(t)));
        }
        if let Some(id) = q.before_id {
            filter(" AND id < ?", Value::Integer(id));
        }
        let limit = if q.limit == 0 { 200 } else { q.limit };
        sql.push_str(" ORDER BY id DESC LIMIT ?");
        args.push(Value::Integer(i64::try_from(limit).unwrap_or(i64::MAX)));

        let conn = self.conn.locked();
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(args), |row| {
            let action: String = row.get(4)?;
            let outcome: String = row.get(6)?;
            Ok(AuditRecord {
                id: row.get(0)?,
                at: from_ms(row.get(1)?),
                host: row.get(2)?,
                session_id: row.get(3)?,
                action: Action::parse(&action).ok_or_else(|| bad_column(4, &action))?,
                target: row.get(5)?,
                outcome: Outcome::parse(&outcome).ok_or_else(|| bad_column(6, &outcome))?,
                detail: row.get(7)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// The most recent row, if any.
    pub fn last(&self) -> Result<Option<AuditRecord>> {
        let max: Option<i64> = self
            .conn.locked()
            .query_row("SELECT MAX(id) FROM remote_audit", [], |r| r.get(0))
            .optional()?
            .flatten();
        match max {
            None => Ok(None),
            Some(id) => Ok(self
                .query(&AuditQuery {
                    before_id: Some(id + 1),
                    limit: 1,
                    ..AuditQuery::default()
                })?
                .pop()),
        }
    }
}

fn bad_column(index: usize, value: &str) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        index,
        rusqlite::types::Type::Text,
        format!("unknown value `{value}`").into(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_survive_reopen_and_filter_by_session_and_outcome() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.db");
        let t0 = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        {
            let log = AuditLog::open(&path).unwrap();
            for (i, (session, outcome)) in [
                ("s1", Outcome::Ok),
                ("s2", Outcome::Refused),
                ("s1", Outcome::Denied),
            ]
            .into_iter()
            .enumerate()
            {
                log.record(&NewRecord {
                    at: t0 + Duration::from_secs(i as u64),
                    host: "db",
                    session_id: session,
                    action: Action::Write,
                    target: "/etc/app.conf",
                    outcome,
                    detail: "",
                })
                .unwrap();
            }
        }
        let log = AuditLog::open(&path).unwrap();
        let s1 = log
            .query(&AuditQuery {
                session_id: Some("s1".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(
            s1.iter().map(|r| r.outcome).collect::<Vec<_>>(),
            vec![Outcome::Denied, Outcome::Ok]
        );
        assert_eq!(s1[1].at, t0);
        let refused = log
            .query(&AuditQuery {
                outcome: Some(Outcome::Refused),
                since: Some(t0 + Duration::from_secs(1)),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(refused.len(), 1);
        assert_eq!(refused[0].session_id, "s2");
        let page = log
            .query(&AuditQuery {
                before_id: Some(s1[0].id),
                limit: 1,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(page[0].session_id, "s2");
        assert_eq!(log.last().unwrap().unwrap().id, s1[0].id);
    }
}
