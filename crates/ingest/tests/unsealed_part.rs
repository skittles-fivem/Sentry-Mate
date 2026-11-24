#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use ingest::{Conn, Judge, Store, Tokens, Uploads, router};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
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

const IP: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 20));

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

/// A last chunk that arrived but was not sealed: the sealing step fails with an I/O error (here: the
/// `.sealing` path is blocked by a directory), the part keeps every byte, and after the cause is gone no request
/// can finish the upload. Expected: a retry of the last chunk (or a restart at offset 0) completes the file.
#[tokio::test]
async fn a_last_chunk_whose_sealing_failed_can_be_finished_afterwards() {
    let t = setup().await;
    let bytes: Vec<u8> = (0..1000u32).map(|i| (i % 251) as u8).collect();
    let (s, _) = put(&t, LOG, &bytes, 0, &bytes[..400]).await;
    assert_eq!(s, StatusCode::OK);
    let mut parts = Vec::new();
    files_ending(&t.data, ".part", &mut parts);
    let [part] = &parts[..] else { panic!("one upload under way: {parts:?}") };
    let sealing = part
        .with_file_name(format!("{}.sealing", part.file_name().unwrap().to_string_lossy().trim_end_matches(".part")));
    std::fs::create_dir(&sealing).unwrap();
    let (s, _) = put(&t, LOG, &bytes, 400, &bytes[400..]).await;
    assert!(s.is_server_error(), "the sealing fails: {s}");
    std::fs::remove_dir(&sealing).unwrap();

    // the app asks where to resume, then sends what is missing (nothing) or the last chunk again
    let (_, b) = status(&t, &[LOG]).await;
    let received = b["files"][LOG]["received"].as_u64().unwrap() as usize;
    let (s, b) = put(&t, LOG, &bytes, received.min(400), &bytes[received.min(400)..]).await;
    let mut finished = s == StatusCode::CREATED;
    if !finished {
        // or start over from offset 0
        let (s0, _) = put(&t, LOG, &bytes, 0, &bytes[..400]).await;
        finished = s0 == StatusCode::OK || s0 == StatusCode::CREATED;
        let (s1, _) = put(&t, LOG, &bytes, 400, &bytes[400..]).await;
        finished = finished && s1 == StatusCode::CREATED;
    }
    assert!(finished, "stuck: status says received {received} of 1000; retry answered {s} {b}");
}
