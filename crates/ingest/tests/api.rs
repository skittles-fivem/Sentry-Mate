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
    assert_eq!(b["files"][LOG], json!({ "state": "partial", "received": 400, "length": 1000 }));
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
    assert_eq!(b["files"][LOG], json!({ "state": "partial", "received": 400, "length": 1000 }));
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
    assert_eq!(b["files"][LOG], json!({ "state": "partial", "received": 400, "length": 1000 }));
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
    // the car's trust head is damaged: its uploads are still kept, but can't be judged
    let car = t.data.join("can/abcdef123456");
    std::fs::create_dir_all(&car).unwrap();
    std::fs::write(car.join("trust.head"), b"damaged, not sealed").unwrap();
    let (s, _) = put(&t, LOG, b"a minute", 0, b"a minute").await;
    assert_eq!(s, StatusCode::CREATED);
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
