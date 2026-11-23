//! One TeslaFi row (a poll of the car) read by column name: its time, what the car said, in Sentry Mate's units.

use super::csv::Fields;
use super::{Distance, Temperature};
use chrono::{LocalResult, NaiveDate, NaiveDateTime, TimeZone};
use model::{ChargeKind, Gear};
use std::collections::BTreeMap;

/// The columns the importer reads (TeslaFi's names, matched without regard to case).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Col {
    DataId,
    Date,
    State,
    CarState,
    ShiftState,
    ChargingState,
    Latitude,
    Longitude,
    Heading,
    Elevation,
    Speed,
    Power,
    Odometer,
    OdometerF,
    BatteryLevel,
    UsableBatteryLevel,
    BatteryRange,
    IdealBatteryRange,
    ChargeEnergyAdded,
    ChargerPower,
    ChargerVoltage,
    ChargerActualCurrent,
    ChargerPhases,
    FastChargerPresent,
    FastChargerType,
    InsideTemp,
    OutsideTemp,
    InsideTempF,
    OutsideTempF,
    CarVersion,
    Vin,
    DisplayName,
    VehicleId,
    Measure,
    GuiDistanceUnits,
    GuiTemperatureUnits,
}

impl Col {
    pub const ALL: [Col; 36] = [
        Col::DataId,
        Col::Date,
        Col::State,
        Col::CarState,
        Col::ShiftState,
        Col::ChargingState,
        Col::Latitude,
        Col::Longitude,
        Col::Heading,
        Col::Elevation,
        Col::Speed,
        Col::Power,
        Col::Odometer,
        Col::OdometerF,
        Col::BatteryLevel,
        Col::UsableBatteryLevel,
        Col::BatteryRange,
        Col::IdealBatteryRange,
        Col::ChargeEnergyAdded,
        Col::ChargerPower,
        Col::ChargerVoltage,
        Col::ChargerActualCurrent,
        Col::ChargerPhases,
        Col::FastChargerPresent,
        Col::FastChargerType,
        Col::InsideTemp,
        Col::OutsideTemp,
        Col::InsideTempF,
        Col::OutsideTempF,
        Col::CarVersion,
        Col::Vin,
        Col::DisplayName,
        Col::VehicleId,
        Col::Measure,
        Col::GuiDistanceUnits,
        Col::GuiTemperatureUnits,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Col::DataId => "data_id",
            Col::Date => "Date",
            Col::State => "state",
            Col::CarState => "carState",
            Col::ShiftState => "shift_state",
            Col::ChargingState => "charging_state",
            Col::Latitude => "latitude",
            Col::Longitude => "longitude",
            Col::Heading => "heading",
            Col::Elevation => "elevation",
            Col::Speed => "speed",
            Col::Power => "power",
            Col::Odometer => "odometer",
            Col::OdometerF => "odometerF",
            Col::BatteryLevel => "battery_level",
            Col::UsableBatteryLevel => "usable_battery_level",
            Col::BatteryRange => "battery_range",
            Col::IdealBatteryRange => "ideal_battery_range",
            Col::ChargeEnergyAdded => "charge_energy_added",
            Col::ChargerPower => "charger_power",
            Col::ChargerVoltage => "charger_voltage",
            Col::ChargerActualCurrent => "charger_actual_current",
            Col::ChargerPhases => "charger_phases",
            Col::FastChargerPresent => "fast_charger_present",
            Col::FastChargerType => "fast_charger_type",
            Col::InsideTemp => "inside_temp",
            Col::OutsideTemp => "outside_temp",
            Col::InsideTempF => "inside_tempF",
            Col::OutsideTempF => "outside_tempF",
            Col::CarVersion => "car_version",
            Col::Vin => "vin",
            Col::DisplayName => "display_name",
            Col::VehicleId => "vehicle_id",
            Col::Measure => "measure",
            Col::GuiDistanceUnits => "gui_distance_units",
            Col::GuiTemperatureUnits => "gui_temperature_units",
        }
    }
}

/// Columns of TeslaFi's raw data a file must have some of to be taken for one.
const MARKS: [&str; 11] = [
    "data_id",
    "shift_state",
    "charging_state",
    "battery_level",
    "odometer",
    "latitude",
    "longitude",
    "state",
    "carstate",
    "battery_range",
    "car_version",
];

