//! Sentry Mate's shared types: the contract between the parts that work out the car's history (`timeline`), keep it
//! (`db`), bring old history in (`import`), and show it (`web`).
//!
//! Units, everywhere: time in milliseconds since 1970 UTC (`*_ms`), distance in km, speed in km/h, power in kW,
//! energy in kWh, temperature in °C, tire pressure in bar, voltage in V, current in A. A value the car didn't send
//! is `None`, never 0. Power is positive out of the pack (driving, heating) and negative into it (charging,
//! regenerating), as Sentry Six Manager shows it.

use serde::{Deserialize, Serialize};

pub type CarId = u32;
pub type DriveId = u64;
pub type ChargeId = u64;
pub type PlaceId = u32;

/// Where a piece of history came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Source {
    /// Worked out from CAN Bridge logs.
    #[default]
    Can,
    /// Brought in from a TeslaMate backup (Tesla API data: a point every few seconds, unsigned).
    Teslamate,
}

/// The DBC set a car is decoded with (`dbc::Profile` by name).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum CarProfile {
    /// Model 3 and Y, and the 2021+ Model S and X.
    #[default]
    Tesla3Y,
    /// Model S and X before the 2021 refresh.
    TeslaSxLegacy,
}

impl CarProfile {
    pub fn dbc_name(self) -> &'static str {
        match self {
            CarProfile::Tesla3Y => "tesla-3y",
            CarProfile::TeslaSxLegacy => "tesla-sx-legacy",
        }
    }
}

/// What the car's VIN says (the serial number is never kept).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct VinInfo {
    /// The VIN without its last six characters (the serial), e.g. `7SAYGDEE1NF`.
    pub vin_prefix: String,
    /// `3`, `Y`, `S` or `X`.
    pub model: Option<char>,
    pub year: Option<u16>,
    /// The plant letter (11th character).
    pub plant: Option<char>,
    /// Which message it came from: 0x405 or 0x508.
    pub via: u32,
}

/// A car, as the owner set it up.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Car {
    pub id: CarId,
    pub name: String,
    /// The CAN Bridge keys (one per install: a Screenmate, an app reinstall, a Raspberry Pi logger) that belong to
    /// this car. A key belongs to one car.
    pub vehicle_keys: Vec<String>,
    /// When two keys record at once, the one the timeline uses (else the verified one with fewer drops).
    pub preferred_key: Option<String>,
    /// From the CAN bus, when seen.
    pub vin: Option<VinInfo>,
    /// `None`: automatic (from the VIN, else Model 3/Y).
    pub profile_setting: Option<CarProfile>,
    /// IANA time zone, e.g. `America/Chicago` (days, tariffs and the UI's clock follow it).
    pub time_zone: String,
    /// Wh per km at the wheels used for range figures, when known.
    pub efficiency_wh_km: Option<f64>,
    pub created_ms: u64,
}

impl Car {
    /// The DBC set in use: the setting, else what the VIN says, else Model 3/Y.
    pub fn profile(&self) -> CarProfile {
        if let Some(p) = self.profile_setting {
            return p;
        }
        match &self.vin {
            Some(v) if v.via == 0x508 => CarProfile::TeslaSxLegacy,
            Some(v) if matches!(v.model, Some('S') | Some('X')) && v.year.is_some_and(|y| y < 2021) => {
                CarProfile::TeslaSxLegacy
            }
            _ => CarProfile::Tesla3Y,
        }
    }
}

/// Gear, as the drive inverter reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Gear {
    P,
    R,
    N,
    D,
}

/// AC (onboard charger) or DC (fast charger).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChargeKind {
    Ac,
    Dc,
}

