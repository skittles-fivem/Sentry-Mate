//! TeslaMate's tables as its backup holds them: which are read, and which of their columns, by name.
//!
//! Columns are found by name in each block's `COPY` line, never by position, so a backup from an older or newer
//! TeslaMate reads the same: a column it doesn't have reads as absent (empty), and a column this importer doesn't know
//! is passed over and listed in the report. Each table also lists the columns TeslaMate has that are deliberately not
//! used (climate settings, Tesla's own ids), so "unknown" means new to this importer, not merely unused. TeslaMate
//! keeps everything metric (km, km/h, °C, kW, kWh, bar, m) and its times in UTC, whatever its display settings say,
//! so no unit depends on the owner's settings.

use crate::copy::{CopyHeader, RawRow};
use crate::value::{self, Field, Val};
use serde::{Deserialize, Serialize};
use std::cell::Cell;

/// A TeslaMate table the importer reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Table {
    Cars,
    CarSettings,
    Addresses,
    Geofences,
    Positions,
    Drives,
    ChargingProcesses,
    Charges,
    States,
    Updates,
}

impl Table {
    pub const ALL: [Table; 10] = [
        Table::Cars,
        Table::CarSettings,
        Table::Addresses,
        Table::Geofences,
        Table::Positions,
        Table::Drives,
        Table::ChargingProcesses,
        Table::Charges,
        Table::States,
        Table::Updates,
    ];

    /// The tables that hold one car's history (their rows go to that car).
    pub const PER_CAR: [Table; 6] =
        [Table::Positions, Table::Drives, Table::ChargingProcesses, Table::Charges, Table::States, Table::Updates];

    /// The name TeslaMate gives it.
    pub fn name(self) -> &'static str {
        match self {
            Table::Cars => "cars",
            Table::CarSettings => "car_settings",
            Table::Addresses => "addresses",
            Table::Geofences => "geofences",
            Table::Positions => "positions",
            Table::Drives => "drives",
            Table::ChargingProcesses => "charging_processes",
            Table::Charges => "charges",
            Table::States => "states",
            Table::Updates => "updates",
        }
    }

    pub fn from_name(name: &str) -> Option<Table> {
        Table::ALL.into_iter().find(|t| t.name() == name)
    }

    pub fn per_car(self) -> bool {
        Table::PER_CAR.contains(&self)
    }

    /// Its place in [`Table::PER_CAR`].
    pub(crate) fn car_ix(self) -> Option<usize> {
        Table::PER_CAR.iter().position(|t| *t == self)
    }

    pub(crate) fn used(self) -> &'static [&'static str] {
        match self {
            Table::Cars => Car::USED,
            Table::CarSettings => CarSetting::USED,
            Table::Addresses => Addr::USED,
            Table::Geofences => Geo::USED,
            Table::Positions => Pos::USED,
            Table::Drives => Drv::USED,
            Table::ChargingProcesses => Cp::USED,
            Table::Charges => Chg::USED,
            Table::States => St::USED,
            Table::Updates => Upd::USED,
        }
    }

    fn known_unused(self) -> &'static [&'static str] {
        match self {
            Table::Cars => Car::KNOWN,
            Table::CarSettings => CarSetting::KNOWN,
            Table::Addresses => Addr::KNOWN,
            Table::Geofences => Geo::KNOWN,
            Table::Positions => Pos::KNOWN,
            Table::Drives => Drv::KNOWN,
            Table::ChargingProcesses => Cp::KNOWN,
            Table::Charges => Chg::KNOWN,
            Table::States => St::KNOWN,
            Table::Updates => Upd::KNOWN,
        }
    }

    /// Without these the table can't be read at all.
    fn required(self) -> &'static [&'static str] {
        match self {
            Table::Cars | Table::CarSettings | Table::Addresses => &["id"],
            Table::Geofences => &["id", "latitude", "longitude"],
            Table::Positions => &["id", "car_id", "date", "latitude", "longitude"],
            Table::Drives | Table::ChargingProcesses | Table::States | Table::Updates => {
                &["id", "car_id", "start_date", "end_date"]
            }
            Table::Charges => &["id", "charging_process_id", "date"],
        }
    }
}

/// What a `COPY` block is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    Ours(Table),
    /// Tesla's tokens (`private.*`, and `tokens` wherever it is): never read.
    Secret,
    /// Anything else (TeslaMate's settings, its migrations list, its own import bookkeeping, other schemas).
    Other,
}

pub fn classify(h: &CopyHeader) -> Class {
    let schema = h.schema.as_deref().unwrap_or("public");
    if schema == "private" || h.table == "tokens" {
        return Class::Secret;
    }
    if schema != "public" {
        return Class::Other;
    }
    Table::from_name(&h.table).map_or(Class::Other, Class::Ours)
}

