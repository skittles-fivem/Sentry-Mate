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
use std::io::Write;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use tower::ServiceExt;

struct T {
    _dir: tempfile::TempDir,
    data: std::path::PathBuf,
    up: Arc<Uploads>,
    token: String,
}

type MakeStore = fn(std::path::PathBuf, Arc<vault::Vault>) -> Store;

async fn setup_with(store: MakeStore) -> T {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let (v, _) = vault::Vault::create(&data, &dir.path().join("keys"), vault::Kdf::FAST_FOR_TESTS).unwrap();
    let v = Arc::new(v);
    let tokens = Tokens::open(data.join("tokens.json"), v.clone()).await.unwrap();
    let (token, _) = tokens.create("Model Y").await.unwrap();
    let store = store(data.join("can"), v);
    let up = Uploads::new(store.clone(), tokens, Judge::new(store));
    T { _dir: dir, data, up, token }
}

async fn setup() -> T {
    setup_with(|can, v| Store::with_limits(can, v, 0, 200_000, |_| u64::MAX)).await
}

const IP: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 20));

async fn call(
    t: &T,
    method: &str,
    path: &str,
    token: Option<&str>,
    headers: &[(&str, String)],
    body: Vec<u8>,
    tls: bool,
) -> (StatusCode, Value) {
    let mut b = Request::builder().method(method).uri(path);
    if let Some(tok) = token {
        b = b.header("authorization", format!("Bearer {tok}"));
    }
    for (k, v) in headers {
        b = b.header(*k, v);
    }
    let mut req = b.body(Body::from(body)).unwrap();
    req.extensions_mut().insert(Conn { tls, ip: IP });
    let res = router(t.up.clone()).oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

async fn put(t: &T, name: &str, bytes: &[u8], offset: usize, chunk: &[u8]) -> (StatusCode, Value) {
    let sha = hex::encode(Sha256::digest(bytes));
    let h = [("upload-offset", offset.to_string()), ("upload-length", bytes.len().to_string()), ("upload-sha256", sha)];
    call(t, "POST", &format!("/api/can/logs/{name}"), Some(&t.token), &h, chunk.to_vec(), true).await
}

async fn status(t: &T, names: &[&str]) -> (StatusCode, Value) {
    call(
        t,
        "POST",
        "/api/can/logs/status",
        Some(&t.token),
        &[("content-type", "application/json".into())],
        serde_json::to_vec(&json!({ "names": names })).unwrap(),
        true,
    )
    .await
}

const LOG: &str = "abcdef123456_20261108T143000Z_1.log.xz";

#[tokio::test]
async fn plain_http_wrong_tokens_and_unknown_paths() {
    let t = setup().await;
    let (s, b) = call(&t, "GET", "/api/can/logs/ping", Some(&t.token), &[], vec![], false).await;
    assert_eq!((s, b["code"].as_str()), (StatusCode::FORBIDDEN, Some("https_only")));
    let (s, b) = call(&t, "GET", "/api/can/logs/ping", Some("nope"), &[], vec![], true).await;
    assert_eq!((s, b["code"].as_str()), (StatusCode::UNAUTHORIZED, Some("bad_token")));
    for _ in 0..20 {
        call(&t, "GET", "/api/can/logs/ping", None, &[], vec![], true).await;
    }
    let (s, b) = call(&t, "GET", "/api/can/logs/ping", Some("nope"), &[], vec![], true).await;
    assert_eq!((s, b["code"].as_str()), (StatusCode::TOO_MANY_REQUESTS, Some("slow_down")));
    // a right token is never refused because of wrong ones
    let (s, b) = call(&t, "GET", "/api/can/logs/ping", Some(&t.token), &[], vec![], true).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b["vehicleKey"], Value::Null);
    assert_eq!(b["formats"], json!(["canbridge-candump-xz/1", "canbridge-columns-xz/2"]));
    assert_eq!(b["maxChunk"], json!(16 << 20));
    let (s, b) = call(&t, "GET", "/api/footage", Some(&t.token), &[], vec![], true).await;
    assert_eq!((s, b["code"].as_str()), (StatusCode::NOT_FOUND, Some("not_found")));
}

