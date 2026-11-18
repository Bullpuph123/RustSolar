//! Battery modelling: SOC simulation, outage runtime and health estimates.
//!
//! Everything here is an estimate built from configured or observed parameters.
//! The Enphase gateway reports SOC as a percentage of usable energy; we follow
//! that convention (0 % = empty usable energy, 100 % = full).

use serde::Serialize;

/// Battery parameters for simulation.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct BatteryParams {
    /// Usable energy at 100 % SOC, Wh.
    pub usable_wh: f64,
    /// Grid-tied operation will not discharge below this SOC (backup reserve), %.
    pub reserve_pct: f64,
    /// Lowest SOC the battery will reach during an outage, %. Default 0.
    pub outage_floor_pct: f64,
    pub max_charge_w: f64,
    pub max_discharge_w: f64,
    /// AC-to-AC round-trip efficiency, 0..1. Split evenly (sqrt) between charge and discharge.
    pub round_trip_eff: f64,
}

impl BatteryParams {
    fn eff_one_way(&self) -> f64 {
        self.round_trip_eff.clamp(0.5, 1.0).sqrt()
    }
}

/// One step of the simulation input. `hours` may be fractional (e.g. the rest of the current hour).
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct SimStep {
    pub start_ts: i64,
    pub hours: f64,
    pub pv_wh: f64,
    pub load_wh: f64,
}

/// Simulated state at the END of a step.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct SimPoint {
    pub end_ts: i64,
    pub soc_pct: f64,
    pub charge_wh: f64,
    pub discharge_wh: f64,
    pub import_wh: f64,
    pub export_wh: f64,
    /// True when charging was limited because the battery was full.
    pub full: bool,
    /// True when discharge stopped at the reserve.
    pub at_reserve: bool,
    /// Fraction of the step during which SOC was changing (1.0 unless a limit was hit
    /// part-way through). Used to time the moment the limit was reached.
    pub active_frac: f64,
}

/// Simulate self-consumption mode (Enphase "Self-Consumption" profile):
/// surplus solar charges the battery, deficits are covered by the battery down
/// to the reserve, the grid covers the rest and absorbs surplus once full.
/// Grid charging and time-of-use arbitrage are not modelled.
pub fn simulate(p: &BatteryParams, start_soc_pct: f64, steps: &[SimStep]) -> Vec<SimPoint> {
    let eff = p.eff_one_way();
    let cap = p.usable_wh.max(1.0);
    let mut soc_wh = (start_soc_pct.clamp(0.0, 100.0) / 100.0) * cap;
    let reserve_wh = (p.reserve_pct.clamp(0.0, 100.0) / 100.0) * cap;
    let mut out = Vec::with_capacity(steps.len());
    for st in steps {
        let h = st.hours.max(0.0);
        let net = st.pv_wh.max(0.0) - st.load_wh.max(0.0);
        let (mut charge, mut discharge, mut import, mut export) = (0.0, 0.0, 0.0, 0.0);
        let (mut full, mut at_reserve) = (false, false);
        let mut active_frac = 1.0;
        if net >= 0.0 {
            // AC energy into battery, limited by power and headroom.
            let room_ac = (cap - soc_wh).max(0.0) / eff;
            let uncapped = net.min(p.max_charge_w * h);
            charge = uncapped.min(room_ac);
            soc_wh += charge * eff;
            export = net - charge;
            full = room_ac - charge < 1e-6 && export > 1e-6;
            if full && uncapped > 0.0 {
                active_frac = (charge / uncapped).clamp(0.0, 1.0);
            }
        } else {
            let need = -net;
            let avail_ac = (soc_wh - reserve_wh).max(0.0) * eff;
            let uncapped = need.min(p.max_discharge_w * h);
            discharge = uncapped.min(avail_ac);
            soc_wh -= discharge / eff;
            import = need - discharge;
            at_reserve = avail_ac - discharge < 1e-6 && import > 1e-6;
            if at_reserve && uncapped > 0.0 {
                active_frac = (discharge / uncapped).clamp(0.0, 1.0);
            }
        }
        let end_ts = st.start_ts + (h * 3600.0).round() as i64;
        out.push(SimPoint {
            end_ts,
            soc_pct: (soc_wh / cap * 100.0).clamp(0.0, 100.0),
            charge_wh: charge,
            discharge_wh: discharge,
            import_wh: import,
            export_wh: export,
            full,
            at_reserve,
            active_frac,
        });
    }
    out
}