/// The car once a second, from its decoded signals: the last value of each in that second. The timeline is built
/// from these, and positions and charge curves are kept as these.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Sample {
    /// The start of the second.
    pub t_ms: u64,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    /// GPS accuracy, m.
    pub gps_accuracy_m: Option<f64>,
    pub elevation_m: Option<f64>,
    pub speed_kmh: Option<f64>,
    pub gear: Option<Gear>,
    pub odometer_km: Option<f64>,
    /// Battery % as the car's screen shows it.
    pub soc: Option<f64>,
    /// Battery % from the battery controller (may differ from the screen; kept for checking).
    pub soc_bms: Option<f64>,
    /// Rated range, km.
    pub range_km: Option<f64>,
    pub ideal_range_km: Option<f64>,
    pub energy_remaining_kwh: Option<f64>,
    /// Pack voltage and current; power = V × I / 1000, positive out of the pack.
    pub battery_voltage_v: Option<f64>,
    pub battery_current_a: Option<f64>,
    pub battery_power_kw: Option<f64>,
    pub battery_temp_c: Option<f64>,
    pub outside_temp_c: Option<f64>,
    pub inside_temp_c: Option<f64>,
    /// The car says it is charging (BMS_CHARGING, or the first charge status signal there).
    pub charging: Option<bool>,
    pub charge_kind: Option<ChargeKind>,
    /// Power going into the pack while charging, kW (positive).
    pub charge_power_kw: Option<f64>,
    pub charger_voltage_v: Option<f64>,
    pub charger_current_a: Option<f64>,
    /// Lifetime energy counters of the pack, kWh (when the car sends them).
    pub total_charge_kwh: Option<f64>,
    pub total_discharge_kwh: Option<f64>,
    pub lv_voltage_v: Option<f64>,
    /// Tire pressures, bar: front left, front right, rear left, rear right.
    pub tpms_bar: [Option<f64>; 4],
}

/// Whether the car was reachable and what it was doing, over a span.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum State {
    /// Frames on the car's buses.
    Online,
    /// The bridge closed a log as quiet and no frames came after: the car slept.
    Asleep,
    /// No log covers it, or recording was paused (`# not recorded`): we don't know. Never counted as sleep.
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StateSpan {
    pub car: CarId,
    pub state: State,
    pub start_ms: u64,
    pub end_ms: u64,
    /// The buses that sent frames in it (`can0`, `can1`, ...): the vehicle bus awake with the chassis asleep is a
    /// parked car kept awake (charging, keeping the pack warm).
    pub buses: Vec<String>,
    pub source: Source,
}

/// A point of a drive's track or of the car parked: a [`Sample`] reduced to what maps and charts need.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Position {
    pub t_ms: u64,
    pub lat: f64,
    pub lon: f64,
    pub elevation_m: Option<f64>,
    pub speed_kmh: Option<f64>,
    pub power_kw: Option<f64>,
    pub soc: Option<f64>,
    pub odometer_km: Option<f64>,
    pub range_km: Option<f64>,
    pub outside_temp_c: Option<f64>,
    pub inside_temp_c: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Drive {
    pub id: DriveId,
    pub car: CarId,
    pub start_ms: u64,
    pub end_ms: u64,
    pub start: Option<Position>,
    pub end: Option<Position>,
    pub start_place: Option<PlaceId>,
    pub end_place: Option<PlaceId>,
    /// Addresses (reverse geocoded), when looked up.
    pub start_address: Option<String>,
    pub end_address: Option<String>,
    pub start_odometer_km: Option<f64>,
    pub end_odometer_km: Option<f64>,
    /// From the odometer (GPS only when the car sent no odometer).
    pub distance_km: Option<f64>,
    pub speed_max_kmh: Option<f64>,
    pub power_max_kw: Option<f64>,
    pub power_min_kw: Option<f64>,
    /// Integrated battery power over the drive (regenerated energy taken off).
    pub energy_used_kwh: Option<f64>,
    pub start_soc: Option<f64>,
    pub end_soc: Option<f64>,
    pub start_range_km: Option<f64>,
    pub end_range_km: Option<f64>,
    pub outside_temp_avg_c: Option<f64>,
    pub inside_temp_avg_c: Option<f64>,
    pub ascent_m: Option<f64>,
    pub descent_m: Option<f64>,
    pub source: Source,
    /// The timeline rules that made it; a drive made by older rules is made again.
    pub rules_version: u32,
}

