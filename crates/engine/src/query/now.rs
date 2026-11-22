//! `/cars/{car}/now`: the car's newest values. Every value read from the car is `{ value, at_ms }` or null; the
//! snapshot's own facts (`state`, `since_ms`, `at_ms`, ...) are plain.

use super::{Address, CarRef, PlaceBrief, QResult, car_of, kpa, place_ref, r};
use crate::{Engine, now_ms, records};
use model::{CarId, ChargeKind, Drive, Gear, Now, Sample, Source, State};
use serde::Serialize;

/// No upload for longer than this: stale.
pub const STALE_MS: u64 = 3_600_000;

/// A value read from the car: `{ value, at_ms, sources: [{ kind, label }] }`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct V<T> {
    pub value: T,
    pub at_ms: u64,
    /// The input that sent it.
    pub origin: model::Origin,
}

impl<T: Serialize> Serialize for V<T> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut st = s.serialize_struct("V", 3)?;
        st.serialize_field("value", &self.value)?;
        st.serialize_field("at_ms", &self.at_ms)?;
        st.serialize_field("sources", &[super::SourceRef::from(self.origin)])?;
        st.end()
    }
}

/// When a value was recorded and which input sent it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct At(pub u64, pub model::Origin);

/// A value of the car's newest values.
fn v<T>(x: Option<T>, at: At) -> Option<V<T>> {
    x.map(|value| V { value, at_ms: at.0, origin: at.1 })
}

/// A live input with when its newest value was recorded.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct InputNow {
    #[serde(flatten)]
    pub input: super::SourceRef,
    pub last_ms: Option<u64>,
}

