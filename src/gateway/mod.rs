//! Read-only client for the Enphase IQ Gateway local API.
//!
//! Only HTTP GET is ever issued. There is deliberately no code path that sends
//! POST/PUT/DELETE to the gateway: this application never changes battery
//! settings, relays, grid behaviour or streaming state.

pub mod model;
pub mod tls;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use serde::Serialize;
use tokio::sync::RwLock;

use crate::config::GatewayConfig;
use crate::secrets::Token;

/// Endpoints probed by discovery. All are read-only GETs.
pub const DISCOVERY_ENDPOINTS: &[&str] = &[
    "/info",
    "/ivp/livedata/status",
    "/ivp/ensemble/inventory",
    "/ivp/ensemble/status",
    "/ivp/ensemble/power",
    "/ivp/meters",
    "/ivp/meters/readings",
    "/production.json?details=1",
    "/api/v1/production",
    "/api/v1/production/inverters",
];

#[derive(Debug, thiserror::Error)]
pub enum GatewayError {
    #[error("gateway host is not configured")]
    NoHost,
    #[error("no gateway token configured")]
    NoToken,
    #[error("token rejected by the gateway (HTTP {0}); it may have expired")]
    Unauthorized(u16),
    #[error("endpoint not available (HTTP {0})")]
    NotAvailable(u16),
    #[error("gateway returned HTTP {0}")]
    Http(u16),
    #[error("request timed out")]
    Timeout,
    #[error("TLS: {0}")]
    Tls(String),
    #[error("network: {0}")]
    Network(String),
    #[error("response was not valid JSON: {0}")]
    Parse(String),
}

impl GatewayError {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::NoHost => "no_host",
            Self::NoToken => "no_token",
            Self::Unauthorized(_) => "unauthorized",
            Self::NotAvailable(_) => "not_available",
            Self::Http(_) => "http",
            Self::Timeout => "timeout",
            Self::Tls(_) => "tls",
            Self::Network(_) => "network",
            Self::Parse(_) => "parse",
        }
    }
    /// Worth retrying soon (vs. a configuration problem that needs the user).
    pub fn transient(&self) -> bool {
        matches!(self, Self::Timeout | Self::Network(_) | Self::Http(_))
    }
}

#[derive(Debug)]
pub struct Response {
    pub value: serde_json::Value,
    pub raw: String,
    pub latency_ms: u64,
}

#[derive(Clone)]
pub struct GatewayClient {
    http: reqwest::Client,
    base: String,
    token: Arc<RwLock<Option<Token>>>,
    seen_cert: Arc<Mutex<Option<[u8; 32]>>>,
}

impl GatewayClient {
    pub fn new(cfg: &GatewayConfig, token: Option<Token>) -> Result<Self> {
        let (tls, seen) = tls::gateway_tls(cfg)?;
        Self::with_tls(cfg, token, tls, seen)
    }

    /// Accept-any client used only to read the certificate fingerprint for pinning.
    pub fn for_capture(cfg: &GatewayConfig) -> Result<Self> {
        let (tls, seen) = tls::capture_tls()?;
        Self::with_tls(cfg, None, tls, seen)
    }

    fn with_tls(
        cfg: &GatewayConfig,
        token: Option<Token>,
        tls: rustls::ClientConfig,
        seen: Arc<Mutex<Option<[u8; 32]>>>,
    ) -> Result<Self> {
        let http = reqwest::Client::builder()
            .tls_backend_preconfigured(tls)
            .connect_timeout(Duration::from_secs(cfg.connect_timeout_s))
            .timeout(Duration::from_secs(cfg.request_timeout_s))
            .pool_max_idle_per_host(1)
            .pool_idle_timeout(Duration::from_secs(60))
            .user_agent(concat!("RustSolar/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self {
            http,
            base: format!("https://{}", cfg.host),
            token: Arc::new(RwLock::new(token)),
            seen_cert: seen,
        })
    }

    pub async fn set_token(&self, t: Option<Token>) {
        *self.token.write().await = t;
    }

    pub async fn has_token(&self) -> bool {
        self.token.read().await.is_some()
    }

    pub fn seen_fingerprint(&self) -> Option<[u8; 32]> {
        self.seen_cert.lock().ok().and_then(|g| *g)
    }

