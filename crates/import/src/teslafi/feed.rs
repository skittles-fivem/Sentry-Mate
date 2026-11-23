//! One answer of TeslaFi's API (`feed.php`), read with the same rules as a row of its download: the API's keys are
//! the download's column names, so [`super::poll::read`] reads both. Values come as text (`"237.17"`, `"1"`, `null`,
//! `""` or `"NULL"`); [`fields_of`] turns the JSON object into text values first.
//!
//! Units as the download's: the base columns are Tesla's (miles, °C) unless the row itself shows otherwise (the
//! `...F` columns beside them, `gui_*_units`, `measure`), the same detection the import runs on a file's first rows.
//! `Date` is TeslaFi's wall time in the account's time zone, turned into UTC as the import does.

use super::poll::{self, Col, Row, Utc};
use super::{CarState, Detect, Options, Units};
use crate::vin_info;
use std::collections::BTreeMap;

/// One poll of TeslaFi's, in Sentry Mate's units.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FeedPoll {
    /// When TeslaFi polled the car (UTC ms), not when it was read.
    pub t_ms: u64,
    /// TeslaFi's id of the poll: the same id twice is the same poll.
    pub data_id: Option<i64>,
    pub state: Option<CarState>,
    /// TeslaFi's own word for what the car does (`Driving`, `Charging`, `Idling`, `Sleeping`, `Sentry`...).
    pub car_state: Option<String>,
    pub sample: model::Sample,
    pub driving: bool,
    pub charging: bool,
    /// The software version without its build hash.
    pub version: Option<String>,
    /// The whole VIN, upper case (only when it reads as one).
    pub vin: Option<String>,
    pub display_name: Option<String>,
    pub usable_soc: Option<f64>,
    pub energy_added_kwh: Option<f64>,
    /// The values no CAN message carries, as the poll gave them ([`off_can`]).
    pub off_can: model::OffCan,
    /// The units the values were read in.
    pub units: Option<Units>,
    /// Values there but not numbers, by column.
    pub bad: BTreeMap<&'static str, u64>,
}

impl FeedPoll {
    /// Whether it says anything worth keeping as a point (a position, a level, the odometer or the range).
    pub fn has_values(&self) -> bool {
        let s = &self.sample;
        s.lat.is_some() || s.soc.is_some() || s.odometer_km.is_some() || s.range_km.is_some()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum FeedError {
    #[error("the answer has no time TeslaFi polled the car at")]
    NoTime,
    #[error("the answer's time is one the clocks skip in that time zone")]
    SkippedTime,
}

/// A JSON value as the download would have it: text as it is, numbers as written, booleans `1`/`0`, null as empty.
/// Objects and lists have no column value: `None`.
pub fn text_of(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::Null => Some(String::new()),
        serde_json::Value::String(t) => Some(t.clone()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        serde_json::Value::Bool(b) => Some(if *b { "1" } else { "0" }.to_string()),
        _ => None,
    }
}

/// An answer's keys and text values (objects and lists left out, as the download has no column for them).
pub fn fields_of(o: &serde_json::Map<String, serde_json::Value>) -> BTreeMap<String, String> {
    o.iter().filter_map(|(k, v)| Some((k.clone(), text_of(v)?))).collect()
}

/// A field's text, `None` when it is missing or no value (`""`, `NULL`, ...).
fn field<'a>(fields: &'a BTreeMap<String, String>, key: &str) -> Option<&'a str> {
    let v = fields.get(key)?.trim();
    (!(v.is_empty()
        || v.eq_ignore_ascii_case("none")
        || v.eq_ignore_ascii_case("null")
        || v.eq_ignore_ascii_case("nan")
        || v.eq_ignore_ascii_case("<invalid>")))
    .then_some(v)
}

fn field_num(fields: &BTreeMap<String, String>, key: &str) -> Option<f64> {
    field(fields, key)?.parse::<f64>().ok().filter(|x| x.is_finite())
}

/// `1`/`0` (or `true`/`false`); anything else is no value.
fn field_bool(fields: &BTreeMap<String, String>, key: &str) -> Option<bool> {
    match field(fields, key)?.to_ascii_lowercase().as_str() {
        "1" | "true" => Some(true),
        "0" | "false" => Some(false),
        _ => None,
    }
}

fn field_word(fields: &BTreeMap<String, String>, key: &str, n: usize) -> Option<String> {
    field(fields, key).map(|v| v.chars().filter(|c| !c.is_control()).take(n).collect())
}

