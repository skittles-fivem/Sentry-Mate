#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! Restoring from an export file through the real router (web::testkit): the checks before the upload (each error
//! code), the chunks fed as they arrive, a file that fails part way, giving up, and the switch to `done`. The export
//! is a real one, made by the moving crate from a made-up old server whose data key is the test server's own (so the
//! restored folder opens with the server's vault, as it does after the real switch and restart).

use base64::Engine as _;
use serde_json::{Value, json};
use sha2::Digest;
use std::path::PathBuf;
use std::sync::Arc;
use web::testkit::{Client, TestServer};

struct Kit {
    s: TestServer,
    data: PathBuf,
    keys: PathBuf,
    /// The made-up export file and the recovery key that opens it.
    file: Vec<u8>,
    recovery: String,
    _dir: tempfile::TempDir,
}

const APP: &str = env!("CARGO_PKG_VERSION");
const URL: &str = "/api/v1/setup/restore";

/// An empty new server (its move side on folders of its own) and an export of a made-up old server.
async fn kit() -> Kit {
    let dir = tempfile::tempdir().unwrap();
    let (data, keys) = (dir.path().join("new/data"), dir.path().join("new/keys"));
    std::fs::create_dir_all(&data).unwrap();
    std::fs::create_dir_all(&keys).unwrap();
    let mut opts = moving::Options::new(APP);
    opts.min_free = 0;
    opts.checkpoint = 1;
    let site = Arc::new(web::moving::MoveSite::new(data.clone(), keys.clone(), APP).with_import_options(opts));
    let s = TestServer::start_with(move |b| b.service(site)).await;
    // the old server: the test server's vault (copied), a recovery key of its own, and a few files
    let (od, ok) = (dir.path().join("old/data"), dir.path().join("old/keys"));
    std::fs::create_dir_all(&od).unwrap();
    std::fs::create_dir_all(&ok).unwrap();
    std::fs::copy(s.dir.path().join("data").join(vault::VAULT_FILE), od.join(vault::VAULT_FILE)).unwrap();
    std::fs::copy(s.dir.path().join("keys").join(vault::KEY_FILE), ok.join(vault::KEY_FILE)).unwrap();
    let v = vault::Vault::open(&od, &ok).unwrap();
    let recovery = v.new_recovery_key(&od, vault::Kdf::FAST_FOR_TESTS).unwrap().as_str().to_string();
    std::fs::create_dir_all(od.join("can")).unwrap();
    for i in 0..3u32 {
        let b: Vec<u8> = (0..180_000u32).map(|x| (x.wrapping_mul(i + 3) % 247) as u8).collect();
        std::fs::write(od.join(format!("can/big{i}.bin")), b).unwrap();
    }
    std::fs::create_dir_all(od.join("tls")).unwrap();
    std::fs::write(od.join("tls/cert.pem"), "-----BEGIN CERTIFICATE-----\nAAEC\n-----END CERTIFICATE-----\n").unwrap();
    v.write_file(&od.join("can/made-up"), "can/made-up", b"a made-up row").unwrap();
    let file = export_of(&od, &v, APP);
    Kit { s, data, keys, file, recovery, _dir: dir }
}

fn export_of(data: &std::path::Path, v: &vault::Vault, app: &str) -> Vec<u8> {
    let gate = gate::Gate::open(data).unwrap();
    let snap = moving::Snapshot::make(data, v, &gate, app, std::time::Duration::from_secs(5)).unwrap();
    let mut out = Vec::new();
    snap.write_to(v, &mut out).unwrap();
    out
}

fn head64(file: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(&file[..file.len().min(65_536)])
}

fn start_body(k: &Kit, name: &str) -> Value {
    json!({ "name": name, "size": k.file.len(), "recovery_key": k.recovery, "header": head64(&k.file) })
}

