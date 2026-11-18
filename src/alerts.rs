//! Alerts: evaluated after each intelligence pass. Each condition has a stable key; an
//! active alert is updated in place (no duplicates), cleared when the condition ends,
//! and not re-raised within `repeat_after_min`. Notifications (optional webhook) respect
//! quiet hours; alerts are always recorded in-app.

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

use crate::db::{self, now};
use crate::forecast;
use crate::intel::Intel;
use crate::state::Shared;

#[derive(Debug, Clone, PartialEq)]
pub struct Condition {
    pub key: String,
    pub severity: &'static str,
    pub title: String,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AlertRow {
    pub id: i64,
    pub key: String,
    pub severity: String,
    pub title: String,
    pub detail: String,
    pub created_at: i64,
    pub last_seen: i64,
    pub cleared_at: Option<i64>,
    pub acked_at: Option<i64>,
    pub count: i64,
}

fn cond(
    key: &str,
    severity: &'static str,
    title: impl Into<String>,
    detail: impl Into<String>,
) -> Condition {
    Condition {
        key: key.into(),
        severity,
        title: title.into(),
        detail: detail.into(),
    }
}

/// Build the list of currently true conditions.
pub async fn conditions(state: &Shared, intel: &Intel) -> Result<Vec<Condition>> {
    let cfg = state.cfg();
    let live = state.live();
    let stats = state.stats.lock().expect("stats").clone();
    let t = now();
    let mut out = Vec::new();

    if !state.demo && cfg.collector.enabled && !cfg.gateway.host.is_empty() {
        let down_for = stats
            .last_success_at
            .map(|s| t - s)
            .unwrap_or(t - stats.started_at);
        match live.collector.as_str() {
            "no_token" => out.push(cond("gateway_token_missing", "warn", "Gateway token needed", "Add your Enphase gateway token in Settings to start collecting data.")),
            "unauthorized" => out.push(cond("gateway_token_rejected", "critical", "Gateway token rejected", "The gateway refused the token; it has probably expired. Generate a new token at entrez.enphaseenergy.com and enter it in Settings.")),
            "tls" => out.push(cond("gateway_tls", "critical", "Gateway certificate changed", "The gateway's certificate no longer matches the pinned fingerprint. If you replaced or reset the gateway, re-pin it in Settings.")),
            _ if down_for > cfg.collector.warn_after_s() && live.consecutive_failures > 0 => out.push(cond(
                "gateway_offline",
                "critical",
                "Gateway offline",
                format!("No successful reading for {} minutes. Last error: {}", down_for / 60, live.last_error.clone().unwrap_or_default()),
            )),
            _ => {}
        }
        if live.warn && live.consecutive_failures == 0 && live.received_at.is_some() {
            out.push(cond(
                "telemetry_stale",
                "warn",
                "Telemetry not updating",
                format!(
                    "The gateway answers but its measurement time has not advanced for {} s.",
                    live.age_s.unwrap_or(0)
                ),
            ));
        }
    }
    // Token expiry.
    let token_info = crate::secrets::load_token(&crate::secrets::token_path(
        cfg.gateway.token_file.as_deref(),
        &state.data_dir,
    ))
    .ok()
    .flatten()
    .map(|t| t.info());
    if let Some(d) = token_info.and_then(|i| i.days_left(t))
        && d < 14.0
    {
        out.push(cond("token_expiring", if d < 3.0 { "critical" } else { "warn" }, "Gateway token expiring", format!("Your gateway token expires in {d:.0} days. Renew it at entrez.enphaseenergy.com and paste it in Settings.")));
    }
    if let Some(s) = &live.status
        && !live.stale
    {
        if let Some(soc) = s.soc_pct
            && soc < cfg.alerts.low_soc_pct
            && s.storage_w >= -50.0
        {
            out.push(cond(
                "low_soc",
                "warn",
                "Battery low",
                format!("Battery is at {soc:.0} % and not charging."),
            ));
        }
        if s.possibly_off_grid() {
            out.push(cond("possible_outage", "critical", "Possible grid outage", format!("The gateway reports main relay state {:?} (1 = grid connected on observed firmware). The home may be running on batteries.", s.main_relay_state)));
        }
        let surplus = s.pv_w - s.load_w;
        if surplus > 1500.0
            && s.soc_pct.is_some_and(|x| x < 95.0)
            && s.storage_w > -100.0
            && s.grid_w < -1000.0
        {
            out.push(cond("not_charging", "warn", "Battery not charging", format!("{:.1} kW of surplus solar is being exported while the battery is at {:.0} % and not charging.", surplus / 1000.0, s.soc_pct.unwrap_or(0.0))));
        }
    }
    // Only meaningful with a solar forecast; without one the simulation assumes no sun.
    if let (Some(tr), Some(sr)) = (
        intel.battery.time_to_reserve,
        intel
            .sun_tomorrow
            .as_ref()
            .and_then(|s| s.sunrise)
            .map(|x| x.timestamp()),
    ) && intel.solar.is_some()
        && tr < sr
        && tr - t < 12 * 3600
    {
        out.push(cond("low_overnight", "info", "Battery may reach reserve overnight", "The battery simulation reaches the reserve before sunrise; the grid will cover the rest."));
    }
    for r in &intel.recommendations {
        if r.id == "underperformance" {
            out.push(cond(
                "solar_underperformance",
                "warn",
                r.title.clone(),
                r.message.clone(),
            ));
        }
        if r.id == "high-load" {
            out.push(cond(
                "high_load",
                "info",
                r.title.clone(),
                r.message.clone(),
            ));
        }
    }
    // Weather-based alerts over the next 24 h.
    let wx = state
        .db
        .call(move |c| crate::weather::latest_hours(c, t, t + 86400))
        .await
        .unwrap_or_default();
    if let Some(w) = wx
        .iter()
        .find(|w| matches!(w.code, Some(95..=99)) || w.gust_ms.is_some_and(|g| g > 20.0))
    {
        let tz = cfg
            .location
            .as_ref()
            .map(|l| l.tz())
            .unwrap_or(chrono_tz::UTC);
        let when = forecast::utc(w.ts).with_timezone(&tz).format("%a %-I %p");
        out.push(cond(
            "severe_weather",
            "warn",
            "Severe weather forecast",
            format!(
                "{} expected around {when} (gusts {:.0} m/s).",
                crate::weather::wmo_text(w.code.unwrap_or(0)),
                w.gust_ms.unwrap_or(0.0)
            ),
        ));
    }
    if let Some(w) = wx
        .iter()
        .find(|w| w.aqi.is_some_and(|a| a > 150.0) || w.pm25.is_some_and(|p| p > 55.0))
    {
        out.push(cond("smoke", "warn", "Smoke or poor air quality", format!("Air quality forecast: US AQI {:.0}, PM2.5 {:.0} µg/m³. Smoke can reduce solar output noticeably.", w.aqi.unwrap_or(0.0), w.pm25.unwrap_or(0.0))));
    }
    // Daily forecast error (yesterday).
    if let Some(loc) = &cfg.location {
        let tz = loc.tz();
        let yday = forecast::local_date(t, tz).pred_opt();
        if let Some(y) = yday {
            let (a, b) = forecast::day_bounds(y, tz);
            let pairs = state
                .db
                .call(move |c| crate::intel::forecast_pairs(c, "pv_wh", a, b, 6 * 3600))
                .await
                .unwrap_or_default();
            let (f, act): (f64, f64) = pairs.iter().fold((0.0, 0.0), |s, p| (s.0 + p.1, s.1 + p.2));
            if pairs.len() >= 8 && act > 3000.0 && ((f - act) / act).abs() > 0.4 {
                out.push(cond("forecast_error", "info", "Large forecast error yesterday", format!("Yesterday's solar forecast was {f:.0} Wh vs {act:.0} Wh measured ({:+.0} %).", (f - act) / act * 100.0)));
            }
        }
    }
    if cfg.weather.enabled && cfg.location.is_some() {
        let wc = cfg.weather.clone();
        if let Ok(ws) = state.db.call(move |c| crate::weather::status(c, &wc)).await
            && ws.last_success_at.is_some_and(|s| t - s > 6 * 3600)
        {
            out.push(cond(
                "weather_stale",
                "info",
                "Weather forecast out of date",
                format!(
                    "Last successful weather update was {} hours ago; using the cached forecast.",
                    (t - ws.last_success_at.unwrap_or(t)) / 3600
                ),
            ));
        }
    }
    Ok(out)
}

/// Apply conditions to the alerts table. Returns newly raised alerts.
pub fn apply(
    c: &mut Connection,
    conds: &[Condition],
    repeat_after_min: i64,
    t: i64,
) -> Result<Vec<Condition>> {
    let tx = c.transaction()?;
    let mut raised = Vec::new();
    for k in conds {
        let active: Option<i64> = tx
            .query_row(
                "SELECT id FROM alerts WHERE key = ?1 AND cleared_at IS NULL",
                [&k.key],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(id) = active {
            tx.execute("UPDATE alerts SET last_seen = ?1, detail = ?2, title = ?3, severity = ?4 WHERE id = ?5", params![t, k.detail, k.title, k.severity, id])?;
            continue;
        }
        let recent: Option<i64> = tx
            .query_row(
                "SELECT id FROM alerts WHERE key = ?1 AND cleared_at > ?2 ORDER BY id DESC LIMIT 1",
                params![k.key, t - repeat_after_min * 60],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(id) = recent {
            // Flapping: reopen the recent alert instead of creating a new one.
            tx.execute("UPDATE alerts SET cleared_at = NULL, last_seen = ?1, count = count + 1, detail = ?2 WHERE id = ?3", params![t, k.detail, id])?;
            continue;
        }
        tx.execute(
            "INSERT INTO alerts(key, severity, title, detail, created_at, last_seen) VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
            params![k.key, k.severity, k.title, k.detail, t],
        )?;
        raised.push(k.clone());
    }
    // Clear alerts whose condition is gone.
    let keys: Vec<&str> = conds.iter().map(|k| k.key.as_str()).collect();
    let active: Vec<(i64, String)> = {
        let mut st = tx.prepare("SELECT id, key FROM alerts WHERE cleared_at IS NULL")?;
        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?
    };
    for (id, key) in active {
        if !keys.contains(&key.as_str()) {
            tx.execute(
                "UPDATE alerts SET cleared_at = ?1 WHERE id = ?2",
                params![t, id],
            )?;
        }
    }
    tx.commit()?;
    Ok(raised)
}

pub fn list(c: &Connection, active_only: bool, limit: i64) -> Result<Vec<AlertRow>> {
    let sql = if active_only {
        "SELECT id, key, severity, title, detail, created_at, last_seen, cleared_at, acked_at, count FROM alerts WHERE cleared_at IS NULL ORDER BY id DESC LIMIT ?1"
    } else {
        "SELECT id, key, severity, title, detail, created_at, last_seen, cleared_at, acked_at, count FROM alerts ORDER BY id DESC LIMIT ?1"
    };
    let mut st = c.prepare(sql)?;
    let rows = st.query_map([limit], |r| {
        Ok(AlertRow {
            id: r.get(0)?,
            key: r.get(1)?,
            severity: r.get(2)?,
            title: r.get(3)?,
            detail: r.get(4)?,
            created_at: r.get(5)?,
            last_seen: r.get(6)?,
            cleared_at: r.get(7)?,
            acked_at: r.get(8)?,
            count: r.get(9)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn ack(c: &Connection, id: i64) -> Result<bool> {
    Ok(c.execute(
        "UPDATE alerts SET acked_at = ?1 WHERE id = ?2 AND acked_at IS NULL",
        params![now(), id],
    )? > 0)
}

pub fn in_quiet_hours(local_hour: u32, start: u32, end: u32) -> bool {
    if start == end {
        false
    } else if start < end {
        (start..end).contains(&local_hour)
    } else {
        local_hour >= start || local_hour < end
    }
}

pub async fn evaluate(state: &Shared, intel: &Intel) -> Result<()> {
    let cfg = state.cfg();
    let conds = conditions(state, intel).await?;
    let repeat = cfg.alerts.repeat_after_min;
    let t = now();
    let raised = state.db.call(move |c| apply(c, &conds, repeat, t)).await?;
    if raised.is_empty() {
        return Ok(());
    }
    for r in &raised {
        tracing::info!("alert raised: {} — {}", r.title, r.detail);
    }
    let tz = cfg
        .location
        .as_ref()
        .map(|l| l.tz())
        .unwrap_or(chrono_tz::UTC);
    let quiet = in_quiet_hours(
        forecast::local_hour(t, tz),
        cfg.alerts.quiet_start_hour,
        cfg.alerts.quiet_end_hour,
    );
    if let Some(url) = cfg.alerts.webhook_url.clone() {
        let urgent: Vec<_> = raised
            .iter()
            .filter(|r| !quiet || r.severity == "critical")
            .collect();
        if !urgent.is_empty() {
            let body = serde_json::json!({
                "source": "RustSolar",
                "alerts": urgent.iter().map(|r| serde_json::json!({"key": r.key, "severity": r.severity, "title": r.title, "detail": r.detail})).collect::<Vec<_>>()
            });
            match crate::weather::http_client() {
                Ok(http) => {
                    if let Err(e) = http.post(&url).json(&body).send().await {
                        let msg = format!("webhook delivery failed: {e}");
                        let _ = state
                            .db
                            .call(move |c| db::log_event(c, "alerts", "warn", &msg))
                            .await;
                    }
                }
                Err(e) => tracing::warn!("webhook client: {e:#}"),
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedupe_clear_and_flap() {
        let db = crate::db::Db::open_memory().unwrap();
        db.with(|c| {
            let a = cond("low_soc", "warn", "Battery low", "x");
            assert_eq!(apply(c, std::slice::from_ref(&a), 60, 1000)?.len(), 1);
            assert_eq!(
                apply(c, std::slice::from_ref(&a), 60, 1100)?.len(),
                0,
                "no duplicate while active"
            );
            assert_eq!(list(c, true, 10)?.len(), 1);
            apply(c, &[], 60, 1200)?;
            assert_eq!(list(c, true, 10)?.len(), 0, "cleared");
            assert_eq!(
                apply(c, std::slice::from_ref(&a), 60, 1300)?.len(),
                0,
                "flap within repeat window reopens"
            );
            assert_eq!(list(c, false, 10)?[0].count, 2);
            apply(c, &[], 60, 1400)?;
            assert_eq!(
                apply(c, std::slice::from_ref(&a), 60, 1400 + 3601)?.len(),
                1,
                "new alert after window"
            );
            let id = list(c, true, 1)?[0].id;
            assert!(ack(c, id)?);
            assert!(!ack(c, id)?);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn quiet_hours_wrap() {
        assert!(in_quiet_hours(23, 22, 7));
        assert!(in_quiet_hours(3, 22, 7));
        assert!(!in_quiet_hours(12, 22, 7));
        assert!(in_quiet_hours(13, 12, 14));
        assert!(!in_quiet_hours(5, 5, 5));
    }
}
