//! RustSolar core: pure, deterministic calculations with no I/O.
//!
//! Every function here is unit-tested. Units are SI unless the name says
//! otherwise (`_w`, `_wh`, `_kw`, `_kwh`, `_deg`, `_pct`).

pub mod astro;
pub mod battery;
pub mod energy;
pub mod pge_nbt;
pub mod pvmodel;
pub mod tariff;
