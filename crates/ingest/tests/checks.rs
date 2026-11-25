//! The `check` of `POST /api/can/logs/status`, a failed log sent again replacing its stored copy, and `maxParallel`.
//! Made-up keys and logs only.
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
use std::sync::Arc;
use std::time::{Duration, Instant};
use tower::ServiceExt;

const KEY: &str = "abcdef123456";
const IP: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 20));

struct T {
    _dir: tempfile::TempDir,
    up: Arc<Uploads>,
    token: String,
}

/// `wait_ms`: how long a log waits for its sidecar, and for the log before it.
async fn setup(wait_ms: u64) -> T {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let (v, _) = vault::Vault::create(&data, &dir.path().join("keys"), vault::Kdf::FAST_FOR_TESTS).unwrap();
    let v = Arc::new(v);
    let tokens = Tokens::open(data.join("tokens.json"), v.clone()).await.unwrap();
    let (token, _) = tokens.create("Model Y").await.unwrap();
    let store = Store::with_limits(data.join("can"), v, 0, 200_000, |_| u64::MAX);
    let up = Uploads::new(store.clone(), tokens, Judge::new(store).wait_ms(wait_ms));
    T { _dir: dir, up, token }
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

/// One chunk of `bytes` at `offset`.
async fn chunk(t: &T, name: &str, bytes: &[u8], offset: usize, piece: &[u8]) -> (StatusCode, Value) {
    let sha = hex::encode(Sha256::digest(bytes));
    let h = [("upload-offset", offset.to_string()), ("upload-length", bytes.len().to_string()), ("upload-sha256", sha)];
    call(t, "POST", &format!("/api/can/logs/{name}"), &h, piece.to_vec()).await
}

async fn put(t: &T, name: &str, bytes: &[u8]) -> (StatusCode, Value) {
    chunk(t, name, bytes, 0, bytes).await
}

async fn status(t: &T, names: &[String]) -> Value {
    let body = serde_json::to_vec(&json!({ "names": names })).unwrap();
    let (s, b) = call(t, "POST", "/api/can/logs/status", &[("content-type", "application/json".into())], body).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    b["files"].clone()
}

async fn check_of(t: &T, name: &str) -> (Option<String>, Option<String>) {
    let f = status(t, &[name.to_string()]).await;
    let s = |k: &str| f[name][k].as_str().map(str::to_string);
    (s("check"), s("check_reason"))
}

fn verified() -> (Option<String>, Option<String>) {
    (Some("verified".into()), None)
}

fn failed(reason: &str) -> (Option<String>, Option<String>) {
    (Some("failed".into()), Some(reason.into()))
}

fn pending() -> (Option<String>, Option<String>) {
    (Some("pending".into()), None)
}

fn name(i: u64) -> String {
    format!("{KEY}_20261122T{:02}{:02}00Z_{i:04}.log.xz", 10 + i / 60, i % 60)
}

fn sha(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}

fn log_bytes(i: u64) -> Vec<u8> {
    format!("made-up CAN log number {i}\n").repeat(20).into_bytes()
}

/// A sidecar signing `signed` (the bytes the bridge wrote) as log `index`, after `prev` (name, SHA-256).
fn sidecar(key: &SigningKey, name: &str, signed: &[u8], index: u64, prev: (&str, &str)) -> Vec<u8> {
    let b64 = base64::engine::general_purpose::STANDARD;
    let der = key.verifying_key().to_public_key_der().unwrap().as_bytes().to_vec();
    let sha = sha(signed);
    let msg = trust::signed_message(KEY, name, &sha, signed.len() as u64, index, prev.0, prev.1);
    let sig: Signature = key.sign(msg.as_bytes());
    serde_json::to_vec(&json!({ "format": "canbridge-columns-xz/2", "integrity": {
        "signed": true, "version": trust::VERSION, "algorithm": trust::ALGORITHM, "public_key": b64.encode(&der),
        "key_fingerprint": trust::fingerprint(&der), "vehicle_key": KEY, "sha256": sha, "length": signed.len(),
        "index": index, "prev_name": prev.0, "prev_sha256": prev.1, "signature": b64.encode(sig.to_der().as_bytes()) } }))
    .unwrap()
}

fn car_key() -> SigningKey {
    SigningKey::from_slice(&Sha256::digest(b"made-up bridge key for checks")).unwrap()
}

/// Log `i` of the chain (signed after log `i - 1`), with its sidecar: sidecar first, then the log `stored`.
async fn send(t: &T, key: &SigningKey, i: u64, stored: &[u8]) {
    let n = name(i);
    let prev =
        if i == 0 { ("-".to_string(), trust::GENESIS_SHA.to_string()) } else { (name(i - 1), sha(&log_bytes(i - 1))) };
    let side = sidecar(key, &n, &log_bytes(i), i, (&prev.0, &prev.1));
    assert_eq!(put(t, &format!("{n}.json"), &side).await.0, StatusCode::CREATED);
    assert_eq!(put(t, &n, stored).await.0, StatusCode::CREATED);
}

#[tokio::test]
async fn ping_says_how_many_uploads_to_send_at_once() {
    let t = setup(60_000).await;
    let (s, b) = call(&t, "GET", "/api/can/logs/ping", &[], vec![]).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b["maxParallel"], json!(16));
    assert_eq!(ingest::server::MAX_PARALLEL_UPLOADS * 2, ingest::server::MAX_PER_ADDRESS);
}

