//! What goes on the wire: a car's values as one retained topic each, and its drive and charge events as JSON. Pure
//! functions of their inputs, so Settings → MQTT can preview exactly what a broker would get.
//!
//! Values are text (`80.5`, `true`, `driving`), in the owner's units; a value the car didn't send is not published
//! (the broker keeps the last one), never sent as 0. Positions go out only when location publishing is on; while it is
//! off, the position topics are published empty, which clears anything a broker kept from before.

use crate::error::Error;
use crate::settings::{Settings, Units};
use crate::topics::{Topics, check_prefix};
use model::{CarId, Charge, ChargeKind, Drive, Gear, Now, Position, State};
use serde_json::{Map, Value, json};

/// One MQTT message. Everything is sent at QoS 1 (at least once).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub topic: String,
    pub payload: String,
    pub retain: bool,
}

/// What Home Assistant shows for a car's device.
#[derive(Debug, Clone, PartialEq, Eq, Default, Hash)]
pub struct CarInfo {
    pub name: String,
    /// e.g. `Model Y 2022`.
    pub model: Option<String>,
}

impl CarInfo {
    pub fn of(car: &model::Car) -> CarInfo {
        let model = car.vin.as_ref().and_then(|v| {
            let m = v.model?;
            Some(match v.year {
                Some(y) => format!("Model {m} {y}"),
                None => format!("Model {m}"),
            })
        });
        CarInfo { name: car.name.clone(), model }
    }
}

/// Where and how things are published: made once from the settings.
#[derive(Debug, Clone)]
pub struct Layout {
    pub topics: Topics,
    pub units: Units,
    pub location: bool,
    pub discovery: bool,
}

impl Layout {
    /// Needs only the prefixes to be valid (not a broker), so Settings → MQTT can preview before one is set.
    pub fn new(settings: &Settings) -> Result<Layout, Error> {
        check_prefix(&settings.prefix)?;
        check_prefix(&settings.discovery_prefix)?;
        Ok(Layout {
            topics: Topics::new(&settings.prefix, &settings.discovery_prefix),
            units: settings.units,
            location: settings.publish_location,
            discovery: settings.discovery,
        })
    }
}

/// A car's published values, one retained topic each (`<prefix>/cars/<id>/<key>`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Field {
    /// `driving`, `charging`, `online`, `asleep` or `unknown`.
    State,
    Driving,
    Charging,
    /// The saved place's name, empty when the car is at none.
    Place,
    Soc,
    Range,
    Speed,
    /// Battery power, kW, positive out of the pack.
    Power,
    ChargePower,
    /// `ac` or `dc`.
    ChargeKind,
    EnergyRemaining,
    BatteryTemp,
    OutsideTemp,
    InsideTemp,
    TireFrontLeft,
    TireFrontRight,
    TireRearLeft,
    TireRearRight,
    Odometer,
    /// `P`, `R`, `N` or `D`.
    Gear,
    /// The 12 V system's voltage.
    LvVoltage,
    /// When these values were recorded on the car (ISO 8601, UTC); about a minute behind live.
    Updated,
    Latitude,
    Longitude,
    /// `{"latitude":…,"longitude":…,"gps_accuracy":…}`, for Home Assistant's device tracker.
    Location,
}

impl Field {
    pub const COUNT: usize = 25;
    pub const ALL: [Field; Field::COUNT] = [
        Field::State,
        Field::Driving,
        Field::Charging,
        Field::Place,
        Field::Soc,
        Field::Range,
        Field::Speed,
        Field::Power,
        Field::ChargePower,
        Field::ChargeKind,
        Field::EnergyRemaining,
        Field::BatteryTemp,
        Field::OutsideTemp,
        Field::InsideTemp,
        Field::TireFrontLeft,
        Field::TireFrontRight,
        Field::TireRearLeft,
        Field::TireRearRight,
        Field::Odometer,
        Field::Gear,
        Field::LvVoltage,
        Field::Updated,
        Field::Latitude,
        Field::Longitude,
        Field::Location,
    ];