/// Where each used column is in a block's rows.
#[derive(Debug, Clone)]
pub struct ColumnMap {
    pub table: Table,
    /// By used column: its position in the row.
    at: Vec<Option<usize>>,
    /// How many fields each row has.
    pub width: usize,
    /// Columns this importer doesn't know.
    pub unknown: Vec<String>,
    /// Used columns this backup doesn't have.
    pub missing: Vec<&'static str>,
    /// Required columns it doesn't have (the table can't be read).
    pub missing_required: Vec<&'static str>,
}

impl ColumnMap {
    pub fn new(table: Table, header: &CopyHeader) -> ColumnMap {
        let used = table.used();
        let at: Vec<Option<usize>> = used.iter().map(|c| header.columns.iter().position(|h| h == c)).collect();
        let unknown = header
            .columns
            .iter()
            .filter(|h| !used.contains(&h.as_str()) && !table.known_unused().contains(&h.as_str()))
            .cloned()
            .collect();
        let missing: Vec<&'static str> = used.iter().zip(&at).filter(|(_, a)| a.is_none()).map(|(c, _)| *c).collect();
        let missing_required = table.required().iter().filter(|c| missing.contains(c)).copied().collect();
        ColumnMap { table, at, width: header.columns.len(), unknown, missing, missing_required }
    }

    pub fn row<'r, 'a>(&'r self, raw: &'r RawRow<'a>) -> Row<'r, 'a> {
        Row { raw, at: &self.at, names: self.table.used(), bad: Cell::new(0) }
    }
}

/// A table's columns, by name.
pub trait Columns: Copy {
    /// The columns read, in the order of the enum.
    const USED: &'static [&'static str];
    /// TeslaMate's columns that are deliberately not used.
    const KNOWN: &'static [&'static str];
    fn ix(self) -> usize;
}

macro_rules! columns {
    ($(#[$m:meta])* $name:ident { $($col:ident),* $(,)? } unused [$($k:literal),* $(,)?]) => {
        $(#[$m])*
        #[allow(non_camel_case_types)]
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum $name { $($col),* }
        impl Columns for $name {
            const USED: &'static [&'static str] = &[$(stringify!($col)),*];
            const KNOWN: &'static [&'static str] = &[$($k),*];
            fn ix(self) -> usize { self as usize }
        }
    };
}

columns!(
    /// `cars`: Tesla's own ids (`eid`, `vid`) are not kept.
    Car {
        id, vin, name, model, trim_badging, marketing_name, exterior_color, wheel_type, efficiency, settings_id,
        inserted_at
    }
    unused ["eid", "vid", "updated_at", "spoiler_type", "display_priority"]
);
columns!(
    /// `car_settings`: TeslaMate's polling settings mean nothing here.
    CarSetting { id, lfp_battery, free_supercharging }
    unused ["suspend_min", "suspend_after_idle_min", "req_not_unlocked", "use_streaming_api", "enabled"]
);
columns!(
    /// `addresses`: Nominatim's whole answer (`raw`) is not kept.
    Addr {
        id, display_name, latitude, longitude, name, house_number, road, neighbourhood, city, county, postcode, state,
        state_district, country, osm_id, osm_type
    }
    unused ["raw", "inserted_at", "updated_at"]
);
columns!(
    Geo { id, name, latitude, longitude, radius, cost_per_unit, session_fee, billing_type }
    unused ["inserted_at", "updated_at"]
);
columns!(
    /// `positions`: speed in km/h, power in kW (positive out of the pack), odometer and ranges in km, temperatures
    /// in °C, elevation in m, tire pressures in bar.
    Pos {
        id, car_id, drive_id, date, latitude, longitude, speed, power, odometer, ideal_battery_range_km,
        rated_battery_range_km, battery_level, usable_battery_level, outside_temp, inside_temp, elevation,
        tpms_pressure_fl, tpms_pressure_fr, tpms_pressure_rl, tpms_pressure_rr
    }
    unused [
        "est_battery_range_km", "fan_status", "driver_temp_setting", "passenger_temp_setting", "is_climate_on",
        "is_rear_defroster_on", "is_front_defroster_on", "battery_heater", "battery_heater_on",
        "battery_heater_no_power"
    ]
);
columns!(
    /// `drives`: odometer (`start_km`, `end_km`) and distance in km, speed in km/h, power in kW, ascent in m.
    Drv {
        id, car_id, start_date, end_date, outside_temp_avg, inside_temp_avg, speed_max, power_max, power_min,
        start_rated_range_km, end_rated_range_km, start_km, end_km, distance, start_address_id, end_address_id,
        start_position_id, end_position_id, start_geofence_id, end_geofence_id, ascent, descent
    }
    unused ["duration_min", "start_ideal_range_km", "end_ideal_range_km"]
);
columns!(
    /// `charging_processes`: one charge; energy in kWh.
    Cp {
        id, car_id, start_date, end_date, charge_energy_added, charge_energy_used, start_battery_level,
        end_battery_level, start_rated_range_km, end_rated_range_km, outside_temp_avg, cost, position_id, address_id,
        geofence_id
    }
    unused ["duration_min", "start_ideal_range_km", "end_ideal_range_km"]
);
columns!(
    /// `charges`: the points of a charge's curve (power in kW, the charger's volts and amps).
    Chg {
        id, charging_process_id, date, battery_level, usable_battery_level, charge_energy_added,
        charger_actual_current, charger_phases, charger_power, charger_voltage, fast_charger_present,
        rated_battery_range_km, ideal_battery_range_km, outside_temp
    }
    unused [
        "battery_heater_on", "battery_heater", "battery_heater_no_power", "charger_pilot_current", "conn_charge_cable",
        "fast_charger_brand", "fast_charger_type", "not_enough_power_to_heat"
    ]
);
columns!(St { id, car_id, state, start_date, end_date } unused []);
columns!(Upd { id, car_id, start_date, end_date, version } unused []);