/// Stored chunks don't count against the token's requests: a bridge with a backlog of more files than that (a few
/// days away from home) goes on at full speed instead of a file a second after a fast start. Everything else still
/// counts.
#[tokio::test]
async fn stored_chunks_do_not_use_up_the_token_s_requests() {
    let t = setup().await;
    let n = ingest::api::MAX_REQUESTS_PER_TOKEN + 50;
    let bytes: Vec<u8> = (0..n + 10).map(|i| (i % 251) as u8).collect();
    for off in 0..n {
        let (s, b) = put(&t, LOG, &bytes, off, &bytes[off..off + 1]).await;
        assert_eq!((s, b["received"].as_u64()), (StatusCode::OK, Some(off as u64 + 1)), "chunk {off}: {b}");
    }
    // a refused chunk counts, and so do pings
    let (s, _) = put(&t, LOG, &bytes, 0, &bytes[..1]).await;
    assert_eq!(s, StatusCode::CONFLICT);
    let mut last = StatusCode::OK;
    for _ in 0..ingest::api::MAX_REQUESTS_PER_TOKEN {
        last = call(&t, "GET", "/api/can/logs/ping", Some(&t.token), &[], vec![], true).await.0;
    }
    assert_eq!(last, StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn empty_chunks_are_refused_and_count() {
    let t = setup().await;
    let bytes = [7u8; 100];
    let mut last = (StatusCode::OK, Value::Null);
    for _ in 0..=ingest::api::MAX_REQUESTS_PER_TOKEN {
        last = put(&t, LOG, &bytes, 0, &[]).await;
        if last.0 != StatusCode::BAD_REQUEST {
            break;
        }
        assert_eq!(last.1["code"].as_str(), Some("empty_chunk"), "{}", last.1);
    }
    assert_eq!(last.0, StatusCode::TOO_MANY_REQUESTS, "{}", last.1);
}

#[tokio::test]
async fn sidecars_with_no_log_here_count() {
    let t = setup().await;
    let body = br#"{"x":1}"#;
    let mut last = (StatusCode::OK, Value::Null);
    for i in 0..ingest::api::MAX_REQUESTS_PER_TOKEN + 10 {
        last = put(&t, &format!("abcdef123456_20261108T143000Z_{i}.log.xz.json"), body, 0, body).await;
        if last.0 != StatusCode::CREATED {
            break;
        }
    }
    assert_eq!(last.0, StatusCode::TOO_MANY_REQUESTS, "{}", last.1);
}

#[tokio::test]
async fn a_sidecar_after_its_log_does_not_count() {
    let t = setup().await;
    let body = br#"{"x":1}"#;
    for i in 0..ingest::api::MAX_REQUESTS_PER_TOKEN / 2 + 10 {
        let log = format!("abcdef123456_20261108T143000Z_{i}.log.xz");
        let (s, b) = put(&t, &log, &[i as u8; 10], 0, &[i as u8; 10]).await;
        assert_eq!(s, StatusCode::CREATED, "log {i}: {b}");
        let (s, b) = put(&t, &format!("{log}.json"), body, 0, body).await;
        assert_eq!(s, StatusCode::CREATED, "sidecar {i}: {b}");
    }
}

#[tokio::test]
async fn a_log_uploads_in_chunks_and_resumes() {
    let t = setup().await;
    let bytes: Vec<u8> = (0..1000u32).map(|i| (i % 251) as u8).collect();
    let (s, b) = status(&t, &[LOG, "nonsense"]).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b["files"][LOG]["state"], "missing");
    assert_eq!(b["files"]["nonsense"]["state"], "invalid");

    let (s, b) = put(&t, LOG, &bytes, 0, &bytes[..400]).await;
    assert_eq!((s, b["received"].as_u64(), b["complete"].as_bool()), (StatusCode::OK, Some(400), Some(false)));
    let (_, b) = status(&t, &[LOG]).await;
    assert_eq!(b["files"][LOG], json!({ "state": "partial", "received": 400, "length": 1000, "size": 1000 }));
    // a lost connection: the app resends from the wrong place and is told where to resume
    let (s, b) = put(&t, LOG, &bytes, 300, &bytes[300..]).await;
    assert_eq!(
        (s, b["code"].as_str(), b["received"].as_u64()),
        (StatusCode::CONFLICT, Some("offset_mismatch"), Some(400))
    );
    let (s, b) = put(&t, LOG, &bytes, 400, &bytes[400..]).await;
    assert_eq!((s, b["complete"].as_bool()), (StatusCode::CREATED, Some(true)));
    // a retry of a complete file is harmless; a different file under the name is refused
    let (s, b) = put(&t, LOG, &bytes, 0, &bytes).await;
    assert_eq!((s, b["already"].as_bool()), (StatusCode::CREATED, Some(true)));
    let other = vec![7u8; 1000];
    let (s, b) = put(&t, LOG, &other, 0, &other).await;
    assert_eq!((s, b["code"].as_str()), (StatusCode::CONFLICT, Some("exists_different")));
    let (_, b) = status(&t, &[LOG]).await;
    assert_eq!(b["files"][LOG]["state"], "complete");
}

fn files_ending(dir: &std::path::Path, end: &str, out: &mut Vec<std::path::PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            files_ending(&p, end, out);
        } else if p.to_string_lossy().ends_with(end) {
            out.push(p);
        }
    }
}

