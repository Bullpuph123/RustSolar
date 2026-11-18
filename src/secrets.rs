//! Gateway token handling. The token is read from `RUSTSOLAR_GATEWAY_TOKEN` or a
//! token file outside the source tree, is never logged, never sent to the browser,
//! and never sent anywhere except the configured local gateway.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use base64::Engine;
use serde::Serialize;

pub const TOKEN_ENV: &str = "RUSTSOLAR_GATEWAY_TOKEN";

/// A secret string whose Debug/Display never reveal the value.
#[derive(Clone)]
pub struct Token(String);

impl Token {
    pub fn new(s: impl Into<String>) -> Result<Self> {
        let s: String = s
            .into()
            .trim()
            .trim_start_matches("Bearer ")
            .trim()
            .to_string();
        if s.len() < 20 || s.chars().any(|c| c.is_whitespace() || c.is_control()) {
            bail!("token looks malformed (too short or contains spaces)");
        }
        Ok(Self(s))
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
    pub fn info(&self) -> TokenInfo {
        TokenInfo::from_jwt(&self.0)
    }
}

impl std::fmt::Debug for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Token(<redacted>)")
    }
}

/// Non-secret facts decoded from the JWT payload (signature is not verified; the gateway does that).
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct TokenInfo {
    pub expires_at: Option<i64>,
    pub issued_at: Option<i64>,
    pub role: Option<String>,
    pub serial: Option<String>,
}

impl TokenInfo {
    pub fn from_jwt(jwt: &str) -> Self {
        let Some(payload) = jwt.split('.').nth(1) else {
            return Self::default();
        };
        let engine = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let Ok(bytes) = engine.decode(payload.trim_end_matches('=')) else {
            return Self::default();
        };
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            return Self::default();
        };
        Self {
            expires_at: v.get("exp").and_then(|x| x.as_i64()),
            issued_at: v.get("iat").and_then(|x| x.as_i64()),
            role: v
                .get("enphaseUser")
                .and_then(|x| x.as_str())
                .map(String::from),
            serial: v.get("aud").and_then(|x| x.as_str()).map(String::from),
        }
    }
    pub fn days_left(&self, now: i64) -> Option<f64> {
        self.expires_at.map(|e| (e - now) as f64 / 86400.0)
    }
}

pub fn token_path(configured: Option<&Path>, data_dir: &Path) -> PathBuf {
    configured
        .map(Path::to_path_buf)
        .unwrap_or_else(|| data_dir.join("gateway.token"))
}

/// Load the token: env var first, then file. `Ok(None)` when neither is set.
pub fn load_token(path: &Path) -> Result<Option<Token>> {
    if let Ok(v) = std::env::var(TOKEN_ENV)
        && !v.trim().is_empty()
    {
        return Token::new(v)
            .map(Some)
            .context(format!("{TOKEN_ENV} is set but invalid"));
    }
    if !path.exists() {
        return Ok(None);
    }
    warn_if_world_readable(path);
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading token file {}", path.display()))?;
    Token::new(text)
        .map(Some)
        .with_context(|| format!("token file {} is invalid", path.display()))
}

/// Write the token file with owner-only permissions where the OS supports it.
pub fn save_token(path: &Path, token: &Token) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, token.expose())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(unix)]
fn warn_if_world_readable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(m) = std::fs::metadata(path)
        && m.permissions().mode() & 0o077 != 0
    {
        tracing::warn!(
            "token file {} is readable by other users; run: chmod 600 {}",
            path.display(),
            path.display()
        );
    }
}
#[cfg(not(unix))]
fn warn_if_world_readable(_path: &Path) {}

/// Replace anything that looks like a bearer token or JWT in a string.
pub fn redact(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for word in s.split_inclusive(|c: char| c.is_whitespace() || c == '"' || c == '\'') {
        let core = word.trim_end_matches(|c: char| c.is_whitespace() || c == '"' || c == '\'');
        let tail = &word[core.len()..];
        let looks_jwt =
            core.len() > 40 && core.matches('.').count() == 2 && core.starts_with("eyJ");
        if looks_jwt {
            out.push_str("<redacted>");
            out.push_str(tail);
        } else {
            out.push_str(word);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_jwt(payload: &str) -> String {
        let e = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        format!(
            "eyJhbGciOiJFUzI1NiJ9.{}.c2lnbmF0dXJlc2lnbmF0dXJl",
            e.encode(payload)
        )
    }

    #[test]
    fn decodes_expiry_and_hides_value() {
        let t = Token::new(fake_jwt(
            r#"{"aud":"123456789012","exp":1800000000,"iat":1768464000,"enphaseUser":"owner"}"#,
        ))
        .unwrap();
        let i = t.info();
        assert_eq!(i.expires_at, Some(1_800_000_000));
        assert_eq!(i.role.as_deref(), Some("owner"));
        assert_eq!(format!("{t:?}"), "Token(<redacted>)");
        assert!((i.days_left(1_800_000_000 - 86400).unwrap() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn strips_bearer_prefix_and_rejects_junk() {
        let j = fake_jwt(r#"{"exp":1}"#);
        assert_eq!(Token::new(format!("Bearer {j}\n")).unwrap().expose(), j);
        assert!(Token::new("short").is_err());
        assert!(Token::new("has spaces in the middle of it ok").is_err());
    }

    #[test]
    fn redacts_jwts() {
        let j = fake_jwt(r#"{"exp":1,"aud":"x"}"#);
        let s = format!("Authorization: Bearer {j} rest");
        let r = redact(&s);
        assert!(!r.contains(&j));
        assert!(r.contains("<redacted>"));
        assert!(r.ends_with(" rest"));
    }

    #[test]
    fn save_and_load_file() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("gateway.token");
        let t = Token::new(fake_jwt(r#"{"exp":5}"#)).unwrap();
        save_token(&p, &t).unwrap();
        // Env var may be set on a dev machine; only check the file path logic when it isn't.
        if std::env::var(TOKEN_ENV).is_err() {
            assert_eq!(load_token(&p).unwrap().unwrap().expose(), t.expose());
        }
    }
}
