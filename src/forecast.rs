//! Solar and household-load forecasting.
//!
//! Solar: a physical model (sun position → Hay–Davies plane-of-array irradiance → cell
//! temperature → DC → AC with clipping, see `rustsolar_core::pvmodel`) driven by the
//! Open-Meteo hourly forecast, then multiplied by a learned correction factor per
//! (sky condition × sun elevation) cell. The correction is learned only from forecasts
//! that were available BEFORE each hour began (no leakage), with exponential forgetting
//! and shrinkage toward 1.0 while data is scarce.
//!
//! When no array specs are configured, an empirical model (`pv ≈ k·GHI`) is learned from
//! history instead; until then no solar forecast is shown.
//!
//! Load: recency-weighted hour-of-day profile split by weekday/weekend, with an optional
//! temperature term once enough data exists.

use chrono::{DateTime, Datelike, NaiveDate, TimeZone, Timelike, Utc};
use rustsolar_core::astro;
use rustsolar_core::pvmodel::{self, ArraySpec, SunAngles, Weather};
use serde::{Deserialize, Serialize};

use crate::db::HourRow;
use crate::weather::WeatherHour;

pub const PHYSICAL_MODEL: &str = "physical-v1";
pub const EMPIRICAL_MODEL: &str = "empirical-ghi-v1";

/// Sky bins by clear-sky index (sun bins by elevation are internal).
pub const SKY_BINS: [&str; 3] = ["overcast", "partly cloudy", "clear"];

pub fn sky_bin(kt: Option<f64>) -> usize {
    match kt {
        Some(k) if k >= 0.7 => 2,
        Some(k) if k >= 0.3 => 1,
        _ => 0,
    }
}
pub fn sun_bin(elev: f64) -> usize {
    if elev < 15.0 {
        0
    } else if elev < 35.0 {
        1
    } else {
        2
    }
}

/// Baseline (uncorrected) estimate for the energy hour starting at `hour_start`,
/// from the weather row whose radiation covers that hour (row ts = hour_start + 3600).
#[derive(Debug, Clone, Copy, Serialize)]
pub struct Baseline {
    /// Physical model AC Wh, or for the empirical model, GHI Wh/m² (the regressor).
    pub value: f64,
    pub kt: Option<f64>,
    pub elevation_deg: f64,
    pub cell: usize,
}

pub fn baseline_for(
    hour_start: i64,
    w: &WeatherHour,
    lat: f64,
    lon: f64,
    arrays: &[ArraySpec],
) -> Baseline {
    let mid = DateTime::from_timestamp(hour_start + 1800, 0).unwrap_or_default();
    let pos = astro::solar_position(mid, lat, lon);
    let ghi = w.ghi.unwrap_or(0.0).max(0.0);
    let kt = pvmodel::clear_sky_index(ghi, pos.zenith_deg);
    let cell = sky_bin(kt) * 3 + sun_bin(pos.elevation_deg);
    let value = if arrays.is_empty() {
        ghi
    } else {
        let wx = Weather {
            ghi,
            dni: w.dni.unwrap_or(0.0).max(0.0),
            dhi: w.dhi.unwrap_or(0.0).max(0.0),
            temp_air_c: w.temp_c.unwrap_or(20.0),
            wind_ms: w.wind_ms.unwrap_or(1.0),
        };
        let sun = SunAngles {
            zenith_deg: pos.zenith_deg,
            azimuth_deg: pos.azimuth_deg,
        };
        pvmodel::system_ac_w(arrays, sun, &wx, mid.ordinal())
    };
    Baseline {
        value,
        kt,
        elevation_deg: pos.elevation_deg,
        cell,
    }
}

/// Learned correction state, persisted as JSON in `model_params`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SolarCorrection {
    pub model: String,
    /// Per cell: Σw·actual, Σw·baseline, sample count.
    pub num: [f64; 9],
    pub den: [f64; 9],
    pub n: [u32; 9],
    /// Recent actual/corrected-forecast ratios per sky bin, for intervals.
    pub ratios: [Vec<f64>; 3],
    /// Last energy hour (start) incorporated.
    pub last_hour: i64,
    pub version: u32,
    pub reset_at: i64,
}

