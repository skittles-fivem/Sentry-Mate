#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! Figures that must agree between pages: an API-only drive's gaps, track and speed bands; Wh/km over the drives with
//! energy; the logbook against the drives list; unknown parked energy; Trip's charge kinds and moving speed; track
//! thinning. Rows are seeded straight into the database; made-up values, the open sea.

use engine::query::{self, Paging, Period};
use engine::{Engine, Options};
use model::{CarId, Charge, Drive, Gear, Position, Sample, Source};
use std::sync::Arc;
use timeline::{ApiSpan, ApiState};

/// 2026-01-01T00:00:00Z.
const T0: u64 = 1_767_225_600_000;
const MIN: u64 = 60_000;
const H: u64 = 3_600_000;
/// The day the seeded rows are on (UTC midnight).
const D: u64 = T0 + 24 * H;
const HOME: (f64, f64) = (-30.0, -140.0);

struct Kit {
    engine: Engine,
    db: db::Db,
    car: CarId,
    _dir: tempfile::TempDir,
    rt: tokio::runtime::Runtime,
}

fn kit() -> Kit {
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (data, keys) = (dir.path().join("data"), dir.path().join("keys"));
    std::fs::create_dir_all(&data).unwrap();
    let (v, _) = vault::Vault::create(&data, &keys, vault::Kdf::FAST_FOR_TESTS).unwrap();
    let v = Arc::new(v);
    let db = db::Db::open(&data, v.clone(), db::Options::default()).unwrap();
    let car = db
        .create_car(model::Car {
            id: 0,
            name: "Made-up".into(),
            vehicle_keys: Vec::new(),
            preferred_key: None,
            vin: None,
            profile_setting: None,
            time_zone: "UTC".into(),
            time_zone_typed: Some(true),
            efficiency_wh_km: None,
            created_ms: T0,
            inputs: Default::default(),
        })
        .unwrap()
        .id;
    let engine = {
        let _in = rt.enter();
        let store = ingest::Store::with_limits(data.join("can"), v, 0, 200_000, |_| u64::MAX);
        let opts = Options { workers: 2, api_rebuild_ms: 0, rebuild_settle_ms: 200, ..Options::default() };
        let engine = Engine::new(db.clone(), store, opts);
        engine.start().unwrap();
        engine
    };
    Kit { engine, db, car, _dir: dir, rt }
}

fn live_drive(k: &Kit, start: u64, minutes: u64, km: f64, kwh: Option<f64>) -> Drive {
    Drive {
        id: timeline::drive_id(k.car, start),
        car: k.car,
        start_ms: start,
        end_ms: start + minutes * MIN,
        start: None,
        end: None,
        start_place: None,
        end_place: None,
        start_address: None,
        end_address: None,
        start_odometer_km: None,
        end_odometer_km: None,
        distance_km: Some(km),
        speed_max_kmh: None,
        power_max_kw: None,
        power_min_kw: None,
        energy_used_kwh: kwh,
        start_soc: None,
        end_soc: None,
        start_range_km: None,
        end_range_km: None,
        outside_temp_avg_c: None,
        inside_temp_avg_c: None,
        ascent_m: None,
        descent_m: None,
        source: Source::Live,
        inputs: model::Inputs::SMO,
        rules_version: timeline::RULES_VERSION,
    }
}

fn put_drives(k: &Kit, drives: &[Drive]) {
    k.db.upsert(k.car, drives).unwrap();
}

fn day() -> Period {
    Period::new(Some(D), Some(D + 24 * H))
}

fn now_after_day() -> u64 {
    D + 72 * H
}

// ---------------------------------------------------------------------------------------------------------------
// A drive read by an API alone has data all along (its readings are an API's pace apart): no gaps, one track, speed
// bands, and the list and the page agree.

fn api_readings(every: u64, a: u64, b: u64, to: u64) -> Vec<Sample> {
    (0..to)
        .step_by(every as usize)
        .map(|s| {
            let moving = (a..b).contains(&s);
            let driven_km = (s.clamp(a, b) - a) as f64 * 0.01; // 36 km/h
            Sample {
                t_ms: T0 + s * 1000,
                lat: Some(10.0),
                lon: Some(-30.0 + driven_km / 110.0),
                speed_kmh: moving.then_some(36.0),
                gear: moving.then_some(Gear::D),
                odometer_km: Some(12_345.0 + driven_km),
                soc: Some(70.0),
                range_km: Some(200.0),
                battery_power_kw: Some(if moving { 10.0 } else { 0.2 }),
                inside_temp_c: Some(21.5),
                ..Sample::default()
            }
        })
        .collect()
}