async fn claimed(k: &Kit) -> Client {
    let mut c = k.s.client();
    let r = c.post("/api/v1/setup/claim", json!({ "setup_code": k.s.setup_code })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    c
}

async fn patch(c: &mut Client, offset: u64, bytes: &[u8], extra: &[(&str, &str)]) -> web::testkit::Res {
    let off = offset.to_string();
    let mut h = vec![("Content-Type", "application/offset+octet-stream"), ("Upload-Offset", off.as_str())];
    h.extend_from_slice(extra);
    c.request("PATCH", URL, Some(bytes), &h).await
}

fn field(r: &web::testkit::Res, f: &str) -> bool {
    r.json()["error"]["fields"][f].is_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_file_and_the_key_are_checked_before_the_upload() {
    let k = kit().await;
    let mut c = k.s.client();
    // no claim
    let r = c.post(URL, start_body(&k, "a.smexport")).await;
    assert_eq!((r.status, r.code()), (401, Some("not_claimed".into())), "{}", r.text());
    let r = patch(&mut c, 0, b"x", &[]).await;
    assert_eq!((r.status, r.code()), (401, Some("not_claimed".into())));
    let mut c = claimed(&k).await;
    let r = patch(&mut c, 0, b"x", &[]).await;
    assert_eq!((r.status, r.code()), (404, Some("no_such_job".into())), "{}", r.text());

    // the fields
    let r = c.post(URL, json!({})).await;
    assert_eq!(r.status, 400);
    for f in ["name", "size", "recovery_key", "header"] {
        assert!(field(&r, f), "{f}: {}", r.text());
    }
    let r = c.post(URL, json!({ "name": "a", "size": 1, "recovery_key": "", "header": "", "more": 1 })).await;
    assert_eq!(r.status, 400, "only these fields");
    let r = c.post(URL, json!({ "name": "a", "size": "10", "recovery_key": 4, "header": [] })).await;
    assert_eq!(r.status, 400);
    for f in ["size", "recovery_key", "header"] {
        assert!(field(&r, f), "{f}: {}", r.text());
    }
    let mut b = start_body(&k, "a.smexport");
    b["size"] = json!(0);
    let r = c.post(URL, b.clone()).await;
    assert_eq!((r.status, r.code(), field(&r, "size")), (422, Some("invalid".into()), true), "{}", r.text());
    b["size"] = json!(65u64 << 40);
    assert_eq!(c.post(URL, b.clone()).await.status, 422);
    b["size"] = json!(-3);
    assert_eq!(c.post(URL, b.clone()).await.status, 422);
    let mut b = start_body(&k, "a.smexport");
    b["header"] = json!("not base64 at all!");
    let r = c.post(URL, b.clone()).await;
    assert_eq!((r.status, field(&r, "header")), (400, true), "{}", r.text());
    b["header"] = json!(base64::engine::general_purpose::STANDARD.encode(&k.file[..100]));
    let r = c.post(URL, b.clone()).await;
    assert_eq!((r.status, field(&r, "header")), (400, true), "the first 64 KiB: {}", r.text());
    let mut b = start_body(&k, "a.smexport");
    b["recovery_key"] = json!("SMRK-AAAA-BBBB");
    let r = c.post(URL, b).await;
    assert_eq!((r.status, field(&r, "recovery_key")), (400, true), "{}", r.text());

    // the header
    let mut zip = vec![0u8; 5000];
    zip[..4].copy_from_slice(b"PK\x03\x04");
    let b = json!({ "name": "photos.zip", "size": 5000, "recovery_key": k.recovery,
                    "header": base64::engine::general_purpose::STANDARD.encode(&zip) });
    let r = c.post(URL, b).await;
    assert_eq!((r.status, r.code(), field(&r, "header")), (422, Some("not_an_export".into()), true), "{}", r.text());
    let mut b = start_body(&k, "a.smexport");
    b["size"] = json!(k.file.len() - 1);
    let r = c.post(URL, b).await;
    assert_eq!((r.status, r.code()), (422, Some("export_cut_short".into())), "{}", r.text());
    let mut newer = k.file.clone();
    newer[4] = 9;
    let b = json!({ "name": "a.smexport", "size": newer.len(), "recovery_key": k.recovery, "header": head64(&newer) });
    let r = c.post(URL, b).await;
    assert_eq!((r.status, r.code()), (422, Some("export_too_new".into())), "{}", r.text());
    let od = k._dir.path().join("old/data");
    let v = vault::Vault::open(&od, &k._dir.path().join("old/keys")).unwrap();
    let by_newer = export_of(&od, &v, "99.0.0");
    let b = json!({ "name": "a.smexport", "size": by_newer.len(), "recovery_key": k.recovery,
                    "header": head64(&by_newer) });
    let r = c.post(URL, b).await;
    assert_eq!((r.status, r.code()), (422, Some("export_too_new".into())), "{}", r.text());

    // the key: wrong, then throttled
    let (_v, other) =
        vault::Vault::create(&k._dir.path().join("x/data"), &k._dir.path().join("x/keys"), vault::Kdf::FAST_FOR_TESTS)
            .unwrap();
    let mut b = start_body(&k, "a.smexport");
    b["recovery_key"] = json!(other.as_str());
    for _ in 0..web::moving::MAX_WRONG_CODES {
        let r = c.post(URL, b.clone()).await;
        assert_eq!((r.status, r.code(), field(&r, "recovery_key")), (422, Some("wrong_recovery_key".into()), true));
    }
    let r = c.post(URL, start_body(&k, "a.smexport")).await;
    assert_eq!((r.status, r.code()), (429, Some("throttled".into())), "{}", r.text());
    assert!(r.header("retry-after").is_some());
    assert!(!k.data.join(moving::import::STAGING).exists(), "nothing written");
    k.s.advance(61_000);
    let r = c.post(URL, start_body(&k, "a.smexport")).await;
    assert_eq!(r.status, 201, "a minute later: {}", r.text());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_upload_in_chunks_then_checking_switching_and_done() {
    let k = kit().await;
    let mut c = claimed(&k).await;
    let n = k.file.len() as u64;
    let r = c.post(URL, start_body(&k, "sentry-mate-2026-11-17.smexport")).await;
    assert_eq!(r.status, 201, "{}", r.text());
    let j = r.json();
    assert_eq!((j["kind"].as_str(), j["state"].as_str()), (Some("restore"), Some("uploading")));
    assert_eq!((j["received"].as_u64(), j["size"].as_u64()), (Some(0), Some(n)));
    assert_eq!(j["upload_url"], URL);
    assert_eq!(j["chunk_max_bytes"], json!(web::resources::imports::CHUNK_MAX));
    assert_eq!(j["file_name"], "sentry-mate-2026-11-17.smexport");
    assert!(j["from"].is_null() && j["result"].is_null() && j["problem"].is_null());
    let header: Value =
        serde_json::from_slice(&k.file[9..9 + u32::from_be_bytes(k.file[5..9].try_into().unwrap()) as usize]).unwrap();
    assert_eq!(j["id"], header["id"]);
    assert_eq!(j["progress"]["files_total"], header["files"]);
    assert_eq!(j["progress"]["bytes_total"].as_u64(), Some(n));
    for key in ["round", "files_done", "bytes_done", "cars_done", "cars_total", "bytes_per_s", "eta_s"] {
        assert!(j["progress"].get(key).is_some(), "{key}");
    }
    // the same file again: the same job
    let r = c.post(URL, start_body(&k, "sentry-mate-2026-11-17.smexport")).await;
    assert_eq!((r.status, r.json()["id"].clone()), (200, j["id"].clone()), "{}", r.text());
    // another file: busy
    let r = c.post(URL, start_body(&k, "another-name.smexport")).await;
    assert_eq!((r.status, r.code()), (409, Some("setup_busy".into())), "{}", r.text());
    // a fresh start meanwhile: busy
    let r = c.post("/api/v1/setup/fresh", json!({ "name": "someone", "password": "a long made-up password" })).await;
    assert_eq!((r.status, r.code()), (409, Some("setup_busy".into())), "{}", r.text());

    // the chunk's headers
    let r = c.request("PATCH", URL, Some(b"abc"), &[("Upload-Offset", "0")]).await;
    assert_eq!((r.status, field(&r, "Content-Type")), (400, true), "{}", r.text());
    let r = c.request("PATCH", URL, Some(b"abc"), &[("Content-Type", "application/offset+octet-stream")]).await;
    assert_eq!((r.status, field(&r, "Upload-Offset")), (400, true));
    let r = patch(&mut c, 0, b"", &[]).await;
    assert_eq!(r.status, 400);
    let r = patch(&mut c, 0, &k.file[..10], &[("Upload-Length", "12")]).await;
    assert_eq!((r.status, r.code()), (422, Some("invalid".into())));
    let r = patch(&mut c, 0, &k.file[..10], &[("Upload-SHA256", "zz")]).await;
    assert_eq!((r.status, field(&r, "Upload-SHA256")), (400, true));
    let r = patch(&mut c, 0, &k.file[..10], &[("Upload-SHA256", &hex::encode([7u8; 32]))]).await;
    assert_eq!((r.status, r.code()), (422, Some("checksum_mismatch".into())));
    let r = patch(&mut c, 5, &k.file[5..10], &[]).await;
    assert_eq!((r.status, r.code()), (409, Some("wrong_offset".into())));
    assert_eq!((r.json()["error"]["offset"].as_u64(), r.header("upload-offset")), (Some(0), Some("0")));

    // the file in chunks of odd sizes, some with their SHA-256 (hex or base64), some with the length
    let len = n.to_string();
    let mut at = 0usize;
    let mut i = 0usize;
    while at < k.file.len() {
        let size = [70_001usize, 1, 65_576, 200_000, 9, 131_152][i % 6];
        let end = (at + size).min(k.file.len());
        let piece = &k.file[at..end];
        let sha = sha2::Sha256::digest(piece);
        let (hex_sha, b64_sha) = (hex::encode(sha), base64::engine::general_purpose::STANDARD.encode(sha));
        let extra: Vec<(&str, &str)> = match i % 3 {
            0 => vec![("Upload-SHA256", hex_sha.as_str()), ("Upload-Length", len.as_str())],
            1 => vec![("Upload-SHA256", b64_sha.as_str())],
            _ => vec![],
        };
        k.s.advance(500);
        let r = patch(&mut c, at as u64, piece, &extra).await;
        assert_eq!(r.status, 200, "at {at}: {}", r.text());
        at = end;
        assert_eq!(r.json()["received"].as_u64(), Some(at as u64));
        if at < k.file.len() {
            assert_eq!(r.json()["state"], "uploading");
            // the speed from the second chunk on (this server's clock moved half a second each)
            assert_eq!(r.json()["progress"]["bytes_per_s"].as_u64().is_some(), i > 0, "{}", r.text());
        }
        i += 1;
    }
    let j = c.get("/api/v1/setup/job").await.json();
    assert_eq!(j["state"], "checking", "{j}");
    assert_eq!(j["progress"]["files_done"], header["files"]);
    assert!(matches!(moving::import::pending(&k.data, &k.keys), moving::Pending::Ready { .. }));
    let r = patch(&mut c, n, b"more", &[]).await;
    assert_eq!((r.status, r.code()), (409, Some("not_uploading".into())), "{}", r.text());

    // the program's switch (stopped after its first step: switching)
    assert!(moving::import::switch_steps(&k.data, &k.keys, 1).is_err());
    let j = c.get("/api/v1/setup/job").await.json();
    assert_eq!(j["state"], "switching", "{j}");
    let r = c.delete("/api/v1/setup/job", json!({})).await;
    assert_eq!((r.status, r.code()), (409, Some("finished".into())), "{}", r.text());
    moving::import::switch(&k.data, &k.keys).unwrap();
    let j = c.get("/api/v1/setup/job").await.json();
    assert_eq!(j["state"], "done", "{j}");
    let res = &j["result"];
    assert_eq!(res["file_name"], "sentry-mate-2026-11-17.smexport");
    assert_eq!((res["forced"].as_bool(), res["same_machine"].as_bool()), (Some(false), Some(false)));
    assert!(res["from"].is_null() && res["at_ms"].as_u64().is_some());
    assert_eq!(res["cert_sha256"], hex::encode(sha2::Sha256::digest([0u8, 1, 2])));
    assert!(k.data.join("can/big2.bin").exists(), "the files are in place");
    let r = c.delete("/api/v1/setup/job", json!({})).await;
    assert_eq!((r.status, r.code()), (409, Some("finished".into())));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_damaged_file_fails_part_way_and_giving_up_removes_it() {
    let k = kit().await;
    let mut c = claimed(&k).await;
    assert_eq!(c.post(URL, start_body(&k, "a.smexport")).await.status, 201);
    let mut bad = k.file.clone();
    let mid = bad.len() / 2;
    bad[mid] ^= 0x04;
    let r = patch(&mut c, 0, &bad[..mid - 1000], &[]).await;
    assert_eq!((r.status, r.json()["state"].as_str()), (200, Some("uploading")), "{}", r.text());
    let r = patch(&mut c, (mid - 1000) as u64, &bad[mid - 1000..], &[]).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let j = r.json();
    assert_eq!((j["state"].as_str(), j["problem"]["code"].as_str()), (Some("failed"), Some("export_damaged")), "{j}");
    assert!(!k.data.join(moving::import::STAGING).exists(), "nothing kept");
    let r = patch(&mut c, j["received"].as_u64().unwrap(), b"x", &[]).await;
    assert_eq!((r.status, r.code()), (409, Some("not_uploading".into())));
    // given up: the job goes, and the file can be chosen again
    let r = c.delete("/api/v1/setup/job", json!({})).await;
    assert_eq!((r.status, r.json()["deleted"].as_bool()), (200, Some(true)), "{}", r.text());
    assert_eq!(r.json()["id"], j["id"]);
    assert_eq!(c.get("/api/v1/setup/job").await.code(), Some("no_such_job".into()));
    let r = c.post(URL, start_body(&k, "a.smexport")).await;
    assert_eq!(r.status, 201, "{}", r.text());
    // given up while it uploads: the staging goes too
    assert_eq!(patch(&mut c, 0, &k.file[..200_000], &[]).await.status, 200);
    assert!(k.data.join(moving::import::STAGING).exists());
    let r = c.delete("/api/v1/setup/job", json!({})).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert!(!k.data.join(moving::import::STAGING).exists());
    assert_eq!(moving::import::pending(&k.data, &k.keys), moving::Pending::None);
    // the audit (no account yet: the address only)
    let mut admin = k.s.admin().await;
    let audit = admin.get("/api/v1/audit").await.json().to_string();
    for a in ["setup-restore-started", "setup-restore-cancelled"] {
        assert!(audit.contains(a), "{a}: {audit}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn after_a_restart_the_same_file_carries_on_from_a_chunk_boundary() {
    let k = kit().await;
    let mut c = claimed(&k).await;
    assert_eq!(c.post(URL, start_body(&k, "a.smexport")).await.status, 201);
    let cut = 300_000usize;
    assert_eq!(patch(&mut c, 0, &k.file[..cut], &[]).await.status, 200);
    // a new web side on the same folders (the server started again): the claim and the open import are gone
    let mut opts = moving::Options::new(APP);
    opts.min_free = 0;
    opts.checkpoint = 1;
    let site = Arc::new(web::moving::MoveSite::new(k.data.clone(), k.keys.clone(), APP).with_import_options(opts));
    let s2 = TestServer::start_with(move |b| b.service(site)).await;
    let mut c2 = s2.client();
    assert_eq!(c2.post("/api/v1/setup/claim", json!({ "setup_code": s2.setup_code })).await.status, 200);
    let j = c2.get("/api/v1/setup/job").await.json();
    assert_eq!((j["kind"].as_str(), j["state"].as_str()), (Some("restore"), Some("uploading")), "{j}");
    let r = patch(&mut c2, cut as u64, &k.file[cut..cut + 10], &[]).await;
    assert_eq!((r.status, r.code()), (409, Some("not_uploading".into())), "choose the file again: {}", r.text());
    let r = c2.post(URL, start_body(&k, "a.smexport")).await;
    assert_eq!(r.status, 200, "the same file carries on: {}", r.text());
    let from = r.json()["received"].as_u64().unwrap();
    let body = 9 + u64::from(u32::from_be_bytes(k.file[5..9].try_into().unwrap()));
    assert!(from <= cut as u64 && from >= body && (from - body) % (65_536 + 40) == 0, "a chunk boundary: {from}");
    let r = patch(&mut c2, cut as u64, &k.file[cut..cut + 10], &[]).await;
    assert_eq!((r.status, r.code()), (409, Some("wrong_offset".into())));
    assert_eq!(r.json()["error"]["offset"].as_u64(), Some(from));
    let r = patch(&mut c2, from, &k.file[from as usize..], &[]).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.json()["state"], "checking", "{}", r.text());
    assert!(matches!(moving::import::pending(&k.data, &k.keys), moving::Pending::Ready { .. }));
}
