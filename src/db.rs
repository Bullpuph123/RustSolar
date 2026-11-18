//! SQLite storage. All timestamps are UTC Unix seconds. One connection behind a mutex;
//! heavy work runs on the blocking pool via [`Db::call`].

use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};
use rustsolar_core::energy::{Energy, Sample};
use serde::Serialize;

use crate::gateway::model::LiveStatus;

const MIGRATIONS: &[&str] = &[
    // 1: initial schema
    r#"
    CREATE TABLE samples(
        ts INTEGER PRIMARY KEY, recv_ts INTEGER NOT NULL,
        pv_w REAL NOT NULL, load_w REAL NOT NULL, storage_w REAL NOT NULL, grid_w REAL NOT NULL, gen_w REAL,
        soc REAL, backup_soc REAL, enc_energy REAL, relay INTEGER, residual_w REAL NOT NULL);
    CREATE TABLE energy_hourly(
        hour_ts INTEGER PRIMARY KEY,
        pv_wh REAL NOT NULL DEFAULT 0, load_wh REAL NOT NULL DEFAULT 0,
        import_wh REAL NOT NULL DEFAULT 0, export_wh REAL NOT NULL DEFAULT 0,
        charge_wh REAL NOT NULL DEFAULT 0, discharge_wh REAL NOT NULL DEFAULT 0, gen_wh REAL NOT NULL DEFAULT 0,
        pv_to_load_wh REAL NOT NULL DEFAULT 0, pv_to_batt_wh REAL NOT NULL DEFAULT 0, pv_to_grid_wh REAL NOT NULL DEFAULT 0,
        batt_to_load_wh REAL NOT NULL DEFAULT 0, batt_to_grid_wh REAL NOT NULL DEFAULT 0,
        grid_to_load_wh REAL NOT NULL DEFAULT 0, grid_to_batt_wh REAL NOT NULL DEFAULT 0,
        covered_s REAL NOT NULL DEFAULT 0, soc_end REAL, peak_load_w REAL, peak_pv_w REAL);
    CREATE TABLE agg_5min(
        ts INTEGER PRIMARY KEY, n INTEGER NOT NULL,
        pv_w REAL, load_w REAL, storage_w REAL, grid_w REAL, soc REAL);
    CREATE TABLE weather_fetch(
        id INTEGER PRIMARY KEY, fetched_at INTEGER NOT NULL, provider TEXT NOT NULL, kind TEXT NOT NULL,
        ok INTEGER NOT NULL, status TEXT, body TEXT);
    CREATE INDEX weather_fetch_t ON weather_fetch(kind, fetched_at);
    CREATE TABLE weather_hourly(
        ts INTEGER NOT NULL, fetched_at INTEGER NOT NULL,
        temp_c REAL, rh REAL, cloud REAL, cloud_low REAL, cloud_mid REAL, cloud_high REAL,
        ghi REAL, dni REAL, dhi REAL, gti REAL, precip_prob REAL, precip_mm REAL, snow_cm REAL,
        wind_ms REAL, gust_ms REAL, visibility_m REAL, code INTEGER, is_day INTEGER,
        pm25 REAL, aqi REAL, aod REAL,
        PRIMARY KEY(ts, fetched_at));
    CREATE TABLE forecasts(
        kind TEXT NOT NULL, created_at INTEGER NOT NULL, target_ts INTEGER NOT NULL,
        model TEXT NOT NULL, value REAL NOT NULL, lo REAL, hi REAL,
        PRIMARY KEY(kind, created_at, target_ts));
    CREATE INDEX forecasts_target ON forecasts(kind, target_ts);
    CREATE TABLE model_params(key TEXT PRIMARY KEY, value TEXT NOT NULL, updated_at INTEGER NOT NULL);
    CREATE TABLE alerts(
        id INTEGER PRIMARY KEY, key TEXT NOT NULL, severity TEXT NOT NULL, title TEXT NOT NULL, detail TEXT NOT NULL,
        created_at INTEGER NOT NULL, last_seen INTEGER NOT NULL, cleared_at INTEGER, acked_at INTEGER,
        count INTEGER NOT NULL DEFAULT 1, notified INTEGER NOT NULL DEFAULT 0);
    CREATE INDEX alerts_key ON alerts(key, cleared_at);
    CREATE TABLE events(id INTEGER PRIMARY KEY, ts INTEGER NOT NULL, source TEXT NOT NULL, level TEXT NOT NULL, message TEXT NOT NULL);
    CREATE INDEX events_ts ON events(ts);
    CREATE TABLE raw_responses(id INTEGER PRIMARY KEY, ts INTEGER NOT NULL, path TEXT NOT NULL, body TEXT NOT NULL);
    CREATE TABLE kv(key TEXT PRIMARY KEY, value TEXT NOT NULL, updated_at INTEGER NOT NULL);
    CREATE TABLE annotations(id INTEGER PRIMARY KEY, start_ts INTEGER NOT NULL, end_ts INTEGER, label TEXT NOT NULL, watts REAL);
    CREATE TABLE api_usage(day TEXT NOT NULL, provider TEXT NOT NULL, count INTEGER NOT NULL, PRIMARY KEY(day, provider));
    "#,
    // 2: air quality (smoke) by hour
    "CREATE TABLE IF NOT EXISTS kv_air(ts INTEGER PRIMARY KEY, pm25 REAL, aqi REAL, aod REAL);",
];