#[tokio::test]
async fn chained_logs_are_verified_and_incomplete_ones_get_no_check() {
    let t = setup(60_000).await;
    let key = car_key();
    for i in 0..3 {
        send(&t, &key, i, &log_bytes(i)).await;
    }
    let names: Vec<String> = (0..3).map(name).chain([format!("{}.json", name(1)), name(7)]).collect();
    let f = status(&t, &names).await;
    for i in 0..3 {
        let n = name(i);
        assert_eq!(f[&n]["check"], json!("verified"), "{f}");
        assert!(f[&n].get("check_reason").is_none(), "{f}");
        assert_eq!(f[&n]["size"], json!(log_bytes(i).len()), "size is the whole file's, beside length: {f}");
        assert_eq!(f[&n]["size"], f[&n]["length"], "{f}");
    }
    assert_eq!(f[format!("{}.json", name(1))]["check"], json!("verified"), "a sidecar has its log's check");
    assert_eq!(
        f[name(7)],
        json!({ "state": "missing", "received": 0, "length": null, "size": 0 }),
        "missing: no check"
    );

    // a partial upload has no check either
    let b = log_bytes(9);
    chunk(&t, &name(9), &b, 0, &b[..10]).await;
    let f = status(&t, &[name(9)]).await;
    assert_eq!((f[name(9)]["state"].as_str(), f[name(9)].get("check")), (Some("partial"), None), "{f}");
}

