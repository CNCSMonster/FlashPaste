use crate::{generate_token_value, get_d1, now_timestamp, session, TokenRow};
use serde::Deserialize;
use worker::*;

#[derive(Default)]
pub struct RequestAuth {
    pub token: Option<TokenRow>,
    pub cookie: bool,
}

fn error(status: u16, code: &str) -> Response {
    Response::from_json(&serde_json::json!({"error":code,"code":code}))
        .unwrap()
        .with_status(status)
}

fn clear(mut response: Response) -> Response {
    response
        .headers_mut()
        .set("Set-Cookie", &session::clear_cookie())
        .unwrap();
    response
}

pub fn configured_origin(req: &Request, env: &Env) -> Option<String> {
    let expected = session::origin(&env.var("FLASHPASTE_ORIGIN").ok()?.to_string())?;
    let url = req.url().ok()?;
    (url.scheme() == "https" && url.origin().ascii_serialization() == expected).then_some(expected)
}

fn csrf(req: &Request, env: &Env) -> bool {
    session::csrf(
        configured_origin(req, env).as_deref(),
        req.headers().get("Origin").ok().flatten().as_deref(),
        req.headers().get("Content-Type").ok().flatten().as_deref(),
    )
}

fn cookie_hash(req: &Request) -> Option<String> {
    session::cookie(&req.headers().get("Cookie").ok()??).map(session::digest)
}

#[derive(Deserialize)]
struct SessionRow {
    id: String,
    name: String,
    level: String,
    created_at: f64,
    expires_at: f64,
    quota_bytes: Option<i64>,
}

async fn current(db: &D1Database, hash: &str) -> Result<Option<(TokenRow, i64)>> {
    let row: Option<SessionRow> = db.prepare("SELECT t.id,t.name,t.level,t.created_at,t.quota_bytes,s.expires_at FROM sessions s JOIN tokens t ON t.id=s.token_id WHERE s.session_hash=? AND s.expires_at>? AND s.token_version=t.auth_version")
        .bind(&[hash.into(),now_timestamp().into()])?.first(None).await?;
    Ok(row.map(|r| {
        (
            TokenRow {
                id: r.id,
                name: r.name,
                level: r.level,
                created_at: r.created_at,
                quota_bytes: r.quota_bytes,
            },
            r.expires_at as i64,
        )
    }))
}

pub async fn authenticate(req: &Request, env: &Env) -> std::result::Result<RequestAuth, Response> {
    let path = req.path();
    if path.starts_with("/api/auth/") || !(path.starts_with("/api/") || path.starts_with("/d/")) {
        return Ok(RequestAuth::default());
    }
    let db = get_d1(env).map_err(|_| error(503, "AUTH_STORAGE_UNAVAILABLE"))?;
    if let Some(header) = req.headers().get("Authorization").ok().flatten() {
        let raw = session::bearer(&header).ok_or_else(|| error(401, "INVALID_AUTHORIZATION"))?;
        let token = db
            .prepare("SELECT id,name,level,created_at,quota_bytes FROM tokens WHERE token_hash=?")
            .bind(&[session::digest(raw).into()])
            .map_err(|_| error(503, "AUTH_STORAGE_UNAVAILABLE"))?
            .first::<TokenRow>(None)
            .await
            .map_err(|_| error(503, "AUTH_STORAGE_UNAVAILABLE"))?
            .ok_or_else(|| error(401, "INVALID_AUTHORIZATION"))?;
        return Ok(RequestAuth {
            token: Some(token),
            cookie: false,
        });
    }
    if req
        .headers()
        .get("Cookie")
        .ok()
        .flatten()
        .as_deref()
        .is_some_and(session::has_cookie)
    {
        if configured_origin(req, env).is_none() {
            return Err(error(503, "HTTPS_REQUIRED"));
        }
        let hash = cookie_hash(req).ok_or_else(|| clear(error(401, "INVALID_SESSION")))?;
        let (token, _) = current(&db, &hash)
            .await
            .map_err(|_| error(503, "AUTH_STORAGE_UNAVAILABLE"))?
            .ok_or_else(|| clear(error(401, "INVALID_SESSION")))?;
        if !matches!(req.method(), Method::Get | Method::Head | Method::Options) && !csrf(req, env)
        {
            return Err(error(403, "CSRF_REJECTED"));
        }
        return Ok(RequestAuth {
            token: Some(token),
            cookie: true,
        });
    }
    Ok(RequestAuth::default())
}

pub fn config(req: &Request, env: &Env) -> Result<Response> {
    let url = req.url()?;
    let local_http = url.scheme() == "http"
        && matches!(
            url.host_str(),
            Some("localhost" | "127.0.0.1" | "[::1]" | "::1")
        );
    Response::from_json(
        &serde_json::json!({"cookie_login":configured_origin(req,env).is_some(),"debug_bearer":local_http}),
    )
}