#[derive(Clone)]
pub struct Db {
    conn: Arc<Mutex<Connection>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StoredSample {
    pub ts: i64,
    pub pv_w: f64,
    pub load_w: f64,
    pub storage_w: f64,
    pub grid_w: f64,
    pub soc: Option<f64>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct HourRow {
    pub hour_ts: i64,
    pub e: EnergyRow,
    pub soc_end: Option<f64>,
    pub peak_load_w: Option<f64>,
    pub peak_pv_w: Option<f64>,
}

/// Serializable mirror of [`Energy`].
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct EnergyRow {
    pub pv_wh: f64,
    pub load_wh: f64,
    pub import_wh: f64,
    pub export_wh: f64,
    pub charge_wh: f64,
    pub discharge_wh: f64,
    pub gen_wh: f64,
    pub pv_to_load_wh: f64,
    pub pv_to_batt_wh: f64,
    pub pv_to_grid_wh: f64,
    pub batt_to_load_wh: f64,
    pub batt_to_grid_wh: f64,
    pub grid_to_load_wh: f64,
    pub grid_to_batt_wh: f64,
    pub covered_s: f64,
}

impl From<EnergyRow> for Energy {
    fn from(r: EnergyRow) -> Self {
        Energy {
            pv_wh: r.pv_wh,
            load_wh: r.load_wh,
            import_wh: r.import_wh,
            export_wh: r.export_wh,
            charge_wh: r.charge_wh,
            discharge_wh: r.discharge_wh,
            gen_wh: r.gen_wh,
            pv_to_load_wh: r.pv_to_load_wh,
            pv_to_batt_wh: r.pv_to_batt_wh,
            pv_to_grid_wh: r.pv_to_grid_wh,
            batt_to_load_wh: r.batt_to_load_wh,
            batt_to_grid_wh: r.batt_to_grid_wh,
            grid_to_load_wh: r.grid_to_load_wh,
            grid_to_batt_wh: r.grid_to_batt_wh,
            covered_s: r.covered_s,
        }
    }
}

impl From<Energy> for EnergyRow {
    fn from(e: Energy) -> Self {
        EnergyRow {
            pv_wh: e.pv_wh,
            load_wh: e.load_wh,
            import_wh: e.import_wh,
            export_wh: e.export_wh,
            charge_wh: e.charge_wh,
            discharge_wh: e.discharge_wh,
            gen_wh: e.gen_wh,
            pv_to_load_wh: e.pv_to_load_wh,
            pv_to_batt_wh: e.pv_to_batt_wh,
            pv_to_grid_wh: e.pv_to_grid_wh,
            batt_to_load_wh: e.batt_to_load_wh,
            batt_to_grid_wh: e.batt_to_grid_wh,
            grid_to_load_wh: e.grid_to_load_wh,
            grid_to_batt_wh: e.grid_to_batt_wh,
            covered_s: e.covered_s,
        }
    }
}

const ENERGY_COLS: &str = "pv_wh, load_wh, import_wh, export_wh, charge_wh, discharge_wh, gen_wh, pv_to_load_wh, pv_to_batt_wh, pv_to_grid_wh, batt_to_load_wh, batt_to_grid_wh, grid_to_load_wh, grid_to_batt_wh, covered_s";

fn energy_from_row(r: &rusqlite::Row<'_>, off: usize) -> rusqlite::Result<EnergyRow> {
    Ok(EnergyRow {
        pv_wh: r.get(off)?,
        load_wh: r.get(off + 1)?,
        import_wh: r.get(off + 2)?,
        export_wh: r.get(off + 3)?,
        charge_wh: r.get(off + 4)?,
        discharge_wh: r.get(off + 5)?,
        gen_wh: r.get(off + 6)?,
        pv_to_load_wh: r.get(off + 7)?,
        pv_to_batt_wh: r.get(off + 8)?,
        pv_to_grid_wh: r.get(off + 9)?,
        batt_to_load_wh: r.get(off + 10)?,
        batt_to_grid_wh: r.get(off + 11)?,
        grid_to_load_wh: r.get(off + 12)?,
        grid_to_batt_wh: r.get(off + 13)?,
        covered_s: r.get(off + 14)?,
    })
}

pub fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

impl Db {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        Self::init(conn)
    }

    pub fn open_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(mut conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        for (i, m) in MIGRATIONS.iter().enumerate().skip(version as usize) {
            let tx = conn.transaction()?;
            tx.execute_batch(m)
                .with_context(|| format!("migration {}", i + 1))?;
            tx.pragma_update(None, "user_version", (i + 1) as i64)?;
            tx.commit()?;
            tracing::info!("database migrated to version {}", i + 1);
        }
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Run a closure with the connection on the blocking thread pool.
    pub async fn call<F, R>(&self, f: F) -> Result<R>
    where
        F: FnOnce(&mut Connection) -> Result<R> + Send + 'static,
        R: Send + 'static,
    {
        let c = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let mut g = c.lock().map_err(|_| anyhow::anyhow!("db mutex poisoned"))?;
            f(&mut g)
        })
        .await?
    }

