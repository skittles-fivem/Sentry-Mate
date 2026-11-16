#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! The upload side with many cars: every car's state is per car on disk, memory stays within the caches' sizes
//! whatever the number of cars, and an upload never reads a car's whole folder. All data here is synthetic.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use ingest::{Conn, Judge, Store, Tokens, Uploads, router};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tower::ServiceExt;

const IP: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 20));
const CACHE: usize = 64;

struct T {
    _dir: tempfile::TempDir,
    data: PathBuf,
    vault: Arc<vault::Vault>,
}

fn vault_dir() -> T {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let (v, _) = vault::Vault::create(&data, &dir.path().join("keys"), vault::Kdf::FAST_FOR_TESTS).unwrap();
    T { _dir: dir, data, vault: Arc::new(v) }
}

/// The upload side with every cache at [`CACHE`] but the store's log counts at `store_cache`.
async fn uploads(t: &T, max_logs: usize, wait_ms: u64, store_cache: usize) -> Arc<Uploads> {
    let tokens = Tokens::open(t.data.join("tokens.json"), t.vault.clone()).await.unwrap().cache_size(CACHE);
    let store =
        Store::with_limits(t.data.join("can"), t.vault.clone(), 0, max_logs, |_| u64::MAX).cache_size(store_cache);
    let judge = Judge::new(store.clone()).cache_size(CACHE).wait_ms(wait_ms);
    Uploads::new(store, tokens, judge)
}

async fn post(
    up: &Arc<Uploads>,
    path: &str,
    token: &str,
    headers: &[(&str, String)],
    body: Vec<u8>,
) -> (StatusCode, Value) {
    let mut b = Request::builder().method("POST").uri(path).header("authorization", format!("Bearer {token}"));
    for (k, v) in headers {
        b = b.header(*k, v);
    }
    let mut req = b.body(Body::from(body)).unwrap();
    req.extensions_mut().insert(Conn { tls: true, ip: IP });
    let res = router(up.clone()).oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

async fn put(up: &Arc<Uploads>, token: &str, name: &str, bytes: &[u8]) -> (StatusCode, Value) {
    let sha = hex::encode(Sha256::digest(bytes));
    let h = [("upload-offset", "0".to_string()), ("upload-length", bytes.len().to_string()), ("upload-sha256", sha)];
    post(up, &format!("/api/can/logs/{name}"), token, &h, bytes.to_vec()).await
}

/// A few KB that look like a log (not a real one: nothing here is real data).
fn fake_log(seed: u32) -> Vec<u8> {
    let mut x = seed.wrapping_mul(2_654_435_761).wrapping_add(1);
    (0..4096)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x as u8
        })
        .collect()
}

