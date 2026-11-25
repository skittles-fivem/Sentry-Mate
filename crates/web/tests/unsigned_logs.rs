#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! An owner's unsigned CAN logs of a paired car (made by a CAN Bridge app from before it signed logs, sent after its
//! first signed log paired the car): listed with why each is unsigned, and used on an admin's word, their password
//! asked again, each change in the audit log; a log with a bad signature is never used. The car is a made-up one
//! (synth: a made-up VIN, made-up places, a public signing seed).

use engine::Engine;
use model::CarId;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use web::testkit::{Client, TestServer};

struct EngineCars(db::Db);

impl web::CarDirectory for EngineCars {
    fn exists(&self, car: CarId) -> bool {
        self.0.car(car).ok().flatten().is_some()
    }
}

async fn put(store: &ingest::Store, judge: &ingest::Judge, name: &str, bytes: &[u8]) {
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

const ALL: &str = "from_ms=0&to_ms=4102444799999";

async fn drives(c: &mut Client, car: &str) -> Vec<u64> {
    let v = ok(c, &format!("/api/v1/cars/{car}/drives?{ALL}&limit=500")).await;
    v["items"].as_array().unwrap().iter().map(|d| d["start_ms"].as_u64().unwrap()).collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unsigned_logs_of_a_paired_car_are_used_on_the_owners_word_and_bad_ones_never() {
    let sc = synth::Scenario::new(5).days(1).rates(synth::Rates::Light).with_trouble(false);
    let truth = sc.truth();
    assert!(truth.drives.len() >= 2, "{:?}", truth.drives.len());
    // the logs up to the end of the first drive were made before the app signed logs
    let split = truth.drives[0].end_ms + 5 * 60_000;
    let logs: Vec<synth::LogFile> = sc.logs().collect();
    let (early, late): (Vec<_>, Vec<_>) = logs.into_iter().partition(|l| l.start_ms < split);
    assert!(early.len() > 3 && !late.is_empty());

    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let (v, _) = vault::Vault::create(&data, &dir.path().join("keys"), vault::Kdf::FAST_FOR_TESTS).unwrap();
    let v = Arc::new(v);
    let db = db::Db::open(&data, v.clone(), db::Options::default()).unwrap();
    let store = ingest::Store::with_limits(data.join("can"), v, 0, 200_000, |_| u64::MAX);
    let engine = Engine::new(db.clone(), store.clone(), engine::Options { workers: 2, ..engine::Options::default() });
    let judge = ingest::Judge::new(store.clone()).on_judged(engine.listener());
    engine.start().unwrap();
    // the signed ones first: the first of them pairs the car
    for log in &late {
        put(&store, &judge, &log.name, &log.bytes).await;
        put(&store, &judge, &log.sidecar_name(), &log.sidecar_bytes()).await;
    }
    // then the older ones, without a signature; the last keeps a signature, changed (a bad one, after the drive)
    let bad = early.last().unwrap().name.clone();
    for log in &early {
        let mut side = log.sidecar.clone();
        if log.name == bad {
            side["integrity"]["sha256"] = json!("00".repeat(32));
        } else {
            side.as_object_mut().unwrap().remove("integrity");
        }
        put(&store, &judge, &log.name, &log.bytes).await;
        put(&store, &judge, &log.sidecar_name(), &serde_json::to_vec(&side).unwrap()).await;
    }
    engine.idle().await;
    let car_id = engine::query::all_cars(&engine).unwrap()[0];
    let car = format!("c{car_id}");
    let (e2, d2) = (engine.clone(), db.clone());
    let s = TestServer::start_with(move |b| b.cars(Arc::new(EngineCars(d2))).service(Arc::new(e2))).await;
    let mut admin = s.admin().await;
    let mut watch = s.watch(&mut admin, &[car_id]).await;
    let first_drive = truth.drives[0].start_ms;
    let near = |ds: &[u64]| ds.iter().any(|&d| d.abs_diff(first_drive) < 10 * 60_000);
    assert!(!near(&drives(&mut admin, &car).await), "the unsigned logs' drive is not there");

    // the list says why each is unsigned
    let list = ok(&mut admin, &format!("/api/v1/cars/{car}/logs?{ALL}&limit=500")).await;
    let rows = list["items"].as_array().unwrap();
    let row = |name: &str| rows.iter().find(|r| r["name"] == name).cloned().unwrap();
    let r = row(&early[0].name);
    assert_eq!((r["result"].as_str(), r["unsigned_kind"].as_str()), (Some("failed"), Some("no_integrity")), "{r}");
    assert_eq!((r["before_first_signed"].as_bool(), r["override_use"].as_bool()), (Some(true), Some(false)));
    assert_eq!(r["used"], false);
    let r = row(&bad);
    assert!(r["unsigned_kind"].is_null() && r["result"] == "failed", "{r}");
    let r = row(&late[0].name);
    assert!(r["unsigned_kind"].is_null() && r["result"] == "verified" && r["before_first_signed"] == false, "{r}");
    let u = ok(&mut admin, &format!("/api/v1/cars/{car}/logs/unsigned")).await;
    let n = early.len() as u64 - 1;
    assert_eq!((u["can_use"].as_u64(), u["can_use_before_first_signed"].as_u64()), (Some(n), Some(n)), "{u}");
    assert_eq!(
        (u["kinds"]["no_integrity"].as_u64(), u["in_use"].as_u64(), u["never"].as_u64()),
        (Some(n), Some(0), Some(1))
    );
    assert_eq!(u["items"].as_array().unwrap().len() as u64, n);

    let path = format!("/api/v1/cars/{car}/logs/use-unsigned");
    // a watch account may not, nor see the list; a car that isn't there is not found
    assert_eq!(watch.post(&path, json!({ "all": true, "confirm_password": "x" })).await.status, 403);
    assert_eq!(watch.get(&format!("/api/v1/cars/{car}/logs/unsigned")).await.status, 403);
    let r = admin.post("/api/v1/cars/c999/logs/use-unsigned", json!({ "all": true })).await;
    assert_eq!((r.status, r.code().as_deref()), (404, Some("no_such_car")), "{}", r.text());
    // the password is asked again
    let r = admin.post(&path, json!({ "all": true })).await;
    assert_eq!((r.status, r.code().as_deref()), (403, Some("password_needed")), "{}", r.text());
    let r = admin.post(&path, json!({ "all": true, "confirm_password": "not it" })).await;
    assert_eq!((r.status, r.code().as_deref()), (403, Some("wrong_password")), "{}", r.text());
    // names or all, one of them
    let pw = web::testkit::ADMIN_PASSWORD;
    let r = admin.post(&path, json!({ "confirm_password": pw })).await;
    assert_eq!(r.status, 400, "{}", r.text());
    // a bad one among the names: refused, naming it, nothing changes
    let names = vec![early[0].name.clone(), bad.clone()];
    let r = admin.post(&path, json!({ "names": names, "confirm_password": pw })).await;
    assert_eq!((r.status, r.code().as_deref()), (422, Some("invalid")), "{}", r.text());
    assert!(r.json()["error"]["fields"]["names"].as_str().unwrap().contains(&bad), "{}", r.text());
    // a verified one too
    let r = admin.post(&path, json!({ "names": [late[0].name], "confirm_password": pw })).await;
    assert_eq!(r.status, 422, "{}", r.text());
    // a log of another car key
    let r = admin.post(&path, json!({ "names": ["zz9999_20261108T100000Z_1.log.xz"], "confirm_password": pw })).await;
    assert_eq!(r.status, 422, "{}", r.text());
    engine.idle().await;
    assert!(!near(&drives(&mut admin, &car).await));

    // all of them: the drive appears
    let r = admin.post(&path, json!({ "all": true, "before_first_signed": true, "confirm_password": pw })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.json()["changed"].as_u64(), Some(n), "{}", r.text());
    engine.idle().await;
    assert!(near(&drives(&mut admin, &car).await), "the drive of the logs used is there");
    let list = ok(&mut admin, &format!("/api/v1/cars/{car}/logs?{ALL}&limit=500")).await;
    let rows = list["items"].as_array().unwrap();
    let r = rows.iter().find(|r| r["name"] == early[0].name).unwrap();
    assert_eq!(
        (r["override_use"].as_bool(), r["used"].as_bool(), r["decoded"].as_str()),
        (Some(true), Some(true), Some("decoded")),
        "{r}"
    );
    let r = rows.iter().find(|r| r["name"] == bad).unwrap();
    assert_eq!((r["used"].as_bool(), r["decoded"].as_str()), (Some(false), Some("left_out")), "{r}");
    // the audit log names the car and how many
    let a = ok(&mut admin, "/api/v1/audit?limit=50").await;
    let e = a["items"].as_array().unwrap().iter().find(|e| e["action"] == "can-logs-unsigned-used").cloned();
    let e = e.unwrap_or_else(|| panic!("no entry: {a}"));
    assert_eq!(e["car"].as_str(), Some(car.as_str()), "{e}");
    assert!(e["summary"].as_str().unwrap().contains(&format!("{n} unsigned CAN logs")), "{e}");

    // and back
    let r = admin.delete(&path, json!({ "all": true })).await;
    assert_eq!((r.status, r.code().as_deref()), (403, Some("password_needed")));
    let r = admin.delete(&path, json!({ "all": true, "confirm_password": pw })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.json()["changed"].as_u64(), Some(n));
    engine.idle().await;
    assert!(!near(&drives(&mut admin, &car).await), "the drive is gone again");
    let a = ok(&mut admin, "/api/v1/audit?limit=50").await;
    assert!(a["items"].as_array().unwrap().iter().any(|e| e["action"] == "can-logs-unsigned-stopped"), "{a}");
    let u = ok(&mut admin, &format!("/api/v1/cars/{car}/logs/unsigned")).await;
    assert_eq!((u["can_use"].as_u64(), u["in_use"].as_u64()), (Some(n), Some(0)), "{u}");
}
