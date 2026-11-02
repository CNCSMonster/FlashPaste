// 仅在 worker feature 激活时编译 Workers 版本
#![cfg(feature = "worker")]

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use wasm_bindgen::JsValue;
use worker::*;

mod body_store;
mod pages;
mod session;
mod worker_auth;
mod worker_bodies;

#[event(fetch)]
async fn main(req: Request, env: Env, ctx: Context) -> Result<Response> {
    let result = match worker_auth::authenticate(&req, &env).await {
        Ok(auth) => dispatch(req, env, &auth, &ctx).await,
        Err(response) => Ok(response),
    };
    let response = match result {
        Ok(response) => response,
        Err(error) => {
            console_error!(
                "{}",
                format!("{{\"event\":\"request_failed\",\"code\":\"SERVICE_UNAVAILABLE\",\"detail\":\"{}\"}}", error)
            );
            json_error("Service temporarily unavailable", 503)?
        }
    };
    let headers = response.headers().clone();
    // Static shells carry their own cache policy; everything else stays uncacheable.
    if headers.get("Cache-Control").ok().flatten().is_none() {
        headers.set("Cache-Control", "no-store")?;
    }
    headers.set("X-Content-Type-Options", "nosniff")?;
    headers.set("Referrer-Policy", "no-referrer")?;
    headers.set("X-Robots-Tag", "noindex, nofollow")?;
    headers.set("X-Frame-Options", "DENY")?;
    headers.set("Content-Security-Policy", "default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; frame-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'")?;
    Ok(response.with_headers(headers))
}

async fn dispatch(
    req: Request,
    env: Env,
    auth: &worker_auth::RequestAuth,
    ctx: &Context,
) -> Result<Response> {
    let path = req.path().to_string();
    let method = req.method().to_string();

    match (method.as_str(), path.as_str()) {
        ("GET", "/") => handle_index(req, &env, ctx).await,
        ("GET", "/app") => shell_response(&env, &req, ctx).await,

        ("GET", "/api/bootstrap/status") => handle_bootstrap_status(&env).await,

        ("GET", "/health") => Response::ok("OK"),

        ("POST", "/api/auth/verify") => handle_verify(req, &env).await,
        ("GET", "/api/auth/config") => worker_auth::config(&req, &env),
        ("POST", "/api/auth/login") => worker_auth::login(req, &env).await,
        ("GET", "/api/auth/session") => worker_auth::restore(req, &env).await,
        ("POST", "/api/auth/logout") => worker_auth::logout(req, &env).await,

        ("POST", "/api/tokens") => handle_create_token(req, &env, auth).await,

        ("GET", "/api/tokens") => handle_list_tokens(&env, auth).await,

        ("GET", "/api/tokens/lookup") => handle_lookup_token(req, &env, auth).await,
        ("POST", p) if p.starts_with("/api/tokens/") && p.ends_with("/rotate") => {
            let id = p
                .strip_prefix("/api/tokens/")
                .unwrap()
                .strip_suffix("/rotate")
                .unwrap();
            handle_rotate_token(id, &env, auth).await
        }
        ("DELETE", p) if p.starts_with("/api/tokens/") => {
            handle_delete_token(p.trim_start_matches("/api/tokens/"), &env, auth).await
        }

        ("POST", "/api/contents") => handle_create_content(req, &env, auth).await,
        ("GET", "/api/dashboard") => handle_dashboard(&env, auth).await,
        ("POST", p) if p.starts_with("/api/contents/") && p.ends_with("/consume") => {
            let id = p
                .trim_start_matches("/api/contents/")
                .trim_end_matches("/consume");
            handle_get_content_body(id, req, &env, auth).await
        }
        ("GET", p) if p.starts_with("/api/contents/") && !p.ends_with("/body") => {
            handle_get_content_metadata(p.trim_start_matches("/api/contents/"), &env, auth).await
        }
        ("PATCH", p) if p.starts_with("/api/contents/") => {
            handle_patch_content(p.trim_start_matches("/api/contents/"), req, &env, auth).await
        }
        ("DELETE", p) if p.starts_with("/api/contents/") => {
            handle_delete_content(p.trim_start_matches("/api/contents/"), &env, auth).await
        }
        ("GET" | "HEAD" | "POST", p) if p.starts_with("/d/") => {
            handle_download_content(p.trim_start_matches("/d/"), req, &env, auth).await
        }

        ("GET" | "HEAD", p) if p.starts_with("/api/contents/") && p.ends_with("/body") => {
            let id = p
                .strip_prefix("/api/contents/")
                .unwrap()
                .strip_suffix("/body")
                .unwrap();
            handle_get_content_body(id, req, &env, auth).await
        }

        ("GET", p) if p.starts_with("/v/") => {
            let id = p.strip_prefix("/v/").unwrap();
            handle_view_content(id, &env).await
        }

        _ => not_found_html(),
    }
}

