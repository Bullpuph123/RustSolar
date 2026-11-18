//! Periodic "intelligence" pass: learns from completed hours, builds solar/load
//! forecasts, simulates the battery, generates recommendations and evaluates alerts.
//! Results are cached in memory for the API; forecasts are also stored for later
//! accuracy evaluation.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use rustsolar_core::astro::{self, SunTimes};
use rustsolar_core::battery::{self, BatteryParams, SimStep};
use serde::Serialize;
use tokio_util::sync::CancellationToken;

use crate::config::Config;
use crate::db::{self, now};
use crate::forecast::{
    self, EMPIRICAL_MODEL, LoadForecast, PHYSICAL_MODEL, SolarCorrection, SolarForecast,
};
use crate::gateway::model::BatteryInventory;
use crate::state::{LiveSnapshot, Shared};
use crate::weather;

/// IQ Battery 5P continuous power per unit, W (Enphase IQ Battery 5P datasheet, 2023:
/// 3.84 kVA continuous, 5.0 kWh usable). Used only when the inventory confirms the count.
pub const IQ5P_CONTINUOUS_W: f64 = 3840.0;

#[derive(Debug, Clone, Serialize)]
pub struct Intel {
    pub computed_at: i64,
    pub sun_today: Option<SunTimes>,
    pub sun_tomorrow: Option<SunTimes>,
    pub solar: Option<SolarForecast>,
    pub load: Option<LoadForecast>,
    pub battery: BatteryOutlook,
    pub recommendations: Vec<Recommendation>,
    pub needs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct BatteryOutlook {
    pub soc_pct: Option<f64>,
    pub reserve_pct: Option<f64>,
    pub usable_wh: Option<f64>,
    pub capacity_source: String,
    pub max_power_w: Option<f64>,
    pub power_source: String,
    pub units: Option<usize>,
    pub state: String,
    pub power_w: f64,
    pub time_to_full: Option<i64>,
    pub time_to_reserve: Option<i64>,
    pub soc_at_sunset: Option<f64>,
    pub soc_at_sunrise: Option<f64>,
    pub sim: Vec<(i64, f64)>,
    pub expected_import_wh_24h: Option<f64>,
    pub expected_export_wh_24h: Option<f64>,
    pub overnight_battery_wh: Option<f64>,
    pub outage: Vec<OutageScenario>,
    pub health: BatteryHealth,
    pub assumptions: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OutageScenario {
    pub name: String,
    pub load_w: Option<f64>,
    pub hours: Option<f64>,
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct BatteryHealth {
    pub charge_kwh: f64,
    pub discharge_kwh: f64,
    pub equivalent_cycles: Option<f64>,
    pub observed_round_trip: Option<f64>,
    pub since: Option<i64>,
    pub implied_capacity_wh: Option<f64>,
    pub capacity_trend: Vec<(i64, f64)>,
    pub note: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Recommendation {
    pub id: String,
    pub kind: String,
    pub severity: String,
    pub title: String,
    pub message: String,
    pub why: String,
    pub evidence: Vec<(String, String)>,
    pub confidence: String,
    pub benefit: Option<String>,
    pub window: Option<(i64, i64)>,
    pub actionable_now: bool,
}

// ---------------- battery parameters ----------------

pub fn battery_params(
    c: &Connection,
    cfg: &Config,
    live: &LiveSnapshot,
) -> Result<(Option<BatteryParams>, BatteryOutlook)> {
    let mut o = BatteryOutlook::default();
    let inv: Option<BatteryInventory> = db::kv_get(c, "gw:/ivp/ensemble/inventory")?
        .and_then(|(v, _)| serde_json::from_str(&v).ok());
    o.units = inv.as_ref().map(|i| i.count).filter(|n| *n > 0);
    let implied = implied_capacity(c, now() - 86400 * 3)?;
    o.health.implied_capacity_wh = implied;
    let (usable, src) = if let Some(k) = cfg.battery.usable_kwh {
        (Some(k * 1000.0), "configured".to_string())
    } else if let Some(cap) = inv.as_ref().and_then(|i| i.capacity_wh) {
        (
            Some(cap),
            format!(
                "gateway inventory ({} units)",
                inv.as_ref().map(|i| i.count).unwrap_or(0)
            ),
        )
    } else if let Some(cap) = implied {
        (
            Some(cap),
            "estimated from gateway stored energy ÷ SOC (unverified)".to_string(),
        )
    } else {
        (
            None,
            "unknown — set battery.usable_kwh or wait for inventory".to_string(),
        )
    };
    o.usable_wh = usable;
    o.capacity_source = src;
    let max_obs: Option<f64> = c
        .query_row(
            "SELECT MAX(ABS(storage_w)) FROM samples WHERE ts > ?1",
            [now() - 30 * 86400],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    let (pw, psrc) = if let Some(k) = cfg.battery.max_power_kw {
        (Some(k * 1000.0), "configured".to_string())
    } else if let Some(n) = o.units {
        (
            Some(n as f64 * IQ5P_CONTINUOUS_W),
            format!("{n} × 3.84 kW (IQ Battery 5P datasheet)"),
        )
    } else if let Some(m) = max_obs.filter(|m| *m > 500.0) {
        (
            Some(m),
            "highest observed battery power (lower bound)".to_string(),
        )
    } else {
        (None, "unknown".to_string())
    };
    o.max_power_w = pw;
    o.power_source = psrc;
    if let Some(s) = &live.status {
        o.soc_pct = s.soc_pct;
        o.reserve_pct = s.backup_soc_pct;
        o.power_w = s.storage_w;
        o.state = if s.storage_w < -50.0 {
            "charging"
        } else if s.storage_w > 50.0 {
            "discharging"
        } else {
            "idle"
        }
        .into();
    }
    let params = match (usable, pw) {
        (Some(u), Some(p)) => Some(BatteryParams {
            usable_wh: u,
            reserve_pct: o.reserve_pct.unwrap_or(0.0),
            outage_floor_pct: cfg.battery.outage_floor_pct,
            max_charge_w: p,
            max_discharge_w: p,
            round_trip_eff: cfg.battery.round_trip_eff,
        }),
        _ => None,
    };
    Ok((params, o))
}

/// Median of `enc_agg_energy / (SOC/100)` over samples with SOC ≥ 30 %.
pub fn implied_capacity(c: &Connection, since: i64) -> Result<Option<f64>> {
    let mut st = c.prepare("SELECT enc_energy / (soc / 100.0) FROM samples WHERE ts > ?1 AND soc >= 30 AND enc_energy > 0 ORDER BY ts DESC LIMIT 3000")?;
    let mut v: Vec<f64> = st
        .query_map([since], |r| r.get::<_, f64>(0))?
        .filter_map(|x| x.ok())
        .filter(|x| x.is_finite())
        .collect();
    if v.len() < 10 {
        return Ok(None);
    }
    v.sort_by(|a, b| a.total_cmp(b));
    Ok(Some(v[v.len() / 2]))
}

fn health(c: &Connection, usable: Option<f64>) -> Result<BatteryHealth> {
    let (ch, dis, since): (f64, f64, Option<i64>) = c.query_row(
        "SELECT COALESCE(SUM(charge_wh),0), COALESCE(SUM(discharge_wh),0), MIN(CASE WHEN covered_s > 0 THEN hour_ts END) FROM energy_hourly",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    // Round trip over the last 14 days if SOC is back near where it started.
    let t0 = now() - 14 * 86400;
    let socs: Option<(f64, f64)> = c
        .query_row(
            "SELECT (SELECT soc_end FROM energy_hourly WHERE hour_ts >= ?1 AND soc_end IS NOT NULL ORDER BY hour_ts LIMIT 1),
                    (SELECT soc_end FROM energy_hourly WHERE soc_end IS NOT NULL ORDER BY hour_ts DESC LIMIT 1)",
            [t0],
            |r| Ok(r.get::<_, Option<f64>>(0)?.zip(r.get::<_, Option<f64>>(1)?)),
        )
        .optional()?
        .flatten();
    let (c14, d14): (f64, f64) = c.query_row("SELECT COALESCE(SUM(charge_wh),0), COALESCE(SUM(discharge_wh),0) FROM energy_hourly WHERE hour_ts >= ?1", [t0], |r| Ok((r.get(0)?, r.get(1)?)))?;
    let rte = socs.and_then(|(a, b)| battery::observed_round_trip(c14, d14, a, b));
    // Weekly implied-capacity trend.
    let mut trend = Vec::new();
    for w in (0..12).rev() {
        let a = now() - (w + 1) * 7 * 86400;
        let b = a + 7 * 86400;
        let mut st = c.prepare("SELECT enc_energy / (soc / 100.0) FROM samples WHERE ts >= ?1 AND ts < ?2 AND soc >= 30 AND enc_energy > 0")?;
        let mut v: Vec<f64> = st
            .query_map(params![a, b], |r| r.get::<_, f64>(0))?
            .filter_map(|x| x.ok())
            .collect();
        if v.len() >= 50 {
            v.sort_by(|x, y| x.total_cmp(y));
            trend.push((b, v[v.len() / 2]));
        }
    }
    Ok(BatteryHealth {
        charge_kwh: ch / 1000.0,
        discharge_kwh: dis / 1000.0,
        equivalent_cycles: usable.map(|u| battery::equivalent_cycles(dis, u)),
        observed_round_trip: rte,
        since,
        implied_capacity_wh: None,
        capacity_trend: trend,
        note: "Calculated from metered battery power and gateway-reported energy; not a battery-management-system state-of-health value.".into(),
    })
}

// ---------------- learning ----------------

fn load_correction(c: &Connection, model: &str) -> Result<SolarCorrection> {
    let key = format!("solar_correction:{model}");
    let v: Option<String> = c
        .query_row(
            "SELECT value FROM model_params WHERE key = ?1",
            [&key],
            |r| r.get(0),
        )
        .optional()?;
    Ok(v.and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| SolarCorrection::new(model, now())))
}

fn save_correction(c: &Connection, corr: &SolarCorrection) -> Result<()> {
    c.execute(
        "INSERT INTO model_params(key, value, updated_at) VALUES (?1, ?2, ?3) ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        params![format!("solar_correction:{}", corr.model), serde_json::to_string(corr)?, now()],
    )?;
    Ok(())
}

pub fn reset_models(c: &Connection) -> Result<usize> {
    Ok(c.execute(
        "DELETE FROM model_params WHERE key LIKE 'solar_correction:%'",
        [],
    )?)
}

/// Feed completed hours into the correction, using only weather fetched before each hour began.
fn learn(c: &Connection, cfg: &Config, corr: &mut SolarCorrection) -> Result<usize> {
    let Some(loc) = &cfg.location else {
        return Ok(0);
    };
    let start = corr
        .last_hour
        .max(corr.reset_at.div_euclid(3600) * 3600 - 3600)
        .max(now() - 30 * 86400);
    let end = now().div_euclid(3600) * 3600; // completed hours only
    let rows = db::hourly(c, start + 3600, end)?;
    let mut n = 0;
    for h in rows {
        if h.e.covered_s < 3240.0 {
            corr.last_hour = h.hour_ts;
            continue;
        }
        let w = weather::hours_as_of(c, h.hour_ts + 3600, h.hour_ts + 3601, h.hour_ts)?;
        if let Some(w) = w.first() {
            let b = forecast::baseline_for(h.hour_ts, w, loc.latitude, loc.longitude, &cfg.arrays);
            // Scale partial coverage up to the full hour.
            corr.learn(&b, h.e.pv_wh * 3600.0 / h.e.covered_s);
            n += 1;
        }
        corr.last_hour = h.hour_ts;
    }
    Ok(n)
}

fn store_forecasts(
    c: &mut Connection,
    kind: &str,
    model: &str,
    created: i64,
    rows: &[(i64, f64, f64, f64)],
) -> Result<()> {
    let tx = c.transaction()?;
    for (ts, v, lo, hi) in rows {
        tx.execute(
            "INSERT OR REPLACE INTO forecasts(kind, created_at, target_ts, model, value, lo, hi) VALUES (?1,?2,?3,?4,?5,?6,?7)",
            params![kind, created, ts, model, v, lo, hi],
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// Forecast-vs-actual pairs: for each completed hour, the most recent stored forecast that
/// was created at least `min_lead_s` before the hour started.
pub fn forecast_pairs(
    c: &Connection,
    kind: &str,
    from: i64,
    to: i64,
    min_lead_s: i64,
) -> Result<Vec<(i64, f64, f64, usize)>> {
    let col = if kind == "pv_wh" { "pv_wh" } else { "load_wh" };
    let mut st = c.prepare(&format!(
        "SELECT f.target_ts, f.value, e.{col} * 3600.0 / e.covered_s
         FROM forecasts f JOIN energy_hourly e ON e.hour_ts = f.target_ts
         WHERE f.kind = ?1 AND f.target_ts >= ?2 AND f.target_ts < ?3 AND e.covered_s >= 3240
           AND f.created_at = (SELECT MAX(created_at) FROM forecasts f2 WHERE f2.kind = f.kind AND f2.target_ts = f.target_ts AND f2.created_at <= f.target_ts - ?4)
         ORDER BY f.target_ts"
    ))?;
    let rows = st.query_map(params![kind, from, to, min_lead_s], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, f64>(1)?,
            r.get::<_, f64>(2)?,
        ))
    })?;
    let mut out = Vec::new();
    for r in rows {
        let (ts, f, a) = r?;
        let cloud: Option<f64> = c
            .query_row(
                "SELECT cloud FROM weather_hourly WHERE ts = ?1 ORDER BY fetched_at DESC LIMIT 1",
                [ts + 3600],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        let sky = match cloud {
            Some(cc) if cc < 25.0 => 2,
            Some(cc) if cc < 75.0 => 1,
            _ => 0,
        };
        out.push((ts, f, a, sky));
    }
    Ok(out)
}

// ---------------- the pass ----------------

pub async fn compute(state: &Shared) -> Result<Intel> {
    let cfg = state.cfg();
    let live = state.live();
    let t = now();
    let demo = state.demo;
    state
        .db
        .call(move |c| {
            let mut needs = Vec::new();
            let (tz, lat, lon) = match &cfg.location {
                Some(l) => (l.tz(), l.latitude, l.longitude),
                None => {
                    needs.push("Set your location (latitude, longitude, time zone) in Settings to enable sun times, weather and solar forecasts.".into());
                    (chrono_tz::UTC, f64::NAN, f64::NAN)
                }
            };
            let today = forecast::local_date(t, tz);
            let (sun_today, sun_tomorrow) = if lat.is_finite() {
                (Some(astro::sun_times(today, lat, lon, tz)), today.succ_opt().map(|d| astro::sun_times(d, lat, lon, tz)))
            } else {
                (None, None)
            };
            if cfg.arrays.is_empty() {
                needs.push("Add your roof array specs (DC capacity, tilt, azimuth) in Settings for a physical solar forecast. Until then an empirical model is learned from history.".into());
            }

            // Solar forecast.
            let model = if cfg.arrays.is_empty() { EMPIRICAL_MODEL } else { PHYSICAL_MODEL };
            let mut corr = load_correction(c, model)?;
            let learned = if cfg.location.is_some() { learn(c, &cfg, &mut corr)? } else { 0 };
            if learned > 0 {
                save_correction(c, &corr)?;
            }
            let wx = weather::latest_hours(c, t - 3600, t + 17 * 86400)?;
            let solar = if lat.is_finite() && !wx.is_empty() { forecast::solar_forecast(t, &wx, lat, lon, tz, &cfg.arrays, &corr) } else { None };
            if lat.is_finite() && wx.is_empty() && cfg.weather.enabled {
                needs.push("Waiting for the first weather forecast download.".into());
            }

            // Load forecast.
            let hist = db::hourly(c, t - 42 * 86400, t)?;
            let temps: HashMap<i64, f64> = weather::latest_hours(c, t - 42 * 86400, t + 3 * 86400)?
                .into_iter()
                .filter_map(|w| w.temp_c.map(|tc| (w.ts - 3600, tc)))
                .collect();
            let sunset_today = sun_today.as_ref().and_then(|s| s.sunset).map(|x| x.timestamp());
            // Next sunrise: today's if it hasn't happened yet, otherwise tomorrow's.
            let sunrise_next = sun_today
                .as_ref()
                .and_then(|s| s.sunrise)
                .map(|x| x.timestamp())
                .filter(|sr| *sr > t)
                .or_else(|| sun_tomorrow.as_ref().and_then(|s| s.sunrise).map(|x| x.timestamp()));
            let load = forecast::load_forecast(t, &hist, &temps, tz, 72, sunrise_next, sunset_today);

            // Store hourly snapshots once per hour (first pass in each hour), next 48 h.
            let hour = t.div_euclid(3600) * 3600;
            let last_store: Option<i64> = c.query_row("SELECT MAX(created_at) FROM forecasts WHERE kind = 'pv_wh'", [], |r| r.get(0))?;
            if !demo && last_store.is_none_or(|l| l < hour) {
                if let Some(s) = &solar {
                    let rows: Vec<_> = s.hours.iter().filter(|h| h.ts < hour + 48 * 3600).map(|h| (h.ts, h.wh, h.lo, h.hi)).collect();
                    store_forecasts(c, "pv_wh", &s.model, hour, &rows)?;
                }
                if let Some(l) = &load {
                    let rows: Vec<_> = l.hours.iter().filter(|h| h.ts < hour + 48 * 3600).map(|h| (h.ts, h.wh, h.lo, h.hi)).collect();
                    store_forecasts(c, "load_wh", &l.model, hour, &rows)?;
                }
                c.execute("DELETE FROM forecasts WHERE created_at < ?1", [t - 400 * 86400])?;
            }

            // Battery.
            let (params_opt, mut bo) = battery_params(c, &cfg, &live)?;
            bo.health = BatteryHealth { implied_capacity_wh: bo.health.implied_capacity_wh, ..health(c, bo.usable_wh)? };
            simulate_outlook(&mut bo, params_opt.as_ref(), solar.as_ref(), load.as_ref(), &live, t, sunset_today, sunrise_next, &cfg);

            let recommendations = recommendations(c, &cfg, &live, solar.as_ref(), load.as_ref(), &bo, sun_today.as_ref(), sunrise_next, tz, t)?;
            Ok(Intel { computed_at: t, sun_today, sun_tomorrow, solar, load, battery: bo, recommendations, needs })
        })
        .await
}

#[allow(clippy::too_many_arguments)]
fn simulate_outlook(
    bo: &mut BatteryOutlook,
    p: Option<&BatteryParams>,
    solar: Option<&SolarForecast>,
    load: Option<&LoadForecast>,
    live: &LiveSnapshot,
    t: i64,
    sunset: Option<i64>,
    sunrise_next: Option<i64>,
    cfg: &Config,
) {
    let Some(p) = p else {
        bo.assumptions
            .push("Battery capacity or power limit unknown, so no simulation is shown.".into());
        return;
    };
    let Some(soc) = bo.soc_pct else { return };
    let cur_load = live.status.as_ref().map(|s| s.load_w).unwrap_or(0.0);
    let hour = t.div_euclid(3600) * 3600;
    let mut steps = Vec::new();
    for k in 0..48 {
        let hs = hour + k * 3600;
        let (start, frac) = if k == 0 {
            (t, 1.0 - (t - hour) as f64 / 3600.0)
        } else {
            (hs, 1.0)
        };
        let pv = solar
            .and_then(|s| s.hours.iter().find(|h| h.ts == hs))
            .map(|h| h.wh);
        let ld = load
            .and_then(|l| l.hours.iter().find(|h| h.ts == hs))
            .map(|h| h.wh)
            .unwrap_or(cur_load);
        let Some(pv) = pv.or(if solar.is_none() { Some(0.0) } else { None }) else {
            break;
        };
        steps.push(SimStep {
            start_ts: start,
            hours: frac,
            pv_wh: pv * frac,
            load_wh: ld * frac,
        });
    }
    if solar.is_none() {
        bo.assumptions.push("No solar forecast: the simulation assumes no solar production, so daytime values are pessimistic.".into());
    }
    if load.is_none() {
        bo.assumptions.push(format!(
            "Not enough history for a load forecast: assumes the current {:.1} kW load continues.",
            cur_load / 1000.0
        ));
    }
    bo.assumptions.push(format!(
        "Self-consumption mode; round-trip efficiency {:.0} %; reserve {:.0} %; usable {:.1} kWh ({}).",
        p.round_trip_eff * 100.0,
        p.reserve_pct,
        p.usable_wh / 1000.0,
        bo.capacity_source
    ));
    let pts = battery::simulate(p, soc, &steps);
    bo.sim = std::iter::once((t, soc))
        .chain(pts.iter().map(|x| (x.end_ts, x.soc_pct)))
        .collect();
    bo.time_to_full = battery::time_to_soc(t, soc, &pts, 99.5, true).filter(|_| soc < 99.5);
    bo.time_to_reserve = battery::time_to_soc(t, soc, &pts, p.reserve_pct + 0.5, false)
        .filter(|_| soc > p.reserve_pct + 0.5);
    bo.soc_at_sunset = sunset
        .filter(|s| *s > t)
        .and_then(|s| battery::soc_at(t, soc, &pts, s));
    bo.soc_at_sunrise = sunrise_next.and_then(|s| battery::soc_at(t, soc, &pts, s));
    let day_end = t + 86400;
    let mut it = steps.iter().zip(&pts).filter(|(s, _)| s.start_ts < day_end);
    let (mut imp, mut exp) = (0.0, 0.0);
    for (_, pt) in it.by_ref() {
        imp += pt.import_wh;
        exp += pt.export_wh;
    }
    bo.expected_import_wh_24h = Some(imp);
    bo.expected_export_wh_24h = Some(exp);
    if let (Some(ss), Some(sr)) = (sunset, sunrise_next) {
        bo.overnight_battery_wh = Some(
            steps
                .iter()
                .zip(&pts)
                .filter(|(s, _)| s.start_ts >= ss.min(sr) - 3600 && s.start_ts < sr)
                .map(|(_, pt)| pt.discharge_wh)
                .sum(),
        );
    }
    // Outage scenarios.
    let avg_night = load.and_then(|l| {
        let v: Vec<f64> = l
            .hours
            .iter()
            .filter(|h| {
                let hr = forecast::local_hour(
                    h.ts,
                    cfg.location
                        .as_ref()
                        .map(|x| x.tz())
                        .unwrap_or(chrono_tz::UTC),
                );
                !(6..22).contains(&hr)
            })
            .map(|h| h.wh)
            .collect();
        (!v.is_empty()).then(|| v.iter().sum::<f64>() / v.len() as f64)
    });
    let mut sc = vec![(
        format!("Current load ({:.1} kW)", cur_load / 1000.0),
        cur_load,
    )];
    if let Some(a) = avg_night {
        sc.push(("Typical overnight load".into(), a));
    }
    sc.push((
        format!(
            "Essential loads only ({:.1} kW, your setting)",
            cfg.battery.essential_load_kw
        ),
        cfg.battery.essential_load_kw * 1000.0,
    ));
    for (name, w) in sc {
        let h = battery::outage_runtime_h(p, soc, w);
        let note = match h {
            None => "Load exceeds the batteries' continuous power limit.".into(),
            Some(x) if x.is_infinite() => "No load.".into(),
            Some(_) => "Battery only, no solar recharge, constant load.".into(),
        };
        bo.outage.push(OutageScenario {
            name,
            load_w: Some(w),
            hours: h.filter(|x| x.is_finite()),
            note,
        });
    }
    if solar.is_some() && load.is_some() {
        let outage_steps: Vec<SimStep> = steps.clone();
        let h = battery::outage_runtime_profile_h(p, soc, &outage_steps);
        bo.outage.push(OutageScenario {
            name: "Forecast load with forecast solar recharge".into(),
            load_w: None,
            hours: h,
            note: match h {
                None => "Lasts the full 48-hour forecast window under these assumptions.".into(),
                Some(_) => "Uses the hourly load and solar forecasts.".into(),
            },
        });
    }
    bo.assumptions.push("Outage runtimes are estimates, not guarantees: actual loads, battery temperature and inverter limits vary.".into());
}

#[allow(clippy::too_many_arguments)]
fn recommendations(
    c: &Connection,
    cfg: &Config,
    live: &LiveSnapshot,
    solar: Option<&SolarForecast>,
    load: Option<&LoadForecast>,
    bo: &BatteryOutlook,
    sun: Option<&SunTimes>,
    next_sunrise: Option<i64>,
    tz: chrono_tz::Tz,
    t: i64,
) -> Result<Vec<Recommendation>> {
    let mut out = Vec::new();
    let fmt_t = |ts: i64| {
        forecast::utc(ts)
            .with_timezone(&tz)
            .format("%-I:%M %p")
            .to_string()
    };
    let today = forecast::local_date(t, tz);
    let (d0, _) = forecast::day_bounds(today, tz);
    let today_actual = db::energy_sum(c, d0, t)?;
    let conf = |s: &SolarForecast| {
        if s.learned_hours >= 24 * 14 {
            "medium"
        } else {
            "low"
        }
        .to_string()
    };
    let rate = cfg
        .tariff
        .enabled
        .then(|| cfg.tariff.rate_at(forecast::utc(t)))
        .flatten();

    if let (Some(s), Some(l)) = (solar, load) {
        // Best surplus window tomorrow.
        let tomorrow = today.succ_opt();
        let surplus: Vec<(i64, f64)> = s
            .hours
            .iter()
            .filter(|h| Some(forecast::local_date(h.ts, tz)) == tomorrow)
            .map(|h| {
                (
                    h.ts,
                    h.wh - l
                        .hours
                        .iter()
                        .find(|x| x.ts == h.ts)
                        .map(|x| x.wh)
                        .unwrap_or(0.0),
                )
            })
            .collect();
        if let (Some(tw), Some(lw)) = (s.tomorrow_wh, l.tomorrow_wh)
            && tw > 1.3 * lw
        {
            let good: Vec<&(i64, f64)> = surplus.iter().filter(|x| x.1 > 1500.0).collect();
            if let (Some(a), Some(b)) = (good.first(), good.last()) {
                out.push(Recommendation {
                    id: "sunny-tomorrow".into(),
                    kind: "opportunity".into(),
                    severity: "info".into(),
                    title: "Sunny day tomorrow".into(),
                    message: format!("Tomorrow is forecast to produce about {:.0} kWh. Consider scheduling energy-intensive tasks between {} and {}.", tw / 1000.0, fmt_t(a.0), fmt_t(b.0 + 3600)),
                    why: "Forecast solar production exceeds forecast household use by more than 30 %, with surplus above 1.5 kW in this window.".into(),
                    evidence: vec![("Forecast solar".into(), format!("{:.1} kWh", tw / 1000.0)), ("Forecast use".into(), format!("{:.1} kWh", lw / 1000.0))],
                    confidence: conf(s),
                    benefit: Some(format!("Up to {:.1} kWh of surplus solar available", good.iter().map(|x| x.1).sum::<f64>() / 1000.0)),
                    window: Some((a.0, b.0 + 3600)),
                    actionable_now: false,
                });
            }
        }
        // Cloudy tomorrow vs today.
        let today_total = today_actual.pv_wh + s.rest_of_today_wh.unwrap_or(0.0);
        if let Some(tw) = s.tomorrow_wh
            && today_total > 5000.0
            && tw < 0.6 * today_total
        {
            let pct = (1.0 - tw / today_total) * 100.0;
            out.push(Recommendation {
                id: "cloudy-tomorrow".into(),
                kind: "planning".into(),
                severity: "info".into(),
                title: "Less sun tomorrow".into(),
                message: format!("Tomorrow's solar production is predicted to be {pct:.0} % lower than today's. Consider shifting flexible electricity use to today."),
                why: "Day-over-day forecast comparison.".into(),
                evidence: vec![("Today (measured + forecast)".into(), format!("{:.1} kWh", today_total / 1000.0)), ("Tomorrow (forecast)".into(), format!("{:.1} kWh", tw / 1000.0))],
                confidence: conf(s),
                benefit: None,
                window: sun.and_then(|x| x.sunset).map(|ss| (t, ss.timestamp())),
                actionable_now: true,
            });
        }
    }
    // Battery full soon with surplus afterward.
    if let (Some(tf), Some(s)) = (bo.time_to_full, solar)
        && forecast::local_date(tf, tz) == today
        && tf > t
    {
        let after: f64 = s
            .hours
            .iter()
            .filter(|h| h.ts >= tf && forecast::local_date(h.ts, tz) == today)
            .map(|h| h.wh)
            .sum::<f64>()
            - load
                .map(|l| {
                    l.hours
                        .iter()
                        .filter(|h| h.ts >= tf && forecast::local_date(h.ts, tz) == today)
                        .map(|h| h.wh)
                        .sum::<f64>()
                })
                .unwrap_or(0.0);
        if after > 1000.0 {
            out.push(Recommendation {
                id: "battery-full".into(),
                kind: "opportunity".into(),
                severity: "info".into(),
                title: "Batteries will fill early".into(),
                message: format!("Your batteries are likely to reach full charge around {}, with about {:.1} kWh of surplus solar afterward.", fmt_t(tf), after / 1000.0),
                why: "Battery simulation using the solar and load forecasts.".into(),
                evidence: vec![("SOC now".into(), format!("{:.0} %", bo.soc_pct.unwrap_or(0.0)))],
                confidence: conf(s),
                benefit: Some(format!("{:.1} kWh that would otherwise be exported", after / 1000.0)),
                window: sun.and_then(|x| x.sunset).map(|ss| (tf, ss.timestamp())),
                actionable_now: tf <= t + 1800,
            });
        }
    }
    // Low overnight reserve.
    if let Some(tr) = bo.time_to_reserve
        && solar.is_some()
        && next_sunrise.is_none_or(|sr| tr < sr)
        && tr - t < 18 * 3600
    {
        out.push(Recommendation {
                id: "reserve-overnight".into(),
                kind: "warning".into(),
                severity: "warn".into(),
                title: "Battery may reach reserve".into(),
                message: format!(
                    "Based on your recent consumption, your battery may reach its {:.0} % reserve around {}. Reducing evening use would keep more charge in the battery.",
                    bo.reserve_pct.unwrap_or(0.0),
                    fmt_t(tr)
                ),
                why: "Battery simulation with forecast load and solar.".into(),
                evidence: vec![("SOC now".into(), format!("{:.0} %", bo.soc_pct.unwrap_or(0.0))), ("Expected grid import (24 h)".into(), format!("{:.1} kWh", bo.expected_import_wh_24h.unwrap_or(0.0) / 1000.0))],
                confidence: if load.map(|l| l.days_of_history).unwrap_or(0.0) > 14.0 { "medium" } else { "low" }.into(),
                benefit: rate.as_ref().map(|r| format!("Grid energy now costs about {}{:.2}/kWh ({})", currency(cfg), r.import_per_kwh, r.period)),
                window: Some((t, tr)),
                actionable_now: true,
            });
    }
    // Unexpected consumption right now vs profile.
    if let (Some(l), Some(s)) = (load, &live.status) {
        let hour = t.div_euclid(3600) * 3600;
        if let Some(h) = l.hours.iter().find(|h| h.ts == hour)
            && s.load_w > h.hi.max(h.wh * cfg.alerts.high_load_factor)
            && s.load_w - h.wh > 1500.0
            && !live.stale
        {
            out.push(Recommendation {
                id: "high-load".into(),
                kind: "anomaly".into(),
                severity: "info".into(),
                title: "Higher use than usual".into(),
                message: format!("Household power use is {:.1} kW, well above the {:.1} kW typical for this hour.", s.load_w / 1000.0, h.wh / 1000.0),
                why: "Live load compared with your recency-weighted profile for this hour and day type.".into(),
                evidence: vec![("Now".into(), format!("{:.2} kW", s.load_w / 1000.0)), ("Typical".into(), format!("{:.2} kW", h.wh / 1000.0))],
                confidence: "medium".into(),
                benefit: None,
                window: None,
                actionable_now: true,
            });
        }
    }
    // Underperformance over the last two completed hours.
    if let Some(s) = solar
        && s.model == PHYSICAL_MODEL
    {
        let pairs = forecast_pairs(c, "pv_wh", t - 3 * 3600, t, 0)?;
        let (f, a): (f64, f64) = pairs
            .iter()
            .filter(|p| p.3 == 2)
            .fold((0.0, 0.0), |acc, p| (acc.0 + p.1, acc.1 + p.2));
        if f > 2000.0 && a < f * (1.0 - cfg.alerts.underperformance_frac) {
            let pct = (1.0 - a / f) * 100.0;
            out.push(Recommendation {
                id: "underperformance".into(),
                kind: "anomaly".into(),
                severity: "warn".into(),
                title: "Solar below expectation".into(),
                message: format!("Solar generation is {pct:.0} % below the weather-adjusted expectation over the last hours of clear sky."),
                why: "Measured production compared with the forecast issued before each hour, clear-sky hours only.".into(),
                evidence: vec![("Expected".into(), format!("{:.1} kWh", f / 1000.0)), ("Measured".into(), format!("{:.1} kWh", a / 1000.0))],
                confidence: conf(s),
                benefit: None,
                window: None,
                actionable_now: false,
            });
        }
    }
    // Export surplus now.
    if let Some(st) = &live.status
        && st.grid_w < -1000.0
        && bo.soc_pct.is_some_and(|x| x >= 98.0)
        && !live.stale
    {
        out.push(Recommendation {
            id: "surplus-now".into(),
            kind: "opportunity".into(),
            severity: "info".into(),
            title: "Surplus solar right now".into(),
            message: format!("Batteries are full and {:.1} kW is going to the grid. Now is a good time for flexible loads.", -st.grid_w / 1000.0),
            why: "Live measurements: battery ≥ 98 % and exporting more than 1 kW.".into(),
            evidence: vec![("Export".into(), format!("{:.2} kW", -st.grid_w / 1000.0))],
            confidence: "high".into(),
            benefit: rate.as_ref().map(|r| format!("Using it saves about {}{:.2}/kWh versus buying later (export credit {}{:.2}/kWh)", currency(cfg), r.import_per_kwh, currency(cfg), r.export_per_kwh)),
            window: None,
            actionable_now: true,
        });
    }
    Ok(out)
}

fn currency(cfg: &Config) -> &'static str {
    if cfg.tariff.currency == "USD" {
        "$"
    } else {
        ""
    }
}

pub async fn run(state: Shared, cancel: CancellationToken) {
    let mut rx = state.intel_tx.subscribe();
    // Give the collector a moment to deliver the first reading.
    let mut live_rx = state.live_tx.subscribe();
    let _ = tokio::time::timeout(Duration::from_secs(20), async {
        while live_rx.borrow_and_update().status.is_none() {
            if live_rx.changed().await.is_err() {
                break;
            }
        }
    })
    .await;
    loop {
        match compute(&state).await {
            Ok(i) => {
                let i = Arc::new(i);
                *state.intel.write().expect("intel lock") = Some(i.clone());
                if let Err(e) = crate::alerts::evaluate(&state, &i).await {
                    tracing::warn!("alerts: {e:#}");
                }
            }
            Err(e) => tracing::warn!("intel pass failed: {e:#}"),
        }
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = tokio::time::sleep(Duration::from_secs(120)) => {}
            _ = rx.changed() => {}
        }
    }
}
