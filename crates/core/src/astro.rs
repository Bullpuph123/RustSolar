//! Local astronomical calculations (no network access).
//!
//! Solar position and sun event times follow the NOAA Global Monitoring
//! Laboratory solar calculator spreadsheet algorithm
//! (<https://gml.noaa.gov/grad/solcalc/calcdetails.html>), which is based on
//! Jean Meeus, *Astronomical Algorithms* (2nd ed., Willmann-Bell, 1998).
//!
//! Conventions: all angles in the public API are degrees, longitudes are
//! east-positive, azimuths are measured clockwise from true north, and all
//! instants are UTC (`DateTime<Utc>`).

use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveTime, TimeZone, Utc};
use chrono_tz::Tz;
use serde::Serialize;

/// Altitude of the sun's centre at sunrise/sunset: -0.833° accounts for
/// standard atmospheric refraction (~34′) plus the solar semidiameter (~16′).
pub const SUNRISE_ALTITUDE_DEG: f64 = -0.833;
/// Altitude for civil twilight.
pub const CIVIL_ALTITUDE_DEG: f64 = -6.0;
/// Altitude for nautical twilight.
pub const NAUTICAL_ALTITUDE_DEG: f64 = -12.0;
/// Altitude for astronomical twilight.
pub const ASTRONOMICAL_ALTITUDE_DEG: f64 = -18.0;

/// Apparent position of the sun as seen from a point on the Earth.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct SolarPosition {
    /// Apparent elevation (with NOAA atmospheric refraction correction), degrees.
    pub elevation_deg: f64,
    /// Apparent zenith angle, `90 - elevation_deg`.
    pub zenith_deg: f64,
    /// Azimuth clockwise from true north, `0..360`.
    pub azimuth_deg: f64,
    /// Solar declination, degrees.
    pub declination_deg: f64,
    /// Equation of time, minutes (apparent minus mean solar time).
    pub equation_of_time_min: f64,
}

/// Polar day / night indicator for days without sunrise or sunset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Polar {
    /// The sun stays above the horizon all day.
    Day,
    /// The sun stays below the horizon all day.
    Night,
}

/// Sun event times for one local calendar date.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SunTimes {
    /// Local calendar date the times belong to.
    pub date: NaiveDate,
    pub solar_noon: DateTime<Utc>,
    /// `None` in polar day/night.
    pub sunrise: Option<DateTime<Utc>>,
    pub sunset: Option<DateTime<Utc>>,
    /// Sun at -6°.
    pub civil_dawn: Option<DateTime<Utc>>,
    pub civil_dusk: Option<DateTime<Utc>>,
    /// Sun at -12°.
    pub nautical_dawn: Option<DateTime<Utc>>,
    pub nautical_dusk: Option<DateTime<Utc>>,
    /// Sun at -18°.
    pub astronomical_dawn: Option<DateTime<Utc>>,
    pub astronomical_dusk: Option<DateTime<Utc>>,
    /// `sunset - sunrise` in seconds; 0 in polar night, 86400 in polar day.
    pub day_length_s: f64,
    /// Apparent solar elevation at solar noon, degrees.
    pub noon_elevation_deg: f64,
    /// `Some` when there is no sunrise/sunset on this date.
    pub polar: Option<Polar>,
}

/// Intermediate solar quantities that depend only on time.
#[derive(Debug, Clone, Copy)]
struct SunCoords {
    declination_deg: f64,
    equation_of_time_min: f64,
}

fn sin_d(x: f64) -> f64 {
    x.to_radians().sin()
}
fn cos_d(x: f64) -> f64 {
    x.to_radians().cos()
}
fn tan_d(x: f64) -> f64 {
    x.to_radians().tan()
}

/// Julian Day of a UTC instant (Meeus ch. 7; Unix epoch = JD 2440587.5).
pub fn julian_day(t: DateTime<Utc>) -> f64 {
    let secs = t.timestamp() as f64 + f64::from(t.timestamp_subsec_nanos()) * 1e-9;
    secs / 86_400.0 + 2_440_587.5
}