/// What a header line is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HeaderKind {
    /// TeslaFi's raw data (its monthly download).
    Raw,
    /// TeslaFi's list of drives or charges (one row each, no positions).
    Summary,
    Other,
}

/// What a header says: where each column is.
#[derive(Debug, Clone)]
pub(crate) struct Header {
    at: BTreeMap<Col, usize>,
    /// Columns not read here, and how many.
    pub unknown: usize,
    pub fields: usize,
}

pub(crate) fn header_kind(names: &[String]) -> HeaderKind {
    let low: Vec<String> = names.iter().map(|n| n.trim().to_ascii_lowercase()).collect();
    let has = |n: &str| low.iter().any(|x| x == n);
    if has("date") && MARKS.iter().filter(|m| has(m)).count() >= 4 {
        HeaderKind::Raw
    } else if has("start time") && has("end time") && !has("data_id") {
        HeaderKind::Summary
    } else {
        HeaderKind::Other
    }
}

impl Header {
    pub fn new(names: &[String]) -> Header {
        let mut at = BTreeMap::new();
        let mut known = 0;
        for (i, n) in names.iter().enumerate() {
            let n = n.trim();
            if let Some(c) = Col::ALL.iter().find(|c| c.name().eq_ignore_ascii_case(n)) {
                at.entry(*c).or_insert(i);
                known += 1;
            }
        }
        Header { at, unknown: names.len().saturating_sub(known), fields: names.len() }
    }

    pub fn has(&self, c: Col) -> bool {
        self.at.contains_key(&c)
    }

    /// The columns read here that this file lacks.
    pub fn missing(&self) -> Vec<&'static str> {
        Col::ALL.iter().filter(|c| !self.has(**c)).map(|c| c.name()).collect()
    }
}

/// A row's fields by column: a line of the CSV download, or one answer of TeslaFi's API (its keys are the same
/// column names; values as text, see [`super::feed`]).
pub(crate) enum Row<'a> {
    Csv { h: &'a Header, f: &'a Fields, line: &'a str },
    Map(&'a BTreeMap<String, String>),
}

/// Empty, `None`, `null`, `NaN` and `<invalid>`: no value.
fn is_null(s: &str) -> bool {
    s.is_empty()
        || s.eq_ignore_ascii_case("none")
        || s.eq_ignore_ascii_case("null")
        || s.eq_ignore_ascii_case("nan")
        || s.eq_ignore_ascii_case("<invalid>")
}

impl Row<'_> {
    /// The value of a column as it stands (`None`: no such column).
    fn raw(&self, c: Col) -> Option<std::borrow::Cow<'_, str>> {
        match self {
            Row::Csv { h, f, line } => f.get(line, *h.at.get(&c)?),
            Row::Map(m) => m
                .get(c.name())
                .or_else(|| m.iter().find(|(k, _)| k.eq_ignore_ascii_case(c.name())).map(|(_, v)| v))
                .map(|v| std::borrow::Cow::Borrowed(v.trim())),
        }
    }

    /// Whether the row has the column at all.
    pub fn has(&self, c: Col) -> bool {
        match self {
            Row::Csv { h, .. } => h.has(c),
            Row::Map(_) => self.raw(c).is_some(),
        }
    }

    pub fn text(&self, c: Col) -> Option<String> {
        let v = self.raw(c)?;
        (!is_null(&v)).then(|| v.into_owned())
    }

    /// A number; `bad` is told a value that is there but not a number.
    pub fn num(&self, c: Col, bad: &mut BTreeMap<&'static str, u64>) -> Option<f64> {
        let v = self.raw(c)?;
        if is_null(&v) {
            return None;
        }
        match v.parse::<f64>() {
            Ok(x) if x.is_finite() => Some(x),
            _ => {
                *bad.entry(c.name()).or_default() += 1;
                None
            }
        }
    }

    pub fn flag(&self, c: Col) -> Option<bool> {
        let v = self.text(c)?;
        match v.to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" => Some(true),
            "false" | "0" | "no" => Some(false),
            _ => None,
        }
    }
}

/// TeslaFi's `state`: what the Tesla API said.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CarState {
    Online,
    Asleep,
    Offline,
}