#[tokio::test]
async fn a_status_request_reads_an_upload_without_cutting_or_changing_it() {
    let t = setup().await;
    let bytes: Vec<u8> = (0..1000u32).map(|i| (i % 251) as u8).collect();
    put(&t, LOG, &bytes, 0, &bytes[..400]).await;
    let mut parts = Vec::new();
    files_ending(&t.data, ".part", &mut parts);
    let [part] = &parts[..] else { panic!("one upload under way: {parts:?}") };
    // a chunk being written at this moment (or cut by a crash): its record isn't whole yet
    let mut f = std::fs::OpenOptions::new().append(true).open(part).unwrap();
    f.write_all(&[0, 0, 0, 200, 1, 2, 3]).unwrap();
    drop(f);
    let before = std::fs::read(part).unwrap();
    let (_, b) = status(&t, &[LOG]).await;
    assert_eq!(b["files"][LOG], json!({ "state": "partial", "received": 400, "length": 1000, "size": 1000 }));
    assert_eq!(std::fs::read(part).unwrap(), before, "the status request left the upload as it is");
    // the next chunk carries on after the whole records
    let (s, b) = put(&t, LOG, &bytes, 400, &bytes[400..]).await;
    assert_eq!((s, b["complete"].as_bool()), (StatusCode::CREATED, Some(true)));
}

/// The one upload under way, as (its part, its promise).
fn the_part(t: &T) -> (std::path::PathBuf, std::path::PathBuf) {
    let mut parts = Vec::new();
    files_ending(&t.data, ".part", &mut parts);
    let [part] = &parts[..] else { panic!("one upload under way: {parts:?}") };
    let mut meta = part.clone().into_os_string();
    meta.push(".json");
    (part.clone(), meta.into())
}

#[tokio::test]
async fn a_damaged_part_from_before_format_2_is_dropped_and_the_upload_starts_over() {
    let t = setup().await;
    let bytes: Vec<u8> = (0..1000u32).map(|i| (i % 251) as u8).collect();
    put(&t, LOG, &bytes, 0, &bytes[..400]).await;
    let (part, meta) = the_part(&t);
    // the part as the vault's format 1 left it, one record damaged (the bytes of a record don't open): it reads as
    // 400 bytes received, but can't be written again in format 2, which checks every record
    let mut old = vault::records::MAGIC_V1.to_vec();
    old.extend_from_slice(&[7u8; 16]);
    for n in [300u32, 100] {
        old.extend_from_slice(&(n + 16).to_be_bytes());
        old.extend(std::iter::repeat_n(9u8, n as usize + 16));
    }
    std::fs::write(&part, &old).unwrap();
    let (_, b) = status(&t, &[LOG]).await;
    assert_eq!(b["files"][LOG], json!({ "state": "partial", "received": 400, "length": 1000, "size": 1000 }));
    // the next chunk: the part is dropped and the app told to start again from 0, at once
    let (s, b) = put(&t, LOG, &bytes, 400, &bytes[400..]).await;
    assert_eq!(
        (s, b["code"].as_str(), b["received"].as_u64()),
        (StatusCode::CONFLICT, Some("offset_mismatch"), Some(0)),
        "{b}"
    );
    assert!(!part.exists() && !meta.exists() && !vault::records::converting_path(&part).exists());
    let (_, b) = status(&t, &[LOG]).await;
    assert_eq!(b["files"][LOG]["state"], "missing");
    let (s, b) = put(&t, LOG, &bytes, 0, &bytes).await;
    assert_eq!((s, b["complete"].as_bool()), (StatusCode::CREATED, Some(true)));
    assert_eq!(&**t.up.store.read(LOG).await.unwrap().unwrap(), &bytes[..]);
}

#[tokio::test]
async fn a_copy_left_while_a_part_was_written_in_format_2_is_cleaned_with_stale_uploads() {
    let t = setup().await;
    let bytes: Vec<u8> = (0..1000u32).map(|i| (i % 251) as u8).collect();
    put(&t, LOG, &bytes, 0, &bytes[..400]).await;
    let (part, _) = the_part(&t);
    // a crash while the vault wrote the part again in format 2 (the part itself is whole)
    let copy = vault::records::converting_path(&part);
    std::fs::write(&copy, b"half a copy").unwrap();
    std::fs::OpenOptions::new()
        .write(true)
        .open(&copy)
        .unwrap()
        .set_modified(std::time::SystemTime::now() - DAYS_3)
        .unwrap();
    // a fresh copy is left alone (a conversion may be under way), an old one goes with the stale uploads
    let fresh = vault::records::converting_path(&part.with_file_name("abcdef123456_20261108T150000Z_2.log.xz.part"));
    std::fs::write(&fresh, b"being written").unwrap();
    assert_eq!(t.up.store.clean_stale(ingest::store::STALE_PARTIAL_MS).await, 0, "no upload is stale");
    assert!(!copy.exists() && fresh.exists() && part.exists());
    // the upload carries on
    let (s, b) = put(&t, LOG, &bytes, 400, &bytes[400..]).await;
    assert_eq!((s, b["complete"].as_bool()), (StatusCode::CREATED, Some(true)));
}

const DAYS_3: std::time::Duration = std::time::Duration::from_secs(3 * 24 * 3600);