fn vf(x: Option<f64>, d: i32, at: At) -> Option<V<f64>> {
    v(r(x, d), at)
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Bus {
    /// `vehicle` or `chassis`.
    pub bus: &'static str,
    pub awake: bool,
    pub at_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Location {
    pub lat: Option<V<f64>>,
    pub lon: Option<V<f64>>,
    pub accuracy_m: Option<V<f64>>,
    pub elevation_m: Option<V<f64>>,
    pub heading_deg: Option<V<f64>>,
    pub place: Option<PlaceBrief>,
    /// `{ short, locality, region, country_code }`; null until the spot's address is known.
    pub address: Option<Address>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ChargeNow {
    /// `disconnected`, `charging`, `complete`, `stopped` ... (null when the car does not say).
    pub status: Option<V<&'static str>>,
    pub plugged: Option<V<bool>>,
    pub kind: Option<V<&'static str>>,
    pub power_kw: Option<V<f64>>,
    pub volts_v: Option<V<f64>>,
    pub amps_a: Option<V<f64>>,
    pub phases: Option<V<f64>>,
    pub pilot_a: Option<V<f64>>,
    pub limit_source: Option<V<&'static str>>,
    pub added_kwh: Option<V<f64>>,
    pub limit_pct: Option<V<f64>>,
    pub to_full_kwh: Option<V<f64>>,
    pub to_full_min: Option<V<f64>>,
    pub finish_ms: Option<u64>,
    pub charge_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DriveNow {
    pub drive_id: String,
    pub start_ms: u64,
    pub distance_km: Option<f64>,
    pub energy_kwh: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Software {
    pub version: Option<String>,
    pub version_source: Option<&'static str>,
    pub version_since_ms: Option<u64>,
    pub changed_since_ms: Option<u64>,
}

/// The whole Now of a car.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NowView {
    pub car: CarRef,
    pub time_zone: String,
    /// `driving`, `charging`, `parked`, `asleep` or `unknown`.
    pub state: &'static str,
    pub since_ms: Option<u64>,
    /// The newest sample.
    pub at_ms: Option<u64>,
    /// When the log it came in was worked on.
    pub received_ms: Option<u64>,
    pub delay_s: Option<f64>,
    pub stale: bool,
    /// The live inputs these values came from, with each one's newest value.
    pub inputs: Vec<InputNow>,
    pub buses: Vec<Bus>,
    pub driving: bool,
    pub gear: Option<V<&'static str>>,
    pub speed_kph: Option<V<f64>>,
    pub power_kw: Option<V<f64>>,
    pub soc_pct: Option<V<f64>>,
    pub soc_bms_pct: Option<V<f64>>,
    pub usable_soc_pct: Option<V<f64>>,
    pub range_km: Option<V<f64>>,
    pub ideal_range_km: Option<V<f64>>,
    pub est_range_km: Option<V<f64>>,
    pub est_basis: Option<&'static str>,
    pub energy_remaining_kwh: Option<V<f64>>,
    pub odometer_km: Option<V<f64>>,
    pub outside_c: Option<V<f64>>,
    pub cabin_c: Option<V<f64>>,
    pub battery_c: Option<V<f64>>,
    pub setpoint_left_c: Option<V<f64>>,
    pub setpoint_right_c: Option<V<f64>>,
    pub lv_v: Option<V<f64>>,
    pub location: Option<Location>,
    pub tyres_kpa: [Option<V<f64>>; 4],
    pub tyres_slots_mapped: bool,
    pub tyre_recommended_kpa: Option<V<f64>>,
    pub flags: serde_json::Value,
    pub charge: ChargeNow,
    pub drive: Option<DriveNow>,
    pub awake_reasons: Vec<&'static str>,
    pub software: Software,
}

/// The state word of a Now: driving, charging, parked, asleep or unknown.
pub fn state_word(n: Option<&Now>) -> &'static str {
    let Some(n) = n else { return "unknown" };
    if n.driving {
        return "driving";
    }
    // no state span says so (the newest span is a sliver of unknown), but the newest sample shows the car standing
    // still: it is parked (online)
    let still = n.sample.gear == Some(Gear::P) || n.sample.speed_kmh == Some(0.0);
    match n.state {
        Some(State::Asleep) => "asleep",
        Some(State::Unknown) | None if !still => "unknown",
        _ if n.sample.charging == Some(true) => "charging",
        _ => "parked",
    }
}

/// The state word of a Now, with the car's newest state spans (newest first): as [`state_word`], but an unknown
/// state after a log that closed quiet (an asleep span with no frames after the newest sample) is asleep.
pub(crate) fn state_word_with(n: Option<&Now>, spans: &[model::StateSpan]) -> &'static str {
    let word = state_word(n);
    let Some(n) = n.filter(|n| !n.driving && matches!(n.state, Some(State::Unknown) | None)) else { return word };
    let slept = spans
        .iter()
        .find(|s| s.state != State::Unknown)
        .is_some_and(|s| s.state == State::Asleep && s.end_ms >= n.at_ms);
    if slept { "asleep" } else { word }
}

pub(crate) fn gear_word(g: Gear) -> &'static str {
    match g {
        Gear::P => "P",
        Gear::R => "R",
        Gear::N => "N",
        Gear::D => "D",
    }
}

pub(crate) fn kind_word(k: ChargeKind) -> &'static str {
    match k {
        ChargeKind::Ac => "ac",
        ChargeKind::Dc => "dc",
    }
}

/// Whether the car is plugged in, from its newest second only (`t`: when each value was recorded, `at`: the newest
/// second): a charger kind or charging then says yes, driving says no; an older value says nothing.
pub(crate) fn plugged_now(s: &Sample, driving: bool, t: &dyn Fn(&str) -> u64, at: u64) -> Option<bool> {
    if driving {
        return Some(false);
    }
    let fresh = |f: &str| t(f) >= at;
    if s.charge_kind.is_some() && fresh("charge_kind") || s.charging == Some(true) && fresh("charging") {
        return Some(true);
    }
    None
}

/// The charge status word of the car (`disconnected`, `no_power`, ...), or `None` for a word the API does not name.
fn charge_status_word(w: &str) -> Option<&'static str> {
    ["disconnected", "no_power", "about_to_charge", "charging", "complete", "stopped"].into_iter().find(|k| *k == w)
}

/// Which limit sets the AC current: `pilot`, `cable` or `car`.
fn limit_source_word(w: &str) -> Option<&'static str> {
    ["pilot", "cable", "car"].into_iter().find(|k| *k == w)
}