/// Declination and equation of time at instant `t`.
///
/// Source: NOAA GML solar calculator spreadsheet, columns G–V
/// (geometric mean longitude/anomaly, eccentricity, equation of centre,
/// apparent longitude, corrected obliquity, declination, equation of time),
/// after Meeus, *Astronomical Algorithms*, ch. 25 and 28.
fn sun_coords(t: DateTime<Utc>) -> SunCoords {
    let jc = (julian_day(t) - 2_451_545.0) / 36_525.0;
    let l0 = (280.46646 + jc * (36000.76983 + jc * 0.0003032)).rem_euclid(360.0);
    let m = 357.52911 + jc * (35999.05029 - 0.0001537 * jc);
    let e = 0.016708634 - jc * (0.000042037 + 0.0000001267 * jc);
    let c = sin_d(m) * (1.914602 - jc * (0.004817 + 0.000014 * jc))
        + sin_d(2.0 * m) * (0.019993 - 0.000101 * jc)
        + sin_d(3.0 * m) * 0.000289;
    let true_long = l0 + c;
    let omega = 125.04 - 1934.136 * jc;
    let app_long = true_long - 0.00569 - 0.00478 * sin_d(omega);
    let mean_obliq =
        23.0 + (26.0 + (21.448 - jc * (46.815 + jc * (0.00059 - jc * 0.001813))) / 60.0) / 60.0;
    let obliq = mean_obliq + 0.00256 * cos_d(omega);
    let declination_deg = (sin_d(obliq) * sin_d(app_long)).asin().to_degrees();
    let y = tan_d(obliq / 2.0).powi(2);
    let eot_rad = y * sin_d(2.0 * l0) - 2.0 * e * sin_d(m)
        + 4.0 * e * y * sin_d(m) * cos_d(2.0 * l0)
        - 0.5 * y * y * sin_d(4.0 * l0)
        - 1.25 * e * e * sin_d(2.0 * m);
    SunCoords {
        declination_deg,
        equation_of_time_min: 4.0 * eot_rad.to_degrees(),
    }
}

/// Hour angle (degrees, normalised to `[-180, 180)`) of the sun at `t`.
///
/// Source: NOAA GML solar calculator spreadsheet, columns AB (true solar
/// time = UTC minutes + EoT + 4·lon) and AC (hour angle = TST/4 − 180).
fn hour_angle_deg(t: DateTime<Utc>, lon_deg: f64, eot_min: f64) -> f64 {
    let midnight = t.date_naive().and_time(NaiveTime::MIN).and_utc();
    let utc_min = (t - midnight).num_milliseconds() as f64 / 60_000.0;
    let tst = utc_min + eot_min + 4.0 * lon_deg;
    (tst / 4.0 - 180.0 + 180.0).rem_euclid(360.0) - 180.0
}

/// Atmospheric refraction correction (degrees) to add to a geometric
/// elevation `elev_deg`.
///
/// Source: NOAA GML solar calculator spreadsheet, column AF (piecewise
/// approximation after Saemundsson / Bennett as used by NOAA).
pub fn refraction_correction_deg(elev_deg: f64) -> f64 {
    let arcsec = if elev_deg > 85.0 {
        0.0
    } else if elev_deg > 5.0 {
        let t = tan_d(elev_deg);
        58.1 / t - 0.07 / t.powi(3) + 0.000086 / t.powi(5)
    } else if elev_deg > -0.575 {
        let e = elev_deg;
        1735.0 + e * (-518.2 + e * (103.4 + e * (-12.79 + e * 0.711)))
    } else {
        -20.772 / tan_d(elev_deg)
    };
    arcsec / 3600.0
}