#[tokio::test]
async fn each_reason_of_a_failed_log() {
    let t = setup(60_000).await;
    let key = car_key();
    send(&t, &key, 0, &log_bytes(0)).await; // pairs the car
    assert_eq!(check_of(&t, &name(0)).await, verified());

    // the file is not the one the sidecar names
    let mut changed = log_bytes(1);
    changed.push(b'!');
    send(&t, &key, 1, &changed).await;
    assert_eq!(check_of(&t, &name(1)).await, failed("mismatch"));

    // another key's signature
    let other = SigningKey::from_slice(&Sha256::digest(b"made-up other key")).unwrap();
    let n2 = name(2);
    let side = sidecar(&other, &n2, &log_bytes(2), 2, (&name(1), &sha(&log_bytes(1))));
    put(&t, &format!("{n2}.json"), &side).await;
    put(&t, &n2, &log_bytes(2)).await;
    assert_eq!(check_of(&t, &n2).await, failed("bad_signature"));

    // a signature that doesn't match its message
    let n3 = name(3);
    let mut side: Value =
        serde_json::from_slice(&sidecar(&key, &n3, &log_bytes(3), 3, (&n2, &sha(&log_bytes(2))))).unwrap();
    let other_sig: Signature = key.sign(b"something else");
    side["integrity"]["signature"] =
        json!(base64::engine::general_purpose::STANDARD.encode(other_sig.to_der().as_bytes()));
    put(&t, &format!("{n3}.json"), &serde_json::to_vec(&side).unwrap()).await;
    put(&t, &n3, &log_bytes(3)).await;
    assert_eq!(check_of(&t, &n3).await, failed("bad_signature"));

    // not signed, from a paired car
    let n4 = name(4);
    put(&t, &format!("{n4}.json"), br#"{"format":"canbridge-columns-xz/2"}"#).await;
    put(&t, &n4, &log_bytes(4)).await;
    assert_eq!(check_of(&t, &n4).await, failed("unsigned"));
    // the bridge said it could not sign
    let n5 = name(5);
    put(&t, &format!("{n5}.json"), br#"{"integrity":{"signed":false,"error":"keystore locked"}}"#).await;
    put(&t, &n5, &log_bytes(5)).await;
    assert_eq!(check_of(&t, &n5).await, failed("unsigned"));

    // signed by the paired key, but naming a log before it that is here, verified, and not the one it names
    let n6 = name(6);
    let side = sidecar(&key, &n6, &log_bytes(6), 1, (&name(0), &sha(b"not log zero")));
    put(&t, &format!("{n6}.json"), &side).await;
    put(&t, &n6, &log_bytes(6)).await;
    assert_eq!(check_of(&t, &n6).await, failed("chain_break"));
    // its signature is fine: the judge's record says verified, the chain does not fit
    assert_eq!(t.up.judge.record(KEY, &n6).await.unwrap().status, ingest::judge::Status::Verified);

    // signed by the paired key and naming the log before it, which failed its own check (could not sign): that is
    // log 5's failure, not this one's
    let n7 = name(7);
    let side = sidecar(&key, &n7, &log_bytes(7), 7, (&n5, &sha(&log_bytes(5))));
    put(&t, &format!("{n7}.json"), &side).await;
    put(&t, &n7, &log_bytes(7)).await;
    assert_eq!(check_of(&t, &n7).await, verified());
}

#[tokio::test]
async fn an_unsigned_log_of_a_car_not_paired_is_unsigned() {
    let t = setup(60_000).await;
    let n = name(0);
    put(&t, &format!("{n}.json"), br#"{"format":"canbridge-columns-xz/2"}"#).await;
    put(&t, &n, &log_bytes(0)).await;
    assert_eq!(check_of(&t, &n).await, failed("unsigned"));
}

#[tokio::test]
async fn not_judged_yet_is_pending() {
    let t = setup(60_000).await;
    let key = car_key();
    send(&t, &key, 0, &log_bytes(0)).await;
    // a log whose sidecar hasn't come
    put(&t, &name(1), &log_bytes(1)).await;
    assert_eq!(check_of(&t, &name(1)).await, pending());
    // a sidecar whose log hasn't come
    let side = sidecar(&key, &name(5), &log_bytes(5), 5, (&name(4), &sha(&log_bytes(4))));
    put(&t, &format!("{}.json", name(5)), &side).await;
    assert_eq!(check_of(&t, &format!("{}.json", name(5))).await, pending());
    // a log whose previous log is still arriving
    let b2 = log_bytes(2);
    let side2 = sidecar(&key, &name(3), &log_bytes(3), 3, (&name(2), &sha(&b2)));
    chunk(&t, &name(2), &b2, 0, &b2[..10]).await;
    put(&t, &format!("{}.json", name(3)), &side2).await;
    put(&t, &name(3), &log_bytes(3)).await;
    assert_eq!(t.up.judge.record(KEY, &name(3)).await.unwrap().status, ingest::judge::Status::Verified);
    assert_eq!(check_of(&t, &name(3)).await, pending(), "the log before it is on its way");
    // and one whose previous log hasn't come at all, while older logs of its chain are here: pending for the wait
    send(&t, &key, 6, &log_bytes(6)).await;
    assert_eq!(check_of(&t, &name(6)).await, pending());
}

#[tokio::test]
async fn a_missing_link_is_a_gap_not_a_failure_once_the_wait_is_over() {
    let t = setup(0).await;
    let key = car_key();
    // a chain received from its middle: its first log here names one never sent here
    send(&t, &key, 4, &log_bytes(4)).await;
    send(&t, &key, 5, &log_bytes(5)).await;
    // log 6 never comes
    send(&t, &key, 7, &log_bytes(7)).await;
    let f = status(&t, &[name(4), name(5), name(7)]).await;
    assert_eq!(f[name(4)]["check"], json!("verified"), "{f}");
    assert_eq!(f[name(5)]["check"], json!("verified"), "{f}");
    // its own signature and file are fine: verified, starting a new segment; the chain report names the gap
    assert_eq!(f[name(7)]["check"], json!("verified"), "{f}");
    assert!(f[name(7)].get("check_reason").is_none(), "{f}");
    let chain = t.up.judge.chain(KEY).await;
    assert!(
        chain.report.issues.iter().any(|i| matches!(i, trust::ChainIssue::Gap { from: 6, to: 6, .. })),
        "{chain:?}"
    );
    // log 6 arrives after all: 7 fits it
    send(&t, &key, 6, &log_bytes(6)).await;
    assert_eq!(check_of(&t, &name(7)).await, verified());
}

#[tokio::test]
async fn five_hundred_names_in_one_call_are_quick() {
    let t = setup(60_000).await;
    let key = car_key();
    for i in 0..500 {
        send(&t, &key, i, &log_bytes(i)).await;
    }
    let names: Vec<String> = (0..500).map(name).collect();
    let began = Instant::now();
    let f = status(&t, &names).await;
    let took = began.elapsed();
    assert_eq!(f.as_object().unwrap().len(), 500);
    assert!(names.iter().all(|n| f[n]["check"] == json!("verified")), "{f}");
    assert!(took < Duration::from_secs(2), "500 names took {took:?}");
}

fn quarantined_bytes(q: Option<zeroize::Zeroizing<Vec<u8>>>) -> Option<Vec<u8>> {
    q.map(|b| b.to_vec())
}

#[tokio::test]
async fn a_failed_log_sent_again_replaces_its_copy_and_a_verified_one_is_never_overwritten() {
    let t = setup(60_000).await;
    let key = car_key();
    send(&t, &key, 0, &log_bytes(0)).await;
    let mut bad = log_bytes(1);
    bad[3] ^= 1;
    send(&t, &key, 1, &bad).await;
    send(&t, &key, 2, &log_bytes(2)).await;
    let f = status(&t, &[name(1), name(2)]).await;
    assert_eq!((&f[name(1)]["check"], &f[name(1)]["check_reason"]), (&json!("failed"), &json!("mismatch")), "{f}");
    assert_eq!(f[name(2)]["check"], json!("verified"), "the log after a failed one is not failed for it: {f}");
    let count = t.up.store.log_count(KEY).await.unwrap();

    // another bad copy: it takes the place of the first, which is kept aside; still failed
    let mut bad2 = log_bytes(1);
    bad2[5] ^= 1;
    assert_eq!(put(&t, &name(1), &bad2).await.0, StatusCode::CREATED);
    assert_eq!(check_of(&t, &name(1)).await, failed("mismatch"));
    assert_eq!(t.up.store.quarantined(KEY).await, vec![name(1)]);
    assert_eq!(quarantined_bytes(t.up.store.read_quarantined(&name(1)).await.unwrap()), Some(bad.clone()));

    // the right copy, in two chunks: until its last byte the stored copy stays as it is
    let good = log_bytes(1);
    let (s, b) = chunk(&t, &name(1), &good, 0, &good[..100]).await;
    assert_eq!((s, b["received"].as_u64()), (StatusCode::OK, Some(100)), "{b}");
    let f = status(&t, &[name(1)]).await;
    assert_eq!((&f[name(1)]["state"], &f[name(1)]["check"]), (&json!("complete"), &json!("failed")), "{f}");
    let (s, _) = chunk(&t, &name(1), &good, 100, &good[100..]).await;
    assert_eq!(s, StatusCode::CREATED);
    assert_eq!(t.up.store.read(&name(1)).await.unwrap().unwrap().to_vec(), good);
    let f = status(&t, &[name(0), name(1), name(2)]).await;
    for i in 0..3 {
        assert_eq!(f[name(i)]["check"], json!("verified"), "the replacement verified and the next log fits: {f}");
    }
    assert!(t.up.store.quarantined(KEY).await.is_empty(), "the bad copy goes once the new one verifies");
    assert_eq!(t.up.store.log_count(KEY).await.unwrap(), count, "a replacement is not a new log");

    // a verified log is never overwritten; the same bytes again are taken as before
    let (s, b) = put(&t, &name(1), &bad).await;
    assert_eq!((s, b["code"].as_str()), (StatusCode::CONFLICT, Some("exists_different")), "{b}");
    let (s, b) = put(&t, &name(1), &good).await;
    assert_eq!((s, b["complete"].as_bool()), (StatusCode::CREATED, Some(true)), "{b}");
    assert_eq!(t.up.store.read(&name(1)).await.unwrap().unwrap().to_vec(), good);
}

#[tokio::test]
async fn a_failed_sidecar_is_replaced_too_and_a_pending_log_is_not() {
    let t = setup(60_000).await;
    let key = car_key();
    send(&t, &key, 0, &log_bytes(0)).await;
    // a sidecar whose signature was damaged: the log is fine
    let n = name(1);
    let good_side = sidecar(&key, &n, &log_bytes(1), 1, (&name(0), &sha(&log_bytes(0))));
    let mut bad_side: Value = serde_json::from_slice(&good_side).unwrap();
    bad_side["integrity"]["signature"] = json!("AAAA");
    put(&t, &format!("{n}.json"), &serde_json::to_vec(&bad_side).unwrap()).await;
    put(&t, &n, &log_bytes(1)).await;
    assert_eq!(check_of(&t, &n).await, failed("bad_signature"));
    // "Resend all": the log again (the same bytes) and its sidecar (now right)
    assert_eq!(put(&t, &n, &log_bytes(1)).await.0, StatusCode::CREATED);
    assert_eq!(put(&t, &format!("{n}.json"), &good_side).await.0, StatusCode::CREATED);
    assert_eq!(check_of(&t, &n).await, verified());
    assert!(t.up.store.quarantined(KEY).await.is_empty());

    // a log still waiting for its sidecar is not replaced
    let n2 = name(2);
    put(&t, &n2, &log_bytes(2)).await;
    assert_eq!(check_of(&t, &n2).await, pending());
    let (s, b) = put(&t, &n2, b"another file").await;
    assert_eq!((s, b["code"].as_str()), (StatusCode::CONFLICT, Some("exists_different")), "{b}");
}

/// The checks of logs `0..n`, as words (`verified`, `pending`, or `failed:<reason>`).
async fn words(t: &T, n: u64) -> Vec<String> {
    let names: Vec<String> = (0..n).map(name).collect();
    let f = status(t, &names).await;
    names
        .iter()
        .map(|k| match (f[k]["check"].as_str(), f[k]["check_reason"].as_str()) {
            (Some("failed"), Some(r)) => format!("failed:{r}"),
            (Some(c), _) => c.to_string(),
            (None, _) => format!("{}", f[k]["state"]),
        })
        .collect()
}

#[tokio::test]
async fn a_chain_with_one_bad_log_fails_that_log_alone() {
    // no wait: a log that is missing is missing for good at once
    let t = setup(0).await;
    let key = car_key();
    for i in 0..12 {
        match i {
            // changed after signing: a mismatch
            3 => {
                let mut b = log_bytes(3);
                b[2] ^= 1;
                send(&t, &key, 3, &b).await;
            }
            // never sent (the car no longer has it): a gap, never replaced
            6 => {}
            // not signed though the car is paired
            9 => {
                put(&t, &format!("{}.json", name(9)), br#"{"format":"canbridge-columns-xz/2"}"#).await;
                put(&t, &name(9), &log_bytes(9)).await;
            }
            _ => send(&t, &key, i, &log_bytes(i)).await,
        }
    }
    let w = words(&t, 12).await;
    let mut want = vec!["verified".to_string(); 12];
    want[3] = "failed:mismatch".into();
    want[6] = "\"missing\"".into();
    want[9] = "failed:unsigned".into();
    assert_eq!(w, want, "only the bad logs fail; the ones after them start new segments");

    // the chain report names each break once: the two failed logs and the gap
    let chain = t.up.judge.chain(KEY).await.report;
    let failed_at: Vec<u64> = chain
        .issues
        .iter()
        .filter_map(|i| match i {
            trust::ChainIssue::Failed { index, .. } => Some(*index),
            _ => None,
        })
        .collect();
    assert_eq!(failed_at, [3], "{chain:?}");
    let gaps: Vec<(u64, u64)> = chain
        .issues
        .iter()
        .filter_map(|i| match i {
            trust::ChainIssue::Gap { from, to, .. } => Some((*from, *to)),
            _ => None,
        })
        .collect();
    // (log 9 is not signed, so the paired key's chain has no log 9: a gap there too)
    assert_eq!(gaps, [(6, 6), (9, 9)], "{chain:?}");
    assert!(!chain.issues.iter().any(|i| matches!(i, trust::ChainIssue::Mismatch { .. })), "{chain:?}");

    // the counts an owner passes on: no names, no keys
    let c = t.up.judge.log_checks(KEY, Default::default()).await.unwrap();
    assert_eq!((c.verified, c.pending, c.failed), (9, 0, 2), "{c:?}");
    assert_eq!((c.mismatch, c.bad_signature, c.unsigned, c.chain_break), (1, 0, 1, 0), "{c:?}");
    assert_eq!(c.runs_total, 2);
    let starts = |i: u64| trust::LogName::parse(&name(i)).unwrap().started_ms;
    assert_eq!(
        c.failed_runs.iter().map(|r| (r.first_start_ms, r.last_start_ms, r.logs)).collect::<Vec<_>>(),
        [(starts(3), starts(3), 1), (starts(9), starts(9), 1)]
    );
    let text = serde_json::to_string(&c).unwrap();
    assert!(!text.contains(KEY) && !text.contains(".log.xz"), "{text}");

    // a log that contradicts the verified log before it still fails: that is the one real chain break
    let n12 = name(12);
    let side = sidecar(&key, &n12, &log_bytes(12), 12, (&name(11), &sha(b"not log eleven")));
    put(&t, &format!("{n12}.json"), &side).await;
    put(&t, &n12, &log_bytes(12)).await;
    assert_eq!(check_of(&t, &n12).await, failed("chain_break"));
}

#[tokio::test]
async fn logs_arriving_out_of_order_are_pending_never_failed() {
    let t = setup(60_000).await;
    let key = car_key();
    send(&t, &key, 0, &log_bytes(0)).await;
    // 16 at once, finishing in any order: here newest first, then the odd ones, then the even ones
    let mut order: Vec<u64> = (1..=16).rev().filter(|i| i % 3 == 0).collect();
    order.extend((1..=16).filter(|i| i % 3 == 1));
    order.extend((1..=16).filter(|i| i % 3 == 2));
    // log 8 still arriving at the end of the first round: half of it is here
    let b8 = log_bytes(8);
    chunk(&t, &name(8), &b8, 0, &b8[..20]).await;
    for &i in order.iter().filter(|&&i| i != 8) {
        send(&t, &key, i, &log_bytes(i)).await;
        let w = words(&t, 17).await;
        assert!(w.iter().all(|c| !c.starts_with("failed")), "after log {i}: {w:?}");
    }
    let w = words(&t, 17).await;
    assert_eq!(w[9], "pending", "the log before it is on its way: {w:?}");
    // log 8's last bytes and its sidecar: every log fits now, judged again by the next question
    let side = sidecar(&key, &name(8), &b8, 8, (&name(7), &sha(&log_bytes(7))));
    put(&t, &format!("{}.json", name(8)), &side).await;
    chunk(&t, &name(8), &b8, 20, &b8[20..]).await;
    assert_eq!(words(&t, 17).await, vec!["verified".to_string(); 17]);
}

#[tokio::test]
async fn a_log_judged_while_the_one_before_it_was_still_coming_is_told_again_when_it_comes() {
    let t = setup(0).await;
    let key = car_key();
    send(&t, &key, 0, &log_bytes(0)).await;
    send(&t, &key, 1, &log_bytes(1)).await;
    // log 2 is sent long after log 3 (the wait is over): log 3 is verified meanwhile, not failed
    send(&t, &key, 3, &log_bytes(3)).await;
    assert_eq!(check_of(&t, &name(3)).await, verified());
    let gaps = t.up.judge.chain(KEY).await.report.issues;
    assert!(matches!(gaps.as_slice(), [trust::ChainIssue::Gap { from: 2, to: 2, .. }]), "one gap: {gaps:?}");
    // log 2 comes: the chain is whole again
    send(&t, &key, 2, &log_bytes(2)).await;
    assert_eq!(words(&t, 4).await, vec!["verified".to_string(); 4]);
    let chain = t.up.judge.chain(KEY).await.report;
    assert!(chain.issues.is_empty() && chain.segments.len() == 1, "{chain:?}");
}

#[tokio::test]
async fn judging_every_log_again_answers_the_stored_verdicts_or_pending_never_a_new_failure() {
    let t = setup(60_000).await;
    let key = car_key();
    for i in 0..40 {
        send(&t, &key, i, &log_bytes(i)).await;
    }
    let names: Vec<String> = (0..40).map(name).collect();
    // a rebuild (or a newer version) judges every log again while the app keeps asking
    let up = t.up.clone();
    let again = tokio::spawn(async move {
        for _ in 0..3 {
            up.judge.rejudge_all(KEY).await.unwrap();
        }
    });
    while !again.is_finished() {
        let f = status(&t, &names).await;
        for n in &names {
            let c = f[n]["check"].as_str().unwrap();
            assert!(c == "verified" || c == "pending", "{n}: {c}");
        }
        tokio::task::yield_now().await;
    }
    again.await.unwrap();
    assert_eq!(words(&t, 40).await, vec!["verified".to_string(); 40]);

    // a sidecar gone from the disk (a backup put back in part): judging again keeps the log's verdict
    let side_file = t._dir.path().join("data/can").join(KEY).join(format!("{}.json", name(5)));
    assert!(side_file.exists(), "{side_file:?}");
    std::fs::remove_file(&side_file).unwrap();
    t.up.judge.rejudge_all(KEY).await.unwrap();
    assert_eq!(t.up.judge.record(KEY, &name(5)).await.unwrap().status, ingest::judge::Status::Verified);
    assert_eq!(check_of(&t, &name(5)).await, verified());
}

#[tokio::test]
async fn a_failed_log_sent_again_mends_the_chain_after_it() {
    let t = setup(0).await;
    let key = car_key();
    send(&t, &key, 0, &log_bytes(0)).await;
    // log 1 arrives without its sidecar, which comes after the wait: judged unsigned, so failed
    put(&t, &name(1), &log_bytes(1)).await;
    let mut cut = log_bytes(2);
    cut.truncate(100);
    send(&t, &key, 2, &cut).await;
    for i in 3..6 {
        send(&t, &key, i, &log_bytes(i)).await;
    }
    let w = words(&t, 6).await;
    assert_eq!(&w[..3], ["verified", "failed:unsigned", "failed:mismatch"], "{w:?}");
    assert!(w[3..].iter().all(|c| c == "verified"), "{w:?}");
    assert!(!t.up.judge.chain(KEY).await.report.issues.is_empty());

    // "Resend all": log 1's sidecar, and log 2 whole
    let side = sidecar(&key, &name(1), &log_bytes(1), 1, (&name(0), &sha(&log_bytes(0))));
    put(&t, &format!("{}.json", name(1)), &side).await;
    assert_eq!(put(&t, &name(2), &log_bytes(2)).await.0, StatusCode::CREATED);
    assert_eq!(words(&t, 6).await, vec!["verified".to_string(); 6]);
    let chain = t.up.judge.chain(KEY).await.report;
    assert!(chain.issues.is_empty(), "{chain:?}");
    assert_eq!((chain.segments.len(), chain.count), (1, 6));
}

/// A log stored but not judged (the key's trust lock file cannot be taken) is told to the sender as an error, which
/// sends it again; once the fault is gone the same bytes answer `already` and get judged. Before, both answered
/// `201` and the log answered `pending` for ever (nothing looked at a stored file with no record).
#[tokio::test]
async fn a_log_that_could_not_be_judged_is_an_error_and_is_judged_when_sent_again() {
    let t = setup(60_000).await;
    let key = car_key();
    // (a first log ties the token to the car)
    send(&t, &key, 0, &log_bytes(0)).await;
    let lock = t._dir.path().join("data").join("can").join(KEY).join("trust.lock");
    std::fs::remove_file(&lock).unwrap();
    // (a folder where the lock file should be: opening it fails, and that is not a kind the lock puts up with)
    std::fs::create_dir_all(&lock).unwrap();
    let n = name(1);
    let log = log_bytes(1);
    let side = sidecar(&key, &n, &log, 1, (&name(0), &sha(&log_bytes(0))));
    let (s, _) = put(&t, &format!("{n}.json"), &side).await;
    assert_eq!(s, StatusCode::INTERNAL_SERVER_ERROR);
    let (s, _) = put(&t, &n, &log).await;
    assert_eq!(s, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(check_of(&t, &n).await, (None, None), "no answer while the key cannot be read");
    // the fault clears: the same bytes again are stored already, and are judged now
    std::fs::remove_dir(&lock).unwrap();
    assert_eq!(put(&t, &format!("{n}.json"), &side).await.0, StatusCode::CREATED);
    assert_eq!(put(&t, &n, &log).await.0, StatusCode::CREATED);
    assert_eq!(check_of(&t, &n).await, verified());
}
