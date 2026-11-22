#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! A TeslaMate import and a TeslaFi import of the same days, with no live data, through the real router
//! (web::testkit): one drive per real drive (the import with more data in the span shown), the other import once an
//! admin flips the span (audited, version checked), totals, statistics, the timeline and series never doubled, and
//! each site still alone with its own `source=`.

use engine::Engine;
use model::{Car, CarId, Source, State, StateSpan};
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

fn drive(car: CarId, source: Source, n: i64, start_ms: u64, odo: f64) -> model::Drive {
    model::Drive {
        id: engine::importer::imported_id_of(source, car, n),
        car,
        start_ms,
        end_ms: start_ms + 20 * 60_000,
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
        rules_version: 0,
    }
}

fn charge(car: CarId, source: Source, n: i64, start_ms: u64) -> model::Charge {
    model::Charge {
        id: engine::importer::imported_id_of(source, car, n),
        car,
        start_ms,
        end_ms: start_ms + H,
        position: None,
        place: None,
        address: None,
        kind: Some(model::ChargeKind::Ac),
        energy_added_kwh: Some(20.0),
        energy_used_kwh: None,
        start_soc: None,
        end_soc: None,
        start_range_km: None,
        end_range_km: None,
        power_max_kw: None,
        outside_temp_avg_c: None,
        battery_temp_avg_c: None,
        cost: None,
        source,
        inputs: model::Inputs::NONE,
        rules_version: 0,
    }
}

