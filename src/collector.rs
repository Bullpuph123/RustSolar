//! Telemetry collector: polls `/ivp/livedata/status` on a fixed cadence (never
//! overlapping), de-duplicates by gateway timestamp, integrates energy, detects stale
//! data, and backs off exponentially on failure.

use std::time::Duration;

use rustsolar_core::energy::{Sample, Step, integrate};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::db::{self, now};
use crate::gateway::model::{BatteryInventory, LiveStatus, ProductionTotals, parse_inverters};
use crate::gateway::{GatewayClient, GatewayError};
use crate::state::{LiveSnapshot, Shared};

const LIVE_PATH: &str = "/ivp/livedata/status";

/// Exponential backoff with ±20 % jitter, capped.
pub fn backoff_s(base_s: u64, failures: u32, max_s: u64) -> u64 {
    let exp = base_s.saturating_mul(1u64 << failures.min(10));
    let capped = exp.min(max_s).max(base_s);
    let jitter = (rand::random::<f64>() * 0.4 - 0.2) * capped as f64;
    ((capped as f64 + jitter).round() as u64).clamp(base_s, max_s.max(base_s))
}

pub async fn run(state: Shared, cancel: CancellationToken) {
    let cfg = state.cfg();
    if !cfg.collector.enabled {
        publish(&state, |s| s.collector = "disabled".into());
        return;
    }
    let Some(client) = state.gateway() else {
        publish(&state, |s| s.collector = "no_host".into());
        return;
    };
    let interval = cfg.collector.interval_s;
    let mut prev: Option<Sample> = state
        .db
        .call(|c| Ok(db::last_sample(c)?.map(|x| x.0)))
        .await
        .ok()
        .flatten();
    let mut failures: u32 = 0;
    let mut last_new_recv: Option<i64> = None;
    let mut last_logged_kind: Option<&'static str> = None;
    let mut next = Instant::now();
    publish(&state, |s| s.collector = "starting".into());

    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = tokio::time::sleep_until(next) => {}
        }
        let started = Instant::now();
        // Hard outer timeout as a second guard; reqwest already enforces request_timeout_s.
        let res = tokio::time::timeout(
            Duration::from_secs(cfg.gateway.request_timeout_s + 5),
            client.get_json(LIVE_PATH),
        )
        .await
        .unwrap_or(Err(GatewayError::Timeout));
        let recv = now();
        {
            let mut st = state.stats.lock().expect("stats");
            st.polls += 1;
        }
        match res {
            Ok(resp) => {
                let parsed = LiveStatus::from_value(&resp.value);
                if cfg.gateway.store_raw {
                    let body = resp.raw.clone();
                    let _ = state
                        .db
                        .call(move |c| db::store_raw(c, LIVE_PATH, &body))
                        .await;
                }
                match parsed {
                    Ok(live) => {
                        failures = 0;
                        if last_logged_kind.take().is_some() {
                            log(
                                &state,
                                "collector",
                                "info",
                                "gateway reachable again".into(),
                            )
                            .await;
                        }
                        let sample = live.sample();
                        let is_new = prev.is_none_or(|p| sample.ts > p.ts);
                        let (mut gap, mut dup) = (false, false);
                        if is_new {
                            let parts = match prev.map(|p| {
                                integrate(&p, &sample, 3600, cfg.collector.max_integration_gap_s)
                            }) {
                                Some(Step::Energy(p)) => p,
                                Some(Step::Gap { .. }) => {
                                    gap = true;
                                    vec![]
                                }
                                _ => vec![],
                            };
                            let l2 = live.clone();
                            match state
                                .db
                                .call(move |c| db::insert_sample(c, &l2, recv, &parts))
                                .await
                            {
                                Ok(_) => {}
                                Err(e) => tracing::error!("storing sample: {e:#}"),
                            }
                            let interval_s = prev.map(|p| sample.ts - p.ts);
                            prev = Some(sample);
                            last_new_recv = Some(recv);
                            let mut st = state.stats.lock().expect("stats");
                            st.last_update_interval_s = interval_s;
                        } else {
                            dup = true;
                        }
                        let balanced = sample.is_balanced(150.0, 0.05);
                        {
                            let mut st = state.stats.lock().expect("stats");
                            st.successes += 1;
                            st.duplicates += dup as u64;
                            st.gaps += gap as u64;
                            st.unbalanced += (!balanced) as u64;
                            st.last_latency_ms = Some(resp.latency_ms);
                            st.max_latency_ms =
                                Some(st.max_latency_ms.unwrap_or(0).max(resp.latency_ms));
                            st.avg_latency_ms = Some(match st.avg_latency_ms {
                                Some(a) => a * 0.95 + resp.latency_ms as f64 * 0.05,
                                None => resp.latency_ms as f64,
                            });
                            st.last_success_at = Some(recv);
                            st.current_backoff_s = 0;
                            st.clock_skew_s = Some(live.last_update - recv);
                        }
                        let age = last_new_recv.map(|t| recv - t);
                        let stale = age.is_none_or(|a| a > cfg.collector.stale_after_s as i64);
                        let warn = age.is_some_and(|a| a > cfg.collector.warn_after_s());
                        let flows = sample.flows();
                        publish(&state, |s| {
                            s.status = Some(live.clone());
                            s.flows = flows;
                            s.received_at = last_new_recv;
                            s.age_s = age;
                            s.stale = stale;
                            s.warn = warn;
                            s.residual_w = Some(sample.residual_w());
                            s.balanced = Some(balanced);
                            s.collector = "ok".into();
                            s.last_error = None;
                            s.consecutive_failures = 0;
                        });
                        next = started + Duration::from_secs(interval);
                        continue;
                    }
                    Err(e) => {
                        failures += 1;
                        let msg = format!("unexpected livedata format: {e}");
                        fail(
                            &state,
                            "parse",
                            &msg,
                            failures,
                            &mut last_logged_kind,
                            last_new_recv,
                            &cfg.collector,
                        )
                        .await;
                    }
                }
            }
            Err(e) => {
                failures += 1;
                fail(
                    &state,
                    e.kind(),
                    &e.to_string(),
                    failures,
                    &mut last_logged_kind,
                    last_new_recv,
                    &cfg.collector,
                )
                .await;
                if !e.transient() {
                    // Configuration problems: retry slowly, the user has to act.
                    failures = failures.max(4);
                }
            }
        }
        let wait = backoff_s(
            interval,
            failures.saturating_sub(1),
            cfg.collector.max_backoff_s,
        );
        state.stats.lock().expect("stats").current_backoff_s = wait;
        next = started + Duration::from_secs(wait);
    }
}

