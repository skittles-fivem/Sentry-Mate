#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! Finding a car's Screenmate GPS layout through the API (`/cars/{car}/gps-layout/fit`, Data health's "Find GPS
//! layout") and applying it (`PUT /cars/{car}/gps-layout`), over an engine with a made-up car: ten minutes of logs
//! whose GPS frame carries the longitude off by a made-up offset that is no power of two of its steps, and its local
//! TeslaMate positions (a made-up drive in the open sea). The answers hold layouts, counts and metres only.

use engine::Engine;
use model::gps::GpsField;
use model::{CarId, Sample, Stream};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use std::time::Duration;
use web::testkit::{Client, TestServer};

const KEY: &str = "abcdef123456";
const FOUND: &str = "lat 0|28@1- (1e-6,0); lon 28|28@1- (1e-6,16.9234567)";

struct EngineCars(db::Db);

impl web::CarDirectory for EngineCars {
    fn exists(&self, car: CarId) -> bool {
        self.0.car(car).ok().flatten().is_some()
    }
}

fn drive(n: usize) -> Vec<(f64, f64)> {
    (0..n)
        .map(|i| {
            let k = i as f64;
            (-30.0 + 0.0004 * k + 0.002 * (k / 40.0).sin(), -20.0 + 0.0003 * k + 0.002 * (k / 55.0).cos())
        })
        .collect()
}

/// A frame with the latitude and longitude encoded by Intel fields `lat` and `lon`.
fn frame(lat: &GpsField, lon: &GpsField, la: f64, lo: f64) -> [u8; 8] {
    let mut le = 0u64;
    for (f, v) in [(lat, la), (lon, lo)] {
        let raw = ((v - f.offset) / f.scale).round() as i64;
        le |= ((raw as u64) & ((1u64 << f.width) - 1)) << f.start;
    }
    le.to_le_bytes()
}

/// `YYYYMMDDTHHMMSSZ` of ms since 1970.
fn stamp(ms: u64) -> String {
    let (days, secs) = ((ms / 86_400_000) as i64, ms / 1000 % 86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}{m:02}{d:02}T{:02}{:02}{:02}Z", secs / 3600, secs / 60 % 60, secs % 60)
}

async fn put(store: &ingest::Store, judge: &ingest::Judge, name: &str, bytes: &[u8]) {
    let sha = hex::encode(Sha256::digest(bytes));
    let r = store.receive(name, bytes.to_vec().into(), 0, bytes.len() as u64, &sha).await.unwrap();
    assert!(r.complete, "{name}");
    judge.arrived(name).await.unwrap();
}

struct Seeded {
    engine: Engine,
    s: TestServer,
    car: CarId,
    _dir: tempfile::TempDir,
}

async fn seeded() -> Seeded {
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
            vehicle_keys: vec![KEY.into()],
            preferred_key: None,
            vin: None,
            profile_setting: None,
            time_zone: "UTC".into(),
            time_zone_typed: None,
            efficiency_wh_km: None,
            created_ms: 0,
            inputs: model::Inputs::NONE,
        })
        .unwrap()
        .id;
    let lat = engine::gps_fit::dbc_field("GPSLatitude04F").unwrap();
    let low = GpsField { offset: 16.9234567, ..engine::gps_fit::dbc_field("GPSLongitude04F").unwrap() };
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
    let t0 = (now - 2 * 86_400_000) / 60_000 * 60_000;
    let path = drive(600);
    let refs: Vec<Sample> = path
        .iter()
        .enumerate()
        .map(|(i, &(la, lo))| Sample { t_ms: t0 + i as u64 * 1000, lat: Some(la), lon: Some(lo), ..Sample::default() })
        .collect();
    db.write_points(car, Stream::TeslamateLocal, t0, t0 + 600_000, &refs).unwrap();
    let store = ingest::Store::with_limits(data.join("can"), v, 0, 200_000, |_| u64::MAX);
    let engine = Engine::new(db.clone(), store.clone(), engine::Options { workers: 2, ..engine::Options::default() });
    let judge = ingest::Judge::new(store.clone());
    for minute in 0..10u64 {
        let start = t0 + minute * 60_000;
        let mut text = String::from("# canbridge v1 app=test\n");
        for j in 0..300u64 {
            let t = start + j * 200 + 500;
            let (la, lo) = path[(((t - t0) / 1000) as usize).min(path.len() - 1)];
            let hex: String = frame(&lat, &low, la, lo).iter().map(|b| format!("{b:02X}")).collect();
            text += &format!("({}.{:06}) can3 04F#{hex}\n", t / 1000, (t % 1000) * 1000);
        }
        let xz = synth::xz::pack(text.as_bytes(), 1).unwrap();
        let name = format!("{KEY}_{}_{}.log.xz", stamp(start), minute + 1);
        let side = json!({ "format": "canbridge-candump-xz/1", "bus": { "can0": "vehicle", "can1": "chassis",
                           "can3": "screenmate-internal" } });
        put(&store, &judge, &name, &xz).await;
        put(&store, &judge, &format!("{name}.json"), &serde_json::to_vec(&side).unwrap()).await;
    }
    engine.start().unwrap();
    engine.idle().await;
    let (e2, d2) = (engine.clone(), db.clone());
    let s = TestServer::start_with(move |b| b.cars(Arc::new(EngineCars(d2))).service(Arc::new(e2))).await;
    Seeded { s, car, engine, _dir: dir }
}