fn api_only_drive(k: &Kit) -> (CarId, query::drives::DriveView) {
    let car = k
        .engine
        .api_new_car(&tesla::NewCar {
            vin_prefix: Some("XP7YGCEK0RB".into()),
            model: Some('Y'),
            year: Some(2024),
            display_name: Some("Blue".into()),
        })
        .unwrap();
    // a 30 minute drive (600 s to 2400 s), one reading every 15 s
    let span = ApiSpan {
        start_ms: T0,
        end_ms: T0 + 3000 * 1000,
        state: ApiState::Online,
        data: false,
        inputs: model::Inputs::NONE,
    };
    k.engine.api_points(car, api_readings(15, 600, 2400, 3000), vec![span]);
    k.rt.block_on(k.engine.idle());
    let list = query::drives::drives(&k.engine, car, &Default::default(), &Paging::default()).unwrap().body.items;
    assert_eq!(list.len(), 1, "{list:#?}");
    (car, list.into_iter().next().unwrap())
}

#[test]
fn api_only_drive_page_has_no_gaps() {
    let k = kit();
    let (car, listed) = api_only_drive(&k);
    let detail = query::drives::drive(&k.engine, car, listed.id.parse().unwrap(), None).unwrap();
    assert_eq!(
        detail.drive.flags.gap_ms, 0,
        "the page says {} ms of the drive have no data",
        detail.drive.flags.gap_ms
    );
    assert!(detail.gaps.is_empty(), "{} gaps", detail.gaps.len());
}

#[test]
fn api_only_drive_list_and_page_agree_on_gaps() {
    let k = kit();
    let (car, listed) = api_only_drive(&k);
    let detail = query::drives::drive(&k.engine, car, listed.id.parse().unwrap(), None).unwrap();
    assert_eq!(detail.drive.flags.gap_ms, listed.flags.gap_ms, "list and page disagree");
}

#[test]
fn api_only_drive_track_is_not_cut_into_single_points() {
    let k = kit();
    let (car, listed) = api_only_drive(&k);
    let track = query::drives::track(&k.engine, car, listed.id.parse().unwrap()).unwrap();
    let one_point = track.stretches.iter().filter(|s| s.len() < 2).count();
    assert_eq!(one_point, 0, "{} of {} stretches are a single point", one_point, track.stretches.len());
}

#[test]
fn api_only_drive_has_speed_bands() {
    let k = kit();
    let (car, listed) = api_only_drive(&k);
    let detail = query::drives::drive(&k.engine, car, listed.id.parse().unwrap(), None).unwrap();
    assert!(!detail.speed_bands.is_empty(), "no speed bands");
}

// ---------------------------------------------------------------------------------------------------------------
// Wh/km in Trip, Statistics and Compare counts only the km of drives with energy, as the Drives list does.

/// Two drives of 100 km on one day: 20 kWh and no energy.
fn two_drives_one_without_energy(k: &Kit) {
    put_drives(k, &[live_drive(k, D + 8 * H, 60, 100.0, Some(20.0)), live_drive(k, D + 14 * H, 60, 100.0, None)]);
}

#[test]
fn trip_wh_per_km_leaves_out_drives_without_energy() {
    let k = kit();
    two_drives_one_without_energy(&k);
    let list = query::drives::drives(&k.engine, k.car, &Default::default(), &Paging::default()).unwrap().body;
    assert_eq!(list.summary.wh_per_km, Some(200.0), "the list is the reference");
    let t = query::trips::trip(&k.engine, k.car, &day(), now_after_day()).unwrap().body;
    assert_eq!(t.wh_per_km.net, Some(200.0), "trip: {:?}", t.wh_per_km);
}

#[test]
fn statistics_net_wh_per_km_leaves_out_drives_without_energy() {
    let k = kit();
    two_drives_one_without_energy(&k);
    let s = query::reports::statistics(&k.engine, &[k.car], "day", &day(), &Paging::first(10)).unwrap();
    let row = s.items.iter().find(|r| r.drives == 2).unwrap_or_else(|| panic!("the day's row in {:?}", s.items));
    assert_eq!(row.net_wh_per_km, Some(200.0), "statistics: {:?}", row.net_wh_per_km);
}

#[test]
fn compare_wh_per_km_leaves_out_drives_without_energy() {
    let k = kit();
    two_drives_one_without_energy(&k);
    let c = query::reports::compare(&k.engine, &[k.car], Some("wh_per_km"), &day(), None).unwrap();
    assert_eq!(c.series[0].values, vec![200.0], "compare: {:?}", c.series[0].values);
}

// ---------------------------------------------------------------------------------------------------------------
// A live drive whose start an import fills has the same place, odometer and purpose in the logbook as in the list.

