//! Logs the server should have but no longer does: the scan finds them (a stored log deleted, a chain gap named by a
//! later log's `prev`), `GET /api/can/logs/ping` asks the car for them as `wanted`, the same bytes sent again take
//! them off the list, other bytes of their name are refused, and the list outlives a restart. Made-up keys and logs.
#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::Engine;
use http_body_util::BodyExt;
use ingest::{Conn, Judge, Store, Tokens, Uploads, router};
use p256::ecdsa::{Signature, SigningKey, signature::Signer};
use p256::pkcs8::EncodePublicKey;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;
use std::sync::Arc;
use tower::ServiceExt;

const KEY: &str = "fedcba654321";
const IP: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 31));

struct T {
    dir: tempfile::TempDir,
    up: Arc<Uploads>,
    token: String,
}

fn data(dir: &tempfile::TempDir) -> PathBuf {
    dir.path().join("data")
}

async fn open(dir: tempfile::TempDir, token: String) -> T {
    let v = Arc::new(vault::Vault::open(&data(&dir), &dir.path().join("keys")).unwrap());
    let tokens = Tokens::open(data(&dir).join("tokens.json"), v.clone()).await.unwrap();
    let store = Store::with_limits(data(&dir).join("can"), v, 0, 200_000, |_| u64::MAX);
    let up = Uploads::new(store.clone(), tokens, Judge::new(store).wait_ms(0));
    T { dir, up, token }
}

async fn setup() -> T {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(data(&dir)).unwrap();
    let (v, _) = vault::Vault::create(&data(&dir), &dir.path().join("keys"), vault::Kdf::FAST_FOR_TESTS).unwrap();
    let tokens = Tokens::open(data(&dir).join("tokens.json"), Arc::new(v)).await.unwrap();
    let (token, _) = tokens.create("Model 3").await.unwrap();
    drop(tokens);
    open(dir, token).await
}

/// The same data folder opened again (a restart).
async fn restart(t: T) -> T {
    let T { dir, up, token } = t;
    drop(up);
    open(dir, token).await
}

