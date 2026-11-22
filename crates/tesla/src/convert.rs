//! Tesla's vehicle data in Sentry Mate's units. Tesla sends miles and mph whatever the car's screen shows, °C,
//! kW, kWh, V, A and bar; times in ms (sections) and s (`gps_as_of`). The car's display settings are never used
//! to convert. A field that is missing, null or of another type is `None`, never 0.

use model::{ChargeKind, Gear, Sample};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const KM_PER_MILE: f64 = 1.609_344;

/// What the API told about the car beside its [`Sample`]: values the CAN side does not decode (or not yet), for the
/// car's settings, Now and charges.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ApiMeta {
    pub at_ms: u64,
    /// The firmware, e.g. `2026.32.5` (the first word of `car_version`).
    pub car_version: Option<String>,
    pub display_name: Option<String>,
    pub car_type: Option<String>,
    pub trim_badging: Option<String>,
    pub exterior_color: Option<String>,
    pub wheel_type: Option<String>,
    pub roof_color: Option<String>,
    pub spoiler_type: Option<String>,
    pub right_hand_drive: Option<bool>,
    pub charge_limit_soc: Option<f64>,
    pub usable_battery_level: Option<f64>,
    pub est_range_km: Option<f64>,
    pub charging_state: Option<String>,
    pub charge_energy_added_kwh: Option<f64>,
    pub minutes_to_full_charge: Option<f64>,
    pub fast_charger_brand: Option<String>,
    pub fast_charger_type: Option<String>,
    pub conn_charge_cable: Option<String>,
    pub scheduled_charging_pending: Option<bool>,
    pub scheduled_charging_start_ms: Option<u64>,
    pub locked: Option<bool>,
    pub sentry_mode: Option<bool>,
    pub user_present: Option<bool>,
    pub climate_on: Option<bool>,
    pub preconditioning: Option<bool>,
    pub climate_keeper_mode: Option<String>,
    pub heading_deg: Option<f64>,
    pub software_update_status: Option<String>,
    pub software_update_version: Option<String>,
    /// The values no CAN message carries, as this answer gave them ([`model::off_can`]).
    pub off_can: model::OffCan,
}

/// What the car is doing, for the poller's cadence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Activity {
    pub driving: bool,
    pub charging: bool,
    /// Something that keeps the car awake on purpose (someone in it, climate, Sentry mode, a charge, a
    /// scheduled charge, a software update): the poller does not try to let it sleep.
    pub busy: bool,
}

/// One `vehicle_data` answer, converted.
#[derive(Debug, Clone, PartialEq)]
pub struct Reading {
    pub sample: Sample,
    pub meta: ApiMeta,
    pub activity: Activity,
}

fn f(v: &Value, section: &str, key: &str) -> Option<f64> {
    v.get(section)?.get(key)?.as_f64().filter(|x| x.is_finite())
}

fn b(v: &Value, section: &str, key: &str) -> Option<bool> {
    let x = v.get(section)?.get(key)?;
    x.as_bool().or_else(|| x.as_u64().map(|n| n != 0))
}

fn s(v: &Value, section: &str, key: &str) -> Option<String> {
    let t = v.get(section)?.get(key)?.as_str()?.trim();
    (!t.is_empty() && t != "<invalid>").then(|| t.chars().take(64).collect())
}

fn top_s(v: &Value, key: &str) -> Option<String> {
    let t = v.get(key)?.as_str()?.trim();
    (!t.is_empty()).then(|| t.chars().take(64).collect())
}

fn ms(v: &Value, section: &str) -> Option<u64> {
    v.get(section)?.get("timestamp")?.as_u64().filter(|t| *t > 1_000_000_000_000)
}

pub fn gear(word: &str) -> Option<Gear> {
    match word.trim() {
        "P" => Some(Gear::P),
        "R" => Some(Gear::R),
        "N" => Some(Gear::N),
        "D" => Some(Gear::D),
        _ => None,
    }
}

fn mi(x: Option<f64>) -> Option<f64> {
    x.map(|m| m * KM_PER_MILE)
}

/// A window's position number (0 closed, anything else open, partly or fully): whether it is open.
fn window(v: &Value, key: &str) -> Option<bool> {
    let x = v.get("vehicle_state")?.get(key)?;
    x.as_f64().filter(|n| n.is_finite()).map(|n| n != 0.0).or_else(|| x.as_bool())
}

/// A time in s since the epoch (positive) as ms.
fn secs_ms(v: &Value, section: &str, key: &str) -> Option<u64> {
    v.get(section)?.get(key)?.as_u64().filter(|t| *t > 0).map(|t| t * 1000)
}

