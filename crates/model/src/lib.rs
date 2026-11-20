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

/// The row family a piece of history belongs to: what the engine builds from live inputs (and may rebuild), or what
/// an import brought in (never touched by a rebuild). Which live input gave a row is in its `inputs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum Source {
    /// Worked out by the engine from live inputs (SMO logs, the Tesla API). Stored as `can`, the word older servers
    /// wrote.
    #[default]
    #[serde(rename = "can")]
    Live,
    /// Brought in from a TeslaMate backup (Tesla API data: a point every few seconds, unsigned).
    #[serde(rename = "teslamate")]
    Teslamate,
}

/// A live input: where the engine's own history comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Input {
    /// SMO, the Screenmate CAN Bridge: signed CAN logs.
    Smo,
    /// The Tesla Owner API, polled by this server.
    TeslaApi,
}

impl Input {
    pub const ALL: [Input; 2] = [Input::Smo, Input::TeslaApi];

    fn bit(self) -> u8 {
        match self {
            Input::Smo => 1,
            Input::TeslaApi => 2,
        }
    }
}

/// A set of live inputs (stored as a list of words, `["smo", "tesla_api"]`). Empty on rows written before inputs were
/// kept: a live row then counts as SMO, the only live input there was.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Inputs(u8);

impl Inputs {
    pub const NONE: Inputs = Inputs(0);
    pub const SMO: Inputs = Inputs(1);
    pub const TESLA_API: Inputs = Inputs(2);
    pub const BOTH: Inputs = Inputs(3);

    pub fn is_empty(&self) -> bool {
        self.0 == 0
    }

    pub fn contains(self, i: Input) -> bool {
        self.0 & i.bit() != 0
    }

    pub fn insert(&mut self, i: Input) {
        self.0 |= i.bit();
    }

    pub fn union(self, o: Inputs) -> Inputs {
        Inputs(self.0 | o.0)
    }

    /// In a fixed order: SMO first.
    pub fn iter(self) -> impl Iterator<Item = Input> {
        Input::ALL.into_iter().filter(move |i| self.contains(*i))
    }
}

impl From<Input> for Inputs {
    fn from(i: Input) -> Inputs {
        Inputs(i.bit())
    }
}

impl FromIterator<Input> for Inputs {
    fn from_iter<T: IntoIterator<Item = Input>>(it: T) -> Inputs {
        let mut s = Inputs::NONE;
        for i in it {
            s.insert(i);
        }
        s
    }
}

impl Serialize for Inputs {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_seq(self.iter())
    }
}

impl<'de> Deserialize<'de> for Inputs {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Inputs, D::Error> {
        Ok(Vec::<Input>::deserialize(d)?.into_iter().collect())
    }
}

/// A site history can be imported from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportSite {
    Teslamate,
}

impl ImportSite {
    /// The word the API writes (`site`).
    pub fn word(self) -> &'static str {
        match self {
            ImportSite::Teslamate => "teslamate",
        }
    }

    /// The site's name as people know it.
    pub fn name(self) -> &'static str {
        match self {
            ImportSite::Teslamate => "TeslaMate",
        }
    }

    pub fn from_word(w: &str) -> Option<ImportSite> {
        match w {
            "teslamate" => Some(ImportSite::Teslamate),
            _ => None,
        }
    }
}

/// Where a row or a value came from, as the API names it: SMO, the Tesla API, or an import from a site.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Origin {
    Smo,
    TeslaApi,
    Import(ImportSite),
}

