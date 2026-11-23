//! Supercharger invoices on the web API: off until an admin turns them on (with the password again, audited), the
//! charge priced from its made-up invoice with the PDF to download for whoever sees the car, the admins' list, its
//! CSV and the zip of every PDF (audited). Everything is made up.
#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]

use engine::Engine;
use engine::invoices::{Fee, Session};
use model::{CarId, Charge, ChargeKind, Position, Source};
use serde_json::json;
use std::sync::Arc;
use web::resources::tesla::TeslaService;
use web::testkit::{ADMIN_PASSWORD, TestServer};

const T0: u64 = 1_800_000_000_000;
const MIN: u64 = 60_000;

struct EngineCars(db::Db);

impl web::CarDirectory for EngineCars {
    fn exists(&self, car: CarId) -> bool {
        self.0.car(car).ok().flatten().is_some()
    }
}

struct World {
    s: TestServer,
    svc: Arc<TeslaService>,
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
    let store = ingest::Store::with_limits(data.join("can"), v.clone(), 0, 200_000, |_| u64::MAX);
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
    let svc = Arc::new(tesla::Service::new(
        &data,
        v,
        Arc::new(engine::TeslaSink::new(engine.clone())),
        tesla::Options::default(),
    ));
    let (e2, d2, s2) = (engine.clone(), db.clone(), svc.clone());
    let s = TestServer::start_with(move |b| b.cars(Arc::new(EngineCars(d2))).service(Arc::new(e2)).service(s2)).await;
    World { s, svc, engine, db, car, _dir: dir }
}

fn seed(w: &World) {
    let ch = Charge {
        id: 900,
        car: w.car,
        start_ms: T0,
        end_ms: T0 + 30 * MIN,
        position: Some(Position { lat: 45.0, lon: 7.0, ..Position::default() }),
        place: None,
        address: None,
        kind: Some(ChargeKind::Dc),
        energy_added_kwh: Some(30.0),
        energy_used_kwh: None,
        start_soc: Some(20.0),
        end_soc: Some(70.0),
        start_range_km: None,
        end_range_km: None,
        power_max_kw: Some(150.0),
        outside_temp_avg_c: None,
        battery_temp_avg_c: None,
        cost: None,
        source: Source::Live,
        inputs: Default::default(),
        rules_version: 0,
    };
    w.db.replace_window::<Charge>(w.car, T0 - 3_600_000, T0 + 3_600_000, &[ch]).unwrap();
    let s = |key: &str, car: Option<CarId>, start: u64, total: f64| Session {
        key: key.into(),
        source: "tesla".into(),
        account: "ta1".into(),
        vehicle: Some("tv1".into()),
        car,
        invoice_id: Some(format!("made-up-{key}")),
        invoice_name: Some("MADE-UP.pdf".into()),
        site: Some("Made-up Supercharger, Hilltown".into()),
        lat: None,
        lon: None,
        country_code: Some("US".into()),
        start_ms: start,
        end_ms: start + 32 * MIN,
        total: Some(total),
        currency: Some("USD".into()),
        kwh_billed: Some(31.0),
        minutes_billed: None,
        fees: vec![Fee {
            kind: "charging".into(),
            amount: total,
            rate: Some(0.45),
            usage: Some(31.0),
            unit: Some("kwh".into()),
        }],
        first_seen_ms: 0,
        updated_ms: 0,
        pdf: false,
        pdf_bytes: 0,
    };
    w.engine
        .invoice_upsert(&[s("tesla-1", Some(w.car), T0 + MIN, 13.95), s("tesla-2", None, T0 + 86_400_000, 4.5)])
        .unwrap();
    assert!(w.engine.invoice_put_pdf("tesla-1", b"%PDF-1.4\n% made-up invoice one\n").unwrap());
}