#[event(scheduled)]
async fn scheduled(_event: ScheduledEvent, env: Env, _ctx: ScheduleContext) {
    if let Err(error) = async {
        let db = get_d1(&env)?;
        let now = now_timestamp();
        db.batch(vec![
            db.prepare("DELETE FROM access_control WHERE content_id IN (SELECT id FROM contents WHERE expires_at <= ?)")
                .bind(&[now.into()])?,
            db.prepare("DELETE FROM contents WHERE expires_at <= ?").bind(&[now.into()])?,
            db.prepare("DELETE FROM sessions WHERE expires_at <= ?").bind(&[now.into()])?,
        ]).await?;
        worker_bodies::collect_orphans(&env,now).await?;
        Ok::<(), worker::Error>(())
    }.await {
        worker_sys::console_error!("FlashPaste cleanup failed: {:?}", error);
    }
}

// ─── 辅助函数 ────────────────────────────────────────────────────────────────

fn now_timestamp() -> f64 {
    js_sys::Date::now() / 1000.0
}

pub(crate) const DEFAULT_MEMBER_QUOTA_BYTES: i64 = 100 * 1024 * 1024;

pub(crate) async fn active_usage_bytes(db: &D1Database, token_id: &str) -> Result<i64> {
    let row: Option<serde_json::Value> = db
        .prepare(
            "SELECT COALESCE(SUM(CASE WHEN b.size_bytes IS NOT NULL THEN b.size_bytes
                                      ELSE length(CAST(c.content AS BLOB)) END), 0) AS used
             FROM contents c LEFT JOIN content_bodies b ON b.content_id = c.id
             WHERE c.created_by = ? AND c.expires_at > ?",
        )
        .bind(&[token_id.into(), now_timestamp().into()])?
        .first(None)
        .await?;
    Ok(row
        .and_then(|value| value.get("used").and_then(|used| used.as_f64()))
        .unwrap_or(0.0) as i64)
}

fn hash_token(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    hex::encode(hasher.finalize())
}

fn api_token_level(level: &str) -> &str {
    if level == "user" || level == "admin" {
        "normal"
    } else {
        level
    }
}

fn generate_id() -> String {
    let mut bytes = [0u8; 16];
    getrandom::getrandom(&mut bytes).unwrap();
    hex::encode(bytes)
}

fn generate_token_value() -> String {
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes).unwrap();
    hex::encode(bytes)
}

fn generate_token_id() -> String {
    format!("tok_{}", generate_id())
}

fn json_response<T: Serialize>(value: &T) -> Result<Response> {
    Response::from_json(value)
}

fn json_error(msg: &str, status: u16) -> Result<Response> {
    Ok(Response::from_json(&serde_json::json!({"error": msg}))?.with_status(status))
}

fn not_found_json() -> Result<Response> {
    json_error("Not found", 404)
}

fn not_found_html() -> Result<Response> {
    Ok(Response::from_html(pages::unavailable())?.with_status(404))
}

fn get_d1(env: &Env) -> Result<D1Database> {
    env.d1("flashpaste_db")
}

// ─── 数据结构 ────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct VerifyRequest {
    token: String,
}

#[derive(Serialize)]
struct VerifyResponse {
    token_id: String,
    name: String,
    level: String,
}

#[derive(Clone, Deserialize, Serialize)]
struct TokenRow {
    id: String,
    name: String,
    level: String,
    created_at: f64,
    quota_bytes: Option<i64>,
}

