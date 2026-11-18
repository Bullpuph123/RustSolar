//! Configurable electricity tariff engine.
//!
//! **Everything this module produces is an ESTIMATE.** Real utility bills
//! include rounding, proration, taxes, surcharges, true-up rules and
//! billing-period boundaries that this engine does not model. No real
//! utility prices are hard-coded anywhere: the user types every rate from
//! their own bill / tariff sheet into the `[tariff]` config section.
//!
//! Features:
//! * time-of-use periods (peak / partial-peak / off-peak, any names) per
//!   season, with weekday / weekend / all-day schedules (holidays count as
//!   weekend);
//! * several plan versions selected by `effective_from` date;
//! * fixed daily charges;
//! * optional PG&E-style baseline credit, optional generic monthly tiers;
//! * optional demand charge ($/kW of the monthly max hourly-average import);
//! * explicit export compensation rules ([`ExportRule`]). Exports earn
//!   nothing unless a rule says otherwise, and they earn the retail import
//!   price only under [`ExportRule::NetMetering`].
//!
//! All time-of-day lookups use the local wall-clock hour (in
//! [`TariffConfig::time_zone`]) of each UTC hour start, so a local day has
//! 23 or 25 hours on DST transition days.
//!
//! Dates in TOML may be bare (`effective_from = 2025-01-01`) or quoted
//! strings (`"2025-01-01"`).

use std::collections::{BTreeMap, BTreeSet, HashSet};

use chrono::{DateTime, Datelike, Duration, NaiveDate, TimeZone, Timelike, Utc, Weekday};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

/// Accepts dates as `"YYYY-MM-DD"` strings or as TOML native dates (which
/// the `toml` crate hands to serde as a single-entry map wrapping a string).
/// Accept `null` (JSON) as "no export credit" as well as a tagged rule.
fn export_or_none<'de, D: serde::Deserializer<'de>>(d: D) -> Result<ExportRule, D::Error> {
    use serde::Deserialize;
    Ok(Option::<ExportRule>::deserialize(d)?.unwrap_or_default())
}

fn yes() -> bool {
    true
}

mod date_compat {
    use chrono::NaiveDate;
    use serde::de::{self, Deserializer, MapAccess, SeqAccess, Visitor};
    use std::fmt;

    struct DateVisitor;
    impl<'de> Visitor<'de> for DateVisitor {
        type Value = NaiveDate;
        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
            f.write_str("a date like 2025-01-01")
        }
        fn visit_str<E: de::Error>(self, v: &str) -> Result<NaiveDate, E> {
            NaiveDate::parse_from_str(v.trim(), "%Y-%m-%d")
                .map_err(|e| E::custom(format!("invalid date '{v}' (expected YYYY-MM-DD): {e}")))
        }
        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<NaiveDate, A::Error> {
            let Some((_, v)) = map.next_entry::<String, String>()? else {
                return Err(de::Error::custom("expected a date"));
            };
            self.visit_str(&v)
        }
    }

    struct Wrap(NaiveDate);
    impl<'de> serde::Deserialize<'de> for Wrap {
        fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
            d.deserialize_any(DateVisitor).map(Wrap)
        }
    }

    pub fn one<'de, D: Deserializer<'de>>(d: D) -> Result<NaiveDate, D::Error> {
        d.deserialize_any(DateVisitor)
    }

    pub fn opt<'de, D: Deserializer<'de>>(d: D) -> Result<Option<NaiveDate>, D::Error> {
        Ok(<Option<Wrap> as serde::Deserialize>::deserialize(d)?.map(|w| w.0))
    }

    pub fn vec<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<NaiveDate>, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Vec<NaiveDate>;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a list of dates")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut out = Vec::new();
                while let Some(Wrap(x)) = seq.next_element()? {
                    out.push(x);
                }
                Ok(out)
            }
        }
        d.deserialize_seq(V)
    }
}

/// Sanity bound for any $/kWh value accepted by [`TariffConfig::validate`].
pub const MAX_ABS_PER_KWH: f64 = 10.0;

/// Top-level tariff configuration (the `[tariff]` section).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct TariffConfig {
    /// When false the app should not show cost estimates.
    pub enabled: bool,
    /// Display currency code, e.g. "USD". Purely informational.
    pub currency: String,
    /// IANA time zone used for every time-of-use lookup.
    pub time_zone: String,
    /// Local dates treated as weekend days for schedule purposes.
    #[serde(deserialize_with = "date_compat::vec")]
    pub holidays: Vec<NaiveDate>,
    /// Plan versions; the one with the latest `effective_from <= date` applies.
    pub plans: Vec<RatePlan>,
    /// Also treat PG&E's eight time-of-use holidays (on their observed dates) as weekend
    /// days, every year, without listing them in `holidays`.
    pub pge_holidays: bool,
}

impl Default for TariffConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            currency: "USD".to_string(),
            time_zone: "America/Los_Angeles".to_string(),
            holidays: Vec::new(),
            plans: Vec::new(),
            pge_holidays: false,
        }
    }
}

/// One version of a rate plan.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RatePlan {
    #[serde(default)]
    pub name: String,
    /// First local date on which this version applies.
    #[serde(deserialize_with = "date_compat::one")]
    pub effective_from: NaiveDate,
    /// Fixed charge per local day that has any data ($/day).
    #[serde(default)]
    pub fixed_daily: f64,
    /// Seasons; every month 1..=12 must be in exactly one season.
    #[serde(default)]
    pub seasons: Vec<Season>,
    /// How exported energy is compensated. Defaults to [`ExportRule::None`]; `null`
    /// (from the JSON editor) also means `None`.
    #[serde(default, deserialize_with = "export_or_none")]
    pub export: ExportRule,
    /// Optional PG&E-style baseline allowance credit.
    #[serde(default)]
    pub baseline: Option<Baseline>,
    /// Optional tiers by cumulative monthly import (adders on top of TOU).
    #[serde(default)]
    pub tiers: Vec<Tier>,
    /// Optional demand charge in $/kW of the monthly max hourly import.
    #[serde(default)]
    pub demand_per_kw: Option<f64>,
}

/// A season: a set of months sharing one TOU schedule.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Season {
    #[serde(default)]
    pub name: String,
    /// Months 1..=12.
    pub months: Vec<u32>,
    pub periods: Vec<Period>,
}

/// Which days a [`Period`] applies to. Holidays count as weekend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DayType {
    Weekday,
    Weekend,
    #[default]
    All,
}

/// A time-of-use period: local hours `[start_hour, end_hour)`.
/// `end_hour` may be 24; `start_hour > end_hour` wraps past midnight
/// (e.g. 21..7).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Period {
    /// E.g. "peak", "partial-peak", "off-peak".
    pub name: String,
    #[serde(default)]
    pub days: DayType,
    pub start_hour: u32,
    pub end_hour: u32,
    /// Import energy price in $/kWh.
    pub import_per_kwh: f64,
}

/// PG&E-style baseline allowance: within each calendar month the first
/// `daily_kwh × days-with-data` kWh of grid import earn `credit_per_kwh`.
///
/// Simplification (estimate): the allowance is per calendar month (not per
/// billing cycle), counts only days present in the input, and is consumed by
/// hourly gross grid import in chronological order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Baseline {
    /// Daily allowance in kWh for January..December (12 values).
    pub daily_kwh_by_month: Vec<f64>,
    /// Credit in $/kWh (positive number; it is subtracted).
    pub credit_per_kwh: f64,
}