#[tokio::test]
async fn a_file_that_does_not_match_its_hash_is_discarded() {
    let t = setup().await;
    let bytes = b"real bytes".to_vec();
    let sha = hex::encode(Sha256::digest(b"other bytes"));
    let h = [("upload-offset", "0".to_string()), ("upload-length", bytes.len().to_string()), ("upload-sha256", sha)];
    let (s, b) = call(&t, "POST", &format!("/api/can/logs/{LOG}"), Some(&t.token), &h, bytes, true).await;
    assert_eq!((s, b["code"].as_str()), (StatusCode::UNPROCESSABLE_ENTITY, Some("sha256_mismatch")));
    let (_, b) = status(&t, &[LOG]).await;
    assert_eq!(b["files"][LOG]["state"], "missing");
    let side = format!("{LOG}.json");
    let (s, b) = put(&t, &side, b"not json", 0, b"not json").await;
    assert_eq!((s, b["code"].as_str()), (StatusCode::UNPROCESSABLE_ENTITY, Some("bad_sidecar")));
}

#[tokio::test]
async fn bad_requests_are_refused() {
    let t = setup().await;
    let (s, b) = put(&t, "../../etc/passwd", b"x", 0, b"x").await;
    assert_eq!((s, b["code"].as_str()), (StatusCode::NOT_FOUND, Some("not_found")));
    let (s, b) = put(&t, "abc_20261108T143000Z_1.log", b"x", 0, b"x").await;
    assert_eq!((s, b["code"].as_str()), (StatusCode::BAD_REQUEST, Some("bad_name")));
    let (s, b) = put(&t, LOG, b"abc", 0, b"abcd").await;
    assert_eq!((s, b["code"].as_str()), (StatusCode::BAD_REQUEST, Some("past_end")));
    let h = [("upload-offset", "-1".to_string()), ("upload-length", "3".into()), ("upload-sha256", "0".repeat(64))];
    let (s, b) = call(&t, "POST", &format!("/api/can/logs/{LOG}"), Some(&t.token), &h, b"abc".to_vec(), true).await;
    assert_eq!((s, b["code"].as_str()), (StatusCode::BAD_REQUEST, Some("bad_offset")));
    let h = [
        ("upload-offset", "0".to_string()),
        ("upload-length", (2u64 << 20).to_string()),
        ("upload-sha256", "0".repeat(64)),
    ];
    let (s, b) = call(&t, "POST", &format!("/api/can/logs/{LOG}.json"), Some(&t.token), &h, b"{}".to_vec(), true).await;
    assert_eq!((s, b["code"].as_str()), (StatusCode::PAYLOAD_TOO_LARGE, Some("too_large")));
    let (s, b) = call(&t, "POST", "/api/can/logs/status", Some(&t.token), &[], b"[1,2]".to_vec(), true).await;
    assert_eq!((s, b["code"].as_str()), (StatusCode::BAD_REQUEST, Some("bad_request")));
}

#[tokio::test]
async fn a_token_belongs_to_one_car_after_its_first_upload() {
    let t = setup().await;
    let other = "zzz999_20261108T143000Z_1.log.xz";
    let (s, b) = status(&t, &[LOG, other]).await;
    assert_eq!((s, b["code"].as_str()), (StatusCode::BAD_REQUEST, Some("mixed_cars")));
    put(&t, LOG, b"abc", 0, b"abc").await;
    let (_, b) = call(&t, "GET", "/api/can/logs/ping", Some(&t.token), &[], vec![], true).await;
    assert_eq!(b["vehicleKey"], "abcdef123456");
    let (s, b) = put(&t, other, b"abc", 0, b"abc").await;
    assert_eq!((s, b["code"].as_str()), (StatusCode::FORBIDDEN, Some("wrong_car")));
    let (_, b) = status(&t, &[LOG, other]).await;
    assert_eq!(b["files"][other]["state"], "forbidden");
    assert_eq!(b["files"][LOG]["state"], "complete");
}

async fn put_as(t: &T, token: &str, name: &str, bytes: &[u8]) -> (StatusCode, Value) {
    let sha = hex::encode(Sha256::digest(bytes));
    let h = [("upload-offset", "0".to_string()), ("upload-length", bytes.len().to_string()), ("upload-sha256", sha)];
    call(t, "POST", &format!("/api/can/logs/{name}"), Some(token), &h, bytes.to_vec(), true).await
}

async fn status_as(t: &T, token: &str, names: &[&str]) -> (StatusCode, Value) {
    let body = serde_json::to_vec(&json!({ "names": names })).unwrap();
    call(t, "POST", "/api/can/logs/status", Some(token), &[("content-type", "application/json".into())], body, true)
        .await
}

