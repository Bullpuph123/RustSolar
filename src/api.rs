//! HTTP API and static web UI. All gateway access is read-only; nothing here can send a
//! write command to the gateway. The gateway token is accepted (setup) but never returned.

use std::convert::Infallible;
use std::time::Duration;

use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{Datelike, NaiveDate};
use futures::Stream;
use include_dir::{Dir, include_dir};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::config::{Config, TlsMode};
use crate::db::{self, now};
use crate::forecast;
use crate::gateway::{self, GatewayClient, tls};
use crate::secrets::{self, Token};
use crate::state::Shared;

static WEB: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/web/dist");
const SESSION_COOKIE: &str = "rs_session";

pub struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        tracing::warn!("api error: {e:#}");
        ApiError(
            StatusCode::INTERNAL_SERVER_ERROR,
            secrets::redact(&format!("{e:#}")),
        )
    }
}

fn bad(msg: impl Into<String>) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, msg.into())
}

type ApiResult<T> = Result<Json<T>, ApiError>;

pub fn router(state: Shared) -> Router {
    let api = Router::new()
        .route("/live", get(live))
        .route("/stream", get(stream))
        .route("/overview", get(overview))
        .route("/history", get(history))
        .route("/daily", get(daily))
        .route("/intel", get(intel))
        .route("/sun", get(sun))
        .route("/weather", get(weather_get))
        .route("/weather/refresh", post(weather_refresh))
        .route("/accuracy", get(accuracy))
        .route("/costs", get(costs))
        .route("/tariff/day", get(tariff_day))
        .route("/tariff/pge-nbt", get(pge_nbt))
        .route("/alerts", get(alerts_list))
        .route("/alerts/{id}/ack", post(alerts_ack))
        .route("/diagnostics", get(diagnostics))
        .route("/config", get(config_get).post(config_post))
        .route("/setup/token", post(setup_token))
        .route("/setup/pin", post(setup_pin))
        .route("/setup/test", post(setup_test))
        .route("/discover", post(discover))
        .route("/model/reset", post(model_reset))
        .route("/backup", post(backup_now))
        .route("/export/csv", get(export_csv))
        .route("/export/json", get(export_json))
        .route("/annotations", get(annotations_list).post(annotations_add))
        .route("/login", post(login))
        .route("/logout", post(logout))
        .route("/session", get(session))
        .layer(middleware::from_fn_with_state(state.clone(), auth));
    Router::new()
        .nest("/api", api)
        .fallback(static_file)
        .layer(middleware::from_fn(security_headers))
        // Outermost: requests for another dashboard's host name never reach RustSolar's routes.
        .layer(middleware::from_fn_with_state(
            state.clone(),
            crate::forward::forward,
        ))
        .with_state(state)
}

// ---------------- middleware ----------------

async fn security_headers(req: Request, next: Next) -> Response {
    let mut r = next.run(req).await;
    let h = r.headers_mut();
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'self'; img-src 'self' data:; style-src 'self' 'unsafe-inline'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'"),
    );
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    r
}

fn cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|s| s.split(';'))
        .find_map(|kv| {
            let (k, v) = kv.trim().split_once('=')?;
            (k == name).then(|| v.to_string())
        })
}

fn authed(state: &Shared, headers: &HeaderMap) -> bool {
    let cfg = state.cfg();
    if cfg.server.password_hash.is_none() {
        return true;
    }
    let Some(sid) = cookie(headers, SESSION_COOKIE) else {
        return false;
    };
    let mut s = state.sessions.lock().expect("sessions");
    let t = now();
    s.retain(|_, exp| *exp > t);
    s.contains_key(&sid)
}

async fn auth(State(state): State<Shared>, req: Request, next: Next) -> Response {
    let path = req.uri().path().to_string();
    // CSRF: state-changing requests must carry a custom header (browsers block it cross-site).
    if req.method() != axum::http::Method::GET && req.headers().get("x-rustsolar").is_none() {
        return ApiError(StatusCode::FORBIDDEN, "missing X-RustSolar header".into())
            .into_response();
    }
    let open = path.ends_with("/login") || path.ends_with("/session");
    if !open && !authed(&state, req.headers()) {
        return ApiError(StatusCode::UNAUTHORIZED, "login required".into()).into_response();
    }
    next.run(req).await
}

#[derive(Deserialize)]
struct LoginReq {
    password: String,
}

async fn login(
    State(state): State<Shared>,
    Json(req): Json<LoginReq>,
) -> Result<Response, ApiError> {
    let cfg = state.cfg();
    let Some(hash) = &cfg.server.password_hash else {
        return Ok(Json(json!({"ok": true})).into_response());
    };
    // Slow down guessing.
    tokio::time::sleep(Duration::from_millis(400)).await;
    if !crate::auth::verify_password(&req.password, hash) {
        return Err(ApiError(StatusCode::UNAUTHORIZED, "wrong password".into()));
    }
    let sid = crate::auth::new_session_id();
    let days = cfg.server.session_days.max(1) as i64;
    state
        .sessions
        .lock()
        .expect("sessions")
        .insert(sid.clone(), now() + days * 86400);
    let c = format!(
        "{SESSION_COOKIE}={sid}; Path=/; HttpOnly; SameSite=Strict; Max-Age={}",
        days * 86400
    );
    Ok(([(header::SET_COOKIE, c)], Json(json!({"ok": true}))).into_response())
}

