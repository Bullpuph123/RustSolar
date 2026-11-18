//! Weather foreasts. Primary provider: Open-Meteo free API (no key; non-commercial use;
//! limits 600/min, 5,000/h, 10,000/day; CC BY 4.0 attribution required — checked
//! 2026-10-25 at https://open-meteo.com/en/terms). We call it about twice an hour at
//! most and cache every response in SQLite, so forecasts survive internet outages.
//!
//! Only latitude/longitude (rounded to 0.01°, ~1 km) and variable names are sent.

use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::config::{Location, WeatherConfig};
use crate::db::{self, now};
use crate::state::Shared;

pub const PROVIDER: &str = "open-meteo";
pub const ATTRIBUTION: &str = "Weather data by Open-Meteo.com (CC BY 4.0)";

/// A forecast source. Add new providers by implementing this (after verifying their
/// current terms, cost and limits; see docs/EXTERNAL-SERVICES.md).
pub trait Provider: Send + Sync {
    fn name(&self) -> &'static str;
    fn attribution(&self) -> &'static str;
    fn forecast_url(&self, loc: &Location, days: u32) -> String;
    fn parse_forecast(&self, body: &serde_json::Value) -> Result<Vec<WeatherHour>>;
}

/// Open-Meteo free API.
pub struct OpenMeteo;

impl Provider for OpenMeteo {
    fn name(&self) -> &'static str {
        PROVIDER
    }
    fn attribution(&self) -> &'static str {
        ATTRIBUTION
    }
    fn forecast_url(&self, loc: &Location, days: u32) -> String {
        forecast_url(loc, days)
    }
    fn parse_forecast(&self, body: &serde_json::Value) -> Result<Vec<WeatherHour>> {
        parse_forecast(body)
    }
}

const HOURLY_VARS: &str = "temperature_2m,relative_humidity_2m,cloud_cover,cloud_cover_low,cloud_cover_mid,cloud_cover_high,\
shortwave_radiation,direct_normal_irradiance,diffuse_radiation,precipitation_probability,precipitation,snowfall,\
wind_speed_10m,wind_gusts_10m,visibility,weather_code,is_day";

/// One forecast hour. Radiation values are means over the hour ENDING at `ts`
/// (Open-Meteo convention); temperature/wind are instantaneous at `ts`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct WeatherHour {
    pub ts: i64,
    pub temp_c: Option<f64>,
    pub rh: Option<f64>,
    pub cloud: Option<f64>,
    pub cloud_low: Option<f64>,
    pub cloud_mid: Option<f64>,
    pub cloud_high: Option<f64>,
    pub ghi: Option<f64>,
    pub dni: Option<f64>,
    pub dhi: Option<f64>,
    pub precip_prob: Option<f64>,
    pub precip_mm: Option<f64>,
    pub snow_cm: Option<f64>,
    pub wind_ms: Option<f64>,
    pub gust_ms: Option<f64>,
    pub visibility_m: Option<f64>,
    pub code: Option<i64>,
    pub is_day: Option<i64>,
    pub pm25: Option<f64>,
    pub aqi: Option<f64>,
    pub aod: Option<f64>,
}

fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

pub fn forecast_url(loc: &Location, days: u32) -> String {
    format!(
        "https://api.open-meteo.com/v1/forecast?latitude={}&longitude={}&hourly={HOURLY_VARS}\
         &current=temperature_2m,cloud_cover,weather_code,wind_speed_10m,is_day\
         &timezone=UTC&timeformat=unixtime&wind_speed_unit=ms&past_days=2&forecast_days={}",
        round2(loc.latitude),
        round2(loc.longitude),
        days.clamp(1, 16)
    )
}

pub fn air_quality_url(loc: &Location) -> String {
    format!(
        "https://air-quality-api.open-meteo.com/v1/air-quality?latitude={}&longitude={}\
         &hourly=pm2_5,us_aqi,aerosol_optical_depth&timeformat=unixtime&past_days=1&forecast_days=4",
        round2(loc.latitude),
        round2(loc.longitude)
    )
}

