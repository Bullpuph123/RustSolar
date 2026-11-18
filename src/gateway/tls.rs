//! TLS for the local gateway. The IQ Gateway presents a certificate issued by an
//! Enphase private CA ("Cloud.HSM.TA") for a `*.enphaseenergy.com` name, so normal
//! web-PKI validation against an IP address cannot succeed. We support:
//!
//! - **Pin** (default): accept only a leaf certificate with a known SHA-256
//!   fingerprint, with full handshake signature checks.
//! - **Ca**: verify against a CA file the user supplies.
//! - **InsecureDev**: accept anything (development only, warned on every start).

use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, WebPkiSupportedAlgorithms};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, SignatureScheme};
use sha2::{Digest, Sha256};

use crate::config::{GatewayConfig, TlsMode};

/// Records the SHA-256 of the last certificate the gateway presented.
pub type SeenCert = Arc<Mutex<Option<[u8; 32]>>>;

pub fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// Parse "AA:BB:..." or "aabb..." into 32 bytes.
pub fn parse_fingerprint(s: &str) -> Option<[u8; 32]> {
    let hexs: String = s
        .chars()
        .filter(|c| !matches!(c, ':' | ' ' | '-'))
        .collect();
    let bytes = hex::decode(hexs).ok()?;
    bytes.try_into().ok()
}

pub fn format_fingerprint(fp: &[u8]) -> String {
    fp.iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

pub fn fingerprint(cert: &CertificateDer<'_>) -> [u8; 32] {
    Sha256::digest(cert.as_ref()).into()
}

#[derive(Debug)]
struct PinnedOrAny {
    /// None = accept any certificate.
    pin: Option<[u8; 32]>,
    algs: WebPkiSupportedAlgorithms,
    /// Records the fingerprint of the last certificate seen (used by `pin-cert`).
    seen: Arc<Mutex<Option<[u8; 32]>>>,
}

impl ServerCertVerifier for PinnedOrAny {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let fp = fingerprint(end_entity);
        if let Ok(mut s) = self.seen.lock() {
            *s = Some(fp);
        }
        match self.pin {
            None => Ok(ServerCertVerified::assertion()),
            Some(p) if p == fp => Ok(ServerCertVerified::assertion()),
            Some(_) => Err(rustls::Error::General(format!(
                "gateway certificate fingerprint {} does not match the pinned value",
                format_fingerprint(&fp)
            ))),
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.algs)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.algs)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algs.supported_schemes()
    }
}

/// Build the rustls config for the gateway. Returns the config and a handle that
/// records the fingerprint of the certificate presented.
pub fn gateway_tls(cfg: &GatewayConfig) -> Result<(ClientConfig, SeenCert)> {
    let prov = provider();
    let seen = Arc::new(Mutex::new(None));
    let builder = ClientConfig::builder_with_provider(prov.clone())
        .with_safe_default_protocol_versions()
        .context("TLS protocol versions")?;
    let config = match cfg.tls {
        TlsMode::Pin => {
            let pin = cfg
                .cert_sha256
                .as_deref()
                .and_then(parse_fingerprint)
                .ok_or_else(|| anyhow!("gateway.tls = \"pin\" but no valid gateway.cert_sha256; run `rustsolar pin-cert`"))?;
            let v = PinnedOrAny {
                pin: Some(pin),
                algs: prov.signature_verification_algorithms,
                seen: seen.clone(),
            };
            builder
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(v))
                .with_no_client_auth()
        }
        TlsMode::InsecureDev => {
            let v = PinnedOrAny {
                pin: None,
                algs: prov.signature_verification_algorithms,
                seen: seen.clone(),
            };
            builder
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(v))
                .with_no_client_auth()
        }
        TlsMode::Ca => {
            let path = cfg
                .ca_cert_path
                .as_ref()
                .ok_or_else(|| anyhow!("gateway.ca_cert_path not set"))?;
            let pem = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
            let mut roots = rustls::RootCertStore::empty();
            use rustls::pki_types::pem::PemObject;
            for c in CertificateDer::pem_slice_iter(&pem) {
                roots
                    .add(c.context("parsing CA PEM")?)
                    .context("adding CA cert")?;
            }
            builder.with_root_certificates(roots).with_no_client_auth()
        }
    };
    Ok((config, seen))
}

/// A config that accepts any certificate and records its fingerprint. Used only by
/// `rustsolar pin-cert` / the setup wizard to show the fingerprint for the user to confirm.
pub fn capture_tls() -> Result<(ClientConfig, SeenCert)> {
    let cfg = GatewayConfig {
        tls: TlsMode::InsecureDev,
        ..Default::default()
    };
    gateway_tls(&cfg)
}

/// TLS config for public internet services (weather): Mozilla roots via webpki-roots.
pub fn public_tls() -> Result<ClientConfig> {
    let roots = rustls::RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    Ok(ClientConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()?
        .with_root_certificates(roots)
        .with_no_client_auth())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_formats() {
        let s = "00:11:22:33:44:55:66:77:88:99:AA:BB:CC:DD:EE:FF:00:11:22:33:44:55:66:77:88:99:AA:BB:CC:DD:EE:FF";
        let fp = parse_fingerprint(s).unwrap();
        assert_eq!(format_fingerprint(&fp), s);
        assert_eq!(
            parse_fingerprint(&s.replace(':', "").to_lowercase()),
            Some(fp)
        );
        assert!(parse_fingerprint("abcd").is_none());
        assert!(parse_fingerprint(&"zz".repeat(32)).is_none());
    }

    #[test]
    fn pin_mode_requires_fingerprint() {
        let cfg = GatewayConfig::default();
        assert!(gateway_tls(&cfg).is_err());
        let ok = GatewayConfig {
            cert_sha256: Some("00".repeat(32)),
            ..Default::default()
        };
        assert!(gateway_tls(&ok).is_ok());
        assert!(public_tls().is_ok());
    }
}
