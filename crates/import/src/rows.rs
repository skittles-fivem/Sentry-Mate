//! What the importer hands over: TeslaMate's rows as Sentry Mate's own types (tagged [`Source::Teslamate`]), with
//! TeslaMate's ids beside them, and the [`Sink`] that receives them.
//!
//! Rows come per car, in batches, as the backup is read; nothing waits for the whole file. Each row keeps its
//! TeslaMate id (`tm_id`) so the caller can store it keyed by car, table and that id: then a row handed over twice
//! (an import that stopped half-way and was run again) replaces itself instead of doubling.
//!
//! What TeslaMate doesn't have is left empty, never made up. Ids that are the caller's own (`Drive::id`,
//! `Place::id`, places of drives and charges) are 0 or `None`: the caller gives them, using the TeslaMate ids in
//! `tm` to link geofences to its places.

use crate::report::CarReport;
use crate::state::ImportState;
use model::{CarId, Charge, ChargePoint, Drive, Place, Position, Sample, StateSpan, Update, VinInfo};
use serde::{Deserialize, Serialize};

pub type SinkError = Box<dyn std::error::Error + Send + Sync>;

/// The version of the mapping from TeslaMate's tables, put in imported drives' and charges' `rules_version` (the
/// timeline never rebuilds those; a new mapping can be told apart).
pub const MAPPING_VERSION: u32 = 1;

/// A car as TeslaMate kept it. Its VIN's serial number isn't kept: only the first 11 characters and what they say.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct TmCar {
    pub tm_id: i64,
    /// `None` when the backup has no readable VIN for it.
    pub vin: Option<VinInfo>,
    /// The name the owner gave it in TeslaMate.
    pub name: Option<String>,
    /// TeslaMate's model letter (`3`, `Y`, `S`, `X`).
    pub model: Option<String>,
    pub trim_badging: Option<String>,
    pub marketing_name: Option<String>,
    pub exterior_color: Option<String>,
    pub wheel_type: Option<String>,
    /// Wh per km (TeslaMate keeps kWh per km), from its charges.
    pub efficiency_wh_km: Option<f64>,
    pub lfp_battery: Option<bool>,
    pub free_supercharging: Option<bool>,
    /// When TeslaMate first saw it.
    pub added_ms: Option<u64>,
}

/// Which Sentry Mate car a TeslaMate car is, and what earlier imports took from it.
#[derive(Debug, Clone, PartialEq)]
pub struct Target {
    pub car: CarId,
    /// [`ImportState::default`] the first time.
    pub state: ImportState,
}

/// A position: everything TeslaMate recorded at that moment.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PositionRow {
    pub tm_id: i64,
    /// The drive it belongs to, if any.
    pub tm_drive_id: Option<i64>,
    /// `t_ms`, `lat`, `lon`, `elevation_m`, `speed_kmh`, `odometer_km`, `soc` (the battery % the car showed),
    /// `range_km` (rated), `ideal_range_km`, `battery_power_kw` (positive out of the pack), `outside_temp_c`,
    /// `inside_temp_c` and `tpms_bar`, where TeslaMate had them.
    pub sample: Sample,
    /// The battery % usable at that moment (lower in the cold).
    pub usable_soc: Option<f64>,
}

impl PositionRow {
    pub fn position(&self) -> Position {
        position_of(&self.sample)
    }
}

/// A [`Sample`] reduced to a [`Position`] (a sample without a place gives 0, 0: check `lat`/`lon` first).
pub fn position_of(s: &Sample) -> Position {
    Position {
        t_ms: s.t_ms,
        lat: s.lat.unwrap_or_default(),
        lon: s.lon.unwrap_or_default(),
        elevation_m: s.elevation_m,
        speed_kmh: s.speed_kmh,
        power_kw: s.battery_power_kw,
        soc: s.soc,
        odometer_km: s.odometer_km,
        range_km: s.range_km,
        outside_temp_c: s.outside_temp_c,
        inside_temp_c: s.inside_temp_c,
    }
}

/// TeslaMate's ids a drive points to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct DriveRefs {
    pub start_position: Option<i64>,
    pub end_position: Option<i64>,
    pub start_address: Option<i64>,
    pub end_address: Option<i64>,
    pub start_geofence: Option<i64>,
    pub end_geofence: Option<i64>,
}

/// A drive. `start` and `end` (and with them `start_soc`, `end_soc`) come from its first and last positions when
/// they were found ([`Options::max_endpoints`](crate::Options)); otherwise the caller can take them from the car's
/// imported positions (`tm.start_position`, or by time). TeslaMate keeps no measured energy for a drive:
/// `energy_used_kwh` is an estimate, the rated range used times the car's efficiency (noted in the report).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DriveRow {
    pub tm_id: i64,
    pub drive: Drive,
    pub tm: DriveRefs,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ChargeRefs {
    pub position: Option<i64>,
    pub address: Option<i64>,
    pub geofence: Option<i64>,
}

/// A charge. `kind` (AC/DC) and `power_max_kw` come from its points.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChargeRow {
    pub tm_id: i64,
    pub charge: Charge,
    pub tm: ChargeRefs,
}

