// JSON shapes returned by the RustSolar API (mirrors the Rust structs; Option → null).

export interface LiveStatus {
  last_update: number;
  pv_w: number;
  storage_w: number;
  grid_w: number;
  load_w: number;
  gen_w: number | null;
  soc_pct: number | null;
  backup_soc_pct: number | null;
  enc_agg_energy: number | null;
  enc_agg_soc_pct: number | null;
  main_relay_state: number | null;
  gen_relay_state: number | null;
  phase_count: number | null;
  sc_stream: string | null;
  mqtt_state: string | null;
}

export interface Flows {
  pv_w: number;
  load_w: number;
  import_w: number;
  export_w: number;
  charge_w: number;
  discharge_w: number;
  gen_w: number;
  pv_to_load: number;
  pv_to_batt: number;
  pv_to_grid: number;
  batt_to_load: number;
  batt_to_grid: number;
  grid_to_load: number;
  grid_to_batt: number;
  gen_to_load: number;
  unexplained_w: number;
}

export interface LiveSnapshot {
  status: LiveStatus | null;
  flows: Flows;
  received_at: number | null;
  age_s: number | null;
  stale: boolean;
  /** No new reading for longer than the warning delay (Settings). */
  warn: boolean;
  residual_w: number | null;
  balanced: boolean | null;
  collector: string;
  last_error: string | null;
  consecutive_failures: number;
  demo: boolean;
}

export interface LiveMsg {
  now: number;
  live: LiveSnapshot;
}

export interface SunTimes {
  date: string;
  solar_noon: string;
  sunrise: string | null;
  sunset: string | null;
  civil_dawn: string | null;
  civil_dusk: string | null;
  nautical_dawn: string | null;
  nautical_dusk: string | null;
  astronomical_dawn: string | null;
  astronomical_dusk: string | null;
  day_length_s: number;
  noon_elevation_deg: number;
  polar: 'Day' | 'Night' | null;
}

export interface FcHour {
  ts: number;
  wh: number;
  lo: number;
  hi: number;
  baseline_wh: number | null;
  factor: number;
  ghi: number | null;
  cloud: number | null;
  temp_c: number | null;
  kt: number | null;
  elevation_deg: number;
}

export interface FcDay {
  date: string;
  kwh: number;
  lo_kwh: number;
  hi_kwh: number;
  peak_w: number;
  peak_ts: number | null;
  coverage: number;
}

export interface SolarForecast {
  model: string;
  created_at: number;
  hours: FcHour[];
  days: FcDay[];
  next_hour_wh: number | null;
  rest_of_today_wh: number | null;
  tomorrow_wh: number | null;
  learned_hours: number;
  confidence: string;
  notes: string[];
}

export interface LoadHour {
  ts: number;
  wh: number;
  lo: number;
  hi: number;
}

export interface LoadForecast {
  model: string;
  hours: LoadHour[];
  rest_of_today_wh: number;
  overnight_wh: number | null;
  tomorrow_wh: number | null;
  tomorrow_peak_w: number | null;
  days_of_history: number;
  temperature_term: { cooling_wh_per_c: number; heating_wh_per_c: number; n: number } | null;
  notes: string[];
}

export interface OutageScenario {
  name: string;
  load_w: number | null;
  hours: number | null;
  note: string;
}

export interface BatteryHealth {
  charge_kwh: number;
  discharge_kwh: number;
  equivalent_cycles: number | null;
  observed_round_trip: number | null;
  since: number | null;
  implied_capacity_wh: number | null;
  capacity_trend: [number, number][];
  note: string;
}

export interface BatteryOutlook {
  soc_pct: number | null;
  reserve_pct: number | null;
  usable_wh: number | null;
  capacity_source: string;
  max_power_w: number | null;
  power_source: string;
  units: number | null;
  state: string;
  power_w: number;
  time_to_full: number | null;
  time_to_reserve: number | null;
  soc_at_sunset: number | null;
  soc_at_sunrise: number | null;
  sim: [number, number][];
  expected_import_wh_24h: number | null;
  expected_export_wh_24h: number | null;
  overnight_battery_wh: number | null;
  outage: OutageScenario[];
  health: BatteryHealth;
  assumptions: string[];
}

export interface Recommendation {
  id: string;
  kind: string;
  severity: string;
  title: string;
  message: string;
  why: string;
  evidence: [string, string][];
  confidence: string;
  benefit: string | null;
  window: [number, number] | null;
  actionable_now: boolean;
}

export interface Intel {
  pending?: boolean;
  computed_at: number;
  sun_today: SunTimes | null;
  sun_tomorrow: SunTimes | null;
  solar: SolarForecast | null;
  load: LoadForecast | null;
  battery: BatteryOutlook;
  recommendations: Recommendation[];
  needs: string[];
}

export interface EnergyRow {
  pv_wh: number;
  load_wh: number;
  import_wh: number;
  export_wh: number;
  charge_wh: number;
  discharge_wh: number;
  gen_wh: number;
  pv_to_load_wh: number;
  pv_to_batt_wh: number;
  pv_to_grid_wh: number;
  batt_to_load_wh: number;
  batt_to_grid_wh: number;
  grid_to_load_wh: number;
  grid_to_batt_wh: number;
  covered_s: number;
}

export interface PeriodSummary {
  name: string;
  from: number;
  energy: EnergyRow;
  self_consumption: number | null;
  solar_direct_share: number | null;
  battery_share: number | null;
  grid_independence: number | null;
}