/// Parse an Open-Meteo forecast body into hours.
pub fn parse_forecast(v: &serde_json::Value) -> Result<Vec<WeatherHour>> {
    let h = v.get("hourly").ok_or_else(|| anyhow!("no hourly block"))?;
    let times: Vec<i64> = h
        .get("time")
        .and_then(|t| t.as_array())
        .ok_or_else(|| anyhow!("no hourly.time"))?
        .iter()
        .filter_map(|x| x.as_i64())
        .collect();
    let col = |name: &str| -> Vec<Option<f64>> {
        h.get(name)
            .and_then(|a| a.as_array())
            .map(|a| a.iter().map(|x| x.as_f64()).collect())
            .unwrap_or_default()
    };
    let (temp, rh, cc, ccl, ccm, cch) = (
        col("temperature_2m"),
        col("relative_humidity_2m"),
        col("cloud_cover"),
        col("cloud_cover_low"),
        col("cloud_cover_mid"),
        col("cloud_cover_high"),
    );
    let (ghi, dni, dhi) = (
        col("shortwave_radiation"),
        col("direct_normal_irradiance"),
        col("diffuse_radiation"),
    );
    let (pp, pr, sn, ws, wg, vis, code, day) = (
        col("precipitation_probability"),
        col("precipitation"),
        col("snowfall"),
        col("wind_speed_10m"),
        col("wind_gusts_10m"),
        col("visibility"),
        col("weather_code"),
        col("is_day"),
    );
    let at = |c: &Vec<Option<f64>>, i: usize| c.get(i).copied().flatten();
    Ok(times
        .iter()
        .enumerate()
        .map(|(i, &ts)| WeatherHour {
            ts,
            temp_c: at(&temp, i),
            rh: at(&rh, i),
            cloud: at(&cc, i),
            cloud_low: at(&ccl, i),
            cloud_mid: at(&ccm, i),
            cloud_high: at(&cch, i),
            ghi: at(&ghi, i),
            dni: at(&dni, i),
            dhi: at(&dhi, i),
            precip_prob: at(&pp, i),
            precip_mm: at(&pr, i),
            snow_cm: at(&sn, i),
            wind_ms: at(&ws, i),
            gust_ms: at(&wg, i),
            visibility_m: at(&vis, i),
            code: at(&code, i).map(|x| x as i64),
            is_day: at(&day, i).map(|x| x as i64),
            ..Default::default()
        })
        .collect())
}

/// Parse air quality into (ts, pm25, aqi, aod).
/// (ts, PM2.5 µg/m³, US AQI, aerosol optical depth)
pub type AirRow = (i64, Option<f64>, Option<f64>, Option<f64>);

pub fn parse_air(v: &serde_json::Value) -> Vec<AirRow> {
    let Some(h) = v.get("hourly") else {
        return vec![];
    };
    let times: Vec<i64> = h
        .get("time")
        .and_then(|t| t.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_i64()).collect())
        .unwrap_or_default();
    let col = |n: &str| -> Vec<Option<f64>> {
        h.get(n)
            .and_then(|a| a.as_array())
            .map(|a| a.iter().map(|x| x.as_f64()).collect())
            .unwrap_or_default()
    };
    let (pm, aqi, aod) = (col("pm2_5"), col("us_aqi"), col("aerosol_optical_depth"));
    times
        .iter()
        .enumerate()
        .map(|(i, &t)| {
            (
                t,
                pm.get(i).copied().flatten(),
                aqi.get(i).copied().flatten(),
                aod.get(i).copied().flatten(),
            )
        })
        .collect()
}

pub fn store_hours(c: &mut Connection, fetched_at: i64, hours: &[WeatherHour]) -> Result<()> {
    let tx = c.transaction()?;
    {
        let mut st = tx.prepare(
            "INSERT OR REPLACE INTO weather_hourly(ts, fetched_at, temp_c, rh, cloud, cloud_low, cloud_mid, cloud_high, ghi, dni, dhi,
               precip_prob, precip_mm, snow_cm, wind_ms, gust_ms, visibility_m, code, is_day)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19)",
        )?;
        for h in hours {
            st.execute(params![
                h.ts,
                fetched_at,
                h.temp_c,
                h.rh,
                h.cloud,
                h.cloud_low,
                h.cloud_mid,
                h.cloud_high,
                h.ghi,
                h.dni,
                h.dhi,
                h.precip_prob,
                h.precip_mm,
                h.snow_cm,
                h.wind_ms,
                h.gust_ms,
                h.visibility_m,
                h.code,
                h.is_day
            ])?;
        }
    }
    tx.commit()?;
    Ok(())
}