/// What one answer tells of the values no CAN message carries: the `vehicle_data` key names without their sections
/// (`locked`, `fd_window`, `sun_roof_state`, `scheduled_charging_start_time`, `active_route_destination` ...), and
/// TeslaFi's `newVersion` / `newVersionStatus` for the waiting update. `version`: the poll's software version;
/// `distance`: what the answer's distances are in.
pub fn off_can(fields: &BTreeMap<String, String>, version: Option<String>, distance: super::Distance) -> model::OffCan {
    let km = |x: f64| match distance {
        super::Distance::Mi => x * poll::KM_PER_MI,
        super::Distance::Km => x,
    };
    let secs = |key: &str| field_num(fields, key).filter(|t| *t > 0.0 && t.fract() == 0.0).map(|t| t as u64 * 1000);
    let window = |key: &str| field_num(fields, key).map(|n| n != 0.0);
    model::OffCan {
        version,
        update_status: field_word(fields, "newVersionStatus", 32).map(|w| w.to_ascii_lowercase()),
        update_version: field(fields, "newVersion")
            .and_then(|w| w.split_whitespace().next())
            .map(|w| w.chars().take(32).collect()),
        update_download_pct: field_num(fields, "download_perc").filter(|p| (0.0..=100.0).contains(p)),
        locked: field_bool(fields, "locked"),
        windows: ["fd_window", "fp_window", "rd_window", "rp_window"].map(window),
        windows_open: None,
        sunroof_state: field_word(fields, "sun_roof_state", 32).map(|w| w.to_ascii_lowercase()),
        sunroof_pct: field_num(fields, "sun_roof_percent_open").filter(|p| (0.0..=100.0).contains(p)),
        scheduled_charge_start_ms: secs("scheduled_charging_start_time"),
        scheduled_departure_ms: secs("scheduled_departure_time"),
        route_destination: field_word(fields, "active_route_destination", 64),
        route_km: field_num(fields, "active_route_miles_to_arrival").filter(|x| *x >= 0.0).map(km),
        route_min: field_num(fields, "active_route_minutes_to_arrival").filter(|x| *x >= 0.0),
        route_traffic_min: field_num(fields, "active_route_traffic_minutes_delay").filter(|x| *x >= 0.0),
    }
}

