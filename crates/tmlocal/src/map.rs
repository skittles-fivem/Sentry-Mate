//! TeslaMate's MQTT values (one topic per value, `<prefix>/cars/<id>/<name>`, plain text payloads, per TeslaMate's
//! public documentation) to [`model::Sample`]s: the newest value of each, kept per car, and a sample of all of them at
//! most about once a second while values change. The `state` topic says what TeslaMate sees of the car
//! ([`ApiFact`]: online, asleep, offline). Values a `Sample` has no field for but [`tesla::ApiMeta`] has (locked, the
//! software version and update, the scheduled charge start ...) go into an `ApiMeta` handed on beside. [`target`] is
//! the whole table: every topic, and where it goes (or that it is left out).

use model::{Gear, Sample};
use tesla::{ApiFact, ApiMeta, ApiState};

/// The longest TeslaMate car id taken (digits only).
pub const MAX_CAR_ID: usize = 9;

/// At most one `ApiMeta` per car this often.
pub const META_EVERY_MS: u64 = 10_000;

/// A new latitude (or longitude) waits this long for its other half (TeslaMate sends them as two topics, one after
/// the other) before the sample is made: a sample never pairs a new latitude with the longitude of an earlier
/// position. After it, the other half is taken as unchanged (TeslaMate sends only what changed).
pub const PAIR_WAIT_MS: u64 = 2_000;

/// A topic's place: `<prefix>/cars/<id>/<name>` → (`id`, `name`). Anything else (another prefix, more levels, an id
/// that is not a number): `None`.
pub fn split_topic<'a>(prefix: &str, topic: &'a str) -> Option<(&'a str, &'a str)> {
    let rest = topic.strip_prefix(prefix)?.strip_prefix("/cars/")?;
    let (id, name) = rest.split_once('/')?;
    let ok_id = !id.is_empty() && id.len() <= MAX_CAR_ID && id.bytes().all(|b| b.is_ascii_digit());
    let ok_name = !name.is_empty() && !name.contains('/') && name.len() <= 64;
    (ok_id && ok_name).then_some((id, name))
}

/// A value of the sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleField {
    Lat,
    Lon,
    Elevation,
    Speed,
    Heading,
    Power,
    Odometer,
    Soc,
    UsableSoc,
    RatedRange,
    IdealRange,
    ChargePower,
    ChargerVoltage,
    ChargerCurrent,
    ChargerPhases,
    ChargeLimit,
    /// Hours in TeslaMate, minutes in the sample.
    TimeToFull,
    PluggedIn,
    InsideTemp,
    OutsideTemp,
    Gear,
    Sentry,
    Trunk,
    Frunk,
    Climate,
    UserPresent,
    /// Front left, front right, rear left, rear right.
    Tpms(usize),
}

/// A value of the `ApiMeta` beside the samples.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetaField {
    Locked,
    /// The software version.
    Version,
    /// `true`: an update is available (`software_update_status` `available`).
    UpdateAvailable,
    UpdateVersion,
    /// An ISO 8601 time.
    ScheduledChargeStart,
    ChargeEnergyAdded,
    EstRange,
    /// One flag for all four windows.
    WindowsOpen,
    RouteDestination,
    /// Miles left on the route.
    RouteMiles,
    RouteMinutes,
    RouteTrafficMinutes,
}

/// Where a topic goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Sample(SampleField),
    Meta(MetaField),
    DisplayName,
    State,
    /// `{"latitude": .., "longitude": ..}` (newer TeslaMate versions).
    Location,
    /// Nothing here holds it (yet).
    LeftOut,
}