    /// Synchronous access (CLI commands and tests).
    pub fn with<R>(&self, f: impl FnOnce(&mut Connection) -> Result<R>) -> Result<R> {
        let mut g = self
            .conn
            .lock()
            .map_err(|_| anyhow::anyhow!("db mutex poisoned"))?;
        f(&mut g)
    }
}

// ---------- samples and energy ----------

/// Insert a sample and add its integrated energy, atomically. Returns false if the
/// timestamp was already stored (duplicate reading).
pub fn insert_sample(
    c: &mut Connection,
    s: &LiveStatus,
    recv_ts: i64,
    parts: &[(i64, Energy)],
) -> Result<bool> {
    let tx = c.transaction()?;
    let sample = s.sample();
    let n = tx.execute(
        "INSERT OR IGNORE INTO samples(ts, recv_ts, pv_w, load_w, storage_w, grid_w, gen_w, soc, backup_soc, enc_energy, relay, residual_w)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
        params![s.last_update, recv_ts, s.pv_w, s.load_w, s.storage_w, s.grid_w, s.gen_w, s.soc_pct, s.backup_soc_pct, s.enc_agg_energy, s.main_relay_state, sample.residual_w()],
    )?;
    if n == 0 {
        return Ok(false);
    }
    for (hour, e) in parts {
        add_energy(&tx, *hour, e)?;
    }
    // Hourly SOC and peaks for the hour containing this sample.
    let hour = s.last_update.div_euclid(3600) * 3600;
    tx.execute(
        "INSERT INTO energy_hourly(hour_ts, soc_end, peak_load_w, peak_pv_w) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(hour_ts) DO UPDATE SET soc_end = COALESCE(excluded.soc_end, soc_end),
           peak_load_w = MAX(COALESCE(peak_load_w, 0), excluded.peak_load_w),
           peak_pv_w = MAX(COALESCE(peak_pv_w, 0), excluded.peak_pv_w)",
        params![hour, s.soc_pct, s.load_w.max(0.0), s.pv_w.max(0.0)],
    )?;
    let b5 = s.last_update.div_euclid(300) * 300;
    tx.execute(
        "INSERT INTO agg_5min(ts, n, pv_w, load_w, storage_w, grid_w, soc) VALUES (?1, 1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(ts) DO UPDATE SET
           pv_w = (pv_w * n + excluded.pv_w) / (n + 1), load_w = (load_w * n + excluded.load_w) / (n + 1),
           storage_w = (storage_w * n + excluded.storage_w) / (n + 1), grid_w = (grid_w * n + excluded.grid_w) / (n + 1),
           soc = COALESCE(excluded.soc, soc), n = n + 1",
        params![b5, s.pv_w, s.load_w, s.storage_w, s.grid_w, s.soc_pct],
    )?;
    tx.commit()?;
    Ok(true)
}

