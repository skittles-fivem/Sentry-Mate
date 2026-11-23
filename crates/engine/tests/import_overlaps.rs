//! Imported history beside live history and beside another import, where a row is in both: a drive or charge that
//! was going on when live logging began, and a software update both imports have. Rows seeded straight into the
//! database; made-up values, the open sea.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use engine::query::{self, Paging, Period};
use engine::{Engine, Options};
use model::{CarId, ChargeKind, Position, Source, State, StateSpan};
use serde_json::{Value, json};
use std::sync::Arc;

/// 2026-01-01T00:00:00Z.
const T0: u64 = 1_767_225_600_000;
const MIN: u64 = 60_000;
const H: u64 = 3_600_000;
const DAY: u64 = 86_400_000;

struct Kit {
    engine: Engine,
    db: db::Db,
    car: CarId,
    _dir: tempfile::TempDir,
    _rt: tokio::runtime::Runtime,
}

fn kit() -> Kit {
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let (v, _) = vault::Vault::create(&data, &dir.path().join("keys"), vault::Kdf::FAST_FOR_TESTS).unwrap();
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
            created_ms: T0 - 200 * DAY,
            inputs: Default::default(),
        })
        .unwrap()
        .id;
    let engine = {
        let _in = rt.enter();
        let store = ingest::Store::new(data.join("can"), v);
        let engine = Engine::new(db.clone(), store, Options { workers: 1, ..Options::default() });
        engine.start().unwrap();
        engine
    };
    Kit { engine, db, car, _dir: dir, _rt: rt }
}

fn j<T: serde::Serialize>(v: &T) -> Value {
    serde_json::to_value(v).unwrap()
}

/// An imported row's id (`n`: its number in the import), or a live row's from its start.
fn id_of(k: &Kit, source: Source, n: i64, start: u64) -> u64 {
    match source {
        Source::Live => timeline::drive_id(k.car, start),
        _ => engine::importer::imported_id_of(source, k.car, n),
    }
}

fn at(t_ms: u64, lat: f64) -> Position {
    Position { t_ms, lat, lon: -140.0, ..Position::default() }
}

fn drive(k: &Kit, source: Source, n: i64, start: u64, end: u64, odo: f64, km: f64) -> model::Drive {
    model::Drive {
        id: id_of(k, source, n, start),
        car: k.car,
        start_ms: start,
        end_ms: end,
        start: Some(at(start, -30.0)),
        end: Some(at(end, -30.1)),
        start_place: None,
        end_place: None,
        start_address: None,
        end_address: None,
        start_odometer_km: Some(odo),
        end_odometer_km: Some(odo + km),
        distance_km: Some(km),
        speed_max_kmh: None,
        power_max_kw: None,
        power_min_kw: None,
        energy_used_kwh: Some(km * 0.15),
        start_soc: None,
        end_soc: None,
        start_range_km: None,
        end_range_km: None,
        outside_temp_avg_c: None,
        inside_temp_avg_c: None,
        ascent_m: None,
        descent_m: None,
        source,
        inputs: model::Inputs::NONE,
        rules_version: timeline::RULES_VERSION,
    }
}

fn charge(k: &Kit, source: Source, n: i64, start: u64, end: u64, kwh: f64) -> model::Charge {
    model::Charge {
        id: id_of(k, source, n, start),
        car: k.car,
        start_ms: start,
        end_ms: end,
        position: Some(at(start, -30.0)),
        place: None,
        address: None,
        kind: Some(ChargeKind::Ac),
        energy_added_kwh: Some(kwh),
        energy_used_kwh: None,
        energy_spans: None,
        start_soc: Some(40.0),
        end_soc: Some(80.0),
        start_range_km: None,
        end_range_km: None,
        power_max_kw: None,
        outside_temp_avg_c: None,
        battery_temp_avg_c: None,
        cost: None,
        source,
        inputs: model::Inputs::NONE,
        rules_version: timeline::RULES_VERSION,
    }
}

fn state(k: &Kit, source: Source, n: i64, a: u64, b: u64) -> db::StateRow {
    db::StateRow {
        id: id_of(k, source, n, a),
        span: StateSpan {
            car: k.car,
            state: State::Online,
            start_ms: a,
            end_ms: b,
            buses: Vec::new(),
            source,
            inputs: model::Inputs::NONE,
        },
    }
}

fn update(k: &Kit, source: Source, n: i64, a: u64, b: u64, version: &str) -> db::UpdateRow {
    db::UpdateRow {
        id: id_of(k, source, n, a),
        update: model::Update {
            car: k.car,
            start_ms: a,
            end_ms: Some(b),
            version: version.into(),
            source,
            inputs: model::Inputs::NONE,
        },
    }
}