impl SolarCorrection {
    pub fn new(model: &str, now: i64) -> Self {
        Self {
            model: model.into(),
            num: [0.0; 9],
            den: [0.0; 9],
            n: [0; 9],
            ratios: Default::default(),
            last_hour: 0,
            version: 1,
            reset_at: now,
        }
    }

    /// Prior weight (in baseline units: Wh for the physical model, Wh/m² for the
    /// empirical one) that pulls the factor toward the prior while data is scarce.
    fn prior(&self) -> f64 {
        3000.0
    }

    /// Correction factor for a cell. Physical model: shrinks toward 1.0. Empirical model:
    /// pooled factor across cells as the prior (None until any data exists).
    pub fn factor(&self, cell: usize) -> Option<f64> {
        if self.model == PHYSICAL_MODEL {
            let p = self.prior();
            Some(((self.num[cell] + p) / (self.den[cell] + p)).clamp(0.3, 2.0))
        } else {
            let (tn, td): (f64, f64) = (self.num.iter().sum(), self.den.iter().sum());
            if td < 2000.0 {
                return None;
            }
            let pooled = tn / td;
            let p = self.prior();
            Some((self.num[cell] + p * pooled) / (self.den[cell] + p))
        }
    }

    /// Incorporate one completed hour. `decay` per hour (e.g. half-life 30 days).
    pub fn learn(&mut self, b: &Baseline, actual_wh: f64) {
        if b.value <= 1.0 || b.elevation_deg < 2.0 {
            return;
        }
        let decay = 0.5f64.powf(1.0 / (30.0 * 24.0));
        for i in 0..9 {
            self.num[i] *= decay;
            self.den[i] *= decay;
        }
        if let Some(f) = self.factor(b.cell) {
            let pred = b.value * f;
            if pred > 50.0 {
                let r = &mut self.ratios[b.cell / 3];
                r.push(actual_wh / pred);
                if r.len() > 300 {
                    r.remove(0);
                }
            }
        }
        self.num[b.cell] += actual_wh.max(0.0);
        self.den[b.cell] += b.value;
        self.n[b.cell] += 1;
    }

    pub fn total_samples(&self) -> u32 {
        self.n.iter().sum()
    }