/// A poll in Sentry Mate's units, with what the segments need.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Poll {
    pub t_ms: u64,
    pub data_id: Option<i64>,
    pub state: Option<CarState>,
    pub gear: Option<Gear>,
    pub charging: bool,
    /// `charging_state` was there (else charging isn't known).
    pub charging_known: bool,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    pub elevation_m: Option<f64>,
    pub speed_kmh: Option<f64>,
    pub power_kw: Option<f64>,
    pub odometer_km: Option<f64>,
    pub soc: Option<f64>,
    pub usable_soc: Option<f64>,
    pub range_km: Option<f64>,
    pub ideal_range_km: Option<f64>,
    pub energy_added_kwh: Option<f64>,
    pub charger_power_kw: Option<f64>,
    pub charger_voltage_v: Option<f64>,
    pub charger_current_a: Option<f64>,
    pub phases: Option<u8>,
    pub kind: Option<ChargeKind>,
    pub inside_temp_c: Option<f64>,
    pub outside_temp_c: Option<f64>,
    /// The software version without its build hash (`2023.2.10`).
    pub version: Option<String>,
}

impl Poll {
    pub fn driving(&self) -> bool {
        matches!(self.gear, Some(Gear::D | Gear::R | Gear::N))
    }

    pub fn moving(&self) -> bool {
        self.speed_kmh.is_some_and(|s| s > 0.5)
    }

    pub fn sample(&self) -> model::Sample {
        model::Sample {
            t_ms: self.t_ms,
            lat: self.lat,
            lon: self.lon,
            elevation_m: self.elevation_m,
            speed_kmh: self.speed_kmh,
            gear: self.gear,
            odometer_km: self.odometer_km,
            soc: self.soc,
            range_km: self.range_km,
            ideal_range_km: self.ideal_range_km,
            battery_power_kw: self.power_kw,
            outside_temp_c: self.outside_temp_c,
            inside_temp_c: self.inside_temp_c,
            charging: self.charging_known.then_some(self.charging),
            charge_kind: if self.charging { self.kind } else { None },
            charge_power_kw: if self.charging { self.charger_power_kw } else { None },
            charger_voltage_v: if self.charging { self.charger_voltage_v } else { None },
            charger_current_a: if self.charging { self.charger_current_a } else { None },
            // (the API's count as TeslaFi passes it on: 2 is three phases)
            extra: model::SampleExtra {
                charger_phases: self.phases.filter(|_| self.charging).and_then(|p| model::api_phases(f64::from(p))),
                ..model::SampleExtra::default()
            },
            ..model::Sample::default()
        }
    }

    /// Whether it says anything worth keeping as a point.
    pub fn worth_a_point(&self) -> bool {
        self.lat.is_some() || self.soc.is_some() || self.odometer_km.is_some() || self.range_km.is_some()
    }
}

pub(crate) const KM_PER_MI: f64 = 1.609_344;

fn round(x: f64, d: i32) -> f64 {
    let m = 10f64.powi(d);
    (x * m).round() / m
}