fn add_energy(tx: &rusqlite::Transaction<'_>, hour: i64, e: &Energy) -> Result<()> {
    tx.execute(
        &format!(
            "INSERT INTO energy_hourly(hour_ts, {ENERGY_COLS}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)
             ON CONFLICT(hour_ts) DO UPDATE SET
               pv_wh = pv_wh + excluded.pv_wh, load_wh = load_wh + excluded.load_wh,
               import_wh = import_wh + excluded.import_wh, export_wh = export_wh + excluded.export_wh,
               charge_wh = charge_wh + excluded.charge_wh, discharge_wh = discharge_wh + excluded.discharge_wh,
               gen_wh = gen_wh + excluded.gen_wh,
               pv_to_load_wh = pv_to_load_wh + excluded.pv_to_load_wh, pv_to_batt_wh = pv_to_batt_wh + excluded.pv_to_batt_wh,
               pv_to_grid_wh = pv_to_grid_wh + excluded.pv_to_grid_wh, batt_to_load_wh = batt_to_load_wh + excluded.batt_to_load_wh,
               batt_to_grid_wh = batt_to_grid_wh + excluded.batt_to_grid_wh, grid_to_load_wh = grid_to_load_wh + excluded.grid_to_load_wh,
               grid_to_batt_wh = grid_to_batt_wh + excluded.grid_to_batt_wh, covered_s = covered_s + excluded.covered_s"
        ),
        params![
            hour, e.pv_wh, e.load_wh, e.import_wh, e.export_wh, e.charge_wh, e.discharge_wh, e.gen_wh,
            e.pv_to_load_wh, e.pv_to_batt_wh, e.pv_to_grid_wh, e.batt_to_load_wh, e.batt_to_grid_wh,
            e.grid_to_load_wh, e.grid_to_batt_wh, e.covered_s
        ],
    )?;
    Ok(())
}

/// Last stored sample with its SOC and backup-reserve SOC.
pub type LastSample = (Sample, Option<f64>, Option<f64>);

pub fn last_sample(c: &Connection) -> Result<Option<LastSample>> {
    Ok(c.query_row(
        "SELECT ts, pv_w, load_w, storage_w, grid_w, COALESCE(gen_w,0), soc, backup_soc FROM samples ORDER BY ts DESC LIMIT 1",
        [],
        |r| {
            Ok((
                Sample { ts: r.get(0)?, pv_w: r.get(1)?, load_w: r.get(2)?, storage_w: r.get(3)?, grid_w: r.get(4)?, gen_w: r.get(5)? },
                r.get(6)?,
                r.get(7)?,
            ))
        },
    )
    .optional()?)
}

