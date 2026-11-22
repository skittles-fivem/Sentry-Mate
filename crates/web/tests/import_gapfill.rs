#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! Import gap filling through the real router (web::testkit): a live drive (SMO) that lacks the outside temperature
//! and the elevation, over the same time as a TeslaMate import and a TeslaFi import that have them. The chosen import
//! fills what live lacks (series, the drive's row and detail, its track, statistics), labelled as imported; a live
//! value is never overridden; only the import chosen for the span fills; drives are never doubled and totals never
//! change; the car's or the server's switch turns it off; a live span flipped to the imported side still shows the
//! import's own rows.

use engine::Engine;
use model::{Car, CarId, Sample, Source, State, StateSpan};
use serde_json::{Value, json};
use std::sync::Arc;
use web::testkit::{Client, TestServer};

struct Kit {
    s: TestServer,
    car: String,
    _engine: Engine,
    _dir: tempfile::TempDir,
}

struct EngineCars(db::Db);

impl web::CarDirectory for EngineCars {
    fn exists(&self, car: CarId) -> bool {
        self.0.car(car).ok().flatten().is_some()
    }
}

/// A made-up day (UTC midnight) long ago.
const DAY0: u64 = 1_780_000_000_000 / 86_400_000 * 86_400_000;
const H: u64 = 3_600_000;
const MIN: u64 = 60_000;
/// The live drive: 20 minutes from here.
const LIVE_AT: u64 = DAY0 + 3 * H + 10_000;

fn drive(car: CarId, source: Source, id: u64, start_ms: u64, odo: f64) -> model::Drive {
    model::Drive {
        id,
        car,
        start_ms,
        end_ms: start_ms + 20 * MIN,
        start: None,
        end: None,
        start_place: None,
        end_place: None,
        start_address: None,
        end_address: None,
        start_odometer_km: Some(odo),
        end_odometer_km: Some(odo + 10.0),
        distance_km: Some(10.0),
        speed_max_kmh: None,
        power_max_kw: None,
        power_min_kw: None,
        energy_used_kwh: Some(1.6),
        start_soc: Some(80.0),
        end_soc: Some(78.0),
        start_range_km: Some(400.0),
        end_range_km: Some(390.0),
        outside_temp_avg_c: None,
        inside_temp_avg_c: Some(21.0),
        ascent_m: None,
        descent_m: None,
        source,
        inputs: model::Inputs::NONE,
        rules_version: 0,
    }
}

fn state(car: CarId, source: Source, id: u64, a: u64, b: u64) -> db::StateRow {
    db::StateRow {
        id,
        span: StateSpan {
            car,
            state: State::Online,
            start_ms: a,
            end_ms: b,
            buses: Vec::new(),
            source,
            inputs: model::Inputs::NONE,
        },
    }
}

/// SMO: speed and the cabin each second of the drive, a position, no outside temperature, no elevation.
fn live_samples() -> Vec<Sample> {
    (0..20 * 60u64)
        .map(|i| Sample {
            t_ms: LIVE_AT + i * 1000,
            speed_kmh: Some(50.0),
            inside_temp_c: Some(21.0),
            lat: Some(37.0 + i as f64 * 1e-5),
            lon: Some(-122.0),
            ..Default::default()
        })
        .collect()
}

/// An import's positions every 10 s over the drive (a minute either side): its own speed, outside and cabin
/// temperature, and a climb of 1 m a point.
fn import_samples(kmh: f64, outside: f64) -> Vec<Sample> {
    (0..(22 * 6u64))
        .map(|i| Sample {
            t_ms: LIVE_AT - MIN + i * 10_000,
            speed_kmh: Some(kmh),
            outside_temp_c: Some(outside),
            inside_temp_c: Some(25.0),
            elevation_m: Some(100.0 + i as f64),
            lat: Some(37.0),
            lon: Some(-122.0),
            ..Default::default()
        })
        .collect()
}

