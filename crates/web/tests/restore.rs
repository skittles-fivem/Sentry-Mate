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
    site: Arc<web::moving::MoveSite>,
    /// The web side after the switch (from [`kit_after`]): its own server on the same folders and claim.
    after: Option<TestServer>,
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
    kit_with(web::moving::ClaimKeep::default(), false, None).await
}

/// A test's stop at one of the move side's named points ([`web::moving::MoveSite::with_pause`]): the server waits
/// there until [`Stop::go`].
struct Stop {
    hit: tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<()>>,
    go: std::sync::mpsc::Sender<()>,
}

type Hook = Arc<dyn Fn(&str) + Send + Sync>;

fn stop_at(at: &'static str) -> (Arc<Stop>, Hook) {
    let (hit_tx, hit_rx) = tokio::sync::mpsc::unbounded_channel();
    let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
    let go_rx = std::sync::Mutex::new(go_rx);
    let hook: Hook = Arc::new(move |point: &str| {
        if point == at {
            hit_tx.send(()).unwrap();
            go_rx.lock().unwrap().recv().unwrap();
        }
    });
    (Arc::new(Stop { hit: tokio::sync::Mutex::new(hit_rx), go: go_tx }), hook)
}

impl Stop {
    async fn reached(&self) {
        tokio::time::timeout(std::time::Duration::from_secs(60), self.hit.lock().await.recv()).await.unwrap().unwrap();
    }

    fn go(&self) {
        self.go.send(()).unwrap();
    }
}

/// The same, the claim kept by the program, and the web side it starts after the switch: a server of its own on the
/// same folders and claim, whose data key the old server has (so the restored folder opens with it).
async fn kit_after() -> Kit {
    kit_with(web::moving::ClaimKeep::default(), true, None).await
}

