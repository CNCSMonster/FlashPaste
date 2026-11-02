use axum::{
    extract::{Query, State},
    http::{HeaderMap, HeaderValue, Method, Request, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
    Extension, Json, Router,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::Arc;

mod auth;
mod body_store;
mod db;
mod pages;
mod session;
#[cfg(test)]
mod session_tests;

#[derive(Clone)]
struct AppState {
    db: Arc<db::Database>,
    cookie_origin: Option<String>,
}

#[tokio::main]
async fn main() {
    // 初始化数据库
    let db = db::Database::new("flashpaste.db").expect("Failed to init database");

    let cookie_origin = std::env::var("FLASHPASTE_ORIGIN").ok().map(|value| {
        session::origin(&value).expect("FLASHPASTE_ORIGIN must be a canonical HTTPS origin")
    });
    let bind = std::env::var("FLASHPASTE_BIND").unwrap_or_else(|_| {
        if cookie_origin.is_some() {
            "127.0.0.1:3000".into()
        } else {
            "0.0.0.0:3000".into()
        }
    });
    let state = AppState {
        db: Arc::new(db),
        cookie_origin,
    };
    let cleanup_db = state.db.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(3600));
        loop {
            interval.tick().await;
            if let Err(error) = cleanup_db.cleanup_expired(chrono::Utc::now().timestamp()) {
                eprintln!("FlashPaste expired-content cleanup failed: {error}");
            }
        }
    });

    // 路由
    let app = build_router(state);

    // 启动服务
    let listener = tokio::net::TcpListener::bind(&bind).await.unwrap();

    println!("FlashPaste listening on {bind}");

    axum::serve(listener, app).await.unwrap();
}

fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/app", get(workspace))
        .route("/health", get(health_check))
        .route("/api/auth/verify", post(verify_token))
        .route("/api/auth/config", get(auth::config))
        .route("/api/auth/login", post(auth::login))
        .route("/api/auth/session", get(auth::current))
        .route("/api/auth/logout", post(auth::logout))
        .route("/api/contents", post(create_content))
        .route(
            "/api/contents/:id",
            get(get_content_metadata)
                .patch(patch_content)
                .delete(delete_content),
        )
        .route("/api/contents/:id/body", get(get_content_body))
        .route("/api/contents/:id/consume", post(get_content_body))
        .route("/api/tokens", post(create_token).get(list_tokens))
        .route("/api/tokens/lookup", get(lookup_token))
        .route("/api/tokens/:id/rotate", post(rotate_token))
        .route("/api/tokens/:id", delete(delete_token))
        .route("/api/dashboard", get(dashboard))
        .route("/v/:id", get(view_content))
        .route("/d/:id", get(download_content).post(download_content))
        .layer(axum::extract::DefaultBodyLimit::max(
            body_store::REQUEST_LIMIT,
        ))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth::authenticate,
        ))
        .layer(middleware::from_fn(security_headers))
        .with_state(state)
}

struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(serde_json::json!({"error": self.1}))).into_response()
    }
}

async fn security_headers(request: Request<axum::body::Body>, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    if !headers.contains_key("Cache-Control") {
        headers.insert("Cache-Control", HeaderValue::from_static("no-store"));
    }
    headers.insert(
        "X-Content-Type-Options",
        HeaderValue::from_static("nosniff"),
    );
    headers.insert("Referrer-Policy", HeaderValue::from_static("no-referrer"));
    headers.insert(
        "X-Robots-Tag",
        HeaderValue::from_static("noindex, nofollow"),
    );
    headers.insert("X-Frame-Options", HeaderValue::from_static("DENY"));
    headers.insert("Content-Security-Policy", HeaderValue::from_static("default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'"));
    response
}

fn shell_etag(html: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(html.as_bytes());
    format!(
        "\"{}\"",
        &hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()[..16]
    )
}

fn shell_response(state: &AppState, headers: &HeaderMap) -> Response {
    let html = pages::inject_config(
        &render_local_app(),
        state.cookie_origin.is_some(),
        state.cookie_origin.is_none(),
    );
    let etag = shell_etag(&html);
    if headers
        .get("if-none-match")
        .and_then(|value| value.to_str().ok())
        == Some(etag.as_str())
    {
        return (
            StatusCode::NOT_MODIFIED,
            [
                ("ETag", etag),
                ("Cache-Control", "public, max-age=60".to_string()),
            ],
        )
            .into_response();
    }
    (
        [
            ("Content-Type", "text/html; charset=utf-8".to_string()),
            ("ETag", etag),
            ("Cache-Control", "public, max-age=60".to_string()),
        ],
        html,
    )
        .into_response()
}

async fn index(State(state): State<AppState>, headers: HeaderMap) -> Response {
    // 检查初始化状态
    let count = state.db.count_tokens_by_level("super").unwrap_or(0)
        + state.db.count_tokens_by_level("user").unwrap_or(0);

    if count == 0 {
        // 首次启动：自动生成 Super Token
        let token_value = generate_token_value();
        let token_hash = hash_token(&token_value);
        let id = db::generate_token_id();
        let now = chrono::Utc::now().timestamp();
        let _ = state
            .db
            .create_token(&id, &token_hash, "Super", "super", None, now);

        return axum::response::Html(pages::bootstrap(&token_value)).into_response();
    }

    shell_response(&state, &headers)
}