async fn logout(State(state): State<Shared>, headers: HeaderMap) -> Response {
    if let Some(sid) = cookie(&headers, SESSION_COOKIE) {
        state.sessions.lock().expect("sessions").remove(&sid);
    }
    (
        [(
            header::SET_COOKIE,
            format!("{SESSION_COOKIE}=; Path=/; Max-Age=0"),
        )],
        Json(json!({"ok": true})),
    )
        .into_response()
}

async fn session(State(state): State<Shared>, headers: HeaderMap) -> Json<Value> {
    let cfg = state.cfg();
    Json(
        json!({ "authenticated": authed(&state, &headers), "password_required": cfg.server.password_hash.is_some(), "demo": state.demo, "version": env!("CARGO_PKG_VERSION"), "warnings": cfg.warnings() }),
    )
}

// ---------------- static ----------------

async fn static_file(uri: axum::http::Uri, headers: HeaderMap) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    let file = WEB.get_file(path).or_else(|| WEB.get_file("index.html"));
    match file {
        Some(f) => {
            let mime = match f.path().extension().and_then(|e| e.to_str()) {
                Some("html") => "text/html; charset=utf-8",
                Some("js") => "text/javascript; charset=utf-8",
                Some("css") => "text/css; charset=utf-8",
                Some("png") => "image/png",
                Some("svg") => "image/svg+xml",
                Some("json") | Some("webmanifest") => "application/json",
                _ => "application/octet-stream",
            };
            // Browsers revalidate every time (cheap 304), so an upgrade shows at once.
            let etag = format!("\"{:016x}\"", fnv1a(f.contents()));
            let fresh = headers
                .get(header::IF_NONE_MATCH)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| v.split(',').any(|t| t.trim() == etag));
            let hdrs = [
                (header::CONTENT_TYPE, mime.to_string()),
                (header::CACHE_CONTROL, "no-cache".to_string()),
                (header::ETAG, etag),
            ];
            if fresh {
                (StatusCode::NOT_MODIFIED, hdrs).into_response()
            } else {
                (hdrs, f.contents()).into_response()
            }
        }
        None => (StatusCode::NOT_FOUND, "web UI not built").into_response(),
    }
}

fn fnv1a(b: &[u8]) -> u64 {
    b.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, x| {
        (h ^ *x as u64).wrapping_mul(0x0100_0000_01b3)
    })
}

// ---------------- live ----------------

async fn live(State(state): State<Shared>) -> Json<Value> {
    Json(json!({ "now": now(), "live": state.live() }))
}

async fn stream(State(state): State<Shared>) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let mut live_rx = state.live_tx.subscribe();
    let mut intel_rx = state.intel_tx.subscribe();
    let s = async_stream::stream! {
        let snap = state.live();
        yield Ok(Event::default().event("live").json_data(json!({"now": now(), "live": snap})).unwrap_or_default());
        loop {
            tokio::select! {
                r = live_rx.changed() => {
                    if r.is_err() { break; }
                    let snap = live_rx.borrow_and_update().clone();
                    yield Ok(Event::default().event("live").json_data(json!({"now": now(), "live": snap})).unwrap_or_default());
                }
                r = intel_rx.changed() => {
                    if r.is_err() { break; }
                    let _ = intel_rx.borrow_and_update();
                    yield Ok(Event::default().event("intel").data("updated"));
                }
            }
        }
    };
    Sse::new(s).keep_alive(KeepAlive::new().interval(Duration::from_secs(20)))
}

// ---------------- overview & history ----------------

fn tz_of(cfg: &Config) -> chrono_tz::Tz {
    cfg.location
        .as_ref()
        .map(|l| l.tz())
        .unwrap_or(chrono_tz::UTC)
}

#[derive(Serialize)]
struct PeriodSummary {
    name: &'static str,
    from: i64,
    energy: db::EnergyRow,
    self_consumption: Option<f64>,
    solar_direct_share: Option<f64>,
    battery_share: Option<f64>,
    grid_independence: Option<f64>,
}

async fn overview(State(state): State<Shared>) -> ApiResult<Value> {
    let cfg = state.cfg();
    let tz = tz_of(&cfg);
    let t = now();
    let today = forecast::local_date(t, tz);
    let week_start = today - chrono::Duration::days(today.weekday().num_days_from_monday() as i64);
    let month_start = today.with_day(1).unwrap_or(today);
    let year_start = NaiveDate::from_ymd_opt(today.year(), 1, 1).unwrap_or(today);
    let bounds = [
        ("today", forecast::day_start(today, tz)),
        ("week", forecast::day_start(week_start, tz)),
        ("month", forecast::day_start(month_start, tz)),
        ("year", forecast::day_start(year_start, tz)),
        ("all", 0),
    ];
    let v = state
        .db
        .call(move |c| {
            let mut periods = Vec::new();
            for (name, from) in bounds {
                let e = db::energy_sum(c, from, t + 3600)?;
                periods.push(PeriodSummary {
                    name,
                    from,
                    energy: e.into(),
                    self_consumption: e.self_consumption(),
                    solar_direct_share: e.solar_direct_share(),
                    battery_share: e.battery_share(),
                    grid_independence: e.grid_independence(),
                });
            }
            let (d0, d1) = forecast::day_bounds(today, tz);
            let peaks: (Option<f64>, Option<f64>) = c.query_row("SELECT MAX(peak_pv_w), MAX(peak_load_w) FROM energy_hourly WHERE hour_ts >= ?1 AND hour_ts < ?2", [d0, d1], |r| Ok((r.get(0)?, r.get(1)?)))?;
            let gw_prod: Option<Value> = db::kv_get(c, "gw:/production.json?details=1")?.and_then(|(v, _)| serde_json::from_str(&v).ok());
            Ok(json!({
                "periods": periods,
                "today_peak_pv_w": peaks.0,
                "today_peak_load_w": peaks.1,
                "monitoring_since": db::first_hour_ts(c)?,
                "gateway_totals": gw_prod,
            }))
        })
        .await?;
    Ok(Json(v))
}