    /// GET a path and parse the body as JSON (full `serde_json::Value`, which keeps
    /// case-distinct keys like `MqttClient_connect_err` / `MqttClient_connect_Err`
    /// and repeated empty keys without complaint).
    pub async fn get_json(&self, path: &str) -> Result<Response, GatewayError> {
        if self.base == "https://" {
            return Err(GatewayError::NoHost);
        }
        let token = self.token.read().await.clone();
        let mut req = self
            .http
            .get(format!("{}{}", self.base, path))
            .header("Accept", "application/json");
        if let Some(t) = &token {
            req = req.bearer_auth(t.expose());
        }
        let start = Instant::now();
        let resp = req.send().await.map_err(classify)?;
        let status = resp.status().as_u16();
        let body = resp.text().await.map_err(classify)?;
        let latency_ms = start.elapsed().as_millis() as u64;
        match status {
            200 => {}
            401 | 403 if token.is_none() => return Err(GatewayError::NoToken),
            401 | 403 => return Err(GatewayError::Unauthorized(status)),
            404 | 405 | 501 => return Err(GatewayError::NotAvailable(status)),
            s => return Err(GatewayError::Http(s)),
        }
        let value = serde_json::from_str(&body).map_err(|e| GatewayError::Parse(e.to_string()))?;
        Ok(Response {
            value,
            raw: body,
            latency_ms,
        })
    }

    /// Plain GET returning status and the body (for `/info`, which is XML).
    pub async fn get_text(&self, path: &str) -> Result<(u16, String), GatewayError> {
        let resp = self
            .http
            .get(format!("{}{}", self.base, path))
            .send()
            .await
            .map_err(classify)?;
        let status = resp.status().as_u16();
        Ok((status, resp.text().await.map_err(classify)?))
    }
}

fn classify(e: reqwest::Error) -> GatewayError {
    if e.is_timeout() {
        return GatewayError::Timeout;
    }
    // Walk the source chain for a TLS error message.
    let mut src: Option<&dyn std::error::Error> = Some(&e);
    while let Some(s) = src {
        let msg = s.to_string();
        if msg.contains("certificate") || msg.contains("fingerprint") || s.is::<rustls::Error>() {
            return GatewayError::Tls(crate::secrets::redact(&msg));
        }
        src = s.source();
    }
    GatewayError::Network(crate::secrets::redact(&format!("{e}")))
}

#[derive(Debug, Clone, Serialize)]
pub struct DiscoveryResult {
    pub path: String,
    pub ok: bool,
    pub status: String,
    pub latency_ms: Option<u64>,
    pub top_level_keys: Vec<String>,
    pub bytes: usize,
}