/// The table: every topic TeslaMate's documentation lists, and where it goes.
pub fn target(name: &str) -> Target {
    use MetaField as M;
    use SampleField as S;
    use Target::{Meta, Sample as V};
    match name {
        "display_name" => Target::DisplayName,
        "state" => Target::State,
        "location" => Target::Location,
        "latitude" => V(S::Lat),
        "longitude" => V(S::Lon),
        "elevation" => V(S::Elevation),
        "speed" => V(S::Speed),
        "heading" => V(S::Heading),
        "power" => V(S::Power),
        "odometer" => V(S::Odometer),
        "battery_level" => V(S::Soc),
        "usable_battery_level" => V(S::UsableSoc),
        "rated_battery_range_km" => V(S::RatedRange),
        "ideal_battery_range_km" => V(S::IdealRange),
        "est_battery_range_km" => Meta(M::EstRange),
        "charger_power" => V(S::ChargePower),
        "charger_voltage" => V(S::ChargerVoltage),
        "charger_actual_current" => V(S::ChargerCurrent),
        "charger_phases" => V(S::ChargerPhases),
        "charge_energy_added" => Meta(M::ChargeEnergyAdded),
        "charge_limit_soc" => V(S::ChargeLimit),
        "time_to_full_charge" => V(S::TimeToFull),
        "plugged_in" => V(S::PluggedIn),
        "inside_temp" => V(S::InsideTemp),
        "outside_temp" => V(S::OutsideTemp),
        "shift_state" => V(S::Gear),
        "sentry_mode" => V(S::Sentry),
        "trunk_open" => V(S::Trunk),
        "frunk_open" => V(S::Frunk),
        "is_climate_on" => V(S::Climate),
        "is_user_present" => V(S::UserPresent),
        "tpms_pressure_fl" => V(S::Tpms(0)),
        "tpms_pressure_fr" => V(S::Tpms(1)),
        "tpms_pressure_rl" => V(S::Tpms(2)),
        "tpms_pressure_rr" => V(S::Tpms(3)),
        "locked" => Meta(M::Locked),
        "version" => Meta(M::Version),
        "update_available" => Meta(M::UpdateAvailable),
        "update_version" => Meta(M::UpdateVersion),
        "scheduled_charging_start_time" => Meta(M::ScheduledChargeStart),
        "windows_open" => Meta(M::WindowsOpen),
        "active_route_destination" => Meta(M::RouteDestination),
        "active_route_miles_to_arrival" => Meta(M::RouteMiles),
        "active_route_minutes_to_arrival" => Meta(M::RouteMinutes),
        "active_route_traffic_minutes_delay" => Meta(M::RouteTrafficMinutes),
        // one flag for all doors (the sample has a flag per door, and one flag cannot say which), `since`, `healthy`,
        // the charge port door, ...: nothing holds them yet
        _ => Target::LeftOut,
    }
}