#[derive(Deserialize)]
struct RangeQ {
    range: Option<String>,
    from: Option<i64>,
    to: Option<i64>,
    points: Option<i64>,
}

fn resolve_range(q: &RangeQ) -> (i64, i64) {
    let t = now();
    if let (Some(a), Some(b)) = (q.from, q.to) {
        return (a.min(b), a.max(b));
    }
    let span = match q.range.as_deref().unwrap_or("24h") {
        "6h" => 6 * 3600,
        "7d" => 7 * 86400,
        "30d" => 30 * 86400,
        "12m" => 365 * 86400,
        _ => 86400,
    };
    (t - span, t)
}

async fn history(State(state): State<Shared>, Query(q): Query<RangeQ>) -> ApiResult<Value> {
    let (from, to) = resolve_range(&q);
    let points = q.points.unwrap_or(600).clamp(50, 3000);
    let raw_days = state.cfg().retention.raw_days as i64;
    let v = state
        .db
        .call(move |c| {
            let series = db::power_series(c, from, to, points, raw_days)?;
            Ok(json!({"from": from, "to": to, "series": series}))
        })
        .await?;
    Ok(Json(v))
}

#[derive(Serialize)]
struct DayRow {
    date: NaiveDate,
    start: i64,
    energy: db::EnergyRow,
    self_consumption: Option<f64>,
    grid_independence: Option<f64>,
    peak_pv_w: Option<f64>,
    peak_load_w: Option<f64>,
}

fn daily_rows(
    c: &rusqlite::Connection,
    from: i64,
    to: i64,
    tz: chrono_tz::Tz,
    monthly: bool,
) -> anyhow::Result<Vec<DayRow>> {
    let hours = db::hourly(c, from, to)?;
    let mut out: Vec<DayRow> = Vec::new();
    for h in hours {
        let mut d = forecast::local_date(h.hour_ts, tz);
        if monthly {
            d = d.with_day(1).unwrap_or(d);
        }
        if out.last().is_none_or(|r| r.date != d) {
            out.push(DayRow {
                date: d,
                start: forecast::day_start(d, tz),
                energy: Default::default(),
                self_consumption: None,
                grid_independence: None,
                peak_pv_w: None,
                peak_load_w: None,
            });
        }
        let r = out.last_mut().expect("pushed");
        let mut e: rustsolar_core::energy::Energy = r.energy.into();
        e.add(&h.e.into());
        r.energy = e.into();
        r.peak_pv_w = max_opt(r.peak_pv_w, h.peak_pv_w);
        r.peak_load_w = max_opt(r.peak_load_w, h.peak_load_w);
    }
    for r in &mut out {
        let e: rustsolar_core::energy::Energy = r.energy.into();
        r.self_consumption = e.self_consumption();
        r.grid_independence = e.grid_independence();
    }
    Ok(out)
}

#[derive(Deserialize)]
struct DailyQ {
    from: Option<i64>,
    to: Option<i64>,
    days: Option<i64>,
    group: Option<String>,
}

async fn daily(State(state): State<Shared>, Query(q): Query<DailyQ>) -> ApiResult<Value> {
    let tz = tz_of(&state.cfg());
    let t = now();
    let to = q.to.unwrap_or(t + 3600);
    let from = q.from.unwrap_or_else(|| {
        forecast::local_midnight(t - q.days.unwrap_or(30).clamp(1, 3660) * 86400, tz)
    });
    let monthly = q.group.as_deref() == Some("month");
    let v = state
        .db
        .call(move |c| Ok(json!({"days": daily_rows(c, from, to, tz, monthly)?})))
        .await?;
    Ok(Json(v))
}

async fn intel(State(state): State<Shared>) -> ApiResult<Value> {
    match state.intel() {
        Some(i) => Ok(Json(
            serde_json::to_value(&*i).map_err(anyhow::Error::from)?,
        )),
        None => Ok(Json(json!({"pending": true}))),
    }
}

#[derive(Deserialize)]
struct SunQ {
    year: Option<i32>,
}