/// First time the simulated SOC reaches `target_pct` (rising if `rising`), interpolated
/// linearly inside the step. `start_ts`/`start_soc` describe the state before `points[0]`.
pub fn time_to_soc(
    start_ts: i64,
    start_soc: f64,
    points: &[SimPoint],
    target_pct: f64,
    rising: bool,
) -> Option<i64> {
    let hit = |s: f64| {
        if rising {
            s >= target_pct - 1e-6
        } else {
            s <= target_pct + 1e-6
        }
    };
    if hit(start_soc) {
        return Some(start_ts);
    }
    let (mut t0, mut s0) = (start_ts, start_soc);
    for pt in points {
        if hit(pt.soc_pct) {
            let ds = pt.soc_pct - s0;
            let u = if ds.abs() < 1e-9 {
                1.0
            } else {
                ((target_pct - s0) / ds).clamp(0.0, 1.0)
            };
            let span = (pt.end_ts - t0) as f64 * pt.active_frac;
            return Some(t0 + (span * u).round() as i64);
        }
        t0 = pt.end_ts;
        s0 = pt.soc_pct;
    }
    None
}

/// SOC (%) at time `t` by linear interpolation of a simulation.
pub fn soc_at(start_ts: i64, start_soc: f64, points: &[SimPoint], t: i64) -> Option<f64> {
    if t <= start_ts {
        return Some(start_soc);
    }
    let (mut t0, mut s0) = (start_ts, start_soc);
    for pt in points {
        if t <= pt.end_ts {
            let span = ((pt.end_ts - t0) as f64 * pt.active_frac).max(1.0);
            let u = ((t - t0) as f64 / span).min(1.0);
            return Some(s0 + (pt.soc_pct - s0) * u);
        }
        t0 = pt.end_ts;
        s0 = pt.soc_pct;
    }
    None
}

/// Hours a constant `load_w` can run from the battery during a grid outage, from
/// `soc_pct` down to the outage floor, ignoring solar. Limited by max discharge
/// power: returns `None` when the load exceeds what the batteries can supply.
pub fn outage_runtime_h(p: &BatteryParams, soc_pct: f64, load_w: f64) -> Option<f64> {
    if load_w <= 0.0 {
        return Some(f64::INFINITY);
    }
    if load_w > p.max_discharge_w {
        return None;
    }
    let avail_wh =
        ((soc_pct - p.outage_floor_pct).max(0.0) / 100.0) * p.usable_wh * p.eff_one_way();
    Some(avail_wh / load_w)
}

/// Outage runtime with an hourly load profile and optional solar recharge, starting at
/// `steps[0]`. Returns hours until the battery reaches the outage floor (or `None` if it
/// lasts the whole horizon). Solar production above load recharges the battery
/// (a microinverter system keeps producing during an outage with batteries present).
pub fn outage_runtime_profile_h(p: &BatteryParams, soc_pct: f64, steps: &[SimStep]) -> Option<f64> {
    let outage = BatteryParams {
        reserve_pct: p.outage_floor_pct,
        ..*p
    };
    let pts = simulate(&outage, soc_pct, steps);
    let mut elapsed = 0.0;
    for (st, pt) in steps.iter().zip(&pts) {
        if pt.import_wh > 1e-6 {
            // Unserved energy: the battery ran out (or hit the power limit) inside this step.
            let served_frac = if st.load_wh > 0.0 {
                1.0 - pt.import_wh / (st.load_wh - st.pv_wh).max(1e-9)
            } else {
                1.0
            };
            return Some(elapsed + st.hours * served_frac.clamp(0.0, 1.0));
        }
        elapsed += st.hours;
    }
    None
}

/// Usable capacity implied by the gateway's aggregate stored energy and SOC
/// (`enc_agg_energy` Wh at `soc` %). Only meaningful above ~20 % SOC; returns None otherwise.
/// This is an observation, not a nameplate value.
pub fn capacity_from_stored(stored_wh: f64, soc_pct: f64) -> Option<f64> {
    (soc_pct >= 20.0 && stored_wh > 0.0).then(|| stored_wh / (soc_pct / 100.0))
}

/// Equivalent full cycles = discharge throughput / usable capacity.
pub fn equivalent_cycles(discharge_wh: f64, usable_wh: f64) -> f64 {
    if usable_wh > 0.0 {
        discharge_wh / usable_wh
    } else {
        0.0
    }
}

