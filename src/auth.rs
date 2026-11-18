//! Dashboard password (Argon2id) and session ids.

use anyhow::{Result, anyhow};
use argon2::Argon2;
use argon2::password_hash::phc::PasswordHash;
use argon2::password_hash::{PasswordHasher, PasswordVerifier};

pub fn hash_password(pw: &str) -> Result<String> {
    Ok(Argon2::default()
        .hash_password(pw.as_bytes())
        .map_err(|e| anyhow!("hashing failed: {e}"))?
        .to_string())
}

pub fn verify_password(pw: &str, phc: &str) -> bool {
    PasswordHash::new(phc)
        .is_ok_and(|h| Argon2::default().verify_password(pw.as_bytes(), &h).is_ok())
}

/// 256-bit random session id, hex.
pub fn new_session_id() -> String {
    let bytes: [u8; 32] = rand::random();
    hex::encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_and_verify() {
        let h = hash_password("correct horse").unwrap();
        assert!(h.starts_with("$argon2id$"));
        assert!(verify_password("correct horse", &h));
        assert!(!verify_password("wrong", &h));
        assert!(!verify_password("x", "not a hash"));
        assert_ne!(new_session_id(), new_session_id());
        assert_eq!(new_session_id().len(), 64);
    }
}