async fn sun(State(state): State<Shared>, Query(q): Query<SunQ>) -> ApiResult<Value> {
    let cfg = state.cfg();
    let Some(loc) = cfg.location.clone() else {
        return Err(bad("location not configured"));
    };
    let tz = loc.tz();
    let t = now();
    let today = forecast::local_date(t, tz);
    let year = q.year.unwrap_or(today.year());
    let (d0, d1) = forecast::day_bounds(today, tz);
    let path = rustsolar_core::astro::sun_samples(
        forecast::utc(d0),
        forecast::utc(d1),
        600,
        loc.latitude,
        loc.longitude,
    );
    let table = rustsolar_core::astro::annual_sun_table(year, loc.latitude, loc.longitude, tz);
    let pos = rustsolar_core::astro::solar_position(forecast::utc(t), loc.latitude, loc.longitude);
    Ok(Json(json!({
        "now": pos,
        "today": rustsolar_core::astro::sun_times(today, loc.latitude, loc.longitude, tz),
        "path": path.iter().map(|(t, p)| json!([t.timestamp(), p.elevation_deg, p.azimuth_deg])).collect::<Vec<_>>(),
        "year": table.iter().map(|s| json!({
            "date": s.date, "sunrise": s.sunrise.map(|x| x.timestamp()), "sunset": s.sunset.map(|x| x.timestamp()),
            "noon": s.solar_noon.timestamp(), "day_length_s": s.day_length_s,
            "civil_dawn": s.civil_dawn.map(|x| x.timestamp()), "civil_dusk": s.civil_dusk.map(|x| x.timestamp())
        })).collect::<Vec<_>>(),
        "time_zone": loc.time_zone,
    })))
}

async fn weather_get(State(state): State<Shared>) -> ApiResult<Value> {
    let cfg = state.cfg();
    let wc = cfg.weather.clone();
    let t = now();
    let v = state
        .db
        .call(move |c| {
            let hours = crate::weather::latest_hours(c, t - 24 * 3600, t + 17 * 86400)?;
            let st = crate::weather::status(c, &wc)?;
            let current: Option<Value> = c
                .query_row("SELECT body FROM weather_fetch WHERE kind='forecast' AND ok=1 AND body IS NOT NULL ORDER BY id DESC LIMIT 1", [], |r| r.get::<_, String>(0))
                .ok()
                .and_then(|b| serde_json::from_str::<Value>(&b).ok())
                .and_then(|v| v.get("current").cloned());
            Ok(json!({"hours": hours, "status": st, "current": current}))
        })
        .await?;
    Ok(Json(v))
}

async fn weather_refresh(State(state): State<Shared>) -> ApiResult<Value> {
    let http = crate::weather::http_client()?;
    let r = crate::weather::refresh_if_due(&state, &http, true).await;
    state.intel_tx.send_modify(|v| *v = now());
    match r {
        Ok(_) => Ok(Json(json!({"ok": true}))),
        Err(e) => Err(bad(format!("{e:#}"))),
    }
}

#[derive(Deserialize)]
struct DaysQ {
    days: Option<i64>,
}

async fn accuracy(State(state): State<Shared>, Query(q): Query<DaysQ>) -> ApiResult<Value> {
    let tz = tz_of(&state.cfg());
    let t = now();
    let from = t - q.days.unwrap_or(30).clamp(1, 400) * 86400;
    let v = state
        .db
        .call(move |c| {
            let pv_hour = crate::intel::forecast_pairs(c, "pv_wh", from, t, 3600)?;
            let pv_day = crate::intel::forecast_pairs(c, "pv_wh", from, t, 12 * 3600)?;
            let load = crate::intel::forecast_pairs(c, "load_wh", from, t, 3600)?;
            let series: Vec<_> = pv_hour.iter().map(|p| json!([p.0, p.1, p.2])).collect();
            Ok(json!({
                "pv_hour_ahead": forecast::accuracy(&pv_hour, tz),
                "pv_day_ahead": forecast::accuracy(&pv_day, tz),
                "load_hour_ahead": forecast::accuracy(&load, tz),
                "pv_series": series,
                "note": "Each forecast is compared with the energy later measured for that hour; only forecasts issued before the hour (≥1 h or ≥12 h ahead) are used.",
            }))
        })
        .await?;
    Ok(Json(v))
}

#[derive(Deserialize)]
struct CostQ {
    from: Option<i64>,
    to: Option<i64>,
    days: Option<i64>,
    lines: Option<bool>,
}

fn hour_energy(rows: &[db::HourRow]) -> Vec<rustsolar_core::tariff::HourEnergy> {
    rows.iter()
        .filter(|h| h.e.covered_s > 0.0)
        .map(|h| rustsolar_core::tariff::HourEnergy {
            hour_start: forecast::utc(h.hour_ts),
            import_kwh: h.e.import_wh / 1000.0,
            export_kwh: h.e.export_wh / 1000.0,
            load_kwh: h.e.load_wh / 1000.0,
            pv_kwh: h.e.pv_wh / 1000.0,
        })
        .collect()
}

async fn costs(State(state): State<Shared>, Query(q): Query<CostQ>) -> ApiResult<Value> {
    let cfg = state.cfg();
    if !cfg.tariff.enabled {
        return Ok(Json(
            json!({"enabled": false, "note": "Enter your electricity rates in Settings → Tariff to see cost estimates."}),
        ));
    }
    let tz = tz_of(&cfg);
    let t = now();
    let to = q.to.unwrap_or(t + 3600);
    let from = q.from.unwrap_or_else(|| {
        forecast::local_midnight(t - q.days.unwrap_or(30).clamp(1, 3660) * 86400, tz)
    });
    let rows = state.db.call(move |c| db::hourly(c, from, to)).await?;
    let mut report = cfg.tariff.cost_hours(&hour_energy(&rows));
    if !q.lines.unwrap_or(false) {
        report.lines.clear();
    }
    Ok(Json(
        json!({"enabled": true, "report": report, "note": "Estimates from measured energy and the rates you entered; your utility bill is authoritative."}),
    ))
}