async fn fail(
    state: &Shared,
    kind: &'static str,
    msg: &str,
    failures: u32,
    last_logged: &mut Option<&'static str>,
    last_new_recv: Option<i64>,
    cc: &crate::config::CollectorConfig,
) {
    let t = now();
    let since_start;
    {
        let mut st = state.stats.lock().expect("stats");
        since_start = t - st.started_at;
        st.failures += 1;
        *st.failures_by_kind.entry(kind.to_string()).or_default() += 1;
        st.last_failure_at = Some(t);
        st.last_error = Some(msg.to_string());
    }
    // Log on change of failure type, not on every retry.
    if *last_logged != Some(kind) {
        *last_logged = Some(kind);
        tracing::warn!("gateway poll failed ({kind}): {msg}");
        log(
            state,
            "collector",
            "warn",
            format!("gateway poll failed ({kind}): {msg}"),
        )
        .await;
    }
    let collector = match kind {
        "no_token" | "unauthorized" | "tls" | "no_host" => kind,
        _ => "retrying",
    };
    let age = last_new_recv.map(|r| t - r);
    publish(state, |s| {
        s.collector = collector.into();
        s.last_error = Some(msg.to_string());
        s.consecutive_failures = failures;
        s.age_s = age;
        s.stale = age.is_none_or(|a| a > cc.stale_after_s as i64);
        // Before the first reading, count from start-up.
        s.warn = age.unwrap_or(since_start) > cc.warn_after_s();
    });
}

async fn log(state: &Shared, source: &'static str, level: &'static str, msg: String) {
    let _ = state
        .db
        .call(move |c| db::log_event(c, source, level, &msg))
        .await;
}

fn publish(state: &Shared, f: impl FnOnce(&mut LiveSnapshot)) {
    state.live_tx.send_modify(|s| {
        f(s);
        s.demo = state.demo;
    });
}

/// Keep `age_s`/`stale` current between polls (e.g. while backing off).
pub async fn staleness_ticker(state: Shared, cancel: CancellationToken) {
    let mut iv = tokio::time::interval(Duration::from_secs(5));
    loop {
        tokio::select! { _ = cancel.cancelled() => break, _ = iv.tick() => {} }
        let cfg = state.cfg();
        let stale_after = cfg.collector.stale_after_s as i64;
        let warn_after = cfg.collector.warn_after_s();
        let t = now();
        state.live_tx.send_if_modified(|s| {
            let Some(r) = s.received_at else { return false };
            let age = t - r;
            let stale = age > stale_after;
            let warn = age > warn_after;
            let changed = s.stale != stale || s.warn != warn || s.age_s != Some(age);
            s.age_s = Some(age);
            s.stale = stale;
            s.warn = warn;
            changed
        });
    }
}