impl Origin {
    /// `smo`, `tesla_api` or `import`.
    pub fn kind(self) -> &'static str {
        match self {
            Origin::Smo => "smo",
            Origin::TeslaApi => "tesla_api",
            Origin::Import(_) => "import",
        }
    }

    /// The site of an import.
    pub fn site(self) -> Option<ImportSite> {
        match self {
            Origin::Import(s) => Some(s),
            _ => None,
        }
    }

    /// One word for filters and CSV: `smo`, `tesla_api`, `import:teslamate`.
    pub fn word(self) -> String {
        match self {
            Origin::Import(s) => format!("import:{}", s.word()),
            o => o.kind().to_string(),
        }
    }

    /// A short label a page can show: "Data from SMO", "Data from the Tesla API", "Imported from TeslaMate".
    pub fn label(self) -> String {
        match self {
            Origin::Smo => "Data from SMO".into(),
            Origin::TeslaApi => "Data from the Tesla API".into(),
            Origin::Import(s) => format!("Imported from {}", s.name()),
        }
    }

    fn short(self) -> String {
        match self {
            Origin::Smo => "SMO".into(),
            Origin::TeslaApi => "the Tesla API".into(),
            Origin::Import(s) => s.name().into(),
        }
    }

    /// The origins of a row: its live inputs (SMO for an older live row that names none), or its import.
    pub fn of_row(source: Source, inputs: Inputs) -> Vec<Origin> {
        match source {
            Source::Teslamate => vec![Origin::Import(ImportSite::Teslamate)],
            Source::Live if inputs.is_empty() => vec![Origin::Smo],
            Source::Live => inputs.iter().map(Origin::from).collect(),
        }
    }

    /// One label for several origins: "Data from SMO and the Tesla API", "Data from SMO; imported from TeslaMate".
    pub fn label_of(origins: &[Origin]) -> String {
        let live: Vec<String> = origins.iter().filter(|o| o.site().is_none()).map(|o| o.short()).collect();
        let imported: Vec<String> = origins.iter().filter(|o| o.site().is_some()).map(|o| o.short()).collect();
        let and = |v: &[String]| match v {
            [] => String::new(),
            [one] => one.clone(),
            [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
        };
        match (live.is_empty(), imported.is_empty()) {
            (true, true) => "No data yet".into(),
            (false, true) => format!("Data from {}", and(&live)),
            (true, false) => format!("Imported from {}", and(&imported)),
            (false, false) => format!("Data from {}; imported from {}", and(&live), and(&imported)),
        }
    }
}

impl From<Input> for Origin {
    fn from(i: Input) -> Origin {
        match i {
            Input::Smo => Origin::Smo,
            Input::TeslaApi => Origin::TeslaApi,
        }
    }
}

/// Where a time series is stored: one stream per input, and one for imports. Points of different streams never mix
/// on disk; reads fuse them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Stream {
    /// From SMO logs (dir `can`, as older servers wrote it).
    Smo,
    /// From the Tesla API (dir `tesla-api`).
    TeslaApi,
    /// From a TeslaMate import (dir `teslamate`).
    Teslamate,
}

impl Stream {
    pub const ALL: [Stream; 3] = [Stream::Smo, Stream::TeslaApi, Stream::Teslamate];

    /// The series directory's suffix.
    pub fn dir_name(self) -> &'static str {
        match self {
            Stream::Smo => "can",
            Stream::TeslaApi => "tesla-api",
            Stream::Teslamate => "teslamate",
        }
    }

    pub fn origin(self) -> Origin {
        match self {
            Stream::Smo => Origin::Smo,
            Stream::TeslaApi => Origin::TeslaApi,
            Stream::Teslamate => Origin::Import(ImportSite::Teslamate),
        }
    }
}

/// A row family reads its own stream: live rows the SMO stream (the Tesla API's is named on its own), imports theirs.
impl From<Source> for Stream {
    fn from(s: Source) -> Stream {
        match s {
            Source::Live => Stream::Smo,
            Source::Teslamate => Stream::Teslamate,
        }
    }
}

impl From<Input> for Stream {
    fn from(i: Input) -> Stream {
        match i {
            Input::Smo => Stream::Smo,
            Input::TeslaApi => Stream::TeslaApi,
        }
    }
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

impl VinInfo {
    /// The DBC set this VIN calls for: a Model S or X from before the 2021 refresh (it sends its VIN in 0x508, or its
    /// VIN says so) gets the legacy set, every other car the Model 3/Y set.
    pub fn profile(&self) -> CarProfile {
        let legacy_sx = matches!(self.model, Some('S') | Some('X')) && self.year.is_some_and(|y| y < 2021);
        if self.via == 0x508 || legacy_sx { CarProfile::TeslaSxLegacy } else { CarProfile::Tesla3Y }
    }
}

impl Car {
    /// The DBC set in use: the setting, else what the VIN says, else Model 3/Y.
    pub fn profile(&self) -> CarProfile {
        if let Some(p) = self.profile_setting {
            return p;
        }
        self.vin.as_ref().map_or(CarProfile::Tesla3Y, VinInfo::profile)
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
    /// The live inputs that gave it (empty on older rows: SMO). Imported rows leave it empty.
    #[serde(default, skip_serializing_if = "Inputs::is_empty")]
    pub inputs: Inputs,
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
    /// The live inputs that gave it (empty on older rows: SMO). Imported rows leave it empty.
    #[serde(default, skip_serializing_if = "Inputs::is_empty")]
    pub inputs: Inputs,
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
    /// The live inputs that gave it (empty on older rows: SMO). Imported rows leave it empty.
    #[serde(default, skip_serializing_if = "Inputs::is_empty")]
    pub inputs: Inputs,
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
    /// The live inputs that gave it (empty on older rows: SMO). Imported rows leave it empty.
    #[serde(default, skip_serializing_if = "Inputs::is_empty")]
    pub inputs: Inputs,
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
    /// 10 m to 5 km.
    pub radius_m: f64,
    pub price: Option<PricePlan>,
}

pub const MIN_RADIUS_M: f64 = 10.0;
pub const MAX_RADIUS_M: f64 = 5000.0;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn vin(model: Option<char>, year: Option<u16>, via: u32) -> VinInfo {
        VinInfo { vin_prefix: String::new(), model, year, plant: None, via }
    }