/// Apparent solar position at UTC instant `t` for latitude `lat_deg` and
/// east-positive longitude `lon_deg`.
///
/// Source: NOAA GML solar calculator spreadsheet algorithm
/// (<https://gml.noaa.gov/grad/solcalc/calcdetails.html>), columns G–AH,
/// based on Meeus, *Astronomical Algorithms*. Azimuth uses the equivalent
/// `atan2` form of the NOAA azimuth formula. Accuracy is roughly ±0.01–0.05°
/// for dates within a few centuries of 2000 and latitudes within ±72°.
pub fn solar_position(t: DateTime<Utc>, lat_deg: f64, lon_deg: f64) -> SolarPosition {
    let sc = sun_coords(t);
    let ha = hour_angle_deg(t, lon_deg, sc.equation_of_time_min);
    let decl = sc.declination_deg;
    let cos_zen =
        (sin_d(lat_deg) * sin_d(decl) + cos_d(lat_deg) * cos_d(decl) * cos_d(ha)).clamp(-1.0, 1.0);
    let geom_zenith = cos_zen.acos().to_degrees();
    let geom_elev = 90.0 - geom_zenith;
    // Azimuth measured from south (westward positive), then shifted to north-based.
    let az_south = sin_d(ha).atan2(cos_d(ha) * sin_d(lat_deg) - tan_d(decl) * cos_d(lat_deg));
    let azimuth_deg = (az_south.to_degrees() + 180.0).rem_euclid(360.0);
    let elevation_deg = geom_elev + refraction_correction_deg(geom_elev);
    SolarPosition {
        elevation_deg,
        zenith_deg: 90.0 - elevation_deg,
        azimuth_deg,
        declination_deg: decl,
        equation_of_time_min: sc.equation_of_time_min,
    }
}

fn add_seconds(t: DateTime<Utc>, s: f64) -> DateTime<Utc> {
    t + Duration::milliseconds((s * 1000.0).round() as i64)
}

/// Solar transit (solar noon) nearest to `guess`.
///
/// Source: NOAA spreadsheet column X (solar noon = 720 − 4·lon − EoT, in
/// minutes), solved iteratively so EoT is evaluated at the transit itself.
fn solar_noon_near(guess: DateTime<Utc>, lon_deg: f64) -> DateTime<Utc> {
    let mut t = guess;
    for _ in 0..4 {
        let sc = sun_coords(t);
        let ha = hour_angle_deg(t, lon_deg, sc.equation_of_time_min);
        t = add_seconds(t, -ha * 240.0);
    }
    t
}

/// Time at which the sun's centre reaches geometric altitude `alt_deg`
/// on the morning (`rising = true`) or evening side of transit `noon`.
///
/// Source: NOAA spreadsheet column W (sunrise hour angle
/// `acos(cos(90°−alt)/(cos φ cos δ) − tan φ tan δ)`) and columns Y/Z
/// (event = noon ∓ 4·HA minutes), iterated three times with declination
/// and EoT recomputed at the event time. Returns `None` if the sun never
/// reaches that altitude.
fn event_time(
    noon: DateTime<Utc>,
    lat_deg: f64,
    lon_deg: f64,
    alt_deg: f64,
    rising: bool,
) -> Option<DateTime<Utc>> {
    let sign = if rising { -1.0 } else { 1.0 };
    let mut t = noon;
    for _ in 0..4 {
        let sc = sun_coords(t);
        let decl = sc.declination_deg;
        let arg = (sin_d(alt_deg) - sin_d(lat_deg) * sin_d(decl)) / (cos_d(lat_deg) * cos_d(decl));
        if !(-1.0..=1.0).contains(&arg) || !arg.is_finite() {
            return None;
        }
        let target = sign * arg.acos().to_degrees();
        let ha = hour_angle_deg(t, lon_deg, sc.equation_of_time_min);
        let mut diff = target - ha;
        // keep on the correct side of the transit
        diff = (diff + 180.0).rem_euclid(360.0) - 180.0;
        t = add_seconds(t, diff * 240.0);
    }
    Some(t)
}