/// Reads a row's values (its time is set by the caller).
pub(crate) fn read(row: &Row<'_>, dist: Distance, temp: Temperature, bad: &mut BTreeMap<&'static str, u64>) -> Poll {
    let km = |x: f64| match dist {
        Distance::Mi => x * KM_PER_MI,
        Distance::Km => x,
    };
    let c = |x: f64| match temp {
        Temperature::C => x,
        Temperature::F => (x - 32.0) * 5.0 / 9.0,
    };
    let state = row.text(Col::State).and_then(|s| match s.to_ascii_lowercase().as_str() {
        "online" | "waking" | "driving" | "charging" => Some(CarState::Online),
        "asleep" | "sleeping" => Some(CarState::Asleep),
        "offline" => Some(CarState::Offline),
        _ => None,
    });
    let state = state.or_else(|| {
        row.text(Col::CarState).and_then(|s| match s.to_ascii_lowercase().as_str() {
            "sleeping" => Some(CarState::Asleep),
            "driving" | "charging" | "idling" | "sentry" => Some(CarState::Online),
            _ => None,
        })
    });
    let gear = row.text(Col::ShiftState).and_then(|s| match s.to_ascii_uppercase().as_str() {
        "P" => Some(Gear::P),
        "R" => Some(Gear::R),
        "N" => Some(Gear::N),
        "D" => Some(Gear::D),
        _ => None,
    });
    let charging_text = row.text(Col::ChargingState);
    let charging = charging_text.as_deref().is_some_and(|s| s.eq_ignore_ascii_case("charging"))
        || (charging_text.is_none() && row.text(Col::CarState).is_some_and(|s| s.eq_ignore_ascii_case("charging")));
    let lat = row.num(Col::Latitude, bad);
    let lon = row.num(Col::Longitude, bad);
    let (lat, lon) = match (lat, lon) {
        (Some(a), Some(o))
            if (-90.0..=90.0).contains(&a) && (-180.0..=180.0).contains(&o) && (a != 0.0 || o != 0.0) =>
        {
            (Some(a), Some(o))
        }
        (None, None) => (None, None),
        _ => {
            *bad.entry("latitude").or_default() += 1;
            (None, None)
        }
    };
    let pct = |c: Col, bad: &mut BTreeMap<&'static str, u64>| row.num(c, bad).map(|x| x.clamp(0.0, 100.0));
    let ideal = row.num(Col::IdealBatteryRange, bad).filter(|x| *x < 998.0 && *x >= 0.0).map(km);
    let fast = row.flag(Col::FastChargerPresent);
    let fast_type = row.text(Col::FastChargerType);
    let dc_type = fast_type.as_deref().is_some_and(|t| {
        let t = t.to_ascii_lowercase();
        ["supercharger", "tesla", "ccs", "chademo", "gb", "combo"].iter().any(|k| t.contains(k))
    });
    let kind = match (fast, dc_type) {
        (Some(true), _) | (_, true) => Some(ChargeKind::Dc),
        (Some(false), false) => Some(ChargeKind::Ac),
        (None, false) => None,
    };
    let version = row.text(Col::CarVersion).and_then(|v| {
        let v = v.split_whitespace().next().unwrap_or_default().to_string();
        (v.len() >= 5 && v.chars().next().is_some_and(|c| c.is_ascii_digit())).then_some(v)
    });
    Poll {
        t_ms: 0,
        data_id: row.text(Col::DataId).and_then(|s| s.parse().ok()),
        state,
        gear,
        charging,
        charging_known: charging_text.is_some() || row.has(Col::ChargingState),
        lat,
        lon,
        elevation_m: row.num(Col::Elevation, bad),
        speed_kmh: row.num(Col::Speed, bad).map(|s| round(km(s.max(0.0)), 1)),
        power_kw: row.num(Col::Power, bad),
        odometer_km: row.num(Col::Odometer, bad).filter(|x| *x >= 0.0).map(|x| round(km(x), 3)),
        soc: pct(Col::BatteryLevel, bad),
        usable_soc: pct(Col::UsableBatteryLevel, bad),
        range_km: row.num(Col::BatteryRange, bad).filter(|x| *x >= 0.0 && *x < 998.0).map(|x| round(km(x), 2)),
        ideal_range_km: ideal.map(|x| round(x, 2)),
        energy_added_kwh: row.num(Col::ChargeEnergyAdded, bad).filter(|x| *x >= 0.0),
        charger_power_kw: row.num(Col::ChargerPower, bad),
        charger_voltage_v: row.num(Col::ChargerVoltage, bad).map(|x| round(x, 1)),
        charger_current_a: row.num(Col::ChargerActualCurrent, bad).map(|x| round(x, 1)),
        phases: row.num(Col::ChargerPhases, bad).filter(|x| (1.0..=3.0).contains(x)).map(|x| x as u8),
        kind,
        inside_temp_c: row.num(Col::InsideTemp, bad).map(|x| round(c(x), 1)),
        outside_temp_c: row.num(Col::OutsideTemp, bad).map(|x| round(c(x), 1)),
        version,
    }
}

