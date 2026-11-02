-- FlashPaste 数据库初始化 Schema

-- 1. Token 表
CREATE TABLE IF NOT EXISTS tokens (
    id TEXT PRIMARY KEY,                    -- 128-bit 随机（32 hex）
    token_hash TEXT NOT NULL UNIQUE,        -- SHA-256(token_plaintext)
    name TEXT NOT NULL UNIQUE,              -- 显示名称
    level TEXT NOT NULL CHECK (level IN ('super', 'user')),
    created_by TEXT REFERENCES tokens(id) ON DELETE SET NULL,
    created_at INTEGER NOT NULL             -- Unix 秒
);

-- 2. 内容表
CREATE TABLE IF NOT EXISTS contents (
    id TEXT PRIMARY KEY,                    -- 128-bit 随机（32 hex）
    title TEXT,                             -- 显示名称
    content_type TEXT NOT NULL CHECK (content_type IN ('text', 'markdown', 'html')),
    content TEXT NOT NULL,                  -- 正文内容
    visibility TEXT NOT NULL CHECK (visibility IN ('public', 'token')),
    created_by TEXT REFERENCES tokens(id) ON DELETE SET NULL,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    max_expires_at INTEGER NOT NULL,
    burn_after_read INTEGER NOT NULL CHECK (burn_after_read IN (0, 1)),
    
    CHECK (burn_after_read = 0 OR visibility = 'token'),
    CHECK (expires_at > created_at),
    CHECK (max_expires_at >= expires_at)
);

-- 3. 访问控制表
CREATE TABLE IF NOT EXISTS access_control (
    content_id TEXT NOT NULL REFERENCES contents(id) ON DELETE CASCADE,
    token_id TEXT NOT NULL REFERENCES tokens(id) ON DELETE CASCADE,
    granted_at INTEGER NOT NULL,
    granted_by TEXT REFERENCES tokens(id) ON DELETE SET NULL,
    PRIMARY KEY (content_id, token_id)
);

-- 索引
CREATE INDEX IF NOT EXISTS idx_contents_expires_at ON contents(expires_at);
CREATE INDEX IF NOT EXISTS idx_contents_visibility ON contents(visibility);
CREATE INDEX IF NOT EXISTS idx_contents_created_by ON contents(created_by);
CREATE INDEX IF NOT EXISTS idx_tokens_level ON tokens(level);
CREATE INDEX IF NOT EXISTS idx_tokens_created_by ON tokens(created_by);
CREATE INDEX IF NOT EXISTS idx_access_control_token_id ON access_control(token_id);