#[derive(Deserialize)]
struct CreateContentRequest {
    title: Option<String>,
    content_type: String,
    content: String,
    visibility: String,
    ttl: i64,
    burn_after_read: Option<bool>,
    allowed_tokens: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct PatchContentRequest {
    expires_at: Option<f64>,
    visibility: Option<String>,
    allowed_tokens: Option<Vec<String>>,
}

#[derive(Serialize)]
struct CreateContentResponse {
    id: String,
    url: Option<String>,
}

#[derive(Deserialize, Serialize)]
struct ContentRow {
    id: String,
    title: Option<String>,
    content_type: String,
    content: String,
    visibility: String,
    created_by: Option<String>,
    created_at: f64,
    expires_at: f64,
    burn_after_read: i32,
    #[serde(skip)]
    body_key: Option<String>,
}

#[derive(Deserialize)]
struct AccessTokenRow {
    token_id: String,
}

#[derive(Serialize)]
struct ContentBodyResponse {
    content_type: String,
    content: String,
    visibility: String,
}

// ─── Handlers ─────────────────────────────────────────────────────────────────

async fn handle_bootstrap_status(env: &Env) -> Result<Response> {
    let db = get_d1(env)?;

    let stmt = db.prepare("SELECT COUNT(*) as count FROM tokens WHERE level = 'super'");
    let row: Option<serde_json::Value> = stmt.first(None).await?;

    let count = row
        .and_then(|r| r.get("count").and_then(|c| c.as_i64()))
        .unwrap_or(0);

    json_response(&serde_json::json!({
        "initialized": count > 0
    }))
}

async fn handle_index(req: Request, env: &Env, ctx: &Context) -> Result<Response> {
    let db = get_d1(env)?;

    // 检查是否有任何 token（初始化状态）
    let stmt = db.prepare("SELECT COUNT(*) as count FROM tokens");
    let row: Option<serde_json::Value> = stmt.first(None).await?;
    let count = row
        .and_then(|r| r.get("count").and_then(|c| c.as_i64()))
        .unwrap_or(0);

    if count == 0 {
        // 首次启动：自动生成管理员 token
        let token_value = generate_token_value();
        let token_hash = hash_token(&token_value);
        let id = generate_token_id();
        let now = now_timestamp();

        db.prepare("INSERT INTO tokens (id, token_hash, name, level, created_by, created_at) VALUES (?, ?, ?, ?, ?, ?)")
            .bind(&[
                id.clone().into(),
                token_hash.into(),
                "Super".into(),
                "super".into(),
                JsValue::NULL,
                now.into(),
            ])?
            .run()
            .await?;

        return Response::from_html(pages::bootstrap(&token_value));
    }

    shell_response(env, &req, ctx).await
}

fn hash_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Serves the app shell with inlined config, a content-hash ETag, browser caching and an
/// edge-cache entry keyed by that hash. The shell contains no user data.
async fn shell_response(env: &Env, req: &Request, ctx: &Context) -> Result<Response> {
    let url = req.url()?;
    let origin = url.origin().ascii_serialization();
    let cookie_login = worker_auth::configured_origin(req, env).is_some();
    let debug_bearer = url.scheme() == "http"
        && matches!(
            url.host_str(),
            Some("localhost" | "127.0.0.1" | "[::1]" | "::1")
        );
    let html = pages::inject_config(
        include_str!("../static/app.html"),
        cookie_login,
        debug_bearer,
    );
    let tag = hash_hex(html.as_bytes());
    let short = &tag[..16];
    let etag = format!("\"{short}\"");
    let cache_key = format!("{origin}/__shell/{short}");
    let browser_ttl = "public, max-age=60";
    let edge_ttl = "public, max-age=300";

    // Edge compression may downgrade the tag to a weak one; compare on the value only.
    let if_none_match = req
        .headers()
        .get("If-None-Match")
        .ok()
        .flatten()
        .map(|value| value.trim().trim_start_matches("W/").to_string());
    if if_none_match.as_deref() == Some(etag.as_str()) {
        return shell_headers(
            Response::empty()?.with_status(304),
            &etag,
            browser_ttl,
            "revalidate",
        );
    }

    if let Some(cached) = Cache::default().get(cache_key.as_str(), true).await? {
        return shell_headers(cached, &etag, browser_ttl, "hit");
    }

    let response = shell_headers(
        Response::from_html(html.clone())?,
        &etag,
        browser_ttl,
        "miss",
    )?;
    let to_cache = shell_headers(Response::from_html(html)?, &etag, edge_ttl, "miss")?;
    ctx.wait_until(async move {
        let _ = Cache::default().put(cache_key, to_cache).await;
    });
    Ok(response)
}

/// Worker responses are immutable once built, so headers are replaced rather than mutated.
fn shell_headers(
    response: Response,
    etag: &str,
    cache_control: &str,
    state: &str,
) -> Result<Response> {
    let headers = response.headers().clone();
    headers.set("ETag", etag)?;
    headers.set("Cache-Control", cache_control)?;
    headers.set("X-FlashPaste-Shell", state)?;
    Ok(response.with_headers(headers))
}

async fn handle_verify(mut req: Request, env: &Env) -> Result<Response> {
    let db = get_d1(env)?;
    let verify_req: VerifyRequest = req.json().await?;
    let token_hash = hash_token(&verify_req.token);

    let stmt = db
        .prepare("SELECT id, name, level, created_at, quota_bytes FROM tokens WHERE token_hash = ?")
        .bind(&[token_hash.into()])?;

    let result: Option<TokenRow> = stmt.first(None).await?;

    let token = if let Some(row) = result {
        row
    } else {
        return json_error("Invalid token", 401);
    };

    json_response(&VerifyResponse {
        token_id: token.id,
        name: token.name,
        level: api_token_level(&token.level).to_string(),
    })
}

async fn handle_list_tokens(env: &Env, auth: &worker_auth::RequestAuth) -> Result<Response> {
    let db = get_d1(env)?;

    let actor = match auth.token.clone() {
        Some(t) => t,
        None => return json_error("Invalid token", 401),
    };
    if actor.level != "super" {
        return json_error("Super token required", 403);
    }

    let stmt = db.prepare("SELECT id, name, CASE WHEN level IN ('user','admin') THEN 'normal' ELSE level END AS level, created_at, quota_bytes FROM tokens ORDER BY created_at DESC");
    let rows: Vec<serde_json::Value> = stmt.all().await?.results()?;
    json_response(&serde_json::json!({"tokens": rows}))
}

async fn handle_delete_token(
    id: &str,
    env: &Env,
    auth: &worker_auth::RequestAuth,
) -> Result<Response> {
    let db = get_d1(env)?;

    let actor = match auth.token.clone() {
        Some(t) => t,
        None => return json_error("Invalid token", 401),
    };
    if actor.level != "super" {
        return json_error("Super token required", 403);
    }

    let target: Option<TokenRow> = db
        .prepare("SELECT id,name,level,created_at,quota_bytes FROM tokens WHERE id=?")
        .bind(&[id.into()])?
        .first(None)
        .await?;
    let target = match target {
        Some(target) => target,
        None => return not_found_json(),
    };
    let deleted: Option<serde_json::Value> = db.prepare("DELETE FROM tokens WHERE id=? AND (level<>'super' OR (SELECT COUNT(*) FROM tokens WHERE level='super')>1) RETURNING id")
        .bind(&[id.into()])?
        .first(None).await?;
    if deleted.is_none() && target.level == "super" {
        return json_error("Cannot delete the last super token", 400);
    }
    if deleted.is_none() {
        return not_found_json();
    }

    json_response(&serde_json::json!({"id": id, "deleted": true}))
}

async fn handle_lookup_token(
    req: Request,
    env: &Env,
    auth: &worker_auth::RequestAuth,
) -> Result<Response> {
    let db = get_d1(env)?;
    if auth.token.is_none() {
        return json_error("Invalid token", 401);
    }
    let name = match req
        .url()?
        .query_pairs()
        .find(|(key, _)| key == "name")
        .map(|(_, value)| value.into_owned())
    {
        Some(name) if !name.is_empty() => name,
        _ => return json_error("Missing name", 400),
    };
    let token: Option<serde_json::Value> = db
        .prepare("SELECT id,name FROM tokens WHERE name=?")
        .bind(&[name.into()])?
        .first(None)
        .await?;
    match token {
        Some(token) => json_response(&token),
        None => not_found_json(),
    }
}

async fn handle_create_token(
    mut req: Request,
    env: &Env,
    auth: &worker_auth::RequestAuth,
) -> Result<Response> {
    let db = get_d1(env)?;

    let actor = match auth.token.clone() {
        Some(t) => t,
        None => return json_error("Invalid token", 401),
    };
    if actor.level != "super" {
        return json_error("Super token required", 403);
    }

    #[derive(Deserialize)]
    struct CreateTokenRequest {
        name: String,
        level: Option<String>,
        quota_mb: Option<f64>,
    }

    let body: CreateTokenRequest = match req.json().await {
        Ok(input) => input,
        Err(_) => return json_error("Invalid token request", 400),
    };
    if body.name.trim().is_empty() || body.name.trim().len() > 64 {
        return json_error("Token name must be 1-64 bytes", 400);
    }
    let name = body.name.trim().to_string();
    let level = match body.level.as_deref().unwrap_or("normal") {
        "normal" => "user",
        "super" => "super",
        _ => return json_error("Invalid level (super|normal)", 400),
    };

    let token_value = generate_token_value();
    let token_hash = hash_token(&token_value);
    let id = generate_token_id();
    let now = now_timestamp();

    let quota_bytes = match (level, body.quota_mb) {
        ("super", _) => None,
        (_, None) => None,
        (_, Some(mb)) if mb.is_finite() && (1.0..=10240.0).contains(&mb) => {
            Some((mb * 1024.0 * 1024.0).round() as i64)
        }
        _ => return json_error("Invalid quota_mb (1-10240)", 400),
    };
    let inserted = db.prepare("INSERT INTO tokens (id, token_hash, name, level, created_by, created_at, quota_bytes) VALUES (?, ?, ?, ?, ?, ?, ?) ON CONFLICT(name) DO NOTHING RETURNING id")
        .bind(&[
            id.clone().into(),
            token_hash.into(),
            name.clone().into(),
            level.into(),
            actor.id.clone().into(),
            now.into(),
            match quota_bytes {
                Some(value) => JsValue::from_f64(value as f64),
                None => JsValue::NULL,
            },
        ])?
        .first::<serde_json::Value>(None)
        .await?;
    if inserted.is_none() {
        return json_error("Token name already exists", 409);
    }

    Ok(Response::from_json(&serde_json::json!({
        "id": id,
        "token_id": id,
        "name": name,
        "level": api_token_level(level),
        "token": token_value,
        "token_plaintext": token_value,
        "quota_bytes": quota_bytes
    }))?
    .with_status(201))
}

async fn handle_rotate_token(
    id: &str,
    env: &Env,
    auth: &worker_auth::RequestAuth,
) -> Result<Response> {
    let db = get_d1(env)?;
    let actor = match auth.token.clone() {
        Some(actor) => actor,
        None => return json_error("Invalid token", 401),
    };
    if actor.level != "super" {
        return json_error("Super token required", 403);
    }
    let plaintext = generate_token_value();
    let hash = hash_token(&plaintext);
    let results = db.batch(vec![
        db.prepare("UPDATE tokens SET token_hash=?,auth_version=auth_version+1 WHERE id=? RETURNING id").bind(&[hash.into(), id.into()])?,
        db.prepare("DELETE FROM sessions WHERE token_id=?").bind(&[id.into()])?,
    ]).await?;
    if results[0].results::<serde_json::Value>()?.is_empty() {
        return not_found_json();
    }
    json_response(&serde_json::json!({"token_id": id, "token_plaintext": plaintext}))
}

async fn handle_create_content(
    mut req: Request,
    env: &Env,
    auth: &worker_auth::RequestAuth,
) -> Result<Response> {
    let db = get_d1(env)?;

    let creator = match auth.token.clone() {
        Some(t) => t,
        None => return json_error("Invalid token", 401),
    };

    let create_req: CreateContentRequest = match worker_bodies::read_create(&mut req).await {
        Ok(input) => input,
        Err(status) => return json_error("Invalid or oversized content request", status),
    };

    if !["text", "markdown", "html"].contains(&create_req.content_type.as_str()) {
        return json_error("Invalid content_type", 400);
    }
    if !["public", "token"].contains(&create_req.visibility.as_str()) {
        return json_error("Invalid visibility", 400);
    }
    if create_req.burn_after_read.unwrap_or(false) && create_req.visibility != "token" {
        return json_error("Burn-after-read content must be token protected", 400);
    }
    let allowed_tokens = create_req.allowed_tokens.unwrap_or_default();
    let mut unique_tokens = std::collections::HashSet::new();
    if allowed_tokens.iter().any(|id| !unique_tokens.insert(id)) {
        return json_error("Duplicate token ID in ACL", 400);
    }
    if create_req.visibility == "public" && !allowed_tokens.is_empty() {
        return json_error("Public content cannot have token ACLs", 400);
    }
    for token_id in &allowed_tokens {
        let exists: Option<serde_json::Value> = db
            .prepare("SELECT id FROM tokens WHERE id=?")
            .bind(&[token_id.as_str().into()])?
            .first(None)
            .await?;
        if exists.is_none() {
            return json_error("Unknown token ID in ACL", 400);
        }
    }
    if create_req.content.len() > body_store::limit(&create_req.content_type) {
        return json_error(
            "Content too large (HTML: 10 MiB; text/markdown: 500 KiB)",
            413,
        );
    }
    let valid_ttl = if create_req.burn_after_read.unwrap_or(false) {
        create_req.ttl == 604800
    } else {
        [3600, 21600, 86400, 172800].contains(&create_req.ttl)
    };
    if !valid_ttl {
        return json_error("Invalid ttl", 400);
    }

    if creator.level != "super" {
        let limit = creator.quota_bytes.unwrap_or(DEFAULT_MEMBER_QUOTA_BYTES);
        let used = active_usage_bytes(&db, &creator.id).await?;
        let incoming = create_req.content.len() as i64;
        if used + incoming > limit {
            return Ok(Response::from_json(&serde_json::json!({
                "error": "QUOTA_EXCEEDED",
                "code": "QUOTA_EXCEEDED",
                "used_bytes": used,
                "limit_bytes": limit,
                "incoming_bytes": incoming
            }))?
            .with_status(409));
        }
    }

    let id = generate_id();
    let now = now_timestamp();
    let expires_at = now + create_req.ttl as f64;
    let burn_after_read = if create_req.burn_after_read.unwrap_or(false) {
        1
    } else {
        0
    };

    let title = create_req
        .title
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(JsValue::from)
        .unwrap_or(JsValue::NULL);
    let content_type = create_req.content_type.clone();
    let external = create_req.content.len() > body_store::INLINE_LIMIT;
    let key = external.then(|| format!("bodies/{id}"));
    if let Some(key) = &key {
        worker_bodies::put(env, key, create_req.content.as_bytes(), now, expires_at).await?;
    }
    let content = if external {
        String::new()
    } else {
        create_req.content.clone()
    };
    let visibility = create_req.visibility.clone();

    let mut batch = vec![db.prepare(
        "INSERT INTO contents (id, title, content_type, content, visibility, created_by, created_at, expires_at, max_expires_at, burn_after_read) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&[
        id.clone().into(),
        title,
        content_type.into(),
        content.into(),
        visibility.into(),
        creator.id.clone().into(),
        now.into(),
        expires_at.into(),
        expires_at.into(),
        burn_after_read.into(),
    ])?];
    if let Some(key) = &key {
        batch.push(db.prepare("INSERT INTO content_bodies(content_id,object_key,size_bytes,sha256) VALUES(?,?,?,?)").bind(&[id.clone().into(),key.clone().into(),(create_req.content.len() as f64).into(),body_store::digest(create_req.content.as_bytes()).into()])?);
    }
    for token_id in allowed_tokens {
        batch.push(db.prepare("INSERT INTO access_control (content_id, token_id, granted_at, granted_by) VALUES (?, ?, ?, ?)")
            .bind(&[id.clone().into(), token_id.into(), now.into(), creator.id.clone().into()])?);
    }
    if let Err(error) = db.batch(batch).await {
        worker_bodies::remove(env, key.as_deref()).await;
        return Err(error);
    }