    /// The topic's last level.
    pub fn key(self) -> &'static str {
        match self {
            Field::State => "state",
            Field::Driving => "driving",
            Field::Charging => "charging",
            Field::Place => "place",
            Field::Soc => "soc",
            Field::Range => "range",
            Field::Speed => "speed",
            Field::Power => "power",
            Field::ChargePower => "charge_power",
            Field::ChargeKind => "charge_kind",
            Field::EnergyRemaining => "energy_remaining",
            Field::BatteryTemp => "battery_temp",
            Field::OutsideTemp => "outside_temp",
            Field::InsideTemp => "inside_temp",
            Field::TireFrontLeft => "tire_pressure_fl",
            Field::TireFrontRight => "tire_pressure_fr",
            Field::TireRearLeft => "tire_pressure_rl",
            Field::TireRearRight => "tire_pressure_rr",
            Field::Odometer => "odometer",
            Field::Gear => "gear",
            Field::LvVoltage => "lv_voltage",
            Field::Updated => "updated",
            Field::Latitude => "latitude",
            Field::Longitude => "longitude",
            Field::Location => "location",
        }
    }

    /// Published only with location publishing on (and cleared while it is off).
    pub fn is_location(self) -> bool {
        matches!(self, Field::Latitude | Field::Longitude | Field::Location)
    }

    pub(crate) fn index(self) -> usize {
        self as usize
    }
}

/// `v` rounded to `decimals`, or `None` if it isn't a finite number. Negative zero becomes 0.
pub(crate) fn round(v: f64, decimals: i32) -> Option<f64> {
    if !v.is_finite() {
        return None;
    }
    let m = 10f64.powi(decimals);
    let r = (v * m).round() / m;
    // so large that scaling overflows: it has no decimals to round anyway
    let r = if r.is_finite() { r } else { v };
    Some(if r == 0.0 { 0.0 } else { r })
}

/// Longest car name, model or place name published, in bytes (longer ones are cut).
pub const MAX_NAME: usize = 256;
/// Longest address published, in bytes (longer ones are cut).
pub const MAX_ADDRESS: usize = 1024;

/// `s` cut to at most `max` bytes, at a character boundary. Names and addresses are typed by people or looked up, so
/// without a bound one could make a message larger than the link sends (or the broker takes); that message would fail
/// the connection, be first in line again after reconnecting, and stop publishing for every car.
pub(crate) fn clip(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

fn text(v: Option<f64>, decimals: i32) -> Option<String> {
    v.and_then(|v| round(v, decimals)).map(|r| r.to_string())
}

fn bool_text(b: bool) -> String {
    if b { "true" } else { "false" }.to_string()
}

/// The state Home Assistant shows: what the car is doing, else whether it is awake.
pub fn car_state(now: &Now) -> &'static str {
    if now.driving {
        "driving"
    } else if now.sample.charging == Some(true) {
        "charging"
    } else {
        match now.state {
            Some(State::Online) => "online",
            Some(State::Asleep) => "asleep",
            Some(State::Unknown) | None => "unknown",
        }
    }
}

pub const STATES: [&str; 5] = ["driving", "charging", "online", "asleep", "unknown"];

fn valid_position(lat: Option<f64>, lon: Option<f64>) -> Option<(f64, f64)> {
    match (lat, lon) {
        (Some(la), Some(lo)) if (-90.0..=90.0).contains(&la) && (-180.0..=180.0).contains(&lo) => Some((la, lo)),
        _ => None,
    }
}

/// The text `field` is published as. `None`: not known, so nothing is published. With location publishing off, the
/// position fields are `Some("")`: an empty retained message, which clears the broker's copy.
pub fn field_payload(layout: &Layout, field: Field, now: &Now, place: Option<&str>) -> Option<String> {
    let (s, u) = (&now.sample, &layout.units);
    if field.is_location() && !layout.location {
        return Some(String::new());
    }
    let tire = |i: usize| text(s.tpms_bar[i].map(|v| u.pressure(v)), u.pressure_decimals());
    match field {
        Field::State => Some(car_state(now).to_string()),
        Field::Driving => Some(bool_text(now.driving)),
        Field::Charging => s.charging.map(bool_text),
        Field::Place => Some(clip(place.unwrap_or_default(), MAX_NAME).to_string()),
        Field::Soc => text(s.soc, 1),
        Field::Range => text(s.range_km.map(|v| u.distance(v)), 1),
        Field::Speed => text(s.speed_kmh.map(|v| u.speed(v)), 1),
        Field::Power => text(s.battery_power_kw, 2),
        Field::ChargePower => text(s.charge_power_kw, 2),
        Field::ChargeKind => s.charge_kind.map(|k| kind_text(k).to_string()),
        Field::EnergyRemaining => text(s.energy_remaining_kwh, 2),
        Field::BatteryTemp => text(s.battery_temp_c.map(|v| u.temperature(v)), 1),
        Field::OutsideTemp => text(s.outside_temp_c.map(|v| u.temperature(v)), 1),
        Field::InsideTemp => text(s.inside_temp_c.map(|v| u.temperature(v)), 1),
        Field::TireFrontLeft => tire(0),
        Field::TireFrontRight => tire(1),
        Field::TireRearLeft => tire(2),
        Field::TireRearRight => tire(3),
        Field::Odometer => text(s.odometer_km.map(|v| u.distance(v)), 1),
        Field::Gear => s.gear.map(|g| gear_text(g).to_string()),
        Field::LvVoltage => text(s.lv_voltage_v, 2),
        Field::Updated => (now.at_ms > 0).then(|| iso_utc(now.at_ms)),
        Field::Latitude => valid_position(s.lat, s.lon).and_then(|(la, _)| text(Some(la), 6)),
        Field::Longitude => valid_position(s.lat, s.lon).and_then(|(_, lo)| text(Some(lo), 6)),
        Field::Location => valid_position(s.lat, s.lon).map(|(la, lo)| {
            let mut o = Map::new();
            o.insert("latitude".into(), num(Some(la), 6));
            o.insert("longitude".into(), num(Some(lo), 6));
            if let Some(acc) = s.gps_accuracy_m.and_then(|a| round(a, 0)) {
                o.insert("gps_accuracy".into(), acc.into());
            }
            Value::Object(o).to_string()
        }),
    }
}