#[derive(Deserialize)]
struct DayQ {
    date: Option<NaiveDate>,
}

#[derive(Deserialize)]
struct NbtQ {
    vintage: Option<i32>,
    year: Option<i32>,
    generation: Option<bool>,
}

/// Built-in PG&E Net Billing export credits, for the settings preview.
async fn pge_nbt(Query(q): Query<NbtQ>) -> ApiResult<Value> {
    use rustsolar_core::pge_nbt;
    let vintages: Vec<Value> = pge_nbt::vintages()
        .into_iter()
        .map(|v| json!({"vintage": v, "acc_plus": pge_nbt::acc_plus_residential(v), "years": pge_nbt::years(v)}))
        .collect();
    let table = match q.vintage {
        Some(v) => {
            let year = q.year.unwrap_or_else(|| chrono::Utc::now().year());
            let (used, weekday, weekend) = pge_nbt::table(v, year, q.generation.unwrap_or(true))
                .ok_or_else(|| bad(format!("no built-in values for vintage {v}")))?;
            json!({"vintage": v, "year": used, "weekday": weekday, "weekend": weekend})
        }
        None => Value::Null,
    };
    Ok(Json(json!({
        "vintages": vintages,
        "table": table,
        "source": "PG&E Solar Billing Plan export rates (pge.com/eecvalues), Schedule NBT",
    })))
}

async fn tariff_day(State(state): State<Shared>, Query(q): Query<DayQ>) -> ApiResult<Value> {
    let cfg = state.cfg();
    if !cfg.tariff.enabled {
        return Ok(Json(json!({"enabled": false})));
    }
    let d = q
        .date
        .unwrap_or_else(|| forecast::local_date(now(), tz_of(&cfg)));
    let now_rate = cfg.tariff.rate_at(forecast::utc(now()));
    Ok(Json(
        json!({"enabled": true, "date": d, "schedule": cfg.tariff.rate_schedule_for_day(d), "now": now_rate, "currency": cfg.tariff.currency}),
    ))
}

#[derive(Deserialize)]
struct AlertQ {
    all: Option<bool>,
}

async fn alerts_list(State(state): State<Shared>, Query(q): Query<AlertQ>) -> ApiResult<Value> {
    let all = q.all.unwrap_or(false);
    let v = state
        .db
        .call(move |c| Ok(json!({"alerts": crate::alerts::list(c, !all, 200)?})))
        .await?;
    Ok(Json(v))
}

async fn alerts_ack(State(state): State<Shared>, Path(id): Path<i64>) -> ApiResult<Value> {
    let ok = state.db.call(move |c| crate::alerts::ack(c, id)).await?;
    Ok(Json(json!({"ok": ok})))
}

// ---------------- diagnostics & config ----------------

async fn diagnostics(State(state): State<Shared>) -> ApiResult<Value> {
    let cfg = state.cfg();
    let stats = state.stats.lock().expect("stats").clone();
    let token_path = secrets::token_path(cfg.gateway.token_file.as_deref(), &state.data_dir);
    let token = secrets::load_token(&token_path);
    let token_json = match &token {
        Ok(Some(t)) => {
            let i = t.info();
            json!({"present": true, "source": if std::env::var(secrets::TOKEN_ENV).is_ok() { "environment" } else { "file" }, "expires_at": i.expires_at, "days_left": i.days_left(now()), "role": i.role})
        }
        Ok(None) => json!({"present": false}),
        Err(e) => json!({"present": false, "error": e.to_string()}),
    };
    let seen = state
        .gateway()
        .and_then(|g| g.seen_fingerprint())
        .map(|f| tls::format_fingerprint(&f));
    let wc = cfg.weather.clone();
    let db_path = state.data_dir.join(if state.demo {
        "demo.db"
    } else {
        "rustsolar.db"
    });
    let db_size = std::fs::metadata(&db_path).map(|m| m.len()).ok();
    let v = state
        .db
        .call(move |c| {
            let endpoints: Vec<(String, String)> = {
                let mut st = c.prepare("SELECT key, value FROM kv WHERE key LIKE 'endpoint:%' ORDER BY key")?;
                st.query_map([], |r| Ok((r.get::<_, String>(0)?.trim_start_matches("endpoint:").to_string(), r.get(1)?)))?.collect::<rusqlite::Result<_>>()?
            };
            let counts: (i64, i64, i64) = c.query_row("SELECT (SELECT COUNT(*) FROM samples), (SELECT COUNT(*) FROM energy_hourly), (SELECT COUNT(*) FROM weather_hourly)", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
            let residual: (Option<f64>, Option<f64>) = c.query_row("SELECT AVG(ABS(residual_w)), MAX(ABS(residual_w)) FROM samples WHERE ts > ?1", [now() - 86400], |r| Ok((r.get(0)?, r.get(1)?)))?;
            let implied = crate::intel::implied_capacity(c, now() - 3 * 86400)?;
            Ok(json!({
                "endpoints": endpoints,
                "events": db::recent_events(c, 100)?,
                "rows": {"samples": counts.0, "hours": counts.1, "weather_hours": counts.2},
                "first_sample": db::first_sample_ts(c)?,
                "residual_24h": {"mean_abs_w": residual.0, "max_abs_w": residual.1},
                "enc_agg_energy_implied_capacity_wh": implied,
                "weather": crate::weather::status(c, &wc)?,
            }))
        })
        .await?;
    Ok(Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "demo": state.demo,
        "data_dir": state.data_dir,
        "db_bytes": db_size,
        "collector": stats,
        "live": state.live(),
        "token": token_json,
        "tls": {"mode": cfg.gateway.tls, "pinned": cfg.gateway.cert_sha256, "seen": seen},
        "store": v,
        "config_problems": cfg.validate(),
    })))
}