    let url = if create_req.visibility == "public" {
        Some(format!("/v/{}", id))
    } else {
        None
    };

    Ok(Response::from_json(&CreateContentResponse { id, url })?.with_status(201))
}

async fn handle_dashboard(env: &Env, auth: &worker_auth::RequestAuth) -> Result<Response> {
    let db = get_d1(env)?;
    let token = match auth.token.clone() {
        Some(token) => token,
        None => return json_error("Invalid token", 401),
    };

    let query = if token.level == "super" {
        "SELECT id, title, content_type, visibility, created_at, expires_at, burn_after_read, created_by FROM contents WHERE expires_at > ? ORDER BY created_at DESC"
    } else {
        "SELECT DISTINCT c.id, CASE WHEN c.burn_after_read=1 AND COALESCE(c.created_by,'')<>? THEN '[阅后即焚]' ELSE c.title END AS title, CASE WHEN c.burn_after_read=1 AND COALESCE(c.created_by,'')<>? THEN NULL ELSE c.content_type END AS content_type, c.visibility, c.created_at, c.expires_at, c.burn_after_read, c.created_by FROM contents c LEFT JOIN access_control a ON a.content_id=c.id WHERE c.expires_at > ? AND (c.created_by=? OR a.token_id=?) ORDER BY c.created_at DESC"
    };
    let stmt = db.prepare(query);
    let rows: Vec<serde_json::Value> = if token.level == "super" {
        stmt.bind(&[now_timestamp().into()])?
            .all()
            .await?
            .results()?
    } else {
        stmt.bind(&[
            token.id.clone().into(),
            token.id.clone().into(),
            now_timestamp().into(),
            token.id.clone().into(),
            token.id.clone().into(),
        ])?
        .all()
        .await?
        .results()?
    };
    let quota = if token.level == "super" {
        serde_json::Value::Null
    } else {
        let used = active_usage_bytes(&db, &token.id).await?;
        serde_json::json!({
            "used_bytes": used,
            "limit_bytes": token.quota_bytes.unwrap_or(DEFAULT_MEMBER_QUOTA_BYTES)
        })
    };
    json_response(&serde_json::json!({"items": rows, "quota": quota}))
}