enum Slot<'a> {
    Num(&'a mut Option<f64>),
    Flag(&'a mut Option<bool>),
    Gear(&'a mut Option<Gear>),
}

fn slot(s: &mut Sample, f: SampleField) -> Slot<'_> {
    use SampleField as F;
    let x = &mut s.extra;
    match f {
        F::Lat => Slot::Num(&mut s.lat),
        F::Lon => Slot::Num(&mut s.lon),
        F::Elevation => Slot::Num(&mut s.elevation_m),
        F::Speed => Slot::Num(&mut s.speed_kmh),
        F::Heading => Slot::Num(&mut x.heading_deg),
        F::Power => Slot::Num(&mut s.battery_power_kw),
        F::Odometer => Slot::Num(&mut s.odometer_km),
        F::Soc => Slot::Num(&mut s.soc),
        F::UsableSoc => Slot::Num(&mut x.usable_soc),
        F::RatedRange => Slot::Num(&mut s.range_km),
        F::IdealRange => Slot::Num(&mut s.ideal_range_km),
        F::ChargePower => Slot::Num(&mut s.charge_power_kw),
        F::ChargerVoltage => Slot::Num(&mut s.charger_voltage_v),
        F::ChargerCurrent => Slot::Num(&mut s.charger_current_a),
        F::ChargerPhases => Slot::Num(&mut x.charger_phases),
        F::ChargeLimit => Slot::Num(&mut x.charge_limit_pct),
        F::TimeToFull => Slot::Num(&mut x.time_to_full_min),
        F::PluggedIn => Slot::Flag(&mut x.plugged_in),
        F::InsideTemp => Slot::Num(&mut s.inside_temp_c),
        F::OutsideTemp => Slot::Num(&mut s.outside_temp_c),
        F::Gear => Slot::Gear(&mut s.gear),
        F::Sentry => Slot::Flag(&mut x.sentry_mode),
        F::Trunk => Slot::Flag(&mut x.trunk_open),
        F::Frunk => Slot::Flag(&mut x.frunk_open),
        F::Climate => Slot::Flag(&mut x.climate_on),
        F::UserPresent => Slot::Flag(&mut x.user_present),
        F::Tpms(i) => Slot::Num(&mut s.tpms_bar[i.min(3)]),
    }
}

/// What one message did.
#[derive(Debug, Clone, PartialEq)]
pub enum Applied {
    /// A value of the sample (or of the `ApiMeta`) changed.
    Value,
    /// The car's state (TeslaMate's word, lower case) changed.
    State(String),
    /// TeslaMate's name of the car.
    Name(String),
    /// Nothing this server keeps (or the same value again).
    Nothing,
}

fn num(s: &str) -> Option<f64> {
    s.trim().parse::<f64>().ok().filter(|x| x.is_finite())
}

fn flag(s: &str) -> Option<bool> {
    match s.trim() {
        "true" | "1" => Some(true),
        "false" | "0" => Some(false),
        _ => None,
    }
}

fn text(s: &str) -> Option<String> {
    let t: String = s.trim().chars().filter(|c| !c.is_control()).take(64).collect();
    (!t.is_empty()).then_some(t)
}

/// `2026-11-22T05:00:00Z`, with fractions of a second or a `+hh:mm` offset → ms since 1970.
pub fn parse_time(s: &str) -> Option<u64> {
    let s = s.trim();
    let (date, rest) = s.split_once(['T', ' '])?;
    let mut d = date.split('-').map(|p| p.parse::<i64>().ok());
    let (y, m, day) = (d.next()??, d.next()??, d.next()??);
    let (clock, offset_s) = if let Some(c) = rest.strip_suffix('Z') {
        (c, 0)
    } else if let Some(i) = rest.rfind(['+', '-']).filter(|i| *i >= 8) {
        let (c, o) = rest.split_at(i);
        let sign = if o.starts_with('-') { -1 } else { 1 };
        let mut hm = o[1..].split(':').map(|p| p.parse::<i64>().ok());
        let (h, mi) = (hm.next()??, hm.next().flatten().unwrap_or(0));
        (c, sign * (h * 3600 + mi * 60))
    } else {
        (rest, 0)
    };
    let mut t = clock.split(':');
    let h = t.next()?.parse::<i64>().ok()?;
    let mi = t.next()?.parse::<i64>().ok()?;
    let sec = t.next().map_or(Some(0.0), |x| x.parse::<f64>().ok())?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&day) || h > 23 || mi > 59 || !(0.0..61.0).contains(&sec) {
        return None;
    }
    // days from the civil date (Howard Hinnant's algorithm)
    let yy = if m <= 2 { y - 1 } else { y };
    let era = yy.div_euclid(400);
    let yoe = yy - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let ms = (days * 86_400 + h * 3600 + mi * 60 - offset_s) * 1000 + (sec * 1000.0).round() as i64;
    u64::try_from(ms).ok()
}

/// TeslaMate's state word as what the API saw.
pub fn api_state(word: &str) -> Option<ApiState> {
    match word {
        "online" | "driving" | "charging" | "updating" => Some(ApiState::Online),
        "asleep" | "suspended" => Some(ApiState::Asleep),
        "offline" => Some(ApiState::Offline),
        _ => None,
    }
}

/// One car's newest values.
#[derive(Debug, Clone, Default)]
pub struct Car {
    values: Sample,
    meta: ApiMeta,
    state: Option<String>,
    /// When the current state was first seen, and when last.
    state_since_ms: u64,
    state_seen_ms: u64,
    dirty: bool,
    meta_dirty: bool,
    last_emit_ms: u64,
    last_meta_ms: u64,
    pub last_message_ms: Option<u64>,
    /// Which half of the position changed since the last whole one (latitude, longitude), and since when.
    half: (bool, bool),
    half_since_ms: u64,
}

fn put<T: PartialEq>(slot: &mut Option<T>, v: Option<T>) -> bool {
    if *slot == v {
        return false;
    }
    *slot = v;
    true
}

impl Car {
    pub fn state(&self) -> Option<&str> {
        self.state.as_deref()
    }

    /// One half of the position changed: a whole position once the other half changed too.
    fn half_changed(&mut self, lat: bool, now_ms: u64) {
        if self.half == (false, false) {
            self.half_since_ms = now_ms;
        }
        if lat {
            self.half.0 = true;
        } else {
            self.half.1 = true;
        }
        if self.half == (true, true) {
            self.half = (false, false);
        }
    }