fn render_local_app() -> String {
    include_str!("../static/app.html").to_string()
}

async fn workspace(State(state): State<AppState>, headers: HeaderMap) -> Response {
    shell_response(&state, &headers)
}

fn generate_token_value() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    let bytes: [u8; 32] = rng.gen();
    hex::encode(bytes)
}

async fn health_check() -> &'static str {
    "OK"
}

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

async fn verify_token(
    State(state): State<AppState>,
    Json(req): Json<VerifyRequest>,
) -> Result<Json<VerifyResponse>, ApiError> {
    let token_hash = hash_token(&req.token);

    // 尝试查询 token
    let token = match state
        .db
        .get_token_by_hash(&token_hash)
        .map_err(|_| ApiError(StatusCode::INTERNAL_SERVER_ERROR, "Database error".into()))?
    {
        Some(t) => t,
        None => return Err(ApiError(StatusCode::UNAUTHORIZED, "Invalid token".into())),
    };

    Ok(Json(VerifyResponse {
        token_id: token.id,
        name: token.name,
        level: api_token_level(&token.level).to_string(),
    }))
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

#[derive(Serialize)]
struct CreateContentResponse {
    id: String,
    url: Option<String>,
}

async fn create_content(
    State(state): State<AppState>,
    Extension(auth): Extension<auth::RequestAuth>,
    Json(req): Json<CreateContentRequest>,
) -> Result<Response, ApiError> {
    // 强制鉴权
    let token_id = auth
        .require_id()
        .map_err(|_| ApiError(StatusCode::UNAUTHORIZED, "Unauthorized".into()))?;
    let creator = state
        .db
        .get_token(&token_id)
        .ok()
        .flatten()
        .ok_or_else(|| ApiError(StatusCode::UNAUTHORIZED, "Unauthorized".into()))?;
    if creator.level != "super" {
        let limit = creator.quota_bytes.unwrap_or(DEFAULT_MEMBER_QUOTA_BYTES);
        let used = state
            .db
            .active_usage_bytes(&token_id, chrono::Utc::now().timestamp())
            .map_err(|_| ApiError(StatusCode::SERVICE_UNAVAILABLE, "Quota unavailable".into()))?;
        let incoming = req.content.len() as i64;
        if used + incoming > limit {
            return Ok((
                StatusCode::CONFLICT,
                Json(serde_json::json!({
                    "error": "QUOTA_EXCEEDED",
                    "code": "QUOTA_EXCEEDED",
                    "used_bytes": used,
                    "limit_bytes": limit,
                    "incoming_bytes": incoming
                })),
            )
                .into_response());
        }
    }

    // 验证参数
    if !["text", "markdown", "html"].contains(&req.content_type.as_str()) {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "Invalid content_type".into(),
        ));
    }
    if !["public", "token"].contains(&req.visibility.as_str()) {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "Invalid visibility".into(),
        ));
    }
    if req.burn_after_read.unwrap_or(false) && req.visibility != "token" {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "Burn-after-read content must be token protected".into(),
        ));
    }
    if req.visibility == "public"
        && req
            .allowed_tokens
            .as_ref()
            .is_some_and(|ids| !ids.is_empty())
    {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "Public content cannot have token ACLs".into(),
        ));
    }
    if req.content.len() > body_store::limit(&req.content_type) {
        return Err(ApiError(
            StatusCode::PAYLOAD_TOO_LARGE,
            "Content too large (HTML: 10 MiB; text/markdown: 500 KiB)".into(),
        ));
    }
    let valid_ttl = if req.burn_after_read.unwrap_or(false) {
        req.ttl == 604800
    } else {
        [3600, 21600, 86400, 172800].contains(&req.ttl)
    };
    if !valid_ttl {
        return Err(ApiError(StatusCode::BAD_REQUEST, "Invalid ttl".into()));
    }

    let id = db::generate_id();
    let now = chrono::Utc::now().timestamp();
    let expires_at = now + req.ttl;

    // 阅后即焚
    let burn_after_read = if req.burn_after_read.unwrap_or(false) {
        1
    } else {
        0
    };

    state
        .db
        .create_content(
            &id,
            req.title
                .as_deref()
                .map(str::trim)
                .filter(|t| !t.is_empty()),
            &req.content_type,
            &req.content,
            &req.visibility,
            Some(token_id.as_str()),
            now,
            expires_at,
            expires_at,
            burn_after_read,
            req.allowed_tokens.as_deref(),
            Some(&token_id),
        )
        .map_err(|error| match error {
            // Validation errors from the DB layer (duplicate/unknown ACL token, oversized
            // content) are client mistakes, not storage outages.
            rusqlite::Error::InvalidParameterName(_) => {
                ApiError(StatusCode::BAD_REQUEST, "Invalid content or ACL".into())
            }
            rusqlite::Error::SqliteFailure(ref failure, _)
                if failure.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                ApiError(StatusCode::BAD_REQUEST, "Invalid content or ACL".into())
            }
            _ => ApiError(
                StatusCode::SERVICE_UNAVAILABLE,
                "Content storage unavailable".into(),
            ),
        })?;

    let url = if req.visibility == "public" {
        Some(format!("/v/{}", id))
    } else {
        None
    };

    Ok((StatusCode::CREATED, Json(CreateContentResponse { id, url })).into_response())
}

