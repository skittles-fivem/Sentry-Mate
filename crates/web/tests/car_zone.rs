#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! A car's time zone through the real router (web::testkit): one an admin set, else the zone of the car's newest
//! position (looked up offline), else the server's default. Made-up cars at made-up points in public places.

use engine::Engine;
use model::{Car, CarId, Now, Sample, State};
use serde_json::json;
use std::sync::Arc;
use web::testkit::TestServer;

fn made_up_car(name: &str, time_zone: &str, typed: Option<bool>) -> Car {
    Car {
        id: 0,
        name: name.into(),
        vehicle_keys: Vec::new(),
        preferred_key: None,
        vin: None,
        profile_setting: None,
        time_zone: time_zone.into(),
        time_zone_typed: typed,
        efficiency_wh_km: None,
        created_ms: 1_790_000_000_000,
        inputs: Default::default(),
    }
}

fn parked_at(car: CarId, at_ms: u64, lat: f64, lon: f64) -> Now {
    let sample = Sample { t_ms: at_ms, lat: Some(lat), lon: Some(lon), soc: Some(60.0), ..Sample::default() };
    Now {
        range_estimated_km: None,
        healthy: None,
        car,
        at_ms,
        state: Some(State::Online),
        driving: false,
        sample,
        place: None,
        since_ms: None,
        trim: None,
        off_can: Box::default(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_car_without_a_zone_set_follows_its_position_then_the_servers_default() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let (v, _) = vault::Vault::create(&data, &dir.path().join("keys"), vault::Kdf::FAST_FOR_TESTS).unwrap();
    let v = Arc::new(v);
    let db = db::Db::open(&data, v.clone(), db::Options::default()).unwrap();
    let now = (accounts::system_clock())();
    // an older record: UTC, never set by anyone, parked in central Chicago
    let a = db.create_car(made_up_car("Older", "UTC", None)).unwrap();
    // a new car with no position yet
    let b = db.create_car(made_up_car("New", "UTC", Some(false))).unwrap();
    db.set_now(parked_at(a.id, now - 60_000, 41.8800, -87.6300)).unwrap();
    let store = ingest::Store::new(data.join("can"), v);
    let engine = Engine::new(db, store, engine::Options::default());
    assert!(engine.set_default_time_zone("Asia/Tokyo"));
    let ids: std::collections::HashSet<CarId> = [a.id, b.id].into_iter().collect();
    let e2 = engine.clone();
    let s = TestServer::start_with(move |x| x.cars(Arc::new(web::FixedCars(ids))).service(Arc::new(e2))).await;
    let mut admin = s.admin().await;
    let zone_of = |v: &serde_json::Value| (v["time_zone"].clone(), v["time_zone_source"].clone());

    let car = admin.get("/api/v1/cars/c1").await.json();
    assert_eq!(zone_of(&car), (json!("America/Chicago"), json!("position")), "{car}");
    let list = admin.get("/api/v1/cars").await.json();
    let new = list["items"].as_array().unwrap().iter().find(|x| x["id"] == "c2").unwrap().clone();
    assert_eq!(zone_of(&new), (json!("Asia/Tokyo"), json!("default")), "{list}");
    // car-scoped answers follow the same zone
    let n = admin.get("/api/v1/cars/c1/now").await;
    assert_eq!(n.status, 200, "{}", n.text());
    assert_eq!(n.json()["time_zone"], json!("America/Chicago"));

    // the owner's zone wins, and the car keeps it
    let r = admin.patch("/api/v1/cars/c1", json!({ "version": 1, "time_zone": "Europe/Berlin" })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(zone_of(&r.json()), (json!("Europe/Berlin"), json!("owner")));
    assert_eq!(admin.get("/api/v1/cars/c1/now").await.json()["time_zone"], json!("Europe/Berlin"));
    // a car moved to Berlin and set to UTC by its owner stays UTC
    let r = admin.patch("/api/v1/cars/c1", json!({ "version": 2, "time_zone": "UTC" })).await;
    assert_eq!(zone_of(&r.json()), (json!("UTC"), json!("owner")));

    // back to automatic: the position's zone again
    let r = admin.patch("/api/v1/cars/c1", json!({ "version": 3, "time_zone": null })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(zone_of(&r.json()), (json!("America/Chicago"), json!("position")));
    // the car drives to Berlin: its zone follows
    engine.db().set_now(parked_at(a.id, now - 1_000, 52.5200, 13.4050)).unwrap();
    assert_eq!(zone_of(&admin.get("/api/v1/cars/c1").await.json()), (json!("Europe/Berlin"), json!("position")));
    let r = admin.patch("/api/v1/cars/c2", json!({ "version": 1, "time_zone": "auto" })).await;
    assert_eq!(zone_of(&r.json()), (json!("Asia/Tokyo"), json!("default")));
    let r = admin.patch("/api/v1/cars/c2", json!({ "version": 2, "time_zone": 7 })).await;
    assert_eq!(r.status, 400, "{}", r.text());
}