/// Latest known values for each hour in [from, to).
pub fn latest_hours(c: &Connection, from: i64, to: i64) -> Result<Vec<WeatherHour>> {
    let mut st = c.prepare(
        "SELECT w.ts, temp_c, rh, cloud, cloud_low, cloud_mid, cloud_high, ghi, dni, dhi, precip_prob, precip_mm, snow_cm,
                wind_ms, gust_ms, visibility_m, code, is_day,
                (SELECT pm25 FROM kv_air WHERE kv_air.ts = w.ts), (SELECT aqi FROM kv_air WHERE kv_air.ts = w.ts), (SELECT aod FROM kv_air WHERE kv_air.ts = w.ts)
         FROM weather_hourly w
         JOIN (SELECT ts, MAX(fetched_at) AS f FROM weather_hourly WHERE ts >= ?1 AND ts < ?2 GROUP BY ts) m
           ON w.ts = m.ts AND w.fetched_at = m.f
         ORDER BY w.ts",
    )?;
    let rows = st.query_map(params![from, to], row_to_hour)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// The forecast for each hour in [from, to) as it was known at `as_of` (no peeking at
/// later fetches). Used to evaluate forecasts without leakage.
pub fn hours_as_of(c: &Connection, from: i64, to: i64, as_of: i64) -> Result<Vec<WeatherHour>> {
    let mut st = c.prepare(
        "SELECT w.ts, temp_c, rh, cloud, cloud_low, cloud_mid, cloud_high, ghi, dni, dhi, precip_prob, precip_mm, snow_cm,
                wind_ms, gust_ms, visibility_m, code, is_day, NULL, NULL, NULL
         FROM weather_hourly w
         JOIN (SELECT ts, MAX(fetched_at) AS f FROM weather_hourly WHERE ts >= ?1 AND ts < ?2 AND fetched_at <= ?3 GROUP BY ts) m
           ON w.ts = m.ts AND w.fetched_at = m.f ORDER BY w.ts",
    )?;
    let rows = st.query_map(params![from, to, as_of], row_to_hour)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

fn row_to_hour(r: &rusqlite::Row<'_>) -> rusqlite::Result<WeatherHour> {
    Ok(WeatherHour {
        ts: r.get(0)?,
        temp_c: r.get(1)?,
        rh: r.get(2)?,
        cloud: r.get(3)?,
        cloud_low: r.get(4)?,
        cloud_mid: r.get(5)?,
        cloud_high: r.get(6)?,
        ghi: r.get(7)?,
        dni: r.get(8)?,
        dhi: r.get(9)?,
        precip_prob: r.get(10)?,
        precip_mm: r.get(11)?,
        snow_cm: r.get(12)?,
        wind_ms: r.get(13)?,
        gust_ms: r.get(14)?,
        visibility_m: r.get(15)?,
        code: r.get(16)?,
        is_day: r.get(17)?,
        pm25: r.get(18)?,
        aqi: r.get(19)?,
        aod: r.get(20)?,
    })
}

/// Thin out old forecast versions: for hours older than 30 days keep only the latest
/// version and the last version fetched at least 24 h ahead (day-ahead).
pub fn prune_versions(c: &Connection) -> Result<usize> {
    let cutoff = now() - 30 * 86400;
    Ok(c.execute(
        "DELETE FROM weather_hourly WHERE ts < ?1 AND fetched_at NOT IN (
            SELECT MAX(fetched_at) FROM weather_hourly w2 WHERE w2.ts = weather_hourly.ts
         ) AND fetched_at NOT IN (
            SELECT MAX(fetched_at) FROM weather_hourly w3 WHERE w3.ts = weather_hourly.ts AND w3.fetched_at <= w3.ts - 86400
         )",
        [cutoff],
    )?)
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct WeatherStatus {
    pub provider: &'static str,
    pub attribution: &'static str,
    pub last_success_at: Option<i64>,
    pub last_attempt_at: Option<i64>,
    pub last_error: Option<String>,
    pub requests_today: i64,
    pub daily_budget: u32,
    pub next_refresh_at: Option<i64>,
}

pub fn status(c: &Connection, cfg: &WeatherConfig) -> Result<WeatherStatus> {
    let last_ok: Option<i64> = c.query_row(
        "SELECT MAX(fetched_at) FROM weather_fetch WHERE kind='forecast' AND ok=1",
        [],
        |r| r.get(0),
    )?;
    let last_try: Option<(i64, Option<String>, i64)> = c
        .query_row("SELECT fetched_at, status, ok FROM weather_fetch WHERE kind='forecast' ORDER BY id DESC LIMIT 1", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .ok();
    Ok(WeatherStatus {
        provider: OpenMeteo.name(),
        attribution: OpenMeteo.attribution(),
        last_success_at: last_ok,
        last_attempt_at: last_try.as_ref().map(|x| x.0),
        last_error: last_try.and_then(|x| if x.2 == 0 { x.1 } else { None }),
        requests_today: db::usage_today(c, PROVIDER)?,
        daily_budget: cfg.daily_request_budget,
        next_refresh_at: last_ok.map(|t| t + cfg.refresh_min as i64 * 60),
    })
}

/// HTTP GET with budget accounting and 429/Retry-After handling.
/// Returns Err with a `retry_after` hint in seconds when rate limited.
async fn fetch(
    state: &Shared,
    http: &reqwest::Client,
    url: &str,
    budget: u32,
) -> Result<serde_json::Value, (String, Option<u64>)> {
    let used = state
        .db
        .call(|c| db::usage_today(c, PROVIDER))
        .await
        .unwrap_or(0);
    if used >= budget as i64 {
        return Err((
            format!("daily request budget reached ({used}/{budget}); using cached forecast"),
            Some(3600),
        ));
    }
    let _ = state.db.call(|c| db::bump_usage(c, PROVIDER)).await;
    let resp = http
        .get(url)
        .send()
        .await
        .map_err(|e| (format!("network: {e}"), None))?;
    let status = resp.status().as_u16();
    if status == 429 {
        let ra = resp
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(3600);
        return Err((
            format!("rate limited (HTTP 429), retry after {ra} s"),
            Some(ra),
        ));
    }
    let body = resp
        .text()
        .await
        .map_err(|e| (format!("reading body: {e}"), None))?;
    if status != 200 {
        // Open-Meteo returns {"error":true,"reason":"..."} for bad requests.
        let reason = serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| v.get("reason").and_then(|r| r.as_str()).map(String::from))
            .unwrap_or_default();
        return Err((format!("HTTP {status} {reason}"), None));
    }
    serde_json::from_str(&body).map_err(|e| (format!("invalid JSON: {e}"), None))
}

pub fn http_client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .tls_backend_preconfigured(crate::gateway::tls::public_tls()?)
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .user_agent(concat!(
            "RustSolar/",
            env!("CARGO_PKG_VERSION"),
            " (personal, non-commercial)"
        ))
        .build()?)
}