/// Generic tier by cumulative monthly import. Tiers must be listed in
/// ascending order; the last one must have `up_to_kwh_per_month` unset.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tier {
    #[serde(default)]
    pub up_to_kwh_per_month: Option<f64>,
    /// Added to the TOU import price for kWh falling in this tier ($/kWh).
    pub adder_per_kwh: f64,
}

/// Export compensation rule. Written in TOML as e.g.
/// `export = { type = "net_metering", non_bypassable_per_kwh = 0.03 }`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ExportRule {
    /// NEM: export credited at the same TOU import energy price, minus
    /// non-bypassable charges (floored at 0). Tier adders and baseline
    /// credits are not applied to exports.
    NetMetering {
        #[serde(default)]
        non_bypassable_per_kwh: f64,
    },
    /// Flat $/kWh for every exported kWh.
    FixedRate { per_kwh: f64 },
    /// Net-billing style: `by_month_hour[month-1][local_hour]` $/kWh
    /// (12 × 24). Optional separate matrix for weekends/holidays.
    Schedule {
        by_month_hour: Vec<Vec<f64>>,
        #[serde(default)]
        weekend_by_month_hour: Option<Vec<Vec<f64>>>,
    },
    /// PG&E Net Billing Tariff (NEM 3): hourly export credits from PG&E's published tables
    /// (built in, see [`crate::pge_nbt`]) for the customer's vintage, plus optional ACC Plus.
    PgeNbt {
        /// Calendar year of the completed interconnection application (2023–2026).
        vintage: i32,
        /// Include the generation part of the credit. True while PG&E supplies your
        /// generation; false if a community choice aggregator (CCA) does.
        #[serde(default = "yes")]
        generation: bool,
        /// Add the residential ACC Plus adder for the vintage (most residential NBT systems
        /// qualify; not homes bought with an existing system or customers moved from NEM).
        #[serde(default)]
        acc_plus: bool,
        /// Permission-to-operate date; ACC Plus stops 9 years after it. Unset = always.
        #[serde(
            default,
            deserialize_with = "date_compat::opt",
            skip_serializing_if = "Option::is_none"
        )]
        pto_date: Option<NaiveDate>,
    },
    /// Exports earn nothing.
    #[default]
    None,
}

/// Result of a single rate lookup.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RateInfo {
    pub plan: String,
    pub season: String,
    pub period: String,
    /// TOU import energy price ($/kWh), excluding tier/baseline adjustments.
    pub import_per_kwh: f64,
    /// Export compensation ($/kWh) per the plan's [`ExportRule`].
    pub export_per_kwh: f64,
    pub is_holiday_or_weekend: bool,
}

/// Measured energy for one UTC hour.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct HourEnergy {
    pub hour_start: DateTime<Utc>,
    pub import_kwh: f64,
    pub export_kwh: f64,
    pub load_kwh: f64,
    pub pv_kwh: f64,
}

/// Estimated cost of one hour.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HourLine {
    pub hour_start: DateTime<Utc>,
    pub local_date: NaiveDate,
    pub local_hour: u32,
    pub plan: String,
    pub season: String,
    pub period: String,
    pub import_kwh: f64,
    pub export_kwh: f64,
    pub load_kwh: f64,
    pub pv_kwh: f64,
    pub import_per_kwh: f64,
    pub export_per_kwh: f64,
    /// `import_kwh × import_per_kwh` (TOU energy only).
    pub import_cost: f64,
    /// Extra cost from tier adders.
    pub tier_adder_cost: f64,
    /// Baseline credit (positive number, subtracted).
    pub baseline_credit: f64,
    /// `export_kwh × export_per_kwh`.
    pub export_credit: f64,
    /// `import_cost + tier_adder_cost − baseline_credit − export_credit`.
    pub net_cost: f64,
    /// `(load − import) × import_per_kwh`: value of self-consumed solar and
    /// battery energy. Can be negative in hours where the battery charges
    /// from the grid.
    pub avoided_cost: f64,
    /// `load × import_per_kwh`: what the load would have cost from the grid.
    pub no_solar_cost: f64,
}

/// Estimated totals for one local day.
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct DayTotals {
    pub date: NaiveDate,
    /// Number of hour lines (23/25 on full DST days).
    pub hours: u32,
    pub import_kwh: f64,
    pub import_cost: f64,
    pub tier_adder_cost: f64,
    pub baseline_credit: f64,
    pub export_kwh: f64,
    pub export_credit: f64,
    pub fixed_charge: f64,
    pub load_kwh: f64,
    pub pv_kwh: f64,
    pub avoided_cost: f64,
    pub no_solar_cost: f64,
    /// `import_cost + tier_adder_cost − baseline_credit − export_credit + fixed_charge`.
    pub net_cost: f64,
}

/// Estimated monthly items (calendar months in local time).
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct MonthTotals {
    pub year: i32,
    pub month: u32,
    pub days_with_data: u32,
    pub import_kwh: f64,
    pub baseline_allowance_kwh: f64,
    /// kWh of import that received the baseline credit.
    pub baseline_credited_kwh: f64,
    pub baseline_credit: f64,
    pub tier_adder_cost: f64,
    /// Max hourly import kWh (= average kW over that hour).
    pub peak_demand_kw: f64,
    pub demand_charge: f64,
}

/// Estimated grand totals.
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct CostTotals {
    pub import_kwh: f64,
    pub import_cost: f64,
    pub tier_adder_cost: f64,
    pub baseline_credit: f64,
    pub export_kwh: f64,
    pub export_credit: f64,
    pub fixed_charge: f64,
    pub demand_charge: f64,
    pub load_kwh: f64,
    pub pv_kwh: f64,
    pub avoided_cost: f64,
    pub no_solar_cost: f64,
    /// Sum of day net costs plus demand charges.
    pub net_cost: f64,
}

/// Output of [`TariffConfig::cost_hours`]. All money values are estimates.
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct CostReport {
    pub currency: String,
    pub lines: Vec<HourLine>,
    pub days: Vec<DayTotals>,
    pub months: Vec<MonthTotals>,
    pub totals: CostTotals,
    pub notes: Vec<String>,
}

const DAY_KINDS: [(DayType, &str); 2] =
    [(DayType::Weekday, "weekday"), (DayType::Weekend, "weekend")];

fn hour_in(start: u32, end: u32, h: u32) -> bool {
    if start < end {
        h >= start && h < end
    } else if start > end {
        h >= start || h < end
    } else {
        false
    }
}

fn day_matches(days: DayType, weekend: bool) -> bool {
    match days {
        DayType::All => true,
        DayType::Weekday => !weekend,
        DayType::Weekend => weekend,
    }
}

/// Format sorted hours as runs, e.g. "0-3, 7".
fn hour_runs(hours: &[u32]) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < hours.len() {
        let mut j = i;
        while j + 1 < hours.len() && hours[j + 1] == hours[j] + 1 {
            j += 1;
        }
        if i == j {
            out.push(format!("{}", hours[i]));
        } else {
            out.push(format!("{}-{}", hours[i], hours[j]));
        }
        i = j + 1;
    }
    out.join(", ")
}