async fn kit() -> Kit {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let (v, _) = vault::Vault::create(&data, &dir.path().join("keys"), vault::Kdf::FAST_FOR_TESTS).unwrap();
    let v = Arc::new(v);
    let db = db::Db::open(&data, v.clone(), db::Options::default()).unwrap();
    let car = db
        .create_car(Car {
            id: 0,
            name: "Filled".into(),
            vehicle_keys: Vec::new(),
            preferred_key: None,
            vin: None,
            profile_setting: None,
            time_zone: "UTC".into(),
            efficiency_wh_km: None,
            created_ms: 1_790_000_000_000,
            inputs: Default::default(),
        })
        .unwrap()
        .id;
    let (tm, tf) = (Source::Teslamate, Source::Teslafi);
    let id = |s: Source, n: i64| engine::importer::imported_id_of(s, car, n);
    // live: a state span over the drive, and the drive
    db.upsert(car, &[state(car, Source::Live, 500, DAY0 + 2 * H + H / 2, DAY0 + 4 * H)]).unwrap();
    db.upsert(car, &[drive(car, Source::Live, 77, LIVE_AT, 1_000.0)]).unwrap();
    // TeslaMate: the same drive (hidden inside live history) and one of its own later; TeslaFi: the same drive
    // (TeslaMate has more drives in the span both cover: it is the import shown)
    db.upsert(
        car,
        &[
            drive(car, tm, id(tm, 1), LIVE_AT - 20_000, 1_000.0),
            drive(car, tm, id(tm, 2), DAY0 + 6 * H, 1_010.0),
            drive(car, tf, id(tf, 1), LIVE_AT + 20_000, 1_000.0),
        ],
    )
    .unwrap();
    db.upsert(
        car,
        &[state(car, tm, id(tm, 1), DAY0, DAY0 + 12 * H), state(car, tf, id(tf, 1), DAY0 + 30_000, DAY0 + 12 * H)],
    )
    .unwrap();
    let ls = live_samples();
    db.write_points(car, Source::Live, LIVE_AT, LIVE_AT + 20 * MIN, &ls).unwrap();
    let (a, b) = (LIVE_AT - MIN, LIVE_AT + 21 * MIN + 1);
    db.write_points(car, tm, a, b, &import_samples(40.0, 20.0)).unwrap();
    db.write_points(car, tf, a, b, &import_samples(45.0, 30.0)).unwrap();
    let store = ingest::Store::new(data.join("can"), v);
    let engine = Engine::new(db.clone(), store, engine::Options::default());
    engine.start().unwrap();
    let (e2, d2) = (engine.clone(), db.clone());
    let s = TestServer::start_with(move |b| b.cars(Arc::new(EngineCars(d2))).service(Arc::new(e2))).await;
    Kit { s, car: format!("c{car}"), _engine: engine, _dir: dir }
}

async fn ok(c: &mut Client, path: &str) -> Value {
    let r = c.get(path).await;
    assert_eq!(r.status, 200, "{path}: {}", r.text());
    r.json()
}

fn nums(v: &Value, field: &str) -> Vec<f64> {
    v["values"][field].as_array().unwrap().iter().filter_map(Value::as_f64).collect()
}

fn kinds(v: &Value) -> Vec<String> {
    v.as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .map(|r| {
            format!(
                "{}{}",
                r["kind"].as_str().unwrap(),
                r["site"].as_str().map(|s| format!(":{s}")).unwrap_or_default()
            )
        })
        .collect()
}

/// The live drive's row in the list, and the list's drive count.
async fn live_row(c: &mut Client, car: &str) -> (Value, usize) {
    let v = ok(c, &format!("/api/v1/cars/{car}/drives")).await;
    let items = v["items"].as_array().unwrap();
    let row = items.iter().find(|d| d["id"] == "77").cloned().unwrap_or(Value::Null);
    (row, items.len())
}

