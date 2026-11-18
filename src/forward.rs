//! Host-name forwarding: lets another dashboard on this machine (e.g. RustSprinkler on
//! port 8086) share RustSolar's port, so `http://rustsprinkler.local` needs no port number.
//! A request whose Host header matches a `[[server.forward]]` entry is relayed unchanged to
//! that entry's address; everything else is handled by RustSolar as usual. Responses are
//! streamed, so live event streams work. Forwarded requests skip RustSolar's own login:
//! the other dashboard applies its own access rules.

use std::net::SocketAddr;
use std::sync::OnceLock;
use std::time::Duration;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderName, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::state::Shared;

const MAX_BODY: usize = 4 * 1024 * 1024;

/// Host header → lowercase name without the port.
pub fn host_name(host: &str) -> String {
    let name = match host.rsplit_once(':') {
        Some((n, port)) if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => n,
        _ => host,
    };
    name.trim_end_matches('.').to_ascii_lowercase()
}

/// A forwarded name: lowercase letters, digits, '-' and '.', e.g. `rustsprinkler.local`.
pub fn valid_host(h: &str) -> bool {
    !h.is_empty()
        && h.len() <= 253
        && h.split('.').all(|l| {
            !l.is_empty()
                && l.len() <= 63
                && !l.starts_with('-')
                && !l.ends_with('-')
                && l.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
}

/// Headers that belong to one connection and must not be relayed.
fn hop_by_hop(name: &HeaderName) -> bool {
    matches!(
        name.as_str(),
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
            | "content-length"
    )
}

fn client() -> Option<&'static reqwest::Client> {
    static CLIENT: OnceLock<Option<reqwest::Client>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            // No overall timeout: event streams stay open for hours.
            reqwest::Client::builder()
                .tls_backend_preconfigured(crate::gateway::tls::public_tls().ok()?)
                .connect_timeout(Duration::from_secs(3))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .ok()
        })
        .as_ref()
}

fn target(state: &Shared, headers: &HeaderMap) -> Option<SocketAddr> {
    let cfg = state.cfg();
    if cfg.server.forward.is_empty() {
        return None;
    }
    let host = host_name(headers.get(header::HOST)?.to_str().ok()?);
    cfg.server
        .forward
        .iter()
        .find(|f| f.host == host)
        .map(|f| f.to)
}

pub async fn forward(State(state): State<Shared>, req: Request, next: Next) -> Response {
    let Some(to) = target(&state, req.headers()) else {
        return next.run(req).await;
    };
    match relay(to, req).await {
        Ok(r) => r,
        Err(msg) => (StatusCode::BAD_GATEWAY, msg).into_response(),
    }
}

async fn relay(to: SocketAddr, req: Request) -> Result<Response, String> {
    let client = client().ok_or("forwarding is unavailable")?;
    let (parts, body) = req.into_parts();
    let path = parts
        .uri
        .path_and_query()
        .map_or("/", |p| p.as_str())
        .to_string();
    let body = axum::body::to_bytes(body, MAX_BODY)
        .await
        .map_err(|_| "request too large to forward".to_string())?;
    let mut out = client.request(parts.method, format!("http://{to}{path}"));
    // Host is passed through unchanged, so the other dashboard sees the name the browser used.
    for (k, v) in &parts.headers {
        if !hop_by_hop(k) {
            out = out.header(k, v);
        }
    }
    let resp = out.body(body).send().await.map_err(|_| {
        format!("The dashboard for this address is not answering on {to}. Is its service running?")
    })?;
    let mut builder = Response::builder().status(resp.status());
    for (k, v) in resp.headers() {
        if !hop_by_hop(k) {
            builder = builder.header(k, v);
        }
    }
    builder
        .body(Body::from_stream(resp.bytes_stream()))
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_names() {
        assert_eq!(host_name("RustSprinkler.local"), "rustsprinkler.local");
        assert_eq!(host_name("rustsprinkler.local:80"), "rustsprinkler.local");
        assert_eq!(host_name("rustsprinkler.local."), "rustsprinkler.local");
        assert_eq!(host_name("192.168.1.20:8085"), "192.168.1.20");
        assert!(valid_host("rustsprinkler.local"));
        assert!(valid_host("sprinkler-2.local"));
        for bad in ["", "Rust.local", "a..b", "-a.local", "a b", "a/b", "a:80"] {
            assert!(!valid_host(bad), "{bad}");
        }
    }

    #[test]
    fn connection_headers_are_not_relayed() {
        assert!(hop_by_hop(&header::CONNECTION));
        assert!(hop_by_hop(&header::TRANSFER_ENCODING));
        assert!(!hop_by_hop(&header::HOST));
        assert!(!hop_by_hop(&header::COOKIE));
        assert!(!hop_by_hop(&header::CONTENT_TYPE));
    }
}
