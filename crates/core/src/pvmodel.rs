//! Physical PV power model: hourly forecast irradiance → plane-of-array
//! irradiance → cell temperature → DC → AC for roof arrays of
//! microinverters.
//!
//! Model chain (see [`array_output`]):
//!
//! 1. Horizon/obstruction shading of the beam ([`shading_factor`]).
//! 2. Transposition of GHI/DNI/DHI to the tilted plane with the Hay & Davies
//!    (1980) anisotropic sky model ([`poa_hay_davies`]).
//! 3. Angle-of-incidence reflection loss on the beam, ASHRAE model
//!    ([`iam_ashrae`]).
//! 4. Cell temperature, Faiman (2008) model as standardised in IEC 61853-2
//!    ([`cell_temp_faiman`]).
//! 5. PVWatts v5 DC model with linear temperature coefficient, lumped system
//!    losses and a constant inverter efficiency with AC clipping
//!    (Dobos 2014, NREL/TP-6A20-62641).
//!
//! Conventions: all values are `f64`; irradiance in W/m², temperatures in
//! °C, wind in m/s, angles in degrees. Azimuths are measured clockwise from
//! north (90 = east, 180 = south, 270 = west). Weather inputs from
//! Open-Meteo are means over the *preceding* hour; callers should evaluate
//! sun angles at the middle of that hour.
//!
//! Inputs are sanitised: non-finite or negative irradiance/wind are treated
//! as 0, so outputs are always finite and non-negative.

use serde::{Deserialize, Serialize};

/// Solar constant, W/m² (Kopp & Lean 2011, Geophys. Res. Lett. 38, L01706).
pub const SOLAR_CONSTANT: f64 = 1361.0;

/// Default ASHRAE incidence-angle-modifier coefficient `b0` (ASHRAE
/// Standard 93; Souka & Safwat 1966). Default estimate for glass-faced
/// modules; also used by PVsyst/pvlib.
pub const IAM_B0_DEFAULT: f64 = 0.05;

/// Faiman constant heat-loss factor `u0`, W/(m²·K). pvlib default
/// (Faiman 2008; IEC 61853-2). Default estimate, not fitted to the user's
/// modules.
pub const FAIMAN_U0: f64 = 25.0;

/// Faiman convective heat-loss factor `u1`, W·s/(m³·K). pvlib default
/// (Faiman 2008; IEC 61853-2). Default estimate.
pub const FAIMAN_U1: f64 = 6.84;

/// Irradiance at Standard Test Conditions, W/m² (IEC 61215).
pub const STC_IRRADIANCE: f64 = 1000.0;

/// Cell temperature at Standard Test Conditions, °C (IEC 61215).
pub const STC_CELL_TEMP_C: f64 = 25.0;

/// One roof array (a set of identically oriented modules, each on an
/// Enphase-style microinverter).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArraySpec {
    /// Display name.
    pub name: String,
    /// STC nameplate DC capacity (sum of module ratings), W.
    pub dc_capacity_w: f64,
    /// Surface tilt from horizontal, degrees (0 = flat, 90 = vertical).
    pub tilt_deg: f64,
    /// Surface azimuth, degrees clockwise from north (180 = south).
    pub azimuth_deg: f64,
    /// Sum of the microinverters' continuous AC ratings, W; the AC clip
    /// point. `None` = no clipping.
    pub ac_limit_w: Option<f64>,
    /// Power temperature coefficient γ, fraction per °C (e.g. -0.0035).
    /// Default estimate -0.0037 (PVWatts v5 "standard" module, Dobos 2014).
    pub temp_coeff_per_c: f64,
    /// Lumped DC/AC system losses (soiling, mismatch, wiring,
    /// connections, LID, nameplate, availability), fraction 0..1.
    /// Default estimate 0.14 (PVWatts v5, Dobos 2014, Table 1).
    pub system_losses: f64,
    /// Inverter conversion efficiency, fraction. Default estimate 0.97
    /// (typical CEC-weighted efficiency of current microinverters; PVWatts
    /// v5 nominal is 0.96).
    pub inverter_efficiency: f64,
    /// Ground reflectance. Default estimate 0.2 (Duffie & Beckman, typical
    /// non-snow ground).
    pub albedo: f64,
    /// Optional beam-shading rules (trees, chimneys, neighbouring houses).
    pub shading: Vec<ShadingRule>,
}