async fn totals(c: &mut Client, car: &str) -> (u64, f64, f64) {
    let t = ok(c, &format!("/api/v1/cars/{car}/totals")).await;
    (t["drives"].as_u64().unwrap(), t["distance_km"].as_f64().unwrap(), t["energy_added_kwh"].as_f64().unwrap())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_chosen_import_fills_what_live_history_lacks() {
    let k = kit().await;
    let car = k.car.clone();
    let mut admin = k.s.admin().await;
    let series = format!(
        "/api/v1/cars/{car}/series?fields=outside_c,cabin_c,speed_kph,elevation_m&from_ms={}&to_ms={}&max_points=40",
        LIVE_AT + 30_000,
        LIVE_AT + 19 * MIN
    );

    // series: the gap in the outside temperature and the elevation filled from TeslaMate, labelled; the live speed
    // and cabin never overridden, and no import second beside a live one
    let s = ok(&mut admin, &series).await;
    let (outside, cabin, speed) = (nums(&s, "outside_c"), nums(&s, "cabin_c"), nums(&s, "speed_kph"));
    assert!(!outside.is_empty() && outside.iter().all(|v| (v - 20.0).abs() < 1e-6), "{s}");
    assert!(!cabin.is_empty() && cabin.iter().all(|v| (v - 21.0).abs() < 1e-6), "live wins: {s}");
    assert!(!speed.is_empty() && speed.iter().all(|v| (v - 50.0).abs() < 1e-6), "live wins: {s}");
    assert!(!nums(&s, "elevation_m").is_empty(), "{s}");
    assert_eq!(kinds(&s["value_sources"]["outside_c"]), ["import:teslamate"], "{s}");
    assert_eq!(kinds(&s["value_sources"]["speed_kph"]), ["smo"], "{s}");
    assert_eq!(kinds(&s["value_sources"]["cabin_c"]), ["smo"], "{s}");
    assert_eq!(kinds(&s["sources"]), ["smo", "import:teslamate"], "{s}");

    // the drive: one row per real drive, the live one with the import's outside temperature and climb, labelled
    assert_eq!(totals(&mut admin, &car).await, (2, 20.0, 0.0), "each drive once");
    let (row, n) = live_row(&mut admin, &car).await;
    assert_eq!(n, 2, "the live drive and TeslaMate's own later one, never a copy");
    assert_eq!(row["outside_avg_c"].as_f64(), Some(20.0), "{row}");
    assert_eq!(row["cabin_avg_c"].as_f64(), Some(21.0), "the row's own: {row}");
    assert!(row["climb_m"].as_f64().unwrap() > 100.0, "{row}");
    assert_eq!(row["energy_basis"], "measured", "{row}");
    assert_eq!(row["distance_km"].as_f64(), Some(10.0), "{row}");
    assert_eq!(row["source"], "can", "{row}");
    assert_eq!(kinds(&row["sources"]), ["smo", "import:teslamate"], "{row}");
    assert_eq!(row["source_label"], "Data from SMO; imported from TeslaMate", "{row}");
    let vs = row["value_sources"].as_object().unwrap();
    assert_eq!(kinds(&vs["outside_avg_c"]), ["import:teslamate"], "{row}");
    assert!(vs.contains_key("climb_m") && !vs.contains_key("cabin_avg_c") && !vs.contains_key("distance_km"), "{row}");
    let d = ok(&mut admin, &format!("/api/v1/cars/{car}/drives/77")).await;
    assert_eq!(d["outside_avg_c"].as_f64(), Some(20.0), "{d}");
    assert_eq!(d["value_sources"], row["value_sources"], "{d}");
    let t = ok(&mut admin, &format!("/api/v1/cars/{car}/drives/77/track")).await;
    assert_eq!(kinds(&t["value_sources"]["elevation_m"]), ["import:teslamate"], "{t}");
    // each point's values from the import: the elevation of the drive's own points, every value of a point the
    // import alone has (its last, a second after the live data stops)
    let imported = t["features"][0]["properties"]["imported"].as_array().unwrap();
    assert!(imported[..imported.len() - 1].iter().all(|p| p == &json!(["elevation_m"])), "{t}");
    let st = ok(&mut admin, &format!("/api/v1/cars/{car}/statistics?period=all")).await;
    assert_eq!(st["items"][0]["drives"].as_u64(), Some(2), "{st}");
    assert_eq!(st["items"][0]["outside_avg_c"].as_f64(), Some(20.0), "{st}");

    // the other import chosen for the span: TeslaFi's values fill instead, TeslaMate's never
    let o = ok(&mut admin, &format!("/api/v1/cars/{car}/overlaps")).await;
    let pair = o["items"].as_array().unwrap().iter().find(|i| i["kind"] == "imports").cloned().unwrap();
    assert_eq!(pair["shown_source"], "import:teslamate", "{o}");
    let body = json!({ "id": pair["id"], "shown": "import:teslafi", "version": pair["version"] });
    assert_eq!(admin.patch(&format!("/api/v1/cars/{car}/overlaps"), body).await.status, 200);
    let s = ok(&mut admin, &series).await;
    assert!(nums(&s, "outside_c").iter().all(|v| (v - 30.0).abs() < 1e-6), "{s}");
    assert_eq!(kinds(&s["value_sources"]["outside_c"]), ["import:teslafi"], "{s}");
    let (row, _) = live_row(&mut admin, &car).await;
    assert_eq!(row["outside_avg_c"].as_f64(), Some(30.0), "{row}");
    assert_eq!(row["source_label"], "Data from SMO; imported from TeslaFi", "{row}");
    // (TeslaFi's span: TeslaMate's later drive is not shown now, TeslaFi's copy of the live one is hidden by it)
    let before = totals(&mut admin, &car).await;
    assert_eq!(before.0, 1, "{before:?}");

    // the car's switch off: nothing filled, the live row as stored, the same totals
    let cs = ok(&mut admin, &format!("/api/v1/cars/{car}/settings")).await;
    assert_eq!((cs["import_gap_fill"].clone(), cs["import_gap_fill_on"].clone()), (Value::Null, json!(true)), "{cs}");
    let r = admin
        .patch(&format!("/api/v1/cars/{car}/settings"), json!({ "version": cs["version"], "import_gap_fill": false }))
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.json()["import_gap_fill_on"], json!(false));
    let s = ok(&mut admin, &series).await;
    assert!(nums(&s, "outside_c").is_empty(), "{s}");
    assert_eq!(kinds(&s["sources"]), ["smo"], "{s}");
    let (row, n) = live_row(&mut admin, &car).await;
    assert_eq!(n, 1);
    assert!(row["outside_avg_c"].is_null() && row["climb_m"].is_null(), "{row}");
    assert_eq!(row["value_sources"], json!({}), "{row}");
    assert_eq!(row["source_label"], "Data from SMO", "{row}");
    assert_eq!(totals(&mut admin, &car).await, before, "totals never change");
    let r = admin.patch(&format!("/api/v1/cars/{car}/settings"), json!({ "import_gap_fill": "yes" })).await;
    assert_eq!(r.status, 400, "{}", r.text());

    // back to the server's setting, and the server's switch off
    let cs = ok(&mut admin, &format!("/api/v1/cars/{car}/settings")).await;
    let r = admin
        .patch(&format!("/api/v1/cars/{car}/settings"), json!({ "version": cs["version"], "import_gap_fill": null }))
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(live_row(&mut admin, &car).await.0["outside_avg_c"].as_f64(), Some(30.0), "on again");
    let ss = ok(&mut admin, "/api/v1/settings").await;
    assert_eq!(ss["import_gap_fill"], json!({ "enabled": true }), "{ss}");
    let r = admin
        .patch("/api/v1/settings", json!({ "version": ss["version"], "import_gap_fill": { "enabled": false } }))
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert!(live_row(&mut admin, &car).await.0["outside_avg_c"].is_null(), "the server's switch off");
    assert!(nums(&ok(&mut admin, &series).await, "outside_c").is_empty());
    let ss = ok(&mut admin, "/api/v1/settings").await;
    let r = admin
        .patch("/api/v1/settings", json!({ "version": ss["version"], "import_gap_fill": { "enabled": true } }))
        .await;
    assert_eq!(r.status, 200, "{}", r.text());

    // asking for one source shows that source alone
    let s = ok(&mut admin, &format!("{series}&source=smo")).await;
    assert!(nums(&s, "outside_c").is_empty(), "{s}");
    let v = ok(&mut admin, &format!("/api/v1/cars/{car}/drives?source=smo")).await;
    assert!(v["items"][0]["outside_avg_c"].is_null(), "{v}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_live_span_flipped_to_the_imported_side_shows_the_import_alone() {
    let k = kit().await;
    let car = k.car.clone();
    let mut admin = k.s.admin().await;
    let before = totals(&mut admin, &car).await;
    let path = format!("/api/v1/cars/{car}/overlaps");
    let o = ok(&mut admin, &path).await;
    let live = o["items"].as_array().unwrap().iter().find(|i| i["kind"] == "live").cloned().unwrap();
    let body = json!({ "id": live["id"], "shown": "import:teslamate", "version": live["version"] });
    let r = admin.patch(&path, body).await;
    assert_eq!(r.status, 200, "{}", r.text());

    // TeslaMate's rows there, as imported (not filled, no live copy beside them)
    let v = ok(&mut admin, &format!("/api/v1/cars/{car}/drives")).await;
    let items = v["items"].as_array().unwrap();
    assert_eq!(items.len(), 2, "{v}");
    assert!(items.iter().all(|d| d["source"] == "teslamate" && d["value_sources"] == json!({})), "{v}");
    assert_eq!(totals(&mut admin, &car).await.0, before.0, "still one row per real drive");

    // the series there: the import's own seconds
    let series = format!(
        "/api/v1/cars/{car}/series?fields=speed_kph&from_ms={}&to_ms={}&max_points=40",
        LIVE_AT + 30_000,
        LIVE_AT + 19 * MIN
    );
    let s = ok(&mut admin, &series).await;
    let speed = nums(&s, "speed_kph");
    assert!(!speed.is_empty() && speed.iter().all(|v| (v - 40.0).abs() < 1e-6), "{s}");
    assert_eq!(kinds(&s["value_sources"]["speed_kph"]), ["import:teslamate"], "{s}");
}
