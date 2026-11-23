#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! The logs the server should have but no longer does (`ingest::missing`), as the owner's pages see them: the scan
//! finds a stored log that was deleted, `/cars/{car}/health` shows it under `missing_logs` (and `logs_missing` in
//! `reasons`), the connections overview's CAN Bridge row counts it, the audit log has the scan's entry, and once the
//! same bytes are sent again it is gone from all of them. Made-up car and logs (synth).

use engine::Engine;
use model::CarId;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::sync::Arc;
use web::testkit::{Client, TestServer};

struct EngineCars(db::Db);

impl web::CarDirectory for EngineCars {
    fn exists(&self, car: CarId) -> bool {
        self.0.car(car).ok().flatten().is_some()
    }
}

async fn put_log(store: &ingest::Store, judge: &ingest::Judge, name: &str, bytes: &[u8]) {
    let sha = hex::encode(Sha256::digest(bytes));
    let r = store.receive(name, bytes.to_vec().into(), 0, bytes.len() as u64, &sha).await.unwrap();
    assert!(r.complete, "{name}");
    judge.arrived(name).await.unwrap();
}

async fn ok(c: &mut Client, path: &str) -> Value {
    let r = c.get(path).await;
    assert_eq!(r.status, 200, "{path}: {}", r.text());
    r.json()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_deleted_log_is_found_shown_and_cleared_when_it_comes_back() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let (v, _) = vault::Vault::create(&data, &dir.path().join("keys"), vault::Kdf::FAST_FOR_TESTS).unwrap();
    let v = Arc::new(v);
    let db = db::Db::open(&data, v.clone(), db::Options::default()).unwrap();
    let store = ingest::Store::with_limits(data.join("can"), v, 0, 200_000, |_| u64::MAX);
    let engine = Engine::new(
        db.clone(),
        store.clone(),
        engine::Options { workers: 2, new_keys: engine::NewKeys::NewCar, ..engine::Options::default() },
    );
    let judge = ingest::Judge::new(store.clone()).on_judged(engine.listener());
    engine.start().unwrap();
    let sc = synth::Scenario::new(11).days(1).rates(synth::Rates::Light).with_trouble(false);
    let logs: Vec<_> = sc.logs().collect();
    assert!(logs.len() >= 3, "{}", logs.len());
    for log in &logs {
        put_log(&store, &judge, &log.name, &log.bytes).await;
        put_log(&store, &judge, &log.sidecar_name(), &log.sidecar_bytes()).await;
    }
    engine.idle().await;
    let ids = engine::query::all_cars(&engine).unwrap();
    let car = format!("c{}", ids[0]);
    let (e2, d2) = (engine.clone(), db.clone());
    let s = TestServer::start_with(move |b| b.cars(Arc::new(EngineCars(d2))).service(Arc::new(e2))).await;
    let mut admin = s.admin().await;
    let gate = gate::Gate::memory();

    // all here: nothing missing, a look written down
    assert_eq!(web::resources::system::scan_missing_logs(&s.app, &gate).await, 0);
    let h = ok(&mut admin, &format!("/api/v1/cars/{car}/health")).await;
    assert_eq!(h["missing_logs"]["count"], 0, "{h}");
    assert!(h["missing_logs"]["scanned_ms"].is_u64(), "{h}");
    assert!(!h["reasons"].as_array().unwrap().iter().any(|r| r == "logs_missing"), "{h}");

    // a stored log deleted
    let lost = &logs[1];
    let key = sc.vehicle_key();
    std::fs::remove_file(data.join("can").join(&key).join(&lost.name)).unwrap();
    assert_eq!(web::resources::system::scan_missing_logs(&s.app, &gate).await, 1);
    let h = ok(&mut admin, &format!("/api/v1/cars/{car}/health")).await;
    let m = &h["missing_logs"];
    assert_eq!((m["count"].as_u64(), m["gone"].as_u64()), (Some(1), Some(0)), "{h}");
    assert_eq!(m["asked"], false, "not asked before the car pings: {h}");
    assert!(m["oldest_ms"].is_u64(), "{h}");
    assert!(h["reasons"].as_array().unwrap().iter().any(|r| r == "logs_missing"), "{h}");
    assert_eq!(h["healthy"], false);
    // asked of the car (what its ping does)
    assert_eq!(engine.judge().wanted(&key).await.unwrap(), vec![lost.name.clone()]);
    let h = ok(&mut admin, &format!("/api/v1/cars/{car}/health")).await;
    assert_eq!(h["missing_logs"]["asked"], true, "{h}");

    let o = ok(&mut admin, "/api/v1/integrations/overview").await;
    let rows = o["connections"].as_array().unwrap();
    let row = rows.iter().find(|r| r["id"] == format!("smo:{car}")).unwrap();
    assert_eq!(row["missing_logs"], 1, "{row}");
    assert!(rows.iter().filter(|r| r["kind"] != "smo").all(|r| r["missing_logs"].is_null()));

    let a = ok(&mut admin, "/api/v1/audit?group=cars&limit=100").await;
    let e = a["items"].as_array().unwrap().iter().find(|r| r["action"] == "can-logs-missing").cloned();
    let e = e.unwrap_or_else(|| panic!("no can-logs-missing entry: {a}"));
    assert_eq!(e["car"], car.as_str(), "{e}");
    // a second look finds nothing new: no second entry
    assert_eq!(web::resources::system::scan_missing_logs(&s.app, &gate).await, 0);
    let a = ok(&mut admin, "/api/v1/audit?group=cars&limit=100").await;
    assert_eq!(a["items"].as_array().unwrap().iter().filter(|r| r["action"] == "can-logs-missing").count(), 1);

    // the same bytes again: off the list
    put_log(&store, engine.judge(), &lost.name, &lost.bytes).await;
    let h = ok(&mut admin, &format!("/api/v1/cars/{car}/health")).await;
    assert_eq!(h["missing_logs"]["count"], 0, "{h}");
    assert!(!h["reasons"].as_array().unwrap().iter().any(|r| r == "logs_missing"), "{h}");
}
