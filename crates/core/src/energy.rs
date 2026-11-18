//! Power-flow attribution and energy integration.
//!
//! Gateway sign conventions (verified against live readings, see docs/ARCHITECTURE.md):
//! - PV positive = production
//! - storage negative = battery charging, positive = discharging
//! - grid positive = import, negative = export
//! - load positive = household consumption
//!
//! Conservation: `pv + storage + grid + generator = load` (within metering tolerance).

use serde::Serialize;

/// One telemetry sample, power in watts, timestamp from the gateway (Unix seconds).
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Sample {
    pub ts: i64,
    pub pv_w: f64,
    pub load_w: f64,
    pub storage_w: f64,
    pub grid_w: f64,
    pub gen_w: f64,
}

impl Sample {
    /// Sources minus sinks. Zero for a perfectly balanced reading.
    pub fn residual_w(&self) -> f64 {
        self.pv_w + self.storage_w + self.grid_w + self.gen_w - self.load_w
    }

    /// True when the residual is within `abs_tol_w` or `rel_tol` of the largest flow.
    /// Metering is not perfectly simultaneous, so allow some slack.
    pub fn is_balanced(&self, abs_tol_w: f64, rel_tol: f64) -> bool {
        let scale = [
            self.pv_w,
            self.load_w,
            self.storage_w,
            self.grid_w,
            self.gen_w,
        ]
        .iter()
        .fold(0.0_f64, |m, v| m.max(v.abs()));
        self.residual_w().abs() <= abs_tol_w.max(rel_tol * scale)
    }

    /// Split signed readings into non-negative directional flows and attribute them.
    pub fn flows(&self) -> Flows {
        Flows::from_sample(self)
    }
}

/// Directional power flows (all >= 0, watts) consistent with one reading.
///
/// Attribution policy (the gateway only reports net values at each meter, so the
/// split between paths is an inference): solar serves the house first, then
/// charges the battery, then exports. The battery serves remaining house load,
/// then any remaining export. The grid serves remaining house load, then any
/// remaining battery charging. A generator is treated like solar for the house.
/// Every path is bounded by the measured totals, so the drawing never shows a
/// flow the meters cannot support.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct Flows {
    pub pv_w: f64,
    pub load_w: f64,
    pub import_w: f64,
    pub export_w: f64,
    pub charge_w: f64,
    pub discharge_w: f64,
    pub gen_w: f64,
    pub pv_to_load: f64,
    pub pv_to_batt: f64,
    pub pv_to_grid: f64,
    pub batt_to_load: f64,
    pub batt_to_grid: f64,
    pub grid_to_load: f64,
    pub grid_to_batt: f64,
    pub gen_to_load: f64,
    /// Load not explained by any source (metering mismatch), watts.
    pub unexplained_w: f64,
}

impl Flows {
    pub fn from_sample(s: &Sample) -> Self {
        let pos = |v: f64| if v.is_finite() { v.max(0.0) } else { 0.0 };
        let pv = pos(s.pv_w);
        let load = pos(s.load_w);
        let import = pos(s.grid_w);
        let export = pos(-s.grid_w);
        let charge = pos(-s.storage_w);
        let discharge = pos(s.storage_w);
        let gen_p = pos(s.gen_w);

        let pv_to_load = pv.min(load);
        let mut rem_load = load - pv_to_load;
        let mut rem_pv = pv - pv_to_load;

        let gen_to_load = gen_p.min(rem_load);
        rem_load -= gen_to_load;

        let pv_to_batt = rem_pv.min(charge);
        rem_pv -= pv_to_batt;
        let mut rem_charge = charge - pv_to_batt;

        let pv_to_grid = rem_pv.min(export);
        let mut rem_export = export - pv_to_grid;

        let batt_to_load = discharge.min(rem_load);
        rem_load -= batt_to_load;
        let rem_dis = discharge - batt_to_load;

        let batt_to_grid = rem_dis.min(rem_export);
        rem_export -= batt_to_grid;
        let _ = rem_export;

        let grid_to_load = import.min(rem_load);
        rem_load -= grid_to_load;
        let rem_import = import - grid_to_load;

        let grid_to_batt = rem_import.min(rem_charge);
        rem_charge -= grid_to_batt;
        let _ = rem_charge;

        Flows {
            pv_w: pv,
            load_w: load,
            import_w: import,
            export_w: export,
            charge_w: charge,
            discharge_w: discharge,
            gen_w: gen_p,
            pv_to_load,
            pv_to_batt,
            pv_to_grid,
            batt_to_load,
            batt_to_grid,
            grid_to_load,
            grid_to_batt,
            gen_to_load,
            unexplained_w: rem_load,
        }
    }
}