/// A rectangular obstruction in sun-position space: when the sun azimuth is
/// within `[azimuth_from_deg, azimuth_to_deg]` (wrapping through north if
/// `from > to`) and the solar elevation is below `below_elevation_deg`, the
/// beam irradiance is multiplied by `beam_factor` (0 = fully shaded,
/// 1 = no effect).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShadingRule {
    pub azimuth_from_deg: f64,
    pub azimuth_to_deg: f64,
    pub below_elevation_deg: f64,
    pub beam_factor: f64,
}

/// Sun position: apparent (refraction-corrected) zenith angle and azimuth
/// clockwise from north, degrees.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct SunAngles {
    pub zenith_deg: f64,
    pub azimuth_deg: f64,
}

/// Hourly weather: global horizontal, direct normal and diffuse horizontal
/// irradiance (W/m²), ambient air temperature (°C), wind speed (m/s).
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Weather {
    pub ghi: f64,
    pub dni: f64,
    pub dhi: f64,
    pub temp_air_c: f64,
    pub wind_ms: f64,
}

/// Plane-of-array irradiance components, W/m², before reflection (IAM)
/// losses, plus the angle of incidence of the beam, degrees.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct PoaComponents {
    pub beam: f64,
    pub sky_diffuse: f64,
    pub ground: f64,
    pub total: f64,
    pub aoi_deg: f64,
}

/// Result of the full model chain for one array.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct ArrayOutput {
    pub poa: PoaComponents,
    pub cell_temp_c: f64,
    /// DC power after system losses, W.
    pub dc_w: f64,
    /// AC power after inverter efficiency and clipping, W.
    pub ac_w: f64,
    /// True when the AC limit reduced the output.
    pub clipped: bool,
}

impl Default for ArraySpec {
    /// Generic defaults; `dc_capacity_w` is 0 so that validation fails until
    /// the user enters their real capacity.
    fn default() -> Self {
        Self {
            name: "Array 1".to_string(),
            dc_capacity_w: 0.0,
            tilt_deg: 20.0,
            azimuth_deg: 180.0,
            ac_limit_w: None,
            temp_coeff_per_c: -0.0037,
            system_losses: 0.14,
            inverter_efficiency: 0.97,
            albedo: 0.2,
            shading: Vec::new(),
        }
    }
}

fn check_range(name: &str, v: f64, lo: f64, hi: f64) -> Result<(), String> {
    if v.is_finite() && v >= lo && v <= hi {
        Ok(())
    } else {
        Err(format!("{name} must be between {lo} and {hi} (got {v})"))
    }
}

impl ArraySpec {
    /// Check that every parameter is finite and physically plausible.
    pub fn validate(&self) -> Result<(), String> {
        let c = self.dc_capacity_w;
        if !(c.is_finite() && c > 0.0 && c < 1e6) {
            return Err(format!(
                "dc_capacity_w must be > 0 and < 1000000 W (got {c})"
            ));
        }
        check_range("tilt_deg", self.tilt_deg, 0.0, 90.0)?;
        check_range("azimuth_deg", self.azimuth_deg, 0.0, 360.0)?;
        if let Some(l) = self.ac_limit_w
            && !(l.is_finite() && l > 0.0)
        {
            return Err(format!("ac_limit_w must be > 0 (got {l})"));
        }
        check_range("temp_coeff_per_c", self.temp_coeff_per_c, -0.01, 0.0)?;
        check_range("system_losses", self.system_losses, 0.0, 0.5)?;
        check_range("inverter_efficiency", self.inverter_efficiency, 0.8, 1.0)?;
        check_range("albedo", self.albedo, 0.0, 1.0)?;
        for (i, r) in self.shading.iter().enumerate() {
            check_range(
                &format!("shading[{i}].azimuth_from_deg"),
                r.azimuth_from_deg,
                0.0,
                360.0,
            )?;
            check_range(
                &format!("shading[{i}].azimuth_to_deg"),
                r.azimuth_to_deg,
                0.0,
                360.0,
            )?;
            check_range(
                &format!("shading[{i}].below_elevation_deg"),
                r.below_elevation_deg,
                0.0,
                90.0,
            )?;
            check_range(
                &format!("shading[{i}].beam_factor"),
                r.beam_factor,
                0.0,
                1.0,
            )?;
        }
        Ok(())
    }
}