/// The day's statistics row: `(drives, distance logged, charges, energy added)`.
fn day(k: &Kit, d0: u64) -> (Value, Value, Value, Value) {
    let p = Period::new(Some(d0), Some(d0 + DAY));
    let s = j(&query::reports::car_statistics(&k.engine, k.car, "day", &p, &Paging::default()).unwrap());
    let it = &s["items"][0];
    (it["drives"].clone(), it["distance_logged_km"].clone(), it["charges"].clone(), it["energy_added_kwh"].clone())
}

fn overlaps(k: &Kit) -> Vec<Value> {
    j(&query::system::overlaps(&k.engine, k.car, &Paging::default()).unwrap())["items"].as_array().unwrap().clone()
}

/// Five imported days, a drive each, then the day live logging begins (`d0`).
fn imported_days(k: &Kit) -> (Vec<model::Drive>, u64) {
    let tm = Source::Teslamate;
    let rows = (0..5u64)
        .map(|d| {
            let t = T0 - (5 - d) * DAY + 8 * H;
            drive(k, tm, d as i64 + 1, t, t + H, 1000.0 + d as f64 * 60.0, 60.0)
        })
        .collect();
    (rows, T0 + DAY)
}

#[test]
fn a_drive_going_on_when_live_logging_began_counts_once() {
    let k = kit();
    let tm = Source::Teslamate;
    let (mut rows, d0) = imported_days(&k);
    // the import has the whole drive, 08:00 to 09:00 (60 km); live logging began at 08:30 and has its second half
    rows.push(drive(&k, tm, 100, d0 + 8 * H, d0 + 9 * H, 2000.0, 60.0));
    rows.push(drive(&k, Source::Live, 0, d0 + 8 * H + 30 * MIN, d0 + 9 * H, 2030.0, 30.0));
    k.db.upsert(k.car, &rows).unwrap();
    k.db.upsert(
        k.car,
        &[state(&k, tm, 1, T0 - 5 * DAY, d0 + 10 * H), state(&k, Source::Live, 0, d0 + 8 * H + 30 * MIN, d0 + 12 * H)],
    )
    .unwrap();
    // one drive that hour, the live one (live wins until the owner says otherwise)
    assert_eq!(day(&k, d0), (json!(1), json!(30.0), json!(0), json!(0.0)));
    let t = j(&query::reports::totals(&k.engine, k.car, None).unwrap());
    assert_eq!((t["drives"].clone(), t["distance_km"].clone()), (json!(6), json!(330.0)));
    // and the owner can settle it: the time the imported drive covers is an overlap of its own
    let o = overlaps(&k);
    assert_eq!(o.len(), 1, "{o:?}");
    assert_eq!(
        (o[0]["kind"].clone(), o[0]["start_ms"].clone(), o[0]["end_ms"].clone()),
        (json!("live"), json!(d0 + 8 * H), json!(d0 + 9 * H))
    );
    assert_eq!((o[0]["live"]["drives"].clone(), o[0]["imported"]["drives"].clone()), (json!(1), json!(1)));
    assert_eq!(o[0]["shown_source"], "live");
    // flipped, the import's whole drive is the one shown
    let id = o[0]["id"].as_str().unwrap();
    query::system::set_overlap(&k.engine, k.car, id, "import:teslamate", None).unwrap();
    assert_eq!(day(&k, d0), (json!(1), json!(60.0), json!(0), json!(0.0)));
    let t = j(&query::reports::totals(&k.engine, k.car, None).unwrap());
    assert_eq!((t["drives"].clone(), t["distance_km"].clone()), (json!(6), json!(360.0)));
}

#[test]
fn a_charge_going_on_when_live_logging_began_counts_once() {
    let k = kit();
    let tm = Source::Teslamate;
    let (rows, d0) = imported_days(&k);
    k.db.upsert(k.car, &rows).unwrap();
    // charging overnight from 01:00 to 07:00 (40 kWh); the live input was set up at 05:00
    k.db.upsert(
        k.car,
        &[
            charge(&k, tm, 100, d0 + H, d0 + 7 * H, 40.0),
            charge(&k, Source::Live, 0, d0 + 5 * H, d0 + 7 * H + 2 * MIN, 13.0),
        ],
    )
    .unwrap();
    k.db.upsert(
        k.car,
        &[state(&k, tm, 1, T0 - 5 * DAY, d0 + 8 * H), state(&k, Source::Live, 0, d0 + 5 * H, d0 + 12 * H)],
    )
    .unwrap();
    assert_eq!(day(&k, d0), (json!(0), json!(0.0), json!(1), json!(13.0)));
    let o = overlaps(&k);
    assert_eq!(o.len(), 1, "{o:?}");
    assert_eq!((o[0]["live"]["charges"].clone(), o[0]["imported"]["charges"].clone()), (json!(1), json!(1)));
    query::system::set_overlap(&k.engine, k.car, o[0]["id"].as_str().unwrap(), "import:teslamate", None).unwrap();
    assert_eq!(day(&k, d0), (json!(0), json!(0.0), json!(1), json!(40.0)));
}