fn kind_text(k: ChargeKind) -> &'static str {
    match k {
        ChargeKind::Ac => "ac",
        ChargeKind::Dc => "dc",
    }
}

fn gear_text(g: Gear) -> &'static str {
    match g {
        Gear::P => "P",
        Gear::R => "R",
        Gear::N => "N",
        Gear::D => "D",
    }
}

/// Every known value of a car, as retained messages in [`Field::ALL`] order.
pub fn now_messages(layout: &Layout, now: &Now, place: Option<&str>) -> Vec<Message> {
    Field::ALL
        .iter()
        .filter_map(|&f| {
            field_payload(layout, f, now, place).map(|payload| Message {
                topic: layout.topics.field(now.car, f.key()),
                payload,
                retain: true,
            })
        })
        .collect()
}

/// Empty retained messages for every value topic of a car: a broker forgets them.
pub fn clear_messages(layout: &Layout, car: CarId) -> Vec<Message> {
    Field::ALL
        .iter()
        .map(|f| Message { topic: layout.topics.field(car, f.key()), payload: String::new(), retain: true })
        .collect()
}

/// A drive or a charge starting or ending. Place names are the saved places' names (the caller looks them up);
/// positions and addresses go out only with location publishing on.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    DriveStarted { drive: Box<Drive>, start_place: Option<String> },
    DriveEnded { drive: Box<Drive>, start_place: Option<String>, end_place: Option<String> },
    ChargeStarted { charge: Box<Charge>, place: Option<String> },
    ChargeEnded { charge: Box<Charge>, place: Option<String> },
}

impl Event {
    /// Home Assistant's event types, as its event entity lists them.
    pub const TYPES: [&'static str; 4] = ["drive_started", "drive_ended", "charge_started", "charge_ended"];

    pub fn car(&self) -> CarId {
        match self {
            Event::DriveStarted { drive, .. } | Event::DriveEnded { drive, .. } => drive.car,
            Event::ChargeStarted { charge, .. } | Event::ChargeEnded { charge, .. } => charge.car,
        }
    }

    pub fn event_type(&self) -> &'static str {
        match self {
            Event::DriveStarted { .. } => "drive_started",
            Event::DriveEnded { .. } => "drive_ended",
            Event::ChargeStarted { .. } => "charge_started",
            Event::ChargeEnded { .. } => "charge_ended",
        }
    }
}

/// A number rounded to `decimals`, or `null` when not known.
fn num(v: Option<f64>, decimals: i32) -> Value {
    v.and_then(|v| round(v, decimals)).map_or(Value::Null, Value::from)
}

fn put(o: &mut Map<String, Value>, key: &str, v: Value) {
    o.insert(key.to_string(), v);
}

fn put_position(
    o: &mut Map<String, Value>,
    layout: &Layout,
    prefix: &str,
    p: Option<&Position>,
    address: Option<&str>,
) {
    if !layout.location {
        return;
    }
    let pos = p.and_then(|p| valid_position(Some(p.lat), Some(p.lon)));
    put(o, &format!("{prefix}latitude"), num(pos.map(|(la, _)| la), 6));
    put(o, &format!("{prefix}longitude"), num(pos.map(|(_, lo)| lo), 6));
    put(o, &format!("{prefix}address"), address.map_or(Value::Null, |a| clip(a, MAX_ADDRESS).into()));
}