#[derive(Deserialize)]
struct Issued {
    token_id: String,
}

pub async fn login(mut req: Request, env: &Env) -> Result<Response> {
    if req.headers().get("Authorization")?.is_some() {
        return Ok(error(401, "COOKIE_SESSION_ONLY"));
    }
    if configured_origin(&req, env).is_none() {
        return Ok(error(503, "HTTPS_REQUIRED"));
    }
    if !csrf(&req, env) {
        return Ok(error(403, "CSRF_REJECTED"));
    }
    let input: session::LoginRequest = match req.json().await {
        Ok(input) => input,
        Err(_) => return Ok(error(400, "INVALID_LOGIN_REQUEST")),
    };
    let db = get_d1(env)?;
    let raw = generate_token_value();
    let hash = session::digest(&raw);
    let now = now_timestamp().floor();
    let expires_at = now + session::ttl(input.remember_device) as f64;
    // Hash verification and version capture are in the SAME INSERT SELECT.
    // A rotation committed first means the old hash cannot insert anything.
    let result = db.batch(vec![
        db.prepare("INSERT INTO sessions(session_hash,token_id,token_version,created_at,expires_at) SELECT ?,id,auth_version,?,? FROM tokens WHERE token_hash=? RETURNING token_id")
            .bind(&[hash.clone().into(),now.into(),expires_at.into(),session::digest(&input.token).into()])?,
        db.prepare("DELETE FROM sessions WHERE session_hash=? AND EXISTS(SELECT 1 FROM sessions WHERE session_hash=?)")
            .bind(&[cookie_hash(&req).unwrap_or_default().into(),hash.clone().into()])?,
        // Read the response identity inside the same transaction. A later
        // database read must not fail after replacing a valid old session.
        db.prepare("SELECT t.id,t.name,t.level,t.created_at,s.expires_at FROM sessions s JOIN tokens t ON t.id=s.token_id WHERE s.session_hash=? AND s.token_version=t.auth_version")
            .bind(&[hash.clone().into()])?,
    ]).await;
    let result = match result {
        Ok(result) => result,
        Err(_) => return Ok(error(503, "AUTH_STORAGE_UNAVAILABLE")),
    };
    let issued: Vec<Issued> = result[0].results()?;
    if issued.is_empty() {
        return Ok(error(401, "INVALID_TOKEN"));
    }
    let identities: Vec<SessionRow> = result[2].results()?;
    let Some(identity) = identities.into_iter().next() else {
        return Ok(error(401, "INVALID_TOKEN"));
    };
    if identity.id != issued[0].token_id {
        return Ok(error(401, "INVALID_TOKEN"));
    }
    let mut response = Response::from_json(&session::Identity {
        token_id: identity.id,
        name: identity.name,
        level: session::api_level(&identity.level).into(),
        expires_at: identity.expires_at as i64,
    })?;
    response.headers_mut().set(
        "Set-Cookie",
        &session::set_cookie(&raw, input.remember_device),
    )?;
    Ok(response)
}

pub async fn restore(req: Request, env: &Env) -> Result<Response> {
    if req.headers().get("Authorization")?.is_some() {
        return Ok(error(401, "COOKIE_SESSION_ONLY"));
    }
    if configured_origin(&req, env).is_none() {
        return Ok(error(503, "HTTPS_REQUIRED"));
    }
    let received = req
        .headers()
        .get("Cookie")
        .ok()
        .flatten()
        .as_deref()
        .is_some_and(session::has_cookie);
    let Some(hash) = cookie_hash(&req) else {
        let response = error(401, "NO_SESSION");
        return Ok(if received { clear(response) } else { response });
    };
    match current(&get_d1(env)?, &hash).await {
        Ok(Some((token, expires_at))) => Response::from_json(&session::Identity {
            token_id: token.id,
            name: token.name,
            level: session::api_level(&token.level).into(),
            expires_at,
        }),
        Ok(None) => Ok(clear(error(401, "INVALID_SESSION"))),
        Err(_) => Ok(error(503, "AUTH_STORAGE_UNAVAILABLE")),
    }
}

pub async fn logout(req: Request, env: &Env) -> Result<Response> {
    if req.headers().get("Authorization")?.is_some() {
        return Ok(error(401, "COOKIE_SESSION_ONLY"));
    }
    if configured_origin(&req, env).is_none() {
        return Ok(error(503, "HTTPS_REQUIRED"));
    }
    if !csrf(&req, env) {
        return Ok(error(403, "CSRF_REJECTED"));
    }
    if let Some(hash) = cookie_hash(&req) {
        if get_d1(env)?
            .prepare("DELETE FROM sessions WHERE session_hash=?")
            .bind(&[hash.into()])?
            .run()
            .await
            .is_err()
        {
            return Ok(error(503, "AUTH_STORAGE_UNAVAILABLE"));
        }
    }
    Ok(clear(Response::empty()?.with_status(204)))
}