/// Energy accumulated over an interval or bucket, watt-hours.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct Energy {
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
    /// Seconds of the bucket covered by valid (non-gap) measurements.
    pub covered_s: f64,
}

impl Energy {
    fn from_flows_trapezoid(a: &Flows, b: &Flows, dt_s: f64) -> Self {
        let k = dt_s / 3600.0 / 2.0;
        let t = |x: f64, y: f64| (x + y) * k;
        Energy {
            pv_wh: t(a.pv_w, b.pv_w),
            load_wh: t(a.load_w, b.load_w),
            import_wh: t(a.import_w, b.import_w),
            export_wh: t(a.export_w, b.export_w),
            charge_wh: t(a.charge_w, b.charge_w),
            discharge_wh: t(a.discharge_w, b.discharge_w),
            gen_wh: t(a.gen_w, b.gen_w),
            pv_to_load_wh: t(a.pv_to_load, b.pv_to_load),
            pv_to_batt_wh: t(a.pv_to_batt, b.pv_to_batt),
            pv_to_grid_wh: t(a.pv_to_grid, b.pv_to_grid),
            batt_to_load_wh: t(a.batt_to_load, b.batt_to_load),
            batt_to_grid_wh: t(a.batt_to_grid, b.batt_to_grid),
            grid_to_load_wh: t(a.grid_to_load, b.grid_to_load),
            grid_to_batt_wh: t(a.grid_to_batt, b.grid_to_batt),
            covered_s: dt_s,
        }
    }

    pub fn add(&mut self, o: &Energy) {
        self.pv_wh += o.pv_wh;
        self.load_wh += o.load_wh;
        self.import_wh += o.import_wh;
        self.export_wh += o.export_wh;
        self.charge_wh += o.charge_wh;
        self.discharge_wh += o.discharge_wh;
        self.gen_wh += o.gen_wh;
        self.pv_to_load_wh += o.pv_to_load_wh;
        self.pv_to_batt_wh += o.pv_to_batt_wh;
        self.pv_to_grid_wh += o.pv_to_grid_wh;
        self.batt_to_load_wh += o.batt_to_load_wh;
        self.batt_to_grid_wh += o.batt_to_grid_wh;
        self.grid_to_load_wh += o.grid_to_load_wh;
        self.grid_to_batt_wh += o.grid_to_batt_wh;
        self.covered_s += o.covered_s;
    }

    /// Share of solar production used on site (house + battery), 0..1.
    pub fn self_consumption(&self) -> Option<f64> {
        (self.pv_wh > 1.0)
            .then(|| ((self.pv_to_load_wh + self.pv_to_batt_wh) / self.pv_wh).clamp(0.0, 1.0))
    }
    /// Share of house load served directly by solar, 0..1.
    pub fn solar_direct_share(&self) -> Option<f64> {
        (self.load_wh > 1.0).then(|| (self.pv_to_load_wh / self.load_wh).clamp(0.0, 1.0))
    }
    /// Share of house load served by the battery, 0..1.
    pub fn battery_share(&self) -> Option<f64> {
        (self.load_wh > 1.0).then(|| (self.batt_to_load_wh / self.load_wh).clamp(0.0, 1.0))
    }
    /// Share of house load NOT supplied by the grid, 0..1.
    pub fn grid_independence(&self) -> Option<f64> {
        (self.load_wh > 1.0).then(|| (1.0 - self.grid_to_load_wh / self.load_wh).clamp(0.0, 1.0))
    }
}