/// Rebuild hourly energy from raw samples in [from, to). Used by `rustsolar rebuild-energy`.
pub fn rebuild_energy(c: &mut Connection, from: i64, to: i64, max_gap_s: i64) -> Result<usize> {
    let samples: Vec<Sample> = {
        let mut st = c.prepare("SELECT ts, pv_w, load_w, storage_w, grid_w, COALESCE(gen_w,0) FROM samples WHERE ts >= ?1 AND ts < ?2 ORDER BY ts")?;
        st.query_map(params![from, to], |r| {
            Ok(Sample {
                ts: r.get(0)?,
                pv_w: r.get(1)?,
                load_w: r.get(2)?,
                storage_w: r.get(3)?,
                grid_w: r.get(4)?,
                gen_w: r.get(5)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?
    };
    let (buckets, _gaps) = rustsolar_core::energy::integrate_series(&samples, 3600, max_gap_s);
    let tx = c.transaction()?;
    tx.execute(
        &format!(
            "UPDATE energy_hourly SET {} WHERE hour_ts >= ?1 AND hour_ts < ?2",
            ENERGY_COLS
                .split(", ")
                .map(|c| format!("{c} = 0"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        params![from, to],
    )?;
    for (h, e) in &buckets {
        add_energy(&tx, *h, e)?;
    }
    tx.commit()?;
    Ok(buckets.len())
}

/// Power series for charts, averaged into `points` buckets, choosing the finest stored table.
pub fn power_series(
    c: &Connection,
    from: i64,
    to: i64,
    points: i64,
    raw_days: i64,
) -> Result<Vec<StoredSample>> {
    let span = (to - from).max(1);
    let bucket = (span / points.max(1)).max(1);
    let raw_ok = from >= now() - raw_days * 86400;
    let (table, min_bucket) = if span <= 3 * 86400 && raw_ok {
        ("samples", 1)
    } else if span <= 120 * 86400 {
        ("agg_5min", 300)
    } else {
        ("", 3600)
    };
    let bucket = bucket.max(min_bucket);
    if table.is_empty() {
        // Long ranges: average power from hourly energy.
        let mut st = c.prepare(
            "SELECT (hour_ts / ?3) * ?3 AS b, SUM(pv_wh), SUM(load_wh), SUM(discharge_wh - charge_wh), SUM(import_wh - export_wh), AVG(soc_end), SUM(covered_s)
             FROM energy_hourly WHERE hour_ts >= ?1 AND hour_ts < ?2 GROUP BY b ORDER BY b",
        )?;
        let rows = st.query_map(params![from, to, bucket], |r| {
            let cov: f64 = r.get::<_, Option<f64>>(6)?.unwrap_or(0.0);
            let h = (cov / 3600.0).max(1e-9);
            Ok(StoredSample {
                ts: r.get(0)?,
                pv_w: r.get::<_, Option<f64>>(1)?.unwrap_or(0.0) / h,
                load_w: r.get::<_, Option<f64>>(2)?.unwrap_or(0.0) / h,
                storage_w: r.get::<_, Option<f64>>(3)?.unwrap_or(0.0) / h,
                grid_w: r.get::<_, Option<f64>>(4)?.unwrap_or(0.0) / h,
                soc: r.get(5)?,
            })
        })?;
        return Ok(rows.collect::<rusqlite::Result<_>>()?);
    }
    let mut st = c.prepare(&format!(
        "SELECT (ts / ?3) * ?3 AS b, AVG(pv_w), AVG(load_w), AVG(storage_w), AVG(grid_w), AVG(soc)
         FROM {table} WHERE ts >= ?1 AND ts < ?2 GROUP BY b ORDER BY b"
    ))?;
    let rows = st.query_map(params![from, to, bucket], |r| {
        Ok(StoredSample {
            ts: r.get(0)?,
            pv_w: r.get(1)?,
            load_w: r.get(2)?,
            storage_w: r.get(3)?,
            grid_w: r.get(4)?,
            soc: r.get(5)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn hourly(c: &Connection, from: i64, to: i64) -> Result<Vec<HourRow>> {
    let mut st = c.prepare(&format!(
        "SELECT hour_ts, {ENERGY_COLS}, soc_end, peak_load_w, peak_pv_w FROM energy_hourly WHERE hour_ts >= ?1 AND hour_ts < ?2 ORDER BY hour_ts"
    ))?;
    let rows = st.query_map(params![from, to], |r| {
        Ok(HourRow {
            hour_ts: r.get(0)?,
            e: energy_from_row(r, 1)?,
            soc_end: r.get(16)?,
            peak_load_w: r.get(17)?,
            peak_pv_w: r.get(18)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn energy_sum(c: &Connection, from: i64, to: i64) -> Result<Energy> {
    let r = c.query_row(
        &format!(
            "SELECT {} FROM energy_hourly WHERE hour_ts >= ?1 AND hour_ts < ?2",
            ENERGY_COLS
                .split(", ")
                .map(|c| format!("COALESCE(SUM({c}),0)"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        params![from, to],
        |r| energy_from_row(r, 0),
    )?;
    Ok(r.into())
}

pub fn first_sample_ts(c: &Connection) -> Result<Option<i64>> {
    Ok(c.query_row("SELECT MIN(ts) FROM samples", [], |r| r.get(0))?)
}

pub fn first_hour_ts(c: &Connection) -> Result<Option<i64>> {
    Ok(c.query_row(
        "SELECT MIN(hour_ts) FROM energy_hourly WHERE covered_s > 0",
        [],
        |r| r.get(0),
    )?)
}

// ---------- key/value, events, raw ----------

pub fn kv_set(c: &Connection, key: &str, value: &str) -> Result<()> {
    c.execute(
        "INSERT INTO kv(key, value, updated_at) VALUES (?1, ?2, ?3) ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        params![key, value, now()],
    )?;
    Ok(())
}

pub fn kv_get(c: &Connection, key: &str) -> Result<Option<(String, i64)>> {
    Ok(c.query_row(
        "SELECT value, updated_at FROM kv WHERE key = ?1",
        [key],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()?)
}

pub fn log_event(c: &Connection, source: &str, level: &str, message: &str) -> Result<()> {
    c.execute(
        "INSERT INTO events(ts, source, level, message) VALUES (?1, ?2, ?3, ?4)",
        params![now(), source, level, crate::secrets::redact(message)],
    )?;
    Ok(())
}

#[derive(Debug, Serialize)]
pub struct EventRow {
    pub ts: i64,
    pub source: String,
    pub level: String,
    pub message: String,
}

pub fn recent_events(c: &Connection, limit: i64) -> Result<Vec<EventRow>> {
    let mut st =
        c.prepare("SELECT ts, source, level, message FROM events ORDER BY id DESC LIMIT ?1")?;
    let rows = st.query_map([limit], |r| {
        Ok(EventRow {
            ts: r.get(0)?,
            source: r.get(1)?,
            level: r.get(2)?,
            message: r.get(3)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn store_raw(c: &Connection, path: &str, body: &str) -> Result<()> {
    c.execute(
        "INSERT INTO raw_responses(ts, path, body) VALUES (?1, ?2, ?3)",
        params![now(), path, crate::secrets::redact(body)],
    )?;
    // Keep the most recent 500 only.
    c.execute(
        "DELETE FROM raw_responses WHERE id <= (SELECT MAX(id) - 500 FROM raw_responses)",
        [],
    )?;
    Ok(())
}

pub fn bump_usage(c: &Connection, provider: &str) -> Result<i64> {
    let day = chrono::Utc::now().format("%Y-%m-%d").to_string();
    c.execute(
        "INSERT INTO api_usage(day, provider, count) VALUES (?1, ?2, 1) ON CONFLICT(day, provider) DO UPDATE SET count = count + 1",
        params![day, provider],
    )?;
    Ok(c.query_row(
        "SELECT count FROM api_usage WHERE day = ?1 AND provider = ?2",
        params![day, provider],
        |r| r.get(0),
    )?)
}

pub fn usage_today(c: &Connection, provider: &str) -> Result<i64> {
    let day = chrono::Utc::now().format("%Y-%m-%d").to_string();
    Ok(c.query_row(
        "SELECT count FROM api_usage WHERE day = ?1 AND provider = ?2",
        params![day, provider],
        |r| r.get(0),
    )
    .optional()?
    .unwrap_or(0))
}

// ---------- maintenance ----------

pub fn apply_retention(
    c: &mut Connection,
    raw_days: u32,
    five_min_days: u32,
    diag_days: u32,
) -> Result<(usize, usize)> {
    let t = now();
    let raw = c.execute(
        "DELETE FROM samples WHERE ts < ?1",
        [t - raw_days as i64 * 86400],
    )?;
    let agg = c.execute(
        "DELETE FROM agg_5min WHERE ts < ?1",
        [t - five_min_days as i64 * 86400],
    )?;
    c.execute(
        "DELETE FROM events WHERE ts < ?1",
        [t - diag_days as i64 * 86400],
    )?;
    // Weather snapshots: keep parsed hourly rows (needed for forecast evaluation), drop bodies after 14 days.
    c.execute(
        "UPDATE weather_fetch SET body = NULL WHERE fetched_at < ?1",
        [t - 14 * 86400],
    )?;
    Ok((raw, agg))
}

pub fn integrity_check(c: &Connection) -> Result<String> {
    Ok(c.query_row("PRAGMA quick_check", [], |r| r.get(0))?)
}

/// Consistent online backup via `VACUUM INTO`.
pub fn backup_to(c: &Connection, dest: &Path) -> Result<()> {
    if dest.exists() {
        std::fs::remove_file(dest)?;
    }
    c.execute("VACUUM INTO ?1", [dest.to_string_lossy()])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustsolar_core::energy::{Step, integrate};

    fn live(ts: i64, pv: f64, load: f64, st: f64, grid: f64) -> LiveStatus {
        LiveStatus {
            last_update: ts,
            pv_w: pv,
            storage_w: st,
            grid_w: grid,
            load_w: load,
            gen_w: None,
            soc_pct: Some(50.0),
            backup_soc_pct: Some(15.0),
            enc_agg_energy: None,
            enc_agg_soc_pct: None,
            main_relay_state: Some(1),
            gen_relay_state: None,
            phase_count: None,
            sc_stream: None,
            mqtt_state: None,
        }
    }

    #[test]
    fn insert_integrate_and_query() {
        let db = Db::open_memory().unwrap();
        db.with(|c| {
            let a = live(7200, 0.0, 1000.0, 0.0, 1000.0);
            assert!(insert_sample(c, &a, 7201, &[])?);
            assert!(!insert_sample(c, &a, 7202, &[])?, "duplicate ignored");
            let mut prev = a.sample();
            for i in 1..=360 {
                let s = live(7200 + i * 10, 0.0, 1000.0, 0.0, 1000.0);
                let parts = match integrate(&prev, &s.sample(), 3600, 120) {
                    Step::Energy(p) => p,
                    _ => vec![],
                };
                insert_sample(c, &s, s.last_update, &parts)?;
                prev = s.sample();
            }
            let e = energy_sum(c, 0, 20000)?;
            assert!((e.load_wh - 1000.0).abs() < 1e-6);
            assert!((e.grid_to_load_wh - 1000.0).abs() < 1e-6);
            let h = hourly(c, 0, 20000)?;
            assert_eq!(h[0].hour_ts, 7200);
            assert_eq!(h[0].soc_end, Some(50.0));
            // Rebuild gives the same answer.
            rebuild_energy(c, 0, 20000, 120)?;
            assert!((energy_sum(c, 0, 20000)?.load_wh - 1000.0).abs() < 1e-6);
            let ser = power_series(c, 7200, 10800, 10, 100_000)?;
            assert!(!ser.is_empty());
            assert!((ser[0].load_w - 1000.0).abs() < 1e-9);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn kv_usage_events_backup() {
        let db = Db::open_memory().unwrap();
        db.with(|c| {
            kv_set(c, "a", "1")?;
            kv_set(c, "a", "2")?;
            assert_eq!(kv_get(c, "a")?.unwrap().0, "2");
            assert_eq!(bump_usage(c, "open-meteo")?, 1);
            assert_eq!(bump_usage(c, "open-meteo")?, 2);
            assert_eq!(usage_today(c, "open-meteo")?, 2);
            log_event(c, "test", "info", "hello")?;
            assert_eq!(recent_events(c, 5)?.len(), 1);
            assert_eq!(integrity_check(c)?, "ok");
            let d = tempfile::tempdir()?;
            backup_to(c, &d.path().join("b.db"))?;
            assert!(d.path().join("b.db").exists());
            Ok(())
        })
        .unwrap();
    }
}