fn check_price(errs: &mut Vec<String>, what: &str, v: f64) {
    if !v.is_finite() || !(-MAX_ABS_PER_KWH..=MAX_ABS_PER_KWH).contains(&v) {
        errs.push(format!(
            "{what}: price {v} $/kWh is not a finite number between -{MAX_ABS_PER_KWH} and {MAX_ABS_PER_KWH}"
        ));
    }
}

fn check_matrix(errs: &mut Vec<String>, what: &str, m: &[Vec<f64>]) {
    if m.len() != 12 || m.iter().any(|row| row.len() != 24) {
        errs.push(format!(
            "{what}: export schedule must be 12 rows (months) × 24 values (hours); got {} rows with lengths {:?}",
            m.len(),
            m.iter().map(Vec::len).collect::<Vec<_>>()
        ));
        return;
    }
    for (mi, row) in m.iter().enumerate() {
        for (h, v) in row.iter().enumerate() {
            check_price(errs, &format!("{what} month {} hour {h}", mi + 1), *v);
        }
    }
}

/// UTC hour starts whose local date (in `tz`) is `date`: 23/24/25 entries.
fn local_day_utc_hours(tz: Tz, date: NaiveDate) -> Vec<DateTime<Utc>> {
    let mut start = None;
    for h in 0..24 {
        if let Some(ndt) = date.and_hms_opt(h, 0, 0)
            && let Some(dt) = tz.from_local_datetime(&ndt).earliest()
        {
            start = Some(dt.with_timezone(&Utc));
            break;
        }
    }
    let Some(mut t) = start else {
        return Vec::new();
    };
    let mut out = Vec::new();
    while t.with_timezone(&tz).date_naive() == date && out.len() < 30 {
        out.push(t);
        t += Duration::hours(1);
    }
    out
}

impl TariffConfig {
    fn tz(&self) -> Option<Tz> {
        self.time_zone.parse::<Tz>().ok()
    }

    /// Check the configuration; returns every problem found.
    pub fn validate(&self) -> Result<(), Vec<String>> {
        let mut errs = Vec::new();
        if self.tz().is_none() {
            errs.push(format!(
                "time_zone '{}' is not a valid IANA time zone (e.g. America/Los_Angeles)",
                self.time_zone
            ));
        }
        if self.enabled && self.plans.is_empty() {
            errs.push("tariff is enabled but no [[tariff.plans]] are defined".to_string());
        }
        let mut seen_dates = HashSet::new();
        for (pi, plan) in self.plans.iter().enumerate() {
            let pl = if plan.name.is_empty() {
                format!("plan #{}", pi + 1)
            } else {
                format!("plan '{}'", plan.name)
            };
            if !seen_dates.insert(plan.effective_from) {
                errs.push(format!(
                    "{pl}: another plan has the same effective_from {}",
                    plan.effective_from
                ));
            }
            if !plan.fixed_daily.is_finite() || !(0.0..=1000.0).contains(&plan.fixed_daily) {
                errs.push(format!(
                    "{pl}: fixed_daily {} must be between 0 and 1000 $/day",
                    plan.fixed_daily
                ));
            }
            if let Some(d) = plan.demand_per_kw
                && (!d.is_finite() || !(0.0..=1000.0).contains(&d))
            {
                errs.push(format!(
                    "{pl}: demand_per_kw {d} must be between 0 and 1000 $/kW"
                ));
            }
            // Season month coverage.
            if plan.seasons.is_empty() {
                errs.push(format!("{pl}: no seasons defined"));
            }
            let mut month_owner: [Vec<String>; 12] = Default::default();
            for (si, season) in plan.seasons.iter().enumerate() {
                let sl = if season.name.is_empty() {
                    format!("{pl} season #{}", si + 1)
                } else {
                    format!("{pl} season '{}'", season.name)
                };
                for &m in &season.months {
                    if (1..=12).contains(&m) {
                        month_owner[(m - 1) as usize].push(sl.clone());
                    } else {
                        errs.push(format!("{sl}: month {m} is not in 1..12"));
                    }
                }
                // Period sanity.
                for p in &season.periods {
                    if p.start_hour > 23 || p.end_hour > 24 {
                        errs.push(format!(
                            "{sl} period '{}': start_hour must be 0..23 and end_hour 0..24 (got {}..{})",
                            p.name, p.start_hour, p.end_hour
                        ));
                    } else if p.start_hour == p.end_hour {
                        errs.push(format!(
                            "{sl} period '{}': start_hour equals end_hour ({}), which covers no hours; use 0..24 for all day",
                            p.name, p.start_hour
                        ));
                    }
                    check_price(
                        &mut errs,
                        &format!("{sl} period '{}'", p.name),
                        p.import_per_kwh,
                    );
                }
                // Hour coverage per day kind.
                for (kind, kind_name) in DAY_KINDS {
                    let weekend = kind == DayType::Weekend;
                    let mut gaps = Vec::new();
                    let mut overlaps: BTreeMap<String, Vec<u32>> = BTreeMap::new();
                    for h in 0..24 {
                        let names: Vec<&str> = season
                            .periods
                            .iter()
                            .filter(|p| {
                                day_matches(p.days, weekend) && hour_in(p.start_hour, p.end_hour, h)
                            })
                            .map(|p| p.name.as_str())
                            .collect();
                        match names.len() {
                            0 => gaps.push(h),
                            1 => {}
                            _ => overlaps.entry(names.join("', '")).or_default().push(h),
                        }
                    }
                    if !gaps.is_empty() {
                        errs.push(format!(
                            "{sl}: on {kind_name}s hour(s) {} are not covered by any period",
                            hour_runs(&gaps)
                        ));
                    }
                    for (names, hours) in overlaps {
                        errs.push(format!(
                            "{sl}: on {kind_name}s hour(s) {} overlap between periods '{names}'",
                            hour_runs(&hours)
                        ));
                    }
                }
            }
            for (mi, owners) in month_owner.iter().enumerate() {
                match owners.len() {
                    0 if !plan.seasons.is_empty() => {
                        errs.push(format!("{pl}: month {} is not in any season", mi + 1))
                    }
                    0 | 1 => {}
                    _ => errs.push(format!(
                        "{pl}: month {} is in more than one season ({})",
                        mi + 1,
                        owners.join(", ")
                    )),
                }
            }
            // Export rule.
            match &plan.export {
                ExportRule::NetMetering {
                    non_bypassable_per_kwh,
                } => check_price(
                    &mut errs,
                    &format!("{pl} export non_bypassable_per_kwh"),
                    *non_bypassable_per_kwh,
                ),
                ExportRule::FixedRate { per_kwh } => {
                    check_price(&mut errs, &format!("{pl} export per_kwh"), *per_kwh)
                }
                ExportRule::Schedule {
                    by_month_hour,
                    weekend_by_month_hour,
                } => {
                    check_matrix(
                        &mut errs,
                        &format!("{pl} export by_month_hour"),
                        by_month_hour,
                    );
                    if let Some(w) = weekend_by_month_hour {
                        check_matrix(&mut errs, &format!("{pl} export weekend_by_month_hour"), w);
                    }
                }
                ExportRule::PgeNbt {
                    vintage, acc_plus, ..
                } => {
                    if crate::pge_nbt::credit(*vintage, plan.effective_from, 12, false).is_none() {
                        errs.push(format!(
                        "{pl}: no built-in PG&E Net Billing export values for vintage {vintage} (available: {:?})",
                        crate::pge_nbt::vintages()
                    ));
                    }
                    if *acc_plus && crate::pge_nbt::acc_plus_residential(*vintage).is_none() {
                        errs.push(format!("{pl}: no ACC Plus amount for vintage {vintage}"));
                    }
                }
                ExportRule::None => {}
            }
            // Baseline.
            if let Some(b) = &plan.baseline {
                if b.daily_kwh_by_month.len() != 12 {
                    errs.push(format!(
                        "{pl}: baseline daily_kwh_by_month must have 12 values (Jan..Dec), got {}",
                        b.daily_kwh_by_month.len()
                    ));
                }
                if b.daily_kwh_by_month
                    .iter()
                    .any(|v| !v.is_finite() || *v < 0.0 || *v > 1000.0)
                {
                    errs.push(format!(
                        "{pl}: baseline daily_kwh_by_month values must be between 0 and 1000"
                    ));
                }
                check_price(
                    &mut errs,
                    &format!("{pl} baseline credit_per_kwh"),
                    b.credit_per_kwh,
                );
            }
            // Tiers.
            let mut prev = 0.0;
            for (ti, t) in plan.tiers.iter().enumerate() {
                check_price(
                    &mut errs,
                    &format!("{pl} tier #{}", ti + 1),
                    t.adder_per_kwh,
                );
                let last = ti + 1 == plan.tiers.len();
                match t.up_to_kwh_per_month {
                    Some(u) if !u.is_finite() || u <= prev => errs.push(format!(
                        "{pl} tier #{}: up_to_kwh_per_month {u} must be greater than the previous tier's limit {prev}",
                        ti + 1
                    )),
                    Some(u) => {
                        prev = u;
                        if last {
                            errs.push(format!(
                                "{pl}: the last tier must omit up_to_kwh_per_month (unbounded)"
                            ));
                        }
                    }
                    None if !last => errs.push(format!(
                        "{pl} tier #{}: only the last tier may omit up_to_kwh_per_month",
                        ti + 1
                    )),
                    None => {}
                }
            }
        }
        if errs.is_empty() { Ok(()) } else { Err(errs) }
    }