/// The event as JSON on `<prefix>/cars/<id>/event`, not retained: `event_type` (as Home Assistant's event entity
/// wants it), the ids, times in ISO 8601 UTC, and the summary in the owner's units (named in `units`). Unknown values
/// are `null`.
pub fn event_message(layout: &Layout, event: &Event) -> Message {
    let u = &layout.units;
    let dist = |v: Option<f64>| num(v.map(|v| u.distance(v)), 1);
    let temp = |v: Option<f64>| num(v.map(|v| u.temperature(v)), 1);
    let place = |p: &Option<String>| p.as_deref().map_or(Value::Null, |s| clip(s, MAX_NAME).into());
    let mut o = Map::new();
    put(&mut o, "event_type", event.event_type().into());
    put(&mut o, "car", event.car().into());
    match event {
        Event::DriveStarted { drive, start_place } | Event::DriveEnded { drive, start_place, .. } => {
            let d = drive;
            put(&mut o, "drive", d.id.into());
            put(&mut o, "start", iso_utc(d.start_ms).into());
            put(&mut o, "start_place", place(start_place));
            put(&mut o, "start_soc", num(d.start_soc, 1));
            put(&mut o, "start_range", dist(d.start_range_km));
            put(&mut o, "start_odometer", dist(d.start_odometer_km));
            put_position(&mut o, layout, "start_", d.start.as_ref(), d.start_address.as_deref());
            if let Event::DriveEnded { end_place, .. } = event {
                put(&mut o, "end", iso_utc(d.end_ms).into());
                put(&mut o, "end_place", place(end_place));
                put(&mut o, "duration_min", num(Some(d.duration_min()), 1));
                put(&mut o, "distance", dist(d.distance_km));
                put(&mut o, "energy_used_kwh", num(d.energy_used_kwh, 2));
                put(&mut o, "efficiency", num(d.efficiency_wh_km().map(|e| u.efficiency(e)), 0));
                put(&mut o, "speed_max", num(d.speed_max_kmh.map(|v| u.speed(v)), 1));
                put(&mut o, "power_max_kw", num(d.power_max_kw, 1));
                put(&mut o, "power_min_kw", num(d.power_min_kw, 1));
                put(&mut o, "end_soc", num(d.end_soc, 1));
                put(&mut o, "end_range", dist(d.end_range_km));
                put(&mut o, "end_odometer", dist(d.end_odometer_km));
                put(&mut o, "outside_temp_avg", temp(d.outside_temp_avg_c));
                put(&mut o, "inside_temp_avg", temp(d.inside_temp_avg_c));
                put(&mut o, "ascent", num(d.ascent_m.map(|v| u.elevation(v)), 0));
                put(&mut o, "descent", num(d.descent_m.map(|v| u.elevation(v)), 0));
                put_position(&mut o, layout, "end_", d.end.as_ref(), d.end_address.as_deref());
            }
        }
        Event::ChargeStarted { charge, place: at } | Event::ChargeEnded { charge, place: at } => {
            let c = charge;
            put(&mut o, "charge", c.id.into());
            put(&mut o, "start", iso_utc(c.start_ms).into());
            put(&mut o, "place", place(at));
            put(&mut o, "kind", c.kind.map_or(Value::Null, |k| kind_text(k).into()));
            put(&mut o, "start_soc", num(c.start_soc, 1));
            put(&mut o, "start_range", dist(c.start_range_km));
            put_position(&mut o, layout, "", c.position.as_ref(), c.address.as_deref());
            if let Event::ChargeEnded { .. } = event {
                put(&mut o, "end", iso_utc(c.end_ms).into());
                put(&mut o, "duration_min", num(Some(c.duration_min()), 1));
                put(&mut o, "energy_added_kwh", num(c.energy_added_kwh, 2));
                put(&mut o, "energy_used_kwh", num(c.energy_used_kwh, 2));
                put(&mut o, "power_max_kw", num(c.power_max_kw, 1));
                put(&mut o, "end_soc", num(c.end_soc, 1));
                put(&mut o, "end_range", dist(c.end_range_km));
                put(&mut o, "battery_temp_avg", temp(c.battery_temp_avg_c));
                put(&mut o, "outside_temp_avg", temp(c.outside_temp_avg_c));
                // in the place's currency
                put(&mut o, "cost", num(c.cost, 2));
            }
        }
    }
    put(
        &mut o,
        "units",
        json!({
            "distance": u.distance_unit(),
            "speed": u.speed_unit(),
            "temperature": u.temperature_unit(),
            "efficiency": u.efficiency_unit(),
            "elevation": u.elevation_unit(),
        }),
    );
    Message { topic: layout.topics.event(event.car()), payload: Value::Object(o).to_string(), retain: false }
}