    /// (lo, hi) multipliers for an ~80 % interval for a sky bin.
    pub fn interval(&self, sky: usize) -> (f64, f64, bool) {
        let r = &self.ratios[sky];
        if r.len() >= 30 {
            let mut v = r.clone();
            v.sort_by(|a, b| a.total_cmp(b));
            let q = |p: f64| v[((v.len() - 1) as f64 * p).round() as usize];
            (q(0.1).min(1.0), q(0.9).max(1.0), true)
        } else {
            // Default bands until history accumulates (assumption, shown as such).
            match sky {
                2 => (0.85, 1.08, false),
                1 => (0.6, 1.3, false),
                _ => (0.4, 1.6, false),
            }
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct FcHour {
    pub ts: i64,
    pub wh: f64,
    pub lo: f64,
    pub hi: f64,
    pub baseline_wh: Option<f64>,
    pub factor: f64,
    pub ghi: Option<f64>,
    pub cloud: Option<f64>,
    pub temp_c: Option<f64>,
    pub kt: Option<f64>,
    pub elevation_deg: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct FcDay {
    pub date: NaiveDate,
    pub kwh: f64,
    pub lo_kwh: f64,
    pub hi_kwh: f64,
    pub peak_w: f64,
    pub peak_ts: Option<i64>,
    /// Fraction of the day's daylight hours covered by forecast data.
    pub coverage: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SolarForecast {
    pub model: String,
    pub created_at: i64,
    pub hours: Vec<FcHour>,
    pub days: Vec<FcDay>,
    pub next_hour_wh: Option<f64>,
    pub rest_of_today_wh: Option<f64>,
    pub tomorrow_wh: Option<f64>,
    pub learned_hours: u32,
    pub confidence: String,
    pub notes: Vec<String>,
}

/// Build the solar forecast for every weather row from the current hour onward.
pub fn solar_forecast(
    now: i64,
    weather: &[WeatherHour],
    lat: f64,
    lon: f64,
    tz: chrono_tz::Tz,
    arrays: &[ArraySpec],
    corr: &SolarCorrection,
) -> Option<SolarForecast> {
    let cur_hour = now.div_euclid(3600) * 3600;
    let mut hours = Vec::new();
    for w in weather {
        let hour_start = w.ts - 3600;
        if hour_start < cur_hour {
            continue;
        }
        let b = baseline_for(hour_start, w, lat, lon, arrays);
        let Some(f) = corr.factor(b.cell) else {
            continue;
        };
        let wh = (b.value * f).max(0.0);
        let (lo, hi, _) = corr.interval(b.cell / 3);
        hours.push(FcHour {
            ts: hour_start,
            wh,
            lo: wh * lo,
            hi: wh * hi,
            baseline_wh: (corr.model == PHYSICAL_MODEL).then_some(b.value),
            factor: f,
            ghi: w.ghi,
            cloud: w.cloud,
            temp_c: w.temp_c,
            kt: b.kt,
            elevation_deg: b.elevation_deg,
        });
    }
    if hours.is_empty() {
        return None;
    }
    // Daily totals by local date.
    let mut days: Vec<FcDay> = Vec::new();
    for h in &hours {
        let d = local_date(h.ts, tz);
        if days.last().is_none_or(|x| x.date != d) {
            days.push(FcDay {
                date: d,
                kwh: 0.0,
                lo_kwh: 0.0,
                hi_kwh: 0.0,
                peak_w: 0.0,
                peak_ts: None,
                coverage: 0.0,
            });
        }
        let day = days.last_mut().expect("pushed");
        day.kwh += h.wh / 1000.0;
        day.lo_kwh += h.lo / 1000.0;
        day.hi_kwh += h.hi / 1000.0;
        if h.wh > day.peak_w {
            day.peak_w = h.wh; // hourly mean power, W
            day.peak_ts = Some(h.ts + 1800);
        }
        if h.elevation_deg > 0.0 {
            day.coverage += 1.0;
        }
    }
    for d in &mut days {
        let st = astro::sun_times(d.date, lat, lon, tz);
        let daylight_h = (st.day_length_s / 3600.0).ceil().max(1.0);
        d.coverage = (d.coverage / daylight_h).min(1.0);
    }
    let today = local_date(now, tz);
    let tomorrow = today.succ_opt();
    let frac_left = 1.0 - (now - cur_hour) as f64 / 3600.0;
    let rest: f64 = hours
        .iter()
        .filter(|h| local_date(h.ts, tz) == today)
        .map(|h| {
            if h.ts == cur_hour {
                h.wh * frac_left
            } else {
                h.wh
            }
        })
        .sum();
    let next_hour = hours.iter().find(|h| h.ts == cur_hour).map(|h| {
        h.wh * frac_left
            + hours
                .iter()
                .find(|x| x.ts == cur_hour + 3600)
                .map(|x| x.wh * (1.0 - frac_left))
                .unwrap_or(0.0)
    });
    let tomorrow_wh = days
        .iter()
        .find(|d| Some(d.date) == tomorrow && d.coverage > 0.9)
        .map(|d| d.kwh * 1000.0);
    let n = corr.total_samples();
    let confidence = if n < 24 * 3 {
        format!("low — model not yet calibrated ({n} daylight hours learned)")
    } else if n < 24 * 30 {
        format!("moderate — {n} daylight hours learned")
    } else {
        format!("calibrated — {n} daylight hours learned")
    };
    let mut notes = vec!["Estimate from forecast irradiance; not a measurement.".to_string()];
    if corr.model == EMPIRICAL_MODEL {
        notes.push("No roof-array specs configured: using an empirical model learned from your history. Add array specs in Settings for a physical model.".into());
    }
    Some(SolarForecast {
        model: corr.model.clone(),
        created_at: now,
        hours,
        days,
        next_hour_wh: next_hour,
        rest_of_today_wh: Some(rest),
        tomorrow_wh,
        learned_hours: n,
        confidence,
        notes,
    })
}

pub fn local_date(ts: i64, tz: chrono_tz::Tz) -> NaiveDate {
    DateTime::from_timestamp(ts, 0)
        .unwrap_or_default()
        .with_timezone(&tz)
        .date_naive()
}

pub fn local_hour(ts: i64, tz: chrono_tz::Tz) -> u32 {
    DateTime::from_timestamp(ts, 0)
        .unwrap_or_default()
        .with_timezone(&tz)
        .hour()
}

/// Start of the local day containing `ts`, as UTC seconds.
pub fn local_midnight(ts: i64, tz: chrono_tz::Tz) -> i64 {
    let d = local_date(ts, tz);
    day_start(d, tz)
}

pub fn day_start(d: NaiveDate, tz: chrono_tz::Tz) -> i64 {
    let naive = d.and_hms_opt(0, 0, 0).expect("midnight");
    match tz.from_local_datetime(&naive) {
        chrono::LocalResult::Single(t) | chrono::LocalResult::Ambiguous(t, _) => t.timestamp(),
        chrono::LocalResult::None => tz
            .from_local_datetime(&(naive + chrono::Duration::hours(1)))
            .earliest()
            .map(|t| t.timestamp())
            .unwrap_or(0),
    }
}

fn is_weekend(d: NaiveDate) -> bool {
    matches!(d.weekday(), chrono::Weekday::Sat | chrono::Weekday::Sun)
}

// ---------------- Load forecast ----------------

#[derive(Debug, Clone, Serialize)]
pub struct LoadHour {
    pub ts: i64,
    pub wh: f64,
    pub lo: f64,
    pub hi: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct LoadForecast {
    pub model: String,
    pub hours: Vec<LoadHour>,
    pub rest_of_today_wh: f64,
    pub overnight_wh: Option<f64>,
    pub tomorrow_wh: Option<f64>,
    pub tomorrow_peak_w: Option<f64>,
    pub days_of_history: f64,
    pub temperature_term: Option<TempTerm>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct TempTerm {
    /// Wh per hour per °C above 22 °C (cooling) and below 15 °C (heating).
    pub cooling_wh_per_c: f64,
    pub heating_wh_per_c: f64,
    pub n: usize,
}

/// Recency-weighted profile: for each (weekend?, local hour) a weighted mean and spread
/// of hourly load from the last 42 days. Hours with < 80 % coverage are skipped.
pub fn load_forecast(
    now: i64,
    history: &[HourRow],
    temps: &std::collections::HashMap<i64, f64>,
    tz: chrono_tz::Tz,
    horizon_h: i64,
    sunrise_next: Option<i64>,
    sunset_today: Option<i64>,
) -> Option<LoadForecast> {
    let good: Vec<&HourRow> = history.iter().filter(|h| h.e.covered_s >= 2880.0).collect();
    if good.len() < 36 {
        return None;
    }
    let days_hist = good.len() as f64 / 24.0;
    // Stats per [weekend][hour]: Σw, Σw·x, Σw·x².
    let mut acc = [[(0.0f64, 0.0f64, 0.0f64); 24]; 2];
    let mut all = [(0.0f64, 0.0f64, 0.0f64); 24];
    for h in &good {
        let d = local_date(h.hour_ts, tz);
        let age_days = (now - h.hour_ts) as f64 / 86400.0;
        let w = 0.5f64.powf(age_days / 14.0);
        let hr = local_hour(h.hour_ts, tz) as usize;
        let x = h.e.load_wh * 3600.0 / h.e.covered_s;
        let a = &mut acc[is_weekend(d) as usize][hr];
        a.0 += w;
        a.1 += w * x;
        a.2 += w * x * x;
        let b = &mut all[hr];
        b.0 += w;
        b.1 += w * x;
        b.2 += w * x * x;
    }
    // Temperature term: least squares of residual vs (CDH, HDH) if enough paired data.
    let base_mean = |we: usize, hr: usize| {
        let a = acc[we][hr];
        if a.0 > 0.5 {
            a.1 / a.0
        } else {
            all[hr].1 / all[hr].0.max(1e-9)
        }
    };
    let mut sxx = [[0.0f64; 2]; 2];
    let mut sxy = [0.0f64; 2];
    let mut n_t = 0usize;
    for h in &good {
        let Some(&t) = temps.get(&h.hour_ts) else {
            continue;
        };
        let d = local_date(h.hour_ts, tz);
        let hr = local_hour(h.hour_ts, tz) as usize;
        let resid = h.e.load_wh * 3600.0 / h.e.covered_s - base_mean(is_weekend(d) as usize, hr);
        let x = [(t - 22.0).max(0.0), (15.0 - t).max(0.0)];
        for i in 0..2 {
            sxy[i] += x[i] * resid;
            for j in 0..2 {
                sxx[i][j] += x[i] * x[j];
            }
        }
        n_t += 1;
    }
    let temp_term = if n_t >= 24 * 14 {
        let det = sxx[0][0] * sxx[1][1] - sxx[0][1] * sxx[1][0];
        (det.abs() > 1e-6).then(|| {
            let c = (sxx[1][1] * sxy[0] - sxx[0][1] * sxy[1]) / det;
            let hcoef = (sxx[0][0] * sxy[1] - sxx[1][0] * sxy[0]) / det;
            TempTerm {
                cooling_wh_per_c: c.max(0.0),
                heating_wh_per_c: hcoef.max(0.0),
                n: n_t,
            }
        })
    } else {
        None
    };

    let cur_hour = now.div_euclid(3600) * 3600;
    let mut hours = Vec::new();
    for k in 0..horizon_h {
        let ts = cur_hour + k * 3600;
        let d = local_date(ts, tz);
        let we = is_weekend(d) as usize;
        let hr = local_hour(ts, tz) as usize;
        let a = if acc[we][hr].0 > 0.5 {
            acc[we][hr]
        } else {
            all[hr]
        };
        if a.0 <= 0.0 {
            continue;
        }
        let mean = a.1 / a.0;
        let var = (a.2 / a.0 - mean * mean).max(0.0);
        let mut wh = mean;
        if let (Some(tt), Some(&t)) = (temp_term, temps.get(&ts)) {
            wh += tt.cooling_wh_per_c * (t - 22.0).max(0.0)
                + tt.heating_wh_per_c * (15.0 - t).max(0.0);
        }
        let sd = var.sqrt();
        hours.push(LoadHour {
            ts,
            wh: wh.max(0.0),
            lo: (wh - 1.28 * sd).max(0.0),
            hi: wh + 1.28 * sd,
        });
    }
    let today = local_date(now, tz);
    let frac_left = 1.0 - (now - cur_hour) as f64 / 3600.0;
    let rest: f64 = hours
        .iter()
        .filter(|h| local_date(h.ts, tz) == today)
        .map(|h| {
            if h.ts == cur_hour {
                h.wh * frac_left
            } else {
                h.wh
            }
        })
        .sum();
    let tomorrow = today.succ_opt();
    let tom: Vec<&LoadHour> = hours
        .iter()
        .filter(|h| Some(local_date(h.ts, tz)) == tomorrow)
        .collect();
    let tomorrow_wh = (tom.len() >= 23).then(|| tom.iter().map(|h| h.wh).sum());
    let tomorrow_peak = tom
        .iter()
        .map(|h| h.wh)
        .fold(None, |m: Option<f64>, x| Some(m.map_or(x, |m| m.max(x))));
    let overnight = match (sunset_today, sunrise_next) {
        (Some(ss), Some(sr)) if sr > ss => Some(
            hours
                .iter()
                .filter(|h| h.ts + 3600 > ss.max(now) && h.ts < sr)
                .map(|h| {
                    let a = h.ts.max(ss.max(now));
                    let b = (h.ts + 3600).min(sr);
                    h.wh * ((b - a).max(0) as f64 / 3600.0)
                })
                .sum(),
        ),
        _ => None,
    };
    let mut notes =
        vec!["Estimate from your recent consumption pattern; not a measurement.".to_string()];
    if days_hist < 14.0 {
        notes.push(format!("Only {days_hist:.1} days of history; the pattern will sharpen over the first few weeks."));
    }
    Some(LoadForecast {
        model: if temp_term.is_some() {
            "profile+temperature-v1".into()
        } else {
            "profile-v1".into()
        },
        hours,
        rest_of_today_wh: rest,
        overnight_wh: overnight,
        tomorrow_wh,
        tomorrow_peak_w: tomorrow_peak,
        days_of_history: days_hist,
        temperature_term: temp_term,
        notes,
    })
}

// ---------------- Accuracy ----------------

#[derive(Debug, Clone, Default, Serialize)]
pub struct Accuracy {
    pub n: usize,
    pub mae_wh: Option<f64>,
    pub rmse_wh: Option<f64>,
    pub bias_wh: Option<f64>,
    /// Mean absolute percentage error over hours with actual ≥ 200 Wh.
    pub mape_pct: Option<f64>,
    pub daily_mape_pct: Option<f64>,
    pub by_hour: Vec<(u32, f64, usize)>,
    pub by_month: Vec<(u32, f64, usize)>,
    pub by_sky: Vec<(String, f64, usize)>,
}

/// Pairs of (target hour start, forecast Wh, actual Wh, sky bin).
pub fn accuracy(pairs: &[(i64, f64, f64, usize)], tz: chrono_tz::Tz) -> Accuracy {
    if pairs.is_empty() {
        return Accuracy::default();
    }
    let n = pairs.len() as f64;
    let mae = pairs.iter().map(|p| (p.1 - p.2).abs()).sum::<f64>() / n;
    let rmse = (pairs.iter().map(|p| (p.1 - p.2).powi(2)).sum::<f64>() / n).sqrt();
    let bias = pairs.iter().map(|p| p.1 - p.2).sum::<f64>() / n;
    let big: Vec<_> = pairs.iter().filter(|p| p.2 >= 200.0).collect();
    let mape = (!big.is_empty()).then(|| {
        big.iter().map(|p| ((p.1 - p.2) / p.2).abs()).sum::<f64>() / big.len() as f64 * 100.0
    });
    let mut by_hour = std::collections::BTreeMap::<u32, (f64, usize)>::new();
    let mut by_month = std::collections::BTreeMap::<u32, (f64, usize)>::new();
    let mut by_sky = [(0.0, 0usize); 3];
    let mut by_day = std::collections::BTreeMap::<NaiveDate, (f64, f64)>::new();
    for p in pairs {
        let e = (p.1 - p.2).abs();
        let h = by_hour.entry(local_hour(p.0, tz)).or_default();
        h.0 += e;
        h.1 += 1;
        let m = by_month.entry(local_date(p.0, tz).month()).or_default();
        m.0 += e;
        m.1 += 1;
        by_sky[p.3.min(2)].0 += e;
        by_sky[p.3.min(2)].1 += 1;
        let d = by_day.entry(local_date(p.0, tz)).or_default();
        d.0 += p.1;
        d.1 += p.2;
    }
    let days: Vec<_> = by_day.values().filter(|d| d.1 >= 1000.0).collect();
    let daily_mape = (!days.is_empty()).then(|| {
        days.iter().map(|d| ((d.0 - d.1) / d.1).abs()).sum::<f64>() / days.len() as f64 * 100.0
    });
    Accuracy {
        n: pairs.len(),
        mae_wh: Some(mae),
        rmse_wh: Some(rmse),
        bias_wh: Some(bias),
        mape_pct: mape,
        daily_mape_pct: daily_mape,
        by_hour: by_hour
            .into_iter()
            .map(|(k, (s, c))| (k, s / c as f64, c))
            .collect(),
        by_month: by_month
            .into_iter()
            .map(|(k, (s, c))| (k, s / c as f64, c))
            .collect(),
        by_sky: by_sky
            .iter()
            .enumerate()
            .filter(|(_, x)| x.1 > 0)
            .map(|(i, x)| (SKY_BINS[i].to_string(), x.0 / x.1 as f64, x.1))
            .collect(),
    }
}

/// Convenience for API consumers: the UTC day boundaries of a local date.
pub fn day_bounds(d: NaiveDate, tz: chrono_tz::Tz) -> (i64, i64) {
    (day_start(d, tz), day_start(d.succ_opt().unwrap_or(d), tz))
}

pub fn utc(ts: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(ts, 0).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::EnergyRow;

    const LAT: f64 = 37.7749;
    const LON: f64 = -122.4194;
    fn tz() -> chrono_tz::Tz {
        chrono_tz::America::Los_Angeles
    }

    fn clear_day(day_start_utc: i64) -> Vec<WeatherHour> {
        // Synthetic clear sky from Haurwitz at each hour (test fixture only).
        (0..48)
            .map(|k| {
                let ts = day_start_utc + k * 3600;
                let mid = utc(ts - 1800);
                let pos = astro::solar_position(mid, LAT, LON);
                let ghi = pvmodel::clear_sky_ghi_haurwitz(pos.zenith_deg);
                let cz = pos.zenith_deg.to_radians().cos().max(0.05);
                let dhi = ghi * 0.12;
                let dni = if ghi > 0.0 { (ghi - dhi) / cz } else { 0.0 };
                WeatherHour {
                    ts,
                    ghi: Some(ghi),
                    dni: Some(dni.min(1000.0)),
                    dhi: Some(dhi),
                    temp_c: Some(20.0),
                    wind_ms: Some(2.0),
                    cloud: Some(0.0),
                    ..Default::default()
                }
            })
            .collect()
    }

    fn array() -> ArraySpec {
        ArraySpec {
            dc_capacity_w: 8000.0,
            tilt_deg: 20.0,
            azimuth_deg: 180.0,
            ..Default::default()
        }
    }

    #[test]
    fn physical_forecast_shape_on_clear_day() {
        let d = NaiveDate::from_ymd_opt(2026, 6, 21).unwrap();
        let start = day_start(d, tz());
        let corr = SolarCorrection::new(PHYSICAL_MODEL, 0);
        let f =
            solar_forecast(start, &clear_day(start), LAT, LON, tz(), &[array()], &corr).unwrap();
        let day = &f.days[0];
        // 8 kW DC, clear June day in SF: physically plausible 35–65 kWh.
        assert!(day.kwh > 35.0 && day.kwh < 65.0, "{}", day.kwh);
        // Peak around solar noon (≈13:10 PDT): between 12:00 and 14:30 local.
        let peak_h = local_hour(day.peak_ts.unwrap(), tz());
        assert!((12..=14).contains(&peak_h), "{peak_h}");
        // Night hours produce nothing.
        assert!(
            f.hours
                .iter()
                .filter(|h| h.elevation_deg < -5.0)
                .all(|h| h.wh == 0.0)
        );
        assert!(f.confidence.starts_with("low"));
    }

    #[test]
    fn correction_learns_systematic_bias() {
        let mut corr = SolarCorrection::new(PHYSICAL_MODEL, 0);
        let b = Baseline {
            value: 4000.0,
            kt: Some(0.9),
            elevation_deg: 50.0,
            cell: 2 * 3 + 2,
        };
        assert!((corr.factor(b.cell).unwrap() - 1.0).abs() < 1e-12);
        for _ in 0..500 {
            corr.learn(&b, 3200.0); // system really makes 80 %
        }
        let f = corr.factor(b.cell).unwrap();
        assert!((f - 0.8).abs() < 0.03, "{f}");
        // Other cells untouched (still the prior).
        assert!((corr.factor(0).unwrap() - 1.0).abs() < 1e-9);
        let (lo, hi, learned) = corr.interval(2);
        assert!(learned && lo <= 1.0 && hi >= 1.0);
    }

    #[test]
    fn empirical_model_needs_data_then_scales_ghi() {
        let mut corr = SolarCorrection::new(EMPIRICAL_MODEL, 0);
        assert!(corr.factor(8).is_none());
        let b = Baseline {
            value: 800.0,
            kt: Some(0.9),
            elevation_deg: 50.0,
            cell: 8,
        };
        for _ in 0..100 {
            corr.learn(&b, 6000.0); // 7.5 Wh per Wh/m²
        }
        assert!((corr.factor(8).unwrap() - 7.5).abs() < 0.2);
    }

    #[test]
    fn load_profile_and_overnight() {
        let d0 = NaiveDate::from_ymd_opt(2026, 3, 2).unwrap();
        let start = day_start(d0, tz());
        let mut hist = vec![];
        for k in 0..(24 * 21) {
            let ts = start + k * 3600;
            let hr = local_hour(ts, tz());
            let wh = if (17..21).contains(&hr) {
                2000.0
            } else {
                500.0
            };
            hist.push(HourRow {
                hour_ts: ts,
                e: EnergyRow {
                    load_wh: wh,
                    covered_s: 3600.0,
                    ..Default::default()
                },
                ..Default::default()
            });
        }
        let now = start + 24 * 21 * 3600 + 600;
        let f = load_forecast(
            now,
            &hist,
            &Default::default(),
            tz(),
            48,
            Some(now + 14 * 3600),
            Some(now),
        )
        .unwrap();
        let evening = f
            .hours
            .iter()
            .find(|h| local_hour(h.ts, tz()) == 18)
            .unwrap();
        assert!((evening.wh - 2000.0).abs() < 1e-6);
        assert!((f.tomorrow_wh.unwrap() - (20.0 * 500.0 + 4.0 * 2000.0)).abs() < 1e-6);
        assert!(f.overnight_wh.unwrap() > 0.0);
        assert!(
            load_forecast(now, &hist[..10], &Default::default(), tz(), 48, None, None).is_none()
        );
    }

    #[test]
    fn accuracy_metrics() {
        let pairs = vec![
            (0, 1100.0, 1000.0, 2),
            (3600, 900.0, 1000.0, 2),
            (7200, 0.0, 0.0, 0),
        ];
        let a = accuracy(&pairs, tz());
        assert_eq!(a.n, 3);
        assert!((a.mae_wh.unwrap() - 200.0 / 3.0).abs() < 1e-9);
        assert!((a.bias_wh.unwrap()).abs() < 1e-9);
        assert!((a.mape_pct.unwrap() - 10.0).abs() < 1e-9);
    }

    #[test]
    fn local_day_boundaries_dst() {
        let d = NaiveDate::from_ymd_opt(2026, 3, 8).unwrap(); // spring forward
        let (a, b) = day_bounds(d, tz());
        assert_eq!(b - a, 23 * 3600);
        let d = NaiveDate::from_ymd_opt(2026, 11, 1).unwrap(); // fall back
        let (a, b) = day_bounds(d, tz());
        assert_eq!(b - a, 25 * 3600);
    }
}