/// One row, read by column.
pub struct Row<'r, 'a> {
    raw: &'r RawRow<'a>,
    at: &'r [Option<usize>],
    names: &'static [&'static str],
    /// Optional columns whose value was there but unreadable (read as empty), one bit per used column.
    bad: Cell<u64>,
}

impl Row<'_, '_> {
    pub fn field(&self, c: impl Columns) -> Field<'_> {
        match self.at.get(c.ix()).copied().flatten() {
            Some(p) => self.raw.field(p),
            None => Field::Absent,
        }
    }

    pub fn val<T>(&self, c: impl Columns, parse: fn(&[u8]) -> Option<T>) -> Val<T> {
        match self.field(c) {
            Field::Absent => Val::Absent,
            Field::Null => Val::Null,
            Field::Raw(b) => parse(b).map_or(Val::Bad, Val::Ok),
        }
    }

    fn opt<T>(&self, c: impl Columns, parse: fn(&[u8]) -> Option<T>) -> Option<T> {
        match self.val(c, parse) {
            Val::Ok(v) => Some(v),
            Val::Bad => {
                self.bad.set(self.bad.get() | 1u64.checked_shl(c.ix() as u32).unwrap_or(0));
                None
            }
            Val::Absent | Val::Null => None,
        }
    }

    /// A value that must be there: `Err` names the column.
    fn req<T>(&self, c: impl Columns, parse: fn(&[u8]) -> Option<T>) -> Result<T, &'static str> {
        match self.val(c, parse) {
            Val::Ok(v) => Ok(v),
            _ => Err(self.names.get(c.ix()).copied().unwrap_or("?")),
        }
    }

    pub fn f64(&self, c: impl Columns) -> Option<f64> {
        self.opt(c, value::number)
    }

    pub fn i64(&self, c: impl Columns) -> Option<i64> {
        self.opt(c, value::int)
    }

    pub fn bool(&self, c: impl Columns) -> Option<bool> {
        self.opt(c, value::boolean)
    }

    pub fn ms(&self, c: impl Columns) -> Option<u64> {
        self.opt(c, value::timestamp_ms)
    }

    /// Text; an empty string reads as none.
    pub fn text(&self, c: impl Columns) -> Option<String> {
        self.opt(c, |b| Some(value::text(b))).filter(|s| !s.is_empty())
    }

    pub fn req_f64(&self, c: impl Columns) -> Result<f64, &'static str> {
        self.req(c, value::number)
    }

    pub fn req_i64(&self, c: impl Columns) -> Result<i64, &'static str> {
        self.req(c, value::int)
    }

    pub fn req_ms(&self, c: impl Columns) -> Result<u64, &'static str> {
        self.req(c, value::timestamp_ms)
    }

    /// The names of optional columns that were unreadable in this row.
    pub fn bad_columns(&self) -> impl Iterator<Item = &'static str> + '_ {
        let bad = self.bad.get();
        self.names.iter().enumerate().filter(move |(i, _)| bad & (1u64 << i) != 0).map(|(_, n)| *n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn column_lists_fit_and_dont_overlap() {
        for t in Table::ALL {
            let used = t.used();
            assert!(used.len() <= 64, "{t:?}");
            for c in t.required() {
                assert!(used.contains(c), "{t:?} requires {c}");
            }
            for c in t.known_unused() {
                assert!(!used.contains(c), "{t:?}: {c}");
            }
            assert_eq!(Table::from_name(t.name()), Some(t));
        }
    }

    fn header(schema: Option<&str>, table: &str) -> CopyHeader {
        CopyHeader { schema: schema.map(String::from), table: table.into(), columns: Vec::new() }
    }

    #[test]
    fn classes() {
        assert_eq!(classify(&header(Some("public"), "drives")), Class::Ours(Table::Drives));
        assert_eq!(classify(&header(None, "positions")), Class::Ours(Table::Positions));
        assert_eq!(classify(&header(Some("private"), "tokens")), Class::Secret);
        assert_eq!(classify(&header(Some("private"), "anything")), Class::Secret);
        assert_eq!(classify(&header(Some("public"), "tokens")), Class::Secret);
        assert_eq!(classify(&header(None, "tokens")), Class::Secret);
        assert_eq!(classify(&header(Some("public"), "settings")), Class::Other);
        assert_eq!(classify(&header(Some("other"), "drives")), Class::Other);
    }
}