impl Drive {
    pub fn duration_min(&self) -> f64 {
        self.end_ms.saturating_sub(self.start_ms) as f64 / 60_000.0
    }

    /// Wh per km over the drive, when both are known.
    pub fn efficiency_wh_km(&self) -> Option<f64> {
        match (self.energy_used_kwh, self.distance_km) {
            (Some(e), Some(d)) if d > 0.1 => Some(e * 1000.0 / d),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Charge {
    pub id: ChargeId,
    pub car: CarId,
    pub start_ms: u64,
    pub end_ms: u64,
    pub position: Option<Position>,
    pub place: Option<PlaceId>,
    pub address: Option<String>,
    pub kind: Option<ChargeKind>,
    /// Into the pack (integrated charge power, or the pack's lifetime counter where sent).
    pub energy_added_kwh: Option<f64>,
    /// From the grid, where known (AC: the charger's input); losses = used − added.
    pub energy_used_kwh: Option<f64>,
    pub start_soc: Option<f64>,
    pub end_soc: Option<f64>,
    pub start_range_km: Option<f64>,
    pub end_range_km: Option<f64>,
    pub power_max_kw: Option<f64>,
    pub outside_temp_avg_c: Option<f64>,
    pub battery_temp_avg_c: Option<f64>,
    /// In the place's currency, at the price of each moment.
    pub cost: Option<f64>,
    pub source: Source,
    pub rules_version: u32,
}

impl Charge {
    pub fn duration_min(&self) -> f64 {
        self.end_ms.saturating_sub(self.start_ms) as f64 / 60_000.0
    }
}

/// A point of a charge curve.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ChargePoint {
    pub t_ms: u64,
    pub power_kw: Option<f64>,
    pub voltage_v: Option<f64>,
    pub current_a: Option<f64>,
    pub soc: Option<f64>,
    pub battery_temp_c: Option<f64>,
    pub range_km: Option<f64>,
}

/// A software update the car installed (when the logs show it).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Update {
    pub car: CarId,
    pub start_ms: u64,
    pub end_ms: Option<u64>,
    pub version: String,
    pub source: Source,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PlaceKind {
    Home,
    Work,
    Charger,
    Other,
}

/// A saved place (geofence) with its own radius and electricity price.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Place {
    pub id: PlaceId,
    pub name: String,
    pub kind: PlaceKind,
    pub lat: f64,
    pub lon: f64,
    /// 50 m to 2 km.
    pub radius_m: f64,
    pub price: Option<PricePlan>,
}

pub const MIN_RADIUS_M: f64 = 50.0;
pub const MAX_RADIUS_M: f64 = 2000.0;
pub const DEFAULT_RADIUS_M: f64 = 200.0;

/// One price, or a time-of-use plan.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PricePlan {
    /// ISO 4217, e.g. `USD`.
    pub currency: String,
    /// Price per kWh when no rate below applies.
    pub per_kwh: f64,
    /// Time-of-use rates, in the car's time zone; the first that matches wins.
    pub rates: Vec<Rate>,
    /// A fixed fee per charge session (e.g. a public charger's).
    pub session_fee: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Rate {
    pub name: String,
    pub per_kwh: f64,
    /// Days it applies: 0 = Monday … 6 = Sunday.
    pub days: Vec<u8>,
    /// `HH:MM` local; `to` before `from` runs past midnight.
    pub from: String,
    pub to: String,
}

/// A car's most recent values, for the Now page, MQTT and Home Assistant.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Now {
    pub car: CarId,
    /// When these values were recorded (not when they arrived).
    pub at_ms: u64,
    pub state: Option<State>,
    pub driving: bool,
    pub sample: Sample,
    pub place: Option<PlaceId>,
}
