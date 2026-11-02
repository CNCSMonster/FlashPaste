use super::*;
use axum::body::{to_bytes, Body};
use tower::ServiceExt;

const ORIGIN: &str = "https://flashpaste.test";

fn fixture() -> (Router, Arc<db::Database>) {
    let db = Arc::new(db::Database::new(":memory:").unwrap());
    for (id, level) in [
        ("super", "super"),
        ("owner", "user"),
        ("reader", "user"),
        ("other", "user"),
    ] {
        db.create_token(id, &hash_token(&format!("{id}-secret")), id, level, None, 1)
            .unwrap();
    }
    let now = chrono::Utc::now().timestamp();
    for (id, burn) in [("private", 0), ("burn", 1)] {
        db.create_content(
            id,
            None,
            "text",
            "test body",
            "token",
            Some("owner"),
            now,
            now + 3600,
            now + 3600,
            burn,
            Some(&["reader".into()]),
            Some("owner"),
        )
        .unwrap();
    }
    let app = build_router(AppState {
        db: db.clone(),
        cookie_origin: Some(ORIGIN.into()),
    });
    (app, db)
}

async fn send(
    app: &Router,
    method: &str,
    uri: &str,
    cookie: Option<&str>,
    source: Option<&str>,
    bearer: Option<&str>,
    payload: Option<serde_json::Value>,
) -> Response {
    let mut request = Request::builder().method(method).uri(uri);
    if let Some(cookie) = cookie {
        request = request.header("Cookie", cookie);
    }
    if let Some(source) = source {
        request = request.header("Origin", source);
    }
    if let Some(bearer) = bearer {
        request = request.header("Authorization", bearer);
    }
    let body = if let Some(payload) = payload {
        request = request.header("Content-Type", "application/json");
        Body::from(payload.to_string())
    } else {
        Body::empty()
    };
    app.clone()
        .oneshot(request.body(body).unwrap())
        .await
        .unwrap()
}

async fn login(app: &Router, token: &str, remember: bool, previous: Option<&str>) -> Response {
    send(
        app,
        "POST",
        "/api/auth/login",
        previous,
        Some(ORIGIN),
        None,
        Some(serde_json::json!({"token":token,"remember_device":remember})),
    )
    .await
}

fn cookie(response: &Response) -> String {
    response.headers()["Set-Cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .into()
}
async fn json(response: Response) -> serde_json::Value {
    serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap()
}

#[tokio::test]
async fn remembered_and_temporary_sessions_never_return_the_credential() {
    let (app, _) = fixture();
    let response = login(&app, "owner-secret", true, None).await;
    assert_eq!(response.status(), StatusCode::OK);
    let header = response.headers()["Set-Cookie"].to_str().unwrap();
    for attribute in [
        "__Host-flashpaste_session=",
        "Secure",
        "HttpOnly",
        "SameSite=Strict",
        "Path=/",
        "Max-Age=2592000",
    ] {
        assert!(header.contains(attribute));
    }
    assert!(!header.contains("Domain="));
    let remembered = cookie(&response);
    let identity = json(response).await;
    assert_eq!(identity["level"], "normal");
    assert!(!identity.to_string().contains("owner-secret"));
    assert!(!identity
        .to_string()
        .contains(remembered.split('=').nth(1).unwrap()));
    let restored = send(
        &app,
        "GET",
        "/api/auth/session",
        Some(&remembered),
        None,
        None,
        None,
    )
    .await;
    assert_eq!(restored.status(), StatusCode::OK);
    assert_eq!(json(restored).await["token_id"], "owner");
    let temporary = login(&app, "owner-secret", false, None).await;
    assert!(!temporary.headers()["Set-Cookie"]
        .to_str()
        .unwrap()
        .contains("Max-Age"));
    let ttl =
        json(temporary).await["expires_at"].as_i64().unwrap() - chrono::Utc::now().timestamp();
    assert!((session::TEMPORARY_TTL - 2..=session::TEMPORARY_TTL).contains(&ttl));
    let missing = send(&app, "GET", "/api/auth/session", None, None, None, None).await;
    assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);
    assert!(!missing.headers().contains_key("Set-Cookie"));
}