export interface Overview {
  periods: PeriodSummary[];
  today_peak_pv_w: number | null;
  today_peak_load_w: number | null;
  monitoring_since: number | null;
  gateway_totals: { pv_lifetime_wh: number | null; pv_today_wh: number | null; consumption_lifetime_wh: number | null; inverter_count: number | null } | null;
}

export interface StoredSample {
  ts: number;
  pv_w: number;
  load_w: number;
  storage_w: number;
  grid_w: number;
  soc: number | null;
}

export interface DayRow {
  date: string;
  start: number;
  energy: EnergyRow;
  self_consumption: number | null;
  grid_independence: number | null;
  peak_pv_w: number | null;
  peak_load_w: number | null;
}

export interface WeatherHour {
  ts: number;
  temp_c: number | null;
  rh: number | null;
  cloud: number | null;
  cloud_low: number | null;
  cloud_mid: number | null;
  cloud_high: number | null;
  ghi: number | null;
  dni: number | null;
  dhi: number | null;
  precip_prob: number | null;
  precip_mm: number | null;
  snow_cm: number | null;
  wind_ms: number | null;
  gust_ms: number | null;
  visibility_m: number | null;
  code: number | null;
  is_day: number | null;
  pm25: number | null;
  aqi: number | null;
  aod: number | null;
}

export interface WeatherStatus {
  provider: string;
  attribution: string;
  last_success_at: number | null;
  last_attempt_at: number | null;
  last_error: string | null;
  requests_today: number;
  daily_budget: number;
  next_refresh_at: number | null;
}

export interface WeatherResp {
  hours: WeatherHour[];
  status: WeatherStatus;
  current: Record<string, number> | null;
}

export interface Accuracy {
  n: number;
  mae_wh: number | null;
  rmse_wh: number | null;
  bias_wh: number | null;
  mape_pct: number | null;
  daily_mape_pct: number | null;
  by_hour: [number, number, number][];
  by_month: [number, number, number][];
  by_sky: [string, number, number][];
}

export interface AccuracyResp {
  pv_hour_ahead: Accuracy;
  pv_day_ahead: Accuracy;
  load_hour_ahead: Accuracy;
  pv_series: [number, number, number][];
  note: string;
}

export interface AlertRow {
  id: number;
  key: string;
  severity: string;
  title: string;
  detail: string;
  created_at: number;
  last_seen: number;
  cleared_at: number | null;
  acked_at: number | null;
  count: number;
}

export interface DayTotals {
  date: string;
  hours: number;
  import_kwh: number;
  import_cost: number;
  tier_adder_cost: number;
  baseline_credit: number;
  export_kwh: number;
  export_credit: number;
  fixed_charge: number;
  load_kwh: number;
  pv_kwh: number;
  avoided_cost: number;
  no_solar_cost: number;
  net_cost: number;
}

export interface CostTotals {
  import_kwh: number;
  import_cost: number;
  tier_adder_cost: number;
  baseline_credit: number;
  export_kwh: number;
  export_credit: number;
  fixed_charge: number;
  demand_charge: number;
  load_kwh: number;
  pv_kwh: number;
  avoided_cost: number;
  no_solar_cost: number;
  net_cost: number;
}

export interface CostResp {
  enabled: boolean;
  note?: string;
  report?: { currency: string; days: DayTotals[]; months: unknown[]; totals: CostTotals; notes: string[] };
}

export interface ArraySpec {
  name: string;
  dc_capacity_w: number;
  tilt_deg: number;
  azimuth_deg: number;
  ac_limit_w: number | null;
  temp_coeff_per_c: number;
  system_losses: number;
  inverter_efficiency: number;
  albedo: number;
  shading: unknown[];
}

export interface Config {
  server: { bind: string; password_set?: boolean; allow_unauthenticated_lan: boolean; session_days: number; mdns: boolean; mdns_name: string; port80: boolean; forward?: { host: string; to: string }[] };
  gateway: {
    host: string;
    tls: 'pin' | 'ca' | 'insecure_dev';
    cert_sha256: string | null;
    ca_cert_path: string | null;
    token_file: string | null;
    connect_timeout_s: number;
    request_timeout_s: number;
    store_raw: boolean;
  };
  collector: { enabled: boolean; interval_s: number; stale_after_s: number; warn_after_min: number; max_integration_gap_s: number; max_backoff_s: number; inventory_interval_s: number };
  location: { latitude: number; longitude: number; elevation_m: number | null; time_zone: string } | null;
  arrays: ArraySpec[];
  battery: { usable_kwh: number | null; max_power_kw: number | null; round_trip_eff: number; outage_floor_pct: number; essential_load_kw: number };
  weather: { enabled: boolean; refresh_min: number; air_quality: boolean; daily_request_budget: number; forecast_days: number };
  tariff: { enabled: boolean; currency: string; time_zone: string; holidays: string[]; plans: unknown[]; pge_holidays?: boolean };
  alerts: {
    low_soc_pct: number;
    underperformance_frac: number;
    high_load_factor: number;
    quiet_start_hour: number;
    quiet_end_hour: number;
    repeat_after_min: number;
    webhook_url: string | null;
  };
  retention: { raw_days: number; five_min_days: number; diagnostics_days: number };
  backup: { enabled: boolean; dir: string | null; hour: number; keep: number };
}

export interface ConfigResp {
  config: Config;
  problems: string[];
  warnings: string[];
  setup: { gateway_host: boolean; cert_pinned: boolean; token: boolean; location: boolean; arrays: boolean; tariff: boolean };
  config_path: string;
}

export interface SessionResp {
  authenticated: boolean;
  password_required: boolean;
  demo: boolean;
  version: string;
  warnings: string[];
}
