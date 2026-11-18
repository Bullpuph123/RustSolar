//! TOML configuration. Loaded once at start; the setup wizard rewrites the file and
//! the server restarts its workers. The gateway token is never stored here.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rustsolar_core::pvmodel::ArraySpec;
use rustsolar_core::tariff::TariffConfig;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    pub server: ServerConfig,
    pub gateway: GatewayConfig,
    pub collector: CollectorConfig,
    pub location: Option<Location>,
    pub arrays: Vec<ArraySpec>,
    pub battery: BatteryConfig,
    pub weather: WeatherConfig,
    pub tariff: TariffConfig,
    pub alerts: AlertConfig,
    pub retention: RetentionConfig,
    pub backup: BackupConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ServerConfig {
    /// Address to listen on. Loopback by default; set "0.0.0.0:8080" for the LAN.
    pub bind: SocketAddr,
    /// Argon2 hash of the dashboard password (`rustsolar hash-password`). Required
    /// when `bind` is not a loopback address unless `allow_unauthenticated_lan = true`.
    pub password_hash: Option<String>,
    pub allow_unauthenticated_lan: bool,
    /// Days a login stays valid.
    pub session_days: u32,
    /// Announce the dashboard on the local network as `http://<mdns_name>.local` (mDNS).
    pub mdns: bool,
    pub mdns_name: String,
    /// Also listen on port 80, so the address needs no port number. Skipped with a warning
    /// if port 80 is taken.
    pub port80: bool,
    /// Other dashboards on this machine that share this server's port by host name
    /// (see `forward.rs`), e.g. `rustsprinkler.local` → `127.0.0.1:8086`.
    pub forward: Vec<Forward>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Forward {
    /// Host name the browser uses, lowercase, without a port.
    pub host: String,
    /// Where that dashboard listens.
    pub to: SocketAddr,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: "127.0.0.1:8080".parse().expect("valid default"),
            password_hash: None,
            allow_unauthenticated_lan: false,
            session_days: 30,
            mdns: true,
            mdns_name: "rustsolar".into(),
            port80: false,
            forward: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TlsMode {
    /// Pin the gateway certificate by SHA-256 fingerprint (recommended).
    #[default]
    Pin,
    /// Verify against a CA certificate file (`ca_cert_path`).
    Ca,
    /// DEVELOPMENT ONLY: accept any certificate. Logged as a warning every start.
    InsecureDev,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct GatewayConfig {
    pub host: String,
    pub tls: TlsMode,
    /// SHA-256 of the gateway's leaf certificate (hex, colons optional). Set with `rustsolar pin-cert`.
    pub cert_sha256: Option<String>,
    pub ca_cert_path: Option<PathBuf>,
    /// Token file path. Default: `<data_dir>/gateway.token`. The env var
    /// `RUSTSOLAR_GATEWAY_TOKEN` overrides the file.
    pub token_file: Option<PathBuf>,
    pub connect_timeout_s: u64,
    pub request_timeout_s: u64,
    /// Keep sanitized raw responses in the database (debugging). Off by default.
    pub store_raw: bool,
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            host: String::new(),
            tls: TlsMode::Pin,
            cert_sha256: None,
            ca_cert_path: None,
            token_file: None,
            connect_timeout_s: 5,
            request_timeout_s: 10,
            store_raw: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct CollectorConfig {
    pub enabled: bool,
    pub interval_s: u64,
    /// Readings older than this count as stale for calculations (forecast inputs,
    /// recommendations).
    pub stale_after_s: u64,
    /// Minutes without a new reading before the dashboard and alerts warn about it, so a
    /// single missed poll stays quiet. 0 = warn as soon as data is stale.
    pub warn_after_min: u32,
    /// Gaps longer than this are not integrated into energy totals.
    pub max_integration_gap_s: i64,
    pub max_backoff_s: u64,
    /// Read-only endpoints polled less often for inventory/lifetime data.
    pub inventory_interval_s: u64,
}

impl CollectorConfig {
    /// Seconds without a new reading before warning (see `warn_after_min`).
    pub fn warn_after_s(&self) -> i64 {
        if self.warn_after_min == 0 {
            self.stale_after_s as i64
        } else {
            self.warn_after_min as i64 * 60
        }
    }
}

impl Default for CollectorConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            interval_s: 12,
            stale_after_s: 60,
            warn_after_min: 5,
            max_integration_gap_s: 120,
            max_backoff_s: 300,
            inventory_interval_s: 3600,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Location {
    pub latitude: f64,
    pub longitude: f64,
    pub elevation_m: Option<f64>,
    /// IANA time zone, e.g. "America/Los_Angeles".
    pub time_zone: String,
}

impl Location {
    pub fn tz(&self) -> chrono_tz::Tz {
        self.time_zone.parse().unwrap_or(chrono_tz::UTC)
    }

    /// Plausibility checks that don't block saving: a coordinate whose sign doesn't fit the
    /// time zone (the usual mistake is a missing minus sign on a western longitude).
    pub fn warnings(&self) -> Vec<String> {
        use chrono::{Datelike, NaiveDate, Offset, TimeZone};
        let mut w = Vec::new();
        let Ok(tz) = self.time_zone.parse::<chrono_tz::Tz>() else {
            return w;
        };
        let (lat, lon) = (self.latitude, self.longitude);
        if lat == 0.0 && lon == 0.0 {
            w.push("Location is 0, 0 (in the ocean off Africa). Enter your home's latitude and longitude.".into());
            return w;
        }
        let year = chrono::Utc::now().year();
        let offset_h = |month: u32| {
            let at = NaiveDate::from_ymd_opt(year, month, 15)
                .and_then(|d| d.and_hms_opt(12, 0, 0))
                .expect("valid date");
            tz.from_utc_datetime(&at).offset().fix().local_minus_utc() as f64 / 3600.0
        };
        let (jan, jul) = (offset_h(1), offset_h(7));
        // Standard time is roughly 15° of longitude per hour from Greenwich.
        let expected = jan.min(jul) * 15.0;
        let dist = |a: f64, b: f64| {
            let d = (a - b).rem_euclid(360.0);
            d.min(360.0 - d)
        };
        if dist(lon, expected) > 40.0 && dist(-lon, expected) <= 25.0 {
            let side = if expected < 0.0 {
                "west of Greenwich, so its longitude should be negative"
            } else {
                "east of Greenwich, so its longitude should be positive"
            };
            w.push(format!(
                "Longitude {lon} looks like it has the wrong sign: time zone {tz} is {side} (about {:.0}). Try {}.",
                expected,
                -lon
            ));
        } else if dist(lon, expected) > 50.0 {
            w.push(format!(
                "Longitude {lon} is far from time zone {tz} (about {expected:.0}). Check the longitude and the time zone."
            ));
        }
        if jan != jul {
            let northern = jul > jan; // summer time in July
            if northern && lat < 0.0 {
                w.push(format!("Latitude {lat} is south of the equator, but {tz} is a northern-hemisphere time zone. Try {}.", -lat));
            } else if !northern && lat > 0.0 {
                w.push(format!("Latitude {lat} is north of the equator, but {tz} is a southern-hemisphere time zone. Try {}.", -lat));
            }
        }
        w
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct BatteryConfig {
    /// Usable energy, kWh. None = derive from gateway (inventory or stored-energy/SOC).
    pub usable_kwh: Option<f64>,
    /// Max continuous charge/discharge power, kW. None = derive from inventory count.
    pub max_power_kw: Option<f64>,
    /// AC round-trip efficiency used until enough history exists. Default estimate 0.89
    /// (Enphase IQ Battery 5P datasheet: 90 % round-trip, rounded down for AC losses).
    pub round_trip_eff: f64,
    /// Lowest SOC reachable during a grid outage, %.
    pub outage_floor_pct: f64,
    /// Essential-load scenario for outage planning, kW (user estimate).
    pub essential_load_kw: f64,
}

impl Default for BatteryConfig {
    fn default() -> Self {
        Self {
            usable_kwh: None,
            max_power_kw: None,
            round_trip_eff: 0.89,
            outage_floor_pct: 0.0,
            essential_load_kw: 0.5,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct WeatherConfig {
    pub enabled: bool,
    /// Minutes between forecast refreshes (Open-Meteo updates models hourly).
    pub refresh_min: u64,
    pub air_quality: bool,
    /// Hard cap on outgoing weather requests per day (Open-Meteo free limit is 10,000).
    pub daily_request_budget: u32,
    pub forecast_days: u32,
}

impl Default for WeatherConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            refresh_min: 60,
            air_quality: true,
            daily_request_budget: 200,
            forecast_days: 7,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct AlertConfig {
    pub low_soc_pct: f64,
    /// Underperformance alert when solar is this fraction below expectation for an hour.
    pub underperformance_frac: f64,
    /// High-load alert when the hour's load exceeds typical by this factor.
    pub high_load_factor: f64,
    /// Local hours [start, end) during which notifications are not pushed (still recorded).
    pub quiet_start_hour: u32,
    pub quiet_end_hour: u32,
    /// Minimum minutes before the same alert can fire again.
    pub repeat_after_min: i64,
    pub webhook_url: Option<String>,
}

impl Default for AlertConfig {
    fn default() -> Self {
        Self {
            low_soc_pct: 25.0,
            underperformance_frac: 0.25,
            high_load_factor: 2.0,
            quiet_start_hour: 22,
            quiet_end_hour: 7,
            repeat_after_min: 240,
            webhook_url: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct RetentionConfig {
    /// Raw samples (one per poll) are kept this many days; hourly energy is kept forever.
    pub raw_days: u32,
    /// 5-minute aggregates kept this many days.
    pub five_min_days: u32,
    pub diagnostics_days: u32,
}

impl Default for RetentionConfig {
    fn default() -> Self {
        Self {
            raw_days: 90,
            five_min_days: 1095,
            diagnostics_days: 30,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct BackupConfig {
    pub enabled: bool,
    pub dir: Option<PathBuf>,
    /// Local hour to run the daily backup.
    pub hour: u32,
    pub keep: usize,
}

impl Default for BackupConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            dir: None,
            hour: 3,
            keep: 14,
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let cfg: Config =
            toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        Ok(cfg)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let text = format!(
            "# RustSolar configuration. Written by the setup wizard; safe to edit by hand.\n\
             # The gateway token is NOT stored here (see gateway.token_file).\n\n{}",
            toml::to_string_pretty(self)?
        );
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Things that look wrong but don't stop the config from being saved.
    pub fn warnings(&self) -> Vec<String> {
        self.location
            .as_ref()
            .map(Location::warnings)
            .unwrap_or_default()
    }

    /// All problems found; empty means valid. Missing optional sections are not errors.
    pub fn validate(&self) -> Vec<String> {
        let mut e = Vec::new();
        let c = &self.collector;
        if !(5..=300).contains(&c.interval_s) {
            e.push("collector.interval_s must be 5–300 seconds".into());
        }
        if c.warn_after_min > 1440 {
            e.push("collector.warn_after_min must be 0–1440 minutes".into());
        }
        if c.stale_after_s < c.interval_s * 2 {
            e.push("collector.stale_after_s must be at least twice interval_s".into());
        }
        if c.max_integration_gap_s < c.interval_s as i64 * 2 {
            e.push("collector.max_integration_gap_s must be at least twice interval_s".into());
        }
        let g = &self.gateway;
        if g.connect_timeout_s == 0 || g.request_timeout_s == 0 || g.request_timeout_s > 120 {
            e.push("gateway timeouts must be 1–120 s".into());
        }
        if g.tls == TlsMode::Ca && g.ca_cert_path.is_none() {
            e.push("gateway.tls = \"ca\" needs gateway.ca_cert_path".into());
        }
        if let Some(fp) = &g.cert_sha256
            && crate::gateway::tls::parse_fingerprint(fp).is_none()
        {
            e.push("gateway.cert_sha256 must be 64 hex digits (colons allowed)".into());
        }
        if !g.host.is_empty() && (g.host.contains('/') || g.host.contains(' ')) {
            e.push("gateway.host must be a bare IP address or hostname".into());
        }
        if let Some(l) = &self.location {
            if !(-90.0..=90.0).contains(&l.latitude) || !(-180.0..=180.0).contains(&l.longitude) {
                e.push("location latitude/longitude out of range".into());
            }
            if l.time_zone.parse::<chrono_tz::Tz>().is_err() {
                e.push(format!(
                    "location.time_zone \"{}\" is not an IANA time zone",
                    l.time_zone
                ));
            }
        }
        for a in &self.arrays {
            if let Err(m) = a.validate() {
                e.push(format!("array \"{}\": {m}", a.name));
            }
        }
        let b = &self.battery;
        if let Some(k) = b.usable_kwh
            && !(0.5..=500.0).contains(&k)
        {
            e.push("battery.usable_kwh must be 0.5–500".into());
        }
        if let Some(k) = b.max_power_kw
            && !(0.1..=200.0).contains(&k)
        {
            e.push("battery.max_power_kw must be 0.1–200".into());
        }
        if !(0.5..=1.0).contains(&b.round_trip_eff) {
            e.push("battery.round_trip_eff must be 0.5–1.0".into());
        }
        if !(0.0..=100.0).contains(&b.outage_floor_pct) {
            e.push("battery.outage_floor_pct must be 0–100".into());
        }
        let w = &self.weather;
        if w.refresh_min < 15 {
            e.push("weather.refresh_min must be at least 15".into());
        }
        if !(1..=16).contains(&w.forecast_days) {
            e.push("weather.forecast_days must be 1–16".into());
        }
        if w.daily_request_budget > 5000 {
            e.push(
                "weather.daily_request_budget must stay well under the free limit (≤ 5000)".into(),
            );
        }
        if self.tariff.enabled
            && let Err(errs) = self.tariff.validate()
        {
            e.extend(errs.into_iter().map(|m| format!("tariff: {m}")));
        }
        let a = &self.alerts;
        if a.quiet_start_hour > 23 || a.quiet_end_hour > 23 {
            e.push("alerts quiet hours must be 0–23".into());
        }
        if let Some(u) = &a.webhook_url
            && !(u.starts_with("http://") || u.starts_with("https://"))
        {
            e.push("alerts.webhook_url must start with http:// or https://".into());
        }
        if self.retention.raw_days < 2 {
            e.push("retention.raw_days must be at least 2".into());
        }
        let s = &self.server;
        for f in &s.forward {
            if !crate::forward::valid_host(&f.host) {
                e.push(format!(
                    "server.forward host '{}' must be a lowercase name such as rustsprinkler.local",
                    f.host
                ));
            }
            if f.host == format!("{}.local", s.mdns_name) {
                e.push(format!(
                    "server.forward host '{}' is RustSolar's own address",
                    f.host
                ));
            }
            if f.to.port() == s.bind.port() || (s.port80 && f.to.port() == 80) {
                e.push(format!(
                    "server.forward '{}' points back at RustSolar itself",
                    f.host
                ));
            }
        }
        if !crate::discovery::valid_name(&s.mdns_name) {
            e.push("server.mdns_name must be 1-63 lowercase letters, digits or '-'".into());
        }
        if !s.bind.ip().is_loopback() && s.password_hash.is_none() && !s.allow_unauthenticated_lan {
            e.push(
                "server.bind is reachable from the network but no password is set: run `rustsolar hash-password` \
                 and set server.password_hash (or set allow_unauthenticated_lan = true to accept the risk)"
                    .into(),
            );
        }
        e
    }

    /// Total installed DC capacity, W.
    pub fn dc_capacity_w(&self) -> f64 {
        self.arrays.iter().map(|a| a.dc_capacity_w).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loc(lat: f64, lon: f64, tz: &str) -> Location {
        Location {
            latitude: lat,
            longitude: lon,
            elevation_m: None,
            time_zone: tz.into(),
        }
    }

    #[test]
    fn warn_delay() {
        let mut c = CollectorConfig::default();
        assert_eq!(c.warn_after_s(), 300);
        c.warn_after_min = 0;
        assert_eq!(c.warn_after_s(), c.stale_after_s as i64);
        // Older config files without the field get the 5-minute default.
        let cfg: Config = toml::from_str(
            "[collector]
stale_after_s = 60
",
        )
        .unwrap();
        assert_eq!(cfg.collector.warn_after_min, 5);
        let mut cfg = Config::default();
        cfg.collector.warn_after_min = 2000;
        assert!(cfg.validate().iter().any(|e| e.contains("warn_after_min")));
    }

    #[test]
    fn location_sign_warnings() {
        // San Francisco, CA: correct, then with the minus sign lost.
        assert!(
            loc(37.7749, -122.4194, "America/Los_Angeles")
                .warnings()
                .is_empty()
        );
        let w = loc(37.7749, 122.4194, "America/Los_Angeles").warnings();
        assert_eq!(w.len(), 1);
        assert!(
            w[0].contains("wrong sign") && w[0].contains("-122.4194"),
            "{w:?}"
        );
        // Southern hemisphere latitude typed as positive.
        assert!(
            loc(-33.87, 151.21, "Australia/Sydney")
                .warnings()
                .is_empty()
        );
        assert!(loc(33.87, 151.21, "Australia/Sydney").warnings()[0].contains("southern"));
        // Near Greenwich, and wide zones, stay quiet.
        assert!(loc(51.5, -0.12, "Europe/London").warnings().is_empty());
        assert!(loc(40.4, -3.7, "Europe/Madrid").warnings().is_empty());
        assert!(loc(39.5, 76.0, "Asia/Shanghai").warnings().is_empty());
        assert!(!loc(0.0, 0.0, "UTC").warnings().is_empty());
    }

    #[test]
    fn default_is_valid() {
        assert!(
            Config::default().validate().is_empty(),
            "{:?}",
            Config::default().validate()
        );
    }

    #[test]
    fn lan_without_password_rejected() {
        let mut c = Config::default();
        c.server.bind = "0.0.0.0:8080".parse().unwrap();
        assert_eq!(c.validate().len(), 1);
        c.server.allow_unauthenticated_lan = true;
        assert!(c.validate().is_empty());
    }

    #[test]
    fn roundtrip_toml() {
        let mut c = Config::default();
        c.gateway.host = "192.168.1.50".into();
        c.location = Some(Location {
            latitude: 37.0,
            longitude: -122.0,
            elevation_m: None,
            time_zone: "America/Los_Angeles".into(),
        });
        c.arrays.push(ArraySpec {
            dc_capacity_w: 8000.0,
            ..Default::default()
        });
        let text = toml::to_string_pretty(&c).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(back.gateway.host, "192.168.1.50");
        assert_eq!(back.arrays.len(), 1);
        assert!(back.validate().is_empty(), "{:?}", back.validate());
    }

    #[test]
    fn tariff_example_parses_and_validates() {
        let text = include_str!("../docs/tariff-example.toml");
        let c: Config = toml::from_str(text).expect("example parses as part of a config");
        assert!(c.tariff.validate().is_ok());
    }

    #[test]
    fn example_config_parses_and_validates() {
        let c: Config =
            toml::from_str(include_str!("../deploy/config.example.toml")).expect("example parses");
        assert!(c.validate().is_empty(), "{:?}", c.validate());
        assert_eq!(c.gateway.host, "192.168.1.50");
    }

    #[test]
    fn tariff_json_accepts_null_export() {
        // Shape sent by the Settings JSON editor.
        let v = serde_json::json!({
            "enabled": true, "currency": "USD", "time_zone": "America/Los_Angeles", "holidays": ["2026-01-01"],
            "plans": [{ "name": "p", "effective_from": "2026-06-01", "fixed_daily": 0.5, "export": null,
                "baseline": null, "tiers": [], "demand_per_kw": null,
                "seasons": [{ "name": "all", "months": [1,2,3,4,5,6,7,8,9,10,11,12],
                    "periods": [{ "name": "flat", "days": "all", "start_hour": 0, "end_hour": 24, "import_per_kwh": 0.3 }] }] }]
        });
        let t: TariffConfig = serde_json::from_value(v).expect("null export accepted");
        assert!(t.validate().is_ok());
        assert_eq!(t.plans[0].export, rustsolar_core::tariff::ExportRule::None);
    }

    #[test]
    fn bad_values_reported() {
        let mut c = Config::default();
        c.collector.interval_s = 1;
        c.weather.refresh_min = 1;
        c.location = Some(Location {
            latitude: 99.0,
            longitude: 0.0,
            elevation_m: None,
            time_zone: "Nowhere/City".into(),
        });
        assert!(c.validate().len() >= 4);
    }
}
