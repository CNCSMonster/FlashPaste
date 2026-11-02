use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const COOKIE: &str = "__Host-flashpaste_session";
pub const TEMPORARY_TTL: i64 = 12 * 3600;
pub const REMEMBER_TTL: i64 = 30 * 86400;

#[derive(Deserialize)]
pub struct LoginRequest {
    pub token: String,
    #[serde(default)]
    pub remember_device: bool,
}

#[derive(Serialize)]
pub struct Identity {
    pub token_id: String,
    pub name: String,
    pub level: String,
    pub expires_at: i64,
}

pub fn digest(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))
}

pub fn origin(value: &str) -> Option<String> {
    let url = url::Url::parse(value).ok()?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    Some(url.origin().ascii_serialization())
}

pub fn csrf(expected: Option<&str>, source: Option<&str>, content_type: Option<&str>) -> bool {
    expected.is_some()
        && expected == source
        && content_type
            .and_then(|value| value.split(';').next())
            .is_some_and(|value| value.trim().eq_ignore_ascii_case("application/json"))
}

// Never interpret an invalid Authorization header as an invitation to use a Cookie.
pub fn bearer(value: &str) -> Option<&str> {
    let (scheme, credential) = value.split_once(' ')?;
    (scheme.eq_ignore_ascii_case("bearer")
        && !credential.is_empty()
        && !credential.chars().any(char::is_whitespace))
    .then_some(credential)
}

pub fn cookie(value: &str) -> Option<&str> {
    let mut matches = value.split(';').filter_map(|part| {
        let (name, value) = part.trim().split_once('=')?;
        (name == COOKIE).then_some(value)
    });
    let value = matches.next()?;
    if matches.next().is_some()
        || value.len() != 64
        || !value.bytes().all(|c| c.is_ascii_hexdigit())
    {
        return None;
    }
    Some(value)
}

pub fn has_cookie(value: &str) -> bool {
    value.split(';').any(|part| {
        part.trim()
            .split_once('=')
            .is_some_and(|(name, _)| name == COOKIE)
    })
}

pub fn set_cookie(value: &str, remember: bool) -> String {
    let mut header = format!("{COOKIE}={value}; Path=/; Secure; HttpOnly; SameSite=Strict");
    if remember {
        header.push_str(&format!("; Max-Age={REMEMBER_TTL}"));
    }
    header
}

pub fn clear_cookie() -> String {
    format!("{COOKIE}=; Path=/; Secure; HttpOnly; SameSite=Strict; Max-Age=0")
}

pub fn ttl(remember: bool) -> i64 {
    if remember {
        REMEMBER_TTL
    } else {
        TEMPORARY_TTL
    }
}

pub fn api_level(value: &str) -> &str {
    if matches!(value, "user" | "admin") {
        "normal"
    } else {
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cookies_origins_and_csrf_are_fail_closed() {
        assert_eq!(
            origin("https://example.test/"),
            Some("https://example.test".into())
        );
        for bad in [
            "http://example.test",
            "https://user@example.test",
            "https://example.test/path",
            "https://example.test/?x=y",
        ] {
            assert!(origin(bad).is_none());
        }
        assert!(csrf(
            Some("https://example.test"),
            Some("https://example.test"),
            Some("application/json; charset=utf-8")
        ));
        for source in [
            None,
            Some("null"),
            Some("https://evil.test"),
            Some("https://example.test:444"),
        ] {
            assert!(!csrf(
                Some("https://example.test"),
                source,
                Some("application/json")
            ));
        }
        let value = "a".repeat(64);
        let header = format!("{COOKIE}={value}");
        assert_eq!(cookie(&header), Some(value.as_str()));
        assert!(cookie(&format!("{header}; {header}")).is_none());
        assert!(!set_cookie(&value, false).contains("Max-Age"));
        assert!(set_cookie(&value, true).contains("Max-Age=2592000"));
        assert!(bearer("Basic abc").is_none());
        assert!(bearer("Bearer ").is_none());
    }
}