/// The car's consumption over the last 30 days of drives before `at`, Wh/km (`None` under 10 km of drives with
/// energy, or an implausible figure).
fn recent_wh_km(e: &Engine, car: CarId, at: u64) -> QResult<Option<f64>> {
    let from = at.saturating_sub(30 * 86_400_000);
    let mut kwh = 0.0;
    let mut km = 0.0;
    for d in super::drives::rows(e, car, &super::Period::new(Some(from), Some(at.saturating_add(1))), None)? {
        if let (Some(k), Some(x)) = (d.energy_used_kwh, d.distance_km)
            && k > 0.0
            && x > 0.0
        {
            kwh += k;
            km += x;
        }
    }
    Ok((km >= 10.0).then(|| kwh * 1000.0 / km).filter(|w| *w > 50.0))
}

/// Sentry Mate's own range estimate, km: the energy left in `s` at the consumption of the last 30 days of drives
/// before `at` (`est_range_km` of the Now, `range_estimated` on MQTT).
pub fn estimated_range_km(e: &Engine, car: CarId, s: &Sample, at: u64) -> QResult<Option<f64>> {
    let Some(kwh) = s.energy_remaining_kwh else { return Ok(None) };
    Ok(recent_wh_km(e, car, at)?.map(|w| kwh * 1000.0 / w))
}

/// The car's stored Now with Sentry Mate's own figures filled in (the range estimate and the Data health verdict),
/// as MQTT publishes it. `None` before the car sent anything.
pub fn published_now(e: &Engine, car: CarId) -> QResult<Option<Now>> {
    let Some(mut n) = e.db().entry(car)?.and_then(|x| x.summary.now) else { return Ok(None) };
    n.sample.odometer_km = possible_odometer(n.sample.odometer_km);
    // (a figure that can't be worked out now is left out, not the whole Now)
    n.range_estimated_km = estimated_range_km(e, car, &n.sample, n.at_ms).ok().flatten();
    n.healthy = super::system::healthy(e, car).ok();
    Ok(Some(n))
}

/// No car reads past 2,000,000 km: a Now kept before the CAN odometer's "not available" (4,294,967 km) was read as
/// such shows no odometer rather than that.
fn possible_odometer(km: Option<f64>) -> Option<f64> {
    km.filter(|k| (0.0..=2.0e6).contains(k))
}

