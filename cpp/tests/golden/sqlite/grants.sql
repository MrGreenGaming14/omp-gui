
CREATE TABLE IF NOT EXISTS grants_issued (id TEXT PRIMARY KEY, holder TEXT NOT NULL, spec TEXT NOT NULL, parent TEXT,
    created_ms INTEGER NOT NULL, revoked_ms INTEGER);
CREATE TABLE IF NOT EXISTS grants_received (id TEXT PRIMARY KEY, issuer TEXT NOT NULL, root TEXT NOT NULL, token BLOB NOT NULL,
    expires_ms INTEGER NOT NULL, reshare INTEGER NOT NULL, refused_ms INTEGER);
CREATE TABLE IF NOT EXISTS grants_revocations (id TEXT PRIMARY KEY, issuer TEXT NOT NULL, revoked_ms INTEGER NOT NULL);