/// `2026-11-16T09:30:00+00:00`: ISO 8601 in UTC, to the second, as Home Assistant's timestamp sensors take it.
pub fn iso_utc(ms: u64) -> String {
    let secs = ms / 1000;
    let (y, m, d) = civil_from_days((secs / 86_400) as i64);
    let rem = secs % 86_400;
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}+00:00", rem / 3600, rem % 3600 / 60, rem % 60)
}

/// Days since 1970-01-01 → (year, month, day) in the proleptic Gregorian calendar (the well-known era arithmetic:
/// 400-year eras of 146,097 days, years starting in March so leap days fall last).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::settings::{DistanceUnit, PressureUnit, TemperatureUnit};
    use model::Sample;

    pub(crate) fn layout(location: bool, units: Units) -> Layout {
        Layout { topics: Topics::new("sentry-mate", "homeassistant"), units, location, discovery: true }
    }

    /// A synthetic parked car (made-up position, nowhere in particular).
    pub(crate) fn parked(car: CarId) -> Now {
        Now {
            car,
            at_ms: 1_794_150_000_000,
            state: Some(State::Online),
            driving: false,
            place: Some(1),
            sample: Sample {
                t_ms: 1_794_150_000_000,
                lat: Some(10.123_456_7),
                lon: Some(-20.765_432_1),
                gps_accuracy_m: Some(4.4),
                speed_kmh: Some(0.0),
                gear: Some(Gear::P),
                odometer_km: Some(12_345.67),
                soc: Some(80.46),
                range_km: Some(402.3),
                energy_remaining_kwh: Some(58.123),
                battery_power_kw: Some(-0.3),
                battery_temp_c: Some(21.04),
                outside_temp_c: Some(-3.4),
                inside_temp_c: Some(19.0),
                charging: Some(false),
                lv_voltage_v: Some(13.456),
                tpms_bar: [Some(2.9), Some(2.875), None, Some(3.0)],
                ..Sample::default()
            },
        }
    }

    fn payload(msgs: &[Message], key: &str) -> Option<String> {
        msgs.iter().find(|m| m.topic.ends_with(&format!("/{key}"))).map(|m| m.payload.clone())
    }

    #[test]
    fn values_and_topics() {
        let l = layout(false, Units::default());
        let msgs = now_messages(&l, &parked(7), Some("Home"));
        assert!(msgs.iter().all(|m| m.retain && m.topic.starts_with("sentry-mate/cars/c7/")));
        let p = |k| payload(&msgs, k);
        assert_eq!(p("state").as_deref(), Some("online"));
        assert_eq!(p("driving").as_deref(), Some("false"));
        assert_eq!(p("charging").as_deref(), Some("false"));
        assert_eq!(p("place").as_deref(), Some("Home"));
        assert_eq!(p("soc").as_deref(), Some("80.5"));
        assert_eq!(p("range").as_deref(), Some("402.3"));
        assert_eq!(p("speed").as_deref(), Some("0"));
        assert_eq!(p("power").as_deref(), Some("-0.3"));
        assert_eq!(p("energy_remaining").as_deref(), Some("58.12"));
        assert_eq!(p("battery_temp").as_deref(), Some("21"));
        assert_eq!(p("outside_temp").as_deref(), Some("-3.4"));
        assert_eq!(p("tire_pressure_fl").as_deref(), Some("2.9"));
        assert_eq!(p("tire_pressure_fr").as_deref(), Some("2.88"));
        assert_eq!(p("tire_pressure_rl"), None, "not sent by the car: not published");
        assert_eq!(p("odometer").as_deref(), Some("12345.7"));
        assert_eq!(p("gear").as_deref(), Some("P"));
        assert_eq!(p("lv_voltage").as_deref(), Some("13.46"));
        assert_eq!(p("updated").as_deref(), Some("2026-11-08T15:00:00+00:00"));
        assert_eq!(p("charge_power"), None);
        assert_eq!(p("charge_kind"), None);
        assert_eq!(p("place"), Some("Home".into()));
        assert_eq!(
            now_messages(&l, &parked(7), None).iter().find(|m| m.topic.ends_with("/place")).unwrap().payload,
            ""
        );
    }

    #[test]
    fn location_only_when_on() {
        let off = now_messages(&layout(false, Units::default()), &parked(7), Some("Home"));
        for key in ["latitude", "longitude", "location"] {
            assert_eq!(payload(&off, key).as_deref(), Some(""), "{key} is cleared while location is off");
        }
        assert!(off.iter().all(|m| !m.payload.contains("10.12") && !m.payload.contains("20.76")), "no position leaks");

        let on = now_messages(&layout(true, Units::default()), &parked(7), Some("Home"));
        assert_eq!(payload(&on, "latitude").as_deref(), Some("10.123457"));
        assert_eq!(payload(&on, "longitude").as_deref(), Some("-20.765432"));
        let loc: Value = serde_json::from_str(&payload(&on, "location").unwrap()).unwrap();
        assert_eq!(loc, json!({"latitude": 10.123457, "longitude": -20.765432, "gps_accuracy": 4.0}));

        let mut nowhere = parked(7);
        nowhere.sample.lat = Some(95.0);
        assert_eq!(payload(&now_messages(&layout(true, Units::default()), &nowhere, None), "location"), None);
        nowhere.sample.lat = None;
        assert_eq!(payload(&now_messages(&layout(true, Units::default()), &nowhere, None), "latitude"), None);
    }

    #[test]
    fn units_as_chosen() {
        let us = Units { distance: DistanceUnit::Mi, temperature: TemperatureUnit::F, pressure: PressureUnit::Psi };
        let msgs = now_messages(&layout(false, us), &parked(7), None);
        let p = |k| payload(&msgs, k).unwrap();
        assert_eq!(p("range"), "250"); // 402.3 km
        assert_eq!(p("odometer"), "7671.2"); // 12,345.67 km
        assert_eq!(p("battery_temp"), "69.9"); // 21.04 °C
        assert_eq!(p("outside_temp"), "25.9"); // -3.4 °C
        assert_eq!(p("tire_pressure_fl"), "42.1"); // 2.9 bar
        assert_eq!(p("soc"), "80.5", "percentages don't change");
        let mut moving = parked(7);
        moving.sample.speed_kmh = Some(100.0);
        assert_eq!(payload(&now_messages(&layout(false, us), &moving, None), "speed").unwrap(), "62.1");
    }

    #[test]
    fn states() {
        let mut n = parked(1);
        assert_eq!(car_state(&n), "online");
        n.sample.charging = Some(true);
        assert_eq!(car_state(&n), "charging");
        n.driving = true;
        assert_eq!(car_state(&n), "driving");
        n = Now { state: Some(State::Asleep), ..Now::default() };
        assert_eq!(car_state(&n), "asleep");
        n.state = None;
        assert_eq!(car_state(&n), "unknown");
        assert!(STATES.contains(&car_state(&n)));
        let msgs = now_messages(&layout(false, Units::default()), &Now::default(), None);
        let keys: Vec<_> = msgs.iter().map(|m| m.topic.rsplit('/').next().unwrap()).collect();
        assert_eq!(
            keys,
            ["state", "driving", "place", "latitude", "longitude", "location"],
            "nothing known, nothing made up"
        );
    }

    #[test]
    fn numbers() {
        assert_eq!(round(0.1 + 0.2, 1), Some(0.3));
        assert_eq!(round(-0.04, 1), Some(0.0));
        assert_eq!(round(f64::NAN, 1), None);
        assert_eq!(round(f64::INFINITY, 1), None);
        assert_eq!(round(1e307, 2), Some(1e307));
        assert_eq!(text(Some(-0.0), 1).as_deref(), Some("0"));
        assert_eq!(text(Some(1234567.891), 2).as_deref(), Some("1234567.89"));
    }

    #[test]
    fn times() {
        assert_eq!(iso_utc(0), "1970-01-01T00:00:00+00:00");
        assert_eq!(iso_utc(1_700_000_000_999), "2023-11-14T22:13:20+00:00");
        assert_eq!(iso_utc(951_782_400_000), "2000-02-29T00:00:00+00:00");
        assert_eq!(iso_utc(4_107_542_400_000), "2100-03-01T00:00:00+00:00");
        assert_eq!(iso_utc(1_798_761_599_000), "2026-12-31T23:59:59+00:00");
    }

    pub(crate) fn drive(car: CarId) -> Drive {
        let pos = |lat: f64, lon: f64| Position { lat, lon, ..Position::default() };
        Drive {
            id: 42,
            car,
            start_ms: 1_794_150_000_000,
            end_ms: 1_794_151_800_000,
            start: Some(pos(10.5, 20.5)),
            end: Some(pos(10.6, 20.6)),
            start_place: Some(1),
            end_place: None,
            start_address: Some("1 Synthetic Road".into()),
            end_address: None,
            start_odometer_km: Some(1000.0),
            end_odometer_km: Some(1032.19),
            distance_km: Some(32.19),
            speed_max_kmh: Some(112.65),
            power_max_kw: Some(150.04),
            power_min_kw: Some(-60.0),
            energy_used_kwh: Some(5.0),
            start_soc: Some(80.0),
            end_soc: Some(73.5),
            start_range_km: Some(400.0),
            end_range_km: Some(367.0),
            outside_temp_avg_c: Some(10.0),
            inside_temp_avg_c: None,
            ascent_m: Some(120.0),
            descent_m: Some(80.0),
            source: model::Source::Can,
            rules_version: 1,
        }
    }

    #[test]
    fn drive_events() {
        let km = layout(false, Units::default());
        let start =
            event_message(&km, &Event::DriveStarted { drive: Box::new(drive(3)), start_place: Some("Home".into()) });
        assert_eq!((start.topic.as_str(), start.retain), ("sentry-mate/cars/c3/event", false));
        let v: Value = serde_json::from_str(&start.payload).unwrap();
        assert_eq!(v["event_type"], "drive_started");
        assert_eq!((v["car"].as_u64(), v["drive"].as_u64()), (Some(3), Some(42)));
        assert_eq!(v["start"], "2026-11-08T15:00:00+00:00");
        assert_eq!(v["start_place"], "Home");
        assert_eq!(v["start_soc"], 80.0);
        assert!(v.get("end").is_none() && v.get("distance").is_none(), "a start has no summary yet");
        assert!(v.get("start_latitude").is_none() && v.get("start_address").is_none(), "location is off");

        let end = event_message(
            &km,
            &Event::DriveEnded { drive: Box::new(drive(3)), start_place: Some("Home".into()), end_place: None },
        );
        let v: Value = serde_json::from_str(&end.payload).unwrap();
        assert_eq!(v["event_type"], "drive_ended");
        assert_eq!(v["end"], "2026-11-08T15:30:00+00:00");
        assert_eq!(v["duration_min"], 30.0);
        assert_eq!(v["distance"], 32.2);
        assert_eq!(v["energy_used_kwh"], 5.0);
        assert_eq!(v["efficiency"], 155.0); // 5 kWh / 32.19 km
        assert_eq!(v["end_place"], Value::Null);
        assert_eq!(v["inside_temp_avg"], Value::Null, "unknown is null, never 0");
        assert_eq!(v["units"]["distance"], "km");
        assert!(!end.payload.contains("Synthetic") && !end.payload.contains("10.5"), "no position or address");

        let us = Units { distance: DistanceUnit::Mi, temperature: TemperatureUnit::F, pressure: PressureUnit::Psi };
        let l = layout(true, us);
        let v: Value = serde_json::from_str(
            &event_message(&l, &Event::DriveEnded { drive: Box::new(drive(3)), start_place: None, end_place: None })
                .payload,
        )
        .unwrap();
        assert_eq!(v["distance"], 20.0); // 32.19 km
        assert_eq!(v["speed_max"], 70.0); // 112.65 km/h
        assert_eq!(v["efficiency"], 250.0); // 155.3 Wh/km
        assert_eq!(v["outside_temp_avg"], 50.0);
        assert_eq!(v["ascent"], 394.0); // 120 m
        assert_eq!(
            v["units"],
            json!({"distance":"mi","speed":"mph","temperature":"°F","efficiency":"Wh/mi","elevation":"ft"})
        );
        assert_eq!((v["start_latitude"].as_f64(), v["end_longitude"].as_f64()), (Some(10.5), Some(20.6)));
        assert_eq!(v["start_address"], "1 Synthetic Road");
        assert_eq!(v["end_address"], Value::Null);
    }

    #[test]
    fn charge_events() {
        let charge = Charge {
            id: 9,
            car: 5,
            start_ms: 1_794_150_000_000,
            end_ms: 1_794_157_200_000,
            position: Some(Position { lat: 1.25, lon: 2.5, ..Position::default() }),
            place: Some(2),
            address: None,
            kind: Some(ChargeKind::Dc),
            energy_added_kwh: Some(40.123),
            energy_used_kwh: None,
            start_soc: Some(10.0),
            end_soc: Some(80.0),
            start_range_km: Some(50.0),
            end_range_km: Some(400.0),
            power_max_kw: Some(250.0),
            outside_temp_avg_c: None,
            battery_temp_avg_c: Some(40.0),
            cost: Some(12.3456),
            source: model::Source::Can,
            rules_version: 1,
        };
        let l = layout(false, Units::default());
        let s: Value = serde_json::from_str(
            &event_message(&l, &Event::ChargeStarted { charge: Box::new(charge.clone()), place: Some("Work".into()) })
                .payload,
        )
        .unwrap();
        assert_eq!(s["event_type"], "charge_started");
        assert_eq!((s["charge"].as_u64(), s["car"].as_u64()), (Some(9), Some(5)));
        assert_eq!((s["place"].as_str(), s["kind"].as_str()), (Some("Work"), Some("dc")));
        assert!(s.get("energy_added_kwh").is_none() && s.get("latitude").is_none());
        let msg = event_message(&l, &Event::ChargeEnded { charge: Box::new(charge), place: Some("Work".into()) });
        assert_eq!(msg.topic, "sentry-mate/cars/c5/event");
        let e: Value = serde_json::from_str(&msg.payload).unwrap();
        assert_eq!(e["event_type"], "charge_ended");
        assert_eq!(e["duration_min"], 120.0);
        assert_eq!(e["energy_added_kwh"], 40.12);
        assert_eq!(e["energy_used_kwh"], Value::Null);
        assert_eq!((e["end_soc"].as_f64(), e["cost"].as_f64()), (Some(80.0), Some(12.35)));
        assert!(Event::TYPES.contains(&"charge_ended"));
    }

    #[test]
    fn clipping_cuts_at_a_character() {
        assert_eq!(clip("Home", 10), "Home");
        assert_eq!(clip("Home", 2), "Ho");
        assert_eq!(clip("Café", 4), "Caf", "é is two bytes: cut before it, not through it");
        assert_eq!(clip("Café", 5), "Café");
        assert_eq!(clip("ééé", 1), "");
    }

    #[test]
    fn every_message_is_small_however_long_the_names() {
        // names and addresses people type or look up: control characters grow 6x as JSON (\u0001), and 4-byte
        // characters must not be cut through
        let long = format!("{}{}", "\u{1}".repeat(400_000), "🚗".repeat(100_000));
        let info = CarInfo { name: long.clone(), model: Some(long.clone()) };
        let mut d = drive(3);
        d.start_address = Some(long.clone());
        d.end_address = Some(long.clone());
        let c = Charge {
            id: 1,
            car: 3,
            start_ms: 0,
            end_ms: 0,
            position: d.start.clone(),
            place: None,
            address: Some(long.clone()),
            kind: None,
            energy_added_kwh: None,
            energy_used_kwh: None,
            start_soc: None,
            end_soc: None,
            start_range_km: None,
            end_range_km: None,
            power_max_kw: None,
            outside_temp_avg_c: None,
            battery_temp_avg_c: None,
            cost: None,
            source: model::Source::Can,
            rules_version: 1,
        };
        let p = || Some(long.clone());
        for l in [layout(true, Units::default()), layout(false, Units::default())] {
            let mut msgs = now_messages(&l, &parked(3), Some(&long));
            msgs.extend(crate::discovery::configs(&l, 3, &info));
            for e in [
                Event::DriveStarted { drive: Box::new(d.clone()), start_place: p() },
                Event::DriveEnded { drive: Box::new(d.clone()), start_place: p(), end_place: p() },
                Event::ChargeStarted { charge: Box::new(c.clone()), place: p() },
                Event::ChargeEnded { charge: Box::new(c.clone()), place: p() },
            ] {
                msgs.push(event_message(&l, &e));
            }
            for m in &msgs {
                let size = m.topic.len() + m.payload.len();
                assert!(size <= 32 * 1024, "{} is {size} bytes", m.topic);
                if m.payload.starts_with('{') {
                    let _: Value = serde_json::from_str(&m.payload).unwrap();
                }
            }
            let place = msgs.iter().find(|m| m.topic.ends_with("/place")).unwrap();
            assert_eq!(place.payload.len(), MAX_NAME);
        }
    }

    #[test]
    fn car_info() {
        let mut car = model::Car {
            id: 1,
            name: "Family car".into(),
            vehicle_keys: vec![],
            preferred_key: None,
            vin: None,
            profile_setting: None,
            time_zone: "UTC".into(),
            efficiency_wh_km: None,
            created_ms: 0,
        };
        assert_eq!(CarInfo::of(&car), CarInfo { name: "Family car".into(), model: None });
        car.vin = Some(model::VinInfo {
            vin_prefix: "X".into(),
            model: Some('Y'),
            year: Some(2022),
            plant: None,
            via: 0x405,
        });
        assert_eq!(CarInfo::of(&car).model.as_deref(), Some("Model Y 2022"));
    }
}