async fn call(t: &T, method: &str, path: &str, headers: &[(&str, String)], body: Vec<u8>) -> (StatusCode, Value) {
    let mut b = Request::builder().method(method).uri(path).header("authorization", format!("Bearer {}", t.token));
    for (k, v) in headers {
        b = b.header(*k, v);
    }
    let mut req = b.body(Body::from(body)).unwrap();
    req.extensions_mut().insert(Conn { tls: true, ip: IP });
    let res = router(t.up.clone()).oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

async fn put(t: &T, name: &str, bytes: &[u8]) -> (StatusCode, Value) {
    let h = [("upload-offset", "0".into()), ("upload-length", bytes.len().to_string()), ("upload-sha256", sha(bytes))];
    call(t, "POST", &format!("/api/can/logs/{name}"), &h, bytes.to_vec()).await
}

async fn wanted(t: &T) -> Vec<String> {
    let (s, b) = call(t, "GET", "/api/can/logs/ping", &[], vec![]).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    serde_json::from_value(b["wanted"].clone()).unwrap()
}

async fn status(t: &T, body: Value) -> Value {
    let body = serde_json::to_vec(&body).unwrap();
    let (s, b) = call(t, "POST", "/api/can/logs/status", &[("content-type", "application/json".into())], body).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    b
}

async fn scan(t: &T) -> ingest::missing::Scan {
    let uploading = t.up.store.uploading(KEY).await;
    t.up.judge.scan_missing(KEY, uploading).await.unwrap()
}

fn name(i: u64) -> String {
    format!("{KEY}_20261121T{:02}{:02}00Z_{i:04}.log.xz", 8 + i / 60, i % 60)
}

fn sha(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}

fn log_bytes(i: u64) -> Vec<u8> {
    format!("made-up CAN log {i} for the missing-logs scan\n").repeat(16).into_bytes()
}

fn car_key() -> SigningKey {
    SigningKey::from_slice(&Sha256::digest(b"made-up bridge key for missing logs")).unwrap()
}

/// The sidecar signing log `i` after log `i - 1` (whether or not that one is ever sent).
fn sidecar(i: u64) -> Vec<u8> {
    let key = car_key();
    let n = name(i);
    let prev =
        if i == 0 { ("-".to_string(), trust::GENESIS_SHA.to_string()) } else { (name(i - 1), sha(&log_bytes(i - 1))) };
    let b64 = base64::engine::general_purpose::STANDARD;
    let der = key.verifying_key().to_public_key_der().unwrap().as_bytes().to_vec();
    let bytes = log_bytes(i);
    let msg = trust::signed_message(KEY, &n, &sha(&bytes), bytes.len() as u64, i, &prev.0, &prev.1);
    let sig: Signature = key.sign(msg.as_bytes());
    serde_json::to_vec(&json!({ "format": "canbridge-columns-xz/2", "integrity": {
        "signed": true, "version": trust::VERSION, "algorithm": trust::ALGORITHM, "public_key": b64.encode(&der),
        "key_fingerprint": trust::fingerprint(&der), "vehicle_key": KEY, "sha256": sha(&bytes), "length": bytes.len(),
        "index": i, "prev_name": prev.0, "prev_sha256": prev.1, "signature": b64.encode(sig.to_der().as_bytes()) } }))
    .unwrap()
}

/// Log `i` with its sidecar, judged.
async fn send(t: &T, i: u64) {
    assert_eq!(put(t, &format!("{}.json", name(i)), &sidecar(i)).await.0, StatusCode::CREATED);
    let (s, b) = put(t, &name(i), &log_bytes(i)).await;
    assert_eq!(s, StatusCode::CREATED, "{b}");
}

/// Deletes a stored file, as a disk mishap or a careless hand would.
fn delete(t: &T, file: &str) {
    std::fs::remove_file(data(&t.dir).join("can").join(KEY).join(file)).unwrap();
}

#[tokio::test]
async fn nothing_is_wanted_while_every_log_is_here() {
    let t = setup().await;
    for i in 0..3 {
        send(&t, i).await;
    }
    let s = scan(&t).await;
    assert_eq!((s.new.len(), s.wanted, s.gone), (0, 0, 0));
    assert_eq!(wanted(&t).await, Vec::<String>::new());
    let m = t.up.judge.missing(KEY).await.unwrap();
    assert!(m.logs.is_empty() && m.scanned_ms.is_some(), "the look is written down: {m:?}");
    assert_eq!(t.up.judge.verify_stored(&name(1)).await.unwrap(), Some(true));
}

#[tokio::test]
async fn a_deleted_log_is_found_asked_for_and_taken_back_only_as_it_was() {
    let t = setup().await;
    for i in 0..3 {
        send(&t, i).await;
    }
    delete(&t, &name(1));
    // not asked for before the scan finds it
    assert!(wanted(&t).await.is_empty());
    let s = scan(&t).await;
    assert_eq!(s.new, vec![name(1)]);
    assert_eq!((s.wanted, s.gone), (1, 0));
    assert_eq!(wanted(&t).await, vec![name(1)]);
    let m = t.up.judge.missing(KEY).await.unwrap();
    let l = &m.logs[&name(1)];
    assert_eq!(l.sha256.as_deref(), Some(sha(&log_bytes(1)).as_str()));
    assert_eq!(l.length, Some(log_bytes(1).len() as u64));
    assert_eq!(l.verdict, Some(ingest::judge::Status::Verified));
    assert!(l.asked_ms.is_some(), "the ping that listed it is written down");
    // a scan again finds nothing new and keeps when it was found
    let again = scan(&t).await;
    assert!(again.new.is_empty());
    assert_eq!(t.up.judge.missing(KEY).await.unwrap().logs[&name(1)].found_ms, l.found_ms);

    // other bytes under its name: refused
    let (s, b) = put(&t, &name(1), b"made-up other bytes under the lost log's name").await;
    assert_eq!(s, StatusCode::CONFLICT, "{b}");
    assert_eq!(b["error"]["code"].as_str().or(b["code"].as_str()), Some("wanted_mismatch"), "{b}");
    assert_eq!(wanted(&t).await, vec![name(1)]);

    // the same bytes again: back, judged, off the list
    let (s, b) = put(&t, &name(1), &log_bytes(1)).await;
    assert_eq!(s, StatusCode::CREATED, "{b}");
    assert!(wanted(&t).await.is_empty());
    let f = status(&t, json!({ "names": [name(1)] })).await;
    assert_eq!(f["files"][name(1)]["check"], json!("verified"), "{f}");
    assert!(t.up.judge.missing(KEY).await.unwrap().logs.is_empty());
}

#[tokio::test]
async fn a_lost_log_and_sidecar_are_both_wanted_oldest_first() {
    let t = setup().await;
    for i in 0..4 {
        send(&t, i).await;
    }
    delete(&t, &name(2));
    delete(&t, &format!("{}.json", name(2)));
    delete(&t, &name(1));
    scan(&t).await;
    assert_eq!(wanted(&t).await, vec![name(1), name(2), format!("{}.json", name(2))]);
    // the log alone back is not enough: it waits for its sidecar
    assert_eq!(put(&t, &name(2), &log_bytes(2)).await.0, StatusCode::CREATED);
    assert_eq!(put(&t, &format!("{}.json", name(2)), &sidecar(2)).await.0, StatusCode::CREATED);
    assert_eq!(wanted(&t).await, vec![name(1)]);
}

#[tokio::test]
async fn a_chain_gap_named_by_a_later_log_is_wanted() {
    let t = setup().await;
    send(&t, 0).await;
    send(&t, 1).await;
    // log 2 never came; log 3 names it as the one before
    send(&t, 3).await;
    let s = scan(&t).await;
    assert_eq!(s.new, vec![name(2)]);
    assert_eq!(wanted(&t).await, vec![name(2)]);
    let m = t.up.judge.missing(KEY).await.unwrap();
    assert_eq!(m.logs[&name(2)].known, ingest::missing::Known::Chain);
    assert_eq!(m.logs[&name(2)].sha256.as_deref(), Some(sha(&log_bytes(2)).as_str()));
    // other bytes refused, the signed ones taken
    assert_eq!(put(&t, &name(2), b"made-up bytes that are not log 2").await.0, StatusCode::CONFLICT);
    send(&t, 2).await;
    assert!(wanted(&t).await.is_empty());
    let f = status(&t, json!({ "names": [name(3)] })).await;
    assert_eq!(f["files"][name(3)]["check"], json!("verified"), "{f}");
}

#[tokio::test]
async fn the_first_log_of_a_chain_received_from_its_middle_is_no_gap() {
    let t = setup().await;
    send(&t, 5).await;
    send(&t, 6).await;
    scan(&t).await;
    assert!(wanted(&t).await.is_empty(), "log 4 was never sent here: nothing to ask for");
}

#[tokio::test]
async fn the_list_outlives_a_restart_and_the_car_can_say_it_no_longer_has_one() {
    let t = setup().await;
    for i in 0..3 {
        send(&t, i).await;
    }
    delete(&t, &name(0));
    delete(&t, &name(2));
    scan(&t).await;
    let t = restart(t).await;
    assert_eq!(wanted(&t).await, vec![name(0), name(2)]);
    // the app asks about log 2 (it holds it) and says it no longer has log 0
    status(&t, json!({ "names": [name(2)], "not_held": [name(0)] })).await;
    assert_eq!(wanted(&t).await, vec![name(2)]);
    let t = restart(t).await;
    let m = t.up.judge.missing(KEY).await.unwrap();
    assert!(m.logs[&name(0)].gone_ms.is_some());
    assert!(m.logs[&name(2)].held_ms.is_some());
    let c = m.counts();
    assert_eq!((c.wanted, c.gone), (1, 1));
    assert_eq!(wanted(&t).await, vec![name(2)]);
    // a scan keeps what the car said
    let s = scan(&t).await;
    assert_eq!((s.wanted, s.gone), (1, 1));
}