    fn sample_value(&mut self, f: SampleField, p: &str, now_ms: u64) -> bool {
        let changed = match slot(&mut self.values, f) {
            Slot::Num(n) => {
                let v = match f {
                    SampleField::Lat => num(p).filter(|v| (-90.0..=90.0).contains(v)),
                    SampleField::Lon => num(p).filter(|v| (-180.0..=180.0).contains(v)),
                    SampleField::TimeToFull => num(p).map(|h| (h * 60.0).round()),
                    // (the API's count, passed on as it is: 2 is three phases)
                    SampleField::ChargerPhases => num(p).and_then(model::api_phases),
                    _ => num(p),
                };
                let changed = put(n, v);
                if changed && matches!(f, SampleField::Lat | SampleField::Lon) {
                    self.half_changed(f == SampleField::Lat, now_ms);
                }
                changed
            }
            Slot::Flag(b) => put(b, flag(p)),
            Slot::Gear(g) => {
                let v = match p.to_ascii_uppercase().as_str() {
                    "P" => Some(Gear::P),
                    "R" => Some(Gear::R),
                    "N" => Some(Gear::N),
                    "D" => Some(Gear::D),
                    _ => None,
                };
                put(g, v)
            }
        };
        self.dirty |= changed;
        changed
    }

    fn meta_value(&mut self, f: MetaField, p: &str) -> bool {
        let m = &mut self.meta;
        let changed = match f {
            MetaField::Locked => put(&mut m.locked, flag(p)) | put(&mut m.off_can.locked, flag(p)),
            MetaField::Version => {
                let v = text(p).and_then(|v| v.split_whitespace().next().map(str::to_string));
                put(&mut m.car_version, v.clone()) | put(&mut m.off_can.version, v)
            }
            MetaField::UpdateAvailable => {
                // true: one is available; false: none waiting ("" as the Tesla API writes it)
                let w = flag(p).map(|a| if a { "available" } else { "" }.to_string());
                put(&mut m.software_update_status, w.clone().filter(|w| !w.is_empty()))
                    | put(&mut m.off_can.update_status, w)
            }
            MetaField::UpdateVersion => {
                put(&mut m.software_update_version, text(p)) | put(&mut m.off_can.update_version, text(p))
            }
            MetaField::ScheduledChargeStart => {
                let t = parse_time(p);
                put(&mut m.scheduled_charging_pending, Some(t.is_some()))
                    | put(&mut m.scheduled_charging_start_ms, t)
                    | put(&mut m.off_can.scheduled_charge_start_ms, t)
            }
            MetaField::ChargeEnergyAdded => put(&mut m.charge_energy_added_kwh, num(p)),
            MetaField::EstRange => put(&mut m.est_range_km, num(p)),
            MetaField::WindowsOpen => put(&mut m.off_can.windows_open, flag(p)),
            MetaField::RouteDestination => put(&mut m.off_can.route_destination, text(p)),
            MetaField::RouteMiles => put(&mut m.off_can.route_km, num(p).map(|mi| mi * tesla::convert::KM_PER_MILE)),
            MetaField::RouteMinutes => put(&mut m.off_can.route_min, num(p)),
            MetaField::RouteTrafficMinutes => put(&mut m.off_can.route_traffic_min, num(p)),
        };
        self.meta_dirty |= changed;
        changed
    }

    fn set_state(&mut self, p: &str, now_ms: u64) -> Applied {
        let w = p.to_ascii_lowercase();
        if w.is_empty() || w.len() > 32 {
            return Applied::Nothing;
        }
        self.state_seen_ms = now_ms;
        if self.state.as_deref() == Some(w.as_str()) {
            return Applied::Nothing;
        }
        self.state_since_ms = now_ms;
        self.state = Some(w.clone());
        self.dirty = true;
        self.values.charging = Some(w == "charging");
        self.values.extra.update_installing = Some(w == "updating");
        Applied::State(w)
    }

    fn location(&mut self, p: &str) -> bool {
        let v: Option<serde_json::Value> = serde_json::from_str(p).ok();
        let lat = v.as_ref().and_then(|v| v["latitude"].as_f64()).filter(|v| (-90.0..=90.0).contains(v));
        let lon = v.as_ref().and_then(|v| v["longitude"].as_f64()).filter(|v| (-180.0..=180.0).contains(v));
        if lat.is_none() || lon.is_none() || (self.values.lat, self.values.lon) == (lat, lon) {
            return false;
        }
        (self.values.lat, self.values.lon) = (lat, lon);
        self.half = (false, false);
        self.dirty = true;
        true
    }

