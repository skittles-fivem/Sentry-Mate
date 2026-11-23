#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! A car's data sources through the real router (web::testkit): `GET /cars/{car}/settings` shows the order its live
//! inputs win in (the default until an admin picks one), each input with whether it is set up, the car's imported
//! history and the server's gap filling default; `PATCH` takes a new order (every input once, any order) or null
//! (the default again), refuses anything else with `fields.source_order`, keeps it across a restart and rebuilds the
//! car; the car's fused values follow the order at once (SMO and the Tesla API over the same minutes, value by value).
//! Made-up data only.

use engine::Engine;
use model::{Car, CarId, Sample, Source, Stream};
use serde_json::{Value, json};
use std::sync::Arc;
use web::testkit::{Client, TestServer};

struct Kit {
    s: TestServer,
    car: String,
    id: CarId,
    engine: Engine,
    db: db::Db,
    data: std::path::PathBuf,
    v: Arc<vault::Vault>,
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
const AT: u64 = DAY0 + 3 * H;

/// SMO each second for ten minutes: 50 km/h, the odometer (the API sends none here).
fn smo_samples() -> Vec<Sample> {
    (0..10 * 60u64)
        .map(|i| Sample {
            t_ms: AT + i * 1000,
            speed_kmh: Some(50.0),
            odometer_km: Some(1_000.0),
            ..Default::default()
        })
        .collect()
}

/// The Tesla API every 10 s over the same minutes (from their first second to just past their last): 60 km/h, the
/// cabin.
fn api_samples() -> Vec<Sample> {
    (0..=60u64)
        .map(|i| Sample {
            t_ms: AT + i * 10_000,
            speed_kmh: Some(60.0),
            inside_temp_c: Some(22.0),
            ..Default::default()
        })
        .collect()
}

/// A TeslaMate import a day before.
fn import_samples() -> Vec<Sample> {
    (0..30u64).map(|i| Sample { t_ms: DAY0 - H + i * 60_000, speed_kmh: Some(30.0), ..Default::default() }).collect()
}

async fn kit() -> Kit {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let (v, _) = vault::Vault::create(&data, &dir.path().join("keys"), vault::Kdf::FAST_FOR_TESTS).unwrap();
    let v = Arc::new(v);
    let db = db::Db::open(&data, v.clone(), db::Options::default()).unwrap();
    let id = db
        .create_car(Car {
            id: 0,
            name: "Sources".into(),
            vehicle_keys: Vec::new(),
            preferred_key: None,
            vin: None,
            profile_setting: None,
            time_zone: "UTC".into(),
            time_zone_typed: Some(true),
            efficiency_wh_km: None,
            created_ms: 1_790_000_000_000,
            inputs: model::Inputs::BOTH,
        })
        .unwrap()
        .id;
    db.write_points(id, Stream::Smo, AT, AT + 10 * MIN, &smo_samples()).unwrap();
    db.write_points(id, Stream::TeslaApi, AT, AT + 10 * MIN + 1, &api_samples()).unwrap();
    db.write_points(id, Source::Teslamate, DAY0 - H, DAY0, &import_samples()).unwrap();
    let store = ingest::Store::new(data.join("can"), v.clone());
    let engine = Engine::new(db.clone(), store, engine::Options::default());
    engine.start().unwrap();
    let (e2, d2) = (engine.clone(), db.clone());
    let s = TestServer::start_with(move |b| b.cars(Arc::new(EngineCars(d2))).service(Arc::new(e2))).await;
    Kit { s, car: format!("c{id}"), id, engine, db, data, v, _dir: dir }
}

async fn ok(c: &mut Client, path: &str) -> Value {
    let r = c.get(path).await;
    assert_eq!(r.status, 200, "{path}: {}", r.text());
    r.json()
}

const DEFAULT: [&str; 4] = ["smo", "tesla_api", "teslafi_api", "teslamate_local"];

/// The mean speed the series gives over the ten minutes.
async fn speeds(c: &mut Client, car: &str) -> Vec<f64> {
    let s = ok(
        c,
        &format!("/api/v1/cars/{car}/series?fields=speed_kph&from_ms={AT}&to_ms={}&max_points=10", AT + 10 * MIN),
    )
    .await;
    s["values"]["speed_kph"].as_array().unwrap().iter().filter_map(Value::as_f64).collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_default_order_its_inputs_and_imports() {
    let k = kit().await;
    let mut admin = k.s.admin().await;
    let cs = ok(&mut admin, &format!("/api/v1/cars/{}/settings", k.car)).await;
    assert_eq!(cs["source_order"], json!(DEFAULT), "{cs}");
    assert_eq!(cs["source_order_default"], json!(DEFAULT), "{cs}");
    assert_eq!(cs["import_gap_fill_default"], json!(true), "{cs}");
    let inputs = cs["inputs"].as_array().unwrap();
    let kinds: Vec<&str> = inputs.iter().map(|i| i["kind"].as_str().unwrap()).collect();
    assert_eq!(kinds, DEFAULT, "{cs}");
    let labels: Vec<&str> = inputs.iter().map(|i| i["label"].as_str().unwrap()).collect();
    assert_eq!(labels, ["SMO", "Tesla API", "TeslaFi", "local TeslaMate"], "{cs}");
    let on: Vec<bool> = inputs.iter().map(|i| i["on"].as_bool().unwrap()).collect();
    assert_eq!(on, [true, true, false, false], "the car has had SMO's and the Tesla API's data: {cs}");
    assert!(inputs.iter().all(|i| i["last_ms"].is_null() || i["last_ms"].is_u64()), "{cs}");
    assert_eq!(cs["imports"].as_array().unwrap().len(), 1, "{cs}");
    let imp = &cs["imports"][0];
    assert_eq!((imp["site"].as_str(), imp["label"].as_str()), (Some("teslamate"), Some("Imported from TeslaMate")));
    assert_eq!((imp["from_ms"].as_u64(), imp["to_ms"].as_u64()), (Some(DAY0 - H), Some(DAY0 - H + 29 * MIN)), "{cs}");
    // SMO wins by default
    let v = speeds(&mut admin, &k.car).await;
    assert!(!v.is_empty() && v.iter().all(|x| (x - 50.0).abs() < 1e-6), "{v:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_new_order_round_trips_rebuilds_and_wins() {
    let k = kit().await;
    let mut admin = k.s.admin().await;
    let path = format!("/api/v1/cars/{}/settings", k.car);
    k.engine.idle().await;
    let jobs_before = k.engine.jobs().iter().filter(|j| j.car == Some(k.id)).count();
    let cs = ok(&mut admin, &path).await;
    let order = json!(["tesla_api", "smo", "teslamate_local", "teslafi_api"]);
    let r = admin.patch(&path, json!({ "version": cs["version"], "source_order": order })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let p = r.json();
    assert_eq!(p["source_order"], order, "{p}");
    assert_eq!(p["source_order_default"], json!(DEFAULT), "{p}");
    assert_eq!(p["version"].as_u64(), cs["version"].as_u64().map(|v| v + 1), "{p}");
    let kinds: Vec<&str> = p["inputs"].as_array().unwrap().iter().map(|i| i["kind"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["tesla_api", "smo", "teslamate_local", "teslafi_api"], "inputs in the car's order: {p}");
    // the car's rebuild was started (pending until it is done)
    let jobs = k.engine.jobs();
    assert_eq!(jobs.iter().filter(|j| j.car == Some(k.id)).count(), jobs_before + 1, "{jobs:?}");
    k.engine.idle().await;
    assert!(!k.engine.rebuild_pending(k.id));
    assert_eq!(ok(&mut admin, &path).await["source_order"], order);
    // (a rebuild makes SMO's samples again from its logs: this car has none, its made-up samples are written again)
    k.db.write_points(k.id, Stream::Smo, AT, AT + 10 * MIN, &smo_samples()).unwrap();
    // the Tesla API's values win where both cover the same time
    let v = speeds(&mut admin, &k.car).await;
    assert!(!v.is_empty() && v.iter().all(|x| (x - 60.0).abs() < 1e-6), "{v:?}");

    // the same order again changes nothing to rebuild
    let cs = ok(&mut admin, &path).await;
    let r = admin.patch(&path, json!({ "version": cs["version"], "source_order": order })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(k.engine.jobs().iter().filter(|j| j.car == Some(k.id)).count(), jobs_before + 1);

    // kept in the db, not in memory: an engine opened over it afresh reads it
    let store = ingest::Store::new(k.data.join("can-2"), k.v.clone());
    let fresh = Engine::new(k.db.clone(), store, engine::Options::default());
    assert_eq!(fresh.source_order(k.id).words(), ["tesla_api", "smo", "teslamate_local", "teslafi_api"]);
    drop(fresh);

    // null: the default again
    let cs = ok(&mut admin, &path).await;
    let r = admin.patch(&path, json!({ "version": cs["version"], "source_order": null })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.json()["source_order"], json!(DEFAULT));
    k.engine.idle().await;
    assert_eq!(ok(&mut admin, &path).await["source_order"], json!(DEFAULT));
    k.db.write_points(k.id, Stream::Smo, AT, AT + 10 * MIN, &smo_samples()).unwrap();
    let v = speeds(&mut admin, &k.car).await;
    assert!(!v.is_empty() && v.iter().all(|x| (x - 50.0).abs() < 1e-6), "{v:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_order_that_is_not_every_input_once_is_refused() {
    let k = kit().await;
    let mut admin = k.s.admin().await;
    let path = format!("/api/v1/cars/{}/settings", k.car);
    let cs = ok(&mut admin, &path).await;
    for bad in [
        json!(["smo", "smo", "teslafi_api", "teslamate_local"]),
        json!(["smo", "tesla_api", "teslafi_api"]),
        json!(["smo", "tesla_api", "teslafi_api", "teslamate_local", "smo"]),
        json!(["smo", "tesla_api", "teslafi_api", "fleet_api"]),
        json!([]),
        json!("smo"),
        json!(3),
        json!([1, 2, 3, 4]),
        json!({ "smo": 1 }),
    ] {
        let r = admin.patch(&path, json!({ "version": cs["version"], "source_order": bad })).await;
        assert_eq!(r.status, 422, "{bad}: {}", r.text());
        let e = r.json();
        assert_eq!(e["error"]["code"], "invalid", "{bad}: {e}");
        let why = e["error"]["fields"]["source_order"].as_str().unwrap_or_default();
        assert!(why.contains("teslamate_local"), "{bad}: a plain sentence naming the words: {e}");
    }
    let after = ok(&mut admin, &path).await;
    assert_eq!(after["version"], cs["version"], "nothing changed");
    assert_eq!(after["source_order"], json!(DEFAULT));
}
