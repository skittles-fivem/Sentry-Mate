#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! Statistics over a made-up imported history whose first month carries another car's odometer (a sold car's rows,
//! 90,470 mi on the clock) before the car's own (53,100 km): the distance by odometer is the car's own, the
//! odometer chart, the totals and the compare series never go down, and the readings left out are counted. Then
//! the total cost: one charge typed, one at the car's default price per kWh (an estimate, shown apart), one with
//! neither; totals per currency, never added across, per 100 km and per day.

use engine::Engine;
use model::{Car, CarId, Source};
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

/// A made-up year's first day (UTC midnight).
const Y0: u64 = 1_767_225_600_000;
const H: u64 = 3_600_000;
const DAY: u64 = 24 * H;
const MI: f64 = 1.609_344;

fn drive(car: CarId, n: i64, start_ms: u64, odo: f64, km: f64) -> model::Drive {
    model::Drive {
        id: engine::importer::imported_id_of(Source::Teslamate, car, n),
        car,
        start_ms,
        end_ms: start_ms + H,
        start: None,
        end: None,
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
        source: Source::Teslamate,
        inputs: model::Inputs::NONE,
        rules_version: 0,
    }
}

fn charge(car: CarId, n: i64, start_ms: u64, added: f64, grid: Option<f64>) -> model::Charge {
    model::Charge {
        id: engine::importer::imported_id_of(Source::Teslamate, car, n),
        car,
        start_ms,
        end_ms: start_ms + H,
        position: None,
        place: None,
        address: None,
        kind: Some(model::ChargeKind::Ac),
        energy_added_kwh: Some(added),
        energy_used_kwh: grid,
        start_soc: None,
        end_soc: None,
        start_range_km: None,
        end_range_km: None,
        power_max_kw: None,
        outside_temp_avg_c: None,
        battery_temp_avg_c: None,
        cost: None,
        source: Source::Teslamate,
        inputs: model::Inputs::NONE,
        rules_version: 0,
    }
}

/// January: the sold car's drives at 145,600 km; February to the end of March: the car's own, 50 km a day.
/// Three charges in March.
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
            name: "Made-up Y".into(),
            vehicle_keys: Vec::new(),
            preferred_key: None,
            vin: None,
            profile_setting: None,
            time_zone: "UTC".into(),
            time_zone_typed: Some(true),
            efficiency_wh_km: None,
            created_ms: Y0,
            inputs: Default::default(),
        })
        .unwrap()
        .id;
    let mut drives = Vec::new();
    for d in 0..31u64 {
        drives.push(drive(car, d as i64 + 1, Y0 + d * DAY + 8 * H, 90_470.0 * MI + d as f64 * 40.0, 40.0));
    }
    for d in 0..59u64 {
        drives.push(drive(car, 100 + d as i64, Y0 + (31 + d) * DAY + 8 * H, 53_100.0 + d as f64 * 50.0, 50.0));
    }
    db.upsert(car, &drives).unwrap();
    let march = Y0 + 59 * DAY;
    db.upsert(
        car,
        &[
            charge(car, 1, march + 20 * H, 30.0, Some(33.0)),
            charge(car, 2, march + DAY + 20 * H, 20.0, None),
            charge(car, 3, march + 2 * DAY + 20 * H, 10.0, Some(11.0)),
        ],
    )
    .unwrap();
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