/// Polls the fit until it is no longer running.
async fn ended(admin: &mut Client, path: &str) -> Value {
    for _ in 0..1200 {
        let r = admin.get(path).await;
        assert_eq!(r.status, 200, "{}", r.text());
        let v = r.json();
        if v["state"] != "running" {
            return v;
        }
        assert!(v["percent"].as_u64().unwrap() <= 100, "{v}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("the fit never ended");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_admin_finds_the_layout_in_the_background_and_applies_it() {
    let k = seeded().await;
    let mut admin = k.s.admin().await;
    let fit = format!("/api/v1/cars/c{}/gps-layout/fit", k.car);
    // none yet
    let idle = admin.get(&fit).await.json();
    assert_eq!((idle["state"].as_str(), &idle["result"], &idle["current"]), (Some("idle"), &Value::Null, &Value::Null));
    // bad days
    let bad = admin.post(&fit, json!({ "days": 0 })).await;
    assert_eq!((bad.status, bad.code()), (422, Some("invalid".into())), "{}", bad.text());
    let bad = admin.post(&fit, json!({ "days": 30, "other": 1 })).await;
    assert_eq!((bad.status, bad.code()), (400, Some("bad_input".into())), "{}", bad.text());
    // started: 202 with its status; asked again while it runs, the same fit
    let r = admin.post(&fit, json!({ "days": 30 })).await;
    assert_eq!(r.status, 202, "{}", r.text());
    let started = r.json();
    assert_eq!(started["days"], json!(30));
    let id = started["id"].clone();
    if started["state"] == "running" {
        let again = admin.post(&fit, json!({})).await.json();
        assert!(again["id"] == id || again["state"] != "running", "{again}");
    }
    let done = ended(&mut admin, &fit).await;
    assert_eq!(done["state"], "done", "{done}");
    assert_eq!(done["id"], id);
    assert_eq!(done["percent"], json!(100));
    let res = &done["result"];
    assert_eq!(res["layout"], json!(FOUND), "{res}");
    assert_eq!(
        (res["is_dbc"].as_bool(), res["good"].as_bool(), res["enough"].as_bool()),
        (Some(false), Some(true), Some(true))
    );
    assert_eq!(
        (res["logs"].as_u64(), res["days_skipped"].as_u64(), res["logs_skipped"].as_u64()),
        (Some(10), Some(0), Some(0))
    );
    assert!(res["pairs"].as_u64().unwrap() > 500, "{res}");
    let local = &res["sources"][0];
    assert_eq!((&local["input"], &local["positions"]), (&json!("teslamate-local"), &json!(600)), "{res}");
    assert_eq!((&local["pairs"], &local["trimmed"], &local["left_out"]), (&res["pairs"], &json!(0), &Value::Null));
    assert!(local["lon_p95_m"].as_f64().unwrap() < 5.0, "{res}");
    // the line of the longitude's bits: the DBC's scale and the offset, constant
    let line = &res["lon"]["line"];
    assert_eq!(
        (&line["bits"], &line["nominal"], &line["drift"]["constant"]),
        (&json!("28|28@1-"), &json!(1e-6), &json!(true)),
        "{res}"
    );
    assert!(res["p95_m"].as_f64().unwrap() < 5.0 && res["median_m"].as_f64().is_some(), "{res}");
    // the DBC's longitude is far off: only "over 10 km" (null), never kilometres that would tell the latitude
    assert_eq!(res["lon"]["dbc"]["median_m"], Value::Null, "{res}");
    assert_eq!(res["lon"]["best"]["layout"], json!("28|28@1- (1e-6,16.9234567)"), "{res}");
    assert_eq!(res["car"], json!(format!("c{}", k.car)));
    // no coordinates anywhere in the answer
    let text = done.to_string();
    for bad in ["-30.", "-20.", "29.9", "19.9"] {
        assert!(!text.contains(bad), "{bad} in {text}");
    }
    assert_eq!((done["applied"].as_bool(), &done["current"]), (Some(false), &Value::Null));

    // Apply: the existing PUT, which rebuilds the car; the fit then says it is the layout in use
    let layout = format!("/api/v1/cars/c{}/gps-layout", k.car);
    let before = admin.get(&layout).await.json();
    assert_eq!((&before["layout"], &before["set_ms"]), (&Value::Null, &Value::Null), "{before}");
    let t0 = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
    let r = admin.put(&layout, json!({ "layout": res["layout"] })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let applied = r.json();
    assert_eq!(applied["layout"], json!(FOUND));
    assert!(applied["job"].is_string(), "{applied}");
    let set = applied["set_ms"].as_u64().expect("set_ms");
    assert!(set >= t0, "{applied}");
    // the GET says the same, watch-only accounts too
    let now = admin.get(&layout).await.json();
    assert_eq!((now["layout"].as_str(), now["set_ms"].as_u64()), (Some(FOUND), Some(set)), "{now}");
    let after = admin.get(&fit).await.json();
    assert_eq!((after["applied"].as_bool(), after["current"].as_str()), (Some(true), Some(FOUND)), "{after}");
    // applied again: nothing to do
    assert_eq!(admin.put(&layout, json!({ "layout": FOUND })).await.json()["job"], Value::Null);

    // admins only; an unknown car is one
    let mut watch = k.s.watch(&mut admin, &[k.car]).await;
    assert_eq!(watch.get(&fit).await.status, 403);
    let seen = watch.get(&layout).await;
    assert_eq!((seen.status, seen.json()["set_ms"].as_u64()), (200, Some(set)), "{}", seen.text());
    assert_eq!(watch.put(&layout, json!({ "layout": null })).await.status, 403);
    assert_eq!(watch.post(&fit, json!({})).await.status, 403);
    let other = format!("/api/v1/cars/c{}/gps-layout/fit", k.car + 9);
    assert_eq!(admin.post(&other, json!({})).await.status, 404);
}

/// A fit asked while a full rebuild runs waits for it (both read the same logs, and the rebuild is what the history
/// waits for): it says why, its phase and counts stay where they were, and once the rebuild ends it goes on from
/// there to the same result. (A rebuild started while a fit runs never waits for it: nothing of the rebuild asks
/// after a fit.)
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_fit_waits_while_a_full_rebuild_runs_and_goes_on_after() {
    let k = seeded().await;
    let e = k.engine.clone();
    e.set_gps_fit_wait(Duration::from_millis(10));
    let mut admin = k.s.admin().await;
    let fit = format!("/api/v1/cars/c{}/gps-layout/fit", k.car);
    assert!(e.rebuild_running().is_none());
    // a full rebuild held while it decodes (a test's hold: it runs for as long as the test needs)
    e.full_rebuild_hold_at(Some(engine::full::Phase::Decoding));
    e.full_rebuild(Some(k.car), engine::full::Reason::Owner, None).unwrap();
    for _ in 0..2000 {
        if e.full_rebuild_status().unwrap().unwrap().phase == engine::full::Phase::Decoding {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(e.rebuild_running().is_some());
    let r = admin.post(&fit, json!({ "days": 30 })).await;
    assert_eq!(r.status, 202, "{}", r.text());
    let id = r.json()["id"].clone();
    // it waits, and says why
    let mut seen = Value::Null;
    for _ in 0..500 {
        seen = admin.get(&fit).await.json();
        if seen["waiting"].is_string() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(seen["state"], "running", "{seen}");
    assert!(seen["waiting"].as_str().unwrap().contains("full rebuild goes first"), "{seen}");
    assert!(seen["server_busy"].as_str().unwrap().starts_with("rebuild"), "{seen}");
    // and stays where it is while the rebuild runs
    tokio::time::sleep(Duration::from_millis(300)).await;
    let still = admin.get(&fit).await.json();
    assert_eq!(still["state"], "running", "{still}");
    assert!(still["waiting"].is_string(), "{still}");
    for f in ["id", "phase", "done", "total", "percent"] {
        assert_eq!(still[f], seen[f], "{f}: {still}");
    }
    assert_eq!(still["id"], id);
    // the rebuild goes on and ends; then the fit goes on from where it was, to the same result
    e.full_rebuild_hold_at(None);
    let done = ended(&mut admin, &fit).await;
    e.idle().await;
    let r = e.full_rebuild_status().unwrap().unwrap();
    assert_eq!(r.status, engine::full::FullStatus::Done, "{r:#?}");
    assert_eq!((done["state"].as_str(), &done["id"], &done["waiting"]), (Some("done"), &id, &Value::Null), "{done}");
    assert_eq!(done["result"]["layout"], json!(FOUND), "{done}");
    assert_eq!(done["result"]["logs"], json!(10), "{done}");
    assert!(e.rebuild_running().is_none());
}
