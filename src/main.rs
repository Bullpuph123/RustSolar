//! RustSolar — Enphase Energy Intelligence. A RocketSalvo project.
//!
//! One executable: collector, analytics, and the web dashboard.

mod alerts;
mod api;
mod auth;
mod collector;
mod config;
mod db;
mod discovery;
mod forecast;
mod forward;
mod gateway;
mod intel;
mod secrets;
mod state;
mod weather;

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use tokio::sync::{Notify, watch};
use tokio_util::sync::CancellationToken;

use crate::config::{Config, TlsMode};
use crate::gateway::GatewayClient;
use crate::state::{AppState, CollectorStats, LiveSnapshot, Shared};

#[derive(Parser)]
#[command(
    name = "rustsolar",
    version,
    about = "RustSolar — Enphase Energy Intelligence (RocketSalvo)"
)]
struct Cli {
    /// Data directory (database, token, logs, backups). Env: RUSTSOLAR_DATA_DIR.
    #[arg(long, global = true, env = "RUSTSOLAR_DATA_DIR")]
    data_dir: Option<PathBuf>,
    /// Config file. Default: <data-dir>/config.toml. Env: RUSTSOLAR_CONFIG.
    #[arg(long, global = true, env = "RUSTSOLAR_CONFIG")]
    config: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the collector and web dashboard (default).
    Serve {
        /// DEMO MODE: synthetic data in a separate demo database, clearly labelled. No gateway access.
        #[arg(long)]
        demo: bool,
    },
    /// Show the gateway's certificate fingerprint and optionally pin it in the config.
    PinCert {
        #[arg(long)]
        host: Option<String>,
        /// Save the fingerprint (and host) to the config file.
        #[arg(long)]
        save: bool,
    },
    /// Read the gateway token from standard input and store it in the token file.
    SetToken,
    /// Print live readings to the console every poll (Ctrl+C to stop).
    Watch,
    /// Probe read-only gateway endpoints and report which are available.
    Discover,
    /// Validate the configuration file.
    CheckConfig,
    /// Hash a dashboard password read from standard input (for server.password_hash).
    HashPassword,
    /// Write a consistent backup of the database.
    Backup {
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Replace the database with a backup. Stop the service first.
    Restore {
        file: PathBuf,
        #[arg(long)]
        force: bool,
    },
    /// Recompute hourly energy totals from stored raw samples.
    RebuildEnergy {
        #[arg(long, default_value_t = 90)]
        days: i64,
    },
}

fn default_data_dir() -> PathBuf {
    #[cfg(windows)]
    {
        if let Some(d) = std::env::var_os("LOCALAPPDATA") {
            return PathBuf::from(d).join("RustSolar");
        }
    }
    #[cfg(not(windows))]
    {
        if let Some(d) = std::env::var_os("XDG_DATA_HOME") {
            return PathBuf::from(d).join("rustsolar");
        }
        if let Some(h) = std::env::var_os("HOME") {
            return PathBuf::from(h).join(".local/share/rustsolar");
        }
    }
    PathBuf::from("data")
}

fn init_logging(
    data_dir: &Path,
    console_only: bool,
) -> Option<tracing_appender::non_blocking::WorkerGuard> {
    use tracing_subscriber::{EnvFilter, fmt, prelude::*};
    let filter = EnvFilter::try_from_env("RUSTSOLAR_LOG")
        .unwrap_or_else(|_| EnvFilter::new("info,hyper=warn,reqwest=warn,rustls=warn"));
    let console = fmt::layer().with_target(false);
    if console_only {
        tracing_subscriber::registry()
            .with(filter)
            .with(console)
            .init();
        return None;
    }
    let _ = std::fs::create_dir_all(data_dir.join("logs"));
    let appender = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("rustsolar")
        .filename_suffix("log")
        .max_log_files(14)
        .build(data_dir.join("logs"))
        .ok();
    match appender {
        Some(a) => {
            let (nb, guard) = tracing_appender::non_blocking(a);
            tracing_subscriber::registry()
                .with(filter)
                .with(console)
                .with(fmt::layer().with_ansi(false).with_writer(nb))
                .init();
            Some(guard)
        }
        None => {
            tracing_subscriber::registry()
                .with(filter)
                .with(console)
                .init();
            None
        }
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let data_dir = cli.data_dir.clone().unwrap_or_else(default_data_dir);
    std::fs::create_dir_all(&data_dir)
        .with_context(|| format!("creating {}", data_dir.display()))?;
    let config_path = cli
        .config
        .clone()
        .unwrap_or_else(|| data_dir.join("config.toml"));
    let serve = matches!(cli.cmd, None | Some(Cmd::Serve { .. }));
    let _guard = init_logging(&data_dir, !serve);
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    rt.block_on(async move {
        match cli.cmd.unwrap_or(Cmd::Serve { demo: false }) {
            Cmd::Serve { demo } => serve_main(data_dir, config_path, demo).await,
            Cmd::PinCert { host, save } => pin_cert(&config_path, host, save).await,
            Cmd::SetToken => set_token(&data_dir, &config_path),
            Cmd::Watch => watch_cmd(&data_dir, &config_path).await,
            Cmd::Discover => discover_cmd(&data_dir, &config_path).await,
            Cmd::CheckConfig => {
                let cfg = Config::load(&config_path)?;
                for w in cfg.warnings() {
                    println!("warning: {w}");
                }
                let p = cfg.validate();
                if p.is_empty() {
                    println!("{}: OK", config_path.display());
                    Ok(())
                } else {
                    for x in &p {
                        println!("- {x}");
                    }
                    bail!("{} problem(s) in {}", p.len(), config_path.display())
                }
            }
            Cmd::HashPassword => {
                eprintln!("Type the dashboard password and press Enter (Ctrl+Z/Ctrl+D to finish):");
                let mut s = String::new();
                std::io::stdin().read_to_string(&mut s)?;
                let pw = s.trim_end_matches(['\r', '\n']);
                if pw.len() < 8 {
                    bail!("use at least 8 characters");
                }
                println!("{}", auth::hash_password(pw)?);
                Ok(())
            }
            Cmd::Backup { out } => {
                let db = db::Db::open(&data_dir.join("rustsolar.db"))?;
                let out = out.unwrap_or_else(|| {
                    data_dir.join("backups").join(format!(
                        "rustsolar-{}.db",
                        chrono::Local::now().format("%Y%m%d-%H%M%S")
                    ))
                });
                if let Some(d) = out.parent() {
                    std::fs::create_dir_all(d)?;
                }
                db.with(|c| db::backup_to(c, &out))?;
                println!("backup written to {}", out.display());
                Ok(())
            }
            Cmd::Restore { file, force } => restore(&data_dir, &file, force),
            Cmd::RebuildEnergy { days } => {
                let cfg = Config::load(&config_path)?;
                let db = db::Db::open(&data_dir.join("rustsolar.db"))?;
                let t = db::now();
                let n = db.with(|c| {
                    db::rebuild_energy(
                        c,
                        (t - days * 86400).div_euclid(3600) * 3600,
                        t + 3600,
                        cfg.collector.max_integration_gap_s,
                    )
                })?;
                println!("rebuilt {n} hourly buckets");
                Ok(())
            }
        }
    })
}

fn restore(data_dir: &Path, file: &Path, force: bool) -> Result<()> {
    let check = rusqlite::Connection::open(file)?;
    let ok: String = check.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
    if ok != "ok" {
        bail!("backup failed integrity check: {ok}");
    }
    drop(check);
    if !force {
        bail!(
            "this replaces the current database. Stop the RustSolar service first, then re-run with --force"
        );
    }
    let dest = data_dir.join("rustsolar.db");
    if dest.exists() {
        let keep = data_dir.join(format!(
            "rustsolar.before-restore-{}.db",
            chrono::Local::now().format("%Y%m%d-%H%M%S")
        ));
        std::fs::rename(&dest, &keep)?;
        println!("previous database kept as {}", keep.display());
        for ext in ["db-wal", "db-shm"] {
            let _ = std::fs::remove_file(dest.with_extension(ext));
        }
    }
    std::fs::copy(file, &dest)?;
    println!("restored {} → {}", file.display(), dest.display());
    Ok(())
}

fn load_gateway(cfg: &Config, data_dir: &Path) -> Result<GatewayClient> {
    if cfg.gateway.host.is_empty() {
        bail!("gateway.host is not set in the config");
    }
    if cfg.gateway.tls == TlsMode::InsecureDev {
        tracing::warn!(
            "gateway.tls = \"insecure_dev\": certificate checks are OFF. Use `rustsolar pin-cert --save` for normal use."
        );
    }
    let token = secrets::load_token(&secrets::token_path(
        cfg.gateway.token_file.as_deref(),
        data_dir,
    ))?;
    GatewayClient::new(&cfg.gateway, token)
}

async fn pin_cert(config_path: &Path, host: Option<String>, save: bool) -> Result<()> {
    let mut cfg = Config::load(config_path)?;
    if let Some(h) = host {
        cfg.gateway.host = h;
    }
    if cfg.gateway.host.is_empty() {
        bail!("give --host 192.168.x.x or set gateway.host in the config");
    }
    let client = GatewayClient::for_capture(&cfg.gateway)?;
    let (status, _) = client
        .get_text("/info")
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let fp = client.seen_fingerprint().context("no certificate seen")?;
    let s = gateway::tls::format_fingerprint(&fp);
    println!("Gateway {} answered HTTP {status}", cfg.gateway.host);
    println!("Certificate SHA-256: {s}");
    if save {
        cfg.gateway.cert_sha256 = Some(s);
        cfg.gateway.tls = TlsMode::Pin;
        cfg.save(config_path)?;
        println!("Pinned in {}", config_path.display());
    } else {
        println!("Re-run with --save to pin it.");
    }
    Ok(())
}

fn set_token(data_dir: &Path, config_path: &Path) -> Result<()> {
    let cfg = Config::load(config_path)?;
    eprintln!("Paste the gateway token, then press Enter and Ctrl+Z (Windows) / Ctrl+D (Linux):");
    let mut s = String::new();
    std::io::stdin().read_to_string(&mut s)?;
    let t = secrets::Token::new(s)?;
    let path = secrets::token_path(cfg.gateway.token_file.as_deref(), data_dir);
    secrets::save_token(&path, &t)?;
    let i = t.info();
    println!("Token saved to {}", path.display());
    if let Some(d) = i.days_left(db::now()) {
        println!("Expires in {d:.0} days.");
    }
    Ok(())
}

async fn watch_cmd(data_dir: &Path, config_path: &Path) -> Result<()> {
    let cfg = Config::load(config_path)?;
    let client = load_gateway(&cfg, data_dir)?;
    println!(
        "{:<20} {:>8} {:>8} {:>9} {:>8} {:>5} {:>8} {:>7}",
        "gateway time", "solar kW", "house kW", "battery kW", "grid kW", "SOC", "resid W", "ms"
    );
    let mut last = 0;
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            r = client.get_json("/ivp/livedata/status") => match r {
                Ok(resp) => match gateway::model::LiveStatus::from_value(&resp.value) {
                    Ok(s) => {
                        let tz = cfg.location.as_ref().map(|l| l.tz()).unwrap_or(chrono_tz::UTC);
                        let when = forecast::utc(s.last_update).with_timezone(&tz).format("%m-%d %H:%M:%S");
                        let mark = if s.last_update == last { " (same reading)" } else { "" };
                        last = s.last_update;
                        println!(
                            "{:<20} {:>8.2} {:>8.2} {:>9.2} {:>8.2} {:>4.0}% {:>8.0} {:>7}{mark}",
                            when.to_string(), s.pv_w / 1000.0, s.load_w / 1000.0, s.storage_w / 1000.0, s.grid_w / 1000.0,
                            s.soc_pct.unwrap_or(f64::NAN), s.sample().residual_w(), resp.latency_ms
                        );
                    }
                    Err(e) => println!("unexpected data: {e}"),
                },
                Err(e) => println!("error: {e}"),
            }
        }
        tokio::time::sleep(Duration::from_secs(cfg.collector.interval_s)).await;
    }
    Ok(())
}