#[test]
fn logbook_row_shows_what_the_import_filled_like_the_drives_list() {
    let k = kit();
    let home = query::places::create(
        &k.engine,
        &query::places::Asker::admin(vec![k.car]),
        query::places::Draft {
            name: "Made-up home".into(),
            kind: "home".into(),
            lat: HOME.0,
            lon: HOME.1,
            radius_m: 100.0,
            time_zone: Some("UTC".into()),
            prices: Vec::new(),
            price_at: None,
        },
    )
    .unwrap()
    .place
    .id;
    query::logbook::add_rule(&k.engine, k.car, Some(&home), None, "all", "commute").unwrap();
    // a live drive with no start fix and no odometer; a TeslaMate import covers its start at the saved place
    let start = D + 8 * H + 10_000;
    put_drives(&k, &[live_drive(&k, start, 20, 10.0, Some(1.6))]);
    let import: Vec<Sample> = (0..22 * 6u64)
        .map(|i| Sample {
            t_ms: start - MIN + i * 10_000,
            lat: Some(HOME.0),
            lon: Some(HOME.1),
            odometer_km: Some(1000.0),
            ..Sample::default()
        })
        .collect();
    k.db.write_points(k.car, Source::Teslamate, start - MIN, start + 21 * MIN + 1, &import).unwrap();

    let list = query::drives::drives(&k.engine, k.car, &Default::default(), &Paging::default()).unwrap().body.items;
    let v = &list[0];
    assert_eq!(v.start.place.as_ref().map(|p| p.name.as_str()), Some("Made-up home"), "the list is the reference");
    assert_eq!(v.start_odometer_km, Some(1000.0));
    assert_eq!(v.purpose.as_ref().and_then(|p| p["value"].as_str()), Some("commute"));

    let book = query::logbook::logbook(&k.engine, k.car, &Period::new(Some(D), Some(D + 24 * H))).unwrap().body;
    let row = &book.rows[0];
    assert_eq!(row.start_place.as_deref(), Some("Made-up home"), "start place");
    assert_eq!(row.start_odometer_km, Some(1000.0), "start odometer");
    assert_eq!(row.purpose.as_deref(), Some("commute"), "purpose");
    assert_eq!(book.totals_by_purpose["commute"].drives, 1, "totals by purpose");
}

// ---------------------------------------------------------------------------------------------------------------
// The logbook's continuity check sees distance missing next to a drive without an odometer, and says a plain 0.

fn with_odo(mut d: Drive, start: Option<f64>, end: Option<f64>) -> Drive {
    d.start_odometer_km = start;
    d.end_odometer_km = end;
    d
}

fn continuity(k: &Kit) -> query::logbook::Continuity {
    let q = query::logbook::LogQuery::default();
    query::logbook::logbook_page(&k.engine, k.car, &Period::new(Some(D), Some(D + 24 * H)), &q).unwrap().body.continuity
}

#[test]
fn continuity_sees_missing_km_beside_a_drive_without_odometer() {
    let k = kit();
    put_drives(
        &k,
        &[
            with_odo(live_drive(&k, D + 8 * H, 60, 100.0, Some(15.0)), Some(1000.0), Some(1100.0)),
            with_odo(live_drive(&k, D + 10 * H, 10, 10.0, Some(2.0)), None, None),
            with_odo(live_drive(&k, D + 12 * H, 60, 100.0, Some(15.0)), Some(1600.0), Some(1700.0)),
        ],
    );
    let c = continuity(&k);
    // 1100 -> 1600 is 500 km on the odometer, 10 of them in the drive without one: 490 km are in no row
    assert_eq!(c.gaps, 1, "{c:?}");
    assert!((c.missing_km - 490.0).abs() < 1.0, "missing_km {}", c.missing_km);
}

#[test]
fn continuity_missing_km_is_a_plain_zero_when_nothing_is_missing() {
    let k = kit();
    put_drives(
        &k,
        &[
            with_odo(live_drive(&k, D + 8 * H, 60, 100.0, Some(15.0)), Some(1000.0), Some(1100.0)),
            with_odo(live_drive(&k, D + 12 * H, 60, 100.0, Some(15.0)), Some(1100.0), Some(1200.0)),
        ],
    );
    let c = continuity(&k);
    assert_eq!(c.gaps, 0);
    assert!(c.missing_km == 0.0 && c.missing_km.is_sign_positive(), "missing_km {:?}", c.missing_km);
    let json = serde_json::to_string(&c).unwrap();
    assert!(!json.contains("-0.0"), "{json}");
}

// ---------------------------------------------------------------------------------------------------------------
// Parked energy that is not known is null, not 0 kWh, and Trip has no gross consumption without it.

