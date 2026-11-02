CREATE TABLE IF NOT EXISTS content_bodies (
    content_id TEXT PRIMARY KEY REFERENCES contents(id) ON DELETE CASCADE,
    object_key TEXT NOT NULL UNIQUE,
    size_bytes INTEGER NOT NULL CHECK (size_bytes > 512000 AND size_bytes <= 10485760),
    sha256 TEXT NOT NULL CHECK (length(sha256) = 64)
);