    /// Takes one message (`name`: the topic's last level). `now_ms`: when it came.
    pub fn apply(&mut self, name: &str, payload: &str, now_ms: u64) -> Applied {
        self.last_message_ms = Some(now_ms);
        // TeslaMate publishes `nil` (Elixir's null) for a value it does not have, as for the route when none is set:
        // no value, as an empty payload, on every topic
        let p = match payload.trim() {
            w if w.eq_ignore_ascii_case("nil") || w.eq_ignore_ascii_case("null") => "",
            w => w,
        };
        let changed = match target(name) {
            Target::DisplayName => {
                let n = text(p);
                if let Some(n) = &n
                    && put(&mut self.meta.display_name, Some(n.clone()))
                {
                    self.meta_dirty = true;
                }
                return n.map_or(Applied::Nothing, Applied::Name);
            }
            Target::State => return self.set_state(p, now_ms),
            Target::Location => self.location(p),
            Target::Sample(f) => self.sample_value(f, p, now_ms),
            Target::Meta(f) => self.meta_value(f, p),
            Target::LeftOut => false,
        };
        if changed { Applied::Value } else { Applied::Nothing }
    }

    /// A sample of the newest values when one changed and the last sample is `every_ms` old (its time: the start of
    /// the second). Charge values only while TeslaMate says the car charges.
    pub fn take(&mut self, now_ms: u64, every_ms: u64) -> Option<Sample> {
        if !self.dirty || (self.last_emit_ms != 0 && now_ms < self.last_emit_ms + every_ms) {
            return None;
        }
        if self.half != (false, false) {
            // half a new position: wait a while for the other half
            if now_ms < self.half_since_ms.saturating_add(PAIR_WAIT_MS) {
                return None;
            }
            self.half = (false, false);
        }
        self.dirty = false;
        self.last_emit_ms = now_ms;
        let mut s = self.values.clone();
        s.t_ms = now_ms / 1000 * 1000;
        if s.charging == Some(false) {
            s.charge_power_kw = None;
            s.charger_voltage_v = None;
            s.charger_current_a = None;
        }
        let any = s.lat.is_some()
            || s.soc.is_some()
            || s.odometer_km.is_some()
            || s.range_km.is_some()
            || s.speed_kmh.is_some()
            || s.battery_power_kw.is_some();
        any.then_some(s)
    }

    /// The `ApiMeta` values when one changed, at most every [`META_EVERY_MS`].
    pub fn take_meta(&mut self, now_ms: u64) -> Option<ApiMeta> {
        if !self.meta_dirty || (self.last_meta_ms != 0 && now_ms < self.last_meta_ms + META_EVERY_MS) {
            return None;
        }
        self.meta_dirty = false;
        self.last_meta_ms = now_ms;
        Some(ApiMeta { at_ms: now_ms, ..self.meta.clone() })
    }

