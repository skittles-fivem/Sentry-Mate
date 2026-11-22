#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! The answers that decode stored CAN logs (`/signals`, `/signals/latest`, the battery pages) run a few at a time: a
//! watch account firing them without end can't keep every core busy, nor queue more than a few of its own.

use engine::Engine;
use model::CarId;
use sha2::{Digest, Sha256};
use std::sync::Arc;
use std::time::Duration;
use web::testkit::TestServer;

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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn log_decoding_answers_take_turns() {
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
    let sc = synth::Scenario::new(11).days(1).rates(synth::Rates::Light);
    for log in sc.logs().take(30) {
        put(&store, &judge, &log.name, &log.bytes).await;
        put(&store, &judge, &log.sidecar_name(), &log.sidecar_bytes()).await;
    }
    engine.idle().await;
    let car_id = engine::query::all_cars(&engine).unwrap()[0];
    let car = format!("c{car_id}");
    let (e2, d2) = (engine.clone(), db.clone());
    let s = TestServer::start_with(move |b| {
        b.cars(Arc::new(EngineCars(d2))).service(Arc::new(e2)).log_queries(1, Duration::from_secs(6))
    })
    .await;
    let mut admin = s.admin().await;
    let watch = s.watch(&mut admin, &[car_id]).await;
    let as_watch = || {
        let mut c = s.client();
        c.cookies = watch.cookies.clone();
        c
    };
    let as_admin = || {
        let mut c = s.client();
        c.cookies = admin.cookies.clone();
        c
    };
    let signals = format!("/api/v1/cars/{car}/signals?series=DI_vehicleSpeed&from_ms=0&to_ms=4102444799999");
    let mut c = as_watch();
    let r = c.get(&signals).await;
    assert_eq!(r.status, 200, "{}", r.text());

    // the server's one turn is someone else's: the watch account waits, then is told the server is busy
    let held = s.app.heavy.turn(u64::MAX).await.unwrap();
    let paths = [
        signals.clone(),
        format!("/api/v1/cars/{car}/signals/latest"),
        format!("/api/v1/cars/{car}/battery"),
        format!("/api/v1/cars/{car}/battery/capacity"),
        format!("/api/v1/cars/{car}/battery-level"),
        format!("/api/v1/cars/{car}/projected-range"),
        format!("/api/v1/cars/{car}/efficiency"),
    ];
    let mut tries = Vec::new();
    for (i, path) in paths.into_iter().enumerate() {
        // four for the watch account, the rest for the admin (each may have four waiting)
        let mut c = if i < 4 { as_watch() } else { as_admin() };
        tries.push(tokio::spawn(async move {
            let r = c.get(&path).await;
            (path, r.status, r.code(), r.header("retry-after").map(str::to_string))
        }));
    }
    for t in tries {
        let (path, status, code, retry) = t.await.unwrap();
        assert_eq!((status, code.as_deref(), retry.as_deref()), (503, Some("busy"), Some("5")), "{path}");
    }

    // one account queues a few, and the next one is refused at once
    let mut waiting = Vec::new();
    for _ in 0..web::heavy::PER_ACCOUNT {
        let (mut c, path) = (as_watch(), signals.clone());
        waiting.push(tokio::spawn(async move { c.get(&path).await.status }));
    }
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let t = std::time::Instant::now();
    let r = c.get(&signals).await;
    assert_eq!((r.status, r.code().as_deref()), (429, Some("throttled")), "{}", r.text());
    assert!(t.elapsed() < Duration::from_millis(1500), "refused without waiting");
    // the turn comes free: the ones waiting are answered, one after the other
    drop(held);
    for w in waiting {
        assert_eq!(w.await.unwrap(), 200);
    }
    // an admin's other pages never wait for these
    let r = as_admin().get(&format!("/api/v1/cars/{car}/now")).await;
    assert_eq!(r.status, 200);
    assert_eq!(as_admin().get(&signals).await.status, 200);
    assert_eq!(s.app.heavy.running(), 0);
}
