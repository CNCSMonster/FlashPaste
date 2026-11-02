use rand::Rng;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

pub struct Database {
    conn: Arc<Mutex<Connection>>,
    body_dir: std::path::PathBuf,
}

impl Database {
    pub fn new(path: &str) -> Result<Self, rusqlite::Error> {
        let existed = std::path::Path::new(path).is_file();
        let conn = Connection::open(path)?;

        let columns = {
            let mut stmt = conn.prepare("PRAGMA table_info(tokens)")?;
            let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        let has_bodies: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='content_bodies')",[],|row|row.get(0))?;
        if existed && (!columns.iter().any(|column| column == "auth_version") || !has_bodies) {
            // SQLite's backup API includes committed WAL data; copying the file does not.
            let stage = if columns.iter().any(|column| column == "auth_version") {
                "large-html"
            } else {
                "sessions"
            };
            let backup = format!("{path}.pre-{stage}-{}.sqlite3", generate_id());
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            options
                .open(&backup)
                .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
            conn.backup(rusqlite::DatabaseName::Main, &backup, None)?;
            eprintln!("FlashPaste pre-session migration backup: {backup}");
        }

        // 启用外键约束
        conn.execute("PRAGMA foreign_keys = ON", [])?;

        // 运行迁移
        let schema = include_str!("../migrations/0001_init.sql");
        conn.execute_batch(schema)?;
        if !columns.iter().any(|column| column == "auth_version") {
            conn.execute_batch(include_str!("../migrations/0002_token_auth_version.sql"))?;
        }
        conn.execute_batch(include_str!("../migrations/0003_sessions.sql"))?;
        conn.execute_batch(include_str!("../migrations/0004_content_bodies.sql"))?;
        let has_quota_column = {
            let mut stmt = conn.prepare("PRAGMA table_info(tokens)")?;
            let names = stmt.query_map([], |row| row.get::<_, String>(1))?;
            names
                .collect::<Result<Vec<_>, _>>()?
                .iter()
                .any(|name| name == "quota_bytes")
        };
        if !has_quota_column {
            conn.execute_batch(include_str!("../migrations/0005_token_quota.sql"))?;
        }
        // Older local databases could contain the retired admin role. It never
        // had distinct permissions; normalize it to the regular user role.
        conn.execute("UPDATE tokens SET level = 'user' WHERE level = 'admin'", [])?;
        let has_legacy_plaintext_column = {
            let mut stmt = conn.prepare("PRAGMA table_info(tokens)")?;
            let columns = stmt.query_map([], |row| row.get::<_, String>(1))?;
            columns
                .collect::<Result<Vec<_>, _>>()?
                .iter()
                .any(|name| name == "token_value")
        };
        if has_legacy_plaintext_column {
            conn.execute_batch(include_str!("../ops/drop_legacy_plaintext_token.sql"))?;
        }

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            body_dir: if path == ":memory:" {
                std::path::PathBuf::from(format!(
                    "/tmp/opencode/flashpaste-bodies-{}",
                    generate_id()
                ))
            } else {
                std::path::PathBuf::from(format!("{path}.bodies"))
            },
        })
    }

    pub fn get_token_by_hash(&self, token_hash: &str) -> Result<Option<Token>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, name, level, created_at, quota_bytes FROM tokens WHERE token_hash = ?",
        )?;

        let result = stmt.query_row(params![token_hash], |row| {
            Ok(Token {
                id: row.get(0)?,
                name: row.get(1)?,
                level: row.get(2)?,
                created_at: row.get(3)?,
                quota_bytes: row.get(4)?,
            })
        });

        match result {
            Ok(token) => Ok(Some(token)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub fn create_token(
        &self,
        id: &str,
        token_hash: &str,
        name: &str,
        level: &str,
        created_by: Option<&str>,
        created_at: i64,
    ) -> Result<(), rusqlite::Error> {
        self.create_token_with_quota(id, token_hash, name, level, created_by, created_at, None)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create_token_with_quota(
        &self,
        id: &str,
        token_hash: &str,
        name: &str,
        level: &str,
        created_by: Option<&str>,
        created_at: i64,
        quota_bytes: Option<i64>,
    ) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO tokens (id, token_hash, name, level, created_by, created_at, quota_bytes)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                id,
                token_hash,
                name,
                level,
                created_by,
                created_at,
                quota_bytes
            ],
        )?;
        Ok(())
    }

    /// Bytes occupied by a token's still-active content, including KV-backed bodies.
    pub fn active_usage_bytes(&self, token_id: &str, now: i64) -> Result<i64, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT COALESCE(SUM(CASE WHEN b.size_bytes IS NOT NULL THEN b.size_bytes
                                      ELSE length(CAST(c.content AS BLOB)) END), 0)
             FROM contents c LEFT JOIN content_bodies b ON b.content_id = c.id
             WHERE c.created_by = ? AND c.expires_at > ?",
            params![token_id, now],
            |row| row.get(0),
        )
    }

    pub fn get_token(&self, id: &str) -> Result<Option<Token>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT id, name, level, created_at, quota_bytes FROM tokens WHERE id = ?")?;

        let result = stmt.query_row(params![id], |row| {
            Ok(Token {
                id: row.get(0)?,
                name: row.get(1)?,
                level: row.get(2)?,
                created_at: row.get(3)?,
                quota_bytes: row.get(4)?,
            })
        });

        match result {
            Ok(token) => Ok(Some(token)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub fn get_token_by_name(&self, name: &str) -> Result<Option<Token>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let mut stmt =
            conn.prepare("SELECT id,name,level,created_at,quota_bytes FROM tokens WHERE name=?")?;
        match stmt.query_row(params![name], |row| {
            Ok(Token {
                id: row.get(0)?,
                name: row.get(1)?,
                level: row.get(2)?,
                created_at: row.get(3)?,
                quota_bytes: row.get(4)?,
            })
        }) {
            Ok(token) => Ok(Some(token)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(error) => Err(error),
        }
    }

    pub fn rotate_token_hash(&self, id: &str, hash: &str) -> Result<bool, rusqlite::Error> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let updated = tx.execute(
            "UPDATE tokens SET token_hash=?,auth_version=auth_version+1 WHERE id=?",
            params![hash, id],
        )? == 1;
        tx.execute("DELETE FROM sessions WHERE token_id=?", params![id])?;
        tx.commit()?;
        Ok(updated)
    }

    pub fn create_session(
        &self,
        token_hash: &str,
        session_hash: &str,
        previous: Option<&str>,
        now: i64,
        ttl: i64,
    ) -> Result<Option<(Token, i64)>, rusqlite::Error> {
        let mut conn = self.conn.lock().unwrap();
        // A writer lock covers hash verification, version capture and insertion.
        // Never re-read a newer version after validating an old credential.
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let snapshot = tx.query_row(
            "SELECT id,name,level,created_at,quota_bytes,auth_version FROM tokens WHERE token_hash=?",
            params![token_hash],
            |row| {
                Ok((
                    Token {
                        id: row.get(0)?,
                        name: row.get(1)?,
                        level: row.get(2)?,
                        created_at: row.get(3)?,
                        quota_bytes: row.get(4)?,
                    },
                    row.get::<_, i64>(5)?,
                ))
            },
        );
        let (token, version) = match snapshot {
            Ok(snapshot) => snapshot,
            Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
            Err(error) => return Err(error),
        };
        tx.execute("INSERT INTO sessions(session_hash,token_id,token_version,created_at,expires_at) VALUES(?,?,?,?,?)", params![session_hash, token.id, version, now, now+ttl])?;
        if let Some(previous) = previous {
            tx.execute(
                "DELETE FROM sessions WHERE session_hash=?",
                params![previous],
            )?;
        }
        tx.commit()?;
        Ok(Some((token, now + ttl)))
    }

    pub fn get_session(
        &self,
        hash: &str,
        now: i64,
    ) -> Result<Option<(Token, i64)>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let result = conn.query_row(
            "SELECT t.id,t.name,t.level,t.created_at,t.quota_bytes,s.expires_at FROM sessions s JOIN tokens t ON t.id=s.token_id WHERE s.session_hash=? AND s.expires_at>? AND s.token_version=t.auth_version",
            params![hash, now],
            |row| Ok((Token { id:row.get(0)?, name:row.get(1)?, level:row.get(2)?, created_at:row.get(3)?, quota_bytes:row.get(4)? }, row.get(5)?)),
        );
        match result {
            Ok(value) => Ok(Some(value)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(error) => Err(error),
        }
    }

    pub fn delete_session(&self, hash: &str) -> Result<(), rusqlite::Error> {
        self.conn
            .lock()
            .unwrap()
            .execute("DELETE FROM sessions WHERE session_hash=?", params![hash])?;
        Ok(())
    }

    pub fn list_tokens(&self) -> Result<Vec<Token>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, name, level, created_at, quota_bytes FROM tokens ORDER BY created_at DESC",
        )?;

        let tokens = stmt
            .query_map([], |row| {
                Ok(Token {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    level: row.get(2)?,
                    created_at: row.get(3)?,
                    quota_bytes: row.get(4)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(tokens)
    }

    pub fn count_tokens_by_level(&self, level: &str) -> Result<i64, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT COUNT(*) FROM tokens WHERE level = ?")?;

        let count: i64 = stmt.query_row(params![level], |row| row.get(0))?;
        Ok(count)
    }

    pub fn delete_token_guarded(&self, id: &str) -> Result<Option<bool>, rusqlite::Error> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let level: Option<String> = tx
            .query_row("SELECT level FROM tokens WHERE id=?", params![id], |r| {
                r.get(0)
            })
            .ok();
        let level = match level {
            Some(level) => level,
            None => return Ok(None),
        };
        if level == "super" {
            let count: i64 =
                tx.query_row("SELECT COUNT(*) FROM tokens WHERE level='super'", [], |r| {
                    r.get(0)
                })?;
            if count <= 1 {
                return Ok(Some(false));
            }
        }
        tx.execute("DELETE FROM tokens WHERE id=?", params![id])?;
        tx.commit()?;
        Ok(Some(true))
    }

    // Mirrors persisted content columns; callers pass this only at the DB boundary.
    #[allow(clippy::too_many_arguments)]
    pub fn create_content(
        &self,
        id: &str,
        title: Option<&str>,
        content_type: &str,
        content: &str,
        visibility: &str,
        created_by: Option<&str>,
        created_at: i64,
        expires_at: i64,
        max_expires_at: i64,
        burn_after_read: i32,
        allowed_tokens: Option<&[String]>,
        granted_by: Option<&str>,
    ) -> Result<(), rusqlite::Error> {
        if content.len() > crate::body_store::limit(content_type) {
            return Err(rusqlite::Error::InvalidParameterName(
                "Content too large".into(),
            ));
        }
        if let Some(token_ids) = allowed_tokens {
            let conn = self.conn.lock().unwrap();
            let mut seen = std::collections::HashSet::new();
            for token_id in token_ids {
                if !seen.insert(token_id) {
                    return Err(rusqlite::Error::InvalidParameterName(
                        "Duplicate ACL token".into(),
                    ));
                }
                let exists: bool = conn.query_row(
                    "SELECT EXISTS(SELECT 1 FROM tokens WHERE id=?)",
                    params![token_id],
                    |row| row.get(0),
                )?;
                if !exists {
                    return Err(rusqlite::Error::InvalidParameterName(
                        "Unknown ACL token".into(),
                    ));
                }
            }
        }
        let external = content.len() > crate::body_store::INLINE_LIMIT;
        let key = external.then(|| format!("bodies/{}", generate_id()));
        if let Some(key) = &key {
            self.write_body(key, content.as_bytes())?;
        }
        let result = (|| {
            let mut conn = self.conn.lock().unwrap();
            let tx = conn.transaction()?;
            tx.execute(
            "INSERT INTO contents (id, title, content_type, content, visibility, created_by, created_at, expires_at, max_expires_at, burn_after_read)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                id,
                title,
                content_type,
                if external { "" } else { content },
                visibility,
                created_by,
                created_at,
                expires_at,
                max_expires_at,
                burn_after_read,
            ],
        )?;
            if let Some(key) = &key {
                tx.execute("INSERT INTO content_bodies(content_id,object_key,size_bytes,sha256) VALUES(?,?,?,?)",params![id,key,content.len() as i64,crate::body_store::digest(content.as_bytes())])?;
            }
            if let Some(token_ids) = allowed_tokens {
                for token_id in token_ids {
                    tx.execute(
                    "INSERT INTO access_control (content_id,token_id,granted_at,granted_by) VALUES (?,?,?,?)",
                    params![id, token_id, created_at, granted_by],
                )?;
                }
            }

            tx.commit()
        })();
        if result.is_err() {
            if let Some(key) = &key {
                self.remove_body(key);
            }
        }
        result
    }

    fn storage_error(error: impl std::error::Error + Send + Sync + 'static) -> rusqlite::Error {
        rusqlite::Error::ToSqlConversionFailure(Box::new(error))
    }

    fn write_body(&self, key: &str, bytes: &[u8]) -> Result<(), rusqlite::Error> {
        use std::io::Write;
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        match builder.create(&self.body_dir) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(Self::storage_error(e)),
        }
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = self.body_dir.join(key.strip_prefix("bodies/").unwrap());
        let result = (|| {
            let mut file = options.open(&file)?;
            file.write_all(bytes)?;
            file.sync_all()
        })();
        if result.is_err() {
            self.remove_body(key);
        }
        result.map_err(Self::storage_error)
    }

    fn remove_body(&self, key: &str) {
        if !crate::body_store::valid_key(key) {
            return;
        }
        if let Err(error) = std::fs::remove_file(self.body_dir.join(&key[7..])) {
            if error.kind() != std::io::ErrorKind::NotFound {
                eprintln!("FlashPaste body cleanup deferred");
            }
        }
    }

    fn hydrate_from(
        conn: &Connection,
        dir: &std::path::Path,
        content: &mut Content,
    ) -> Result<(), rusqlite::Error> {
        use rusqlite::OptionalExtension;
        let row: Option<(Option<String>, Option<usize>, Option<String>)> = conn
            .query_row(
                "SELECT b.object_key,b.size_bytes,b.sha256 FROM contents c LEFT JOIN content_bodies b ON b.content_id=c.id WHERE c.id=?",
                params![content.id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let Some((key, size, hash)) = row else {
            return Err(rusqlite::Error::QueryReturnedNoRows);
        };
        if let Some(key) = key {
            let size = size.ok_or(rusqlite::Error::InvalidQuery)?;
            let hash = hash.ok_or(rusqlite::Error::InvalidQuery)?;
            if !crate::body_store::valid_key(&key) {
                return Err(rusqlite::Error::InvalidQuery);
            }
            let file = std::fs::File::open(dir.join(&key[7..])).map_err(Self::storage_error)?;
            use std::io::Read;
            let mut bytes = Vec::with_capacity(size.min(crate::body_store::HTML_LIMIT));
            file.take((crate::body_store::HTML_LIMIT + 1) as u64)
                .read_to_end(&mut bytes)
                .map_err(Self::storage_error)?;
            content.content = crate::body_store::verify(bytes, size, &hash)
                .map_err(|_| rusqlite::Error::InvalidQuery)?;
        }
        Ok(())
    }

    pub fn hydrate_content(&self, content: &mut Content) -> Result<(), rusqlite::Error> {
        Self::hydrate_from(&self.conn.lock().unwrap(), &self.body_dir, content)
    }

    pub fn has_external_body(&self, id: &str) -> Result<bool, rusqlite::Error> {
        self.conn.lock().unwrap().query_row(
            "SELECT EXISTS(SELECT 1 FROM content_bodies WHERE content_id=?)",
            params![id],
            |row| row.get(0),
        )
    }

    pub fn patch_content(
        &self,
        id: &str,
        visibility: &str,
        expires_at: i64,
        allowed_tokens: Option<&[String]>,
        granted_by: &str,
        now: i64,
    ) -> Result<(), rusqlite::Error> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        tx.execute(
            "UPDATE contents SET visibility=?, expires_at=? WHERE id=?",
            params![visibility, expires_at, id],
        )?;
        if visibility == "public" || allowed_tokens.is_some() {
            tx.execute("DELETE FROM access_control WHERE content_id=?", params![id])?;
        }
        if visibility == "token" {
            if let Some(token_ids) = allowed_tokens {
                for token_id in token_ids {
                    tx.execute(
                        "INSERT INTO access_control (content_id,token_id,granted_at,granted_by) VALUES (?,?,?,?)",
                        params![id, token_id, now, granted_by],
                    )?;
                }
            }
        }
        tx.commit()
    }

    pub fn get_content(&self, id: &str) -> Result<Option<Content>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, title, content_type, content, visibility, created_by, created_at, expires_at, burn_after_read
             FROM contents WHERE id = ?"
        )?;

        let result = stmt.query_row(params![id], |row| {
            Ok(Content {
                id: row.get(0)?,
                title: row.get(1)?,
                content_type: row.get(2)?,
                content: row.get(3)?,
                visibility: row.get(4)?,
                created_by: row.get(5)?,
                created_at: row.get(6)?,
                expires_at: row.get(7)?,
                burn_after_read: row.get(8)?,
            })
        });

        match result {
            Ok(content) => Ok(Some(content)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub fn consume_authorized_content(
        &self,
        id: &str,
        viewer_id: &str,
        now: i64,
    ) -> Result<Option<Content>, rusqlite::Error> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let content = tx.query_row(
            "SELECT id,title,content_type,content,visibility,created_by,created_at,expires_at,burn_after_read FROM contents WHERE id=? AND expires_at>? AND burn_after_read=1 AND visibility='token'",
            params![id, now],
            |row| Ok(Content {
                id: row.get(0)?, title: row.get(1)?, content_type: row.get(2)?, content: row.get(3)?,
                visibility: row.get(4)?, created_by: row.get(5)?, created_at: row.get(6)?,
                expires_at: row.get(7)?, burn_after_read: row.get(8)?,
            }),
        );
        let mut content = match content {
            Ok(value) => value,
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                tx.commit()?;
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        let is_super: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM tokens WHERE id=? AND level='super')",
            params![viewer_id],
            |row| row.get(0),
        )?;
        let is_owner = content.created_by.as_deref() == Some(viewer_id);
        let is_granted: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM access_control WHERE content_id=? AND token_id=?)",
            params![id, viewer_id],
            |row| row.get(0),
        )?;
        if !is_super && !is_owner && !is_granted {
            tx.commit()?;
            return Ok(None);
        }
        // Storage failure occurs BEFORE the atomic consumption, never burns data.
        Self::hydrate_from(&tx, &self.body_dir, &mut content)?;
        let key: Option<String> = tx
            .query_row(
                "SELECT object_key FROM content_bodies WHERE content_id=?",
                params![id],
                |row| row.get(0),
            )
            .ok();
        let deleted = tx.execute(
            "DELETE FROM contents WHERE id=? AND burn_after_read=1",
            params![id],
        )?;
        tx.commit()?;
        if deleted == 1 {
            if let Some(key) = key {
                self.remove_body(&key);
            }
        }
        Ok((deleted == 1).then_some(content))
    }

    pub fn has_access_control(
        &self,
        content_id: &str,
        token_id: &str,
    ) -> Result<bool, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT COUNT(*) FROM access_control WHERE content_id = ? AND token_id = ?")?;

        let count: i64 = stmt.query_row(params![content_id, token_id], |row| row.get(0))?;
        Ok(count > 0)
    }

    pub fn list_access_tokens(&self, content_id: &str) -> Result<Vec<String>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT token_id FROM access_control WHERE content_id=? ORDER BY token_id")?;
        let rows = stmt.query_map(params![content_id], |row| row.get(0))?;
        rows.collect()
    }

    pub fn delete_managed_content(&self, id: &str) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        use rusqlite::OptionalExtension;
        let key: Option<String> = conn
            .query_row(
                "SELECT object_key FROM content_bodies WHERE content_id=?",
                params![id],
                |row| row.get(0),
            )
            .optional()?;
        conn.execute("DELETE FROM contents WHERE id=?", params![id])?;
        if let Some(key) = key {
            self.remove_body(&key);
        }
        Ok(())
    }

    pub fn cleanup_expired(&self, now: i64) -> Result<usize, rusqlite::Error> {
        let count = {
            let conn = self.conn.lock().unwrap();
            conn.execute("DELETE FROM sessions WHERE expires_at<=?", params![now])?;
            conn.execute("DELETE FROM contents WHERE expires_at<=?", params![now])?
        };
        self.collect_orphans(now)?;
        Ok(count)
    }

    pub fn collect_orphans(&self, now: i64) -> Result<(), rusqlite::Error> {
        let entries = match std::fs::read_dir(&self.body_dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(Self::storage_error(e)),
        };
        for entry in entries {
            let entry = entry.map_err(Self::storage_error)?;
            let key = format!("bodies/{}", entry.file_name().to_string_lossy());
            if !crate::body_store::valid_key(&key) {
                continue;
            }
            let modified = entry
                .metadata()
                .and_then(|m| m.modified())
                .map_err(Self::storage_error)?
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(Self::storage_error)?
                .as_secs();
            if (now.max(0) as u64).saturating_sub(modified)
                < crate::body_store::ORPHAN_GRACE_SECONDS
            {
                continue;
            }
            let referenced: bool = self.conn.lock().unwrap().query_row(
                "SELECT EXISTS(SELECT 1 FROM content_bodies WHERE object_key=?)",
                params![key],
                |r| r.get(0),
            )?;
            if !referenced {
                self.remove_body(&key);
            }
        }
        Ok(())
    }

    pub fn list_dashboard_contents(
        &self,
        token_id: Option<&str>,
    ) -> Result<Vec<Content>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let now = chrono::Utc::now().timestamp();

        let query = if let Some(tid) = token_id {
            // 检查是否是 super
            let is_super: bool = {
                let mut stmt = conn.prepare("SELECT level FROM tokens WHERE id = ?")?;
                let level: Option<String> = stmt.query_row(params![tid], |row| row.get(0)).ok();
                level.as_deref() == Some("super")
            };

            if is_super {
                // super 看所有
                let mut stmt = conn.prepare(
                    "SELECT id, title, content_type, '' AS content, visibility, created_by, created_at, expires_at, burn_after_read
                     FROM contents WHERE expires_at > ? ORDER BY created_at DESC"
                )?;
                let rows = stmt.query_map(params![now], |row| {
                    Ok(Content {
                        id: row.get(0)?,
                        title: row.get(1)?,
                        content_type: row.get(2)?,
                        content: row.get(3)?,
                        visibility: row.get(4)?,
                        created_by: row.get(5)?,
                        created_at: row.get(6)?,
                        expires_at: row.get(7)?,
                        burn_after_read: row.get(8)?,
                    })
                })?;
                let mut results = Vec::new();
                for row in rows {
                    results.push(row?);
                }
                results
            } else {
                // normal 看自己创建 + 被授权的
                let mut stmt = conn.prepare(
                    "SELECT DISTINCT c.id, c.title, c.content_type, '' AS content, c.visibility, c.created_by, c.created_at, c.expires_at, c.burn_after_read
                     FROM contents c
                     LEFT JOIN access_control ac ON c.id = ac.content_id
                     WHERE c.expires_at > ? AND (c.created_by = ? OR ac.token_id = ?)
                     ORDER BY c.created_at DESC"
                )?;
                let rows = stmt.query_map(params![now, tid, tid], |row| {
                    Ok(Content {
                        id: row.get(0)?,
                        title: row.get(1)?,
                        content_type: row.get(2)?,
                        content: row.get(3)?,
                        visibility: row.get(4)?,
                        created_by: row.get(5)?,
                        created_at: row.get(6)?,
                        expires_at: row.get(7)?,
                        burn_after_read: row.get(8)?,
                    })
                })?;
                let mut results = Vec::new();
                for row in rows {
                    results.push(row?);
                }
                results
            }
        } else {
            // Anonymous listing is intentionally forbidden. The HTTP layer rejects this case.
            return Ok(Vec::new());
        };

        Ok(query)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Token {
    pub id: String,
    pub name: String,
    pub level: String,
    pub created_at: i64,
    pub quota_bytes: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Content {
    pub id: String,
    pub title: Option<String>,
    pub content_type: String,
    pub content: String,
    pub visibility: String,
    pub created_by: Option<String>,
    pub created_at: i64,
    pub expires_at: i64,
    pub burn_after_read: i32,
}

pub fn generate_id() -> String {
    let mut rng = rand::thread_rng();
    let bytes: [u8; 16] = rng.gen();
    hex::encode(bytes)
}

pub fn generate_token_id() -> String {
    format!("tok_{}", generate_id())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_bodies_fail_closed_preserve_burn_and_roll_back_uploads() {
        let db = Database::new(":memory:").unwrap();
        db.create_token("owner", "hash", "owner", "user", None, 1)
            .unwrap();
        let body = format!("<h1>Large</h1>{}", "x".repeat(600000));
        let create = |id: &str, acl: Option<&[String]>| {
            db.create_content(
                id,
                None,
                "html",
                &body,
                "token",
                Some("owner"),
                1,
                9999999999,
                9999999999,
                1,
                acl,
                Some("owner"),
            )
        };
        // DB failure after file creation leaves neither a reference nor an object.
        assert!(create("bad", Some(&["missing".into()])).is_err());
        assert!(db.get_content("bad").unwrap().is_none());
        assert!(!db.body_dir.exists());
        create("large", None).unwrap();
        let mut content = db.get_content("large").unwrap().unwrap();
        assert!(content.content.is_empty());
        db.hydrate_content(&mut content).unwrap();
        assert_eq!(content.content, body);
        let file = std::fs::read_dir(&db.body_dir)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        std::fs::write(&file, b"corrupt").unwrap();
        assert!(db.consume_authorized_content("large", "owner", 2).is_err());
        assert!(db.get_content("large").unwrap().is_some());
        std::fs::write(&file, body.as_bytes()).unwrap();
        assert!(db
            .consume_authorized_content("large", "outsider", 2)
            .unwrap()
            .is_none());
        assert_eq!(
            db.consume_authorized_content("large", "owner", 2)
                .unwrap()
                .unwrap()
                .content,
            body
        );
        assert!(!file.exists());
        assert!(db
            .consume_authorized_content("large", "owner", 2)
            .unwrap()
            .is_none());
        create("snapshot", None).unwrap();
        let mut snapshot = db.get_content("snapshot").unwrap().unwrap();
        db.delete_managed_content("snapshot").unwrap();
        assert!(db.hydrate_content(&mut snapshot).is_err());
        std::fs::remove_dir(&db.body_dir).unwrap();
        // Filesystem failure must not create database metadata.
        std::fs::write(&db.body_dir, b"not a directory").unwrap();
        assert!(create("storage-failure", None).is_err());
        assert!(db.get_content("storage-failure").unwrap().is_none());
        std::fs::remove_file(&db.body_dir).unwrap();
    }

    #[test]
    fn invalid_acl_is_rejected_before_external_body_storage() {
        let db = Database::new(":memory:").unwrap();
        let error = db
            .create_content(
                "invalid",
                None,
                "html",
                &"x".repeat(600000),
                "token",
                None,
                1,
                2,
                2,
                0,
                Some(&["missing-token".into()]),
                None,
            )
            .unwrap_err();
        assert!(matches!(error, rusqlite::Error::InvalidParameterName(_)));
        assert!(!db.body_dir.exists());
        assert!(db.get_content("invalid").unwrap().is_none());
    }

    #[test]
    fn external_body_gc_has_grace_period_and_keeps_referenced_objects() {
        let db = Database::new(":memory:").unwrap();
        let now = chrono::Utc::now().timestamp();
        db.create_content(
            "live",
            None,
            "html",
            &"x".repeat(600000),
            "public",
            None,
            now,
            now + 3600,
            now + 3600,
            0,
            None,
            None,
        )
        .unwrap();
        let live = std::fs::read_dir(&db.body_dir)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let orphan = db.body_dir.join(generate_id());
        std::fs::write(&orphan, b"orphan").unwrap();
        db.collect_orphans(now).unwrap();
        assert!(orphan.exists());
        for path in [&live, &orphan] {
            std::fs::File::options()
                .write(true)
                .open(path)
                .unwrap()
                .set_times(std::fs::FileTimes::new().set_modified(
                    std::time::UNIX_EPOCH + std::time::Duration::from_secs((now - 7200) as u64),
                ))
                .unwrap();
        }
        db.collect_orphans(now).unwrap();
        assert!(!orphan.exists());
        assert!(live.exists());
        assert_eq!(db.cleanup_expired(now + 3601).unwrap(), 1);
        assert!(!live.exists());
        std::fs::remove_dir(&db.body_dir).unwrap();
    }

    #[test]
    fn session_migration_backs_up_committed_wal_and_preserves_existing_data() {
        let directory =
            std::env::temp_dir().join(format!("flashpaste-migration-{}", generate_id()));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("legacy.sqlite3");
        let old = Connection::open(&path).unwrap();
        old.execute_batch("PRAGMA journal_mode=WAL;").unwrap();
        old.execute_batch(include_str!("../migrations/0001_init.sql"))
            .unwrap();
        old.execute("INSERT INTO tokens(id,token_hash,name,level,created_at) VALUES('old','hash','old','user',1)",[]).unwrap();
        old.execute("INSERT INTO contents(id,title,content_type,content,visibility,created_by,created_at,expires_at,max_expires_at,burn_after_read) VALUES('legacy-content',NULL,'text','old body','token','old',1,9999999999,9999999999,0)",[]).unwrap();
        old.execute("INSERT INTO access_control(content_id,token_id,granted_at,granted_by) VALUES('legacy-content','old',1,'old')",[]).unwrap();
        let db = Database::new(path.to_str().unwrap()).unwrap();
        let backup = std::fs::read_dir(&directory)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .contains("pre-sessions-")
            })
            .unwrap();
        let saved = Connection::open(&backup).unwrap();
        assert_eq!(
            saved
                .query_row("SELECT COUNT(*) FROM tokens WHERE id='old'", [], |row| row
                    .get::<_, i64>(
                    0
                ))
                .unwrap(),
            1
        );
        assert!(saved.prepare("SELECT auth_version FROM tokens").is_err());
        assert_eq!(db.get_token("old").unwrap().unwrap().name, "old");
        assert_eq!(
            db.get_content("legacy-content").unwrap().unwrap().content,
            "old body"
        );
        assert_eq!(
            saved
                .query_row("SELECT COUNT(*) FROM access_control", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            db.conn
                .lock()
                .unwrap()
                .query_row("SELECT COUNT(*) FROM access_control", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
        // Restart is idempotent and must not repeat ALTER or discard data.
        let reopened = Database::new(path.to_str().unwrap()).unwrap();
        assert_eq!(
            reopened
                .get_content("legacy-content")
                .unwrap()
                .unwrap()
                .content,
            "old body"
        );
        drop(reopened);
        assert_eq!(
            db.conn
                .lock()
                .unwrap()
                .query_row(
                    "SELECT auth_version FROM tokens WHERE id='old'",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&backup).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        drop(saved);
        drop(db);
        drop(old);
        std::fs::remove_file(backup).unwrap();
        std::fs::remove_file(path).unwrap();
        // SQLite may retain its own sidecars until its final connection closes.
        for name in ["legacy.sqlite3-wal", "legacy.sqlite3-shm"] {
            let file = directory.join(name);
            if file.exists() {
                std::fs::remove_file(file).unwrap();
            }
        }
        std::fs::remove_dir(directory).unwrap();
    }

    fn database() -> (Database, String) {
        let path = std::env::temp_dir().join(format!(
            "flashpaste-test-{}-{}.db",
            std::process::id(),
            rand::thread_rng().gen::<u64>()
        ));
        let path = path.to_string_lossy().into_owned();
        (Database::new(&path).unwrap(), path)
    }

    #[test]
    fn consume_content_returns_once_and_deletes() {
        let (db, path) = database();
        db.create_token("reader", "reader_hash", "reader", "user", None, 1)
            .unwrap();
        db.create_content(
            "burn",
            Some("burn"),
            "text",
            "secret",
            "token",
            Some("reader"),
            1,
            100,
            100,
            1,
            None,
            None,
        )
        .unwrap();

        let consumed = db
            .consume_authorized_content("burn", "reader", 2)
            .unwrap()
            .unwrap();
        assert_eq!(consumed.content, "secret");
        assert!(db
            .consume_authorized_content("burn", "reader", 2)
            .unwrap()
            .is_none());
        assert!(db.get_content("burn").unwrap().is_none());
        drop(db);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn expired_content_cannot_be_consumed() {
        let (db, path) = database();
        db.create_token("reader", "reader_hash", "reader", "user", None, 1)
            .unwrap();
        db.create_content(
            "expired",
            Some("expired"),
            "text",
            "secret",
            "token",
            Some("reader"),
            1,
            10,
            10,
            1,
            None,
            None,
        )
        .unwrap();

        assert!(db
            .consume_authorized_content("expired", "reader", 10)
            .unwrap()
            .is_none());
        assert!(db.get_content("expired").unwrap().is_some());
        drop(db);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn acl_replacement_is_atomic_and_dashboard_scoped() {
        let (db, path) = database();
        let now = chrono::Utc::now().timestamp();
        db.create_token("owner", "owner_hash", "owner", "user", None, 1)
            .unwrap();
        db.create_token("viewer", "viewer_hash", "viewer", "user", None, 1)
            .unwrap();
        db.create_content(
            "private",
            Some("private"),
            "text",
            "secret",
            "token",
            Some("owner"),
            now,
            now + 100,
            now + 100,
            0,
            Some(&["viewer".to_string()]),
            Some("owner"),
        )
        .unwrap();
        assert!(db.has_access_control("private", "viewer").unwrap());
        assert_eq!(db.list_dashboard_contents(Some("viewer")).unwrap().len(), 1);

        let invalid = db.patch_content(
            "private",
            "token",
            now + 80,
            Some(&["missing".to_string()]),
            "owner",
            now + 1,
        );
        assert!(invalid.is_err());
        assert!(
            db.has_access_control("private", "viewer").unwrap(),
            "failed ACL replace must roll back prior deletion"
        );
        db.patch_content("private", "public", now + 80, None, "owner", now + 1)
            .unwrap();
        assert!(!db.has_access_control("private", "viewer").unwrap());
        assert_eq!(db.list_dashboard_contents(Some("viewer")).unwrap().len(), 0);
        drop(db);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn guarded_token_delete_prevents_lockout_and_cascades_access() {
        let (db, path) = database();
        db.create_token("super", "super_hash", "super", "super", None, 1)
            .unwrap();
        db.create_token("normal", "normal_hash", "normal", "user", Some("super"), 1)
            .unwrap();
        assert_eq!(db.delete_token_guarded("super").unwrap(), Some(false));
        assert_eq!(db.delete_token_guarded("normal").unwrap(), Some(true));
        assert_eq!(db.delete_token_guarded("normal").unwrap(), None);
        drop(db);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn startup_migration_drops_legacy_plaintext_token_column() {
        let path = std::env::temp_dir().join(format!(
            "flashpaste-legacy-test-{}-{}.db",
            std::process::id(),
            rand::thread_rng().gen::<u64>()
        ));
        let path = path.to_string_lossy().into_owned();
        {
            let old = Connection::open(&path).unwrap();
            old.execute_batch("CREATE TABLE tokens (id TEXT PRIMARY KEY, token_hash TEXT NOT NULL, token_value TEXT, name TEXT NOT NULL, level TEXT NOT NULL, created_by TEXT, created_at INTEGER NOT NULL); INSERT INTO tokens (id, token_hash, token_value, name, level, created_at) VALUES ('legacy-admin', 'hash', NULL, 'legacy-admin', 'admin', 1);")
                .unwrap();
        }
        let db = Database::new(&path).unwrap();
        let conn = db.conn.lock().unwrap();
        let columns: Vec<String> = conn
            .prepare("PRAGMA table_info(tokens)")
            .unwrap()
            .query_map([], |row| row.get(1))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert!(!columns.iter().any(|name| name == "token_value"));
        let legacy_level: String = conn
            .query_row(
                "SELECT level FROM tokens WHERE id='legacy-admin'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(legacy_level, "user");
        drop(conn);
        drop(db);
        let _ = std::fs::remove_file(path);
    }
}