#[tokio::test]
async fn a_fresh_token_cannot_take_another_cars_key() {
    let t = setup().await;
    // the car's own token uploads: its key is in use from now on (and in every log name, so not a secret)
    let (s, _) = put(&t, LOG, b"genuine", 0, b"genuine").await;
    assert_eq!(s, StatusCode::CREATED);
    // a token handed out for another car (never used) claims the car's key
    let (attacker, a) = t.up.tokens.create("Another car").await.unwrap();
    let squat = "abcdef123456_20261120T080000Z_1.log.xz";
    let (s, b) = put_as(&t, &attacker, squat, b"junk").await;
    assert_eq!((s, b["code"].as_str()), (StatusCode::FORBIDDEN, Some("wrong_car")));
    assert!(!t.up.store.exists(squat), "nothing planted in the car's folder");
    // it doesn't learn which of the car's logs exist
    let (s, b) = status_as(&t, &attacker, &[LOG]).await;
    assert_eq!((s, b["code"].as_str()), (StatusCode::FORBIDDEN, Some("wrong_car")));
    // and it stays untied: not listed as the car's token
    let (_, b) = call(&t, "GET", "/api/can/logs/ping", Some(&attacker), &[], vec![], true).await;
    assert_eq!(b["vehicleKey"], Value::Null);
    assert!(t.up.tokens.list().await.iter().any(|x| x.id == a.id && x.vehicle_key.is_none()));
    // the genuine bridge still uploads that name
    let (s, _) = put(&t, squat, b"real log", 0, b"real log").await;
    assert_eq!(s, StatusCode::CREATED);

    // a key another token is tied to, with no log here yet, is just as taken
    let (other, o) = t.up.tokens.create("Second car").await.unwrap();
    t.up.tokens.tie(&o, "0123456789ab").await.unwrap();
    let _ = other;
    let (s, b) = put_as(&t, &attacker, "0123456789ab_20261108T143000Z_1.log.xz", b"x").await;
    assert_eq!((s, b["code"].as_str()), (StatusCode::FORBIDDEN, Some("wrong_car")));

    // the fresh token still works for a car key no one uses yet
    let (s, b) = status_as(&t, &attacker, &["fedcba654321_20261108T143000Z_1.log.xz"]).await;
    assert_eq!(
        (s, b["files"]["fedcba654321_20261108T143000Z_1.log.xz"]["state"].as_str()),
        (StatusCode::OK, Some("missing"))
    );
    let (s, _) = put_as(&t, &attacker, "fedcba654321_20261108T143000Z_1.log.xz", b"mine").await;
    assert_eq!(s, StatusCode::CREATED);

    // a replacement token made for the car (the owner names its key) takes the key over
    let (replacement, _) = t.up.tokens.create_for("Model Y", &["abcdef123456".to_string()]).await.unwrap();
    let (s, b) = status_as(&t, &replacement, &[LOG]).await;
    assert_eq!((s, b["files"][LOG]["state"].as_str()), (StatusCode::OK, Some("complete")));
    let (s, _) = put_as(&t, &replacement, "abcdef123456_20261121T080000Z_1.log.xz", b"after").await;
    assert_eq!(s, StatusCode::CREATED);
    let (_, b) = call(&t, "GET", "/api/can/logs/ping", Some(&replacement), &[], vec![], true).await;
    assert_eq!(b["vehicleKey"], "abcdef123456");
    assert!(t.up.tokens.create_for("Model Y", &["not a key".to_string()]).await.is_err());
}

/// The last contact is the last request with an accepted token: a ping, a status question or a chunk each move it,
/// a wrong token or plain HTTP does not, a refused claim never touches another car's key, and it is read back after
/// a restart.
#[tokio::test]
async fn every_accepted_request_is_the_cars_last_contact() {
    use ingest::{Call, Contacts};
    let t = setup().await;
    let c: Arc<Contacts> = t.up.tokens.contacts().clone();
    let id = t.up.tokens.find(&t.token).await.unwrap().id;
    let key = "abcdef123456";
    let tick = || std::thread::sleep(std::time::Duration::from_millis(3));
    // refused before its token counts: nothing
    call(&t, "GET", "/api/can/logs/ping", Some("smt_wrong"), &[], vec![], true).await;
    call(&t, "GET", "/api/can/logs/ping", Some(&t.token), &[], vec![], false).await;
    assert_eq!((c.token(&id), c.key(key)), (None, None));
    // an untied token: its own contact, no car key's
    let (s, _) = call(&t, "GET", "/api/can/logs/ping", Some(&t.token), &[], vec![], true).await;
    assert_eq!(s, StatusCode::OK);
    let ping = c.token(&id).unwrap();
    assert_eq!(ping.call, Call::Ping);
    tick();
    status(&t, &[LOG]).await;
    let st = c.token(&id).unwrap();
    assert!(st.call == Call::Status && st.at_ms > ping.at_ms, "{st:?}");
    assert_eq!(c.key(key), None, "asking about a key does not tie it");
    // its first upload claims the key: that car's contact, and its newest log
    tick();
    let (s, _) = put(&t, LOG, b"abc", 0, b"abc").await;
    assert_eq!(s, StatusCode::CREATED);
    let up = c.key(key).unwrap();
    assert!(up.call == Call::Upload && up.at_ms > st.at_ms, "{up:?}");
    assert_eq!(c.token(&id), Some(up));
    assert!(c.newest_log(key).is_some_and(|t| t >= up.at_ms));
    // tied now: a ping moves the key's contact too
    tick();
    call(&t, "GET", "/api/can/logs/ping", Some(&t.token), &[], vec![], true).await;
    let ping = c.key(key).unwrap();
    assert!(ping.call == Call::Ping && ping.at_ms > up.at_ms, "{ping:?}");
    // a token made for another car that claims this key is refused: the car's contact does not move
    let (other, o) = t.up.tokens.create("Another car").await.unwrap();
    tick();
    let (s, _) = put_as(&t, &other, "abcdef123456_20261120T080000Z_1.log.xz", b"junk").await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert_eq!(c.key(key), Some(ping));
    assert_eq!(c.token(&o.id).map(|x| x.call), Some(Call::Upload), "its own token's contact is kept");
    assert_eq!(c.newest([key, "fedcba654321"]), Some(ping));
    // kept over a restart
    assert!(c.save().unwrap());
    let again = Tokens::open(t.data.join("tokens.json"), vault_of(&t)).await.unwrap();
    assert_eq!(again.contacts().key(key), Some(ping));
    assert_eq!(again.contacts().token(&id), Some(ping));
    assert_eq!(again.contacts().newest_log(key), c.newest_log(key));
    // and an instance given the server's sees its live values
    let shared = Tokens::open(t.data.join("tokens.json"), vault_of(&t)).await.unwrap().with_contacts(c.clone());
    tick();
    call(&t, "GET", "/api/can/logs/ping", Some(&t.token), &[], vec![], true).await;
    assert!(shared.contacts().key(key).unwrap().at_ms > ping.at_ms);
}