/// A point of a charge's curve: `power_kw` is the charger's power (positive into the car), `voltage_v` and
/// `current_a` the charger's as the car reported them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChargePointRow {
    pub tm_id: i64,
    /// The charge (TeslaMate's `charging_processes` id).
    pub tm_charge_id: i64,
    pub point: ChargePoint,
    /// A DC fast charger.
    pub fast_charger: Option<bool>,
    pub phases: Option<u8>,
    /// Added so far in this charge.
    pub energy_added_kwh: Option<f64>,
    pub outside_temp_c: Option<f64>,
    pub ideal_range_km: Option<f64>,
    pub usable_soc: Option<f64>,
}

/// A state span. TeslaMate's `offline` (the car didn't answer) becomes [`model::State::Unknown`]: never sleep.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StateRow {
    pub tm_id: i64,
    pub span: StateSpan,
    pub offline: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UpdateRow {
    pub tm_id: i64,
    pub update: Update,
}

/// A geofence as a place. TeslaMate keeps no currency: a price's `currency` is empty for the caller to fill with
/// the owner's. A per-minute price has no place in [`model::PricePlan`]: it is kept beside it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlaceRow {
    pub tm_id: i64,
    pub place: Place,
    pub per_minute_price: Option<f64>,
    /// TeslaMate's fee per charge here (also in `place.price` when there is one).
    pub session_fee: Option<f64>,
    /// TeslaMate's radius, m (the place's is kept within 10 m to 5 km).
    pub tm_radius_m: Option<f64>,
}

/// An address TeslaMate looked up (with OpenStreetMap's Nominatim), as a cache of places already named.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AddressRow {
    pub tm_id: i64,
    /// A short name: its name, or road and number, then the city.
    pub label: String,
    pub display_name: Option<String>,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    pub name: Option<String>,
    pub house_number: Option<String>,
    pub road: Option<String>,
    pub neighbourhood: Option<String>,
    pub city: Option<String>,
    pub county: Option<String>,
    pub postcode: Option<String>,
    pub state: Option<String>,
    pub state_district: Option<String>,
    pub country: Option<String>,
    pub osm_id: Option<i64>,
    pub osm_type: Option<String>,
}

/// Rows of one car and one table.
#[derive(Debug, Clone, PartialEq)]
pub enum Batch {
    Positions(Vec<PositionRow>),
    Drives(Vec<DriveRow>),
    Charges(Vec<ChargeRow>),
    ChargePoints(Vec<ChargePointRow>),
    States(Vec<StateRow>),
    Updates(Vec<UpdateRow>),
    /// Rows an earlier import stored that are gone now, by table and id (a TeslaFi drive over a month's end, stored
    /// in two halves, joined into one: the later half goes). Only TeslaFi imports send these.
    Removed(Vec<(crate::Table, i64)>),
    /// Stretches of time `[from_ms, to_ms]` an earlier import from the same site filled and this one reads again (a
    /// TeslaFi month imported before in another time zone or other units): every row of that site stored for the car
    /// in them goes (positions and charge points in them; drives, charges, states and updates starting in them)
    /// before the rows that replace them come. Only TeslaFi imports send these.
    Cleared(Vec<(u64, u64)>),
}

impl Batch {
    pub fn len(&self) -> usize {
        match self {
            Batch::Positions(v) => v.len(),
            Batch::Drives(v) => v.len(),
            Batch::Charges(v) => v.len(),
            Batch::ChargePoints(v) => v.len(),
            Batch::States(v) => v.len(),
            Batch::Updates(v) => v.len(),
            Batch::Removed(v) => v.len(),
            Batch::Cleared(v) => v.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Receives an import. Each car's rows come on their own (`batch` names the car), so the caller can hand them to
/// that car's own lock or queue and work on cars in parallel; nothing needs every car at once.
pub trait Sink {
    /// A car of the backup: which car it is, or `None` to leave its rows out. Asked once per car, before any of its
    /// rows (also when the import later stops on a broken backup, so don't count on rows following). Two TeslaMate
    /// cars may be the same car: the first one's state is used for both.
    fn car(&mut self, car: &TmCar) -> Result<Option<Target>, SinkError>;
    /// Rows of one car (never empty, at most [`Options::batch_rows`](crate::Options)).
    fn batch(&mut self, car: CarId, batch: Batch) -> Result<(), SinkError>;
    /// Geofences. All of them come every time (they are few, and TeslaMate lets them be edited): keep them by
    /// `tm_id`, replacing.
    fn places(&mut self, rows: Vec<PlaceRow>) -> Result<(), SinkError>;
    /// Addresses. All of them come every time too (TeslaMate can look them up again): keep them by `tm_id`.
    fn addresses(&mut self, rows: Vec<AddressRow>) -> Result<(), SinkError>;
    /// A car of the backup is done, once every row of every car was handed over: what became of its rows, and (for
    /// a car imported into) its new state to keep for the next import, now that its rows are stored. Comes once per
    /// car of the backup, at the end, one car at a time; a second TeslaMate car that is the same car as an earlier
    /// one comes with `None` (the first one carried the state). Not called when the import stops before its end.
    fn car_done(&mut self, report: CarReport, state: Option<ImportState>) -> Result<(), SinkError>;
    /// The time zone (IANA) of a car [`Sink::car`] gave, for an import whose times are local wall time (TeslaFi's):
    /// `None` when it has none.
    fn time_zone(&mut self, _car: CarId) -> Option<String> {
        None
    }
}