    /// The plan version in force on `local_date` (latest `effective_from <= date`).
    pub fn plan_for(&self, local_date: NaiveDate) -> Option<&RatePlan> {
        self.plans
            .iter()
            .filter(|p| p.effective_from <= local_date)
            .max_by_key(|p| p.effective_from)
    }

    fn is_weekend_or_holiday(&self, d: NaiveDate) -> bool {
        matches!(d.weekday(), Weekday::Sat | Weekday::Sun)
            || self.holidays.contains(&d)
            || (self.pge_holidays && crate::pge_nbt::is_pge_holiday(d))
    }

    /// Rate lookup for a local date and wall-clock hour.
    fn lookup(&self, date: NaiveDate, hour: u32) -> Option<RateInfo> {
        let plan = self.plan_for(date)?;
        let month = date.month();
        let season = plan.seasons.iter().find(|s| s.months.contains(&month))?;
        let weekend = self.is_weekend_or_holiday(date);
        let period = season
            .periods
            .iter()
            .find(|p| day_matches(p.days, weekend) && hour_in(p.start_hour, p.end_hour, hour))?;
        let import = period.import_per_kwh;
        let export = match &plan.export {
            ExportRule::NetMetering {
                non_bypassable_per_kwh,
            } => (import - non_bypassable_per_kwh).max(0.0),
            ExportRule::FixedRate { per_kwh } => *per_kwh,
            ExportRule::Schedule {
                by_month_hour,
                weekend_by_month_hour,
            } => {
                let m = match weekend_by_month_hour {
                    Some(w) if weekend => w,
                    _ => by_month_hour,
                };
                m.get((month - 1) as usize)
                    .and_then(|row| row.get(hour as usize))
                    .copied()
                    .unwrap_or(0.0)
            }
            ExportRule::PgeNbt {
                vintage,
                generation,
                acc_plus,
                pto_date,
            } => {
                let c = crate::pge_nbt::credit(*vintage, date, hour, weekend);
                let base = c.map_or(0.0, |c| {
                    c.delivery + if *generation { c.generation } else { 0.0 }
                });
                let plus_active = *acc_plus
                    && pto_date.is_none_or(|p| {
                        date < p
                            .with_year(p.year() + 9)
                            .unwrap_or(p + Duration::days(9 * 365))
                    });
                let plus = if plus_active {
                    crate::pge_nbt::acc_plus_residential(*vintage).unwrap_or(0.0)
                } else {
                    0.0
                };
                base + plus
            }
            ExportRule::None => 0.0,
        };
        Some(RateInfo {
            plan: plan.name.clone(),
            season: season.name.clone(),
            period: period.name.clone(),
            import_per_kwh: import,
            export_per_kwh: export,
            is_holiday_or_weekend: weekend,
        })
    }

    /// Rate in force at instant `t` (local wall-clock hour of `t`).
    /// `None` if the time zone is invalid or no plan/season/period matches.
    pub fn rate_at(&self, t: DateTime<Utc>) -> Option<RateInfo> {
        let local = t.with_timezone(&self.tz()?);
        self.lookup(local.date_naive(), local.hour())
    }

    /// Hourly schedule for a local day, for charting:
    /// `(local_hour, period, import $/kWh, export $/kWh)`. Has 23 or 25
    /// entries on DST transition days (the repeated hour appears twice).
    pub fn rate_schedule_for_day(&self, local_date: NaiveDate) -> Vec<(u32, String, f64, f64)> {
        let Some(tz) = self.tz() else {
            return Vec::new();
        };
        local_day_utc_hours(tz, local_date)
            .into_iter()
            .filter_map(|t| {
                let h = t.with_timezone(&tz).hour();
                self.lookup(local_date, h)
                    .map(|r| (h, r.period, r.import_per_kwh, r.export_per_kwh))
            })
            .collect()
    }