/// Probe the read-only endpoint list. Never issues anything but GET.
pub async fn discover(client: &GatewayClient) -> Vec<DiscoveryResult> {
    let mut out = Vec::new();
    for path in DISCOVERY_ENDPOINTS {
        let r = if *path == "/info" {
            match client.get_text(path).await {
                Ok((200, body)) => DiscoveryResult {
                    path: path.to_string(),
                    ok: true,
                    status: "200 (XML)".into(),
                    latency_ms: None,
                    top_level_keys: vec![],
                    bytes: body.len(),
                },
                Ok((s, _)) => DiscoveryResult {
                    path: path.to_string(),
                    ok: false,
                    status: format!("HTTP {s}"),
                    latency_ms: None,
                    top_level_keys: vec![],
                    bytes: 0,
                },
                Err(e) => DiscoveryResult {
                    path: path.to_string(),
                    ok: false,
                    status: e.to_string(),
                    latency_ms: None,
                    top_level_keys: vec![],
                    bytes: 0,
                },
            }
        } else {
            match client.get_json(path).await {
                Ok(resp) => {
                    let keys = match &resp.value {
                        serde_json::Value::Object(m) => m.keys().cloned().collect(),
                        serde_json::Value::Array(a) => vec![format!("[array of {}]", a.len())],
                        _ => vec![],
                    };
                    DiscoveryResult {
                        path: path.to_string(),
                        ok: true,
                        status: "200".into(),
                        latency_ms: Some(resp.latency_ms),
                        top_level_keys: keys,
                        bytes: resp.raw.len(),
                    }
                }
                Err(e) => DiscoveryResult {
                    path: path.to_string(),
                    ok: false,
                    status: e.to_string(),
                    latency_ms: None,
                    top_level_keys: vec![],
                    bytes: 0,
                },
            }
        };
        out.push(r);
        // Be gentle with the gateway's small CPU.
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    out
}

#[cfg(test)]
pub(crate) mod tests {
    //! End-to-end tests against a fake HTTPS gateway with its own self-signed certificate.
    use std::sync::Arc;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;
    use crate::config::{GatewayConfig, TlsMode};

    /// Start a TLS server answering every request with `status` and `body`. Returns (port, leaf DER).
    async fn fake_gateway(status: u16, body: &'static str) -> (u16, Vec<u8>) {
        fake_gateway_fn(move |_| (status, body.to_string())).await
    }

    /// Like `fake_gateway`, but `f(n)` builds the response for the n-th request (0-based).
    pub(crate) async fn fake_gateway_fn(
        f: impl Fn(u64) -> (u16, String) + Send + Sync + 'static,
    ) -> (u16, Vec<u8>) {
        let f = Arc::new(f);
        let counter = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let ck = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
        let der = ck.cert.der().to_vec();
        let key = rustls::pki_types::PrivateKeyDer::Pkcs8(ck.signing_key.serialize_der().into());
        let cfg = rustls::ServerConfig::builder_with_provider(tls::provider())
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(vec![ck.cert.der().clone()], key)
            .unwrap();
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(cfg));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            loop {
                let Ok((sock, _)) = listener.accept().await else {
                    break;
                };
                let acc = acceptor.clone();
                let (f, counter) = (f.clone(), counter.clone());
                tokio::spawn(async move {
                    let Ok(mut s) = acc.accept(sock).await else {
                        return;
                    };
                    let mut buf = vec![0u8; 8192];
                    let n = s.read(&mut buf).await.unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]).to_string();
                    assert!(
                        req.starts_with("GET "),
                        "only GET may reach the gateway: {req}"
                    );
                    let (status, body) =
                        f(counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst));
                    let reason = if status == 200 { "OK" } else { "Error" };
                    let resp = format!(
                        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = s.write_all(resp.as_bytes()).await;
                    let _ = s.shutdown().await;
                });
            }
        });
        (port, der)
    }

    pub(crate) fn cfg(port: u16, pin: Option<String>) -> GatewayConfig {
        GatewayConfig {
            host: format!("127.0.0.1:{port}"),
            tls: TlsMode::Pin,
            cert_sha256: pin,
            connect_timeout_s: 3,
            request_timeout_s: 5,
            ..Default::default()
        }
    }

    pub(crate) fn token() -> crate::secrets::Token {
        crate::secrets::Token::new("eyJhbGciOiJFUzI1NiJ9.eyJleHAiOjF9.c2lnbmF0dXJlc2lnbmF0dXJl")
            .unwrap()
    }

    #[tokio::test]
    async fn pinned_connection_parses_livedata() {
        let (port, der) = fake_gateway(200, model_fixture()).await;
        let pin = tls::format_fingerprint(&tls::fingerprint(&der.into()));
        let c = GatewayClient::new(&cfg(port, Some(pin)), Some(token())).unwrap();
        let r = c
            .get_json("/ivp/livedata/status")
            .await
            .expect("pinned request succeeds");
        let s = model::LiveStatus::from_value(&r.value).unwrap();
        assert!((s.pv_w - 5050.0).abs() < 1e-9);
    }

    #[tokio::test]
    async fn wrong_pin_is_rejected() {
        let (port, _) = fake_gateway(200, "{}").await;
        let c = GatewayClient::new(&cfg(port, Some("00".repeat(32))), Some(token())).unwrap();
        let e = c.get_json("/ivp/livedata/status").await.unwrap_err();
        assert_eq!(e.kind(), "tls", "{e}");
    }

    #[tokio::test]
    async fn unauthorized_and_missing_token() {
        let (port, der) = fake_gateway(401, "{}").await;
        let pin = tls::format_fingerprint(&tls::fingerprint(&der.into()));
        let with = GatewayClient::new(&cfg(port, Some(pin.clone())), Some(token())).unwrap();
        assert_eq!(
            with.get_json("/x").await.unwrap_err().kind(),
            "unauthorized"
        );
        let without = GatewayClient::new(&cfg(port, Some(pin)), None).unwrap();
        assert_eq!(without.get_json("/x").await.unwrap_err().kind(), "no_token");
    }

    #[tokio::test]
    async fn capture_reports_fingerprint() {
        let (port, der) = fake_gateway(200, "{}").await;
        let c = GatewayClient::for_capture(&cfg(port, None)).unwrap();
        let _ = c.get_text("/info").await.unwrap();
        assert_eq!(c.seen_fingerprint(), Some(tls::fingerprint(&der.into())));
    }

    #[tokio::test]
    async fn timeout_is_classified() {
        // A listener that accepts but never answers.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let mut keep = vec![];
            while let Ok((s, _)) = listener.accept().await {
                keep.push(s);
            }
        });
        let mut g = cfg(port, Some("00".repeat(32)));
        g.request_timeout_s = 1;
        let c = GatewayClient::new(&g, Some(token())).unwrap();
        let e = c.get_json("/x").await.unwrap_err();
        assert!(e.transient(), "{e}");
    }

    fn model_fixture() -> &'static str {
        model::tests::FIXTURE
    }
}
