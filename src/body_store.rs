pub const INLINE_LIMIT: usize = 500 * 1024;
pub const HTML_LIMIT: usize = 10 * 1024 * 1024;
pub const REQUEST_LIMIT: usize = 16 * 1024 * 1024;
pub const ORPHAN_GRACE_SECONDS: u64 = 3600;

pub fn limit(content_type: &str) -> usize {
    if content_type == "html" {
        HTML_LIMIT
    } else {
        INLINE_LIMIT
    }
}

pub fn digest(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(bytes))
}

pub fn valid_key(key: &str) -> bool {
    key.strip_prefix("bodies/")
        .is_some_and(|id| id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit()))
}

pub fn verify(bytes: Vec<u8>, size: usize, hash: &str) -> Result<String, String> {
    if size > HTML_LIMIT || bytes.len() != size || digest(&bytes) != hash {
        return Err("Body integrity check failed".into());
    }
    String::from_utf8(bytes).map_err(|_| "Body is not UTF-8".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn limits_keys_and_integrity_are_explicit() {
        assert_eq!(limit("html"), 10485760);
        assert_eq!(limit("markdown"), 512000);
        assert!(valid_key(&format!("bodies/{}", "a".repeat(32))));
        for key in ["bodies/../secret", "/etc/passwd", "bodies/not-an-id"] {
            assert!(!valid_key(key));
        }
        let bytes = b"original".to_vec();
        let hash = digest(&bytes);
        assert_eq!(
            verify(bytes.clone(), bytes.len(), &hash).unwrap(),
            "original"
        );
        assert!(verify(b"changed".to_vec(), bytes.len(), &hash).is_err());
        assert!(verify(vec![255], 1, &digest(&[255])).is_err());
    }
}