fn f(v: &Value) -> f64 {
    v.as_f64().unwrap_or_else(|| panic!("not a number: {v}"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn another_cars_odometer_is_left_out_and_the_total_cost_shows_its_estimate_apart() {
    let k = kit().await;
    let car = k.car.clone();
    let mut admin = k.s.admin().await;
    let period = format!("from_ms={Y0}&to_ms={}", Y0 + 90 * DAY);

    // the year's distance by odometer: the car's own (58 days of 50 km between its first and last reading)
    let st = ok(&mut admin, &format!("/api/v1/cars/{car}/statistics?period=all&{period}")).await;
    let row = &st["items"][0];
    assert!((f(&row["distance_odometer_km"]) - 59.0 * 50.0).abs() < 0.01, "{row}");
    assert!((f(&row["distance_logged_km"]) - (31.0 * 40.0 + 59.0 * 50.0)).abs() < 0.01, "{row}");
    assert_eq!(row["completeness"]["odometer_dropped"], 62, "both readings of each of January's drives");

    // by month: January moved nothing on this car's odometer, February and March their own drives
    let st = ok(&mut admin, &format!("/api/v1/cars/{car}/statistics?period=month&{period}")).await;
    let by: Vec<(String, f64)> = st["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| (x["label"].as_str().unwrap().to_string(), f(&x["distance_odometer_km"])))
        .collect();
    assert_eq!(by.iter().find(|x| x.0.ends_with("-01")).map(|x| x.1), Some(0.0), "{by:?}");
    assert!(by.iter().all(|x| x.1 >= 0.0 && x.1 <= 31.0 * 50.0 + 0.01), "{by:?}");

    // the odometer chart never goes down and starts at the car's own reading
    let m = ok(&mut admin, &format!("/api/v1/cars/{car}/mileage?group=month&{period}")).await;
    let ends: Vec<f64> = m["items"].as_array().unwrap().iter().filter_map(|x| x["odometer_end_km"].as_f64()).collect();
    assert!(ends.windows(2).all(|w| w[1] >= w[0]), "{ends:?}");
    assert!((ends[0] - 53_100.0).abs() < 0.1, "{ends:?}");
    let t = ok(&mut admin, &format!("/api/v1/cars/{car}/totals")).await;
    assert!((f(&t["odometer_first_km"]) - 53_100.0).abs() < 0.01, "{t}");
    assert!((f(&t["odometer_last_km"]) - (53_100.0 + 59.0 * 50.0)).abs() < 0.01, "{t}");
    let cmp = ok(&mut admin, &format!("/api/v1/compare?metric=odometer&cars={car}&{period}")).await;
    let vals: Vec<f64> = cmp["series"][0]["values"].as_array().unwrap().iter().map(f).collect();
    assert!(!vals.is_empty() && vals.iter().all(|v| *v < 60_000.0), "{cmp}");

    // nothing priced: no cost, and nothing to estimate at yet
    let st = ok(&mut admin, &format!("/api/v1/cars/{car}/statistics?period=all&{period}")).await;
    let row = &st["items"][0];
    assert_eq!((row["charges"].as_u64(), row["charges_priced"].as_u64()), (Some(3), Some(0)), "{row}");
    assert_eq!((row["charges_estimated"].as_u64(), &row["cost_total_by_currency"]), (Some(0), &json!([])), "{row}");

    // one charge typed (in euros), a default price for the rest (in dollars)
    let list = ok(&mut admin, &format!("/api/v1/cars/{car}/charges?limit=10")).await;
    let first = list["items"].as_array().unwrap().iter().min_by_key(|c| c["start_ms"].as_u64()).unwrap().clone();
    let r = admin
        .put(
            &format!("/api/v1/cars/{car}/charges/{}/cost", first["id"].as_str().unwrap()),
            json!({ "mode": "total", "amount": 9.0, "currency": "EUR" }),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    let s = ok(&mut admin, &format!("/api/v1/cars/{car}/settings")).await;
    for (bad, field, status) in [
        (json!({ "value": 11, "currency": "USD" }), "default_price_per_kwh.value", 422),
        (json!({ "value": 0.2, "currency": "US" }), "default_price_per_kwh.currency", 422),
        (json!(0.2), "default_price_per_kwh", 400),
    ] {
        let r = admin
            .patch(
                &format!("/api/v1/cars/{car}/settings"),
                json!({ "version": s["version"], "default_price_per_kwh": bad }),
            )
            .await;
        assert_eq!(r.status, status, "{}", r.text());
        assert!(r.json()["error"]["fields"][field].is_string(), "{field}: {}", r.text());
    }
    let r = admin
        .patch(
            &format!("/api/v1/cars/{car}/settings"),
            json!({ "version": s["version"], "default_price_per_kwh": { "value": 0.2, "currency": "usd" } }),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.json()["default_price_per_kwh"], json!({ "value": 0.2, "currency": "USD" }));

    let st = ok(&mut admin, &format!("/api/v1/cars/{car}/statistics?period=all&{period}")).await;
    let row = &st["items"][0];
    assert_eq!((row["charges_priced"].as_u64(), row["charges_estimated"].as_u64()), (Some(1), Some(2)), "{row}");
    assert_eq!(row["cost_by_currency"], json!([{ "currency": "EUR", "amount": 9.0 }]), "the real cost alone");
    // the grid energy where known, else the energy added: 20 kWh + 11 kWh at 0.20
    assert_eq!(row["cost_estimated_by_currency"], json!([{ "currency": "USD", "amount": 6.2 }]), "{row}");
    assert_eq!(
        row["cost_total_by_currency"],
        json!([{ "currency": "EUR", "amount": 9.0 }, { "currency": "USD", "amount": 6.2 }]),
        "never added across currencies"
    );
    let km = f(&row["distance_logged_km"]);
    let days = (f(&row["period_end_ms"]) - f(&row["period_start_ms"])) / DAY as f64;
    let usd = |k: &str| row[k].as_array().unwrap().iter().find(|m| m["currency"] == "USD").map(|m| f(&m["amount"]));
    assert_eq!(usd("cost_total_per_100km"), Some((6.2 / km * 100.0 * 100.0).round() / 100.0), "{row}");
    assert_eq!(usd("cost_total_per_day"), Some((6.2 / days.max(1.0) * 100.0).round() / 100.0), "{row}");

    // forgetting the default price leaves the estimates out again
    let s = ok(&mut admin, &format!("/api/v1/cars/{car}/settings")).await;
    let r = admin
        .patch(
            &format!("/api/v1/cars/{car}/settings"),
            json!({ "version": s["version"], "default_price_per_kwh": null }),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    let st = ok(&mut admin, &format!("/api/v1/cars/{car}/statistics?period=all&{period}")).await;
    assert_eq!(st["items"][0]["charges_estimated"], 0, "{st}");
}