async fn authorize_content(
    db: &D1Database,
    id: &str,
    auth: &worker_auth::RequestAuth,
) -> Result<Option<(TokenRow, ContentRow)>> {
    let viewer = match auth.token.clone() {
        Some(viewer) => viewer,
        None => return Ok(None),
    };
    let content: Option<ContentRow> = db.prepare("SELECT id,title,content_type,content,visibility,created_by,created_at,expires_at,burn_after_read FROM contents WHERE id=?")
        .bind(&[id.into()])?.first(None).await?;
    match content {
        Some(content) => Ok(Some((viewer, content))),
        None => Ok(None),
    }
}

async fn handle_get_content_metadata(
    id: &str,
    env: &Env,
    auth: &worker_auth::RequestAuth,
) -> Result<Response> {
    let db = get_d1(env)?;
    if auth.token.is_none() {
        return json_error("Token required", 401);
    }
    let (viewer, content) = match authorize_content(&db, id, auth).await? {
        Some(pair) => pair,
        None => return not_found_json(),
    };
    let is_owner = content.created_by.as_deref() == Some(&viewer.id);
    let is_super = viewer.level == "super";
    let is_granted = db
        .prepare("SELECT 1 FROM access_control WHERE content_id=? AND token_id=?")
        .bind(&[id.into(), viewer.id.clone().into()])?
        .first::<serde_json::Value>(None)
        .await?
        .is_some();
    if content.expires_at <= now_timestamp()
        || (content.visibility == "token" && !is_owner && !is_super && !is_granted)
        || (content.visibility == "public" && !is_owner && !is_super)
    {
        return not_found_json();
    }
    let can_manage = is_owner || is_super;
    let burn_hidden = content.burn_after_read == 1 && !can_manage;
    let allowed_tokens: Option<Vec<String>> = if can_manage && content.visibility == "token" {
        let rows: Vec<AccessTokenRow> = db
            .prepare("SELECT token_id FROM access_control WHERE content_id=? ORDER BY token_id")
            .bind(&[id.into()])?
            .all()
            .await?
            .results()?;
        Some(rows.into_iter().map(|row| row.token_id).collect())
    } else {
        None
    };
    json_response(&serde_json::json!({
        "id": content.id,
        "title": if burn_hidden { "[阅后即焚]" } else { content.title.as_deref().unwrap_or("") },
        "content_type": if burn_hidden { serde_json::Value::Null } else { serde_json::json!(content.content_type) },
        "visibility": content.visibility,
        "created_at": content.created_at,
        "expires_at": content.expires_at,
        "burn_after_read": content.burn_after_read == 1,
        "created_by": content.created_by,
        "allowed_tokens": allowed_tokens
    }))
}