async fn kit_with(keep: web::moving::ClaimKeep, later: bool, pause: Option<Hook>) -> Kit {
    let dir = tempfile::tempdir().unwrap();
    let (data, keys) = (dir.path().join("new/data"), dir.path().join("new/keys"));
    std::fs::create_dir_all(&data).unwrap();
    std::fs::create_dir_all(&keys).unwrap();
    let mut opts = moving::Options::new(APP);
    opts.min_free = 0;
    opts.checkpoint = 1;
    let after = if later {
        let mut o = moving::Options::new(APP);
        o.min_free = 0;
        let site = Arc::new(
            web::moving::MoveSite::new(data.clone(), keys.clone(), APP)
                .with_import_options(o)
                .keeping_claim(keep.clone()),
        );
        Some(TestServer::start_with(move |b| b.service(site)).await)
    } else {
        None
    };
    let mut site =
        web::moving::MoveSite::new(data.clone(), keys.clone(), APP).with_import_options(opts).keeping_claim(keep);
    if let Some(p) = pause {
        site = site.with_pause(move |at| p(at));
    }
    let site = Arc::new(site);
    let s2 = site.clone();
    let s = TestServer::start_with(move |b| b.service(s2)).await;
    let from = after.as_ref().unwrap_or(&s).dir.path().to_path_buf();
    // the old server: the test server's vault (copied), a recovery key of its own, and a few files
    let (od, ok) = (dir.path().join("old/data"), dir.path().join("old/keys"));
    std::fs::create_dir_all(&od).unwrap();
    std::fs::create_dir_all(&ok).unwrap();
    std::fs::copy(from.join("data").join(vault::VAULT_FILE), od.join(vault::VAULT_FILE)).unwrap();
    std::fs::copy(from.join("keys").join(vault::KEY_FILE), ok.join(vault::KEY_FILE)).unwrap();
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
    Kit { s, site, after, data, keys, file, recovery, _dir: dir }
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
    // the claim before the chunk's headers and body
    let r = c.request("PATCH", URL, Some(b"x"), &[("Content-Type", "text/plain")]).await;
    assert_eq!((r.status, r.code()), (401, Some("not_claimed".into())), "{}", r.text());
    let r = c.request("PATCH", URL, Some(b"x"), &[]).await;
    assert_eq!((r.status, r.code()), (401, Some("not_claimed".into())), "{}", r.text());
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

/// The whole file in chunks of at most `CHUNK_MAX`.
async fn upload_all(c: &mut Client, file: &[u8]) -> Value {
    let mut at = 0usize;
    let mut last = Value::Null;
    while at < file.len() {
        let end = (at + web::resources::imports::CHUNK_MAX).min(file.len());
        let r = patch(c, at as u64, &file[at..end], &[]).await;
        assert_eq!(r.status, 200, "at {at}: {}", r.text());
        last = r.json();
        at = end;
    }
    last
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_old_one_call_set_up_waits_for_a_restore() {
    let k = kit().await;
    let mut c = claimed(&k).await;
    assert_eq!(c.post(URL, start_body(&k, "a.smexport")).await.status, 201);
    // GET /setup says a restore runs: to every browser, with the job only for the one that holds the claim
    let mut other = k.s.client();
    let g = other.get("/api/v1/setup").await.json();
    assert_eq!(
        g,
        json!({ "needs_setup": true, "state": "restoring", "claimed": false, "claim_expires_ms": null, "job": null })
    );
    let g = c.get("/api/v1/setup").await.json();
    assert_eq!((g["state"].as_str(), g["claimed"].as_bool()), (Some("restoring"), Some(true)), "{g}");
    assert_eq!((g["job"]["kind"].as_str(), g["job"]["state"].as_str()), (Some("restore"), Some("uploading")), "{g}");
    // a wrong code is a wrong code while a restore runs too (and counted)
    let r = other
        .post(
            "/api/v1/setup",
            json!({ "code": "SETUP-0000-0000", "name": "someone", "password": "a long made-up password" }),
        )
        .await;
    assert_eq!((r.status, r.code()), (403, Some("wrong_setup_code".into())), "{}", r.text());
    // no claim: the set-up code, a name and a password in one call
    let r = other
        .post(
            "/api/v1/setup",
            json!({ "code": k.s.setup_code, "name": "someone", "password": "a long made-up password" }),
        )
        .await;
    assert_eq!((r.status, r.code()), (409, Some("setup_busy".into())), "{}", r.text());
    assert!(k.s.app.accounts.needs_setup(), "no account made");
    // the restore goes on
    let r = patch(&mut c, 0, &k.file[..1000], &[]).await;
    assert_eq!(r.status, 200, "{}", r.text());
    // given up: the old call works again
    assert_eq!(c.delete("/api/v1/setup/job", json!({})).await.status, 200);
    k.s.admin().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_recovery_key_is_tried_at_a_time() {
    let k = kit().await;
    let mut c = claimed(&k).await;
    let held = k.site.key_work().unwrap();
    let r = c.post(URL, start_body(&k, "a.smexport")).await;
    assert_eq!((r.status, r.code()), (503, Some("busy".into())), "{}", r.text());
    assert!(r.header("retry-after").is_some());
    drop(held);
    // a header asking for more memory than this server's own keys use: refused before Argon2id runs
    let n = u32::from_be_bytes(k.file[5..9].try_into().unwrap()) as usize;
    let mut header: Value = serde_json::from_slice(&k.file[9..9 + n]).unwrap();
    let m = header["recovery"]["kdf"]["m"].as_u64().unwrap();
    assert!(m > 0, "{header}");
    header["recovery"]["kdf"]["m"] = json!(vault::Kdf::STRONG.m * 4);
    let h = serde_json::to_vec(&header).unwrap();
    let mut big = k.file[..5].to_vec();
    big.extend_from_slice(&(h.len() as u32).to_be_bytes());
    big.extend_from_slice(&h);
    big.extend_from_slice(&k.file[9 + n..]);
    let b = json!({ "name": "a.smexport", "size": big.len(), "recovery_key": k.recovery, "header": head64(&big) });
    let r = c.post(URL, b).await;
    assert_eq!(r.status, 422, "{}", r.text());
    // the right key, once nothing else runs
    let r = c.post(URL, start_body(&k, "a.smexport")).await;
    assert_eq!(r.status, 201, "{}", r.text());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_fault_of_this_server_while_storing_says_choose_the_file_again() {
    let k = kit().await;
    let mut c = claimed(&k).await;
    assert_eq!(c.post(URL, start_body(&k, "a.smexport")).await.status, 201);
    assert_eq!(patch(&mut c, 0, &k.file[..100_000], &[]).await.status, 200);
    // the staged folder of the files still to come is a plain file now: the next file can't be made
    let can = k.data.join(moving::import::STAGING).join("data/can");
    assert!(can.is_dir());
    let kept = k._dir.path().join("can-kept");
    std::fs::rename(&can, &kept).unwrap();
    std::fs::write(&can, b"in the way").unwrap();
    let r = patch(&mut c, 100_000, &k.file[100_000..], &[]).await;
    assert_eq!(r.status, 500, "{}", r.text());
    let r = patch(&mut c, 100_000, &k.file[100_000..], &[]).await;
    assert_eq!((r.status, r.code()), (409, Some("not_uploading".into())), "{}", r.text());
    let m = r.json()["error"]["message"].as_str().unwrap_or("").to_string();
    assert!(m.contains("choose the same file again") && !m.contains("is uploading"), "{m}");
    // the same file again carries on
    std::fs::remove_file(&can).unwrap();
    std::fs::rename(&kept, &can).unwrap();
    let r = c.post(URL, start_body(&k, "a.smexport")).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let from = r.json()["received"].as_u64().unwrap();
    let r = patch(&mut c, from, &k.file[from as usize..], &[]).await;
    assert_eq!((r.status, r.json()["state"].as_str()), (200, Some("checking")), "{}", r.text());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn after_the_switch_the_browser_that_restored_sees_done() {
    let mut k = kit_after().await;
    let mut c = claimed(&k).await;
    assert_eq!(c.post(URL, start_body(&k, "a.smexport")).await.status, 201);
    assert_eq!(upload_all(&mut c, &k.file).await["state"], "checking");
    // the program's switch, and the web side started again: the claim is still this browser's
    moving::import::switch(&k.data, &k.keys).unwrap();
    let s2 = k.after.take().unwrap();
    let mut c2 = s2.client();
    c2.cookies = c.cookies.clone();
    let j = c2.get("/api/v1/setup/job").await;
    assert_eq!((j.status, j.json()["state"].as_str()), (200, Some("done")), "{}", j.text());
    assert_eq!(j.json()["result"]["cert_sha256"], hex::encode(sha2::Sha256::digest([0u8, 1, 2])));
    // the export brought accounts: still for this browser, for this call only
    s2.admin().await;
    let j = c2.get("/api/v1/setup/job").await;
    assert_eq!((j.status, j.json()["state"].as_str()), (200, Some("done")), "{}", j.text());
    let r = c2.delete("/api/v1/setup/job", json!({})).await;
    assert_eq!((r.status, r.code()), (409, Some("setup_done".into())), "{}", r.text());
    let r = s2.client().get("/api/v1/setup/job").await;
    assert_eq!((r.status, r.code()), (409, Some("setup_done".into())), "{}", r.text());
}

const SETUP_URL: &str = "/api/v1/setup";

fn setup_body(k: &Kit) -> Value {
    json!({ "code": k.s.setup_code, "name": "someone", "password": "a long made-up password" })
}

/// No restore half done on a server that is set up: the first admin is made while a restore's key is checked.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_first_admin_made_while_a_key_is_checked_stops_the_restore() {
    let (stop, hook) = stop_at("restore-key-opened");
    let k = kit_with(web::moving::ClaimKeep::default(), false, Some(hook)).await;
    let mut c = claimed(&k).await;
    let body = start_body(&k, "a.smexport");
    let restore = tokio::spawn(async move { c.post(URL, body).await });
    stop.reached().await;
    let r = k.s.client().post(SETUP_URL, setup_body(&k)).await;
    assert_eq!(r.status, 200, "{}", r.text());
    stop.go();
    let r = restore.await.unwrap();
    assert_eq!((r.status, r.code()), (409, Some("setup_done".into())), "{}", r.text());
    assert!(matches!(moving::import::pending(&k.data, &k.keys), moving::Pending::None), "nothing staged");
}

/// The other way round: a restore begun while the first admin's password is hashed waits, then sees it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_restore_begun_while_the_first_admin_is_made_waits_and_stops() {
    let (stop, hook) = stop_at("setup-ready");
    let k = kit_with(web::moving::ClaimKeep::default(), false, Some(hook)).await;
    let mut c = claimed(&k).await;
    let mut other = k.s.client();
    let body = setup_body(&k);
    let setup = tokio::spawn(async move { other.post(SETUP_URL, body).await });
    stop.reached().await;
    let body = start_body(&k, "a.smexport");
    let mut restore = tokio::spawn(async move { c.post(URL, body).await });
    // the restore waits for the first admin (its key check takes a moment with the test's fast settings)
    let early = tokio::time::timeout(std::time::Duration::from_secs(3), &mut restore).await;
    assert!(
        early.is_err(),
        "the restore went on while the first admin was made: {:?}",
        early.map(|r| r.unwrap().text())
    );
    stop.go();
    let r = setup.await.unwrap();
    assert_eq!(r.status, 200, "{}", r.text());
    let r = restore.await.unwrap();
    assert_eq!((r.status, r.code()), (409, Some("setup_done".into())), "{}", r.text());
    assert!(matches!(moving::import::pending(&k.data, &k.keys), moving::Pending::None), "nothing staged");
}