#[test]
fn an_imported_drive_that_ran_into_live_logging_with_no_live_drive_stays() {
    let k = kit();
    let tm = Source::Teslamate;
    let (mut rows, d0) = imported_days(&k);
    // live logging began two minutes before the drive ended and made no drive of what was left; a live drive later
    rows.push(drive(&k, tm, 100, d0 + 8 * H, d0 + 9 * H, 2000.0, 60.0));
    rows.push(drive(&k, Source::Live, 0, d0 + 10 * H, d0 + 11 * H, 2060.0, 40.0));
    k.db.upsert(k.car, &rows).unwrap();
    k.db.upsert(
        k.car,
        &[
            state(&k, tm, 1, T0 - 5 * DAY, d0 + 9 * H + 30 * MIN),
            state(&k, Source::Live, 0, d0 + 8 * H + 58 * MIN, d0 + 12 * H),
        ],
    )
    .unwrap();
    assert_eq!(day(&k, d0), (json!(2), json!(100.0), json!(0), json!(0.0)));
    assert!(overlaps(&k).is_empty());
}

#[test]
fn an_update_both_imports_have_is_one_update() {
    let k = kit();
    let (tm, tf) = (Source::Teslamate, Source::Teslafi);
    let (mut ups, mut rows, mut states) = (Vec::new(), Vec::new(), Vec::new());
    for i in 0..3u64 {
        let t = T0 - 150 * DAY + i * 30 * DAY;
        let ver = format!("2025.{}.1", 20 + i * 4);
        ups.push(update(&k, tm, i as i64 + 1, t, t + 25 * MIN, &ver));
        // TeslaFi sees the new version at its next poll, a few minutes on
        ups.push(update(&k, tf, i as i64 + 1, t + 3 * MIN, t + 27 * MIN, &ver));
    }
    for d in 0..120u64 {
        let t = T0 - 160 * DAY + d * DAY + 8 * H;
        rows.push(drive(&k, tm, d as i64 + 1, t, t + H, 1000.0 + d as f64 * 60.0, 60.0));
        rows.push(drive(&k, tf, d as i64 + 1, t + MIN, t + H, 1000.0 + d as f64 * 60.0, 60.0));
        states.push(state(&k, tm, d as i64 + 1, t - 8 * H, t + 16 * H));
        states.push(state(&k, tf, d as i64 + 1, t - 8 * H, t + 16 * H));
    }
    k.db.upsert(k.car, &ups).unwrap();
    k.db.upsert(k.car, &rows).unwrap();
    k.db.upsert(k.car, &states).unwrap();
    let totals = |source: Option<&str>| j(&query::reports::totals(&k.engine, k.car, source).unwrap());
    assert_eq!((totals(None)["drives"].clone(), totals(None)["updates"].clone()), (json!(120), json!(3)));
    assert_eq!(totals(Some("import"))["updates"], 3);
    // one site alone shows its own, whatever was chosen
    assert_eq!(totals(Some("import:teslamate"))["updates"], 3);
    assert_eq!(totals(Some("import:teslafi"))["updates"], 3);
    assert_eq!(totals(Some("smo"))["updates"], 0);
    let u = j(&query::software::updates(&k.engine, k.car, &Period::all(), &Paging::default()).unwrap());
    assert_eq!(u["summary"]["count"], 3, "{}", u["summary"]);
    // a month between installs, less the install itself
    assert_eq!(u["summary"]["median_gap_ms"], json!(30 * DAY - 25 * MIN));
    let items = u["items"].as_array().unwrap();
    assert_eq!(items.len(), 3);
    assert!(items.iter().all(|i| i["detected_by"] == "teslamate"), "the site its span shows: {items:?}");
    assert_eq!(items[0]["since_previous_ms"], json!(30 * DAY - 25 * MIN));
    assert_eq!(u["summary"]["last_update"]["id"], items[0]["id"]);

    // where one import has a hole (no span both cover), the two rows of one version a few minutes apart are one
    let k = kit();
    let t = T0 - 50 * DAY;
    k.db.upsert(
        k.car,
        &[update(&k, tm, 1, t, t + 25 * MIN, "2025.20.1"), update(&k, tf, 1, t + 3 * MIN, t + 27 * MIN, "2025.20.1")],
    )
    .unwrap();
    // (and the same version months later is another install)
    k.db.upsert(k.car, &[update(&k, tf, 2, t + 40 * DAY, t + 40 * DAY + 20 * MIN, "2025.20.1")]).unwrap();
    let u = j(&query::software::updates(&k.engine, k.car, &Period::all(), &Paging::default()).unwrap());
    assert_eq!(u["summary"]["count"], 2, "{}", u["items"]);
    assert_eq!(j(&query::reports::totals(&k.engine, k.car, None).unwrap())["updates"], 2);
}