/// Result of integrating one pair of consecutive samples.
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    /// Energy split into buckets: (bucket start, energy in that bucket).
    Energy(Vec<(i64, Energy)>),
    /// Same or older timestamp: a repeated gateway reading, nothing to add.
    Duplicate,
    /// Interval longer than the allowed gap: not integrated.
    Gap { seconds: i64 },
}

/// Integrate power between two consecutive samples with the trapezoid rule over the
/// real elapsed gateway time, splitting the interval at bucket boundaries
/// (`bucket_s`, e.g. 3600 for hourly). Each directional flow is integrated
/// separately so imports and exports never cancel inside an interval.
///
/// Intervals longer than `max_gap_s` are not integrated: we do not know what
/// happened while the gateway was silent.
pub fn integrate(prev: &Sample, cur: &Sample, bucket_s: i64, max_gap_s: i64) -> Step {
    let dt = cur.ts - prev.ts;
    if dt <= 0 {
        return Step::Duplicate;
    }
    if dt > max_gap_s {
        return Step::Gap { seconds: dt };
    }
    let fa = prev.flows();
    let fb = cur.flows();
    let mut out = Vec::new();
    let mut t0 = prev.ts;
    while t0 < cur.ts {
        let bucket = t0.div_euclid(bucket_s) * bucket_s;
        let t1 = (bucket + bucket_s).min(cur.ts);
        // Linear interpolation of each flow inside the interval.
        let lerp = |x: f64| (x - prev.ts as f64) / dt as f64;
        let (u0, u1) = (lerp(t0 as f64), lerp(t1 as f64));
        let f0 = mix(&fa, &fb, u0);
        let f1 = mix(&fa, &fb, u1);
        out.push((
            bucket,
            Energy::from_flows_trapezoid(&f0, &f1, (t1 - t0) as f64),
        ));
        t0 = t1;
    }
    Step::Energy(out)
}

fn mix(a: &Flows, b: &Flows, u: f64) -> Flows {
    let m = |x: f64, y: f64| x + (y - x) * u;
    Flows {
        pv_w: m(a.pv_w, b.pv_w),
        load_w: m(a.load_w, b.load_w),
        import_w: m(a.import_w, b.import_w),
        export_w: m(a.export_w, b.export_w),
        charge_w: m(a.charge_w, b.charge_w),
        discharge_w: m(a.discharge_w, b.discharge_w),
        gen_w: m(a.gen_w, b.gen_w),
        pv_to_load: m(a.pv_to_load, b.pv_to_load),
        pv_to_batt: m(a.pv_to_batt, b.pv_to_batt),
        pv_to_grid: m(a.pv_to_grid, b.pv_to_grid),
        batt_to_load: m(a.batt_to_load, b.batt_to_load),
        batt_to_grid: m(a.batt_to_grid, b.batt_to_grid),
        grid_to_load: m(a.grid_to_load, b.grid_to_load),
        grid_to_batt: m(a.grid_to_batt, b.grid_to_batt),
        gen_to_load: m(a.gen_to_load, b.gen_to_load),
        unexplained_w: m(a.unexplained_w, b.unexplained_w),
    }
}