#[tokio::test]
async fn invoices_are_opt_in_priced_on_the_charge_and_downloadable() {
    let w = world().await;
    let mut admin = w.s.admin().await;
    let r = admin.get("/api/v1/integrations/tesla").await;
    assert_eq!(r.status, 200, "{}", r.text());
    let inv = &r.json()["invoices"];
    assert_eq!(
        (inv["enabled"].clone(), inv["gives_pdfs"].clone(), inv["backfill"].clone()),
        (json!(false), json!(true), json!("off"))
    );
    assert!(!w.svc.cadence().invoices, "off by default");
    // turning them on asks for the password again, and is audited
    let r = admin.patch("/api/v1/integrations/tesla", json!({ "version": 1, "invoices": true })).await;
    assert_eq!((r.status, r.code().as_deref()), (403, Some("password_needed")));
    let r = admin
        .patch(
            "/api/v1/integrations/tesla",
            json!({ "version": 1, "invoices": true, "confirm_password": ADMIN_PASSWORD }),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.json()["invoices"]["enabled"], json!(true));
    assert!(w.svc.cadence().invoices);
    let r = admin.get("/api/v1/audit?limit=50").await;
    assert!(r.text().contains("tesla-invoices-changed"), "{}", r.text());

    seed(&w);
    let car = format!("c{}", w.car);
    let r = admin.get(&format!("/api/v1/cars/{car}/charges/900")).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let d = r.json();
    assert_eq!((d["cost"]["amount"].clone(), d["cost"]["source"].clone()), (json!(13.95), json!("tesla_invoice")));
    assert_eq!(d["invoices"][0]["id"], json!("tesla-1"));
    assert_eq!((d["invoices"][0]["pdf"].clone(), d["invoices"][0]["applied"].clone()), (json!(true), json!(true)));
    let r = admin.get(&format!("/api/v1/cars/{car}/charges")).await;
    assert_eq!(r.json()["items"][0]["cost"]["source"], json!("tesla_invoice"));

    // the PDF, for whoever sees the car; not another invoice through this charge
    let mut watch = w.s.watch(&mut admin, &[w.car]).await;
    let r = watch.get(&format!("/api/v1/cars/{car}/charges/900/invoices/tesla-1/pdf")).await;
    assert_eq!(r.status, 200);
    assert_eq!(r.header("content-type"), Some("application/pdf"));
    assert!(r.header("content-disposition").unwrap().starts_with("attachment;"));
    assert!(r.body.starts_with(b"%PDF-"));
    let r = watch.get(&format!("/api/v1/cars/{car}/charges/900/invoices/tesla-2/pdf")).await;
    assert_eq!(r.status, 404);
    // the list, its CSV and the zip are the admins'
    assert_eq!(watch.get("/api/v1/integrations/invoices").await.status, 403);
    let r = admin.get("/api/v1/integrations/invoices").await;
    assert_eq!(r.status, 200, "{}", r.text());
    let l = r.json();
    assert_eq!(
        (l["totals"]["sessions"].clone(), l["totals"]["matched"].clone(), l["totals"]["unmatched"].clone()),
        (json!(2), json!(1), json!(1))
    );
    assert_eq!(l["items"][0]["id"], json!("tesla-2"), "newest first");
    assert_eq!(l["items"][1]["charge"], json!("900"));
    let r = admin.get("/api/v1/integrations/invoices?matched=false&limit=1").await;
    assert_eq!(r.json()["items"].as_array().unwrap().len(), 1);
    let r =
        admin.get(&format!("/api/v1/integrations/invoices?format=csv&from_ms={}&to_ms={}", T0, T0 + 3_600_000)).await;
    assert_eq!(r.status, 200);
    assert!(r.header("content-type").unwrap().starts_with("text/csv"));
    let csv = r.text();
    assert_eq!(csv.lines().count(), 2, "{csv}");
    assert!(csv.contains("tesla-1") && !csv.contains("tesla-2"));
    let r = admin.get("/api/v1/integrations/invoices/zip").await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.header("content-type"), Some("application/zip"));
    assert!(r.body.starts_with(b"PK"));
    assert!(r.body.windows(9).any(|x| x == b"tesla-1.p"));
    let r = admin.get(&format!("/api/v1/integrations/invoices/zip?from_ms={}", T0 + 3_600_000)).await;
    assert_eq!((r.status, r.code().as_deref()), (404, Some("no_invoices")));
    let r = admin.get("/api/v1/integrations/invoices/tesla-1/pdf").await;
    assert_eq!(r.status, 200);
    let r = admin.get("/api/v1/audit?limit=50").await;
    assert!(r.text().contains("invoices-downloaded"));

    // a typed cost wins over the invoice
    let r = admin
        .put(
            &format!("/api/v1/cars/{car}/charges/900/cost"),
            json!({ "mode": "total", "amount": 9.5, "currency": "EUR" }),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    let d = admin.get(&format!("/api/v1/cars/{car}/charges/900")).await.json();
    assert_eq!(d["cost"]["source"], json!("manual"));
    assert_eq!(d["invoices"][0]["applied"], json!(false));
}