/// Non-finite or negative → 0.
fn nonneg(x: f64) -> f64 {
    if x.is_finite() && x > 0.0 { x } else { 0.0 }
}

/// Non-finite zenith → treat as sun below horizon.
fn safe_zenith(z: f64) -> f64 {
    if z.is_finite() { z } else { 90.0 }
}

/// Non-finite temperature → STC cell temperature (neutral for derating).
fn safe_temp(t: f64) -> f64 {
    if t.is_finite() { t } else { STC_CELL_TEMP_C }
}

/// Extraterrestrial irradiance on a plane normal to the sun, W/m².
///
/// `G_on = G_sc · (1 + 0.033 · cos(360° · n / 365))`, Duffie & Beckman,
/// *Solar Engineering of Thermal Processes*, 4th ed., Eq. 1.4.1a, with
/// `G_sc` = 1361 W/m² (Kopp & Lean 2011). Accuracy ≈ ±0.1 % against the
/// Spencer (1971) series (D&B Eq. 1.4.1b), ample for the anisotropy index.
/// `day_of_year` is clamped to 1..=366.
pub fn extraterrestrial_normal(day_of_year: u32) -> f64 {
    let n = day_of_year.clamp(1, 366) as f64;
    SOLAR_CONSTANT * (1.0 + 0.033 * (2.0 * std::f64::consts::PI * n / 365.0).cos())
}

/// Cosine of the angle of incidence of beam radiation on a tilted surface
/// (Duffie & Beckman Eq. 1.6.3 in zenith/azimuth form):
/// `cos θ = cos Z cos β + sin Z sin β cos(γs − γ)`.
pub fn cos_aoi(zenith_deg: f64, sun_az_deg: f64, tilt_deg: f64, surface_az_deg: f64) -> f64 {
    let z = zenith_deg.to_radians();
    let b = tilt_deg.to_radians();
    let d = (sun_az_deg - surface_az_deg).to_radians();
    (z.cos() * b.cos() + z.sin() * b.sin() * d.cos()).clamp(-1.0, 1.0)
}

/// Plane-of-array irradiance by the Hay & Davies (1980) anisotropic sky
/// model (Hay, J.E. & Davies, J.A., "Calculation of the solar radiation
/// incident on an inclined surface", Proc. 1st Canadian Solar Radiation
/// Data Workshop; as given in Duffie & Beckman §2.16):
///
/// * beam: `DNI · max(cos θ, 0) · s` where `s` = `shading_beam_factor`;
/// * anisotropy index `A = DNI / G_on` ([`extraterrestrial_normal`]);
/// * `R_b = max(cos θ, 0) / max(cos Z, cos 85°)` (lower bound on cos Z as in
///   pvlib, to avoid blow-up near the horizon);
/// * sky diffuse: `DHI · [A · R_b · s + (1 − A)(1 + cos β)/2]` — the
///   circumsolar part is shaded with the beam because it originates next to
///   the solar disc;
/// * ground-reflected (isotropic): `GHI · ρ · (1 − cos β)/2`.
///
/// When the sun is at or below the horizon (Z ≥ 90°) beam and circumsolar
/// are 0; isotropic diffuse is still counted if DHI > 0 (realistically 0).
pub fn poa_hay_davies(
    sun: SunAngles,
    w: &Weather,
    tilt_deg: f64,
    surface_az_deg: f64,
    albedo: f64,
    day_of_year: u32,
    shading_beam_factor: f64,
) -> PoaComponents {
    let ghi = nonneg(w.ghi);
    let dni = nonneg(w.dni);
    let dhi = nonneg(w.dhi);
    let rho = nonneg(albedo).min(1.0);
    let s = if shading_beam_factor.is_finite() {
        shading_beam_factor.clamp(0.0, 1.0)
    } else {
        1.0
    };
    let tilt = if tilt_deg.is_finite() {
        tilt_deg.clamp(0.0, 180.0)
    } else {
        0.0
    };
    let surf_az = if surface_az_deg.is_finite() {
        surface_az_deg
    } else {
        180.0
    };
    let zen = safe_zenith(sun.zenith_deg);
    let sun_az = if sun.azimuth_deg.is_finite() {
        sun.azimuth_deg
    } else {
        180.0
    };

    let cos_b = tilt.to_radians().cos();
    let sun_up = zen < 90.0;
    let ct = cos_aoi(zen, sun_az, tilt, surf_az);
    let aoi_deg = ct.acos().to_degrees();

    let (beam, circumsolar, a) = if sun_up {
        let cos_z = zen.to_radians().cos().max(85f64.to_radians().cos());
        let rb = ct.max(0.0) / cos_z;
        let a = (dni / extraterrestrial_normal(day_of_year)).clamp(0.0, 1.0);
        (dni * ct.max(0.0) * s, dhi * a * rb * s, a)
    } else {
        (0.0, 0.0, 0.0)
    };
    let isotropic = dhi * (1.0 - a) * (1.0 + cos_b) / 2.0;
    let sky_diffuse = circumsolar + isotropic;
    let ground = ghi * rho * (1.0 - cos_b) / 2.0;
    PoaComponents {
        beam,
        sky_diffuse,
        ground,
        total: beam + sky_diffuse + ground,
        aoi_deg,
    }
}