#[test]
fn unknown_parked_energy_is_not_zero() {
    let k = kit();
    // two drives with nothing known about the car between them (no samples, no ranges)
    put_drives(
        &k,
        &[live_drive(&k, D + 8 * H, 60, 100.0, Some(15.0)), live_drive(&k, D + 34 * H, 60, 10.0, Some(1.5))],
    );
    let p = query::parked::parked(&k.engine, k.car, &Period::new(Some(D), Some(D + 48 * H)), None, &Paging::first(10))
        .unwrap()
        .body;
    assert!(!p.items.is_empty(), "a spell between the drives");
    assert_eq!(p.items[0].energy_kwh, None, "the spell's energy is unknown");
    assert_eq!(p.summary.energy_kwh, None, "summary says {:?} kWh", p.summary.energy_kwh);
    let t = query::trips::trip(&k.engine, k.car, &day(), now_after_day()).unwrap().body;
    assert_eq!(t.wh_per_km.gross, None, "gross {:?} equals net {:?}", t.wh_per_km.gross, t.wh_per_km.net);
}

// ---------------------------------------------------------------------------------------------------------------
// Trip does not count a charge of unknown kind as AC.

#[test]
fn trip_does_not_count_a_charge_of_unknown_kind_as_ac() {
    let k = kit();
    let start = D + 2 * H;
    let ch = Charge {
        id: timeline::charge_id(k.car, start),
        car: k.car,
        start_ms: start,
        end_ms: start + 60 * MIN,
        position: Some(Position { t_ms: start, lat: HOME.0, lon: HOME.1, ..Position::default() }),
        place: None,
        address: None,
        kind: None,
        energy_added_kwh: Some(11.0),
        energy_used_kwh: None,
        energy_spans: None,
        start_soc: Some(40.0),
        end_soc: Some(60.0),
        start_range_km: None,
        end_range_km: None,
        power_max_kw: None,
        outside_temp_avg_c: None,
        battery_temp_avg_c: None,
        cost: None,
        source: Source::Live,
        inputs: Default::default(),
        rules_version: timeline::RULES_VERSION,
    };
    k.db.upsert(k.car, std::slice::from_ref(&ch)).unwrap();
    let t = query::trips::trip(&k.engine, k.car, &day(), now_after_day()).unwrap().body;
    assert_eq!(t.energy_kwh.added_ac, 0.0, "11 kWh of unknown kind counted as AC: {:?}", t.energy_kwh);
    assert_eq!(t.time_ms.charging_ac, 0, "its hour counted as AC charging time");
}

// ---------------------------------------------------------------------------------------------------------------
// Trip's moving speed is right when a drive runs past the end of the period.

#[test]
fn trip_speed_counts_only_the_km_driven_inside_the_period() {
    let k = kit();
    // two drives at 60 km/h: 10:00-11:00 (60 km) and 23:30 to 01:30 the next day (120 km)
    put_drives(
        &k,
        &[
            live_drive(&k, D + 10 * H, 60, 60.0, Some(10.0)),
            live_drive(&k, D + 23 * H + 30 * MIN, 120, 120.0, Some(20.0)),
        ],
    );
    let t = query::trips::trip(&k.engine, k.car, &day(), now_after_day()).unwrap().body;
    assert_eq!(t.time_ms.driving, 90 * MIN, "driving time clipped to the period: {:?}", t.time_ms);
    assert_eq!(t.speed_kph.moving, Some(60.0), "{:?} over {} km", t.speed_kph, t.distance.drives_km);
}

// ---------------------------------------------------------------------------------------------------------------
// Track thinning measures against the segment between a stretch's ends, not the endless line through them.

#[test]
fn simplify_keeps_the_turning_point_of_an_out_and_back_on_one_line() {
    let point = |i: usize, km_north: f64| query::drives::TrackPoint {
        t_ms: T0 + i as u64 * 1000,
        lat: 10.0 + km_north / 111.32,
        lon: -30.0,
        elevation_m: None,
        accuracy_m: None,
        speed_kph: None,
        power_kw: None,
        soc_pct: None,
        imported: Vec::new(),
    };
    // 10 km north in 100 m steps, then back down to 30 m north of the start, along the same meridian
    let mut km: Vec<f64> = (0..=100).map(|i| i as f64 * 0.1).collect();
    km.extend((0..100).map(|i| 9.9 - i as f64 * 0.1 + 0.03 * (i as f64 / 99.0)));
    let track: Vec<_> = km.iter().enumerate().map(|(i, k)| point(i, *k)).collect();
    let out = query::drives::simplify(&track, 5.0);
    let farthest = out.iter().map(|p| (p.lat - 10.0) * 111.32).fold(0.0, f64::max);
    assert!(
        farthest > 9.5,
        "{} of {} points kept; the farthest is {farthest:.2} km from the start",
        out.len(),
        track.len()
    );
}