fn vault_of(t: &T) -> Arc<vault::Vault> {
    let keys = t.data.parent().unwrap().join("keys");
    Arc::new(vault::Vault::open(&t.data, &keys).unwrap())
}

#[tokio::test]
async fn no_room_and_too_many_logs_are_refused() {
    let t = setup_with(|can, v| Store::with_limits(can, v, 1 << 30, 1, |_| 100 << 20)).await;
    let (s, b) = put(&t, LOG, b"abc", 0, b"abc").await;
    assert_eq!((s, b["code"].as_str()), (StatusCode::INSUFFICIENT_STORAGE, Some("no_space")));
    let t = setup_with(|can, v| Store::with_limits(can, v, 0, 1, |_| u64::MAX)).await;
    put(&t, LOG, b"abc", 0, b"abc").await;
    let (s, b) = put(&t, "abcdef123456_20261108T143100Z_2.log.xz", b"abc", 0, b"abc").await;
    assert_eq!((s, b["code"].as_str()), (StatusCode::INSUFFICIENT_STORAGE, Some("car_full")));
}

fn signed(key: &SigningKey, name: &str, log: &[u8], index: u64, prev: (&str, &str)) -> Vec<u8> {
    let b64 = base64::engine::general_purpose::STANDARD;
    let der = key.verifying_key().to_public_key_der().unwrap().as_bytes().to_vec();
    let sha = hex::encode(Sha256::digest(log));
    let msg = trust::signed_message("abcdef123456", name, &sha, log.len() as u64, index, prev.0, prev.1);
    let sig: Signature = key.sign(msg.as_bytes());
    serde_json::to_vec(&json!({ "format": "canbridge-columns-xz/2", "integrity": {
        "signed": true, "version": trust::VERSION, "algorithm": trust::ALGORITHM, "public_key": b64.encode(&der),
        "key_fingerprint": trust::fingerprint(&der), "vehicle_key": "abcdef123456", "sha256": sha, "length": log.len(),
        "index": index, "prev_name": prev.0, "prev_sha256": prev.1, "signature": b64.encode(sig.to_der().as_bytes()) } }))
    .unwrap()
}

