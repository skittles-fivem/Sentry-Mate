//! `/cars/{car}/now`: the car's newest values. Every value read from the car is `{ value, at_ms }` or null; the
//! snapshot's own facts (`state`, `since_ms`, `at_ms`, ...) are plain.

use super::{CarRef, PlaceBrief, QResult, car_of, kpa, place_ref, r};
use crate::{Engine, now_ms, records};
use db::StateRow;
use model::{CarId, ChargeKind, Drive, Gear, Now, Sample, Source, State};
use serde::Serialize;

/// No upload for longer than this: stale.
pub const STALE_MS: u64 = 3_600_000;

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct V<T> {
    pub value: T,
    pub at_ms: u64,
}

fn v<T>(x: Option<T>, at: u64) -> Option<V<T>> {
    x.map(|value| V { value, at_ms: at })
}

fn vf(x: Option<f64>, d: i32, at: u64) -> Option<V<f64>> {
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
    pub address: Option<String>,
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
    pub tyre_recommended_kpa: Option<f64>,
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
    match n.state {
        Some(State::Asleep) => "asleep",
        Some(State::Unknown) | None => "unknown",
        Some(State::Online) if n.sample.charging == Some(true) => "charging",
        Some(State::Online) => "parked",
    }
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

/// The car's Now.
pub fn now_view(e: &Engine, car: CarId) -> QResult<NowView> {
    let c = car_of(e, car)?;
    let db = e.db();
    let n = db.entry(car)?.and_then(|x| x.summary.now);
    let meta = records::meta(db, car)?;
    let received_ms = meta.last_done_ms;
    let now = now_ms();
    let s: Sample = n.as_ref().map(|n| n.sample.clone()).unwrap_or_default();
    let at = n.as_ref().map_or(0, |n| n.at_ms);
    // each value at the time it was recorded (a value kept from an older log keeps its own time)
    let times = records::now_at(db, car)?;
    let t = |f: &str| times.get(f).copied().unwrap_or(at);
    // the span the car is in now, and since when
    let last_state = db
        .list::<StateRow>(car, &db::Query { source: Some(Source::Can), ..db::Query::default().limit(1) })?
        .items
        .into_iter()
        .next();
    let state = state_word(n.as_ref());
    let since_ms = last_state.as_ref().map(|s| s.span.start_ms);
    let buses = last_state.as_ref().map_or_else(Vec::new, |s| {
        let awake = s.span.state == State::Online;
        vec![
            Bus {
                bus: "vehicle",
                awake: awake && s.span.buses.iter().any(|b| b == "can0"),
                at_ms: Some(s.span.end_ms),
            },
            Bus {
                bus: "chassis",
                awake: awake && s.span.buses.iter().any(|b| b == "can1"),
                at_ms: Some(s.span.end_ms),
            },
        ]
    });
    let places = e.places();
    let location = match (s.lat, s.lon) {
        (Some(lat), Some(lon)) => Some(Location {
            lat: vf(Some(lat), 6, t("lat")),
            lon: vf(Some(lon), 6, t("lon")),
            accuracy_m: vf(s.gps_accuracy_m, 1, t("gps_accuracy_m")),
            elevation_m: vf(s.elevation_m, 0, t("elevation_m")),
            heading_deg: None,
            place: places.place_at(lat, lon).map(|p| PlaceBrief { id: place_ref(p.id), name: p.name.clone() }),
            address: None,
        }),
        _ => None,
    };
    let driving = n.as_ref().is_some_and(|n| n.driving);
    let drive = if driving {
        db.list::<Drive>(car, &db::Query { source: Some(Source::Can), ..db::Query::default().limit(1) })?
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
    let charge_id = if charging {
        db.list::<model::Charge>(car, &db::Query { source: Some(Source::Can), ..db::Query::default().limit(1) })?
            .items
            .into_iter()
            .next()
            .filter(|ch| ch.end_ms + 300_000 >= at)
            .map(|ch| ch.id.to_string())
    } else {
        None
    };
    let charge = ChargeNow {
        status: v(s.charging.map(|c| if c { "charging" } else { "disconnected" }), t("charging")),
        plugged: plugged_now(&s, driving, &t, at).map(|p| V { value: p, at_ms: at }),
        kind: v(s.charge_kind.map(kind_word), t("charge_kind")),
        power_kw: vf(s.charge_power_kw, 1, t("charge_power_kw")),
        volts_v: vf(s.charger_voltage_v, 0, t("charger_voltage_v")),
        amps_a: vf(s.charger_current_a, 1, t("charger_current_a")),
        phases: None,
        pilot_a: None,
        limit_source: None,
        added_kwh: None,
        limit_pct: None,
        to_full_kwh: None,
        to_full_min: None,
        finish_ms: None,
        charge_id,
    };
    let mut reasons = Vec::new();
    if charging {
        reasons.push("charging");
    }
    Ok(NowView {
        car: CarRef(car),
        time_zone: c.time_zone.clone(),
        state,
        since_ms,
        at_ms: n.as_ref().map(|n| n.at_ms),
        received_ms,
        delay_s: received_ms.zip(n.as_ref().map(|n| n.at_ms)).map(|(rcv, a)| rcv.saturating_sub(a) as f64 / 1000.0),
        stale: received_ms.is_none_or(|rcv| now.saturating_sub(rcv) > STALE_MS),
        buses,
        driving,
        gear: v(s.gear.map(gear_word), t("gear")),
        speed_kph: vf(s.speed_kmh, 1, t("speed_kmh")),
        power_kw: vf(s.battery_power_kw, 1, t("battery_power_kw")),
        soc_pct: vf(s.soc, 1, t("soc")),
        soc_bms_pct: vf(s.soc_bms, 1, t("soc_bms")),
        usable_soc_pct: None,
        range_km: vf(s.range_km, 1, t("range_km")),
        ideal_range_km: vf(s.ideal_range_km, 1, t("ideal_range_km")),
        est_range_km: None,
        est_basis: None,
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
        flags: serde_json::json!({
            "sentry": null, "sentry_event_guess": null, "service_mode": null, "user_present": null,
            "keep_climate": null, "climate": null, "battery_preconditioning": null, "keep_warm": null,
            "accessory_power": null, "parking_brake": null, "reduced_range": null,
            "doors": { "fl": null, "fr": null, "rl": null, "rr": null }, "trunk": null, "frunk": null,
            "liftgate_deg": null, "charge_port_open": null, "display_on": null,
        }),
        charge,
        drive,
        awake_reasons: reasons,
        software: Software { version: None, version_source: None, version_since_ms: None, changed_since_ms: None },
    })
}