/// Slow poll of read-only inventory/production endpoints. Results are cached in `kv`.
pub async fn inventory_task(state: Shared, client: GatewayClient, cancel: CancellationToken) {
    let every = state.cfg().collector.inventory_interval_s.max(300);
    // Let the live poller go first.
    tokio::select! { _ = cancel.cancelled() => return, _ = tokio::time::sleep(Duration::from_secs(20)) => {} }
    loop {
        for path in [
            "/ivp/ensemble/inventory",
            "/production.json?details=1",
            "/api/v1/production/inverters",
        ] {
            let r = client.get_json(path).await;
            let (status, value) = match r {
                Ok(resp) => {
                    let summary = match path {
                        "/ivp/ensemble/inventory" => BatteryInventory::from_value(&resp.value)
                            .map(|i| serde_json::to_value(i).unwrap_or_default()),
                        "/production.json?details=1" => Some(
                            serde_json::to_value(ProductionTotals::from_value(&resp.value))
                                .unwrap_or_default(),
                        ),
                        _ => Some(
                            serde_json::to_value(parse_inverters(&resp.value)).unwrap_or_default(),
                        ),
                    };
                    ("ok".to_string(), summary)
                }
                Err(e) => (format!("{}: {e}", e.kind()), None),
            };
            let p = path.to_string();
            let _ = state
                .db
                .call(move |c| {
                    db::kv_set(c, &format!("endpoint:{p}"), &status)?;
                    if let Some(v) = value {
                        db::kv_set(c, &format!("gw:{p}"), &v.to_string())?;
                    }
                    Ok(())
                })
                .await;
            tokio::select! { _ = cancel.cancelled() => return, _ = tokio::time::sleep(Duration::from_secs(2)) => {} }
        }
        tokio::select! { _ = cancel.cancelled() => return, _ = tokio::time::sleep(Duration::from_secs(every)) => {} }
    }
}

/// DEMO MODE ONLY: synthetic telemetry so the dashboard can be explored without a
/// gateway. Writes to a separate demo database and every API response is flagged `demo`.
pub async fn demo_run(state: Shared, cancel: CancellationToken) {
    let mut prev: Option<Sample> = state
        .db
        .call(|c| Ok(db::last_sample(c)?.map(|x| x.0)))
        .await
        .ok()
        .flatten();
    let mut soc: f64 = prev.map(|_| 60.0).unwrap_or(60.0);
    let mut iv = tokio::time::interval(Duration::from_secs(5));
    loop {
        tokio::select! { _ = cancel.cancelled() => break, _ = iv.tick() => {} }
        let t = now();
        let live = demo_reading(t, &mut soc, prev.map(|p| t - p.ts).unwrap_or(5));
        let sample = live.sample();
        let parts = match prev.map(|p| integrate(&p, &sample, 3600, 120)) {
            Some(Step::Energy(p)) => p,
            _ => vec![],
        };
        let l2 = live.clone();
        let _ = state
            .db
            .call(move |c| db::insert_sample(c, &l2, t, &parts))
            .await;
        prev = Some(sample);
        let flows = sample.flows();
        publish(&state, |s| {
            s.status = Some(live.clone());
            s.flows = flows;
            s.received_at = Some(t);
            s.age_s = Some(0);
            s.stale = false;
            s.warn = false;
            s.residual_w = Some(0.0);
            s.balanced = Some(true);
            s.collector = "ok".into();
        });
    }
}