async fn discover_cmd(data_dir: &Path, config_path: &Path) -> Result<()> {
    let cfg = Config::load(config_path)?;
    let client = load_gateway(&cfg, data_dir)?;
    println!(
        "Probing read-only endpoints on {} (GET only)…",
        cfg.gateway.host
    );
    for r in gateway::discover(&client).await {
        println!(
            "{:<34} {:<6} {:<40} {}",
            r.path,
            if r.ok { "OK" } else { "--" },
            r.status,
            r.top_level_keys.join(", ")
        );
    }
    Ok(())
}

pub async fn run_backup(state: &Shared) -> Result<PathBuf> {
    let cfg = state.cfg();
    let dir = cfg
        .backup
        .dir
        .clone()
        .unwrap_or_else(|| state.data_dir.join("backups"));
    std::fs::create_dir_all(&dir)?;
    let out = dir.join(format!(
        "rustsolar-{}.db",
        chrono::Local::now().format("%Y%m%d-%H%M%S")
    ));
    let o2 = out.clone();
    state.db.call(move |c| db::backup_to(c, &o2)).await?;
    // Keep the newest N.
    let mut files: Vec<_> = std::fs::read_dir(&dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("rustsolar-") && n.ends_with(".db"))
        })
        .collect();
    files.sort();
    while files.len() > cfg.backup.keep.max(1) {
        let _ = std::fs::remove_file(files.remove(0));
    }
    tracing::info!("backup written to {}", out.display());
    Ok(out)
}