fn files_in(dir: &Path) -> usize {
    std::fs::read_dir(dir)
        .map(|rd| rd.flatten().filter(|e| !e.file_name().to_string_lossy().starts_with('.')).count())
        .unwrap_or(0)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_thousand_cars_each_with_a_token_upload_and_memory_stays_bounded() {
    const CARS: u32 = 2000;
    let t = vault_dir();
    // the store keeps only 4 cars' counts: with 32 uploading at once they are pushed out mid-upload all the time
    let up = uploads(&t, 200_000, 300, 4).await;
    let started = Instant::now();
    let mut tokens = Vec::new();
    for i in 0..CARS {
        let (tok, _) = up.tokens.create(&format!("Synthetic car {i}")).await.unwrap();
        tokens.push(tok);
    }
    let made = started.elapsed();

    // every car uploads its log through the router, 32 at once; even cars send their sidecar too
    let started = Instant::now();
    let gate = Arc::new(tokio::sync::Semaphore::new(32));
    let mut tasks = tokio::task::JoinSet::new();
    for (i, tok) in tokens.iter().enumerate() {
        let (up, tok, gate) = (up.clone(), tok.clone(), gate.clone());
        tasks.spawn(async move {
            let _slot = gate.acquire_owned().await.unwrap();
            let name = format!("car{i:05}_20261108T143000Z_1.log.xz");
            let log = fake_log(i as u32);
            let (s, b) = put(&up, &tok, &name, &log).await;
            assert_eq!(s, StatusCode::CREATED, "car {i}: {b}");
            if i % 2 == 0 {
                let side = serde_json::to_vec(&serde_json::json!({ "format": "canbridge-columns-xz/2" })).unwrap();
                let (s, b) = put(&up, &tok, &format!("{name}.json"), &side).await;
                assert_eq!(s, StatusCode::CREATED, "car {i}: {b}");
            }
        });
    }
    while let Some(r) = tasks.join_next().await {
        r.unwrap();
    }
    let uploaded = started.elapsed();

    let m = up.memory();
    assert!(m.trust_heads <= CACHE, "{m:?}");
    assert!(m.store_cars <= 4, "{m:?}");
    assert!(m.tokens <= CACHE, "{m:?}");
    assert!(m.trust_locks <= 200 && m.store_locks <= 200, "lock tables follow the cars in use now: {m:?}");
    assert_eq!(m.token_windows, CARS as usize, "one rate window per token used in the last ten minutes");
    assert_eq!(m.bad_token_addresses, 0);
    assert_eq!(up.store.folder_scans(), 0, "no upload listed a car's folder, even with its count pushed out");

    // each car's records are its own: a car's token is tied to its key, and its record is there
    for i in [0u32, 1, 999, 1998, 1999] {
        let key = format!("car{i:05}");
        let name = format!("{key}_20261108T143000Z_1.log.xz");
        let r = up.judge.record(&key, &name).await.unwrap();
        let want = if i % 2 == 0 { ingest::judge::Status::Unsigned } else { ingest::judge::Status::Waiting };
        assert_eq!(r.status, want, "car {i}");
        assert_eq!(up.store.log_count(&key).await.unwrap(), 1);
        let (s, b) =
            put(&up, &tokens[i as usize], &format!("car{:05}_20261108T143000Z_2.log.xz", (i + 1) % CARS), b"x").await;
        assert_eq!((s, b["code"].as_str()), (StatusCode::FORBIDDEN, Some("wrong_car")));
    }
    let waiting_dir = t.data.join("can/.waiting");
    assert_eq!(files_in(&waiting_dir), (CARS / 2) as usize, "only cars with a log waiting are marked");

    // the sweep looks only at the marked cars and judges their logs once the wait is over
    tokio::time::sleep(Duration::from_millis(400)).await;
    let started = Instant::now();
    assert_eq!(up.judge.sweep().await.unwrap(), (CARS / 2) as usize);
    let swept = started.elapsed();
    assert_eq!(files_in(&waiting_dir), 0);
    assert!(up.memory().trust_heads <= CACHE);
    let r = up.judge.record("car00001", "car00001_20261108T143000Z_1.log.xz").await.unwrap();
    assert_eq!(r.status, ingest::judge::Status::Unsigned);
    let started = Instant::now();
    assert_eq!(up.judge.sweep().await.unwrap(), 0);
    let idle_sweep = started.elapsed();
    let started = Instant::now();
    assert_eq!(up.store.clean_stale(ingest::store::STALE_PARTIAL_MS).await, 0);
    let cleaned = started.elapsed();
    eprintln!(
        "{CARS} cars: tokens made in {made:?}; logs (+ half the sidecars) uploaded in {uploaded:?} ({:?} a car); \
         sweep judged {} in {swept:?}, an idle sweep took {idle_sweep:?}, clean_stale {cleaned:?}; memory {:?}",
        uploaded / CARS,
        CARS / 2,
        up.memory()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_car_with_fifty_thousand_logs_uploads_without_its_folder_being_read() {
    const LOGS: usize = 50_000;
    let t = vault_dir();
    // a car stored before counts were kept: 50,000 logs in its folder (empty stand-ins: only their names count)
    let car = t.data.join("can/bigcar");
    std::fs::create_dir_all(&car).unwrap();
    for i in 0..LOGS {
        std::fs::File::create(car.join(format!("bigcar_20261108T143000Z_{i}.log.xz"))).unwrap();
    }
    {
        let up = uploads(&t, LOGS + 2, 1000, CACHE).await;
        assert_eq!(up.store.log_count("bigcar").await.unwrap(), LOGS as u64);
        assert_eq!(up.store.folder_scans(), 1, "counted once, then written down");
    }
    assert!(car.join("counts.json").exists());

    // a restart: nothing in memory; uploads now read the count, never the folder
    let up = uploads(&t, LOGS + 2, 1000, CACHE).await;
    let (token, _) = up.tokens.create("Big car").await.unwrap();
    let started = Instant::now();
    let (s, b) = put(&up, &token, "bigcar_20261109T000000Z_1.log.xz", &fake_log(1)).await;
    let first = started.elapsed();
    assert_eq!(s, StatusCode::CREATED, "{b}");
    let (s, _) = put(&up, &token, "bigcar_20261109T000100Z_2.log.xz", &fake_log(2)).await;
    assert_eq!(s, StatusCode::CREATED);
    let (s, b) = put(&up, &token, "bigcar_20261109T000200Z_3.log.xz", &fake_log(3)).await;
    assert_eq!((s, b["code"].as_str()), (StatusCode::INSUFFICIENT_STORAGE, Some("car_full")), "the cap still holds");
    assert_eq!(up.store.folder_scans(), 0, "no upload listed the car's 50,000 files");
    assert_eq!(up.store.log_count("bigcar").await.unwrap(), LOGS as u64 + 2);
    // the sidecar of a full car is still taken (only logs count)
    let side = br#"{"format":"canbridge-columns-xz/2"}"#;
    let (s, _) = put(&up, &token, "bigcar_20261109T000100Z_2.log.xz.json", side).await;
    assert_eq!(s, StatusCode::CREATED);

    // the same upload for a car with nothing stored, to compare
    let (small, _) = up.tokens.create("Small car").await.unwrap();
    let started = Instant::now();
    let (s, _) = put(&up, &small, "smallcar_20261109T000000Z_1.log.xz", &fake_log(1)).await;
    let fresh = started.elapsed();
    assert_eq!(s, StatusCode::CREATED);

    // clean_stale looks only in uploading/ folders: a stale upload is dropped, the 50,000 logs aren't looked at
    let up_dir = car.join("uploading");
    std::fs::create_dir_all(&up_dir).unwrap();
    for f in ["bigcar_20261101T000000Z_9.log.xz.part", "bigcar_20261101T000000Z_9.log.xz.part.json"] {
        let file = std::fs::File::create(up_dir.join(f)).unwrap();
        file.set_modified(std::time::SystemTime::now() - Duration::from_secs(3 * 24 * 3600)).unwrap();
    }
    let started = Instant::now();
    assert_eq!(up.store.clean_stale(ingest::store::STALE_PARTIAL_MS).await, 1);
    let cleaned = started.elapsed();
    assert_eq!(files_in(&up_dir), 0);
    assert_eq!(files_in(&car), LOGS + 2 + 1 + 1 + 1 + 1, "logs, a sidecar, counts.json, uploading/, trust journal");
    eprintln!("a car with {LOGS} logs: an upload took {first:?} (a new car: {fresh:?}); clean_stale {cleaned:?}");
}
