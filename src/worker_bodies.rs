use crate::{body_store, get_d1, ContentRow};
use futures_util::StreamExt;
use serde::Deserialize;
use worker::*;

pub async fn read_create(
    req: &mut Request,
) -> std::result::Result<crate::CreateContentRequest, u16> {
    let mut stream = req.stream().map_err(|_| 400u16)?;
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| 400u16)?;
        if bytes.len().saturating_add(chunk.len()) > body_store::REQUEST_LIMIT {
            return Err(413);
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| 400u16)
}

#[derive(Deserialize)]
struct Reference {
    object_key: Option<String>,
    size_bytes: Option<f64>,
    sha256: Option<String>,
}

pub async fn put(env: &Env, key: &str, content: &[u8], now: f64, expires_at: f64) -> Result<()> {
    if !body_store::valid_key(key) {
        return Err(Error::RustError("Invalid body key".into()));
    }
    let kv = env.kv("flashpaste_bodies")?;
    let mut builder = kv
        .put_bytes(key, content)
        .map_err(|e| Error::RustError(e.to_string()))?
        .metadata(serde_json::json!({ "created_at": now }))
        .map_err(|e| Error::RustError(e.to_string()))?;
    if expires_at > now {
        builder = builder.expiration(expires_at as u64);
    }
    builder
        .execute()
        .await
        .map_err(|e| Error::RustError(e.to_string()))?;
    Ok(())
}

pub async fn hydrate(env: &Env, content: &mut ContentRow) -> Result<bool> {
    let db = get_d1(env)?;
    // Include the content row: a concurrently deleted reference is NOT an empty inline body.
    let reference: Option<Reference> = db
        .prepare("SELECT b.object_key,b.size_bytes,b.sha256 FROM contents c LEFT JOIN content_bodies b ON b.content_id=c.id WHERE c.id=?")
        .bind(&[content.id.clone().into()])?
        .first(None)
        .await?;
    let Some(reference) = reference else {
        return Ok(false);
    };
    let Some(key) = reference.object_key else {
        return Ok(true);
    };
    if !body_store::valid_key(&key) {
        return Err(Error::RustError("Invalid body key".into()));
    }
    let size = reference
        .size_bytes
        .ok_or_else(|| Error::RustError("Missing body size".into()))? as usize;
    let hash = reference
        .sha256
        .ok_or_else(|| Error::RustError("Missing body checksum".into()))?;
    if size > body_store::HTML_LIMIT {
        return Err(Error::RustError("Invalid body size".into()));
    }
    let kv = env.kv("flashpaste_bodies")?;
    let bytes = kv
        .get(&key)
        .bytes()
        .await
        .map_err(|e| Error::RustError(e.to_string()))?;
    let Some(bytes) = bytes else {
        if db
            .prepare("SELECT id FROM contents WHERE id=?")
            .bind(&[content.id.clone().into()])?
            .first::<serde_json::Value>(None)
            .await?
            .is_none()
        {
            return Ok(false);
        }
        return Err(Error::RustError("Stored body unavailable".into()));
    };
    if bytes.len() != size {
        return Err(Error::RustError("Stored body length mismatch".into()));
    }
    content.content = body_store::verify(bytes, size, &hash).map_err(Error::RustError)?;
    content.body_key = Some(key);
    Ok(true)
}

pub async fn key(env: &Env, id: &str) -> Result<Option<String>> {
    #[derive(Deserialize)]
    struct Key {
        object_key: String,
    }
    Ok(get_d1(env)?
        .prepare("SELECT object_key FROM content_bodies WHERE content_id=?")
        .bind(&[id.into()])?
        .first::<Key>(None)
        .await?
        .map(|r| r.object_key))
}

pub async fn remove(env: &Env, key: Option<&str>) {
    if let Some(key) = key {
        if !body_store::valid_key(key) {
            return;
        }
        let result = async {
            let kv = env.kv("flashpaste_bodies")?;
            kv.delete(key)
                .await
                .map_err(|e| Error::RustError(e.to_string()))
        }
        .await;
        if result.is_err() {
            console_error!("{}", r#"{"event":"body_cleanup_deferred"}"#);
        }
    }
}

pub async fn collect_orphans(env: &Env, now: f64) -> Result<()> {
    let kv = match env.kv("flashpaste_bodies") {
        Ok(kv) => kv,
        Err(_) => return Ok(()),
    };
    let db = get_d1(env)?;
    let mut cursor = None;
    loop {
        let mut list = kv.list().prefix("bodies/".to_string()).limit(100);
        if let Some(value) = cursor {
            list = list.cursor(value);
        }
        let page = list
            .execute()
            .await
            .map_err(|e| Error::RustError(e.to_string()))?;
        for key in page.keys {
            let key_name = key.name;
            if !body_store::valid_key(&key_name) {
                continue;
            }
            let created_at = key
                .metadata
                .as_ref()
                .and_then(|v| v.get("created_at"))
                .and_then(|v| v.as_f64());
            if let Some(created) = created_at {
                if now - created < body_store::ORPHAN_GRACE_SECONDS as f64 {
                    continue;
                }
            }
            if db
                .prepare("SELECT content_id FROM content_bodies WHERE object_key=?")
                .bind(&[key_name.clone().into()])?
                .first::<serde_json::Value>(None)
                .await?
                .is_none()
            {
                let _ = kv.delete(&key_name).await;
            }
        }
        if page.list_complete {
            break;
        }
        cursor = page.cursor;
        if cursor.is_none() {
            break;
        }
    }
    Ok(())
}
