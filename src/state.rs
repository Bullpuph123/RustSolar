//! Shared application state.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};

use rustsolar_core::energy::Flows;
use serde::Serialize;
use tokio::sync::{Notify, watch};

use crate::config::Config;
use crate::db::Db;
use crate::gateway::GatewayClient;
use crate::gateway::model::LiveStatus;

#[derive(Debug, Clone, Serialize, Default)]
pub struct LiveSnapshot {
    pub status: Option<LiveStatus>,
    pub flows: Flows,
    /// When we received the newest distinct reading (server clock).
    pub received_at: Option<i64>,
    /// Seconds since the newest distinct reading arrived.
    pub age_s: Option<i64>,
    pub stale: bool,
    /// No new reading for longer than `collector.warn_after_min`: show warnings.
    pub warn: bool,
    pub residual_w: Option<f64>,
    pub balanced: Option<bool>,
    /// ok | starting | retrying | no_host | no_token | unauthorized | tls | disabled
    pub collector: String,
    pub last_error: Option<String>,
    pub consecutive_failures: u32,
    pub demo: bool,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct CollectorStats {
    pub polls: u64,
    pub successes: u64,
    pub failures: u64,
    pub duplicates: u64,
    pub gaps: u64,
    pub unbalanced: u64,
    pub failures_by_kind: HashMap<String, u64>,
    pub last_latency_ms: Option<u64>,
    pub avg_latency_ms: Option<f64>,
    pub max_latency_ms: Option<u64>,
    pub last_success_at: Option<i64>,
    pub last_failure_at: Option<i64>,
    pub last_error: Option<String>,
    /// Seconds between consecutive distinct gateway timestamps (latest).
    pub last_update_interval_s: Option<i64>,
    pub current_backoff_s: u64,
    /// Gateway clock minus server clock at receipt, seconds (latest).
    pub clock_skew_s: Option<i64>,
    pub started_at: i64,
}

pub struct AppState {
    pub config: RwLock<Arc<Config>>,
    pub config_path: PathBuf,
    pub data_dir: PathBuf,
    pub db: Db,
    pub gateway: RwLock<Option<GatewayClient>>,
    pub live_tx: watch::Sender<LiveSnapshot>,
    pub stats: Mutex<CollectorStats>,
    /// Session id → expiry (Unix s).
    pub sessions: Mutex<HashMap<String, i64>>,
    /// Signalled when config changes and workers should restart.
    pub reload: Notify,
    /// Signalled when new forecasts/alerts are ready (SSE nudge).
    pub intel_tx: watch::Sender<i64>,
    /// Latest computed forecasts, battery outlook and recommendations.
    pub intel: RwLock<Option<Arc<crate::intel::Intel>>>,
    pub demo: bool,
}

pub type Shared = Arc<AppState>;

impl AppState {
    pub fn cfg(&self) -> Arc<Config> {
        self.config.read().expect("config lock").clone()
    }
    pub fn gateway(&self) -> Option<GatewayClient> {
        self.gateway.read().expect("gateway lock").clone()
    }
    pub fn live(&self) -> LiveSnapshot {
        self.live_tx.borrow().clone()
    }
    pub fn intel(&self) -> Option<Arc<crate::intel::Intel>> {
        self.intel.read().expect("intel lock").clone()
    }
}
