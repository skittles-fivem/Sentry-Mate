//! Place prices from Supercharger invoices, end to end: made-up invoiced sessions kept by the engine, matched to DC
//! charges at a saved place with no price, give the place a dated price marked `invoice` (the median per kWh billed),
//! which the place's charges without an invoice or a typed cost are then costed at. A typed place price wins over it,
//! a typed charge cost wins over both, and each change is in the audit log (`place-invoice-priced`). Everything is
//! made up.
#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]

use engine::Engine;
use engine::invoices::{Fee, Session};
use model::{CarId, Charge, ChargeKind, Position, Source};
use serde_json::{Value, json};
use std::sync::Arc;
use web::testkit::TestServer;

/// 2026-03-02 09:00 UTC: in the past, so every charge has ended.
const T0: u64 = 1_772_442_000_000;
const MIN: u64 = 60_000;
const DAY: u64 = 86_400_000;
/// A made-up Supercharger site.
const LAT: f64 = 46.5;
const LON: f64 = 8.25;

struct EngineCars(db::Db);

impl web::CarDirectory for EngineCars {
    fn exists(&self, car: CarId) -> bool {
        self.0.car(car).ok().flatten().is_some()
    }
}

struct World {
    s: TestServer,
    engine: Engine,
    db: db::Db,
    car: CarId,
    _dir: tempfile::TempDir,
}

async fn world() -> World {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let (v, _) = vault::Vault::create(&data, &dir.path().join("keys"), vault::Kdf::FAST_FOR_TESTS).unwrap();
    let v = Arc::new(v);
    let db = db::Db::open(&data, v.clone(), db::Options::default()).unwrap();
    let store = ingest::Store::with_limits(data.join("can"), v, 0, 200_000, |_| u64::MAX);
    let engine = Engine::new(db.clone(), store, engine::Options { workers: 1, ..engine::Options::default() });
    engine.start().unwrap();
    let car = engine
        .api_new_car(&tesla::NewCar {
            vin_prefix: Some("5YJ3E1EA0PF".into()),
            model: Some('3'),
            year: Some(2023),
            display_name: Some("Made-up three".into()),
        })
        .unwrap();
    let (e2, d2) = (engine.clone(), db.clone());
    let s = TestServer::start_with(move |b| b.cars(Arc::new(EngineCars(d2))).service(Arc::new(e2))).await;
    World { s, engine, db, car, _dir: dir }
}

fn charge(car: CarId, id: u64, start: u64, kind: ChargeKind) -> Charge {
    Charge {
        id,
        car,
        start_ms: start,
        end_ms: start + 30 * MIN,
        position: Some(Position { lat: LAT, lon: LON, ..Position::default() }),
        place: None,
        address: None,
        kind: Some(kind),
        energy_added_kwh: Some(30.0),
        energy_used_kwh: None,
        start_soc: Some(20.0),
        end_soc: Some(70.0),
        start_range_km: None,
        end_range_km: None,
        power_max_kw: Some(if kind == ChargeKind::Dc { 150.0 } else { 11.0 }),
        outside_temp_avg_c: None,
        battery_temp_avg_c: None,
        cost: None,
        energy_spans: None,
        source: Source::Live,
        inputs: Default::default(),
        rules_version: 0,
    }
}

/// A made-up session billed at `rate` a kWh for 31 kWh.
fn session(car: CarId, key: &str, start: u64, rate: f64) -> Session {
    let amount = (rate * 31.0 * 100.0).round() / 100.0;
    Session {
        key: key.into(),
        source: "tesla".into(),
        account: "ta1".into(),
        vehicle: Some("tv1".into()),
        car: Some(car),
        invoice_id: None,
        invoice_name: None,
        site: Some("Made-up Supercharger, Hilltown".into()),
        lat: Some(LAT),
        lon: Some(LON),
        country_code: Some("CH".into()),
        start_ms: start + MIN,
        end_ms: start + 32 * MIN,
        total: Some(amount),
        currency: Some("EUR".into()),
        kwh_billed: Some(31.0),
        minutes_billed: None,
        fees: vec![Fee {
            kind: "charging".into(),
            amount,
            rate: Some(rate),
            usage: Some(31.0),
            unit: Some("kwh".into()),
        }],
        first_seen_ms: 0,
        updated_ms: 0,
        pdf: false,
        pdf_bytes: 0,
    }
}