async fn handle_patch_content(
    id: &str,
    mut req: Request,
    env: &Env,
    auth: &worker_auth::RequestAuth,
) -> Result<Response> {
    let db = get_d1(env)?;
    if auth.token.is_none() {
        return json_error("Token required", 401);
    }
    let (viewer, content) = match authorize_content(&db, id, auth).await? {
        Some(pair) => pair,
        None => return not_found_json(),
    };
    if viewer.level != "super" && content.created_by.as_deref() != Some(&viewer.id) {
        return not_found_json();
    }
    let patch: PatchContentRequest = match req.json().await {
        Ok(input) => input,
        Err(_) => return json_error("Invalid content patch", 400),
    };
    if let Some(allowed) = &patch.allowed_tokens {
        let mut unique = std::collections::HashSet::new();
        for token_id in allowed {
            if !unique.insert(token_id) {
                return json_error("Duplicate token ID in ACL", 400);
            }
            if db
                .prepare("SELECT id FROM tokens WHERE id=?")
                .bind(&[token_id.as_str().into()])?
                .first::<serde_json::Value>(None)
                .await?
                .is_none()
            {
                return json_error("Unknown token ID in ACL", 400);
            }
        }
    }
    let visibility = patch.visibility.as_deref().unwrap_or(&content.visibility);
    if !["public", "token"].contains(&visibility) {
        return json_error("Invalid visibility", 400);
    }
    let expires_at = patch.expires_at.unwrap_or(content.expires_at);
    if expires_at > content.expires_at || expires_at < content.created_at {
        return json_error("Expiry may only be shortened", 400);
    }
    if content.burn_after_read == 1 && visibility != "token" {
        return json_error("Burn-after-read content must remain token protected", 400);
    }
    let mut batch = vec![db
        .prepare("UPDATE contents SET visibility=?,expires_at=? WHERE id=?")
        .bind(&[visibility.into(), expires_at.into(), id.into()])?];
    if visibility == "public" {
        batch.push(
            db.prepare("DELETE FROM access_control WHERE content_id=?")
                .bind(&[id.into()])?,
        );
    } else if let Some(allowed) = patch.allowed_tokens {
        batch.push(
            db.prepare("DELETE FROM access_control WHERE content_id=?")
                .bind(&[id.into()])?,
        );
        for token_id in allowed {
            batch.push(db.prepare("INSERT INTO access_control (content_id,token_id,granted_at,granted_by) VALUES (?,?,?,?)")
                .bind(&[id.into(), token_id.into(), now_timestamp().into(), viewer.id.clone().into()])?);
        }
    }
    db.batch(batch).await?;
    json_response(&serde_json::json!({"id": id, "updated": true}))
}