/// ASHRAE incidence angle modifier (ASHRAE Standard 93-77; Souka &
/// Safwat 1966): `IAM = 1 − b0 · (1/cos θ − 1)`, clamped to ≥ 0, and 0 for
/// θ ≥ 90°. Typical `b0` = [`IAM_B0_DEFAULT`] (0.05).
pub fn iam_ashrae(aoi_deg: f64, b0: f64) -> f64 {
    if !aoi_deg.is_finite() || aoi_deg.abs() >= 90.0 {
        return 0.0;
    }
    let c = aoi_deg.to_radians().cos();
    if c <= 0.0 {
        return 0.0;
    }
    (1.0 - b0 * (1.0 / c - 1.0)).clamp(0.0, 1.0)
}

/// Effective irradiance reaching the cells, W/m²: IAM applied to the beam
/// only (diffuse and ground IAM losses are small and neglected, as in
/// PVWatts v5, Dobos 2014 §6).
pub fn effective_irradiance(poa: &PoaComponents, b0: f64) -> f64 {
    nonneg(poa.beam * iam_ashrae(poa.aoi_deg, b0) + poa.sky_diffuse + poa.ground)
}

/// Cell temperature by the Faiman (2008) model (Faiman, D., "Assessing the
/// outdoor operating temperature of photovoltaic modules", Prog.
/// Photovolt. 16:307–315; adopted in IEC 61853-2):
/// `T_c = T_a + G_poa / (u0 + u1 · v)` with `u0` = 25.0, `u1` = 6.84 (pvlib
/// defaults; default estimate). Negative/non-finite POA or wind → 0.
pub fn cell_temp_faiman(poa: f64, temp_air_c: f64, wind_ms: f64) -> f64 {
    safe_temp(temp_air_c) + nonneg(poa) / (FAIMAN_U0 + FAIMAN_U1 * nonneg(wind_ms))
}

/// PVWatts v5 DC power before system losses, W (Dobos 2014, Eq. 6, ignoring
/// the low-light correction):
/// `P_dc = P_dc0 · (G_eff/1000) · (1 + γ (T_c − 25))`, never negative.
pub fn dc_power_w(dc_capacity_w: f64, g_eff: f64, cell_temp_c: f64, temp_coeff_per_c: f64) -> f64 {
    let gamma = if temp_coeff_per_c.is_finite() {
        temp_coeff_per_c
    } else {
        0.0
    };
    nonneg(
        nonneg(dc_capacity_w)
            * (nonneg(g_eff) / STC_IRRADIANCE)
            * (1.0 + gamma * (safe_temp(cell_temp_c) - STC_CELL_TEMP_C)),
    )
}

fn az_in_range(az: f64, from: f64, to: f64) -> bool {
    let n = |x: f64| x.rem_euclid(360.0);
    let (az, from, to) = (n(az), n(from), n(to));
    if from <= to {
        az >= from && az <= to
    } else {
        az >= from || az <= to
    }
}