/// Sun event times for the local calendar date `date` in time zone `tz`.
///
/// "Local date" means the calendar date in `tz` (DST-aware): the solar
/// noon closest to 12:00 local clock time on that date is used as the
/// anchor, and events are found on either side of it. Sunrise/sunset use
/// the standard −0.833° altitude; twilight uses −6°, −12°, −18°.
///
/// Source: NOAA GML solar calculator spreadsheet (columns W–AA), Meeus,
/// *Astronomical Algorithms* ch. 15, with iterative refinement.
pub fn sun_times(date: NaiveDate, lat_deg: f64, lon_deg: f64, tz: Tz) -> SunTimes {
    let local_noon = date.and_hms_opt(12, 0, 0).expect("valid time");
    let guess = tz
        .from_local_datetime(&local_noon)
        .earliest()
        .map(|d| d.with_timezone(&Utc))
        .unwrap_or_else(|| add_seconds(local_noon.and_utc(), -lon_deg * 240.0));
    let solar_noon = solar_noon_near(guess, lon_deg);
    let noon_elevation_deg = solar_position(solar_noon, lat_deg, lon_deg).elevation_deg;

    let ev = |alt: f64, rising: bool| event_time(solar_noon, lat_deg, lon_deg, alt, rising);
    let sunrise = ev(SUNRISE_ALTITUDE_DEG, true);
    let sunset = ev(SUNRISE_ALTITUDE_DEG, false);

    let (polar, day_length_s) = match (sunrise, sunset) {
        (Some(r), Some(s)) => (None, (s - r).num_milliseconds() as f64 / 1000.0),
        _ => {
            // Classify by geometric noon altitude relative to -0.833°.
            let sc = sun_coords(solar_noon);
            let geom_noon = 90.0 - (lat_deg - sc.declination_deg).abs();
            if geom_noon > SUNRISE_ALTITUDE_DEG {
                (Some(Polar::Day), 86_400.0)
            } else {
                (Some(Polar::Night), 0.0)
            }
        }
    };
    let (sunrise, sunset) = if polar.is_some() {
        (None, None)
    } else {
        (sunrise, sunset)
    };

    SunTimes {
        date,
        solar_noon,
        sunrise,
        sunset,
        civil_dawn: ev(CIVIL_ALTITUDE_DEG, true),
        civil_dusk: ev(CIVIL_ALTITUDE_DEG, false),
        nautical_dawn: ev(NAUTICAL_ALTITUDE_DEG, true),
        nautical_dusk: ev(NAUTICAL_ALTITUDE_DEG, false),
        astronomical_dawn: ev(ASTRONOMICAL_ALTITUDE_DEG, true),
        astronomical_dusk: ev(ASTRONOMICAL_ALTITUDE_DEG, false),
        day_length_s,
        noon_elevation_deg,
        polar,
    }
}

/// Solar positions sampled every `step_s` seconds from `start` (inclusive)
/// to `end` (inclusive), for charting. Returns an empty vector if
/// `step_s <= 0` or `end < start`.
///
/// Source: NOAA GML solar calculator algorithm (see [`solar_position`]).
pub fn sun_samples(
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    step_s: i64,
    lat_deg: f64,
    lon_deg: f64,
) -> Vec<(DateTime<Utc>, SolarPosition)> {
    let mut out = Vec::new();
    if step_s <= 0 || end < start {
        return out;
    }
    let step = Duration::seconds(step_s);
    let mut t = start;
    while t <= end {
        out.push((t, solar_position(t, lat_deg, lon_deg)));
        t += step;
    }
    out
}