#[tokio::test]
async fn cookie_mutations_require_json_and_exact_trusted_origin() {
    let (app, _) = fixture();
    for source in [
        None,
        Some("null"),
        Some("https://evil.test"),
        Some("https://flashpaste.test:444"),
    ] {
        let response = send(
            &app,
            "POST",
            "/api/auth/login",
            None,
            source,
            None,
            Some(serde_json::json!({"token":"owner-secret"})),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
    let owner = cookie(&login(&app, "owner-secret", true, None).await);
    let body =
        serde_json::json!({"content_type":"text","content":"x","visibility":"public","ttl":3600});
    for source in [
        None,
        Some("null"),
        Some("https://evil.test"),
        Some("https://flashpaste.test:444"),
    ] {
        assert_eq!(
            send(
                &app,
                "POST",
                "/api/contents",
                Some(&owner),
                source,
                None,
                Some(body.clone())
            )
            .await
            .status(),
            StatusCode::FORBIDDEN
        );
    }
    assert_eq!(
        send(
            &app,
            "POST",
            "/api/contents",
            Some(&owner),
            Some(ORIGIN),
            None,
            Some(body.clone())
        )
        .await
        .status(),
        StatusCode::CREATED
    );
    assert_eq!(
        send(
            &app,
            "POST",
            "/api/contents",
            None,
            None,
            Some("Bearer owner-secret"),
            Some(body)
        )
        .await
        .status(),
        StatusCode::CREATED
    );
    assert_eq!(
        send(
            &app,
            "POST",
            "/api/auth/logout",
            Some(&owner),
            Some(ORIGIN),
            None,
            None
        )
        .await
        .status(),
        StatusCode::FORBIDDEN
    );
    let malformed = send(
        &app,
        "GET",
        "/api/dashboard",
        Some(&owner),
        None,
        Some("Bearer invalid"),
        None,
    )
    .await;
    assert_eq!(malformed.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        send(
            &app,
            "GET",
            "/api/dashboard",
            Some(&owner),
            None,
            Some("Basic invalid"),
            None
        )
        .await
        .status(),
        StatusCode::UNAUTHORIZED
    );
    let mixed = send(
        &app,
        "GET",
        "/api/auth/session",
        Some(&owner),
        None,
        Some("Bearer invalid"),
        None,
    )
    .await;
    assert_eq!(mixed.status(), StatusCode::UNAUTHORIZED);
    assert!(!mixed.headers().contains_key("Set-Cookie"));
}

#[tokio::test]
async fn replacement_logout_rotation_and_deletion_revoke_the_right_sessions() {
    let (app, db) = fixture();
    let first = cookie(&login(&app, "owner-secret", true, None).await);
    let other_device = cookie(&login(&app, "owner-secret", true, None).await);
    let failure = login(&app, "wrong", true, Some(&first)).await;
    assert_eq!(failure.status(), StatusCode::UNAUTHORIZED);
    assert!(!failure.headers().contains_key("Set-Cookie"));
    assert_eq!(
        send(
            &app,
            "GET",
            "/api/auth/session",
            Some(&first),
            None,
            None,
            None
        )
        .await
        .status(),
        StatusCode::OK
    );
    let replacement = cookie(&login(&app, "owner-secret", true, Some(&first)).await);
    assert_eq!(
        send(
            &app,
            "GET",
            "/api/auth/session",
            Some(&first),
            None,
            None,
            None
        )
        .await
        .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        send(
            &app,
            "GET",
            "/api/auth/session",
            Some(&other_device),
            None,
            None,
            None
        )
        .await
        .status(),
        StatusCode::OK
    );
    let logged_out = send(
        &app,
        "POST",
        "/api/auth/logout",
        Some(&replacement),
        Some(ORIGIN),
        None,
        Some(serde_json::json!({})),
    )
    .await;
    assert_eq!(logged_out.status(), StatusCode::NO_CONTENT);
    assert!(logged_out.headers()["Set-Cookie"]
        .to_str()
        .unwrap()
        .contains("Max-Age=0"));
    assert_eq!(
        send(
            &app,
            "GET",
            "/api/auth/session",
            Some(&replacement),
            None,
            None,
            None
        )
        .await
        .status(),
        StatusCode::UNAUTHORIZED
    );
    db.rotate_token_hash("owner", &hash_token("new-secret"))
        .unwrap();
    assert_eq!(
        send(
            &app,
            "GET",
            "/api/auth/session",
            Some(&other_device),
            None,
            None,
            None
        )
        .await
        .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        login(&app, "owner-secret", true, None).await.status(),
        StatusCode::UNAUTHORIZED
    );
    let fresh = cookie(&login(&app, "new-secret", true, None).await);
    db.delete_token_guarded("owner").unwrap();
    assert_eq!(
        send(
            &app,
            "GET",
            "/api/auth/session",
            Some(&fresh),
            None,
            None,
            None
        )
        .await
        .status(),
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn cookie_get_head_prefetch_and_private_public_links_cannot_burn() {
    let (app, db) = fixture();
    let owner = cookie(&login(&app, "owner-secret", true, None).await);
    let reader = cookie(&login(&app, "reader-secret", true, None).await);
    let other = cookie(&login(&app, "other-secret", true, None).await);
    assert_eq!(
        send(&app, "GET", "/v/burn", Some(&owner), None, None, None)
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        send(
            &app,
            "GET",
            "/api/contents/burn/body",
            Some(&other),
            None,
            None,
            None
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
    let blocked = send(
        &app,
        "GET",
        "/api/contents/burn/body",
        Some(&reader),
        None,
        None,
        None,
    )
    .await;
    assert_eq!(blocked.status(), StatusCode::CONFLICT);
    assert_eq!(
        json(blocked).await["consume_url"],
        "/api/contents/burn/consume"
    );
    assert_eq!(
        send(
            &app,
            "HEAD",
            "/api/contents/burn/body",
            Some(&reader),
            None,
            None,
            None
        )
        .await
        .status(),
        StatusCode::METHOD_NOT_ALLOWED
    );
    let download = send(&app, "GET", "/d/burn", Some(&reader), None, None, None).await;
    assert_eq!(download.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(download.headers()["Allow"], "POST");
    assert_eq!(
        send(
            &app,
            "GET",
            "/api/contents/burn/body?consume=0",
            Some(&owner),
            None,
            None,
            None
        )
        .await
        .status(),
        StatusCode::OK
    );
    assert_eq!(
        send(
            &app,
            "GET",
            "/api/contents/burn/body?consume=0",
            Some(&reader),
            None,
            None,
            None
        )
        .await
        .status(),
        StatusCode::FORBIDDEN
    );
    assert!(db.get_content("burn").unwrap().is_some());
    assert_eq!(
        send(
            &app,
            "POST",
            "/api/contents/burn/consume",
            Some(&reader),
            Some("null"),
            None,
            Some(serde_json::json!({}))
        )
        .await
        .status(),
        StatusCode::FORBIDDEN
    );
    let (body, download) = tokio::join!(
        send(
            &app,
            "POST",
            "/api/contents/burn/consume",
            Some(&reader),
            Some(ORIGIN),
            None,
            Some(serde_json::json!({}))
        ),
        send(
            &app,
            "POST",
            "/d/burn",
            Some(&reader),
            Some(ORIGIN),
            None,
            Some(serde_json::json!({}))
        )
    );
    assert_eq!(
        [body.status(), download.status()]
            .iter()
            .filter(|status| **status == StatusCode::OK)
            .count(),
        1
    );
    assert!(db.get_content("burn").unwrap().is_none());
}

#[tokio::test]
async fn session_expiry_does_not_depend_on_cron_and_cleanup_preserves_content() {
    let (app, db) = fixture();
    let raw = "b".repeat(64);
    let now = chrono::Utc::now().timestamp();
    db.create_session(
        &hash_token("owner-secret"),
        &session::digest(&raw),
        None,
        now - 2,
        1,
    )
    .unwrap();
    let expired = format!("{}={raw}", session::COOKIE);
    assert_eq!(
        send(
            &app,
            "GET",
            "/api/auth/session",
            Some(&expired),
            None,
            None,
            None
        )
        .await
        .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(db.cleanup_expired(now).unwrap(), 0);
    assert!(db.get_content("private").unwrap().is_some());
    let disabled = build_router(AppState {
        db: db.clone(),
        cookie_origin: None,
    });
    let denied = login(&disabled, "owner-secret", true, None).await;
    assert_eq!(denied.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(!denied.headers().contains_key("Set-Cookie"));
}

#[test]
fn concurrent_login_and_rotation_never_upgrade_a_stale_hash() {
    let (_, db) = fixture();
    let old_hash = hash_token("owner-secret");
    for i in 0..20 {
        db.rotate_token_hash("owner", &old_hash).unwrap();
        let other = db.clone();
        let hash = old_hash.clone();
        let thread = std::thread::spawn(move || {
            other
                .create_session(&hash, &format!("session-{i}"), None, 1, 100)
                .unwrap()
        });
        db.rotate_token_hash("owner", &hash_token("new-secret"))
            .unwrap();
        let _ = thread.join().unwrap();
        assert!(db
            .get_session(&format!("session-{i}"), 2)
            .unwrap()
            .is_none());
    }
}

#[tokio::test]
async fn storage_failure_is_not_misreported_as_logout_or_invalid_credentials() {
    let directory = std::env::temp_dir().join(format!("flashpaste-outage-{}", db::generate_id()));
    std::fs::create_dir(&directory).unwrap();
    let path = directory.join("test.sqlite3");
    let db = Arc::new(db::Database::new(path.to_str().unwrap()).unwrap());
    db.create_token("t", &hash_token("test-secret"), "t", "user", None, 1)
        .unwrap();
    let app = build_router(AppState {
        db: db.clone(),
        cookie_origin: Some(ORIGIN.into()),
    });
    let credential = cookie(&login(&app, "test-secret", true, None).await);
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute("DROP TABLE sessions", [])
        .unwrap();
    for (method, uri, payload) in [
        ("GET", "/api/auth/session", None),
        ("GET", "/api/dashboard", None),
        ("POST", "/api/auth/logout", Some(serde_json::json!({}))),
        (
            "POST",
            "/api/auth/login",
            Some(serde_json::json!({"token":"test-secret","remember_device":true})),
        ),
    ] {
        let response = send(
            &app,
            method,
            uri,
            Some(&credential),
            Some(ORIGIN),
            None,
            payload,
        )
        .await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(!response.headers().contains_key("Set-Cookie"));
    }
    assert_eq!(
        send(
            &app,
            "GET",
            "/api/dashboard",
            None,
            None,
            Some("Bearer test-secret"),
            None
        )
        .await
        .status(),
        StatusCode::OK
    );
    drop(app);
    drop(db);
    std::fs::remove_file(path).unwrap();
    std::fs::remove_dir(directory).unwrap();
}