/// Beam shading multiplier from user rules: the minimum `beam_factor` of all
/// rules that match the current sun position (overlapping rules do not
/// compound), clamped to 0..1. 1 when no rule matches or the sun is down.
pub fn shading_factor(rules: &[ShadingRule], sun: SunAngles) -> f64 {
    let zen = safe_zenith(sun.zenith_deg);
    if zen >= 90.0 || !sun.azimuth_deg.is_finite() {
        return 1.0;
    }
    let elev = 90.0 - zen;
    rules
        .iter()
        .filter(|r| {
            r.beam_factor.is_finite()
                && elev < r.below_elevation_deg
                && az_in_range(sun.azimuth_deg, r.azimuth_from_deg, r.azimuth_to_deg)
        })
        .map(|r| r.beam_factor.clamp(0.0, 1.0))
        .fold(1.0, f64::min)
}

/// Full model chain for one array: shading → Hay–Davies POA → ASHRAE IAM on
/// beam → Faiman cell temperature (from total POA) → PVWatts DC →
/// `× (1 − system_losses)` → `× inverter_efficiency` → clip at
/// `ac_limit_w`. Outputs are never negative.
pub fn array_output(
    spec: &ArraySpec,
    sun: SunAngles,
    w: &Weather,
    day_of_year: u32,
) -> ArrayOutput {
    let s = shading_factor(&spec.shading, sun);
    let poa = poa_hay_davies(
        sun,
        w,
        spec.tilt_deg,
        spec.azimuth_deg,
        spec.albedo,
        day_of_year,
        s,
    );
    let g_eff = effective_irradiance(&poa, IAM_B0_DEFAULT);
    let cell_temp_c = cell_temp_faiman(poa.total, w.temp_air_c, w.wind_ms);
    let losses = nonneg(spec.system_losses).min(1.0);
    let dc_w = dc_power_w(
        spec.dc_capacity_w,
        g_eff,
        cell_temp_c,
        spec.temp_coeff_per_c,
    ) * (1.0 - losses);
    let ac_unclipped = dc_w * nonneg(spec.inverter_efficiency).min(1.0);
    let (ac_w, clipped) = match spec.ac_limit_w {
        Some(lim) if lim.is_finite() && ac_unclipped > lim => (nonneg(lim), true),
        _ => (ac_unclipped, false),
    };
    ArrayOutput {
        poa,
        cell_temp_c,
        dc_w,
        ac_w,
        clipped,
    }
}

/// Total AC power of several arrays, W.
pub fn system_ac_w(specs: &[ArraySpec], sun: SunAngles, w: &Weather, day_of_year: u32) -> f64 {
    specs
        .iter()
        .map(|s| array_output(s, sun, w, day_of_year).ac_w)
        .sum()
}

/// Clear-sky GHI by the Haurwitz (1945) model (Haurwitz, B., "Insolation in
/// relation to cloudiness and cloud density", J. Meteorology 2:154–166):
/// `GHI_cs = 1098 · cos Z · exp(−0.059 / cos Z)`, 0 when Z ≥ 90°.
pub fn clear_sky_ghi_haurwitz(zenith_deg: f64) -> f64 {
    let zen = safe_zenith(zenith_deg);
    if zen >= 90.0 {
        return 0.0;
    }
    let c = zen.to_radians().cos();
    if c <= 0.0 {
        return 0.0;
    }
    1098.0 * c * (-0.059 / c).exp()
}