/// Observed round-trip efficiency over a window that starts and ends at the same SOC:
/// discharge / charge (AC). Returns None for small windows (< 2 kWh charged) or when the
/// SOC changed by more than 2 % (stored energy would bias the ratio).
pub fn observed_round_trip(
    charge_wh: f64,
    discharge_wh: f64,
    soc_start: f64,
    soc_end: f64,
) -> Option<f64> {
    ((soc_end - soc_start).abs() <= 2.0 && charge_wh >= 2000.0)
        .then(|| (discharge_wh / charge_wh).clamp(0.0, 1.2))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params() -> BatteryParams {
        BatteryParams {
            usable_wh: 10_000.0,
            reserve_pct: 20.0,
            outage_floor_pct: 0.0,
            max_charge_w: 7680.0,
            max_discharge_w: 7680.0,
            round_trip_eff: 0.9,
        }
    }
    fn hour(i: i64, pv: f64, load: f64) -> SimStep {
        SimStep {
            start_ts: i * 3600,
            hours: 1.0,
            pv_wh: pv,
            load_wh: load,
        }
    }

    #[test]
    fn surplus_charges_with_loss_then_exports_when_full() {
        let p = params();
        let pts = simulate(&p, 90.0, &[hour(0, 3000.0, 1000.0)]);
        let eff = 0.9f64.sqrt();
        // Room = 1000 Wh DC = 1000/eff AC.
        assert!((pts[0].charge_wh - 1000.0 / eff).abs() < 1e-6);
        assert!((pts[0].soc_pct - 100.0).abs() < 1e-9);
        assert!((pts[0].export_wh - (2000.0 - 1000.0 / eff)).abs() < 1e-6);
        assert!(pts[0].full);
    }

    #[test]
    fn deficit_discharges_to_reserve_then_imports() {
        let p = params();
        let pts = simulate(&p, 30.0, &[hour(0, 0.0, 2000.0)]);
        // 10 % of 10 kWh above reserve = 1000 Wh DC → 1000·eff AC.
        let eff = 0.9f64.sqrt();
        assert!((pts[0].discharge_wh - 1000.0 * eff).abs() < 1e-6);
        assert!((pts[0].soc_pct - 20.0).abs() < 1e-9);
        assert!((pts[0].import_wh - (2000.0 - 1000.0 * eff)).abs() < 1e-6);
        assert!(pts[0].at_reserve);
    }

    #[test]
    fn power_limits_respected() {
        let mut p = params();
        p.max_charge_w = 1000.0;
        let pts = simulate(&p, 50.0, &[hour(0, 5000.0, 0.0)]);
        assert!((pts[0].charge_wh - 1000.0).abs() < 1e-9);
        assert!((pts[0].export_wh - 4000.0).abs() < 1e-9);
    }

    #[test]
    fn energy_is_conserved_each_step() {
        let p = params();
        let steps: Vec<_> = (0..24)
            .map(|i| hour(i, if (8..17).contains(&i) { 3000.0 } else { 0.0 }, 800.0))
            .collect();
        for (st, pt) in steps.iter().zip(simulate(&p, 50.0, &steps)) {
            // pv + discharge + import = load + charge + export
            let lhs = st.pv_wh + pt.discharge_wh + pt.import_wh;
            let rhs = st.load_wh + pt.charge_wh + pt.export_wh;
            assert!((lhs - rhs).abs() < 1e-6);
        }
    }

    #[test]
    fn time_to_full_interpolates() {
        let p = BatteryParams {
            round_trip_eff: 1.0,
            ..params()
        };
        // +2000 Wh/h into a 10 kWh battery from 50 % → full after 2.5 h.
        let steps: Vec<_> = (0..4).map(|i| hour(i, 2000.0, 0.0)).collect();
        let pts = simulate(&p, 50.0, &steps);
        let t = time_to_soc(0, 50.0, &pts, 100.0, true).unwrap();
        assert_eq!(t, 9000);
        assert!((soc_at(0, 50.0, &pts, 1800).unwrap() - 60.0).abs() < 1e-9);
    }

    #[test]
    fn outage_runtime_constant_load() {
        let p = BatteryParams {
            round_trip_eff: 1.0,
            ..params()
        };
        assert!((outage_runtime_h(&p, 50.0, 1000.0).unwrap() - 5.0).abs() < 1e-9);
        assert_eq!(outage_runtime_h(&p, 50.0, 9000.0), None);
    }

    #[test]
    fn outage_runtime_profile_with_solar() {
        let p = BatteryParams {
            round_trip_eff: 1.0,
            ..params()
        };
        // 2 kWh available at 20 %, 1 kW load, no solar → 2 h.
        let steps: Vec<_> = (0..5).map(|i| hour(i, 0.0, 1000.0)).collect();
        assert!((outage_runtime_profile_h(&p, 20.0, &steps).unwrap() - 2.0).abs() < 1e-9);
        // Enough solar → lasts the whole horizon.
        let sunny: Vec<_> = (0..5).map(|i| hour(i, 1500.0, 1000.0)).collect();
        assert_eq!(outage_runtime_profile_h(&p, 20.0, &sunny), None);
    }

    #[test]
    fn capacity_and_cycles() {
        assert_eq!(capacity_from_stored(8400.0, 84.0), Some(10_000.0));
        assert_eq!(capacity_from_stored(100.0, 5.0), None);
        assert!((equivalent_cycles(25_000.0, 10_000.0) - 2.5).abs() < 1e-12);
        assert_eq!(
            observed_round_trip(10_000.0, 9_000.0, 50.0, 51.0),
            Some(0.9)
        );
        assert_eq!(observed_round_trip(10_000.0, 9_000.0, 50.0, 60.0), None);
    }
}