async fn handle_delete_content(
    id: &str,
    env: &Env,
    auth: &worker_auth::RequestAuth,
) -> Result<Response> {
    let db = get_d1(env)?;
    let viewer = match auth.token.clone() {
        Some(v) => v,
        None => return json_error("Invalid token", 401),
    };
    let content = db
        .prepare("SELECT created_by FROM contents WHERE id=?")
        .bind(&[id.into()])?
        .first::<serde_json::Value>(None)
        .await?;
    let owner = content
        .as_ref()
        .and_then(|r| r.get("created_by"))
        .and_then(|v| v.as_str());
    if viewer.level != "super" && owner != Some(viewer.id.as_str()) {
        return not_found_json();
    }
    let body_key = worker_bodies::key(env, id).await?;
    let result = db
        .batch(vec![
            db.prepare("DELETE FROM access_control WHERE content_id=?")
                .bind(&[id.into()])?,
            db.prepare("DELETE FROM contents WHERE id=?")
                .bind(&[id.into()])?,
        ])
        .await?;
    if result
        .last()
        .and_then(|r| r.meta().ok())
        .flatten()
        .and_then(|m| m.changes)
        .unwrap_or(0)
        == 0
    {
        return not_found_json();
    }
    worker_bodies::remove(env, body_key.as_deref()).await;
    json_response(&serde_json::json!({"id": id, "deleted": true}))
}

async fn handle_download_content(
    id: &str,
    req: Request,
    env: &Env,
    auth: &worker_auth::RequestAuth,
) -> Result<Response> {
    if req.method() == Method::Post && auth.token.is_none() {
        return json_error("Authentication required", 401);
    }
    let db = get_d1(env)?;
    let row: Option<ContentRow> = db.prepare("SELECT id,title,content_type,content,visibility,created_by,created_at,expires_at,burn_after_read FROM contents WHERE id=?")
        .bind(&[id.into()])?.first(None).await?;
    let mut content = match row {
        Some(v) if v.expires_at > now_timestamp() => v,
        _ => return not_found_json(),
    };
    let mut viewer = None;
    if content.visibility != "public" {
        let viewer_identity = match auth.token.clone() {
            Some(v) => v,
            None => return not_found_json(),
        };
        let allowed = viewer_identity.level == "super"
            || content.created_by.as_deref() == Some(&viewer_identity.id)
            || db
                .prepare("SELECT 1 FROM access_control WHERE content_id=? AND token_id=?")
                .bind(&[id.into(), viewer_identity.id.clone().into()])?
                .first::<serde_json::Value>(None)
                .await?
                .is_some();
        if !allowed {
            return not_found_json();
        }
        viewer = Some(viewer_identity);
    }
    if req.method() == Method::Post && content.burn_after_read != 1 {
        return json_error("Not burn content", 400);
    }
    if content.burn_after_read == 1 {
        if req.method() == Method::Head || (auth.cookie && req.method() != Method::Post) {
            let mut response = json_error("Use POST to download burn content", 405)?;
            response.headers_mut().set("Allow", "POST")?;
            return Ok(response);
        }
        if content.visibility != "token" {
            return not_found_json();
        }
        let viewer = match viewer {
            Some(viewer) => viewer,
            None => return not_found_json(),
        };
        if !worker_bodies::hydrate(env, &mut content).await? {
            return not_found_json();
        }
        let deleted = db
            .prepare("DELETE FROM contents WHERE id=? AND expires_at>? AND burn_after_read=1 AND visibility='token' AND (created_by=? OR ?='super' OR EXISTS(SELECT 1 FROM access_control WHERE content_id=contents.id AND token_id=?)) RETURNING id")
            .bind(&[id.into(), now_timestamp().into(), viewer.id.clone().into(), viewer.level.into(), viewer.id.into()])?
            .first::<serde_json::Value>(None)
            .await?;
        if deleted.is_none() {
            return not_found_json();
        }
        worker_bodies::remove(env, content.body_key.as_deref()).await;
    } else if !worker_bodies::hydrate(env, &mut content).await? {
        return not_found_json();
    }
    let filename = content
        .title
        .as_deref()
        .unwrap_or("download")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_' || *c == '.')
        .take(100)
        .collect::<String>();
    let ext = match content.content_type.as_str() {
        "html" => "html",
        "markdown" => "md",
        _ => "txt",
    };
    let headers = Headers::new();
    headers.set("Content-Type", "application/octet-stream")?;
    headers.set(
        "Content-Disposition",
        &format!(
            "attachment; filename=\"{}.{}\"",
            if filename.is_empty() {
                "download"
            } else {
                &filename
            },
            ext
        ),
    )?;
    Ok(Response::from_bytes(content.content.into_bytes())?.with_headers(headers))
}