/// Config as shown to the browser: no password hash, never the token.
async fn config_get(State(state): State<Shared>) -> ApiResult<Value> {
    let cfg = state.cfg();
    let mut v = serde_json::to_value(&*cfg).map_err(anyhow::Error::from)?;
    if let Some(s) = v.get_mut("server").and_then(|s| s.as_object_mut()) {
        let has = s.get("password_hash").is_some_and(|x| !x.is_null());
        s.remove("password_hash");
        s.insert("password_set".into(), json!(has));
    }
    let token_present = secrets::load_token(&secrets::token_path(
        cfg.gateway.token_file.as_deref(),
        &state.data_dir,
    ))
    .ok()
    .flatten()
    .is_some();
    Ok(Json(json!({
        "config": v,
        "problems": cfg.validate(),
        "warnings": cfg.warnings(),
        "setup": {
            "gateway_host": !cfg.gateway.host.is_empty(),
            "cert_pinned": cfg.gateway.cert_sha256.is_some() || cfg.gateway.tls != TlsMode::Pin,
            "token": token_present,
            "location": cfg.location.is_some(),
            "arrays": !cfg.arrays.is_empty(),
            "tariff": cfg.tariff.enabled,
        },
        "config_path": state.config_path,
    })))
}

#[derive(Deserialize)]
struct ConfigPost {
    config: Value,
    /// Optional new dashboard password (plain text; hashed here).
    new_password: Option<String>,
    clear_password: Option<bool>,
}

async fn config_post(State(state): State<Shared>, Json(req): Json<ConfigPost>) -> ApiResult<Value> {
    let old = state.cfg();
    let mut v = req.config;
    if let Some(s) = v.get_mut("server").and_then(|s| s.as_object_mut()) {
        s.remove("password_set");
        s.insert(
            "password_hash".into(),
            serde_json::to_value(&old.server.password_hash).unwrap_or(Value::Null),
        );
    }
    let mut cfg: Config =
        serde_json::from_value(v).map_err(|e| bad(format!("invalid settings: {e}")))?;
    if let Some(p) = req.new_password.filter(|p| !p.is_empty()) {
        if p.len() < 8 {
            return Err(bad("password must be at least 8 characters"));
        }
        cfg.server.password_hash = Some(crate::auth::hash_password(&p)?);
    }
    if req.clear_password.unwrap_or(false) {
        cfg.server.password_hash = None;
    }
    let problems = cfg.validate();
    if !problems.is_empty() {
        return Err(bad(problems.join("; ")));
    }
    if !state.demo {
        cfg.save(&state.config_path)?;
    }
    let restart_needed = cfg.server.bind != old.server.bind
        || cfg.server.port80 != old.server.port80
        || cfg.server.mdns != old.server.mdns
        || cfg.server.mdns_name != old.server.mdns_name;
    let warnings = cfg.warnings();
    *state.config.write().expect("config") = std::sync::Arc::new(cfg);
    state.reload.notify_one();
    Ok(Json(
        json!({"ok": true, "restart_needed": restart_needed, "saved": !state.demo, "warnings": warnings}),
    ))
}

#[derive(Deserialize)]
struct TokenReq {
    token: String,
}

async fn setup_token(State(state): State<Shared>, Json(req): Json<TokenReq>) -> ApiResult<Value> {
    let token = Token::new(req.token).map_err(|e| bad(e.to_string()))?;
    let info = token.info();
    if info.expires_at.is_some_and(|e| e < now()) {
        return Err(bad("this token has already expired"));
    }
    let cfg = state.cfg();
    let path = secrets::token_path(cfg.gateway.token_file.as_deref(), &state.data_dir);
    secrets::save_token(&path, &token)?;
    if let Some(g) = state.gateway() {
        g.set_token(Some(token)).await;
    }
    state.reload.notify_one();
    Ok(Json(
        json!({"ok": true, "expires_at": info.expires_at, "serial": info.serial}),
    ))
}

async fn setup_pin(State(state): State<Shared>, Json(req): Json<Value>) -> ApiResult<Value> {
    let cfg = state.cfg();
    let host = req
        .get("host")
        .and_then(|h| h.as_str())
        .map(String::from)
        .unwrap_or(cfg.gateway.host.clone());
    if host.is_empty() {
        return Err(bad("enter the gateway address first"));
    }
    let gcfg = crate::config::GatewayConfig {
        host,
        ..cfg.gateway.clone()
    };
    let client = GatewayClient::for_capture(&gcfg)?;
    let _ = client
        .get_text("/info")
        .await
        .map_err(|e| bad(format!("could not reach the gateway: {e}")))?;
    let fp = client
        .seen_fingerprint()
        .ok_or_else(|| bad("no certificate seen"))?;
    Ok(Json(
        json!({"fingerprint": tls::format_fingerprint(&fp), "note": "Confirm this matches what you expect, then save it as the pinned certificate."}),
    ))
}