async fn maintenance(state: Shared, cancel: CancellationToken) {
    let mut last_backup_day = None;
    loop {
        let cfg = state.cfg();
        let r = cfg.retention.clone();
        let res = state
            .db
            .call(move |c| {
                let (raw, agg) =
                    db::apply_retention(c, r.raw_days, r.five_min_days, r.diagnostics_days)?;
                let pruned = weather::prune_versions(c)?;
                let ok = db::integrity_check(c)?;
                if ok != "ok" {
                    db::log_event(c, "database", "error", &format!("integrity check: {ok}"))?;
                }
                Ok((raw, agg, pruned, ok))
            })
            .await;
        match res {
            Ok((raw, agg, pruned, ok)) => tracing::debug!(
                "maintenance: removed {raw} raw, {agg} 5-min rows, {pruned} old forecast versions; integrity {ok}"
            ),
            Err(e) => tracing::warn!("maintenance: {e:#}"),
        }
        if cfg.backup.enabled && !state.demo {
            let tz = cfg
                .location
                .as_ref()
                .map(|l| l.tz())
                .unwrap_or(chrono_tz::UTC);
            let t = db::now();
            let today = forecast::local_date(t, tz);
            if forecast::local_hour(t, tz) >= cfg.backup.hour && last_backup_day != Some(today) {
                match run_backup(&state).await {
                    Ok(_) => last_backup_day = Some(today),
                    Err(e) => tracing::warn!("backup failed: {e:#}"),
                }
            }
        }
        tokio::select! { _ = cancel.cancelled() => break, _ = tokio::time::sleep(Duration::from_secs(3600)) => {} }
    }
}