/// `YYYY-MM-DD HH:MM[:SS]` (also with a `T`), or `M/D/YYYY h:mm[:ss] [AM|PM]`: wall time, no zone.
pub(crate) fn parse_local(s: &str) -> Option<NaiveDateTime> {
    let s = s.trim();
    let num = |x: &str| x.trim().parse::<u32>().ok();
    let (date, time) = match s.split_once([' ', 'T']) {
        Some((d, t)) => (d, t.trim()),
        None => (s, ""),
    };
    let d = if date.contains('-') {
        let mut p = date.split('-');
        let (y, m, dd) = (num(p.next()?)?, num(p.next()?)?, num(p.next()?)?);
        NaiveDate::from_ymd_opt(i32::try_from(y).ok()?, m, dd)?
    } else if date.contains('/') {
        let mut p = date.split('/');
        let (m, dd, y) = (num(p.next()?)?, num(p.next()?)?, num(p.next()?)?);
        NaiveDate::from_ymd_opt(i32::try_from(y).ok()?, m, dd)?
    } else {
        return None;
    };
    let (clock, ampm) = match time.rsplit_once(' ') {
        Some((c, ap)) if ap.eq_ignore_ascii_case("am") || ap.eq_ignore_ascii_case("pm") => {
            (c.trim(), Some(ap.eq_ignore_ascii_case("pm")))
        }
        _ => (time, None),
    };
    let clock = clock.split(['.', '+', 'Z']).next().unwrap_or_default();
    let mut p = clock.split(':');
    let mut h = num(p.next().filter(|x| !x.is_empty())?)?;
    let mi = num(p.next()?)?;
    let sec = p.next().map_or(Some(0), num)?;
    if let Some(pm) = ampm {
        if h == 0 || h > 12 {
            return None;
        }
        h = match (pm, h) {
            (false, 12) => 0,
            (true, 12) => 12,
            (true, h) => h + 12,
            (false, h) => h,
        };
    }
    d.and_hms_opt(h, mi, sec)
}

/// Wall time to UTC in `tz`. In the hour the clocks go back the earlier reading is taken unless it would be before
/// the row before (`prev_ms`), then the later: the second pass through that hour.
pub(crate) enum Utc {
    One(u64),
    /// In the repeated hour (counted).
    Repeated(u64),
    /// A wall time the clocks skip (spring forward).
    Skipped,
}

pub(crate) fn to_utc(tz: chrono_tz::Tz, local: NaiveDateTime, prev_ms: Option<u64>) -> Utc {
    let ms = |d: chrono::DateTime<chrono_tz::Tz>| u64::try_from(d.timestamp_millis()).unwrap_or(0);
    match tz.from_local_datetime(&local) {
        LocalResult::Single(d) => Utc::One(ms(d)),
        LocalResult::Ambiguous(a, b) => {
            let (a, b) = (ms(a), ms(b));
            if prev_ms.is_some_and(|p| a < p) { Utc::Repeated(b) } else { Utc::Repeated(a) }
        }
        LocalResult::None => Utc::Skipped,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn reads_teslafi_dates() {
        let want = NaiveDate::from_ymd_opt(2023, 3, 4).unwrap().and_hms_opt(22, 15, 2).unwrap();
        assert_eq!(parse_local("2023-03-04 22:15:02"), Some(want));
        assert_eq!(parse_local("2023-03-04T22:15:02"), Some(want));
        assert_eq!(parse_local("3/4/2023 10:15:02 PM"), Some(want));
        assert_eq!(parse_local("3/4/2023 12:00 AM"), NaiveDate::from_ymd_opt(2023, 3, 4).unwrap().and_hms_opt(0, 0, 0));
        assert_eq!(parse_local("2023-02-30 10:00:00"), None);
        assert_eq!(parse_local("energy_remaining"), None);
        assert_eq!(parse_local(""), None);
    }

    #[test]
    fn local_time_goes_to_utc_through_dst() {
        let tz: chrono_tz::Tz = "America/New_York".parse().unwrap();
        // 2023-11-05 01:30 happens twice in New York
        let l = parse_local("2023-11-05 01:30:00").unwrap();
        let Utc::Repeated(first) = to_utc(tz, l, None) else { panic!() };
        let Utc::Repeated(second) = to_utc(tz, l, Some(first + 1)) else { panic!() };
        assert_eq!(second - first, 3_600_000);
        // 2023-03-12 02:30 never happens there
        assert!(matches!(to_utc(tz, parse_local("2023-03-12 02:30:00").unwrap(), None), Utc::Skipped));
    }
}