async fn get_content_body(
    State(state): State<AppState>,
    Extension(auth): Extension<auth::RequestAuth>,
    method: Method,
    axum::extract::Path(id): axum::extract::Path<String>,
    axum::extract::Query(params): axum::extract::Query<BodyParams>,
) -> Result<Response, StatusCode> {
    let token_id = auth.token_id.clone();
    if method == Method::POST && token_id.is_none() {
        return Err(StatusCode::UNAUTHORIZED);
    }

    let content = match state.db.get_content(&id) {
        Ok(Some(c)) => c,
        Ok(None) => return Err(StatusCode::NOT_FOUND),
        Err(_) => return Err(StatusCode::INTERNAL_SERVER_ERROR),
    };

    // 检查过期
    let now = chrono::Utc::now().timestamp();
    if content.expires_at <= now {
        return Err(StatusCode::NOT_FOUND);
    }

    // 检查权限
    if !can_view(token_id.as_deref(), &content, &state.db) {
        return Err(StatusCode::NOT_FOUND);
    }
    if method == Method::POST && content.burn_after_read != 1 {
        return Err(StatusCode::BAD_REQUEST);
    }

    // 阅后即焚逻辑：先判断是否应该焚毁，再返回内容
    let mut content = if content.burn_after_read == 1 {
        let consume = method == Method::POST || params.consume.as_deref() != Some("0");
        if method == Method::HEAD {
            return Ok((StatusCode::METHOD_NOT_ALLOWED, [("Allow", "GET, POST")]).into_response());
        }
        if auth.cookie && method != Method::POST && consume {
            return Ok((StatusCode::CONFLICT, Json(serde_json::json!({"error":"CONSUME_REQUIRES_POST","code":"CONSUME_REQUIRES_POST","consume_url":format!("/api/contents/{id}/consume")}))).into_response());
        }
        let is_creator = content.created_by.as_deref() == token_id.as_deref();
        let is_super = token_id
            .as_deref()
            .map(|t| is_super_token(t, &state.db))
            .unwrap_or(false);

        // 焚毁条件：
        // 1. 默认 consume=true 时焚毁
        // 2. 创建者或 super 可以显式传 consume=false 免焚预览
        // 3. 其他授权用户即使传 consume=false 也强制焚毁
        let should_burn = if is_creator || is_super {
            consume // 创建者/super：按 consume 参数决定
        } else {
            if !consume {
                return Err(StatusCode::FORBIDDEN);
            }
            true // 其他用户：强制焚毁
        };

        if should_burn {
            let viewer_id = token_id.as_deref().ok_or(StatusCode::NOT_FOUND)?;
            match state.db.consume_authorized_content(&id, viewer_id, now) {
                Ok(Some(consumed)) => consumed,
                Ok(None) => return Err(StatusCode::NOT_FOUND),
                Err(_) => return Err(StatusCode::INTERNAL_SERVER_ERROR),
            }
        } else {
            content
        }
    } else {
        content
    };

    // Consumed bodies are already hydrated inside the transaction.
    if !(content.burn_after_read == 1
        && (method == Method::POST || params.consume.as_deref() != Some("0")))
    {
        state
            .db
            .hydrate_content(&mut content)
            .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    }

    Ok(Json(ContentBodyResponse {
        content_type: content.content_type,
        content: content.content,
        visibility: content.visibility,
    })
    .into_response())
}

#[derive(Deserialize)]
struct BodyParams {
    consume: Option<String>,
}

#[derive(Serialize)]
struct ContentBodyResponse {
    content_type: String,
    content: String,
    visibility: String,
}