async fn setup_test(State(state): State<Shared>) -> ApiResult<Value> {
    let Some(g) = state.gateway() else {
        return Err(bad(
            "gateway not configured (address and certificate pin needed)",
        ));
    };
    match g.get_json("/ivp/livedata/status").await {
        Ok(r) => match crate::gateway::model::LiveStatus::from_value(&r.value) {
            Ok(s) => Ok(Json(
                json!({"ok": true, "latency_ms": r.latency_ms, "reading": s}),
            )),
            Err(e) => Ok(Json(
                json!({"ok": false, "error": format!("connected, but unexpected data: {e}")}),
            )),
        },
        Err(e) => Ok(Json(
            json!({"ok": false, "error": e.to_string(), "kind": e.kind()}),
        )),
    }
}

async fn discover(State(state): State<Shared>) -> ApiResult<Value> {
    let Some(g) = state.gateway() else {
        return Err(bad("gateway not configured"));
    };
    let r = gateway::discover(&g).await;
    let rs = r.clone();
    let _ = state
        .db
        .call(move |c| {
            for x in &rs {
                db::kv_set(
                    c,
                    &format!("endpoint:{}", x.path),
                    if x.ok { "ok" } else { &x.status },
                )?;
            }
            Ok(())
        })
        .await;
    Ok(Json(
        json!({"results": r, "note": "Read-only GET requests only."}),
    ))
}

async fn model_reset(State(state): State<Shared>) -> ApiResult<Value> {
    let n = state.db.call(|c| crate::intel::reset_models(c)).await?;
    state.intel_tx.send_modify(|v| *v = now());
    Ok(Json(json!({"ok": true, "reset": n})))
}

async fn backup_now(State(state): State<Shared>) -> ApiResult<Value> {
    let p = crate::run_backup(&state).await?;
    Ok(Json(json!({"ok": true, "path": p})))
}

// ---------------- export ----------------

#[derive(Deserialize)]
struct ExportQ {
    kind: Option<String>,
    from: Option<i64>,
    to: Option<i64>,
    days: Option<i64>,
}

fn csv_escape(s: &str) -> String {
    if s.contains([',', '"', '\n']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

async fn export_csv(
    State(state): State<Shared>,
    Query(q): Query<ExportQ>,
) -> Result<Response, ApiError> {
    let cfg = state.cfg();
    let tz = tz_of(&cfg);
    let t = now();
    let to = q.to.unwrap_or(t + 3600);
    let from = q
        .from
        .unwrap_or(t - q.days.unwrap_or(31).clamp(1, 3660) * 86400);
    let kind = q.kind.unwrap_or_else(|| "hourly".into());
    let k2 = kind.clone();
    let tariff = cfg.tariff.clone();
    let body = state
        .db
        .call(move |c| {
            let mut out = String::new();
            let fmt_local = |ts: i64| forecast::utc(ts).with_timezone(&tz).format("%Y-%m-%d %H:%M:%S %Z").to_string();
            match k2.as_str() {
                "samples" => {
                    out.push_str("utc_ts,local_time,pv_w,load_w,storage_w,grid_w,soc_pct,backup_soc_pct,residual_w\n");
                    let mut st = c.prepare("SELECT ts, pv_w, load_w, storage_w, grid_w, soc, backup_soc, residual_w FROM samples WHERE ts >= ?1 AND ts < ?2 ORDER BY ts")?;
                    let rows = st.query_map([from, to], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, f64>(1)?, r.get::<_, f64>(2)?, r.get::<_, f64>(3)?, r.get::<_, f64>(4)?, r.get::<_, Option<f64>>(5)?, r.get::<_, Option<f64>>(6)?, r.get::<_, f64>(7)?)))?;
                    for r in rows {
                        let r = r?;
                        out.push_str(&format!("{},{},{:.1},{:.1},{:.1},{:.1},{},{},{:.1}\n", r.0, csv_escape(&fmt_local(r.0)), r.1, r.2, r.3, r.4, r.5.map(|x| x.to_string()).unwrap_or_default(), r.6.map(|x| x.to_string()).unwrap_or_default(), r.7));
                    }
                }
                "daily" => {
                    out.push_str("date,pv_kwh,load_kwh,import_kwh,export_kwh,charge_kwh,discharge_kwh,self_consumption_pct,grid_independence_pct,peak_pv_w,peak_load_w\n");
                    for d in daily_rows(c, from, to, tz, false)? {
                        let e = d.energy;
                        out.push_str(&format!(
                            "{},{:.3},{:.3},{:.3},{:.3},{:.3},{:.3},{},{},{},{}\n",
                            d.date,
                            e.pv_wh / 1000.0,
                            e.load_wh / 1000.0,
                            e.import_wh / 1000.0,
                            e.export_wh / 1000.0,
                            e.charge_wh / 1000.0,
                            e.discharge_wh / 1000.0,
                            d.self_consumption.map(|x| format!("{:.1}", x * 100.0)).unwrap_or_default(),
                            d.grid_independence.map(|x| format!("{:.1}", x * 100.0)).unwrap_or_default(),
                            d.peak_pv_w.map(|x| format!("{x:.0}")).unwrap_or_default(),
                            d.peak_load_w.map(|x| format!("{x:.0}")).unwrap_or_default()
                        ));
                    }
                }
                "costs" => {
                    out.push_str("local_date,local_hour,plan,season,period,import_kwh,export_kwh,import_rate,export_rate,import_cost,export_credit,net_cost\n");
                    let rows = db::hourly(c, from, to)?;
                    let rep = tariff.cost_hours(&hour_energy(&rows));
                    for l in rep.lines {
                        out.push_str(&format!(
                            "{},{},{},{},{},{:.3},{:.3},{:.5},{:.5},{:.4},{:.4},{:.4}\n",
                            l.local_date, l.local_hour, csv_escape(&l.plan), csv_escape(&l.season), csv_escape(&l.period), l.import_kwh, l.export_kwh, l.import_per_kwh, l.export_per_kwh, l.import_cost, l.export_credit, l.net_cost
                        ));
                    }
                    out.push_str("# Estimates from measured energy and your entered rates; not a bill.\n");
                }
                _ => {
                    out.push_str("utc_hour_start,local_time,pv_wh,load_wh,import_wh,export_wh,charge_wh,discharge_wh,pv_to_load_wh,pv_to_batt_wh,pv_to_grid_wh,batt_to_load_wh,grid_to_load_wh,grid_to_batt_wh,covered_s,soc_end\n");
                    for h in db::hourly(c, from, to)? {
                        let e = h.e;
                        out.push_str(&format!(
                            "{},{},{:.2},{:.2},{:.2},{:.2},{:.2},{:.2},{:.2},{:.2},{:.2},{:.2},{:.2},{:.2},{:.0},{}\n",
                            h.hour_ts, csv_escape(&fmt_local(h.hour_ts)), e.pv_wh, e.load_wh, e.import_wh, e.export_wh, e.charge_wh, e.discharge_wh, e.pv_to_load_wh, e.pv_to_batt_wh, e.pv_to_grid_wh, e.batt_to_load_wh, e.grid_to_load_wh, e.grid_to_batt_wh, e.covered_s,
                            h.soc_end.map(|x| x.to_string()).unwrap_or_default()
                        ));
                    }
                }
            }
            Ok(out)
        })
        .await?;
    let name = format!(
        "rustsolar-{kind}-{}.csv",
        chrono::Utc::now().format("%Y%m%d")
    );
    Ok((
        [
            (header::CONTENT_TYPE, "text/csv; charset=utf-8".to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{name}\""),
            ),
        ],
        body,
    )
        .into_response())
}