/// Integrate an ordered series into buckets (convenience for tests and rebuilds).
/// Returns buckets sorted by start, plus the number of gaps skipped.
pub fn integrate_series(
    samples: &[Sample],
    bucket_s: i64,
    max_gap_s: i64,
) -> (Vec<(i64, Energy)>, usize) {
    let mut map: std::collections::BTreeMap<i64, Energy> = Default::default();
    let mut gaps = 0;
    let mut last: Option<&Sample> = None;
    for s in samples {
        if let Some(p) = last {
            match integrate(p, s, bucket_s, max_gap_s) {
                Step::Energy(parts) => {
                    for (b, e) in parts {
                        map.entry(b).or_default().add(&e);
                    }
                }
                Step::Duplicate => continue,
                Step::Gap { .. } => gaps += 1,
            }
        }
        last = Some(s);
    }
    (map.into_iter().collect(), gaps)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(ts: i64, pv: f64, load: f64, st: f64, grid: f64) -> Sample {
        Sample {
            ts,
            pv_w: pv * 1000.0,
            load_w: load * 1000.0,
            storage_w: st * 1000.0,
            grid_w: grid * 1000.0,
            gen_w: 0.0,
        }
    }

    /// Real readings from the owner's gateway (2026, project prompt §2).
    #[test]
    fn verified_readings_balance() {
        let rows = [
            s(0, 5.05, 1.58, -1.14, -2.33),
            s(1, 6.35, 2.02, -3.72, -0.61),
            s(2, 6.53, 2.13, -6.26, 1.86),
            s(3, 6.54, 7.41, 0.92, -0.05),
            s(4, 6.55, 7.16, 0.70, -0.09),
        ];
        for r in rows {
            // Displayed values are rounded to 10 W, so allow 30 W.
            assert!(
                r.residual_w().abs() < 30.0,
                "{r:?} residual {}",
                r.residual_w()
            );
            assert!(r.is_balanced(50.0, 0.02));
        }
    }

    #[test]
    fn flows_solar_surplus_charging_and_exporting() {
        let f = s(0, 5.05, 1.58, -1.14, -2.33).flows();
        assert!((f.pv_to_load - 1580.0).abs() < 1e-6);
        assert!((f.pv_to_batt - 1140.0).abs() < 1e-6);
        assert!((f.pv_to_grid - 2330.0).abs() < 1e-6);
        assert_eq!(f.grid_to_load, 0.0);
        assert_eq!(f.batt_to_load, 0.0);
        assert!(f.unexplained_w < 1e-6);
    }

    #[test]
    fn flows_grid_charging_battery() {
        // 12:34:10: battery charging 6.26 kW while importing 1.86 kW.
        let f = s(0, 6.53, 2.13, -6.26, 1.86).flows();
        assert!((f.pv_to_load - 2130.0).abs() < 1e-6);
        assert!((f.pv_to_batt - 4400.0).abs() < 1e-6);
        assert!((f.grid_to_batt - 1860.0).abs() < 1e-6);
        assert_eq!(f.pv_to_grid, 0.0);
    }

    #[test]
    fn flows_battery_and_solar_feed_large_load() {
        let f = s(0, 6.54, 7.41, 0.92, -0.05).flows();
        assert!((f.pv_to_load - 6540.0).abs() < 1e-6);
        assert!((f.batt_to_load - 870.0).abs() < 1e-6);
        assert!((f.batt_to_grid - 50.0).abs() < 1e-6);
    }

    #[test]
    fn flows_night_import() {
        let f = s(0, 0.0, 0.8, 0.0, 0.8).flows();
        assert_eq!(f.grid_to_load, 800.0);
        assert_eq!(f.pv_to_load, 0.0);
    }

    #[test]
    fn flows_never_negative_or_exceed_totals() {
        for r in [
            s(0, -0.01, 0.5, 0.4, 0.11),
            s(0, 3.0, 1.0, 2.0, -4.0),
            s(0, 0.0, 0.0, 0.0, 0.0),
        ] {
            let f = r.flows();
            for v in [
                f.pv_to_load,
                f.pv_to_batt,
                f.pv_to_grid,
                f.batt_to_load,
                f.batt_to_grid,
                f.grid_to_load,
                f.grid_to_batt,
            ] {
                assert!(v >= 0.0);
            }
            assert!(f.pv_to_load + f.pv_to_batt + f.pv_to_grid <= f.pv_w + 1e-9);
            assert!(f.grid_to_load + f.grid_to_batt <= f.import_w + 1e-9);
            assert!(f.batt_to_load + f.batt_to_grid <= f.discharge_w + 1e-9);
        }
    }

    #[test]
    fn constant_power_integrates_exactly_with_irregular_spacing() {
        // 1 kW load from grid for 1 hour, sampled at irregular 11-14 s intervals.
        let mut v = vec![];
        let mut t = 0;
        let steps = [11, 14, 12, 13];
        let mut i = 0;
        while t < 3600 {
            v.push(s(t, 0.0, 1.0, 0.0, 1.0));
            t += steps[i % 4];
            i += 1;
        }
        v.push(s(3600, 0.0, 1.0, 0.0, 1.0));
        let (b, gaps) = integrate_series(&v, 3600, 120);
        assert_eq!(gaps, 0);
        assert_eq!(b.len(), 1);
        assert!((b[0].1.load_wh - 1000.0).abs() < 1e-9);
        assert!((b[0].1.import_wh - 1000.0).abs() < 1e-9);
        assert!((b[0].1.covered_s - 3600.0).abs() < 1e-9);
    }

    #[test]
    fn ramp_integrates_by_trapezoid() {
        let v = [s(0, 0.0, 0.0, 0.0, 0.0), s(3600, 2.0, 2.0, 0.0, 0.0)];
        let (b, _) = integrate_series(&v, 3600, 4000);
        assert!((b[0].1.pv_wh - 1000.0).abs() < 1e-9);
    }

    #[test]
    fn splits_interval_across_hour_boundary() {
        let v = [s(3590, 0.0, 3.6, 0.0, 3.6), s(3610, 0.0, 3.6, 0.0, 3.6)];
        let (b, _) = integrate_series(&v, 3600, 120);
        assert_eq!(b.len(), 2);
        assert_eq!(b[0].0, 0);
        assert_eq!(b[1].0, 3600);
        assert!((b[0].1.load_wh - 10.0).abs() < 1e-9);
        assert!((b[1].1.load_wh - 10.0).abs() < 1e-9);
    }

    #[test]
    fn duplicates_are_not_double_counted() {
        let v = [
            s(0, 0.0, 1.0, 0.0, 1.0),
            s(0, 0.0, 1.0, 0.0, 1.0),
            s(10, 0.0, 1.0, 0.0, 1.0),
            s(10, 0.0, 9.0, 0.0, 9.0),
        ];
        let (b, _) = integrate_series(&v, 3600, 120);
        assert!((b[0].1.load_wh - 10.0 / 3.6).abs() < 1e-9);
    }

    #[test]
    fn long_gaps_are_not_integrated() {
        let v = [
            s(0, 0.0, 1.0, 0.0, 1.0),
            s(10, 0.0, 1.0, 0.0, 1.0),
            s(1000, 0.0, 1.0, 0.0, 1.0),
            s(1010, 0.0, 1.0, 0.0, 1.0),
        ];
        let (b, gaps) = integrate_series(&v, 3600, 120);
        assert_eq!(gaps, 1);
        assert!((b[0].1.covered_s - 20.0).abs() < 1e-9);
    }

    #[test]
    fn import_and_export_do_not_cancel() {
        // Grid swings from +2 kW import to -2 kW export over 20 s.
        let v = [s(0, 0.0, 2.0, 0.0, 2.0), s(20, 4.0, 2.0, 0.0, -2.0)];
        let (b, _) = integrate_series(&v, 3600, 120);
        let e = b[0].1;
        assert!(e.import_wh > 0.0 && e.export_wh > 0.0);
    }

    #[test]
    fn ratios() {
        let e = Energy {
            pv_wh: 10.0,
            load_wh: 8.0,
            pv_to_load_wh: 4.0,
            pv_to_batt_wh: 3.0,
            pv_to_grid_wh: 3.0,
            grid_to_load_wh: 2.0,
            batt_to_load_wh: 2.0,
            ..Default::default()
        };
        assert!((e.self_consumption().unwrap() - 0.7).abs() < 1e-9);
        assert!((e.solar_direct_share().unwrap() - 0.5).abs() < 1e-9);
        assert!((e.grid_independence().unwrap() - 0.75).abs() < 1e-9);
        assert!((e.battery_share().unwrap() - 0.25).abs() < 1e-9);
        assert_eq!(Energy::default().self_consumption(), None);
    }
}