/// Deterministic-ish synthetic reading (DEMO ONLY): bell-shaped solar, evening load bumps,
/// self-consumption battery with a 10 kWh pack and 15 % reserve. Always balanced.
pub fn demo_reading(t: i64, soc: &mut f64, dt_s: i64) -> LiveStatus {
    use chrono::Timelike;
    let local = chrono::DateTime::from_timestamp(t, 0)
        .unwrap_or_default()
        .with_timezone(&chrono_tz::America::Los_Angeles);
    let h = local.hour() as f64 + local.minute() as f64 / 60.0;
    let pv = if (7.0..18.0).contains(&h) {
        6500.0 * ((h - 7.0) / 11.0 * std::f64::consts::PI).sin().powi(2)
    } else {
        0.0
    };
    let wiggle = ((t as f64) / 97.0).sin() * 120.0;
    let load = 450.0
        + wiggle.abs()
        + if (17.0..21.0).contains(&h) {
            1800.0
        } else {
            0.0
        }
        + if (6.5..8.0).contains(&h) { 900.0 } else { 0.0 };
    let cap = 10_000.0;
    let net = pv - load;
    let storage = if net > 0.0 {
        if *soc < 100.0 {
            -(net.min(3840.0))
        } else {
            0.0
        }
    } else if *soc > 15.0 {
        (-net).min(3840.0)
    } else {
        0.0
    };
    *soc = (*soc - storage * dt_s as f64 / 3600.0 / cap * 100.0).clamp(0.0, 100.0);
    let grid = load - pv - storage;
    LiveStatus {
        last_update: t,
        pv_w: pv,
        storage_w: storage,
        grid_w: grid,
        load_w: load,
        gen_w: None,
        soc_pct: Some(soc.round()),
        backup_soc_pct: Some(15.0),
        enc_agg_energy: Some(*soc / 100.0 * cap),
        enc_agg_soc_pct: Some(soc.round()),
        main_relay_state: Some(1),
        gen_relay_state: None,
        phase_count: Some(2),
        sc_stream: Some("disabled".into()),
        mqtt_state: Some("connected".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_and_caps() {
        for _ in 0..50 {
            let b0 = backoff_s(12, 0, 300);
            assert!((12..=15).contains(&b0), "{b0}");
            let b3 = backoff_s(12, 3, 300);
            assert!((76..=116).contains(&b3), "{b3}");
            assert!(backoff_s(12, 20, 300) <= 300);
        }
    }

    #[test]
    fn demo_readings_balance() {
        let mut soc = 50.0;
        for i in 0..2000 {
            let r = demo_reading(1_792_900_000 + i * 60, &mut soc, 60);
            assert!(r.sample().residual_w().abs() < 1e-6);
        }
    }
}

#[cfg(test)]
mod integration {
    //! Collector loop against a fake HTTPS gateway whose timestamp advances 10 s on every
    //! second request (so every other reading is a repeat).
    use std::sync::{Arc, Mutex, RwLock};

    use tokio::sync::{Notify, watch};

    use super::*;
    use crate::gateway::tests::{cfg as gw_cfg, fake_gateway_fn, token};
    use crate::gateway::tls;
    use crate::state::{AppState, CollectorStats, LiveSnapshot};

    fn body(ts: i64) -> String {
        format!(
            r#"{{"meters":{{"last_update":{ts},"soc":50,"backup_soc":15,"pv":{{"agg_p_mw":0}},"storage":{{"agg_p_mw":0}},"grid":{{"agg_p_mw":1000000}},"load":{{"agg_p_mw":1000000}}}},"connection":{{"sc_stream":"disabled"}}}}"#
        )
    }

    #[tokio::test]
    async fn polls_dedupes_and_integrates() {
        let t0 = (now() - 3600).div_euclid(3600) * 3600 + 100;
        let (port, der) = fake_gateway_fn(move |n| (200, body(t0 + (n as i64 / 2) * 10))).await;
        let mut config = crate::config::Config {
            gateway: gw_cfg(
                port,
                Some(tls::format_fingerprint(&tls::fingerprint(&der.into()))),
            ),
            ..Default::default()
        };
        config.collector.interval_s = 1; // faster than production allows, to keep the test short
        let client = GatewayClient::new(&config.gateway, Some(token())).unwrap();
        let (live_tx, _) = watch::channel(LiveSnapshot::default());
        let (intel_tx, _) = watch::channel(0);
        let state: Shared = Arc::new(AppState {
            config: RwLock::new(Arc::new(config)),
            config_path: "unused.toml".into(),
            data_dir: std::env::temp_dir(),
            db: crate::db::Db::open_memory().unwrap(),
            gateway: RwLock::new(Some(client)),
            live_tx,
            stats: Mutex::new(CollectorStats::default()),
            sessions: Mutex::new(Default::default()),
            reload: Notify::new(),
            intel_tx,
            intel: RwLock::new(None),
            demo: false,
        });
        let cancel = CancellationToken::new();
        let task = tokio::spawn(run(state.clone(), cancel.clone()));
        // About five polls, one second apart.
        tokio::time::sleep(std::time::Duration::from_millis(4600)).await;
        cancel.cancel();
        let _ = task.await;
        let stats = state.stats.lock().unwrap().clone();
        assert!(stats.successes >= 4, "{stats:?}");
        assert!(
            stats.duplicates >= 1,
            "repeated readings detected: {stats:?}"
        );
        let (n, load_wh): (i64, f64) = state
            .db
            .with(|c| Ok(c.query_row("SELECT (SELECT COUNT(*) FROM samples), (SELECT COALESCE(SUM(load_wh),0) FROM energy_hourly)", [], |r| Ok((r.get(0)?, r.get(1)?)))?))
            .unwrap();
        // Distinct readings stored once each; 1 kW for (n-1)·10 s.
        assert_eq!(n as u64, stats.successes - stats.duplicates);
        assert!(
            (load_wh - (n - 1) as f64 * 10.0 / 3.6).abs() < 1e-6,
            "{load_wh} for {n} samples"
        );
        let live = state.live();
        assert_eq!(live.collector, "ok");
        assert!(live.status.is_some());
    }
}