async fn serve_main(data_dir: PathBuf, config_path: PathBuf, demo: bool) -> Result<()> {
    let cfg = Config::load(&config_path)?;
    if !config_path.exists() && !demo {
        cfg.save(&config_path)?;
        tracing::info!("wrote default config to {}", config_path.display());
    }
    let problems = cfg.validate();
    if problems.iter().any(|p| p.contains("server.bind")) {
        bail!("{}", problems.join("; "));
    }
    for p in &problems {
        tracing::warn!("config: {p}");
    }
    let db_path = data_dir.join(if demo { "demo.db" } else { "rustsolar.db" });
    let db = db::Db::open(&db_path)?;
    let (live_tx, _) = watch::channel(LiveSnapshot {
        collector: "starting".into(),
        demo,
        ..Default::default()
    });
    let (intel_tx, _) = watch::channel(0i64);
    let state: Shared = Arc::new(AppState {
        config: RwLock::new(Arc::new(cfg.clone())),
        config_path,
        data_dir: data_dir.clone(),
        db,
        gateway: RwLock::new(None),
        live_tx,
        stats: Mutex::new(CollectorStats {
            started_at: db::now(),
            ..Default::default()
        }),
        sessions: Mutex::new(Default::default()),
        reload: Notify::new(),
        intel_tx,
        intel: RwLock::new(None),
        demo,
    });
    if demo {
        tracing::warn!(
            "DEMO MODE: synthetic data only, stored in {}",
            db_path.display()
        );
    }

    let app = api::router(state.clone());
    let listener = tokio::net::TcpListener::bind(cfg.server.bind)
        .await
        .with_context(|| format!("binding {}", cfg.server.bind))?;
    tracing::info!(
        "RustSolar {} dashboard on http://{}",
        env!("CARGO_PKG_VERSION"),
        cfg.server.bind
    );
    for w in cfg.warnings() {
        tracing::warn!("config: {w}");
    }
    let mut listeners = vec![listener];
    let mut public_port = cfg.server.bind.port();
    if cfg.server.port80 && cfg.server.bind.port() != 80 {
        match tokio::net::TcpListener::bind(std::net::SocketAddr::new(cfg.server.bind.ip(), 80))
            .await
        {
            Ok(l) => {
                tracing::info!("also listening on port 80");
                listeners.push(l);
                public_port = 80;
            }
            Err(e) => tracing::warn!(
                "port 80 is not available ({e}); the dashboard is still on port {}",
                cfg.server.bind.port()
            ),
        }
    }
    let announcer = if cfg.server.mdns && !demo {
        match discovery::lan_ip(cfg.server.bind, &cfg.gateway.host) {
            Some(ip) => match discovery::start(&cfg.server.mdns_name, ip, public_port) {
                Ok(a) => {
                    tracing::info!("announced on the local network as {} ({ip})", a.url);
                    Some(a)
                }
                Err(e) => {
                    tracing::warn!("could not announce {}.local: {e:#}", cfg.server.mdns_name);
                    None
                }
            },
            None => {
                tracing::info!(
                    "not announcing {}.local: the dashboard only listens on this computer",
                    cfg.server.mdns_name
                );
                None
            }
        }
    } else {
        None
    };
    let shutdown = CancellationToken::new();
    let servers: Vec<_> = listeners
        .into_iter()
        .map(|l| {
            let sd = shutdown.clone();
            let app = app.clone();
            tokio::spawn(async move {
                axum::serve(l, app)
                    .with_graceful_shutdown(async move { sd.cancelled().await })
                    .await
            })
        })
        .collect();

    loop {
        let cancel = CancellationToken::new();
        let cfg = state.cfg();
        let mut tasks = Vec::new();
        if demo {
            tasks.push(tokio::spawn(collector::demo_run(
                state.clone(),
                cancel.clone(),
            )));
        } else {
            let client = match load_gateway(&cfg, &data_dir) {
                Ok(c) => Some(c),
                Err(e) => {
                    let kind = if cfg.gateway.host.is_empty() {
                        "no_host"
                    } else {
                        "tls"
                    };
                    tracing::warn!("gateway not started: {e:#}");
                    state.live_tx.send_modify(|s| {
                        s.collector = kind.into();
                        s.last_error = Some(format!("{e:#}"));
                    });
                    None
                }
            };
            *state.gateway.write().expect("gateway lock") = client.clone();
            if let Some(c) = client {
                tasks.push(tokio::spawn(collector::run(state.clone(), cancel.clone())));
                tasks.push(tokio::spawn(collector::inventory_task(
                    state.clone(),
                    c,
                    cancel.clone(),
                )));
            }
            tasks.push(tokio::spawn(collector::staleness_ticker(
                state.clone(),
                cancel.clone(),
            )));
        }
        if cfg.weather.enabled && cfg.location.is_some() {
            tasks.push(tokio::spawn(weather::run(state.clone(), cancel.clone())));
        }
        tasks.push(tokio::spawn(intel::run(state.clone(), cancel.clone())));
        tasks.push(tokio::spawn(maintenance(state.clone(), cancel.clone())));

        let stop = tokio::select! {
            _ = shutdown_signal() => true,
            _ = state.reload.notified() => false,
        };
        cancel.cancel();
        for t in tasks {
            let _ = tokio::time::timeout(Duration::from_secs(15), t).await;
        }
        if stop {
            break;
        }
        tracing::info!("configuration changed; workers restarted");
    }
    shutdown.cancel();
    if let Some(a) = announcer {
        a.stop();
    }
    for s in servers {
        let _ = tokio::time::timeout(Duration::from_secs(10), s).await;
    }
    tracing::info!("stopped");
    Ok(())
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("SIGTERM handler");
        tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = term.recv() => {} }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