    /// What TeslaMate says of the car's state, from when it first said so until now, while it said so in the last
    /// `fresh_ms` (TeslaMate sends nothing while a car sleeps, so a state is taken as lasting that long after it was
    /// last said, no longer).
    pub fn fact(&self, now_ms: u64, fresh_ms: u64) -> Option<ApiFact> {
        let state = api_state(self.state.as_deref()?)?;
        let seen = self.state_seen_ms.max(self.last_message_ms.unwrap_or(0));
        let end = now_ms.min(seen.saturating_add(fresh_ms));
        (end >= self.state_since_ms).then_some(ApiFact { start_ms: self.state_since_ms, end_ms: end, state })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topics() {
        assert_eq!(split_topic("teslamate", "teslamate/cars/1/speed"), Some(("1", "speed")));
        assert_eq!(split_topic("a/b", "a/b/cars/12/state"), Some(("12", "state")));
        for bad in ["teslamate/cars/x/speed", "teslamate/cars/1/a/b", "other/cars/1/speed", "teslamate/cars/1/"] {
            assert_eq!(split_topic("teslamate", bad), None, "{bad}");
        }
        assert_eq!(target("doors_open"), Target::LeftOut);
        assert_eq!(target("windows_open"), Target::Meta(MetaField::WindowsOpen));
        assert_eq!(target("locked"), Target::Meta(MetaField::Locked));
    }

    #[test]
    fn values_to_a_sample_about_once_a_second() {
        let mut c = Car::default();
        assert_eq!(c.apply("display_name", "Made-up Car", 1_000), Applied::Name("Made-up Car".into()));
        assert_eq!(c.apply("state", "charging", 1_000), Applied::State("charging".into()));
        assert_eq!(c.apply("battery_level", "55", 1_000), Applied::Value);
        assert_eq!(c.apply("battery_level", "55", 1_000), Applied::Nothing);
        c.apply("latitude", "45.0", 1_000);
        c.apply("longitude", "-93.0", 1_000);
        c.apply("charger_power", "7", 1_000);
        c.apply("time_to_full_charge", "1.5", 1_000);
        c.apply("charger_phases", "2", 1_000);
        c.apply("tpms_pressure_rr", "2.9", 1_000);
        c.apply("shift_state", "", 1_000);
        assert_eq!(c.apply("doors_open", "true", 1_000), Applied::Nothing);
        let s = c.take(1_500, 1_000).unwrap();
        assert_eq!((s.t_ms, s.soc, s.lat, s.lon), (1_000, Some(55.0), Some(45.0), Some(-93.0)));
        assert_eq!((s.charging, s.charge_power_kw, s.extra.time_to_full_min), (Some(true), Some(7.0), Some(90.0)));
        assert_eq!(s.extra.charger_phases, Some(3.0), "the API's 2 is three phases");
        assert_eq!(s.tpms_bar[3], Some(2.9));
        assert!(c.take(1_600, 1_000).is_none(), "nothing new");
        c.apply("battery_level", "56", 1_700);
        assert!(c.take(1_800, 1_000).is_none(), "not yet a second");
        assert_eq!(c.take(2_600, 1_000).unwrap().soc, Some(56.0));
        c.apply("state", "online", 3_000);
        let s = c.take(3_700, 1_000).unwrap();
        assert_eq!((s.charging, s.charge_power_kw), (Some(false), None), "no charge power while not charging");
    }

    #[test]
    fn a_new_latitude_waits_for_its_longitude() {
        let mut c = Car::default();
        c.apply("latitude", "45.0", 1_000);
        c.apply("longitude", "-93.0", 1_000);
        assert_eq!(c.take(1_500, 1_000).map(|s| (s.lat, s.lon)), Some((Some(45.0), Some(-93.0))));
        // a new latitude: no sample with the old longitude, the next has both
        c.apply("latitude", "45.01", 3_000);
        assert!(c.take(3_100, 1_000).is_none(), "half a position waits");
        c.apply("longitude", "-93.01", 3_400);
        assert_eq!(c.take(3_500, 1_000).map(|s| (s.lat, s.lon)), Some((Some(45.01), Some(-93.01))));
        // only the latitude changes (driving due north): after the wait the longitude stands
        c.apply("latitude", "45.02", 6_000);
        assert!(c.take(7_000, 1_000).is_none());
        assert_eq!(c.take(8_000, 1_000).map(|s| (s.lat, s.lon)), Some((Some(45.02), Some(-93.01))));
        // the location topic is a whole position
        c.apply("location", r#"{"latitude": 45.03, "longitude": -93.02}"#, 10_000);
        assert_eq!(c.take(10_100, 1_000).map(|s| (s.lat, s.lon)), Some((Some(45.03), Some(-93.02))));
    }

    #[test]
    fn meta_values() {
        let mut c = Car::default();
        c.apply("locked", "true", 1);
        c.apply("version", "2026.32.5 abc123", 1);
        c.apply("update_available", "true", 1);
        c.apply("update_version", "2026.38.1", 1);
        c.apply("scheduled_charging_start_time", "2026-11-22T05:00:00Z", 1);
        c.apply("charge_energy_added", "12.5", 1);
        let m = c.take_meta(1_000).unwrap();
        assert_eq!((m.locked, m.car_version.as_deref()), (Some(true), Some("2026.32.5")));
        assert_eq!(m.software_update_status.as_deref(), Some("available"));
        assert_eq!(m.software_update_version.as_deref(), Some("2026.38.1"));
        assert_eq!(
            (m.scheduled_charging_pending, m.scheduled_charging_start_ms),
            (Some(true), Some(1_795_323_600_000))
        );
        assert_eq!(m.charge_energy_added_kwh, Some(12.5));
        // the values no CAN message carries, for the car's Now (tagged local TeslaMate by the engine)
        let o = &m.off_can;
        assert_eq!((o.locked, o.version.as_deref()), (Some(true), Some("2026.32.5")));
        assert_eq!((o.update_status.as_deref(), o.update_version.as_deref()), (Some("available"), Some("2026.38.1")));
        assert_eq!(o.scheduled_charge_start_ms, Some(1_795_323_600_000));
        c.apply("windows_open", "false", 3_000_000);
        c.apply("active_route_destination", "Made-up Lake", 3_000_000);
        c.apply("active_route_miles_to_arrival", "10", 3_000_000);
        c.apply("active_route_minutes_to_arrival", "14", 3_000_000);
        c.apply("active_route_traffic_minutes_delay", "2", 3_000_000);
        c.apply("update_available", "false", 3_000_000);
        let o = c.take_meta(3_000_000).unwrap().off_can;
        assert_eq!((o.windows_open, o.windows), (Some(false), [None; 4]), "one flag, no window of its own");
        assert_eq!(o.route_destination.as_deref(), Some("Made-up Lake"));
        assert!(o.route_km.is_some_and(|k| (k - 16.093_44).abs() < 1e-9));
        assert_eq!((o.route_min, o.route_traffic_min), (Some(14.0), Some(2.0)));
        assert_eq!(o.update_status.as_deref(), Some(""), "none waiting");
        assert!(c.take_meta(2_000).is_none());
        assert_eq!(parse_time("2026-11-22T06:00:00.000+01:00"), Some(1_795_323_600_000));
        assert_eq!(parse_time("1970-01-01T00:00:01Z"), Some(1_000));
        assert_eq!(parse_time("not a time"), None);
    }

    #[test]
    fn nil_null_and_empty_are_no_value_on_every_topic() {
        let mut c = Car::default();
        c.apply("display_name", "Made-up car", 1);
        c.apply("active_route_destination", "Made-up Lake", 1);
        c.apply("active_route_miles_to_arrival", "10", 1);
        c.apply("version", "2026.32.5", 1);
        c.apply("battery_level", "70", 1);
        c.apply("speed", "30", 1);
        c.apply("locked", "true", 1);
        c.apply("shift_state", "D", 1);
        assert!(c.take_meta(1_000).is_some() && c.take(1_000, 1_000).is_some());
        // the route ended, and TeslaMate has no value for the rest for a while
        for (name, empty) in [
            ("active_route_destination", "nil"),
            ("active_route_miles_to_arrival", "nil"),
            ("version", "NULL"),
            ("speed", ""),
            ("locked", " nil "),
            ("shift_state", "null"),
        ] {
            assert_eq!(c.apply(name, empty, 2_000), Applied::Value, "{name} {empty:?} clears it");
        }
        let o = c.take_meta(1_000 + META_EVERY_MS).unwrap();
        assert_eq!(o.off_can.route_destination, None, "no route, not a route to \"nil\"");
        assert_eq!((o.off_can.route_km, o.off_can.version.as_deref(), o.locked), (None, None, None));
        let s = c.take(2_000, 1_000).unwrap();
        assert_eq!((s.soc, s.speed_kmh, s.gear), (Some(70.0), None, None), "the values it still has stay");
        // a name, a state and a place are never \"nil\" either
        assert_eq!(c.apply("display_name", "nil", 3_000), Applied::Nothing);
        assert_eq!(c.apply("state", "nil", 3_000), Applied::Nothing);
        assert_eq!(c.apply("location", "null", 3_000), Applied::Nothing);
        assert_eq!(c.meta.display_name.as_deref(), Some("Made-up car"));
        assert!(c.state().is_none());
    }

    #[test]
    fn states_as_facts() {
        let mut c = Car::default();
        assert!(c.fact(1, 10).is_none());
        c.apply("state", "asleep", 1_000);
        assert_eq!(c.fact(5_000, 60_000), Some(ApiFact { start_ms: 1_000, end_ms: 5_000, state: ApiState::Asleep }));
        assert_eq!(c.fact(500_000, 60_000).unwrap().end_ms, 61_000, "only as long as it is fresh");
        c.apply("state", "driving", 600_000);
        assert_eq!(c.fact(601_000, 60_000).unwrap().state, ApiState::Online);
        assert_eq!(api_state("offline"), Some(ApiState::Offline));
        assert_eq!(api_state("start"), None);
    }
}