fn state(car: CarId, source: Source, n: i64, a: u64, b: u64) -> db::StateRow {
    db::StateRow {
        id: engine::importer::imported_id_of(source, car, n),
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

/// A position a minute through the first drive's twenty, at `kmh`.
fn positions(kmh: f64, from: u64) -> Vec<model::Sample> {
    (0..20u64).map(|i| model::Sample { t_ms: from + i * 60_000, speed_kmh: Some(kmh), ..Default::default() }).collect()
}

/// TeslaMate: a drive on a day of its own, then three drives on DAY0 (and its states). TeslaFi: the same three
/// drives 40 s later, and a charge TeslaMate missed (more data: TeslaFi's shown at first).
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
            name: "Twice".into(),
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
    let mut drives = vec![drive(car, tm, 1, DAY0 - 10 * 24 * H, 900.0)];
    for (i, h) in [1u64, 3, 5].into_iter().enumerate() {
        let n = i as i64 + 2;
        drives.push(drive(car, tm, n, DAY0 + h * H, 1_000.0 + 10.0 * i as f64));
        drives.push(drive(car, tf, n, DAY0 + h * H + 40_000, 1_000.0 + 10.0 * i as f64));
    }
    db.upsert(car, &drives).unwrap();
    db.upsert(car, &[charge(car, tf, 1, DAY0 + 8 * H)]).unwrap();
    db.upsert(car, &[state(car, tm, 1, DAY0, DAY0 + 12 * H), state(car, tf, 1, DAY0 + 30_000, DAY0 + 12 * H)]).unwrap();
    db.write_points(car, tm, DAY0 + H, DAY0 + H + 20 * 60_000, &positions(50.0, DAY0 + H)).unwrap();
    db.write_points(car, tf, DAY0 + H, DAY0 + H + 20 * 60_000, &positions(80.0, DAY0 + H + 30_000)).unwrap();
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

/// The rows of a list by their old source word: (TeslaMate's, TeslaFi's).
async fn sites(c: &mut Client, path: &str) -> (usize, usize) {
    let v = ok(c, path).await;
    let items = v["items"].as_array().unwrap();
    let of = |w: &str| items.iter().filter(|d| d["source"] == w).count();
    (of("teslamate"), of("teslafi"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_imports_of_the_same_days_show_one_of_them() {
    let k = kit().await;
    let car = k.car.clone();
    let mut admin = k.s.admin().await;
    let drives = format!("/api/v1/cars/{car}/drives");
    let charges = format!("/api/v1/cars/{car}/charges");

    // one drive per real drive: TeslaFi's (a charge more) on DAY0, TeslaMate's own day kept
    assert_eq!(sites(&mut admin, &drives).await, (1, 3), "the bigger import");
    assert_eq!(sites(&mut admin, &format!("{drives}?source=import")).await, (1, 3), "source=import too");
    assert_eq!(sites(&mut admin, &format!("{drives}?source=import:teslamate")).await, (4, 0));
    assert_eq!(sites(&mut admin, &format!("{drives}?source=import:teslafi")).await, (0, 3));
    assert_eq!(sites(&mut admin, &charges).await, (0, 1));

    // totals, statistics and the timeline count each drive once
    let t = ok(&mut admin, &format!("/api/v1/cars/{car}/totals")).await;
    assert_eq!((t["drives"].as_u64(), t["charges"].as_u64()), (Some(4), Some(1)), "{t}");
    assert!((t["distance_km"].as_f64().unwrap() - 40.0).abs() < 1e-6, "{t}");
    let t = ok(&mut admin, &format!("/api/v1/cars/{car}/totals?source=import")).await;
    assert_eq!(t["drives"].as_u64(), Some(4), "{t}");
    let st = ok(&mut admin, &format!("/api/v1/cars/{car}/statistics?period=all")).await;
    assert_eq!(st["items"][0]["drives"].as_u64(), Some(4), "{st}");
    let tl = ok(&mut admin, &format!("/api/v1/cars/{car}/timeline?kinds=drive&limit=50")).await;
    assert_eq!(tl["items"].as_array().unwrap().len(), 4, "{tl}");
    // series: TeslaFi's positions alone in the span
    let series = format!("/api/v1/cars/{car}/series?fields=speed_kph&from_ms={}&to_ms={}", DAY0 + H, DAY0 + 2 * H);
    let speeds = |v: &Value| -> Vec<f64> {
        v["values"]["speed_kph"].as_array().unwrap().iter().filter_map(Value::as_f64).collect::<Vec<_>>()
    };
    let s = ok(&mut admin, &series).await;
    assert!(!speeds(&s).is_empty() && speeds(&s).iter().all(|v| (v - 80.0).abs() < 1e-6), "{s}");
    let s = ok(&mut admin, &format!("{series}&source=import:teslamate")).await;
    assert!(!speeds(&s).is_empty() && speeds(&s).iter().all(|v| (v - 50.0).abs() < 1e-6), "{s}");

    // the span in /overlaps, with each site's counts
    let path = format!("/api/v1/cars/{car}/overlaps");
    let o = ok(&mut admin, &path).await;
    let items = o["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "{o}");
    let span = &items[0];
    assert_eq!(span["kind"], "imports", "{o}");
    assert_eq!(span["shown_source"], "import:teslafi", "{o}");
    assert_eq!(span["default_source"], "import:teslafi", "{o}");
    assert_eq!(span["default_reason"], "more_drives_and_charges", "{o}");
    assert_eq!(span["sites"]["teslamate"], json!({ "drives": 3, "charges": 0 }), "{o}");
    assert_eq!(span["sites"]["teslafi"], json!({ "drives": 3, "charges": 1 }), "{o}");
    assert_eq!(span["live"], json!({ "drives": 0, "charges": 0 }), "{o}");
    assert!(span["start_ms"].as_u64().unwrap() <= DAY0 + H && span["end_ms"].as_u64().unwrap() >= DAY0 + 9 * H);
    let (id, version) = (span["id"].as_str().unwrap().to_string(), span["version"].as_u64().unwrap());
    assert!(id.starts_with("imports:"), "{id}");

    // the flip: admin only, a site word, the version loaded
    let mut watch = k.s.watch(&mut admin, &[]).await;
    let r = watch.patch(&path, json!({ "id": id, "shown": "import:teslamate", "version": version })).await;
    assert_eq!(r.status, 403);
    let r = admin.patch(&path, json!({ "id": id, "shown": "import:teslamate" })).await;
    assert_eq!((r.status, r.code()), (400, Some("bad_input".into())), "no version: {}", r.text());
    let r = admin.patch(&path, json!({ "id": id, "shown": "live", "version": version })).await;
    assert_eq!((r.status, r.code()), (422, Some("invalid".into())), "{}", r.text());
    let r = admin.patch(&path, json!({ "id": "imports:1", "shown": "import:teslamate", "version": version })).await;
    assert_eq!((r.status, r.code()), (404, Some("no_such_overlap".into())));
    let r = admin.patch(&path, json!({ "id": id, "shown": "import:teslamate", "version": version + 1 })).await;
    assert_eq!((r.status, r.code()), (409, Some("stale_version".into())), "{}", r.text());
    let r = admin.patch(&path, json!({ "id": id, "shown": "import:teslamate", "version": version })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let v = r.json();
    assert_eq!(v["changed"]["shown_source"], "import:teslamate", "{v}");
    assert_eq!(v["items"][0]["shown_source"], "import:teslamate", "{v}");
    assert_eq!(v["items"][0]["default_source"], "import:teslafi", "{v}");
    assert_eq!(v["items"][0]["version"].as_u64(), Some(version + 1), "{v}");
    let r = admin.patch(&path, json!({ "id": id, "shown": "import:teslafi", "version": version })).await;
    assert_eq!((r.status, r.code()), (409, Some("stale_version".into())), "the old version: {}", r.text());
    let audit = ok(&mut admin, "/api/v1/audit").await;
    assert!(audit.to_string().contains("car-overlap"), "{audit}");

    // now TeslaMate's, still once
    assert_eq!(sites(&mut admin, &drives).await, (4, 0));
    assert_eq!(sites(&mut admin, &format!("{drives}?source=import")).await, (4, 0));
    assert_eq!(sites(&mut admin, &format!("{drives}?source=import:teslafi")).await, (0, 3), "TeslaFi alone");
    assert_eq!(sites(&mut admin, &charges).await, (0, 0));
    let t = ok(&mut admin, &format!("/api/v1/cars/{car}/totals")).await;
    assert_eq!((t["drives"].as_u64(), t["charges"].as_u64()), (Some(4), Some(0)), "{t}");
    let st = ok(&mut admin, &format!("/api/v1/cars/{car}/statistics?period=all")).await;
    assert_eq!(st["items"][0]["drives"].as_u64(), Some(4), "{st}");
    let s = ok(&mut admin, &series).await;
    assert!(!speeds(&s).is_empty() && speeds(&s).iter().all(|v| (v - 50.0).abs() < 1e-6), "{s}");
}