async fn ok(c: &mut web::testkit::Client, path: &str) -> Value {
    let r = c.get(path).await;
    assert_eq!(r.status, 200, "{path}: {}", r.text());
    r.json()
}

fn per_kwh(c: &Value) -> Option<f64> {
    c["cost_breakdown"]["parts"][0]["per_kwh"].as_f64()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invoices_price_their_place_and_its_other_charges_and_typed_prices_win() {
    let w = world().await;
    let mut admin = w.s.admin().await;
    let car = format!("c{}", w.car);

    // a saved place with no price where the car Supercharged five times (a day apart) and then charged slowly twice
    let r = admin
        .post(
            "/api/v1/places",
            json!({ "name": "Made-up Supercharger", "kind": "charger", "lat": LAT, "lon": LON, "radius_m": 150,
                    "time_zone": "UTC" }),
        )
        .await;
    assert_eq!(r.status, 201, "{}", r.text());
    let pid = r.json()["id"].as_str().unwrap().to_string();
    let mut rows: Vec<Charge> = (0..5).map(|i| charge(w.car, 900 + i, T0 + i * DAY, ChargeKind::Dc)).collect();
    rows.push(charge(w.car, 950, T0 + 6 * DAY, ChargeKind::Ac));
    rows.push(charge(w.car, 951, T0 + 7 * DAY, ChargeKind::Ac));
    w.db.replace_window::<Charge>(w.car, T0 - DAY, T0 + 8 * DAY, &rows).unwrap();
    // a typed cost on one of the slow charges
    let r = admin
        .put(
            &format!("/api/v1/cars/{car}/charges/951/cost"),
            json!({ "mode": "total", "amount": 4.0, "currency": "EUR", "energy_basis": "grid" }),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    let p = ok(&mut admin, &format!("/api/v1/places/{pid}")).await;
    assert_eq!((p["price"].clone(), p["price_source"].clone()), (Value::Null, Value::Null), "{p}");

    // the invoices, as an input keeps them: 0.50, 0.48, 0.52, 0.48, 0.55 EUR a kWh, each on its fast charge
    let rates = [0.50, 0.48, 0.52, 0.48, 0.55];
    let sessions: Vec<Session> =
        (0..5u64).map(|i| session(w.car, &format!("tesla-made-up-{i}"), T0 + i * DAY, rates[i as usize])).collect();
    let up = w.engine.invoice_upsert(&sessions).unwrap();
    assert_eq!(up.new, 5);
    w.engine.idle().await;

    // the place: the median, from the day of the first session billed at it, marked as from invoices
    let p = ok(&mut admin, &format!("/api/v1/places/{pid}")).await;
    assert_eq!(p["price_source"], "invoice", "{p}");
    assert_eq!((p["price"]["per_kwh"].as_f64(), p["price"]["currency"].as_str()), (Some(0.5), Some("EUR")), "{p}");
    assert_eq!(p["prices"].as_array().unwrap().len(), 1, "{p}");
    assert_eq!(p["prices"][0]["source"], "invoice");
    assert_eq!(p["prices"][0]["from"], "2026-03-02");
    assert_eq!(p["prices"][0]["invoice"]["sessions"], 5);
    // the fast charges keep their invoices' costs; the slow one without a cost takes the place's; the typed one stays
    let c = ok(&mut admin, &format!("/api/v1/cars/{car}/charges/901")).await;
    assert_eq!((c["cost"]["amount"].as_f64(), c["cost"]["source"].as_str()), (Some(14.88), Some("tesla_invoice")));
    let c = ok(&mut admin, &format!("/api/v1/cars/{car}/charges/950")).await;
    assert_eq!((c["cost"]["currency"].as_str(), per_kwh(&c)), (Some("EUR"), Some(0.5)), "{c}");
    let c = ok(&mut admin, &format!("/api/v1/cars/{car}/charges/951")).await;
    assert_eq!((c["cost"]["amount"].as_f64(), c["cost"]["source"].as_str()), (Some(4.0), Some("manual")), "{c}");
    // the charging places list says where its price is from
    let l = ok(
        &mut admin,
        &format!("/api/v1/charging-stats/places?cars={car}&from_ms={}&to_ms={}", T0 - DAY, T0 + 9 * DAY),
    )
    .await;
    let it = l["items"].as_array().unwrap().iter().find(|x| x["place"] == json!(pid)).cloned().unwrap();
    assert_eq!(
        (it["price_source"].as_str(), it["place_price"]["per_kwh"].as_f64()),
        (Some("invoice"), Some(0.5)),
        "{it}"
    );
    // in the audit log, by the server
    assert_eq!(web::resources::places::audit_invoice_prices(&w.s.app), 1);
    assert_eq!(web::resources::places::audit_invoice_prices(&w.s.app), 0, "taken once");
    let a = ok(&mut admin, "/api/v1/audit?limit=50").await;
    let e = a["items"].as_array().unwrap().iter().find(|x| x["action"] == "place-invoice-priced").cloned();
    let e = e.unwrap_or_else(|| panic!("no place-invoice-priced entry: {a}"));
    let said = e.to_string();
    assert!(said.contains("Made-up Supercharger") && said.contains("0.5 EUR a kWh from 2026-03-02"), "{said}");

    // the same invoices read again change nothing
    assert_eq!(w.engine.invoice_upsert(&sessions).unwrap().same, 5);
    w.engine.idle().await;
    assert_eq!(web::resources::places::audit_invoice_prices(&w.s.app), 0);

    // an admin's price wins over it on every day; newer invoices at a dearer price add an invoice price from their
    // day, and the typed one stays the price
    let v = p["version"].as_u64().unwrap();
    let typed = json!({ "currency": "EUR", "per_kwh": 0.3 });
    let r = admin
        .patch(&format!("/api/v1/places/{pid}"), json!({ "version": v, "prices": [{ "from": null, "price": typed }] }))
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!((r.json()["price_source"].as_str(), r.json()["price"]["per_kwh"].as_f64()), (Some("typed"), Some(0.3)));
    let mut later: Vec<Charge> =
        (0..10).map(|i| charge(w.car, 1000 + i, T0 + (20 + i) * DAY, ChargeKind::Dc)).collect();
    later.push(charge(w.car, 1050, T0 + 31 * DAY, ChargeKind::Ac));
    w.db.replace_window::<Charge>(w.car, T0 + 19 * DAY, T0 + 32 * DAY, &later).unwrap();
    let dearer: Vec<Session> =
        (0..10u64).map(|i| session(w.car, &format!("tesla-made-up-{}", 10 + i), T0 + (20 + i) * DAY, 0.61)).collect();
    w.engine.invoice_upsert(&dearer).unwrap();
    w.engine.idle().await;
    let p = ok(&mut admin, &format!("/api/v1/places/{pid}")).await;
    let inv: Vec<(Value, f64)> = p["prices"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|x| x["source"] == "invoice")
        .map(|x| (x["from"].clone(), x["price"]["per_kwh"].as_f64().unwrap()))
        .collect();
    // (with ten sessions or more, the first price is the median of the first ten: 0.55 from the day of the first
    // session at it)
    assert_eq!(inv, vec![(json!("2026-03-06"), 0.55), (json!("2026-03-22"), 0.61)], "{p}");
    assert_eq!((p["price_source"].as_str(), p["price"]["per_kwh"].as_f64()), (Some("typed"), Some(0.3)), "{p}");
    // the slow charge after them is costed at the typed price, not the invoices'
    let c = ok(&mut admin, &format!("/api/v1/cars/{car}/charges/1050")).await;
    assert_eq!(per_kwh(&c), Some(0.3), "{c}");
    assert_eq!(web::resources::places::audit_invoice_prices(&w.s.app), 1);
}