/// Angle of incidence (degrees, `0..=180`) of direct sunlight on a plane
/// tilted `tilt_deg` from horizontal whose outward normal points toward
/// `surface_azimuth_deg` (clockwise from north; 180 = south-facing).
///
/// `cos θ = cos z cos β + sin z sin β cos(γs − γ)`.
/// Source: Duffie & Beckman, *Solar Engineering of Thermal Processes*
/// (4th ed.), eq. 1.6.3 (azimuths re-referenced to north; the difference
/// is unaffected).
pub fn angle_of_incidence_deg(pos: &SolarPosition, tilt_deg: f64, surface_azimuth_deg: f64) -> f64 {
    let z = pos.zenith_deg;
    let cos_t = cos_d(z) * cos_d(tilt_deg)
        + sin_d(z) * sin_d(tilt_deg) * cos_d(pos.azimuth_deg - surface_azimuth_deg);
    cos_t.clamp(-1.0, 1.0).acos().to_degrees()
}

/// [`sun_times`] for every day of `year` (365 or 366 entries), for a
/// sunrise/sunset-through-the-year chart.
///
/// Source: NOAA GML solar calculator algorithm (see [`sun_times`]).
pub fn annual_sun_table(year: i32, lat_deg: f64, lon_deg: f64, tz: Tz) -> Vec<SunTimes> {
    let Some(first) = NaiveDate::from_ymd_opt(year, 1, 1) else {
        return Vec::new();
    };
    first
        .iter_days()
        .take_while(|d| d.year() == year)
        .map(|d| sun_times(d, lat_deg, lon_deg, tz))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Timelike;

    fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, s).unwrap()
    }

    // Reference: Reda & Andreas, "Solar Position Algorithm for Solar Radiation
    // Applications", NREL/TP-560-34302 (2004, rev. 2008), Table A5.1 example:
    // 2003-10-17 12:30:30 local (UTC-7), lat 39.742476, lon -105.1786
    // → topocentric zenith 50.11162°, azimuth 194.34024°.
    #[test]
    fn nrel_spa_example_position() {
        let p = solar_position(utc(2003, 10, 17, 19, 30, 30), 39.742476, -105.1786);
        assert!(
            (p.zenith_deg - 50.11162).abs() < 0.05,
            "zenith {}",
            p.zenith_deg
        );
        assert!(
            (p.azimuth_deg - 194.34024).abs() < 0.1,
            "azimuth {}",
            p.azimuth_deg
        );
        assert!((p.elevation_deg + p.zenith_deg - 90.0).abs() < 1e-12);
    }

    // Same NREL SPA example: sunrise 06:12:43, sunset 17:20:19 local (UTC-7).
    // Etc/GMT+7 is UTC-7 (POSIX sign inversion).
    //
    // Note: SPA computes rise/transit/set within the *UT* day 2003-10-17
    // (00:00–24:00 UT = 17:00 Oct 16 – 17:00 Oct 17 local), so its "sunset
    // 17:20:19" is 2003-10-17 00:20:19 UTC, i.e. the evening of Oct 16 local.
    // Our `sun_times` is keyed by local date, so we check sunrise and transit
    // against the Oct 17 local day and SPA's sunset against the Oct 16 local day.
    // (The Oct 17 local sunset is ~88 s earlier, as expected from the falling
    // October declination.)
    #[test]
    fn nrel_spa_example_sunrise_sunset() {
        let (lat, lon, tz) = (39.742476, -105.1786, chrono_tz::Etc::GMTPlus7);
        let st = sun_times(NaiveDate::from_ymd_opt(2003, 10, 17).unwrap(), lat, lon, tz);
        let rise = st.sunrise.unwrap();
        assert!(
            (rise - utc(2003, 10, 17, 13, 12, 43)).num_seconds().abs() <= 60,
            "sunrise {rise}"
        );
        // SPA transit for the example: 11:46:04.97 local.
        assert!(
            (st.solar_noon - utc(2003, 10, 17, 18, 46, 5))
                .num_seconds()
                .abs()
                <= 60,
            "noon {}",
            st.solar_noon
        );
        assert_eq!(st.polar, None);
        let prev = sun_times(NaiveDate::from_ymd_opt(2003, 10, 16).unwrap(), lat, lon, tz);
        let set = prev.sunset.unwrap();
        assert!(
            (set - utc(2003, 10, 17, 0, 20, 19)).num_seconds().abs() <= 60,
            "sunset {set}"
        );
        let own_set = st.sunset.unwrap();
        let shift = (set + Duration::days(1) - own_set).num_seconds();
        assert!((30..=150).contains(&shift), "shift {shift}");
    }

    // June solstice 2024-06-20 20:51 UTC and March equinox 2024-03-20 03:06 UTC
    // (US Naval Observatory, "Earth's Seasons" table). Obliquity ≈ 23.436°.
    #[test]
    fn solstice_and_equinox_declination() {
        let s = solar_position(utc(2024, 6, 20, 20, 51, 0), 0.0, 0.0);
        assert!(
            (s.declination_deg - 23.44).abs() < 0.05,
            "{}",
            s.declination_deg
        );
        let e = solar_position(utc(2024, 3, 20, 3, 6, 0), 0.0, 0.0);
        assert!(e.declination_deg.abs() < 0.05, "{}", e.declination_deg);
    }

    // Equation of time extrema: ≈ +16.4 min near Nov 3, ≈ −14.2 min near Feb 11
    // (Meeus, Astronomical Algorithms ch. 28; NOAA calculator gives the same).
    #[test]
    fn equation_of_time_extrema() {
        let nov = solar_position(utc(2024, 11, 3, 12, 0, 0), 0.0, 0.0);
        assert!(
            (nov.equation_of_time_min - 16.4).abs() < 0.3,
            "{}",
            nov.equation_of_time_min
        );
        let feb = solar_position(utc(2024, 2, 11, 12, 0, 0), 0.0, 0.0);
        assert!(
            (feb.equation_of_time_min + 14.2).abs() < 0.3,
            "{}",
            feb.equation_of_time_min
        );
    }

    #[test]
    fn tromso_polar_day_and_night() {
        let tz = chrono_tz::Europe::Oslo;
        let summer = sun_times(
            NaiveDate::from_ymd_opt(2024, 6, 21).unwrap(),
            69.65,
            18.96,
            tz,
        );
        assert_eq!(summer.polar, Some(Polar::Day));
        assert!(summer.sunrise.is_none() && summer.sunset.is_none());
        assert_eq!(summer.day_length_s, 86_400.0);
        let winter = sun_times(
            NaiveDate::from_ymd_opt(2024, 12, 21).unwrap(),
            69.65,
            18.96,
            tz,
        );
        assert_eq!(winter.polar, Some(Polar::Night));
        assert_eq!(winter.day_length_s, 0.0);
        assert!(winter.noon_elevation_deg < 0.0);
        // Civil twilight still occurs at Tromsø at the winter solstice (noon ≈ -3°).
        assert!(winter.civil_dawn.is_some());
    }

    fn check_sf_day(date: NaiveDate) -> SunTimes {
        let tz = chrono_tz::America::Los_Angeles;
        let st = sun_times(date, 37.7749, -122.4194, tz);
        let rise = st.sunrise.expect("sunrise");
        let set = st.sunset.expect("sunset");
        assert!(rise < st.solar_noon && st.solar_noon < set);
        for t in [rise, st.solar_noon, set] {
            assert_eq!(t.with_timezone(&tz).date_naive(), date, "{t}");
        }
        // Symmetry about solar noon within 1 minute.
        let am = (st.solar_noon - rise).num_seconds();
        let pm = (set - st.solar_noon).num_seconds();
        assert!((am - pm).abs() <= 60, "am {am} pm {pm}");
        // Twilight ordering.
        assert!(st.astronomical_dawn.unwrap() < st.nautical_dawn.unwrap());
        assert!(st.nautical_dawn.unwrap() < st.civil_dawn.unwrap());
        assert!(st.civil_dawn.unwrap() < rise);
        assert!(set < st.civil_dusk.unwrap());
        assert!(st.civil_dusk.unwrap() < st.nautical_dusk.unwrap());
        assert!(st.nautical_dusk.unwrap() < st.astronomical_dusk.unwrap());
        st
    }

    // DST transition days in San Francisco: properties only (ordering, local date,
    // symmetry). Day length sanity: shorter than 12 h before the March equinox and
    // in November; local solar noon about 13:1x PDT / 12:4x PST.
    #[test]
    fn san_francisco_dst_days() {
        let tz = chrono_tz::America::Los_Angeles;
        let mar = check_sf_day(NaiveDate::from_ymd_opt(2024, 3, 10).unwrap());
        let nov = check_sf_day(NaiveDate::from_ymd_opt(2024, 11, 3).unwrap());
        assert!(mar.day_length_s < 12.0 * 3600.0 && mar.day_length_s > 11.5 * 3600.0);
        assert!(nov.day_length_s < 11.0 * 3600.0 && nov.day_length_s > 10.0 * 3600.0);
        // Mar 10 after 02:00 is PDT (UTC-7): solar noon ≈ 13:1x local.
        assert_eq!(mar.solar_noon.with_timezone(&tz).hour(), 13);
        // Nov 3 after 02:00 is PST (UTC-8): solar noon ≈ 11:4x local
        // (EoT ≈ +16.4 min, lon −122.42 → 12:09.7 − 16.4 ≈ 11:53).
        assert_eq!(nov.solar_noon.with_timezone(&tz).hour(), 11);
    }

    // At the equinox day length at 37.77° exceeds 12 h only by the -0.833°
    // horizon dip (≈ 8.4 min), so |day_length − 12 h| < 10 min. The 2024 March
    // equinox (2024-03-20 03:06 UTC) falls on 2024-03-19 local in Los Angeles.
    #[test]
    fn equinox_day_length_near_twelve_hours() {
        let st = sun_times(
            NaiveDate::from_ymd_opt(2024, 3, 19).unwrap(),
            37.7749,
            -122.4194,
            chrono_tz::America::Los_Angeles,
        );
        assert!(
            (st.day_length_s - 43_200.0).abs() < 600.0,
            "{}",
            st.day_length_s
        );
    }

    #[test]
    fn angle_of_incidence_basics() {
        let overhead = SolarPosition {
            elevation_deg: 90.0,
            zenith_deg: 0.0,
            azimuth_deg: 123.0,
            declination_deg: 0.0,
            equation_of_time_min: 0.0,
        };
        for tilt in [0.0, 20.0, 35.0, 90.0] {
            assert!((angle_of_incidence_deg(&overhead, tilt, 180.0) - tilt).abs() < 1e-9);
        }
        let normal = SolarPosition {
            elevation_deg: 55.0,
            zenith_deg: 35.0,
            azimuth_deg: 180.0,
            ..overhead
        };
        assert!(angle_of_incidence_deg(&normal, 35.0, 180.0).abs() < 1e-6);
        // Sun behind a vertical north-facing wall → 90°+ incidence.
        assert!(angle_of_incidence_deg(&normal, 90.0, 0.0) > 90.0);
    }

    #[test]
    fn samples_and_annual_table() {
        let s = sun_samples(
            utc(2024, 6, 21, 0, 0, 0),
            utc(2024, 6, 22, 0, 0, 0),
            3600,
            40.0,
            -105.0,
        );
        assert_eq!(s.len(), 25);
        assert!(
            sun_samples(
                utc(2024, 1, 1, 0, 0, 0),
                utc(2024, 1, 2, 0, 0, 0),
                0,
                0.0,
                0.0
            )
            .is_empty()
        );
        let tbl = annual_sun_table(2024, 37.7749, -122.4194, chrono_tz::America::Los_Angeles);
        assert_eq!(tbl.len(), 366);
        assert!(tbl.iter().all(|d| d.polar.is_none()));
        // Longest day near the June solstice.
        let longest = tbl
            .iter()
            .max_by(|a, b| a.day_length_s.total_cmp(&b.day_length_s))
            .unwrap();
        assert!(
            (170..=176).contains(&longest.date.ordinal()),
            "{}",
            longest.date
        );
    }
}