#[tokio::test]
async fn the_first_signed_log_pairs_the_car_and_another_key_fails() {
    let t = setup().await;
    let key = SigningKey::random(&mut rand_core::OsRng);
    let side = signed(&key, LOG, b"log one", 0, ("-", trust::GENESIS_SHA));
    put(&t, LOG, b"log one", 0, b"log one").await;
    assert_eq!(t.up.judge.record("abcdef123456", LOG).await.unwrap().status, ingest::judge::Status::Waiting);
    let (s, _) = put(&t, &format!("{LOG}.json"), &side, 0, &side).await;
    assert_eq!(s, StatusCode::CREATED);
    let r = t.up.judge.record("abcdef123456", LOG).await.unwrap();
    assert_eq!(r.status, ingest::judge::Status::Verified, "{r:?}");
    let book = t.up.judge.book("abcdef123456").await;
    assert_eq!(
        book.pin.unwrap().fingerprint,
        trust::fingerprint(key.verifying_key().to_public_key_der().unwrap().as_bytes())
    );

    // the next log, signed by the same key: chained, no issues
    let log2 = "abcdef123456_20261108T143100Z_2.log.xz";
    let sha1 = hex::encode(Sha256::digest(b"log one"));
    let side2 = signed(&key, log2, b"log two", 1, (LOG, &sha1));
    put(&t, &format!("{log2}.json"), &side2, 0, &side2).await; // sidecar first, either order works
    put(&t, log2, b"log two", 0, b"log two").await;
    assert_eq!(t.up.judge.record("abcdef123456", log2).await.unwrap().status, ingest::judge::Status::Verified);
    let chain = t.up.judge.chain("abcdef123456").await;
    assert_eq!((chain.report.count, chain.report.issues.len()), (2, 0), "{chain:?}");
    let paired = trust::fingerprint(key.verifying_key().to_public_key_der().unwrap().as_bytes());
    assert_eq!(chain.signing_key.as_deref(), Some(paired.as_str()), "the report names the key it covers");

    // a log signed by some other key is kept, failed, and its key offered
    let log3 = "abcdef123456_20261108T143200Z_3.log.xz";
    let intruder = SigningKey::random(&mut rand_core::OsRng);
    let side3 = signed(&intruder, log3, b"log three", 2, (log2, &hex::encode(Sha256::digest(b"log two"))));
    put(&t, log3, b"log three", 0, b"log three").await;
    put(&t, &format!("{log3}.json"), &side3, 0, &side3).await;
    let r = t.up.judge.record("abcdef123456", log3).await.unwrap();
    assert_eq!((&r.status, r.reason.as_deref()), (&ingest::judge::Status::Failed, Some("signed by another key")));
    assert!(!r.usable());
    assert!(t.up.judge.book("abcdef123456").await.new_key.is_some());
}

/// Every file under the data folder, with its bytes.
fn all_files(dir: &std::path::Path) -> Vec<(std::path::PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(all_files(&p));
        } else {
            out.push((p.clone(), std::fs::read(&p).unwrap()));
        }
    }
    out
}

#[tokio::test]
async fn nothing_reaches_storage_in_the_clear() {
    let t = setup().await;
    let marker = b"CLEARTEXT-MARKER-0x04F-GPS".repeat(50);
    let key = SigningKey::random(&mut rand_core::OsRng);
    let side = signed(&key, LOG, &marker, 0, ("-", trust::GENESIS_SHA));
    // half a log (an upload in progress), then all of it, then the sidecar
    put(&t, LOG, &marker, 0, &marker[..600]).await;
    for (p, bytes) in all_files(&t.data) {
        assert!(!bytes.windows(17).any(|w| w == b"CLEARTEXT-MARKER-"), "{} holds plaintext mid-upload", p.display());
    }
    put(&t, LOG, &marker, 600, &marker[600..]).await;
    put(&t, &format!("{LOG}.json"), &side, 0, &side).await;
    assert_eq!(t.up.judge.record("abcdef123456", LOG).await.unwrap().status, ingest::judge::Status::Verified);
    let files = all_files(&t.data);
    // the trust records: a sealed journal (sealed records, like an upload in progress)
    assert!(files.iter().any(|(p, _)| p.ends_with("can/abcdef123456/trust-0.journal")));
    for (p, bytes) in &files {
        let name = p.file_name().unwrap().to_string_lossy().to_string();
        if name == "vault.json" {
            continue;
        }
        // the lock file the server and a command take turns by: empty, it holds nothing
        if name == "trust.lock" {
            assert!(bytes.is_empty(), "{} is not empty", p.display());
            continue;
        }
        let sealed = vault::is_sealed(bytes) || bytes.starts_with(vault::records::MAGIC);
        assert!(sealed, "{} is not sealed", p.display());
        for needle in [&b"CLEARTEXT-MARKER-"[..], b"abcdef123456", b"Model Y", b"integrity", t.token.as_bytes()] {
            assert!(
                !bytes.windows(needle.len()).any(|w| w == needle),
                "{} shows {:?}",
                p.display(),
                String::from_utf8_lossy(needle)
            );
        }
    }
    // and it reads back exactly as sent
    assert_eq!(&**t.up.store.read(LOG).await.unwrap().unwrap(), &marker[..]);
}

#[tokio::test]
async fn a_sealed_log_moved_under_another_name_does_not_open() {
    let t = setup().await;
    put(&t, LOG, b"the real minute", 0, b"the real minute").await;
    let other = "abcdef123456_20261108T143100Z_2.log.xz";
    let dir = t.data.join("can/abcdef123456");
    std::fs::copy(dir.join(LOG), dir.join(other)).unwrap();
    assert!(t.up.store.read(other).await.is_err());
    assert!(t.up.store.read(LOG).await.unwrap().is_some());
}

/// What the server writes to its log (kept in the clear), caught for the test running on this thread.
struct Capture(Arc<std::sync::Mutex<Vec<String>>>);

impl tracing::Subscriber for Capture {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        struct Fields(String);
        impl tracing::field::Visit for Fields {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                self.0.push_str(&format!("{}={value:?} ", field.name()));
            }
        }
        let mut f = Fields(String::new());
        event.record(&mut f);
        self.0.lock().unwrap().push(f.0);
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