    #[test]
    fn the_dbc_set_from_the_vin() {
        assert_eq!(vin(Some('Y'), Some(2024), 0x405).profile(), CarProfile::Tesla3Y);
        assert_eq!(vin(Some('3'), Some(2018), 0x405).profile(), CarProfile::Tesla3Y);
        assert_eq!(vin(Some('S'), Some(2022), 0x405).profile(), CarProfile::Tesla3Y, "a refreshed Model S");
        assert_eq!(vin(Some('S'), Some(2017), 0x508).profile(), CarProfile::TeslaSxLegacy);
        assert_eq!(vin(Some('X'), Some(2019), 0).profile(), CarProfile::TeslaSxLegacy, "a VIN from a sidecar");
        assert_eq!(vin(None, None, 0x508).profile(), CarProfile::TeslaSxLegacy, "0x508 is sent by legacy cars only");
        assert_eq!(vin(None, None, 0).profile(), CarProfile::Tesla3Y);
        let mut car = Car {
            id: 1,
            name: "test".into(),
            vehicle_keys: vec![],
            preferred_key: None,
            vin: Some(vin(Some('S'), Some(2017), 0x508)),
            profile_setting: None,
            time_zone: "UTC".into(),
            efficiency_wh_km: None,
            created_ms: 0,
        };
        assert_eq!(car.profile(), CarProfile::TeslaSxLegacy);
        car.profile_setting = Some(CarProfile::Tesla3Y);
        assert_eq!(car.profile(), CarProfile::Tesla3Y, "the setting wins");
        car.vin = None;
        car.profile_setting = None;
        assert_eq!(car.profile(), CarProfile::Tesla3Y);
    }

    #[test]
    fn inputs_read_and_write_as_words() {
        let both: Inputs = [Input::TeslaApi, Input::Smo].into_iter().collect();
        assert_eq!(both, Inputs::BOTH);
        assert_eq!(serde_json::to_string(&both).unwrap(), r#"["smo","tesla_api"]"#);
        assert_eq!(serde_json::from_str::<Inputs>(r#"["tesla_api"]"#).unwrap(), Inputs::TESLA_API);
        assert!(Inputs::NONE.is_empty());
        assert!(serde_json::from_str::<Inputs>(r#"["fleet_api"]"#).is_err(), "an unknown input is refused");
    }

    #[test]
    fn source_keeps_its_stored_words() {
        assert_eq!(serde_json::to_string(&Source::Live).unwrap(), r#""can""#);
        assert_eq!(serde_json::from_str::<Source>(r#""teslamate""#).unwrap(), Source::Teslamate);
        assert_eq!(Stream::from(Source::Live).dir_name(), "can");
        assert_eq!(Stream::TeslaApi.dir_name(), "tesla-api");
    }

    #[test]
    fn a_row_written_before_inputs_reads_as_smo() {
        let old = r#"{"car":1,"start_ms":5,"end_ms":null,"version":"2026.38.4","source":"can"}"#;
        let u: Update = serde_json::from_str(old).unwrap();
        assert!(u.inputs.is_empty());
        assert_eq!(Origin::of_row(u.source, u.inputs), vec![Origin::Smo]);
        assert_eq!(serde_json::to_string(&u).unwrap(), old, "an empty set is not written");
        assert_eq!(Origin::of_row(Source::Teslamate, Inputs::NONE), vec![Origin::Import(ImportSite::Teslamate)]);
        assert_eq!(Origin::of_row(Source::Live, Inputs::BOTH), vec![Origin::Smo, Origin::TeslaApi]);
    }

    #[test]
    fn labels() {
        assert_eq!(Origin::Smo.label(), "Data from SMO");
        assert_eq!(Origin::TeslaApi.label(), "Data from the Tesla API");
        assert_eq!(Origin::Import(ImportSite::Teslamate).label(), "Imported from TeslaMate");
        assert_eq!(Origin::label_of(&[Origin::Smo, Origin::TeslaApi]), "Data from SMO and the Tesla API");
        assert_eq!(
            Origin::label_of(&[Origin::Smo, Origin::Import(ImportSite::Teslamate)]),
            "Data from SMO; imported from TeslaMate"
        );
        assert_eq!(Origin::label_of(&[]), "No data yet");
        assert_eq!(Origin::Import(ImportSite::Teslamate).word(), "import:teslamate");
    }
}