/// What `vehicle_data` tells of the values no CAN message carries ([`model::OffCan`]).
pub fn off_can(v: &Value) -> model::OffCan {
    let update = v.get("vehicle_state").and_then(|x| x.get("software_update"));
    let word = |key: &str| {
        update.and_then(|u| u.get(key)).and_then(Value::as_str).map(|t| t.trim().chars().take(32).collect::<String>())
    };
    model::OffCan {
        version: s(v, "vehicle_state", "car_version").and_then(|c| c.split_whitespace().next().map(str::to_string)),
        // "" is Tesla's word for none waiting: kept as it is
        update_status: word("status").map(|w| w.to_ascii_lowercase()),
        update_version: word("version").and_then(|w| w.split_whitespace().next().map(str::to_string)),
        update_download_pct: update
            .and_then(|u| u.get("download_perc"))
            .and_then(Value::as_f64)
            .filter(|p| p.is_finite() && (0.0..=100.0).contains(p)),
        locked: b(v, "vehicle_state", "locked"),
        windows: ["fd_window", "fp_window", "rd_window", "rp_window"].map(|k| window(v, k)),
        sunroof_state: s(v, "vehicle_state", "sun_roof_state").map(|w| w.to_ascii_lowercase()),
        sunroof_pct: f(v, "vehicle_state", "sun_roof_percent_open").filter(|p| (0.0..=100.0).contains(p)),
        scheduled_charge_start_ms: secs_ms(v, "charge_state", "scheduled_charging_start_time"),
        scheduled_departure_ms: secs_ms(v, "charge_state", "scheduled_departure_time"),
        route_destination: s(v, "drive_state", "active_route_destination"),
        route_km: mi(f(v, "drive_state", "active_route_miles_to_arrival")),
        route_min: f(v, "drive_state", "active_route_minutes_to_arrival"),
        route_traffic_min: f(v, "drive_state", "active_route_traffic_minutes_delay"),
    }
}

