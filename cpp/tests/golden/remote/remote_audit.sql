CREATE TABLE IF NOT EXISTS remote_audit (
    id INTEGER PRIMARY KEY AUTOINCREMENT, at_ms INTEGER NOT NULL, host TEXT NOT NULL, session_id TEXT NOT NULL,
    action TEXT NOT NULL, target TEXT NOT NULL, outcome TEXT NOT NULL, detail TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS remote_audit_session ON remote_audit (session_id, id);
CREATE INDEX IF NOT EXISTS remote_audit_host ON remote_audit (host, id);