/// Refresh once if the cache is older than the refresh interval. Returns true if new data was stored.
pub async fn refresh_if_due(state: &Shared, http: &reqwest::Client, force: bool) -> Result<bool> {
    let cfg = state.cfg();
    let Some(loc) = cfg.location.clone() else {
        bail!("location not configured")
    };
    if !cfg.weather.enabled {
        bail!("weather disabled");
    }
    let wc = cfg.weather.clone();
    let st = state.db.call(move |c| status(c, &wc)).await?;
    let t = now();
    if !force
        && let Some(last) = st.last_success_at
        && t - last < cfg.weather.refresh_min as i64 * 60
    {
        return Ok(false);
    }
    // Don't hammer during outages: at most one attempt per 10 minutes after a failure.
    if let (Some(a), Some(_)) = (st.last_attempt_at, st.last_error.as_ref())
        && t - a < 600
        && !force
    {
        return Ok(false);
    }
    let provider: &dyn Provider = &OpenMeteo;
    let url = provider.forecast_url(&loc, cfg.weather.forecast_days);
    let res = fetch(state, http, &url, cfg.weather.daily_request_budget).await;
    match res {
        Ok(v) => {
            let hours = provider
                .parse_forecast(&v)
                .with_context(|| format!("parsing {} forecast", provider.name()))?;
            let body = v.to_string();
            let n = hours.len();
            let pname = provider.name();
            state
                .db
                .call(move |c| {
                    store_hours(c, t, &hours)?;
                    c.execute("INSERT INTO weather_fetch(fetched_at, provider, kind, ok, status, body) VALUES (?1, ?2, 'forecast', 1, 'ok', ?3)", params![t, pname, body])?;
                    Ok(())
                })
                .await?;
            tracing::info!("weather forecast refreshed ({n} hours)");
        }
        Err((msg, _ra)) => {
            let m2 = msg.clone();
            state
                .db
                .call(move |c| {
                    c.execute("INSERT INTO weather_fetch(fetched_at, provider, kind, ok, status) VALUES (?1, ?2, 'forecast', 0, ?3)", params![t, PROVIDER, m2])?;
                    db::log_event(c, "weather", "warn", &m2)
                })
                .await?;
            bail!(msg);
        }
    }
    if cfg.weather.air_quality {
        match fetch(
            state,
            http,
            &air_quality_url(&loc),
            cfg.weather.daily_request_budget,
        )
        .await
        {
            Ok(v) => {
                let rows = parse_air(&v);
                state
                    .db
                    .call(move |c| {
                        let tx = c.transaction()?;
                        for (ts, pm, aqi, aod) in rows {
                            tx.execute("INSERT OR REPLACE INTO kv_air(ts, pm25, aqi, aod) VALUES (?1,?2,?3,?4)", params![ts, pm, aqi, aod])?;
                        }
                        tx.commit()?;
                        Ok(())
                    })
                    .await?;
            }
            Err((msg, _)) => tracing::warn!("air quality fetch failed: {msg}"),
        }
    }
    Ok(true)
}