/// Converts one `vehicle_data` response. `now_ms` is used only when the answer carries no time of its own.
pub fn reading(v: &Value, now_ms: u64) -> Reading {
    let at =
        ms(v, "drive_state").or_else(|| ms(v, "charge_state")).or_else(|| ms(v, "vehicle_state")).unwrap_or(now_ms);
    let shift = s(v, "drive_state", "shift_state");
    let charging_state = s(v, "charge_state", "charging_state");
    let charging = charging_state.as_deref().map(|c| matches!(c, "Charging" | "Starting"));
    let fast = b(v, "charge_state", "fast_charger_present");
    let lat = f(v, "drive_state", "latitude").or_else(|| f(v, "drive_state", "native_latitude"));
    let lon = f(v, "drive_state", "longitude").or_else(|| f(v, "drive_state", "native_longitude"));
    let (lat, lon) = match (lat, lon) {
        (Some(a), Some(o)) if (-90.0..=90.0).contains(&a) && (-180.0..=180.0).contains(&o) => (Some(a), Some(o)),
        _ => (None, None),
    };
    let sample = Sample {
        t_ms: at - at % 1000,
        lat,
        lon,
        speed_kmh: mi(f(v, "drive_state", "speed")),
        gear: shift.as_deref().and_then(gear),
        odometer_km: mi(f(v, "vehicle_state", "odometer")),
        soc: f(v, "charge_state", "battery_level"),
        range_km: mi(f(v, "charge_state", "battery_range")),
        ideal_range_km: mi(f(v, "charge_state", "ideal_battery_range")),
        battery_power_kw: f(v, "drive_state", "power"),
        outside_temp_c: f(v, "climate_state", "outside_temp"),
        inside_temp_c: f(v, "climate_state", "inside_temp"),
        charging,
        charge_kind: match (charging, fast) {
            (Some(true), Some(true)) => Some(ChargeKind::Dc),
            (Some(true), _) => Some(ChargeKind::Ac),
            _ => None,
        },
        charge_power_kw: if charging == Some(true) { f(v, "charge_state", "charger_power") } else { None },
        charger_voltage_v: if charging == Some(true) { f(v, "charge_state", "charger_voltage") } else { None },
        charger_current_a: if charging == Some(true) { f(v, "charge_state", "charger_actual_current") } else { None },
        tpms_bar: ["fl", "fr", "rl", "rr"].map(|w| f(v, "vehicle_state", &format!("tpms_pressure_{w}"))),
        ..Sample::default()
    };
    let version = s(v, "vehicle_state", "car_version").and_then(|c| c.split_whitespace().next().map(str::to_string));
    let meta = ApiMeta {
        at_ms: at,
        car_version: version,
        display_name: top_s(v, "display_name").or_else(|| s(v, "vehicle_state", "vehicle_name")),
        car_type: s(v, "vehicle_config", "car_type"),
        trim_badging: s(v, "vehicle_config", "trim_badging"),
        exterior_color: s(v, "vehicle_config", "exterior_color"),
        wheel_type: s(v, "vehicle_config", "wheel_type"),
        roof_color: s(v, "vehicle_config", "roof_color"),
        spoiler_type: s(v, "vehicle_config", "spoiler_type"),
        right_hand_drive: b(v, "vehicle_config", "rhd"),
        charge_limit_soc: f(v, "charge_state", "charge_limit_soc"),
        usable_battery_level: f(v, "charge_state", "usable_battery_level"),
        est_range_km: mi(f(v, "charge_state", "est_battery_range")),
        charging_state: charging_state.clone(),
        charge_energy_added_kwh: f(v, "charge_state", "charge_energy_added"),
        minutes_to_full_charge: f(v, "charge_state", "minutes_to_full_charge"),
        fast_charger_brand: s(v, "charge_state", "fast_charger_brand").filter(|_| fast == Some(true)),
        fast_charger_type: s(v, "charge_state", "fast_charger_type").filter(|_| fast == Some(true)),
        conn_charge_cable: s(v, "charge_state", "conn_charge_cable"),
        scheduled_charging_pending: b(v, "charge_state", "scheduled_charging_pending"),
        scheduled_charging_start_ms: v
            .get("charge_state")
            .and_then(|c| c.get("scheduled_charging_start_time"))
            .and_then(Value::as_u64)
            .map(|t| t * 1000),
        locked: b(v, "vehicle_state", "locked"),
        sentry_mode: b(v, "vehicle_state", "sentry_mode"),
        user_present: b(v, "vehicle_state", "is_user_present"),
        climate_on: b(v, "climate_state", "is_climate_on"),
        preconditioning: b(v, "climate_state", "is_preconditioning"),
        climate_keeper_mode: s(v, "climate_state", "climate_keeper_mode"),
        heading_deg: f(v, "drive_state", "heading"),
        software_update_status: v
            .get("vehicle_state")
            .and_then(|x| x.get("software_update"))
            .and_then(|u| u.get("status"))
            .and_then(Value::as_str)
            .filter(|t| !t.is_empty())
            .map(|t| t.chars().take(32).collect()),
        software_update_version: v
            .get("vehicle_state")
            .and_then(|x| x.get("software_update"))
            .and_then(|u| u.get("version"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(|t| t.chars().take(32).collect()),
        off_can: off_can(v),
    };
    let driving = matches!(sample.gear, Some(Gear::D | Gear::R | Gear::N)) || sample.speed_kmh.is_some_and(|x| x > 0.0);
    let plugged = charging_state.as_deref().is_some_and(|c| c != "Disconnected");
    let updating = meta.software_update_status.as_deref().is_some_and(|u| matches!(u, "downloading" | "installing"));
    let keeper = meta.climate_keeper_mode.as_deref().is_some_and(|m| m != "off");
    let busy = charging == Some(true)
        || meta.user_present == Some(true)
        || meta.climate_on == Some(true)
        || meta.preconditioning == Some(true)
        || meta.sentry_mode == Some(true)
        || keeper
        || updating
        || (plugged && meta.scheduled_charging_pending == Some(true));
    Reading { sample, meta, activity: Activity { driving, charging: charging == Some(true), busy: busy || driving } }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::float_cmp)]
    use super::*;
    use serde_json::json;

    fn near(a: Option<f64>, b: f64) -> bool {
        a.is_some_and(|a| (a - b).abs() < 1e-6)
    }

    #[test]
    fn units_and_missing_values() {
        let v = json!({
            "display_name": "Made-up car",
            "drive_state": {"timestamp": 1_800_000_000_123u64, "latitude": 12.5, "longitude": -45.25,
                            "speed": 50, "shift_state": "D", "power": -12, "heading": 90},
            "charge_state": {"battery_level": 64, "battery_range": 200.0, "ideal_battery_range": 210.5,
                             "est_battery_range": 180, "charging_state": "Disconnected", "charger_power": 0,
                             "fast_charger_present": false, "charge_limit_soc": 80},
            "climate_state": {"inside_temp": 21.5, "outside_temp": null, "is_climate_on": false},
            "vehicle_state": {"odometer": 10000.0, "car_version": "2026.32.5 abcdef0123", "locked": true,
                              "tpms_pressure_fl": 2.9, "tpms_pressure_rr": "x"},
            "vehicle_config": {"car_type": "model3", "rhd": false}
        });
        let r = reading(&v, 0);
        let s = &r.sample;
        assert_eq!(s.t_ms, 1_800_000_000_000);
        assert_eq!((s.lat, s.lon), (Some(12.5), Some(-45.25)));
        assert!(near(s.speed_kmh, 80.4672));
        assert_eq!(s.gear, Some(Gear::D));
        assert!(near(s.odometer_km, 16093.44));
        assert!(near(s.range_km, 321.8688));
        assert_eq!(s.soc, Some(64.0));
        assert_eq!(s.battery_power_kw, Some(-12.0));
        assert_eq!(s.outside_temp_c, None);
        assert_eq!(s.inside_temp_c, Some(21.5));
        assert_eq!(s.charging, Some(false));
        assert_eq!(s.charge_kind, None);
        assert_eq!(s.charge_power_kw, None);
        assert_eq!(s.tpms_bar, [Some(2.9), None, None, None]);
        assert_eq!(s.energy_remaining_kwh, None);
        assert_eq!(r.meta.car_version.as_deref(), Some("2026.32.5"));
        assert_eq!(r.meta.display_name.as_deref(), Some("Made-up car"));
        assert_eq!(r.meta.charge_limit_soc, Some(80.0));
        assert!(r.activity.driving && r.activity.busy && !r.activity.charging);
    }

    #[test]
    fn values_not_on_the_can_bus() {
        let v = json!({
            "drive_state": {"timestamp": 1_800_000_000_000u64, "active_route_destination": "Made-up Street 1",
                            "active_route_miles_to_arrival": 10.0, "active_route_minutes_to_arrival": 14.5,
                            "active_route_traffic_minutes_delay": 2},
            "charge_state": {"scheduled_charging_start_time": 1_800_003_600u64, "scheduled_departure_time": null},
            "vehicle_state": {"car_version": "2026.32.5 abcdef0123", "locked": false,
                              "fd_window": 0, "fp_window": 0, "rd_window": 2, "rp_window": null,
                              "sun_roof_state": "Vent", "sun_roof_percent_open": 15,
                              "software_update": {"status": "downloading", "version": "2026.38.1 ffff",
                                                  "download_perc": 40}}
        });
        let o = reading(&v, 0).meta.off_can;
        assert_eq!(o.version.as_deref(), Some("2026.32.5"));
        assert_eq!(o.update_status.as_deref(), Some("downloading"));
        assert_eq!(o.update_version.as_deref(), Some("2026.38.1"));
        assert_eq!(o.update_download_pct, Some(40.0));
        assert_eq!(o.locked, Some(false));
        assert_eq!(o.windows, [Some(false), Some(false), Some(true), None]);
        assert_eq!(o.sunroof_state.as_deref(), Some("vent"));
        assert_eq!(o.sunroof_pct, Some(15.0));
        assert_eq!(o.scheduled_charge_start_ms, Some(1_800_003_600_000));
        assert_eq!(o.scheduled_departure_ms, None);
        assert_eq!(o.route_destination.as_deref(), Some("Made-up Street 1"));
        assert!(near(o.route_km, 16.09344));
        assert_eq!((o.route_min, o.route_traffic_min), (Some(14.5), Some(2.0)));

        // no update waiting: Tesla's empty word is kept, meaning none waiting
        let none = json!({"vehicle_state": {"software_update": {"status": "", "version": " "}}});
        let o = reading(&none, 0).meta.off_can;
        assert_eq!((o.update_status.as_deref(), o.update_version), (Some(""), None));
        assert!(reading(&json!({}), 0).meta.off_can.is_empty());
    }

    #[test]
    fn a_fast_charge() {
        let v = json!({"charge_state": {"timestamp": 1_800_000_000_000u64, "charging_state": "Charging",
            "fast_charger_present": true, "fast_charger_brand": "Tesla", "fast_charger_type": "Supercharger",
            "charger_power": 120, "charger_voltage": 390, "charger_actual_current": 0}});
        let r = reading(&v, 5);
        assert_eq!(r.sample.charge_kind, Some(ChargeKind::Dc));
        assert_eq!(r.sample.charge_power_kw, Some(120.0));
        assert_eq!(r.meta.fast_charger_type.as_deref(), Some("Supercharger"));
        assert!(r.activity.charging && r.activity.busy && !r.activity.driving);
        assert_eq!(r.sample.lat, None);
    }

    #[test]
    fn a_quiet_parked_car() {
        let v = json!({"drive_state": {"timestamp": 1_800_000_000_000u64, "shift_state": null, "speed": null},
            "charge_state": {"charging_state": "Disconnected"},
            "climate_state": {"is_climate_on": false, "climate_keeper_mode": "off"},
            "vehicle_state": {"sentry_mode": false, "is_user_present": false}});
        let r = reading(&v, 0);
        assert_eq!(r.activity, Activity::default());
        let mut sentry = v.clone();
        sentry["vehicle_state"]["sentry_mode"] = json!(true);
        assert!(reading(&sentry, 0).activity.busy);
        let mut plan = v.clone();
        plan["charge_state"] = json!({"charging_state": "Stopped", "scheduled_charging_pending": true});
        assert!(reading(&plan, 0).activity.busy);
    }
}
