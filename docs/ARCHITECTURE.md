# RustSolar architecture

One Rust executable (`rustsolar`) runs the collector, analytics and an Axum web server that
serves the embedded TypeScript dashboard. Storage is a single SQLite file (WAL mode). No
Docker, Redis, PostgreSQL or message broker.

```
            ┌──────────────── rustsolar (tokio) ────────────────┐
 Gateway ──►│ collector ──► SQLite ◄── intel (forecasts, battery,│
 (GET only) │  (12 s)        ▲  ▲       recommendations, alerts) │──► webhook (optional)
            │ inventory ─────┘  │                                │
 Open-Meteo►│ weather (60 min) ─┘   maintenance (retention,      │
            │                        integrity, daily backup)    │
            │ Axum API + SSE ◄── watch channels (live, intel)    │◄── browser (LAN)
            └────────────────────────────────────────────────────┘
```

## Crates and modules

`crates/core` (`rustsolar-core`) — pure, deterministic, unit-tested calculations, no I/O:

| Module | Contents |
|---|---|
| `energy` | Sign conventions, flow attribution, trapezoid integration across hour boundaries, gap/duplicate handling, ratios. |
| `astro` | NOAA solar position (Meeus), sunrise/sunset/twilights by local date (DST-aware), angle of incidence, annual table. |
| `pvmodel` | Extraterrestrial irradiance, Hay–Davies transposition, ASHRAE IAM, Faiman cell temperature, PVWatts DC, inverter efficiency, clipping, shading rules, Haurwitz clear sky. |
| `battery` | Self-consumption SOC simulation, time-to-SOC, outage runtime (constant and profile), capacity from stored energy, cycles, round-trip. |
| `tariff` | Tariff config types, validation, rate lookup, hourly cost report. |

`src/` (binary):

| Module | Role |
|---|---|
| `config` | TOML config with validation (`deny_unknown_fields`). |
| `secrets` | Token loading (env/file), JWT expiry decoding, redaction, owner-only file permissions. |
| `gateway` | Read-only HTTPS client (GET only), TLS pinning, endpoint discovery, JSON extraction. |
| `collector` | Live polling loop, backoff, dedupe, stale detection, inventory poller, demo generator. |
| `db` | Schema migrations, sample insert + hourly energy accumulation in one transaction, queries, retention, backup. |
| `weather` | Open-Meteo client with budget, 429/Retry-After handling, forecast versions, air quality. |
| `forecast` | Solar forecast + learned correction, load forecast, accuracy metrics. |
| `intel` | Periodic pass: learning, forecasts, battery outlook, recommendations; stores forecast snapshots. |
| `alerts` | Condition evaluation, dedupe/flap handling, quiet hours, webhook. |
| `api` / `auth` | HTTP API, SSE stream, login sessions, CSRF check, security headers, static UI. |

## Gateway facts (verified on the owner's system, 2026-10-25)

- IQ Gateway firmware D8.3.6086 (the serial below is a placeholder, `<serial>`).
- Certificate: X.509 v3, ECDSA P-256, issuer `O=Enphase Energy, OU=HSM, CN=Cloud.HSM.TA`,
  subject `<serial>.800-00664-r05._mDevice.enphaseenergy.com`, valid to 9999.
  Pinning its SHA-256 fingerprint works with rustls.
- `/ivp/livedata/status` without a token → HTTP 401 (expected).
- Power fields are milliwatts (`agg_p_mw`); divide by 1 000 for W.
- The body contains case-distinct keys (`MqttClient_connect_err` / `…_Err`) and repeated
  empty keys under `dry_contacts`. `serde_json::Value` accepts both (last duplicate wins);
  typed extraction then reads only the fields we use.

## Sign conventions and conservation

`pv + storage + grid + generator = load`, with storage negative when charging and grid
negative when exporting. Each sample's residual is stored; samples outside
max(150 W, 5 %) are counted as unbalanced in diagnostics (metering is not simultaneous).

Flow attribution (an inference — meters report only net values): solar → home first, then
→ battery, then → grid; battery → remaining home load, then → grid; grid → remaining home
load, then → battery. Every path is bounded by measured totals.

## Energy integration