pub async fn run(state: Shared, cancel: CancellationToken) {
    let http = match http_client() {
        Ok(h) => h,
        Err(e) => {
            tracing::error!("weather HTTP client: {e:#}");
            return;
        }
    };
    loop {
        match refresh_if_due(&state, &http, false).await {
            Ok(true) => {
                state.intel_tx.send_modify(|v| *v = now());
            }
            Ok(false) => {}
            Err(e) => tracing::debug!("weather: {e:#}"),
        }
        tokio::select! { _ = cancel.cancelled() => break, _ = tokio::time::sleep(Duration::from_secs(120)) => {} }
    }
}

/// WMO weather interpretation code → short description.
pub fn wmo_text(code: i64) -> &'static str {
    match code {
        0 => "Clear",
        1 => "Mainly clear",
        2 => "Partly cloudy",
        3 => "Overcast",
        45 | 48 => "Fog",
        51 | 53 | 55 => "Drizzle",
        56 | 57 => "Freezing drizzle",
        61 | 63 | 65 => "Rain",
        66 | 67 => "Freezing rain",
        71 | 73 | 75 | 77 => "Snow",
        80..=82 => "Rain showers",
        85 | 86 => "Snow showers",
        95 => "Thunderstorm",
        96 | 99 => "Thunderstorm with hail",
        _ => "Unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_rounds_location_and_requests_needed_vars() {
        let loc = Location {
            latitude: 37.774_93,
            longitude: -122.419_42,
            elevation_m: None,
            time_zone: "UTC".into(),
        };
        let u = forecast_url(&loc, 7);
        assert!(u.contains("latitude=37.77&longitude=-122.42"));
        assert!(
            u.contains("direct_normal_irradiance")
                && u.contains("diffuse_radiation")
                && u.contains("timeformat=unixtime")
        );
        assert!(
            u.starts_with("https://api.open-meteo.com/v1/forecast?"),
            "free endpoint only"
        );
    }

    #[test]
    fn parses_and_stores_versions() {
        let body = serde_json::json!({"hourly": {"time": [3600, 7200], "temperature_2m": [10.0, null], "shortwave_radiation": [0.0, 400.5],
            "direct_normal_irradiance": [0, 600], "diffuse_radiation": [0, 100], "weather_code": [0, 3]}});
        let hours = parse_forecast(&body).unwrap();
        assert_eq!(hours.len(), 2);
        assert_eq!(hours[1].temp_c, None);
        assert_eq!(hours[1].ghi, Some(400.5));
        assert_eq!(hours[1].code, Some(3));
        let db = crate::db::Db::open_memory().unwrap();
        db.with(|c| {
            store_hours(c, 100, &hours)?;
            let mut newer = hours.clone();
            newer[1].ghi = Some(200.0);
            store_hours(c, 200, &newer)?;
            assert_eq!(latest_hours(c, 0, 10_000)?[1].ghi, Some(200.0));
            assert_eq!(
                hours_as_of(c, 0, 10_000, 150)?[1].ghi,
                Some(400.5),
                "no peeking at later fetches"
            );
            assert!(hours_as_of(c, 0, 10_000, 50)?.is_empty());
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn wmo_codes() {
        assert_eq!(wmo_text(0), "Clear");
        assert_eq!(wmo_text(63), "Rain");
        assert_eq!(wmo_text(1234), "Unknown");
    }
}