async fn view_content(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Response {
    let content = match state.db.get_content(&id) {
        Ok(Some(c)) => c,
        Ok(None) => return unavailable_content(),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    // 检查过期
    let now = chrono::Utc::now().timestamp();
    if content.expires_at <= now {
        return unavailable_content();
    }

    // 只允许 public
    if content.visibility != "public" {
        return unavailable_content();
    }
    match state.db.has_external_body(&id) {
        Ok(true) => {
            return axum::response::Html(pages::public_external_view(content.title.as_deref(), &id))
                .into_response()
        }
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        Ok(false) => {}
    }

    axum::response::Html(pages::public_view(
        content.title.as_deref(),
        &content.content_type,
        &content.content,
    ))
    .into_response()
}

fn unavailable_content() -> Response {
    (
        StatusCode::NOT_FOUND,
        axum::response::Html(pages::unavailable()),
    )
        .into_response()
}

async fn download_content(
    State(state): State<AppState>,
    Extension(auth): Extension<auth::RequestAuth>,
    method: Method,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Response, StatusCode> {
    let token_id = auth.token_id.clone();
    if method == Method::POST && token_id.is_none() {
        return Err(StatusCode::UNAUTHORIZED);
    }

    let mut content = match state.db.get_content(&id) {
        Ok(Some(c)) => c,
        Ok(None) => return Err(StatusCode::NOT_FOUND),
        Err(_) => return Err(StatusCode::INTERNAL_SERVER_ERROR),
    };

    // 检查过期
    let now = chrono::Utc::now().timestamp();
    if content.expires_at <= now {
        return Err(StatusCode::NOT_FOUND);
    }

    // 检查权限
    if !can_view(token_id.as_deref(), &content, &state.db) {
        return Err(StatusCode::NOT_FOUND);
    }
    if method == Method::POST && content.burn_after_read != 1 {
        return Err(StatusCode::BAD_REQUEST);
    }
    if content.burn_after_read == 1 {
        if method == Method::HEAD || (auth.cookie && method != Method::POST) {
            return Ok((StatusCode::METHOD_NOT_ALLOWED, [("Allow", "POST")]).into_response());
        }
        if content.visibility != "token" {
            return Err(StatusCode::NOT_FOUND);
        }
        let viewer_id = token_id.as_deref().ok_or(StatusCode::NOT_FOUND)?;
        content = state
            .db
            .consume_authorized_content(&id, viewer_id, now)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
            .ok_or(StatusCode::NOT_FOUND)?;
    } else {
        state
            .db
            .hydrate_content(&mut content)
            .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    }

    let ext = match content.content_type.as_str() {
        "markdown" => "md",
        "html" => "html",
        _ => "txt",
    };

    let filename = format!(
        "{}.{}",
        content
            .title
            .unwrap_or_else(|| "download".to_string())
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
            .take(100)
            .collect::<String>(),
        ext
    );

    Ok((
        [(
            "Content-Disposition",
            format!("attachment; filename=\"{}\"", filename),
        )],
        content.content,
    )
        .into_response())
}

#[derive(Deserialize)]
struct CreateTokenRequest {
    name: String,
    level: Option<String>,
    quota_mb: Option<f64>,
}

#[derive(Serialize)]
struct CreateTokenResponse {
    id: String,
    token_id: String,
    name: String,
    level: String,
    token: String,
    token_plaintext: String,
    quota_bytes: Option<i64>,
}

async fn create_token(
    State(state): State<AppState>,
    Extension(auth): Extension<auth::RequestAuth>,
    Json(req): Json<CreateTokenRequest>,
) -> Result<(StatusCode, Json<CreateTokenResponse>), StatusCode> {
    let super_id = auth.require_id()?;

    // Token 生命周期管理仅允许 super。
    if !is_super_token(&super_id, &state.db) {
        return Err(StatusCode::FORBIDDEN);
    }

    let (stored_level, exposed_level) = match req.level.as_deref().unwrap_or("normal") {
        "normal" => ("user", "normal"),
        "super" => ("super", "super"),
        _ => return Err(StatusCode::BAD_REQUEST),
    };
    let name = req.name.trim();
    if name.is_empty() || name.len() > 64 {
        return Err(StatusCode::BAD_REQUEST);
    }

    let quota_bytes = match (stored_level, req.quota_mb) {
        ("super", _) => None,
        (_, None) => None,
        (_, Some(mb)) if mb.is_finite() && (1.0..=10240.0).contains(&mb) => {
            Some((mb * 1024.0 * 1024.0).round() as i64)
        }
        _ => return Err(StatusCode::BAD_REQUEST),
    };

    let token_value = generate_token_value();
    let token_hash = hash_token(&token_value);
    let id = db::generate_token_id();
    let now = chrono::Utc::now().timestamp();

    if state
        .db
        .create_token_with_quota(
            &id,
            &token_hash,
            name,
            stored_level,
            Some(&super_id),
            now,
            quota_bytes,
        )
        .is_err()
    {
        return Err(StatusCode::BAD_REQUEST);
    }

    Ok((
        StatusCode::CREATED,
        Json(CreateTokenResponse {
            id: id.clone(),
            token_id: id,
            name: name.to_string(),
            level: exposed_level.to_string(),
            token: token_value.clone(),
            token_plaintext: token_value,
            quota_bytes,
        }),
    ))
}

#[derive(Deserialize)]
struct LookupTokenQuery {
    name: String,
}

async fn lookup_token(
    State(state): State<AppState>,
    Extension(auth): Extension<auth::RequestAuth>,
    Query(query): Query<LookupTokenQuery>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    auth.require_id()?;
    let token = state
        .db
        .get_token_by_name(&query.name)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(
        serde_json::json!({"id": token.id, "name": token.name}),
    ))
}

async fn rotate_token(
    State(state): State<AppState>,
    Extension(auth): Extension<auth::RequestAuth>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let actor = auth.require_id()?;
    if !is_super_token(&actor, &state.db) {
        return Err(StatusCode::FORBIDDEN);
    }
    if state
        .db
        .get_token(&id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .is_none()
    {
        return Err(StatusCode::NOT_FOUND);
    }
    let token = generate_token_value();
    let hash = hash_token(&token);
    if !state
        .db
        .rotate_token_hash(&id, &hash)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    {
        return Err(StatusCode::NOT_FOUND);
    }
    Ok(Json(
        serde_json::json!({"token_id": id, "token_plaintext": token}),
    ))
}

#[derive(Serialize)]
struct TokenInfo {
    id: String,
    name: String,
    level: String,
    created_at: i64,
    quota_bytes: Option<i64>,
}

#[derive(Serialize)]
struct TokenListResponse {
    tokens: Vec<TokenInfo>,
}

async fn list_tokens(
    State(state): State<AppState>,
    Extension(auth): Extension<auth::RequestAuth>,
) -> Result<Json<TokenListResponse>, StatusCode> {
    let token_id = auth.require_id()?;

    // 检查 super 权限
    if !is_super_token(&token_id, &state.db) {
        return Err(StatusCode::FORBIDDEN);
    }

    let tokens = match state.db.list_tokens() {
        Ok(t) => t,
        Err(_) => return Err(StatusCode::INTERNAL_SERVER_ERROR),
    };

    Ok(Json(TokenListResponse {
        tokens: tokens
            .into_iter()
            .map(|t| TokenInfo {
                id: t.id,
                name: t.name,
                level: api_token_level(&t.level).to_string(),
                created_at: t.created_at,
                quota_bytes: t.quota_bytes,
            })
            .collect(),
    }))
}

async fn delete_token(
    State(state): State<AppState>,
    Extension(auth): Extension<auth::RequestAuth>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let token_id = auth.require_id()?;

    // 检查 super 权限
    if !is_super_token(&token_id, &state.db) {
        return Err(StatusCode::FORBIDDEN);
    }

    match state
        .db
        .delete_token_guarded(&id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    {
        Some(true) => Ok(Json(serde_json::json!({"id": id, "deleted": true}))),
        Some(false) => Err(StatusCode::BAD_REQUEST),
        None => Err(StatusCode::NOT_FOUND),
    }
}

#[derive(Serialize)]
struct DashboardItem {
    id: String,
    title: String,
    content_type: Option<String>,
    visibility: String,
    created_at: i64,
    expires_at: i64,
    burn_after_read: bool,
    created_by: Option<String>,
}

#[derive(Serialize)]
struct DashboardResponse {
    items: Vec<DashboardItem>,
    quota: Option<QuotaInfo>,
}

#[derive(Serialize)]
struct QuotaInfo {
    used_bytes: i64,
    limit_bytes: i64,
}

async fn dashboard(
    State(state): State<AppState>,
    Extension(auth): Extension<auth::RequestAuth>,
) -> Result<Json<DashboardResponse>, StatusCode> {
    let token_id = auth.require_id()?;

    let contents = match state.db.list_dashboard_contents(Some(&token_id)) {
        Ok(c) => c,
        Err(_) => return Err(StatusCode::INTERNAL_SERVER_ERROR),
    };

    let super_user = is_super_token(&token_id, &state.db);
    let quota = if super_user {
        None
    } else {
        let token = state.db.get_token(&token_id).ok().flatten();
        let limit = token
            .as_ref()
            .and_then(|t| t.quota_bytes)
            .unwrap_or(DEFAULT_MEMBER_QUOTA_BYTES);
        let used = state
            .db
            .active_usage_bytes(&token_id, chrono::Utc::now().timestamp())
            .unwrap_or(0);
        Some(QuotaInfo {
            used_bytes: used,
            limit_bytes: limit,
        })
    };
    Ok(Json(DashboardResponse {
        quota,
        items: contents
            .into_iter()
            .map(|c| {
                let can_manage = super_user || c.created_by.as_deref() == Some(token_id.as_str());
                let hidden_burn = c.burn_after_read == 1 && !can_manage;
                DashboardItem {
                    id: c.id,
                    title: if hidden_burn {
                        "[阅后即焚]".to_string()
                    } else {
                        c.title.unwrap_or_default()
                    },
                    content_type: if hidden_burn {
                        None
                    } else {
                        Some(c.content_type)
                    },
                    visibility: c.visibility,
                    created_at: c.created_at,
                    expires_at: c.expires_at,
                    burn_after_read: c.burn_after_read == 1,
                    created_by: c.created_by,
                }
            })
            .collect(),
    }))
}

#[derive(Deserialize)]
struct PatchContentRequest {
    expires_at: Option<i64>,
    visibility: Option<String>,
    allowed_tokens: Option<Vec<String>>,
}

async fn get_content_metadata(
    State(state): State<AppState>,
    Extension(auth): Extension<auth::RequestAuth>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let token_id = auth.require_id()?;
    let content = state
        .db
        .get_content(&id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .filter(|c| c.expires_at > chrono::Utc::now().timestamp())
        .ok_or(StatusCode::NOT_FOUND)?;
    if !can_view(Some(&token_id), &content, &state.db) {
        return Err(StatusCode::NOT_FOUND);
    }
    let can_manage = is_super_token(&token_id, &state.db)
        || content.created_by.as_deref() == Some(token_id.as_str());
    let hidden_burn = content.burn_after_read == 1 && !can_manage;
    let allowed_tokens = if can_manage && content.visibility == "token" {
        Some(
            state
                .db
                .list_access_tokens(&id)
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
        )
    } else {
        None
    };
    Ok(Json(serde_json::json!({
        "id": content.id,
        "title": if hidden_burn { "[阅后即焚]".to_string() } else { content.title.unwrap_or_default() },
        "content_type": if hidden_burn { serde_json::Value::Null } else { serde_json::json!(content.content_type) },
        "visibility": content.visibility,
        "created_at": content.created_at,
        "expires_at": content.expires_at,
        "burn_after_read": content.burn_after_read == 1,
        "created_by": content.created_by,
        "allowed_tokens": allowed_tokens
    })))
}

async fn patch_content(
    State(state): State<AppState>,
    Extension(auth): Extension<auth::RequestAuth>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(patch): Json<PatchContentRequest>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let token_id = auth.require_id()?;
    let content = state
        .db
        .get_content(&id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    if !is_super_token(&token_id, &state.db)
        && content.created_by.as_deref() != Some(token_id.as_str())
    {
        return Err(StatusCode::NOT_FOUND);
    }
    let visibility = patch.visibility.as_deref().unwrap_or(&content.visibility);
    if !["public", "token"].contains(&visibility) {
        return Err(StatusCode::BAD_REQUEST);
    }
    let expires_at = patch.expires_at.unwrap_or(content.expires_at);
    if expires_at > content.expires_at || expires_at < content.created_at {
        return Err(StatusCode::BAD_REQUEST);
    }
    if content.burn_after_read == 1 && visibility != "token" {
        return Err(StatusCode::BAD_REQUEST);
    }
    state
        .db
        .patch_content(
            &id,
            visibility,
            expires_at,
            patch.allowed_tokens.as_deref(),
            &token_id,
            chrono::Utc::now().timestamp(),
        )
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    Ok(Json(serde_json::json!({"id": id, "updated": true})))
}

async fn delete_content(
    State(state): State<AppState>,
    Extension(auth): Extension<auth::RequestAuth>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let token_id = auth.require_id()?;
    let content = state
        .db
        .get_content(&id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    if !is_super_token(&token_id, &state.db)
        && content.created_by.as_deref() != Some(token_id.as_str())
    {
        return Err(StatusCode::NOT_FOUND);
    }
    state
        .db
        .delete_managed_content(&id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(serde_json::json!({"id": id, "deleted": true})))
}

// 辅助函数
pub(crate) const DEFAULT_MEMBER_QUOTA_BYTES: i64 = 100 * 1024 * 1024;

fn hash_token(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    hex::encode(hasher.finalize())
}

fn api_token_level(level: &str) -> &str {
    if level == "user" {
        "normal"
    } else {
        level
    }
}

fn can_view(token_id: Option<&str>, content: &db::Content, db: &db::Database) -> bool {
    // public 内容任何人可看
    if content.visibility == "public" {
        return true;
    }

    // token 保护内容需要检查权限
    if let Some(tid) = token_id {
        // super 可看所有
        if is_super_token(tid, db) {
            return true;
        }

        // 创建者可看
        if content.created_by.as_deref() == Some(tid) {
            return true;
        }

        // 检查 access_control
        if db.has_access_control(&content.id, tid).unwrap_or(false) {
            return true;
        }
    }

    false
}

fn is_super_token(token_id: &str, db: &db::Database) -> bool {
    if let Ok(Some(token)) = db.get_token(token_id) {
        token.level == "super"
    } else {
        false
    }
}

#[cfg(test)]
mod api_tests {
    use super::*;

    #[test]
    fn dashboard_embeds_the_shared_responsive_frontend() {
        let html = render_local_app();
        assert_eq!(html, include_str!("../static/app.html"));
        assert!(html.contains("name=\"viewport\""));
        assert!(html.contains("aria-describedby=\"dialogDescription\""));
        assert!(!html.contains("value=\"admin\""));
        for native_dialog in ["alert(", "confirm(", "prompt("] {
            assert!(!html.contains(native_dialog));
        }
    }
    use axum::{body::to_bytes, http::Request as HttpRequest};
    use tower::ServiceExt;

    fn fixture() -> (Router, String, String, String) {
        let path = std::env::temp_dir().join(format!(
            "flashpaste-api-test-{}-{}.db",
            std::process::id(),
            rand::random::<u64>()
        ));
        let db = db::Database::new(&path.to_string_lossy()).unwrap();
        let now = chrono::Utc::now().timestamp();
        let creator_token = "creator-secret";
        let viewer_token = "viewer-secret";
        let super_token = "super-secret";
        db.create_token(
            "super",
            &hash_token(super_token),
            "super",
            "super",
            None,
            now,
        )
        .unwrap();
        db.create_token(
            "creator",
            &hash_token(creator_token),
            "creator",
            "user",
            None,
            now,
        )
        .unwrap();
        db.create_token(
            "viewer",
            &hash_token(viewer_token),
            "viewer",
            "user",
            None,
            now,
        )
        .unwrap();
        db.create_token_with_quota(
            "quota",
            &hash_token("quota-secret"),
            "quota",
            "user",
            None,
            now,
            Some(1_000_000),
        )
        .unwrap();
        db.create_content(
            "private-id",
            Some("private"),
            "text",
            "private body",
            "token",
            Some("creator"),
            now,
            now + 3600,
            now + 3600,
            0,
            Some(&["viewer".to_string()]),
            Some("creator"),
        )
        .unwrap();
        db.create_content(
            "burn-id",
            Some("burn"),
            "text",
            "single-read",
            "token",
            Some("creator"),
            now,
            now + 3600,
            now + 3600,
            1,
            Some(&["viewer".to_string()]),
            Some("creator"),
        )
        .unwrap();
        db.create_content(
            "public-id",
            Some("<script>title</script>"),
            "html",
            "<script>alert(1)</script>",
            "public",
            Some("creator"),
            now,
            now + 3600,
            now + 3600,
            0,
            None,
            None,
        )
        .unwrap();
        let app = build_router(AppState {
            db: Arc::new(db),
            cookie_origin: None,
        });
        (
            app,
            creator_token.to_string(),
            viewer_token.to_string(),
            super_token.to_string(),
        )
    }

    #[tokio::test]
    async fn member_quota_counts_active_bytes_and_rejects_overflow() {
        let (app, _, _, _) = fixture();
        let payload = |index: usize| {
            serde_json::json!({
                "title": format!("quota-{index}"),
                "content_type": "text",
                "content": "a".repeat(500_000),
                "visibility": "public",
                "ttl": 3600
            })
            .to_string()
        };
        for index in 0..2 {
            let response = app
                .clone()
                .oneshot(
                    HttpRequest::builder()
                        .method("POST")
                        .uri("/api/contents")
                        .header("Authorization", "Bearer quota-secret")
                        .header("Content-Type", "application/json")
                        .body(axum::body::Body::from(payload(index)))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::CREATED,
                "item {index} must fit"
            );
        }
        let rejected = app
            .oneshot(
                HttpRequest::builder()
                    .method("POST")
                    .uri("/api/contents")
                    .header("Authorization", "Bearer quota-secret")
                    .header("Content-Type", "application/json")
                    .body(axum::body::Body::from(payload(2)))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(rejected.status(), StatusCode::CONFLICT);
        let body: serde_json::Value =
            serde_json::from_slice(&to_bytes(rejected.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert_eq!(body["code"], "QUOTA_EXCEEDED");
        assert_eq!(body["used_bytes"], 1_000_000);
        assert_eq!(body["limit_bytes"], 1_000_000);
    }

    #[tokio::test]
    async fn dashboard_is_authenticated_and_private_body_obeys_acl() {
        let (app, _creator, viewer, _) = fixture();
        let anonymous = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .uri("/api/dashboard")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(anonymous.status(), StatusCode::UNAUTHORIZED);

        let denied = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .uri("/api/contents/private-id/body")
                    .header("Authorization", "Bearer not-a-token")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);

        let dashboard = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .uri("/api/dashboard")
                    .header("Authorization", format!("Bearer {viewer}"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(dashboard.status(), StatusCode::OK);
        assert_eq!(
            dashboard.headers().get("Cache-Control").unwrap(),
            "no-store"
        );
        let bytes = to_bytes(dashboard.into_body(), usize::MAX).await.unwrap();
        let rows: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let rows = rows["items"].as_array().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["id"], "private-id");

        let body = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/api/contents/private-id/body")
                    .header("Authorization", format!("Bearer {viewer}"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(body.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn public_view_escapes_title_and_never_executes_embedded_script() {
        let (app, _, _, _) = fixture();
        let response = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/v/public-id")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.headers()["Content-Type"]
            .to_str()
            .unwrap()
            .starts_with("text/html"));
        let html = String::from_utf8(
            to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap();
        assert!(html.contains("sandbox=\"\""));
        assert!(html.contains("&lt;script&gt;title&lt;/script&gt;"));
        assert!(html.contains(
            "default-src &#39;none&#39;; style-src &#39;unsafe-inline&#39;; img-src data: blob:"
        ));
        assert!(html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
        assert!(!html.contains("<script>alert(1)</script>"));
        assert!(
            !html.contains("<h1>&lt;script&gt;title&lt;/script&gt;</h1><script>alert(1)</script>")
        );
    }

    #[tokio::test]
    async fn burn_content_preview_is_restricted_and_first_read_consumes_once() {
        let (app, _, viewer, _) = fixture();
        let preview = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .uri("/api/contents/burn-id/body?consume=0")
                    .header("Authorization", format!("Bearer {viewer}"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(preview.status(), StatusCode::FORBIDDEN);

        let first = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .uri("/api/contents/burn-id/body")
                    .header("Authorization", format!("Bearer {viewer}"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(first.status(), StatusCode::OK);
        let second = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/api/contents/burn-id/body")
                    .header("Authorization", format!("Bearer {viewer}"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(second.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn content_acl_management_and_token_rotation_obey_ownership() {
        let (app, creator, viewer, super_token) = fixture();
        let denied_patch = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .method("PATCH")
                    .uri("/api/contents/private-id")
                    .header("Authorization", format!("Bearer {viewer}"))
                    .header("Content-Type", "application/json")
                    .body(axum::body::Body::from(r#"{"allowed_tokens":[]}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(denied_patch.status(), StatusCode::NOT_FOUND);

        let revoke = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .method("PATCH")
                    .uri("/api/contents/private-id")
                    .header("Authorization", format!("Bearer {creator}"))
                    .header("Content-Type", "application/json")
                    .body(axum::body::Body::from(r#"{"allowed_tokens":[]}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(revoke.status(), StatusCode::OK);
        let no_longer_allowed = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .uri("/api/contents/private-id/body")
                    .header("Authorization", format!("Bearer {viewer}"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(no_longer_allowed.status(), StatusCode::NOT_FOUND);

        let rotate = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .method("POST")
                    .uri("/api/tokens/viewer/rotate")
                    .header("Authorization", format!("Bearer {super_token}"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(rotate.status(), StatusCode::OK);
        let value: serde_json::Value =
            serde_json::from_slice(&to_bytes(rotate.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        let fresh = value["token_plaintext"].as_str().unwrap();
        let old_token = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .method("POST")
                    .uri("/api/auth/verify")
                    .header("Content-Type", "application/json")
                    .body(axum::body::Body::from(r#"{"token":"viewer-secret"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(old_token.status(), StatusCode::UNAUTHORIZED);
        let new_token = app
            .oneshot(
                HttpRequest::builder()
                    .method("POST")
                    .uri("/api/auth/verify")
                    .header("Content-Type", "application/json")
                    .body(axum::body::Body::from(
                        serde_json::json!({"token":fresh}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(new_token.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn concurrent_burn_reads_return_content_to_only_one_request() {
        let (app, _, viewer, _) = fixture();
        let request = || {
            HttpRequest::builder()
                .uri("/api/contents/burn-id/body")
                .header("Authorization", format!("Bearer {viewer}"))
                .body(axum::body::Body::empty())
                .unwrap()
        };
        let (first, second) = tokio::join!(app.clone().oneshot(request()), app.oneshot(request()));
        let statuses = [first.unwrap().status(), second.unwrap().status()];
        assert_eq!(
            statuses
                .iter()
                .filter(|status| **status == StatusCode::OK)
                .count(),
            1
        );
        assert_eq!(
            statuses
                .iter()
                .filter(|status| **status == StatusCode::NOT_FOUND)
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn create_route_preserves_untitled_content_without_using_body_as_title() {
        let (app, creator, _, _) = fixture();
        for content_type in ["text", "markdown", "html"] {
            for supplied_title in [
                None,
                Some(serde_json::Value::Null),
                Some(serde_json::json!("")),
                Some(serde_json::json!(" \t\n ")),
                Some(serde_json::json!("  显式标题  ")),
            ] {
                let expected_title = supplied_title
                    .as_ref()
                    .and_then(|value| value.as_str())
                    .unwrap_or("")
                    .trim()
                    .to_string();
                let mut payload = serde_json::json!({
                    "content_type": content_type,
                    "content": "正文不应出现在标题中",
                    "visibility": "public",
                    "ttl": 3600
                });
                if let Some(title) = supplied_title {
                    payload["title"] = title;
                }
                let created = app
                    .clone()
                    .oneshot(
                        HttpRequest::builder()
                            .method("POST")
                            .uri("/api/contents")
                            .header("Authorization", format!("Bearer {creator}"))
                            .header("Content-Type", "application/json")
                            .body(axum::body::Body::from(payload.to_string()))
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                assert_eq!(created.status(), StatusCode::CREATED);
                let created: serde_json::Value = serde_json::from_slice(
                    &to_bytes(created.into_body(), usize::MAX).await.unwrap(),
                )
                .unwrap();
                let metadata = app
                    .clone()
                    .oneshot(
                        HttpRequest::builder()
                            .uri(format!("/api/contents/{}", created["id"].as_str().unwrap()))
                            .header("Authorization", format!("Bearer {creator}"))
                            .body(axum::body::Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                assert_eq!(metadata.status(), StatusCode::OK);
                let metadata: serde_json::Value = serde_json::from_slice(
                    &to_bytes(metadata.into_body(), usize::MAX).await.unwrap(),
                )
                .unwrap();
                assert_eq!(metadata["title"], expected_title);
            }
        }
    }

    #[tokio::test]
    async fn create_route_enforces_auth_burn_visibility_and_ttl() {
        let (app, creator, _, _) = fixture();
        let anonymous = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .method("POST")
                    .uri("/api/contents")
                    .header("Content-Type", "application/json")
                    .body(axum::body::Body::from(
                        r#"{"content_type":"text","content":"x","visibility":"public","ttl":3600}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(anonymous.status(), StatusCode::UNAUTHORIZED);

        let invalid_burn = app.clone().oneshot(HttpRequest::builder()
            .method("POST").uri("/api/contents").header("Authorization", format!("Bearer {creator}"))
            .header("Content-Type", "application/json")
            .body(axum::body::Body::from(r#"{"content_type":"text","content":"x","visibility":"public","ttl":604800,"burn_after_read":true}"#)).unwrap()).await.unwrap();
        assert_eq!(invalid_burn.status(), StatusCode::BAD_REQUEST);

        let created = app.oneshot(HttpRequest::builder()
            .method("POST").uri("/api/contents").header("Authorization", format!("Bearer {creator}"))
            .header("Content-Type", "application/json")
            .body(axum::body::Body::from(r#"{"content_type":"text","content":"new","visibility":"public","ttl":3600}"#)).unwrap()).await.unwrap();
        assert_eq!(created.status(), StatusCode::CREATED);
    }
}