    /// Estimate costs for measured hourly energy (UTC hour starts, may be
    /// gappy/unsorted). See the module docs: all results are ESTIMATES.
    ///
    /// * Fixed daily charge: once per local day with at least one priced hour.
    /// * Baseline credit: per local calendar month, allowance =
    ///   Σ over days-with-data of that day's plan `daily_kwh_by_month`;
    ///   consumed by hourly grid import in time order.
    /// * Tiers: adders by cumulative monthly grid import.
    /// * Demand: per month, max hourly import kWh (= avg kW) × `demand_per_kw`
    ///   of the plan in force at that hour.
    pub fn cost_hours(&self, hours: &[HourEnergy]) -> CostReport {
        let mut report = CostReport {
            currency: self.currency.clone(),
            ..Default::default()
        };
        report
            .notes
            .push("All costs are estimates; taxes, surcharges, true-up and billing-cycle rules are not modeled.".into());
        if !self.enabled {
            report
                .notes
                .push("Tariff is disabled in config; figures computed anyway.".into());
        }
        let Some(tz) = self.tz() else {
            report.notes.push(format!(
                "Invalid time_zone '{}'; nothing priced.",
                self.time_zone
            ));
            return report;
        };

        let mut sorted: Vec<&HourEnergy> = hours.iter().collect();
        sorted.sort_by_key(|h| h.hour_start);
        let mut dupes = 0usize;
        let mut unmatched = 0usize;
        let mut last: Option<DateTime<Utc>> = None;
        // (line, plan) pairs.
        let mut lines: Vec<(HourLine, &RatePlan)> = Vec::new();
        for h in sorted {
            if last == Some(h.hour_start) {
                dupes += 1;
                continue;
            }
            last = Some(h.hour_start);
            let local = h.hour_start.with_timezone(&tz);
            let (date, hour) = (local.date_naive(), local.hour());
            let (Some(rate), Some(plan)) = (self.lookup(date, hour), self.plan_for(date)) else {
                unmatched += 1;
                continue;
            };
            let ip = rate.import_per_kwh;
            let import_cost = h.import_kwh * ip;
            let export_credit = h.export_kwh * rate.export_per_kwh;
            lines.push((
                HourLine {
                    hour_start: h.hour_start,
                    local_date: date,
                    local_hour: hour,
                    plan: rate.plan,
                    season: rate.season,
                    period: rate.period,
                    import_kwh: h.import_kwh,
                    export_kwh: h.export_kwh,
                    load_kwh: h.load_kwh,
                    pv_kwh: h.pv_kwh,
                    import_per_kwh: ip,
                    export_per_kwh: rate.export_per_kwh,
                    import_cost,
                    tier_adder_cost: 0.0,
                    baseline_credit: 0.0,
                    export_credit,
                    net_cost: 0.0,
                    avoided_cost: (h.load_kwh - h.import_kwh) * ip,
                    no_solar_cost: h.load_kwh * ip,
                },
                plan,
            ));
        }
        if dupes > 0 {
            report
                .notes
                .push(format!("{dupes} duplicate hour(s) ignored."));
        }
        if unmatched > 0 {
            report.notes.push(format!(
                "{unmatched} hour(s) had no matching plan/season/period and were not priced."
            ));
        }

        // Months and days with data.
        let mut months: BTreeMap<(i32, u32), MonthTotals> = BTreeMap::new();
        let mut month_days: BTreeMap<(i32, u32), BTreeSet<NaiveDate>> = BTreeMap::new();
        for (l, _) in &lines {
            let key = (l.local_date.year(), l.local_date.month());
            month_days.entry(key).or_default().insert(l.local_date);
        }
        for (key, days) in &month_days {
            let allowance: f64 = days
                .iter()
                .filter_map(|d| self.plan_for(*d)?.baseline.as_ref())
                .filter_map(|b| b.daily_kwh_by_month.get((key.1 - 1) as usize))
                .sum();
            months.insert(
                *key,
                MonthTotals {
                    year: key.0,
                    month: key.1,
                    days_with_data: days.len() as u32,
                    baseline_allowance_kwh: allowance,
                    ..Default::default()
                },
            );
        }

        // Baseline, tiers, demand (chronological per month).
        let mut cum: BTreeMap<(i32, u32), (f64, f64)> = BTreeMap::new(); // (cum import, baseline used)
        for (l, plan) in &mut lines {
            let key = (l.local_date.year(), l.local_date.month());
            let mt = months.get_mut(&key).expect("month present");
            let (c, used) = cum.entry(key).or_insert((0.0, 0.0));
            let imp = l.import_kwh.max(0.0);
            if let Some(b) = &plan.baseline {
                let credited = imp.min((mt.baseline_allowance_kwh - *used).max(0.0));
                *used += credited;
                mt.baseline_credited_kwh += credited;
                l.baseline_credit = credited * b.credit_per_kwh;
            }
            if !plan.tiers.is_empty() {
                let (a, bnd) = (*c, *c + imp);
                let mut lower = 0.0;
                for t in &plan.tiers {
                    let upper = t.up_to_kwh_per_month.unwrap_or(f64::INFINITY);
                    let overlap = (bnd.min(upper) - a.max(lower)).max(0.0);
                    l.tier_adder_cost += overlap * t.adder_per_kwh;
                    lower = upper;
                }
            }
            *c += imp;
            l.net_cost = l.import_cost + l.tier_adder_cost - l.baseline_credit - l.export_credit;
            mt.import_kwh += l.import_kwh;
            mt.baseline_credit += l.baseline_credit;
            mt.tier_adder_cost += l.tier_adder_cost;
            if let Some(rate) = plan.demand_per_kw
                && imp > mt.peak_demand_kw
            {
                mt.peak_demand_kw = imp;
                mt.demand_charge = imp * rate;
            }
        }

        // Days.
        let mut days: BTreeMap<NaiveDate, DayTotals> = BTreeMap::new();
        for (l, plan) in &lines {
            let d = days.entry(l.local_date).or_insert_with(|| DayTotals {
                date: l.local_date,
                fixed_charge: plan_fixed(self, l.local_date, plan),
                ..Default::default()
            });
            d.hours += 1;
            d.import_kwh += l.import_kwh;
            d.import_cost += l.import_cost;
            d.tier_adder_cost += l.tier_adder_cost;
            d.baseline_credit += l.baseline_credit;
            d.export_kwh += l.export_kwh;
            d.export_credit += l.export_credit;
            d.load_kwh += l.load_kwh;
            d.pv_kwh += l.pv_kwh;
            d.avoided_cost += l.avoided_cost;
            d.no_solar_cost += l.no_solar_cost;
        }
        let t = &mut report.totals;
        for d in days.values_mut() {
            d.net_cost = d.import_cost + d.tier_adder_cost - d.baseline_credit - d.export_credit
                + d.fixed_charge;
            t.import_kwh += d.import_kwh;
            t.import_cost += d.import_cost;
            t.tier_adder_cost += d.tier_adder_cost;
            t.baseline_credit += d.baseline_credit;
            t.export_kwh += d.export_kwh;
            t.export_credit += d.export_credit;
            t.fixed_charge += d.fixed_charge;
            t.load_kwh += d.load_kwh;
            t.pv_kwh += d.pv_kwh;
            t.avoided_cost += d.avoided_cost;
            t.no_solar_cost += d.no_solar_cost;
            t.net_cost += d.net_cost;
        }
        for m in months.values() {
            t.demand_charge += m.demand_charge;
            t.net_cost += m.demand_charge;
            let label = format!("{}-{:02}", m.year, m.month);
            if m.baseline_credit > 0.0 {
                report.notes.push(format!(
                    "{label}: baseline credit applied ({:.3} of {:.3} kWh allowance; simplified monthly allowance from days with data).",
                    m.baseline_credited_kwh,
                    m.baseline_allowance_kwh
                ));
            }
            if m.tier_adder_cost != 0.0 {
                report.notes.push(format!(
                    "{label}: tier adders applied by cumulative monthly import."
                ));
            }
            if m.demand_charge > 0.0 {
                report.notes.push(format!(
                    "{label}: demand charge estimated from max hourly import {:.3} kW.",
                    m.peak_demand_kw
                ));
            }
        }
        report.lines = lines.into_iter().map(|(l, _)| l).collect();
        report.days = days.into_values().collect();
        report.months = months.into_values().collect();
        report
    }
}