async fn export_json(
    State(state): State<Shared>,
    Query(q): Query<ExportQ>,
) -> Result<Response, ApiError> {
    let t = now();
    let to = q.to.unwrap_or(t + 3600);
    let from = q
        .from
        .unwrap_or(t - q.days.unwrap_or(31).clamp(1, 3660) * 86400);
    let v = state.db.call(move |c| Ok(json!({"exported_at": t, "from": from, "to": to, "units": {"energy": "Wh", "time": "UTC Unix seconds"}, "hours": db::hourly(c, from, to)?}))).await?;
    let name = format!("rustsolar-{}.json", chrono::Utc::now().format("%Y%m%d"));
    Ok((
        [
            (header::CONTENT_TYPE, "application/json".to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{name}\""),
            ),
        ],
        Json(v),
    )
        .into_response())
}

// ---------------- annotations ----------------

#[derive(Deserialize)]
struct AnnotationReq {
    start_ts: i64,
    end_ts: Option<i64>,
    label: String,
    watts: Option<f64>,
}

async fn annotations_list(State(state): State<Shared>) -> ApiResult<Value> {
    let v = state
        .db
        .call(|c| {
            let mut st = c.prepare("SELECT id, start_ts, end_ts, label, watts FROM annotations ORDER BY start_ts DESC LIMIT 500")?;
            let rows: Vec<Value> = st
                .query_map([], |r| Ok(json!({"id": r.get::<_, i64>(0)?, "start_ts": r.get::<_, i64>(1)?, "end_ts": r.get::<_, Option<i64>>(2)?, "label": r.get::<_, String>(3)?, "watts": r.get::<_, Option<f64>>(4)?})))?
                .collect::<rusqlite::Result<_>>()?;
            Ok(json!({"annotations": rows}))
        })
        .await?;
    Ok(Json(v))
}

async fn annotations_add(
    State(state): State<Shared>,
    Json(a): Json<AnnotationReq>,
) -> ApiResult<Value> {
    let label = a.label.trim().to_string();
    if label.is_empty() || label.len() > 200 {
        return Err(bad("label must be 1–200 characters"));
    }
    if a.watts.is_some_and(|w| !(0.0..=50_000.0).contains(&w)) {
        return Err(bad("watts must be 0–50,000"));
    }
    state
        .db
        .call(move |c| {
            c.execute(
                "INSERT INTO annotations(start_ts, end_ts, label, watts) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![a.start_ts, a.end_ts, label, a.watts],
            )?;
            Ok(())
        })
        .await?;
    Ok(Json(json!({"ok": true})))
}

fn max_opt(a: Option<f64>, b: Option<f64>) -> Option<f64> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.max(y)),
        (x, None) | (None, x) => x,
    }
}
