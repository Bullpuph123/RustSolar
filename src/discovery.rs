//! Local-network discovery: announces the dashboard over multicast DNS (mDNS) so browsers
//! on the home network can open `http://rustsolar.local`. mDNS answers come straight from
//! this machine on the LAN (UDP 5353); no DNS server is involved, so it works whatever DNS
//! the other devices use. Nothing leaves the local network.

use std::net::{IpAddr, SocketAddr, UdpSocket};

use mdns_sd::{IfKind, ServiceDaemon, ServiceInfo};

pub struct Announcer {
    daemon: ServiceDaemon,
    pub url: String,
}

impl Announcer {
    pub fn stop(self) {
        let _ = self.daemon.shutdown();
    }
}

/// Valid mDNS host label: 1–63 of a–z, 0–9 and '-', not starting or ending with '-'.
pub fn valid_name(n: &str) -> bool {
    !n.is_empty()
        && n.len() <= 63
        && !n.starts_with('-')
        && !n.ends_with('-')
        && n.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// The address other LAN devices should use: the bind address if it is specific, otherwise
/// the local address the OS would use to reach the gateway (or any outside address). A UDP
/// "connect" sends no packets; it only picks the route.
pub fn lan_ip(bind: SocketAddr, gateway_host: &str) -> Option<IpAddr> {
    if !bind.ip().is_unspecified() {
        return (!bind.ip().is_loopback()).then_some(bind.ip());
    }
    let probe = |target: &str| -> Option<IpAddr> {
        let s = UdpSocket::bind("0.0.0.0:0").ok()?;
        s.connect(target).ok()?;
        let ip = s.local_addr().ok()?.ip();
        (!ip.is_unspecified() && !ip.is_loopback()).then_some(ip)
    };
    let gw = gateway_host.trim();
    (!gw.is_empty())
        .then(|| probe(&format!("{gw}:443")))
        .flatten()
        .or_else(|| probe("192.0.2.1:9"))
}

/// Start announcing `<name>.local` (A record) and an `_http._tcp` service on `port`.
pub fn start(name: &str, ip: IpAddr, port: u16) -> anyhow::Result<Announcer> {
    let daemon = ServiceDaemon::new()?;
    // Only answer on the LAN interface, not container bridges or VPNs.
    daemon.disable_interface(IfKind::All)?;
    daemon.enable_interface(IfKind::Addr(ip))?;
    let host = format!("{name}.local.");
    let props = [("path", "/"), ("version", env!("CARGO_PKG_VERSION"))];
    let info = ServiceInfo::new(
        "_http._tcp.local.",
        "RustSolar",
        &host,
        ip,
        port,
        &props[..],
    )?;
    daemon.register(info)?;
    let url = if port == 80 {
        format!("http://{name}.local")
    } else {
        format!("http://{name}.local:{port}")
    };
    Ok(Announcer { daemon, url })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert!(valid_name("rustsolar"));
        assert!(valid_name("solar-2"));
        assert!(!valid_name("RustSolar"));
        assert!(!valid_name("-x"));
        assert!(!valid_name("a.b"));
        assert!(!valid_name(""));
    }

    #[test]
    fn specific_bind_is_used() {
        let ip: IpAddr = "192.168.1.20".parse().unwrap();
        assert_eq!(lan_ip(SocketAddr::new(ip, 8085), ""), Some(ip));
        assert_eq!(lan_ip("127.0.0.1:8085".parse().unwrap(), ""), None);
    }
}