For consecutive distinct samples `a`, `b` (gateway timestamps): if `b.ts ≤ a.ts` the reading
is a duplicate; if `b.ts − a.ts > max_integration_gap_s` (120 s) it is a gap and nothing is
integrated. Otherwise each directional flow is integrated with the trapezoid rule, splitting
at hour boundaries by linear interpolation. The hourly row and the sample are written in the
same transaction, so a crash cannot double-count. `rebuild-energy` recomputes from raw samples.

## Data model (SQLite)

| Table | Contents | Retention |
|---|---|---|
| `samples` | Every distinct gateway reading (W, SOC, residual). | 90 days |
| `agg_5min` | 5-minute means. | 3 years |
| `energy_hourly` | Hourly Wh per flow and path, coverage seconds, SOC at end, peaks. | forever |
| `weather_hourly` | Every fetched forecast version per hour. | thinned after 30 days to latest + day-ahead |
| `weather_fetch` | Fetch log and raw bodies (snapshots). | bodies 14 days |
| `kv_air` | PM2.5, US AQI, aerosol optical depth. | — |
| `forecasts` | Hourly solar/load forecasts stored once per hour for 48 h ahead. | 400 days |
| `model_params` | Learned correction state (JSON, versioned). | — |
| `alerts`, `events` | Alert history; diagnostics log. | events 30 days |
| `kv`, `api_usage`, `annotations`, `raw_responses` | Gateway inventory cache, request counters, user notes, optional raw bodies (last 500). | — |

All times are UTC Unix seconds; the UI shows local time.

## Solar forecast

1. Weather row at time T holds radiation averaged over [T−1 h, T) (Open-Meteo convention),
   so it drives the energy hour starting at T−1 h; sun position is taken at its midpoint.
2. Per array: Hay–Davies POA (beam with IAM, anisotropic + isotropic sky diffuse, ground
   reflection), Faiman cell temperature (u0 = 25, u1 = 6.84), DC = Pdc0·G/1000·(1+γ(Tc−25)),
   × (1 − losses), × inverter efficiency, clipped at the AC limit. Defaults (γ −0.37 %/°C,
   14 % losses, 97 % inverter) are generic estimates and are marked as such.
3. Correction factor per cell of (sky: overcast/partly/clear by clear-sky index) × (sun:
   <15°, 15–35°, >35°) = Σ actual / Σ baseline with a 30-day half-life and a prior equal to
   3 kWh of baseline that keeps the factor near 1.0 until data accumulates.
4. Learning uses, for each completed hour, the weather version fetched **before that hour
   began** (`weather::hours_as_of`), so the model never learns from information that was not
   available at forecast time. Reset from Settings (stored in `model_params`).
5. Intervals: 10th–90th percentile of recent actual/forecast ratios per sky bin once 30 exist;
   until then fixed default bands, labelled as defaults.
6. Without array specs an empirical model `pv ≈ k·GHI` is learned from history instead.

Accuracy (MAE, RMSE, bias, MAPE, daily MAPE, by hour/month/sky) compares stored forecasts
issued ≥ 1 h (and ≥ 12 h) before each hour with the energy later measured.

## Battery

Capacity: configured › gateway inventory (sum of per-unit `encharge_capacity`) › median of
`enc_agg_energy ÷ SOC` (labelled unverified). Power limit: configured › units × 3.84 kW
(IQ Battery 5P datasheet, only when the inventory confirms the count) › highest observed
power (lower bound). The battery count is never inferred from the aggregate energy field.
Simulation: self-consumption mode, hourly, √RTE each way, reserve floor in normal operation,
outage floor (default 0 %) during outages. Health values are calculations, not BMS data.

## Alerts

Each condition has a stable key. Active alerts update in place; cleared when the condition
ends; a condition returning within `repeat_after_min` reopens the same alert. Webhook
notifications skip quiet hours except for critical alerts.

## Reliability

Single polling loop (no overlap), reqwest connect/request timeouts plus an outer timeout,
exponential backoff with ±20 % jitter (slow retry for config errors like a rejected token),
graceful shutdown on Ctrl+C/SIGTERM, worker restart on config change, WAL + `synchronous=NORMAL`,
hourly `PRAGMA quick_check`, daily `VACUUM INTO` backups, rolling daily logs (14 kept).