/// Reads one answer (`fields`: its keys and text values) with `tz` for its wall time. `prev_ms`: the poll before,
/// for the hour the clocks go back.
pub fn read(fields: &BTreeMap<String, String>, tz: chrono_tz::Tz, prev_ms: Option<u64>) -> Result<FeedPoll, FeedError> {
    let row = Row::Map(fields);
    let local = row.text(Col::Date).as_deref().and_then(poll::parse_local).ok_or(FeedError::NoTime)?;
    let t_ms = match poll::to_utc(tz, local, prev_ms) {
        Utc::One(t) | Utc::Repeated(t) => t,
        Utc::Skipped => return Err(FeedError::SkippedTime),
    };
    let mut d = Detect::default();
    d.see(&row);
    let units = d.units(&Options::default());
    let mut bad = BTreeMap::new();
    let mut p = poll::read(&row, units.distance, units.temperature, &mut bad);
    p.t_ms = t_ms;
    let vin = row.text(Col::Vin).filter(|v| vin_info(v).is_some()).map(|v| v.trim().to_ascii_uppercase());
    let mut sample = p.sample();
    sample.t_ms = t_ms - t_ms % 1000;
    Ok(FeedPoll {
        t_ms,
        data_id: p.data_id,
        state: p.state,
        car_state: row.text(Col::CarState).map(|s| s.chars().filter(|c| !c.is_control()).take(32).collect()),
        driving: p.driving() || p.moving(),
        charging: p.charging,
        sample,
        version: p.version.clone(),
        vin,
        display_name: row
            .text(Col::DisplayName)
            .map(|n| n.trim().chars().filter(|c| !c.is_control()).take(64).collect::<String>())
            .filter(|n| !n.is_empty()),
        usable_soc: p.usable_soc,
        energy_added_kwh: p.energy_added_kwh,
        off_can: off_can(fields, p.version, units.distance),
        units: Some(units),
        bad,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::float_cmp)]
    use super::*;
    use model::{ChargeKind, Gear};

    fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect()
    }

    #[test]
    fn a_drive_answer_in_teslas_units_and_the_accounts_zone() {
        let m = map(&[
            ("data_id", "123456"),
            ("Date", "2026-07-04 14:05:09"),
            ("state", "online"),
            ("carState", "Driving"),
            ("shift_state", "D"),
            ("speed", "50"),
            ("latitude", "12.5"),
            ("longitude", "-45.25"),
            ("odometer", "10000.0"),
            ("odometerF", "16093.4"),
            ("battery_level", "64"),
            ("battery_range", "200.0"),
            ("inside_temp", "21.5"),
            ("inside_tempF", "70.7"),
            ("outside_temp", "NULL"),
            ("charging_state", "Disconnected"),
            ("car_version", "2026.32.5 abcdef"),
            ("vin", "5yj3e1ea0pf000123"),
            ("display_name", "Made-up car"),
        ]);
        let tz: chrono_tz::Tz = "America/Chicago".parse().unwrap();
        let p = read(&m, tz, None).unwrap();
        // 14:05:09 in Chicago in July (UTC-5)
        assert_eq!(p.t_ms, 1_783_191_909_000);
        assert_eq!(p.data_id, Some(123_456));
        assert_eq!((p.sample.lat, p.sample.lon), (Some(12.5), Some(-45.25)));
        assert_eq!(p.sample.speed_kmh, Some(80.5));
        assert_eq!(p.sample.gear, Some(Gear::D));
        assert_eq!(p.sample.odometer_km, Some(16093.44));
        assert_eq!(p.sample.range_km, Some(321.87));
        assert_eq!(p.sample.inside_temp_c, Some(21.5));
        assert_eq!(p.sample.outside_temp_c, None, "the string NULL is no value");
        assert_eq!(p.sample.charging, Some(false));
        assert!(p.driving && !p.charging);
        assert_eq!(p.version.as_deref(), Some("2026.32.5"));
        assert_eq!(p.vin.as_deref(), Some("5YJ3E1EA0PF000123"));
        assert_eq!(p.car_state.as_deref(), Some("Driving"));
        assert_eq!(p.state, Some(CarState::Online));
        assert!(p.has_values());
        let u = p.units.unwrap();
        assert_eq!((u.distance, u.temperature), (super::super::Distance::Mi, super::super::Temperature::C));
    }

    #[test]
    fn a_sleeping_answer_has_no_values_and_a_charge_reads_as_one() {
        let tz = chrono_tz::Tz::UTC;
        let m = map(&[("data_id", "9"), ("Date", "2026-07-04 03:00:00"), ("carState", "Sleeping"), ("state", "")]);
        let p = read(&m, tz, None).unwrap();
        assert_eq!(p.state, Some(CarState::Asleep));
        assert!(!p.has_values());
        let m = map(&[
            ("Date", "2026-07-04 03:00:00"),
            ("carState", "Charging"),
            ("charging_state", "Charging"),
            ("fast_charger_present", "1"),
            ("charger_power", "120"),
            ("battery_level", "40"),
        ]);
        let p = read(&m, tz, None).unwrap();
        assert!(p.charging);
        assert_eq!(p.sample.charge_kind, Some(ChargeKind::Dc));
        assert_eq!(p.sample.charge_power_kw, Some(120.0));
        assert!(read(&map(&[("carState", "Idling")]), tz, None).is_err(), "no Date: no poll");
    }

    #[test]
    fn values_not_on_the_can_bus() {
        let m = map(&[
            ("Date", "2026-07-04 03:00:00"),
            ("car_version", "2026.32.5 abcdef"),
            ("newVersion", "2026.38.1 ffff"),
            ("newVersionStatus", "Downloading"),
            ("download_perc", "40"),
            ("locked", "1"),
            ("fd_window", "0"),
            ("fp_window", "0"),
            ("rd_window", "3"),
            ("rp_window", "NULL"),
            ("sun_roof_state", "Closed"),
            ("sun_roof_percent_open", "0"),
            ("scheduled_charging_start_time", "1800003600"),
            ("scheduled_departure_time", ""),
            ("active_route_destination", "Made-up Street 1"),
            ("active_route_miles_to_arrival", "10"),
            ("active_route_minutes_to_arrival", "14.5"),
            ("active_route_traffic_minutes_delay", "NULL"),
        ]);
        let o = read(&m, chrono_tz::Tz::UTC, None).unwrap().off_can;
        assert_eq!(o.version.as_deref(), Some("2026.32.5"));
        assert_eq!(o.update_status.as_deref(), Some("downloading"));
        assert_eq!(o.update_version.as_deref(), Some("2026.38.1"));
        assert_eq!(o.update_download_pct, Some(40.0));
        assert_eq!(o.locked, Some(true));
        assert_eq!(o.windows, [Some(false), Some(false), Some(true), None]);
        assert_eq!((o.sunroof_state.as_deref(), o.sunroof_pct), (Some("closed"), Some(0.0)));
        assert_eq!((o.scheduled_charge_start_ms, o.scheduled_departure_ms), (Some(1_800_003_600_000), None));
        assert_eq!(o.route_destination.as_deref(), Some("Made-up Street 1"));
        assert!(o.route_km.is_some_and(|k| (k - 16.09344).abs() < 1e-9));
        assert_eq!((o.route_min, o.route_traffic_min), (Some(14.5), None));
        let bare = read(
            &map(&[("Date", "2026-07-04 03:00:00"), ("locked", "NULL"), ("newVersionStatus", "")]),
            chrono_tz::Tz::UTC,
            None,
        );
        assert!(bare.unwrap().off_can.is_empty(), "NULL and empty are no value");
    }

    #[test]
    fn json_values_as_the_download_writes_them() {
        let v = serde_json::json!({"a": null, "b": true, "c": 12.5, "d": "D", "e": {"commands": 1}, "f": "NULL"});
        let f = fields_of(v.as_object().unwrap());
        assert_eq!(f.get("a").map(String::as_str), Some(""));
        assert_eq!(f.get("b").map(String::as_str), Some("1"));
        assert_eq!(f.get("c").map(String::as_str), Some("12.5"));
        assert_eq!(f.get("d").map(String::as_str), Some("D"));
        assert!(!f.contains_key("e"), "an object is no column");
        let m = map(&[("Date", "2026-07-04 03:00:00"), ("battery_level", "NULL"), ("odometer", "x")]);
        let p = read(&m, chrono_tz::Tz::UTC, None).unwrap();
        assert_eq!(p.sample.soc, None);
        assert_eq!(p.bad.get("odometer"), Some(&1), "a value that is not a number is counted, not read as 0");
    }
}
