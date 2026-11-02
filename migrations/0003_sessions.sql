CREATE TABLE IF NOT EXISTS sessions (
    session_hash TEXT PRIMARY KEY,
    token_id TEXT NOT NULL REFERENCES tokens(id) ON DELETE CASCADE,
    token_version INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL CHECK (expires_at > created_at)
);
CREATE INDEX IF NOT EXISTS idx_sessions_token ON sessions(token_id);
CREATE INDEX IF NOT EXISTS idx_sessions_expires ON sessions(expires_at);
