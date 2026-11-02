//! Axum authentication adapter. The Worker adapter uses the same Cookie policy.
use crate::{session, AppState};
use axum::{
    extract::State,
    http::{HeaderMap, HeaderValue, Method, Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};

#[derive(Clone, Default)]
pub struct RequestAuth {
    pub token_id: Option<String>,
    pub cookie: bool,
}

impl RequestAuth {
    pub fn require_id(&self) -> Result<String, StatusCode> {
        self.token_id.clone().ok_or(StatusCode::UNAUTHORIZED)
    }
}

fn error(status: StatusCode, code: &str) -> Response {
    (status, Json(serde_json::json!({"error":code,"code":code}))).into_response()
}

fn clear(mut response: Response) -> Response {
    response.headers_mut().insert(
        "Set-Cookie",
        HeaderValue::from_str(&session::clear_cookie()).unwrap(),
    );
    response
}

fn csrf(state: &AppState, headers: &HeaderMap) -> bool {
    session::csrf(
        state.cookie_origin.as_deref(),
        headers.get("Origin").and_then(|h| h.to_str().ok()),
        headers.get("Content-Type").and_then(|h| h.to_str().ok()),
    )
}

fn cookie_hash(headers: &HeaderMap) -> Option<String> {
    if headers.get_all("Cookie").iter().count() > 1 {
        return None;
    }
    session::cookie(headers.get("Cookie")?.to_str().ok()?).map(session::digest)
}

pub async fn authenticate(
    State(state): State<AppState>,
    mut request: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let path = request.uri().path();
    let mut auth = RequestAuth::default();
    if (path.starts_with("/api/") && !path.starts_with("/api/auth/")) || path.starts_with("/d/") {
        let headers = request.headers();
        if let Some(value) = headers.get("Authorization") {
            let Some(raw) = value.to_str().ok().and_then(session::bearer) else {
                return error(StatusCode::UNAUTHORIZED, "INVALID_AUTHORIZATION");
            };
            match state.db.get_token_by_hash(&session::digest(raw)) {
                Ok(Some(token)) => auth.token_id = Some(token.id),
                Ok(None) => return error(StatusCode::UNAUTHORIZED, "INVALID_AUTHORIZATION"),
                Err(_) => {
                    return error(StatusCode::SERVICE_UNAVAILABLE, "AUTH_STORAGE_UNAVAILABLE")
                }
            }
        } else if headers
            .get("Cookie")
            .and_then(|h| h.to_str().ok())
            .is_some_and(session::has_cookie)
        {
            if state.cookie_origin.is_none() {
                return error(StatusCode::SERVICE_UNAVAILABLE, "HTTPS_REQUIRED");
            }
            let Some(hash) = cookie_hash(headers) else {
                return clear(error(StatusCode::UNAUTHORIZED, "INVALID_SESSION"));
            };
            match state.db.get_session(&hash, chrono::Utc::now().timestamp()) {
                Ok(Some((token, _))) => {
                    auth.token_id = Some(token.id);
                    auth.cookie = true;
                }
                Ok(None) => return clear(error(StatusCode::UNAUTHORIZED, "INVALID_SESSION")),
                Err(_) => {
                    return error(StatusCode::SERVICE_UNAVAILABLE, "AUTH_STORAGE_UNAVAILABLE")
                }
            }
            if !matches!(
                *request.method(),
                Method::GET | Method::HEAD | Method::OPTIONS
            ) && !csrf(&state, headers)
            {
                return error(StatusCode::FORBIDDEN, "CSRF_REJECTED");
            }
        }
    }
    request.extensions_mut().insert(auth);
    next.run(request).await
}

pub async fn config(State(state): State<AppState>) -> Json<serde_json::Value> {
    Json(
        serde_json::json!({"cookie_login":state.cookie_origin.is_some(),"debug_bearer":state.cookie_origin.is_none()}),
    )
}

pub async fn login(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<session::LoginRequest>,
) -> Response {
    if headers.contains_key("Authorization") {
        return error(StatusCode::UNAUTHORIZED, "COOKIE_SESSION_ONLY");
    }
    if state.cookie_origin.is_none() {
        return error(StatusCode::SERVICE_UNAVAILABLE, "HTTPS_REQUIRED");
    }
    if !csrf(&state, &headers) {
        return error(StatusCode::FORBIDDEN, "CSRF_REJECTED");
    }
    let raw = crate::generate_token_value();
    match state.db.create_session(
        &session::digest(&input.token),
        &session::digest(&raw),
        cookie_hash(&headers).as_deref(),
        chrono::Utc::now().timestamp(),
        session::ttl(input.remember_device),
    ) {
        Ok(Some((token, expires_at))) => {
            let mut response = Json(session::Identity {
                token_id: token.id,
                name: token.name,
                level: session::api_level(&token.level).into(),
                expires_at,
            })
            .into_response();
            response.headers_mut().insert(
                "Set-Cookie",
                HeaderValue::from_str(&session::set_cookie(&raw, input.remember_device)).unwrap(),
            );
            response
        }
        Ok(None) => error(StatusCode::UNAUTHORIZED, "INVALID_TOKEN"),
        Err(_) => error(StatusCode::SERVICE_UNAVAILABLE, "AUTH_STORAGE_UNAVAILABLE"),
    }
}

pub async fn current(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if headers.contains_key("Authorization") {
        return error(StatusCode::UNAUTHORIZED, "COOKIE_SESSION_ONLY");
    }
    if state.cookie_origin.is_none() {
        return error(StatusCode::SERVICE_UNAVAILABLE, "HTTPS_REQUIRED");
    }
    let received = headers
        .get("Cookie")
        .and_then(|h| h.to_str().ok())
        .is_some_and(session::has_cookie);
    let Some(hash) = cookie_hash(&headers) else {
        let response = error(StatusCode::UNAUTHORIZED, "NO_SESSION");
        return if received { clear(response) } else { response };
    };
    match state.db.get_session(&hash, chrono::Utc::now().timestamp()) {
        Ok(Some((token, expires_at))) => Json(session::Identity {
            token_id: token.id,
            name: token.name,
            level: session::api_level(&token.level).into(),
            expires_at,
        })
        .into_response(),
        Ok(None) => clear(error(StatusCode::UNAUTHORIZED, "INVALID_SESSION")),
        Err(_) => error(StatusCode::SERVICE_UNAVAILABLE, "AUTH_STORAGE_UNAVAILABLE"),
    }
}

pub async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if headers.contains_key("Authorization") {
        return error(StatusCode::UNAUTHORIZED, "COOKIE_SESSION_ONLY");
    }
    if state.cookie_origin.is_none() {
        return error(StatusCode::SERVICE_UNAVAILABLE, "HTTPS_REQUIRED");
    }
    if !csrf(&state, &headers) {
        return error(StatusCode::FORBIDDEN, "CSRF_REJECTED");
    }
    if let Some(hash) = cookie_hash(&headers) {
        if state.db.delete_session(&hash).is_err() {
            return error(StatusCode::SERVICE_UNAVAILABLE, "AUTH_STORAGE_UNAVAILABLE");
        }
    }
    clear(StatusCode::NO_CONTENT.into_response())
}
