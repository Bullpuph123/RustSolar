//! Typed extraction of the fields we use from gateway JSON. We parse the full body into
//! `serde_json::Value` first (tolerant of odd keys), then pick fields defensively:
//! a missing or malformed field becomes `None`, never a crash.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Parsed `/ivp/livedata/status`. Power in watts (gateway reports milliwatts).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LiveStatus {
    /// Gateway measurement time, Unix seconds.
    pub last_update: i64,
    pub pv_w: f64,
    pub storage_w: f64,
    pub grid_w: f64,
    pub load_w: f64,
    pub gen_w: Option<f64>,
    pub soc_pct: Option<f64>,
    pub backup_soc_pct: Option<f64>,
    /// Aggregate battery energy reported by the gateway (`enc_agg_energy`). Behaves like
    /// stored energy in Wh on observed firmware; treated as unverified until checked
    /// against SOC × capacity (see diagnostics).
    pub enc_agg_energy: Option<f64>,
    pub enc_agg_soc_pct: Option<f64>,
    pub main_relay_state: Option<i64>,
    pub gen_relay_state: Option<i64>,
    pub phase_count: Option<i64>,
    pub sc_stream: Option<String>,
    pub mqtt_state: Option<String>,
}

fn num(v: &Value, path: &[&str]) -> Option<f64> {
    let mut cur = v;
    for p in path {
        cur = cur.get(p)?;
    }
    match cur {
        Value::Number(n) => n.as_f64().filter(|x| x.is_finite()),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn text(v: &Value, path: &[&str]) -> Option<String> {
    let mut cur = v;
    for p in path {
        cur = cur.get(p)?;
    }
    cur.as_str().map(String::from)
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ParseError {
    #[error("missing field {0}")]
    Missing(&'static str),
}

impl LiveStatus {
    pub fn from_value(v: &Value) -> Result<Self, ParseError> {
        let mw = |k: &'static str| num(v, &["meters", k, "agg_p_mw"]).map(|x| x / 1000.0);
        Ok(Self {
            last_update: num(v, &["meters", "last_update"])
                .ok_or(ParseError::Missing("meters.last_update"))? as i64,
            pv_w: mw("pv").ok_or(ParseError::Missing("meters.pv.agg_p_mw"))?,
            storage_w: mw("storage").unwrap_or(0.0),
            grid_w: mw("grid").ok_or(ParseError::Missing("meters.grid.agg_p_mw"))?,
            load_w: mw("load").ok_or(ParseError::Missing("meters.load.agg_p_mw"))?,
            gen_w: mw("generator"),
            soc_pct: num(v, &["meters", "soc"]),
            backup_soc_pct: num(v, &["meters", "backup_soc"]),
            enc_agg_energy: num(v, &["meters", "enc_agg_energy"]),
            enc_agg_soc_pct: num(v, &["meters", "enc_agg_soc"]),
            main_relay_state: num(v, &["meters", "main_relay_state"]).map(|x| x as i64),
            gen_relay_state: num(v, &["meters", "gen_relay_state"]).map(|x| x as i64),
            phase_count: num(v, &["meters", "phase_count"]).map(|x| x as i64),
            sc_stream: text(v, &["connection", "sc_stream"]),
            mqtt_state: text(v, &["connection", "mqtt_state"]),
        })
    }

    pub fn sample(&self) -> rustsolar_core::energy::Sample {
        rustsolar_core::energy::Sample {
            ts: self.last_update,
            pv_w: self.pv_w,
            load_w: self.load_w,
            storage_w: self.storage_w,
            grid_w: self.grid_w,
            gen_w: self.gen_w.unwrap_or(0.0),
        }
    }

    /// Relay state 1 = closed (grid connected) on observed firmware; anything else
    /// may mean the system is islanded. Reported as "possible" only.
    pub fn possibly_off_grid(&self) -> bool {
        matches!(self.main_relay_state, Some(s) if s != 1)
    }
}

/// Battery facts from `/ivp/ensemble/inventory`, when that endpoint is available.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct BatteryInventory {
    pub count: usize,
    /// Sum of per-unit `encharge_capacity` (Wh) when every unit reports it.
    pub capacity_wh: Option<f64>,
    pub serials: Vec<String>,
    pub per_unit_soc: Vec<Option<f64>>,
    pub max_cell_temp_c: Option<f64>,
}

impl BatteryInventory {
    pub fn from_value(v: &Value) -> Option<Self> {
        let arr = v.as_array()?;
        let group = arr
            .iter()
            .find(|g| g.get("type").and_then(Value::as_str) == Some("ENCHARGE"))?;
        let devices = group.get("devices")?.as_array()?;
        let mut inv = BatteryInventory {
            count: devices.len(),
            ..Default::default()
        };
        let mut caps = Vec::new();
        for d in devices {
            inv.serials.push(
                d.get("serial_num")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            );
            inv.per_unit_soc.push(num(d, &["percentFull"]));
            if let Some(c) = num(d, &["encharge_capacity"]) {
                caps.push(c);
            }
            if let Some(t) = num(d, &["maxCellTemp"]).or_else(|| num(d, &["max_cell_temp"])) {
                inv.max_cell_temp_c = Some(inv.max_cell_temp_c.map_or(t, |m: f64| m.max(t)));
            }
        }
        if !caps.is_empty() && caps.len() == devices.len() {
            inv.capacity_wh = Some(caps.iter().sum());
        }
        Some(inv)
    }
}

/// Lifetime/today totals from `/production.json?details=1`.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ProductionTotals {
    pub pv_lifetime_wh: Option<f64>,
    pub pv_today_wh: Option<f64>,
    pub consumption_lifetime_wh: Option<f64>,
    pub inverter_count: Option<u32>,
}

impl ProductionTotals {
    pub fn from_value(v: &Value) -> Self {
        let mut t = ProductionTotals::default();
        if let Some(prod) = v.get("production").and_then(Value::as_array) {
            for p in prod {
                let ty = p.get("type").and_then(Value::as_str).unwrap_or("");
                let mt = p
                    .get("measurementType")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if ty == "inverters" {
                    t.inverter_count = num(p, &["activeCount"]).map(|x| x as u32);
                    t.pv_lifetime_wh = t.pv_lifetime_wh.or(num(p, &["whLifetime"]));
                }
                if ty == "eim" && mt == "production" {
                    // Revenue-grade meter, preferred over the inverter estimate.
                    if let Some(x) = num(p, &["whLifetime"]).filter(|x| *x > 0.0) {
                        t.pv_lifetime_wh = Some(x);
                    }
                    t.pv_today_wh = num(p, &["whToday"]);
                }
            }
        }
        if let Some(cons) = v.get("consumption").and_then(Value::as_array) {
            for c in cons {
                if c.get("measurementType").and_then(Value::as_str) == Some("total-consumption") {
                    t.consumption_lifetime_wh = num(c, &["whLifetime"]);
                }
            }
        }
        t
    }
}

/// Per-microinverter readings from `/api/v1/production/inverters`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct InverterReading {
    pub serial: String,
    pub last_report: i64,
    pub last_w: f64,
    pub max_w: f64,
}

pub fn parse_inverters(v: &Value) -> Vec<InverterReading> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|d| {
                    Some(InverterReading {
                        serial: d.get("serialNumber")?.as_str()?.to_string(),
                        last_report: num(d, &["lastReportDate"]).unwrap_or(0.0) as i64,
                        last_w: num(d, &["lastReportWatts"]).unwrap_or(0.0),
                        max_w: num(d, &["maxReportWatts"]).unwrap_or(0.0),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Shape of the real response, with the 12:30 PM readings from the owner's gateway.
    /// Includes the case-distinct counter keys and repeated empty keys that break
    /// case-insensitive parsers.
    pub const FIXTURE: &str = r#"{
      "connection": {"mqtt_state": "connected", "prov_state": "configured", "auth_state": "ok", "sc_stream": "disabled", "sc_debug": "disabled"},
      "meters": {
        "last_update": 1792957580, "soc": 84, "main_relay_state": 1, "gen_relay_state": 5,
        "backup_bat_mode": 1, "backup_soc": 15, "is_split_phase": 1, "phase_count": 2,
        "enc_agg_soc": 84, "enc_agg_energy": 8400, "acb_agg_soc": 0, "acb_agg_energy": 0,
        "pv": {"agg_p_mw": 5050000, "agg_s_mva": 5100000},
        "storage": {"agg_p_mw": -1140000, "agg_s_mva": 1200000},
        "grid": {"agg_p_mw": -2330000, "agg_s_mva": 2400000},
        "load": {"agg_p_mw": 1580000, "agg_s_mva": 1700000},
        "generator": {"agg_p_mw": 0, "agg_s_mva": 0},
        "iqpm": {}
      },
      "counters": {"MqttClient_connect_err": 0, "MqttClient_connect_Err": 2},
      "dry_contacts": {"": {"dry_contact_id": "", "dry_contact_status": 3050004}, "": {"dry_contact_id": "", "dry_contact_status": 3050004}}
    }"#;

    #[test]
    fn parses_real_shape_with_conflicting_keys() {
        let v: Value = serde_json::from_str(FIXTURE)
            .expect("serde_json accepts duplicate and case-distinct keys");
        let s = LiveStatus::from_value(&v).unwrap();
        assert_eq!(s.last_update, 1_792_957_580);
        assert!((s.pv_w - 5050.0).abs() < 1e-9);
        assert!((s.storage_w + 1140.0).abs() < 1e-9);
        assert!((s.grid_w + 2330.0).abs() < 1e-9);
        assert!((s.load_w - 1580.0).abs() < 1e-9);
        assert_eq!(s.soc_pct, Some(84.0));
        assert_eq!(s.backup_soc_pct, Some(15.0));
        assert_eq!(s.sc_stream.as_deref(), Some("disabled"));
        assert!(!s.possibly_off_grid());
        assert!(s.sample().is_balanced(30.0, 0.01));
    }

    #[test]
    fn missing_fields() {
        let v: Value = serde_json::from_str(r#"{"meters":{"last_update":1,"pv":{"agg_p_mw":0},"grid":{"agg_p_mw":5},"load":{"agg_p_mw":"5"}}}"#).unwrap();
        let s = LiveStatus::from_value(&v).unwrap();
        assert_eq!(s.storage_w, 0.0);
        assert_eq!(s.soc_pct, None);
        assert!((s.load_w - 0.005).abs() < 1e-12);
        let bad: Value = serde_json::from_str(r#"{"meters":{"pv":{}}}"#).unwrap();
        assert_eq!(
            LiveStatus::from_value(&bad),
            Err(ParseError::Missing("meters.last_update"))
        );
    }

    #[test]
    fn inventory_counts_only_with_full_capacity_data() {
        let v: Value = serde_json::from_str(
            r#"[{"type":"ENCHARGE","devices":[
            {"serial_num":"A","percentFull":80,"encharge_capacity":5000,"maxCellTemp":24},
            {"serial_num":"B","percentFull":82,"encharge_capacity":5000,"maxCellTemp":26}]},
            {"type":"ENPOWER","devices":[{}]}]"#,
        )
        .unwrap();
        let inv = BatteryInventory::from_value(&v).unwrap();
        assert_eq!(inv.count, 2);
        assert_eq!(inv.capacity_wh, Some(10_000.0));
        assert_eq!(inv.max_cell_temp_c, Some(26.0));
        let partial: Value = serde_json::from_str(r#"[{"type":"ENCHARGE","devices":[{"serial_num":"A","encharge_capacity":5000},{"serial_num":"B"}]}]"#).unwrap();
        assert_eq!(
            BatteryInventory::from_value(&partial).unwrap().capacity_wh,
            None
        );
    }

    #[test]
    fn production_totals_prefers_meter() {
        let v: Value = serde_json::from_str(r#"{"production":[
            {"type":"inverters","activeCount":24,"wNow":5000,"whLifetime":1000},
            {"type":"eim","measurementType":"production","whLifetime":2000,"whToday":15000}],
            "consumption":[{"type":"eim","measurementType":"total-consumption","whLifetime":3000}]}"#).unwrap();
        let t = ProductionTotals::from_value(&v);
        assert_eq!(t.pv_lifetime_wh, Some(2000.0));
        assert_eq!(t.pv_today_wh, Some(15000.0));
        assert_eq!(t.inverter_count, Some(24));
        assert_eq!(t.consumption_lifetime_wh, Some(3000.0));
    }
}