fn plan_fixed(cfg: &TariffConfig, date: NaiveDate, fallback: &RatePlan) -> f64 {
    cfg.plan_for(date).unwrap_or(fallback).fixed_daily
}

#[cfg(test)]
mod tests {
    use super::*;

    const LA: Tz = chrono_tz::America::Los_Angeles;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }
    fn la(y: i32, m: u32, day: u32, h: u32, min: u32) -> DateTime<Utc> {
        LA.with_ymd_and_hms(y, m, day, h, min, 0)
            .earliest()
            .unwrap()
            .with_timezone(&Utc)
    }
    fn p(name: &str, days: DayType, s: u32, e: u32, price: f64) -> Period {
        Period {
            name: name.into(),
            days,
            start_hour: s,
            end_hour: e,
            import_per_kwh: price,
        }
    }
    // Test-only placeholder prices.
    fn plan(name: &str, from: NaiveDate, export: ExportRule) -> RatePlan {
        RatePlan {
            name: name.into(),
            effective_from: from,
            fixed_daily: 0.5,
            seasons: vec![
                Season {
                    name: "summer".into(),
                    months: vec![6, 7, 8, 9],
                    periods: vec![
                        p("partial-peak", DayType::All, 15, 16, 0.45),
                        p("peak", DayType::All, 16, 21, 0.50),
                        p("off-peak", DayType::All, 21, 15, 0.30),
                    ],
                },
                Season {
                    name: "winter".into(),
                    months: vec![1, 2, 3, 4, 5, 10, 11, 12],
                    periods: vec![
                        p("peak", DayType::Weekday, 16, 21, 0.40),
                        p("off-peak", DayType::Weekday, 21, 16, 0.25),
                        p("off-peak", DayType::Weekend, 0, 24, 0.20),
                    ],
                },
            ],
            export,
            baseline: None,
            tiers: vec![],
            demand_per_kw: None,
        }
    }
    fn nem(nbc: f64) -> ExportRule {
        ExportRule::NetMetering {
            non_bypassable_per_kwh: nbc,
        }
    }
    fn cfg_with(plans: Vec<RatePlan>) -> TariffConfig {
        TariffConfig {
            enabled: true,
            holidays: vec![d(2026, 1, 1)],
            plans,
            ..Default::default()
        }
    }
    fn cfg() -> TariffConfig {
        cfg_with(vec![plan("v1", d(2020, 1, 1), nem(0.0))])
    }
    fn he(t: DateTime<Utc>, import: f64, export: f64, load: f64) -> HourEnergy {
        HourEnergy {
            hour_start: t,
            import_kwh: import,
            export_kwh: export,
            load_kwh: load,
            pv_kwh: 0.0,
        }
    }
    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn tariff_valid_config_passes() {
        assert_eq!(cfg().validate(), Ok(()));
    }

    #[test]
    fn tariff_tou_boundaries() {
        let c = cfg();
        let r = c.rate_at(la(2026, 7, 15, 15, 59)).unwrap();
        assert_eq!(
            (r.period.as_str(), r.import_per_kwh),
            ("partial-peak", 0.45)
        );
        let r = c.rate_at(la(2026, 7, 15, 16, 0)).unwrap();
        assert_eq!((r.period.as_str(), r.import_per_kwh), ("peak", 0.50));
        assert_eq!(r.season, "summer");
        assert_eq!(c.rate_at(la(2026, 7, 15, 20, 59)).unwrap().period, "peak");
        assert_eq!(
            c.rate_at(la(2026, 7, 15, 21, 0)).unwrap().period,
            "off-peak"
        );
    }

    #[test]
    fn tariff_weekend_vs_weekday_and_holiday() {
        let c = cfg();
        let wd = c.rate_at(la(2026, 1, 14, 17, 0)).unwrap(); // Wednesday
        assert_eq!(
            (
                wd.period.as_str(),
                wd.import_per_kwh,
                wd.is_holiday_or_weekend
            ),
            ("peak", 0.40, false)
        );
        let we = c.rate_at(la(2026, 1, 17, 17, 0)).unwrap(); // Saturday
        assert_eq!(
            (
                we.period.as_str(),
                we.import_per_kwh,
                we.is_holiday_or_weekend
            ),
            ("off-peak", 0.20, true)
        );
        let hol = c.rate_at(la(2026, 1, 1, 17, 0)).unwrap(); // Thursday holiday
        assert_eq!(
            (hol.import_per_kwh, hol.is_holiday_or_weekend),
            (0.20, true)
        );
    }

    #[test]
    fn tariff_wraparound_period() {
        let c = cfg();
        for h in [21, 23, 0, 3, 14] {
            assert_eq!(
                c.rate_at(la(2026, 7, 15, h, 0)).unwrap().import_per_kwh,
                0.30,
                "hour {h}"
            );
        }
        assert_eq!(
            c.rate_at(la(2026, 1, 14, 6, 0)).unwrap().import_per_kwh,
            0.25
        );
    }

    fn utc_range(start: DateTime<Utc>, n: i64) -> Vec<HourEnergy> {
        (0..n)
            .map(|i| he(start + Duration::hours(i), 1.0, 0.0, 1.0))
            .collect()
    }

    #[test]
    fn tariff_dst_spring_forward() {
        let c = cfg();
        // 2026-03-08 00:00 PST = 08:00 UTC; cover 3 local days.
        let start = la(2026, 3, 8, 0, 0);
        let rep = c.cost_hours(&utc_range(start, 23 + 24 + 24));
        let day = rep.days.iter().find(|x| x.date == d(2026, 3, 8)).unwrap();
        assert_eq!(day.hours, 23);
        assert!(
            !rep.lines
                .iter()
                .any(|l| l.local_date == d(2026, 3, 8) && l.local_hour == 2)
        );
        assert_eq!(
            rep.days
                .iter()
                .find(|x| x.date == d(2026, 3, 9))
                .unwrap()
                .hours,
            24
        );
        assert_eq!(c.rate_schedule_for_day(d(2026, 3, 8)).len(), 23);
        // Monday after: 16:00 PDT = 23:00 UTC is peak; 22:00 UTC (15:00 PDT) is not.
        let mon_peak = Utc.with_ymd_and_hms(2026, 3, 9, 23, 0, 0).unwrap();
        assert_eq!(c.rate_at(mon_peak).unwrap().period, "peak");
        let before = Utc.with_ymd_and_hms(2026, 3, 9, 22, 0, 0).unwrap();
        assert_eq!(c.rate_at(before).unwrap().period, "off-peak");
        // Day totals: 23 hours × 1 kWh × weekend 0.20.
        assert!(close(day.import_cost, 23.0 * 0.20));
    }

    #[test]
    fn tariff_dst_fall_back() {
        let c = cfg();
        let start = la(2026, 11, 1, 0, 0); // 07:00 UTC
        let rep = c.cost_hours(&utc_range(start, 25 + 24));
        let day = rep.days.iter().find(|x| x.date == d(2026, 11, 1)).unwrap();
        assert_eq!(day.hours, 25);
        let ones = rep
            .lines
            .iter()
            .filter(|l| l.local_date == d(2026, 11, 1) && l.local_hour == 1)
            .count();
        assert_eq!(ones, 2);
        assert_eq!(c.rate_schedule_for_day(d(2026, 11, 1)).len(), 25);
        assert_eq!(c.rate_schedule_for_day(d(2026, 11, 2)).len(), 24);
        // Monday 16:00 PST = 00:00 UTC Nov 3.
        let peak = Utc.with_ymd_and_hms(2026, 11, 3, 0, 0, 0).unwrap();
        assert_eq!(c.rate_at(peak).unwrap().period, "peak");
        let before = Utc.with_ymd_and_hms(2026, 11, 2, 23, 0, 0).unwrap();
        assert_eq!(c.rate_at(before).unwrap().period, "off-peak");
    }

    #[test]
    fn tariff_effective_date_switching() {
        let mut v2 = plan("v2", d(2026, 5, 1), ExportRule::FixedRate { per_kwh: 0.05 });
        v2.seasons[0].periods[1].import_per_kwh = 0.60;
        let c = cfg_with(vec![v2, plan("v1", d(2020, 1, 1), nem(0.0))]);
        assert_eq!(c.validate(), Ok(()));
        assert_eq!(c.plan_for(d(2026, 4, 30)).unwrap().name, "v1");
        assert_eq!(c.plan_for(d(2026, 5, 1)).unwrap().name, "v2");
        assert!(c.plan_for(d(2019, 12, 31)).is_none());
        let r = c.rate_at(la(2026, 7, 15, 17, 0)).unwrap();
        assert_eq!(
            (r.plan.as_str(), r.import_per_kwh, r.export_per_kwh),
            ("v2", 0.60, 0.05)
        );
        let r = c.rate_at(la(2026, 4, 30, 23, 0)).unwrap();
        assert_eq!((r.plan.as_str(), r.export_per_kwh), ("v1", 0.25));
        assert!(c.rate_at(la(2019, 7, 1, 12, 0)).is_none());
        let rep = c.cost_hours(&[he(la(2019, 7, 1, 12, 0), 1.0, 0.0, 1.0)]);
        assert!(rep.lines.is_empty());
        assert!(
            rep.notes
                .iter()
                .any(|n| n.contains("1 hour(s) had no matching plan"))
        );
    }

    #[test]
    fn pge_nbt_export_credits() {
        let mut c = cfg();
        c.plans[0].export = ExportRule::PgeNbt {
            vintage: 2024,
            generation: true,
            acc_plus: true,
            pto_date: Some(d(2024, 8, 1)),
        };
        c.pge_holidays = true;
        assert!(c.validate().is_ok(), "{:?}", c.validate());
        // Vintage 2024, Sep 2026 weekday 7 pm: 2.52139 + ACC Plus 0.0176.
        let r = c.rate_at(la(2026, 9, 15, 19, 30)).unwrap();
        assert!(
            (r.export_per_kwh - (2.52139 + 0.0176)).abs() < 1e-9,
            "{r:?}"
        );
        // ACC Plus ends 9 years after PTO.
        let r = c.rate_at(la(2033, 9, 15, 19, 30)).unwrap();
        let r2 = c.rate_at(la(2033, 7, 15, 19, 30)).unwrap();
        assert!(r.export_per_kwh > 0.0 && r2.export_per_kwh > 0.0);
        let plus = |t| {
            let mut c2 = c.clone();
            if let ExportRule::PgeNbt { acc_plus, .. } = &mut c2.plans[0].export {
                *acc_plus = false;
            }
            c.rate_at(t).unwrap().export_per_kwh - c2.rate_at(t).unwrap().export_per_kwh
        };
        assert!((plus(la(2033, 7, 31, 12, 0)) - 0.0176).abs() < 1e-9);
        assert!(plus(la(2033, 8, 1, 12, 0)).abs() < 1e-9);
        // Observed Independence Day 2026 (Friday Jul 3) uses the weekend table.
        assert!(
            c.rate_at(la(2026, 7, 3, 12, 0))
                .unwrap()
                .is_holiday_or_weekend
        );
        // Delivery only (CCA customer) is lower.
        if let ExportRule::PgeNbt {
            generation,
            acc_plus,
            ..
        } = &mut c.plans[0].export
        {
            *generation = false;
            *acc_plus = false;
        }
        let r = c.rate_at(la(2026, 9, 15, 19, 30)).unwrap();
        assert!((r.export_per_kwh - 0.06685).abs() < 1e-9, "{r:?}");
        // JSON form from the settings page.
        let p: ExportRule = serde_json::from_str(
            r#"{"type":"pge_nbt","vintage":2025,"acc_plus":true,"pto_date":null}"#,
        )
        .unwrap();
        assert_eq!(
            p,
            ExportRule::PgeNbt {
                vintage: 2025,
                generation: true,
                acc_plus: true,
                pto_date: None
            }
        );
        c.plans[0].export = ExportRule::PgeNbt {
            vintage: 2019,
            generation: true,
            acc_plus: false,
            pto_date: None,
        };
        assert!(
            c.validate()
                .unwrap_err()
                .iter()
                .any(|e| e.contains("vintage 2019"))
        );
    }

    #[test]
    fn tariff_validate_catches_problems() {
        let mut c = cfg();
        c.plans[0].seasons[1].months.retain(|m| *m != 12);
        c.plans[0].seasons[0].periods[0].end_hour = 17; // overlap 16 with peak
        c.plans[0].seasons[1].periods[1].start_hour = 22; // gap at 21 on weekdays
        c.plans[0].seasons[1].periods[2].import_per_kwh = 11.0;
        c.time_zone = "Mars/Olympus".into();
        let errs = c.validate().unwrap_err();
        let has = |s: &str| errs.iter().any(|e| e.contains(s));
        assert!(has("month 12 is not in any season"), "{errs:?}");
        assert!(
            has("hour(s) 16 overlap between periods 'partial-peak', 'peak'"),
            "{errs:?}"
        );
        assert!(has("on weekdays hour(s) 21 are not covered"), "{errs:?}");
        assert!(has("price 11"), "{errs:?}");
        assert!(has("Mars/Olympus"), "{errs:?}");

        let mut c = cfg();
        c.plans[0].seasons[0].months.push(1);
        c.plans[0].export = ExportRule::Schedule {
            by_month_hour: vec![vec![0.0; 24]; 11],
            weekend_by_month_hour: None,
        };
        let errs = c.validate().unwrap_err();
        assert!(
            errs.iter()
                .any(|e| e.contains("month 1 is in more than one season")),
            "{errs:?}"
        );
        assert!(errs.iter().any(|e| e.contains("12 rows")), "{errs:?}");

        let c = cfg_with(vec![]);
        assert!(c.validate().is_err());
    }

    #[test]
    fn tariff_export_rules() {
        let t = la(2026, 7, 15, 17, 0); // summer weekday peak 0.50
        let c = cfg_with(vec![plan("p", d(2020, 1, 1), nem(0.03))]);
        assert!(close(c.rate_at(t).unwrap().export_per_kwh, 0.47));
        let c = cfg_with(vec![plan(
            "p",
            d(2020, 1, 1),
            ExportRule::FixedRate { per_kwh: 0.04 },
        )]);
        assert_eq!(c.rate_at(t).unwrap().export_per_kwh, 0.04);
        let m: Vec<Vec<f64>> = (1..=12)
            .map(|mo| {
                (0..24)
                    .map(|h| mo as f64 * 0.01 + h as f64 * 0.001)
                    .collect()
            })
            .collect();
        let c = cfg_with(vec![plan(
            "p",
            d(2020, 1, 1),
            ExportRule::Schedule {
                by_month_hour: m,
                weekend_by_month_hour: Some(vec![vec![0.07; 24]; 12]),
            },
        )]);
        assert_eq!(c.validate(), Ok(()));
        assert!(close(c.rate_at(t).unwrap().export_per_kwh, 0.07 + 0.017));
        assert!(close(
            c.rate_at(la(2026, 7, 18, 17, 0)).unwrap().export_per_kwh,
            0.07
        )); // Saturday
        let c = cfg_with(vec![plan("p", d(2020, 1, 1), ExportRule::None)]);
        assert_eq!(c.rate_at(t).unwrap().export_per_kwh, 0.0);
        let rep = c.cost_hours(&[he(t, 0.0, 5.0, 1.0)]);
        assert_eq!(rep.totals.export_credit, 0.0);
        assert_eq!(ExportRule::default(), ExportRule::None);
    }

    #[test]
    fn tariff_fixed_daily_once_per_day_with_data() {
        let c = cfg();
        let hours = vec![
            he(la(2026, 7, 15, 10, 0), 1.0, 0.0, 1.0),
            he(la(2026, 7, 17, 10, 0), 1.0, 0.0, 1.0),
            he(la(2026, 7, 17, 11, 0), 1.0, 0.0, 1.0),
            he(la(2026, 7, 17, 12, 0), 1.0, 0.0, 1.0),
        ];
        let rep = c.cost_hours(&hours);
        assert_eq!(rep.days.len(), 2);
        assert!(close(rep.totals.fixed_charge, 1.0));
        assert!(close(rep.days[1].net_cost, 3.0 * 0.30 + 0.5));
        assert!(close(rep.totals.net_cost, 4.0 * 0.30 + 1.0));
    }

    #[test]
    fn tariff_baseline_credit() {
        let mut pl = plan("p", d(2020, 1, 1), nem(0.0));
        let mut daily = vec![0.0; 12];
        daily[0] = 10.0;
        pl.baseline = Some(Baseline {
            daily_kwh_by_month: daily,
            credit_per_kwh: 0.10,
        });
        let c = cfg_with(vec![pl]);
        assert_eq!(c.validate(), Ok(()));
        let rep = c.cost_hours(&[
            he(la(2026, 1, 14, 10, 0), 15.0, 0.0, 15.0),
            he(la(2026, 1, 15, 10, 0), 15.0, 0.0, 15.0),
        ]);
        assert_eq!(rep.months[0].baseline_allowance_kwh, 20.0);
        assert!(close(rep.lines[0].baseline_credit, 1.5));
        assert!(close(rep.lines[1].baseline_credit, 0.5));
        assert!(close(rep.totals.baseline_credit, 2.0));
        assert!(close(rep.totals.net_cost, 30.0 * 0.25 - 2.0 + 1.0));
        assert!(
            rep.notes
                .iter()
                .any(|n| n.contains("baseline credit applied"))
        );
    }

    #[test]
    fn tariff_tiers() {
        let mut pl = plan("p", d(2020, 1, 1), nem(0.0));
        pl.tiers = vec![
            Tier {
                up_to_kwh_per_month: Some(10.0),
                adder_per_kwh: 0.0,
            },
            Tier {
                up_to_kwh_per_month: None,
                adder_per_kwh: 0.10,
            },
        ];
        let c = cfg_with(vec![pl]);
        assert_eq!(c.validate(), Ok(()));
        let rep = c.cost_hours(&[
            he(la(2026, 7, 15, 10, 0), 6.0, 0.0, 6.0),
            he(la(2026, 7, 15, 11, 0), 6.0, 0.0, 6.0),
            he(la(2026, 8, 1, 11, 0), 6.0, 0.0, 6.0), // new month resets
        ]);
        assert_eq!(rep.lines[0].tier_adder_cost, 0.0);
        assert!(close(rep.lines[1].tier_adder_cost, 0.2));
        assert_eq!(rep.lines[2].tier_adder_cost, 0.0);
        assert!(close(rep.totals.tier_adder_cost, 0.2));

        let mut bad = plan("p", d(2020, 1, 1), nem(0.0));
        bad.tiers = vec![Tier {
            up_to_kwh_per_month: Some(10.0),
            adder_per_kwh: 0.0,
        }];
        assert!(cfg_with(vec![bad]).validate().is_err());
    }

    #[test]
    fn tariff_demand_charge() {
        let mut pl = plan("p", d(2020, 1, 1), nem(0.0));
        pl.demand_per_kw = Some(10.0);
        let c = cfg_with(vec![pl]);
        let rep = c.cost_hours(&[
            he(la(2026, 7, 15, 10, 0), 2.0, 0.0, 2.0),
            he(la(2026, 7, 15, 11, 0), 5.0, 0.0, 5.0),
            he(la(2026, 7, 16, 11, 0), 3.0, 0.0, 3.0),
            he(la(2026, 8, 3, 11, 0), 1.0, 0.0, 1.0),
        ]);
        assert_eq!(rep.months.len(), 2);
        assert_eq!(rep.months[0].peak_demand_kw, 5.0);
        assert!(close(rep.months[0].demand_charge, 50.0));
        assert!(close(rep.months[1].demand_charge, 10.0));
        assert!(close(rep.totals.demand_charge, 60.0));
        let energy = 11.0 * 0.30;
        assert!(close(rep.totals.net_cost, energy + 3.0 * 0.5 + 60.0));
    }

    #[test]
    fn tariff_avoided_cost_and_nem_credit() {
        let c = cfg();
        let rep = c.cost_hours(&[he(la(2026, 7, 15, 17, 0), 1.0, 0.5, 3.0)]);
        let l = &rep.lines[0];
        assert_eq!(l.period, "peak");
        assert!(close(l.import_cost, 0.5));
        assert!(close(l.export_credit, 0.25));
        assert!(close(l.avoided_cost, 1.0));
        assert!(close(l.no_solar_cost, 1.5));
        assert!(close(rep.days[0].avoided_cost, 1.0));
        assert!(close(rep.totals.net_cost, 0.5 - 0.25 + 0.5));
    }

    #[test]
    fn tariff_duplicates_and_unsorted_input() {
        let c = cfg();
        let t = la(2026, 7, 15, 10, 0);
        let rep = c.cost_hours(&[
            he(t + Duration::hours(1), 1.0, 0.0, 1.0),
            he(t, 1.0, 0.0, 1.0),
            he(t, 9.0, 0.0, 9.0),
        ]);
        assert_eq!(rep.lines.len(), 2);
        assert!(rep.lines[0].hour_start < rep.lines[1].hour_start);
        assert!(rep.notes.iter().any(|n| n.contains("duplicate")));
    }
}