#[tokio::test]
async fn a_car_that_cant_be_judged_is_reported_without_its_key() {
    let t = setup().await;
    let logs = Arc::new(std::sync::Mutex::new(Vec::new()));
    let _catch = tracing::subscriber::set_default(Capture(logs.clone()));
    // the car's trust head is damaged: its uploads are still kept, but can't be judged (and the sender is told, so it
    // sends them again)
    let car = t.data.join("can/abcdef123456");
    std::fs::create_dir_all(&car).unwrap();
    std::fs::write(car.join("trust.head"), b"damaged, not sealed").unwrap();
    // (the car's folder is there, so its key is in use: the car's own token is tied to it)
    let tok = t.up.tokens.find(&t.token).await.unwrap();
    t.up.tokens.tie(&tok, "abcdef123456").await.unwrap();
    let (s, _) = put(&t, LOG, b"a minute", 0, b"a minute").await;
    assert_eq!(s, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(t.up.store.exists(LOG), "the log is kept");
    // and the sweep can't judge its waiting logs either
    std::fs::create_dir_all(t.data.join("can/.waiting")).unwrap();
    std::fs::write(t.data.join("can/.waiting/abcdef123456"), b"").unwrap();
    assert_eq!(t.up.judge.sweep().await.unwrap(), 0);
    let logs = logs.lock().unwrap().clone();
    assert!(logs.iter().any(|l| l.contains("could not judge")), "{logs:?}");
    assert!(logs.iter().any(|l| l.contains("judging the waiting logs")), "{logs:?}");
    for l in &logs {
        assert!(!l.contains("abcdef123456"), "the car's key is in the server's log: {l}");
    }
}

/// Uploads go through the data folder's write gate: during a pause (an export's snapshot) they wait, then answer
/// `503 moving` with `Retry-After`; one that waited while the gate was closed (a move's commit) is refused with the
/// gate's answer and never answered `2xx`.
#[tokio::test(flavor = "multi_thread")]
async fn uploads_wait_at_the_write_gate_and_are_refused_once_it_closes() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let (v, _) = vault::Vault::create(&data, &dir.path().join("keys"), vault::Kdf::FAST_FOR_TESTS).unwrap();
    let v = Arc::new(v);
    let tokens = Tokens::open(data.join("tokens.json"), v.clone()).await.unwrap();
    let (token, _) = tokens.create("Model Y").await.unwrap();
    let store = Store::with_limits(data.join("can"), v, 0, 200_000, |_| u64::MAX);
    let gate = gate::Gate::open(&data).unwrap();
    let wait = std::time::Duration::from_millis(300);
    let up = Uploads::with_gate(store.clone(), tokens, Judge::new(store), gate.clone(), wait);
    let send = |bytes: Vec<u8>| {
        let (up, token) = (up.clone(), token.clone());
        async move {
            let sha = hex::encode(Sha256::digest(&bytes));
            let mut req = Request::builder()
                .method("POST")
                .uri(format!("/api/can/logs/{LOG}"))
                .header("authorization", format!("Bearer {token}"))
                .header("upload-offset", "0")
                .header("upload-length", bytes.len().to_string())
                .header("upload-sha256", sha)
                .body(Body::from(bytes))
                .unwrap();
            req.extensions_mut().insert(Conn { tls: true, ip: IP });
            let res = router(up).oneshot(req).await.unwrap();
            let retry = res.headers().get("retry-after").map(|h| h.to_str().unwrap().to_string());
            let status = res.status();
            let b = res.into_body().collect().await.unwrap().to_bytes();
            (status, serde_json::from_slice::<Value>(&b).unwrap_or(Value::Null), retry)
        }
    };
    let bytes = vec![3u8; 1000];
    // a pause holds the upload, which gives up after its wait
    let pause = gate.pause(std::time::Duration::from_secs(1)).unwrap();
    let (s, b, retry) = send(bytes.clone()).await;
    assert_eq!(
        (s, b["code"].as_str(), retry.as_deref()),
        (StatusCode::SERVICE_UNAVAILABLE, Some("moving"), Some("60"))
    );
    drop(pause);
    // a writer that waits while the gate closes is refused with the gate's answer
    let pause = gate.pause(std::time::Duration::from_secs(1)).unwrap();
    let waiting = tokio::spawn(send(bytes.clone()));
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    gate.close(gate::Closed { code: "moved", message: "moved away".into(), retry_after_s: 3600 });
    drop(pause);
    let (s, b, retry) = waiting.await.unwrap();
    assert_eq!(
        (s, b["code"].as_str(), retry.as_deref()),
        (StatusCode::SERVICE_UNAVAILABLE, Some("moved"), Some("3600"))
    );
    assert!(!data.join("can/abcdef123456").join(LOG).exists(), "nothing was written");
    // open again: the upload goes in
    gate.reopen();
    let (s, _, _) = send(bytes.clone()).await;
    assert_eq!(s, StatusCode::CREATED);
    assert_eq!(gate.inside(), 0, "the pass was given back");
}