/// The car's Now.
pub fn now_view(e: &Engine, car: CarId) -> QResult<NowView> {
    let c = car_of(e, car)?;
    let db = e.db();
    let n = db.entry(car)?.and_then(|x| x.summary.now);
    let meta = records::meta(db, car)?;
    // when the newest data arrived: SMO's newest log, or the Tesla API's newest reading
    let received_ms = meta.received_ms();
    let now = now_ms();
    let mut s: Sample = n.as_ref().map(|n| n.sample.clone()).unwrap_or_default();
    s.odometer_km = possible_odometer(s.odometer_km);
    let at = n.as_ref().map_or(0, |n| n.at_ms);
    // each value at the time it was recorded (a value kept from an older log keeps its own time)
    let times = records::now_at(db, car)?;
    let from = records::now_from(db, car)?;
    let origin = |f: &str| from.get(f).map_or(model::Origin::Smo, |i| model::Origin::from(*i));
    let t = |f: &str| At(times.get(f).copied().unwrap_or(at), origin(f));
    let tm = |f: &str| t(f).0;
    // the Tesla API's newest values beside its samples (charge limit, flags), while they are the car's newest
    let api = records::api_meta(db, car)?;
    let api_at = At(api.meta.at_ms, model::Origin::TeslaApi);
    let api_fresh = api.meta.at_ms > 0 && api.meta.at_ms + STALE_MS >= at;
    let m = api_fresh.then_some(&api.meta);
    // the car's newest state spans, joined across the rows they are stored in (the newest row is often a stub at
    // the end of the newest log)
    let spans = super::timeline::newest_spans(e, car, 8)?;
    let state = state_word_with(n.as_ref(), &spans);
    let places = e.places();
    let location = match (s.lat, s.lon) {
        (Some(lat), Some(lon)) => Some(Location {
            lat: vf(Some(lat), 6, t("lat")),
            lon: vf(Some(lon), 6, t("lon")),
            accuracy_m: vf(s.gps_accuracy_m, 1, t("gps_accuracy_m")),
            elevation_m: vf(s.elevation_m, 0, t("elevation_m")),
            heading_deg: vf(s.extra.heading_deg, 0, t("extra.heading_deg"))
                .or_else(|| m.and_then(|m| vf(m.heading_deg, 0, api_at))),
            place: places.place_at(lat, lon).map(|p| PlaceBrief { id: place_ref(p.id), name: p.name.clone() }),
            address: None,
        }),
        _ => None,
    };
    let driving = n.as_ref().is_some_and(|n| n.driving);
    let drive = if driving {
        db.list::<Drive>(car, &db::Query { source: Some(Source::Live), ..db::Query::default().limit(1) })?
            .items
            .into_iter()
            .next()
            .filter(|d| d.end_ms + 120_000 >= at)
            .map(|d| DriveNow {
                drive_id: d.id.to_string(),
                start_ms: d.start_ms,
                distance_km: r(s.odometer_km.zip(d.start_odometer_km).map(|(a, b)| a - b).or(d.distance_km), 3),
                energy_kwh: r(d.energy_used_kwh, 3),
            })
    } else {
        None
    };
    let charging = s.charging == Some(true);
    let current_charge = if charging {
        db.list::<model::Charge>(car, &db::Query { source: Some(Source::Live), ..db::Query::default().limit(1) })?
            .items
            .into_iter()
            .next()
            .filter(|ch| ch.end_ms + 300_000 >= at)
    } else {
        None
    };
    let charge_start = current_charge.as_ref().map(|ch| ch.start_ms);
    let charge_id = current_charge.map(|ch| ch.id.to_string());
    // since when: the drive's or charge's start, else the newest span of the state the car is in
    let want = match state {
        "asleep" => Some(State::Asleep),
        "unknown" => Some(State::Unknown),
        "parked" => Some(State::Online),
        _ => None,
    };
    let since_ms = match state {
        "driving" => drive.as_ref().map(|d| d.start_ms),
        "charging" => charge_start,
        _ => None,
    }
    .or_else(|| want.and_then(|w| spans.iter().find(|s| s.state == w)).map(|s| s.start_ms))
    .or_else(|| spans.first().map(|s| s.start_ms));
    // the buses: those of the newest awake span while the car is awake, else none
    let awake_now = matches!(state, "driving" | "charging" | "parked");
    let online = spans.iter().find(|s| s.state == State::Online);
    let buses = match (spans.first(), online) {
        (None, _) => Vec::new(),
        (Some(newest), online) => {
            let on = |bus: &str| awake_now && online.is_some_and(|s| s.buses.iter().any(|b| b == bus));
            let at = Some(if awake_now { online.map_or(newest.end_ms, |s| s.end_ms) } else { newest.end_ms });
            vec![
                Bus { bus: "vehicle", awake: on("can0"), at_ms: at },
                Bus { bus: "chassis", awake: on("can1"), at_ms: at },
            ]
        }
    };
    let charge = ChargeNow {
        status: v(s.extra.charge_status.as_deref().and_then(charge_status_word), t("extra.charge_status"))
            .or_else(|| v(s.charging.map(|c| if c { "charging" } else { "disconnected" }), t("charging"))),
        plugged: v(s.extra.plugged_in, t("extra.plugged_in")).or_else(|| {
            plugged_now(&s, driving, &tm, at).map(|p| V { value: p, at_ms: at, origin: origin("charging") })
        }),
        kind: v(s.charge_kind.map(kind_word), t("charge_kind")),
        power_kw: vf(s.charge_power_kw, 1, t("charge_power_kw")),
        volts_v: vf(s.charger_voltage_v, 0, t("charger_voltage_v")),
        amps_a: vf(s.charger_current_a, 1, t("charger_current_a")),
        phases: vf(s.extra.charger_phases, 0, t("extra.charger_phases")),
        pilot_a: vf(s.extra.charge_current_limit_a, 1, t("extra.charge_current_limit_a")),
        limit_source: v(
            s.extra.charge_limit_source.as_deref().and_then(limit_source_word),
            t("extra.charge_limit_source"),
        ),
        added_kwh: m.filter(|_| charging).and_then(|m| vf(m.charge_energy_added_kwh, 2, api_at)),
        limit_pct: vf(s.extra.charge_limit_pct, 0, t("extra.charge_limit_pct"))
            .or_else(|| m.and_then(|m| vf(m.charge_limit_soc, 0, api_at))),
        to_full_kwh: None,
        to_full_min: vf(s.extra.time_to_full_min, 0, t("extra.time_to_full_min"))
            .or_else(|| m.filter(|_| charging).and_then(|m| vf(m.minutes_to_full_charge, 0, api_at))),
        finish_ms: None,
        charge_id,
    };
    let mut reasons = Vec::new();
    if charging {
        reasons.push("charging");
    }
    let est_range = estimated_range_km(e, car, &s, at)?;
    // the software the car runs: the newest update's version
    let update =
        db.list::<db::UpdateRow>(car, &db::Query::default().limit(1))?.items.into_iter().next().map(|u| u.update);
    Ok(NowView {
        car: CarRef(car),
        time_zone: c.time_zone.clone(),
        state,
        since_ms,
        at_ms: n.as_ref().map(|n| n.at_ms),
        received_ms,
        delay_s: received_ms.zip(n.as_ref().map(|n| n.at_ms)).map(|(rcv, a)| rcv.saturating_sub(a) as f64 / 1000.0),
        stale: received_ms.is_none_or(|rcv| now.saturating_sub(rcv) > STALE_MS),
        inputs: {
            // each live input with its newest value: SMO's from its values in the Now, the API's from its newest
            // reading
            let smo_last = times
                .iter()
                .filter(|(f, _)| !from.contains_key(f.as_str()))
                .map(|(_, t)| *t)
                .max()
                .or_else(|| n.as_ref().filter(|_| times.is_empty()).map(|n| n.at_ms));
            let mut v = Vec::new();
            if let Some(t) = smo_last.filter(|_| n.is_some()) {
                v.push(InputNow { input: model::Origin::Smo.into(), last_ms: Some(t) });
            }
            for i in model::Input::apis() {
                if let Some(t) = meta.api_times(i).0 {
                    v.push(InputNow { input: model::Origin::from(i).into(), last_ms: Some(t) });
                }
            }
            v
        },
        buses,
        driving,
        gear: v(s.gear.map(gear_word), t("gear")),
        speed_kph: vf(s.speed_kmh, 1, t("speed_kmh")),
        power_kw: vf(s.battery_power_kw, 1, t("battery_power_kw")),
        soc_pct: vf(s.soc, 1, t("soc")),
        soc_bms_pct: vf(s.soc_bms, 1, t("soc_bms")),
        usable_soc_pct: vf(s.extra.usable_soc, 1, t("extra.usable_soc"))
            .or_else(|| m.and_then(|m| vf(m.usable_battery_level, 1, api_at))),
        range_km: vf(s.range_km, 1, t("range_km")),
        ideal_range_km: vf(s.ideal_range_km, 1, t("ideal_range_km")),
        est_range_km: vf(est_range, 1, t("energy_remaining_kwh")),
        est_basis: est_range.map(|_| "wh_per_km_30d"),
        energy_remaining_kwh: vf(s.energy_remaining_kwh, 2, t("energy_remaining_kwh")),
        odometer_km: vf(s.odometer_km, 3, t("odometer_km")),
        outside_c: vf(s.outside_temp_c, 1, t("outside_temp_c")),
        cabin_c: vf(s.inside_temp_c, 1, t("inside_temp_c")),
        battery_c: vf(s.battery_temp_c, 1, t("battery_temp_c")),
        setpoint_left_c: None,
        setpoint_right_c: None,
        lv_v: vf(s.lv_voltage_v, 2, t("lv_voltage_v")),
        location,
        tyres_kpa: [0, 1, 2, 3].map(|i| v(kpa(s.tpms_bar[i]), t(&format!("tpms_bar{i}")))),
        tyres_slots_mapped: false,
        tyre_recommended_kpa: None,
        flags: {
            // the car's own word (CAN) first, the Tesla API's while its reading is current
            let x = &s.extra;
            let can = |value: Option<bool>, f: &str| v(value, t(&format!("extra.{f}")));
            let climate_word = |on: bool, pre: Option<bool>| match (on, pre) {
                (true, Some(true)) => "precondition",
                (true, _) => "on",
                (false, _) => "off",
            };
            serde_json::json!({
                "sentry": can(x.sentry_mode, "sentry_mode").or_else(|| m.and_then(|m| v(m.sentry_mode, api_at))),
                "sentry_event_guess": null,
                "service_mode": can(x.service_mode, "service_mode"),
                "user_present": can(x.user_present, "user_present")
                    .or_else(|| m.and_then(|m| v(m.user_present, api_at))),
                "keep_climate": v(x.keep_climate.clone(), t("extra.keep_climate"))
                    .or_else(|| m.and_then(|m| v(m.climate_keeper_mode.clone(), api_at))),
                "climate": v(x.climate_on.map(|on| climate_word(on, x.preconditioning_cabin)), t("extra.climate_on"))
                    .or_else(|| m.and_then(|m| v(m.climate_on.map(|on| climate_word(on, m.preconditioning)), api_at))),
                "battery_preconditioning": can(x.preconditioning_battery, "preconditioning_battery")
                    .or_else(|| m.and_then(|m| v(m.preconditioning, api_at))),
                "keep_warm": null,
                "accessory_power": null, "parking_brake": null, "reduced_range": null,
                "doors": {
                    "fl": can(x.door_fl, "door_fl"), "fr": can(x.door_fr, "door_fr"),
                    "rl": can(x.door_rl, "door_rl"), "rr": can(x.door_rr, "door_rr"),
                },
                "trunk": can(x.trunk_open, "trunk_open"), "frunk": can(x.frunk_open, "frunk_open"),
                "liftgate_deg": null,
                "charge_port_open": can(x.charge_port_open, "charge_port_open"),
                "display_on": can(x.display_on, "display_on"),
            })
        },
        charge,
        drive,
        awake_reasons: reasons,
        software: Software {
            version: update.as_ref().map(|u| u.version.clone()).filter(|v| !v.is_empty()),
            version_source: update.as_ref().and_then(|u| {
                if let Some(site) = u.source.site() {
                    Some(site.word())
                } else if u.inputs.contains(model::Input::TeslaApi) {
                    Some("tesla_api")
                } else {
                    None
                }
            }),
            version_since_ms: update.as_ref().map(|u| u.end_ms.unwrap_or(u.start_ms)),
            changed_since_ms: None,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use model::StateSpan;

    fn now(state: State, gear: Option<Gear>, speed: Option<f64>) -> Now {
        let sample = Sample { gear, speed_kmh: speed, ..Sample::default() };
        Now {
            car: 1,
            at_ms: 10_000,
            state: Some(state),
            driving: false,
            sample,
            place: None,
            range_estimated_km: None,
            healthy: None,
        }
    }

    fn span(state: State, start_ms: u64, end_ms: u64) -> StateSpan {
        StateSpan {
            car: 1,
            state,
            start_ms,
            end_ms,
            buses: Vec::new(),
            source: Source::Live,
            inputs: Default::default(),
        }
    }

    #[test]
    fn an_unknown_sliver_with_the_car_standing_still_is_parked_or_asleep() {
        // gear P (or speed 0) in the newest sample: parked, not unknown
        assert_eq!(state_word(Some(&now(State::Unknown, Some(Gear::P), None))), "parked");
        assert_eq!(state_word(Some(&now(State::Unknown, None, Some(0.0)))), "parked");
        // nothing says the car stands still: still unknown
        assert_eq!(state_word(Some(&now(State::Unknown, Some(Gear::D), Some(30.0)))), "unknown");
        assert_eq!(state_word(None), "unknown");
        // the newest log closed quiet (asleep after the newest sample), then an unknown sliver: asleep
        let n = now(State::Unknown, Some(Gear::P), None);
        let spans =
            [span(State::Unknown, 20_000, 21_000), span(State::Asleep, 10_500, 20_000), span(State::Online, 0, 10_500)];
        assert_eq!(state_word_with(Some(&n), &spans), "asleep");
        // the newest non-unknown span is online: parked
        assert_eq!(state_word_with(Some(&n), &spans[2..]), "parked");
    }
}