/// Clear-sky index `k_c = GHI / GHI_cs` (Haurwitz clear sky), clamped to
/// 0..1.5 (values > 1 occur with cloud enhancement). `None` when the
/// clear-sky GHI is below 20 W/m² (sun too low for a meaningful ratio).
pub fn clear_sky_index(ghi: f64, zenith_deg: f64) -> Option<f64> {
    let cs = clear_sky_ghi_haurwitz(zenith_deg);
    if cs < 20.0 {
        None
    } else {
        Some((nonneg(ghi) / cs).clamp(0.0, 1.5))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() <= tol
    }

    fn spec(dc: f64) -> ArraySpec {
        ArraySpec {
            dc_capacity_w: dc,
            ..ArraySpec::default()
        }
    }

    /// Sun exactly normal to a 30°-tilt, south-facing surface.
    fn normal_sun() -> SunAngles {
        SunAngles {
            zenith_deg: 30.0,
            azimuth_deg: 180.0,
        }
    }

    fn beam_only(dni: f64, temp_air_c: f64) -> Weather {
        Weather {
            ghi: dni * 30f64.to_radians().cos(),
            dni,
            dhi: 0.0,
            temp_air_c,
            wind_ms: 1.0,
        }
    }

    #[test]
    fn horizontal_poa_equals_ghi_closure() {
        for &(z, saz, dni, dhi) in &[
            (30.0, 150.0, 800.0, 100.0),
            (60.0, 250.0, 400.0, 200.0),
            (10.0, 180.0, 900.0, 60.0),
            (80.0, 90.0, 100.0, 50.0),
        ] {
            let ghi = dni * f64::to_radians(z).cos() + dhi;
            let w = Weather {
                ghi,
                dni,
                dhi,
                temp_air_c: 20.0,
                wind_ms: 2.0,
            };
            let sun = SunAngles {
                zenith_deg: z,
                azimuth_deg: saz,
            };
            let p = poa_hay_davies(sun, &w, 0.0, 180.0, 0.2, 172, 1.0);
            assert_eq!(p.ground, 0.0);
            assert!(
                close(p.total, ghi, ghi * 0.01),
                "z={z}: {} vs {ghi}",
                p.total
            );
            assert!(close(p.aoi_deg, z, 1e-9));
        }
    }

    #[test]
    fn normal_incidence_beam_equals_dni() {
        let w = beam_only(900.0, 20.0);
        let p = poa_hay_davies(normal_sun(), &w, 30.0, 180.0, 0.0, 100, 1.0);
        assert!(close(p.beam, 900.0, 1e-9));
        assert!(close(p.aoi_deg, 0.0, 1e-6));
        assert_eq!(p.sky_diffuse, 0.0);
        assert_eq!(p.ground, 0.0);
        assert!(close(iam_ashrae(p.aoi_deg, IAM_B0_DEFAULT), 1.0, 1e-12));
    }

    #[test]
    fn tilted_surface_gets_ground_and_less_isotropic_diffuse() {
        // Overcast: DNI 0 → A = 0, purely isotropic. Vertical surface sees
        // half the sky dome and half the ground.
        let w = Weather {
            ghi: 200.0,
            dni: 0.0,
            dhi: 200.0,
            temp_air_c: 10.0,
            wind_ms: 0.0,
        };
        let p = poa_hay_davies(normal_sun(), &w, 90.0, 180.0, 0.2, 1, 1.0);
        assert!(close(p.sky_diffuse, 100.0, 1e-9));
        assert!(close(p.ground, 200.0 * 0.2 * 0.5, 1e-9));
    }

    #[test]
    fn sun_below_horizon_no_beam() {
        let w = Weather {
            ghi: 0.0,
            dni: 500.0,
            dhi: 0.0,
            temp_air_c: 5.0,
            wind_ms: 0.0,
        };
        let sun = SunAngles {
            zenith_deg: 95.0,
            azimuth_deg: 180.0,
        };
        let o = array_output(&spec(5000.0), sun, &w, 10);
        assert_eq!(o.poa.beam, 0.0);
        assert_eq!(o.ac_w, 0.0);
    }

    #[test]
    fn negative_and_nan_inputs_are_safe() {
        let w = Weather {
            ghi: -5.0,
            dni: f64::NAN,
            dhi: -1.0,
            temp_air_c: f64::NAN,
            wind_ms: -3.0,
        };
        let o = array_output(&spec(5000.0), normal_sun(), &w, 10);
        assert_eq!(o.ac_w, 0.0);
        assert!(o.cell_temp_c.is_finite());
        let o = array_output(
            &spec(5000.0),
            SunAngles {
                zenith_deg: f64::NAN,
                azimuth_deg: f64::NAN,
            },
            &beam_only(800.0, 20.0),
            10,
        );
        assert!(o.ac_w.is_finite() && o.ac_w >= 0.0);
    }

    #[test]
    fn iam_values() {
        assert!(close(iam_ashrae(0.0, 0.05), 1.0, 1e-12));
        assert!(close(iam_ashrae(60.0, 0.05), 0.95, 1e-9));
        assert_eq!(iam_ashrae(90.0, 0.05), 0.0);
        assert_eq!(iam_ashrae(120.0, 0.05), 0.0);
        // Clamped at 0 near grazing.
        assert_eq!(iam_ashrae(89.9, 0.05), 0.0);
        // Monotonic decrease.
        assert!(iam_ashrae(70.0, 0.05) < iam_ashrae(50.0, 0.05));
    }

    #[test]
    fn faiman_values() {
        assert!(close(cell_temp_faiman(0.0, 17.0, 3.0), 17.0, 1e-12));
        let t = cell_temp_faiman(1000.0, 20.0, 1.0);
        assert!(close(t, 20.0 + 1000.0 / 31.84, 1e-9));
        assert!(close(t - 20.0, 31.4, 0.05));
        // More wind cools.
        assert!(cell_temp_faiman(1000.0, 20.0, 5.0) < t);
    }

    #[test]
    fn stc_dc_and_clipping() {
        // Choose Tair so Faiman gives exactly 25 °C at 1000 W/m², wind 1.
        let ta = STC_CELL_TEMP_C - 1000.0 / (FAIMAN_U0 + FAIMAN_U1);
        let w = beam_only(1000.0, ta);
        let mut s = spec(4000.0);
        s.tilt_deg = 30.0;
        s.albedo = 0.0;
        let o = array_output(&s, normal_sun(), &w, 100);
        assert!(close(o.poa.total, 1000.0, 1e-9));
        assert!(close(o.cell_temp_c, 25.0, 1e-9));
        assert!(close(o.dc_w, 4000.0 * (1.0 - 0.14), 1e-6));
        assert!(close(o.ac_w, o.dc_w * 0.97, 1e-6));
        assert!(!o.clipped);

        s.ac_limit_w = Some(3000.0);
        let o = array_output(&s, normal_sun(), &w, 100);
        assert!(o.clipped);
        assert!(close(o.ac_w, 3000.0, 1e-9));

        // Direct DC function at STC.
        assert!(close(
            dc_power_w(4000.0, 1000.0, 25.0, -0.0037),
            4000.0,
            1e-9
        ));
    }

    #[test]
    fn hotter_means_less_power() {
        let s = spec(4000.0);
        let cool = array_output(&s, normal_sun(), &beam_only(800.0, 0.0), 100);
        let hot = array_output(&s, normal_sun(), &beam_only(800.0, 35.0), 100);
        assert!(hot.cell_temp_c > cool.cell_temp_c);
        assert!(hot.ac_w < cool.ac_w);
        // Ratio follows the linear coefficient.
        let expected =
            (1.0 - 0.0037 * (hot.cell_temp_c - 25.0)) / (1.0 - 0.0037 * (cool.cell_temp_c - 25.0));
        assert!(close(hot.dc_w / cool.dc_w, expected, 1e-9));
    }

    #[test]
    fn extraterrestrial_perihelion_aphelion() {
        let jan = extraterrestrial_normal(3);
        let jul = extraterrestrial_normal(185);
        assert!((1405.0..=1415.0).contains(&jan), "{jan}");
        assert!((1316.0..=1325.0).contains(&jul), "{jul}");
    }

    #[test]
    fn haurwitz_values() {
        let z0 = clear_sky_ghi_haurwitz(0.0);
        assert!(close(z0, 1098.0 * (-0.059f64).exp(), 1e-9));
        assert!(close(z0, 1035.1, 0.1));
        assert_eq!(clear_sky_ghi_haurwitz(90.0), 0.0);
        assert_eq!(clear_sky_ghi_haurwitz(100.0), 0.0);
        assert!(clear_sky_ghi_haurwitz(60.0) < z0);
    }

    #[test]
    fn clear_sky_index_bounds() {
        assert_eq!(clear_sky_index(10.0, 89.5), None);
        let cs = clear_sky_ghi_haurwitz(30.0);
        assert!(close(clear_sky_index(cs / 2.0, 30.0).unwrap(), 0.5, 1e-12));
        assert_eq!(clear_sky_index(cs * 3.0, 30.0), Some(1.5));
        assert_eq!(clear_sky_index(-10.0, 30.0), Some(0.0));
    }

    #[test]
    fn shading_rules() {
        let rules = vec![
            ShadingRule {
                azimuth_from_deg: 200.0,
                azimuth_to_deg: 260.0,
                below_elevation_deg: 25.0,
                beam_factor: 0.0,
            },
            ShadingRule {
                azimuth_from_deg: 340.0,
                azimuth_to_deg: 20.0,
                below_elevation_deg: 10.0,
                beam_factor: 0.5,
            },
        ];
        let sun = |el: f64, az: f64| SunAngles {
            zenith_deg: 90.0 - el,
            azimuth_deg: az,
        };
        assert_eq!(shading_factor(&rules, sun(15.0, 230.0)), 0.0);
        assert_eq!(shading_factor(&rules, sun(30.0, 230.0)), 1.0);
        assert_eq!(shading_factor(&rules, sun(15.0, 180.0)), 1.0);
        assert_eq!(shading_factor(&rules, sun(5.0, 355.0)), 0.5); // wraps through north
        assert_eq!(shading_factor(&rules, sun(5.0, 10.0)), 0.5);
        assert_eq!(shading_factor(&[], sun(5.0, 10.0)), 1.0);

        // Fully shaded beam removes beam POA in the chain.
        let mut s = spec(4000.0);
        s.tilt_deg = 30.0;
        s.shading = vec![ShadingRule {
            azimuth_from_deg: 170.0,
            azimuth_to_deg: 190.0,
            below_elevation_deg: 70.0,
            beam_factor: 0.0,
        }];
        let o = array_output(&s, normal_sun(), &beam_only(900.0, 20.0), 100);
        assert_eq!(o.poa.beam, 0.0);
    }

    #[test]
    fn validation() {
        assert!(ArraySpec::default().validate().is_err());
        let good = spec(5000.0);
        assert!(good.validate().is_ok());
        let bad: Vec<ArraySpec> = vec![
            ArraySpec {
                dc_capacity_w: f64::NAN,
                ..good.clone()
            },
            ArraySpec {
                dc_capacity_w: 2e6,
                ..good.clone()
            },
            ArraySpec {
                tilt_deg: 95.0,
                ..good.clone()
            },
            ArraySpec {
                azimuth_deg: -1.0,
                ..good.clone()
            },
            ArraySpec {
                ac_limit_w: Some(0.0),
                ..good.clone()
            },
            ArraySpec {
                temp_coeff_per_c: 0.004,
                ..good.clone()
            },
            ArraySpec {
                temp_coeff_per_c: -0.02,
                ..good.clone()
            },
            ArraySpec {
                system_losses: 0.6,
                ..good.clone()
            },
            ArraySpec {
                inverter_efficiency: 0.5,
                ..good.clone()
            },
            ArraySpec {
                albedo: 1.5,
                ..good.clone()
            },
            ArraySpec {
                shading: vec![ShadingRule {
                    azimuth_from_deg: 0.0,
                    azimuth_to_deg: 90.0,
                    below_elevation_deg: 10.0,
                    beam_factor: 2.0,
                }],
                ..good.clone()
            },
        ];
        for b in bad {
            assert!(b.validate().is_err(), "{b:?}");
        }
    }

    #[test]
    fn system_sums_arrays() {
        let mut a = spec(3000.0);
        a.name = "South".into();
        a.tilt_deg = 30.0;
        let mut b = spec(2000.0);
        b.name = "West".into();
        b.azimuth_deg = 270.0;
        let w = Weather {
            ghi: 700.0,
            dni: 600.0,
            dhi: 180.0,
            temp_air_c: 22.0,
            wind_ms: 2.0,
        };
        let sun = SunAngles {
            zenith_deg: 40.0,
            azimuth_deg: 200.0,
        };
        let total = system_ac_w(&[a.clone(), b.clone()], sun, &w, 150);
        let sum = array_output(&a, sun, &w, 150).ac_w + array_output(&b, sun, &w, 150).ac_w;
        assert!(total > 0.0);
        assert!(close(total, sum, 1e-9));
        assert_eq!(system_ac_w(&[], sun, &w, 150), 0.0);
    }
}