async fn handle_get_content_body(
    id: &str,
    req: Request,
    env: &Env,
    auth: &worker_auth::RequestAuth,
) -> Result<Response> {
    if req.method() == Method::Post && auth.token.is_none() {
        return json_error("Authentication required", 401);
    }
    let db = get_d1(env)?;

    let stmt = db.prepare(
        "SELECT id, title, content_type, content, visibility, created_by, created_at, expires_at, burn_after_read FROM contents WHERE id = ?",
    ).bind(&[id.into()])?;

    let content: Option<ContentRow> = stmt.first(None).await?;

    let mut content = match content {
        Some(row) => row,
        None => return not_found_json(),
    };

    let now = now_timestamp();
    if content.expires_at <= now {
        return not_found_json();
    }

    let viewer = auth.token.clone();
    let allowed = if content.visibility == "public" {
        true
    } else if let Some(viewer) = viewer.as_ref() {
        if viewer.level == "super" || content.created_by.as_deref() == Some(&viewer.id) {
            true
        } else {
            db.prepare("SELECT 1 FROM access_control WHERE content_id=? AND token_id=?")
                .bind(&[id.into(), viewer.id.clone().into()])?
                .first::<serde_json::Value>(None)
                .await?
                .is_some()
        }
    } else {
        false
    };
    if !allowed {
        return not_found_json();
    }
    if req.method() == Method::Post && content.burn_after_read != 1 {
        return json_error("Not burn content", 400);
    }

    let consume = req.method() == Method::Post
        || req
            .url()
            .ok()
            .and_then(|url| url.query().map(str::to_string))
            .map(|query| query.split('&').any(|p| p == "consume=0"))
            .map(|not_consume| !not_consume)
            .unwrap_or(true);
    if content.burn_after_read == 1 {
        if req.method() == Method::Head {
            let mut response = json_error("HEAD cannot consume content", 405)?;
            response.headers_mut().set("Allow", "GET, POST")?;
            return Ok(response);
        }
        if auth.cookie && req.method() != Method::Post && consume {
            return Ok(Response::from_json(&serde_json::json!({"error":"CONSUME_REQUIRES_POST","code":"CONSUME_REQUIRES_POST","consume_url":format!("/api/contents/{id}/consume")}))?.with_status(409));
        }
        if content.visibility != "token" {
            return not_found_json();
        }
        let can_preview = viewer
            .as_ref()
            .map(|v| v.level == "super" || content.created_by.as_deref() == Some(&v.id))
            .unwrap_or(false);
        if !consume && !can_preview {
            return json_error(
                "Burn content preview is restricted to its creator or super",
                403,
            );
        }
        if consume {
            let viewer = match viewer.as_ref() {
                Some(viewer) => viewer,
                None => return not_found_json(),
            };
            if !worker_bodies::hydrate(env, &mut content).await? {
                return not_found_json();
            }
            let deleted = db
                .prepare("DELETE FROM contents WHERE id=? AND expires_at>? AND burn_after_read=1 AND visibility='token' AND (created_by=? OR ?='super' OR EXISTS(SELECT 1 FROM access_control WHERE content_id=contents.id AND token_id=?)) RETURNING id")
                .bind(&[id.into(), now.into(), viewer.id.clone().into(), viewer.level.clone().into(), viewer.id.clone().into()])?
                .first::<serde_json::Value>(None)
                .await?;
            if deleted.is_none() {
                return not_found_json();
            }
            worker_bodies::remove(env, content.body_key.as_deref()).await;
        } else if !worker_bodies::hydrate(env, &mut content).await? {
            return not_found_json();
        }
    } else if !worker_bodies::hydrate(env, &mut content).await? {
        return not_found_json();
    }

    json_response(&ContentBodyResponse {
        content_type: content.content_type,
        content: content.content,
        visibility: content.visibility,
    })
}

async fn handle_view_content(id: &str, env: &Env) -> Result<Response> {
    let db = get_d1(env)?;

    let stmt = db.prepare(
        "SELECT id, title, content_type, content, visibility, created_by, created_at, expires_at, burn_after_read FROM contents WHERE id = ?",
    ).bind(&[id.into()])?;

    let content: Option<ContentRow> = stmt.first(None).await?;

    let content = match content {
        Some(row) => row,
        None => return not_found_html(),
    };

    let now = now_timestamp();
    if content.expires_at <= now {
        return not_found_html();
    }

    if content.visibility != "public" {
        return not_found_html();
    }
    if worker_bodies::key(env, id).await?.is_some() {
        return Response::from_html(pages::public_external_view(content.title.as_deref(), id));
    }

    Response::from_html(pages::public_view(
        content.title.as_deref(),
        &content.content_type,
        &content.content,
    ))
}
