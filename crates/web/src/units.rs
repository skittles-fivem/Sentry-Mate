//! Units. The API is metric and names the unit in each field (`_km`, `_kwh`, `_c`, `_kpa`, ...) and never converts:
//! pages apply the person's choices. These are the choices themselves, with the server's defaults
//! ([`Defaults`], set from `/api/v1/settings`) that a person's preferences start from.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Distance {
    #[default]
    Km,
    Mi,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Temperature {
    #[default]
    C,
    F,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Pressure {
    #[default]
    Bar,
    Psi,
}

/// Rated or ideal range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Range {
    #[default]
    Rated,
    Ideal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Units {
    pub distance: Distance,
    pub temperature: Temperature,
    pub pressure: Pressure,
}

/// The allowed words of each unit, for field errors.
pub const DISTANCE: &[&str] = &["km", "mi"];
pub const TEMPERATURE: &[&str] = &["c", "f"];
pub const PRESSURE: &[&str] = &["bar", "psi"];
pub const RANGE: &[&str] = &["rated", "ideal"];

/// What everyone's preferences start from until they choose (server-wide, `/api/v1/settings`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Defaults {
    pub units: Units,
    pub range: Range,
    /// Car ids (`c<n>`) in the server's order; each person's list shows only the cars they see.
    pub car_order: Vec<String>,
}

/// Unit conversions for the few places the server itself writes a unit (texts, CSV headers stay metric).
pub fn km_to_mi(km: f64) -> f64 {
    km / 1.609_344
}

pub fn c_to_f(c: f64) -> f64 {
    c * 9.0 / 5.0 + 32.0
}

pub fn kpa_to_psi(kpa: f64) -> f64 {
    kpa / 6.894_757
}

pub fn kpa_to_bar(kpa: f64) -> f64 {
    kpa / 100.0
}
