#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! A move from the web pages over the real router (web::testkit), with a made-up old server on loopback: the job and
//! its id, a stopped move carried on with a new code, the finish with the recovery key (its throttle), and what a
//! moved server answers (`409 moved`, its notices, undo from the web). All made up: no real keys, tokens or data.

use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use web::testkit::{ADMIN_PASSWORD, TestServer};

struct Kit {
    s: TestServer,
    data: PathBuf,
    keys: PathBuf,
    _dir: tempfile::TempDir,
}

/// A server whose move side works on a folder of its own (`with_data`: it holds a made-up file of the owner's).
async fn kit(with_data: bool) -> Kit {
    let dir = tempfile::tempdir().unwrap();
    let (data, keys) = (dir.path().join("data"), dir.path().join("keys"));
    std::fs::create_dir_all(&data).unwrap();
    std::fs::create_dir_all(&keys).unwrap();
    vault::Vault::create(&data, &keys, vault::Kdf::FAST_FOR_TESTS).unwrap();
    let site = Arc::new(web::moving::MoveSite::new(data.clone(), keys.clone(), env!("CARGO_PKG_VERSION")));
    let s = TestServer::start_with(move |b| b.service(site)).await;
    if with_data {
        std::fs::create_dir_all(data.join("db")).unwrap();
        s.app.vault.write_file(&data.join("db").join("made-up"), "db/made-up", b"a made-up row").unwrap();
    }
    Kit { s, data, keys, _dir: dir }
}

/// A made-up old server: its own folders, a few sealed files, and its upload port with the move's routes.
struct Old {
    _dir: tempfile::TempDir,
    data: PathBuf,
    vault: Arc<vault::Vault>,
    recovery: String,
    address: String,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Old {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn old_server() -> Old {
    let dir = tempfile::tempdir().unwrap();
    let (data, keys) = (dir.path().join("data"), dir.path().join("keys"));
    std::fs::create_dir_all(&data).unwrap();
    let (v, rk) = vault::Vault::create(&data, &keys, vault::Kdf::FAST_FOR_TESTS).unwrap();
    let v = Arc::new(v);
    std::fs::create_dir_all(data.join("db")).unwrap();
    for i in 0..5 {
        v.write_file(&data.join("db").join(format!("made-up-{i}")), &format!("db/made-up-{i}"), b"a made-up row")
            .unwrap();
    }
    let id =
        ingest::server::Identity::load_or_create(&ingest::server::tls_dir(&data), &["localhost".into()], &v).unwrap();
    let gate = gate::Gate::open(&data).unwrap();
    let store = ingest::Store::with_limits(data.join("can"), v.clone(), 0, 200_000, |_| u64::MAX);
    let tokens = ingest::Tokens::open(data.join("tokens.json"), v.clone()).await.unwrap();
    let up = ingest::Uploads::with_gate(
        store.clone(),
        tokens,
        ingest::Judge::new(store),
        gate.clone(),
        Duration::from_secs(2),
    );
    let (d, v2, fp) = (data.clone(), v.clone(), id.fingerprint());
    let offer =
        tokio::task::spawn_blocking(move || moving::old::Offer::open(&d, v2, gate, env!("CARGO_PKG_VERSION"), &fp))
            .await
            .unwrap()
            .unwrap();
    let app = ingest::router(up).merge(moving::old::router(offer));
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let address = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
    let tls = id.tls_config().unwrap();
    let task = tokio::spawn(async move {
        let _ = ingest::server::serve_tls(listener, tls, app).await;
    });
    Old { _dir: dir, data, vault: v, recovery: rk.as_str().to_string(), address, task }
}

fn new_code(old: &Old) -> String {
    moving::old::make_code(&old.data, &old.vault, false, false).unwrap().code.to_string()
}

fn pull_opts() -> moving::pull::Options {
    let mut o = moving::pull::Options::new(env!("CARGO_PKG_VERSION"));
    o.min_free = 0;
    o.free_bytes = Box::new(|_| u64::MAX);
    o.retry_most = Duration::from_millis(200);
    o.between_rounds = Duration::from_millis(200);
    o.first_start_also = moving::import::WEB_FIRST_START.iter().map(|n| n.to_string()).collect();
    o
}

/// What the running server does between the page's calls: carries the move on until it stops.
async fn run_move(data: &Path, keys: &Path) -> Result<moving::pull::Done, moving::pull::Problem> {
    let (d, k) = (data.to_path_buf(), keys.to_path_buf());
    let mut o = pull_opts();
    o.stop_before_switch = true;
    tokio::task::spawn_blocking(move || moving::pull::run(&d, &k, &o)).await.unwrap()
}

fn audit_codes(k: &Kit) -> String {
    let page = k.s.app.accounts.audit().list(500, None).unwrap();
    page.entries.iter().map(|e| e.action.clone()).collect::<Vec<_>>().join(" ")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stopped_move_carries_on_and_finishes_with_the_recovery_key() {
    let old = old_server().await;
    let k = kit(false).await;
    let mut c = k.s.client();
    // nothing here yet: no job, and no move to finish
    assert_eq!(c.post("/api/v1/setup/claim", json!({ "setup_code": k.s.setup_code })).await.status, 200);
    let r = c.post("/api/v1/setup/move-here/finish", json!({ "recovery_key": old.recovery, "confirm": true })).await;
    assert_eq!((r.status, r.code()), (404, Some("no_such_job".into())), "{}", r.text());
    let r = c.post("/api/v1/setup/move-here/finish", json!({ "recovery_key": old.recovery })).await;
    assert_eq!(r.status, 400);

    // the move begins: a SetupJob with an id of its own
    let r = c.post("/api/v1/setup/move-here", json!({ "from": old.address, "code": new_code(&old) })).await;
    assert_eq!(r.status, 201, "{}", r.text());
    let job = r.json();
    let id = job["id"].as_str().unwrap().to_string();
    assert_eq!(id.len(), 8, "{job}");
    assert_eq!(job["kind"], "move_here");
    assert!(matches!(job["state"].as_str(), Some("copying" | "connecting")), "{job}");
    assert_eq!(job["progress"]["files_total"].as_u64().map(|n| n > 0), Some(true), "{job}");
    let started = job["started_ms"].clone();
    // the old server's notice: a new server copies it
    let now = k.s.clock_ms.load(std::sync::atomic::Ordering::Relaxed);
    let n = web::resources::notices::move_notices(&old.data, &old.vault, now, false);
    assert_eq!(n.iter().map(|n| n.code).collect::<Vec<_>>(), ["moving"]);
    assert!(n[0].message.contains("127.0.0.1"), "{:?}", n[0]);

    // cancelled on the old server: the run stops, the job is stopped
    moving::old::cancel(&old.data, &old.vault).unwrap();
    let p = run_move(&k.data, &k.keys).await.unwrap_err();
    assert_eq!(p.code, "cancelled");
    let j = c.get("/api/v1/setup/job").await.json();
    assert_eq!((j["state"].as_str(), j["problem"]["code"].as_str()), (Some("stopped"), Some("cancelled")), "{j}");
    assert_eq!(j["id"], json!(id));
    // nothing was copied: no switch to an empty copy (the old server still serves)
    assert_eq!((j["progress"]["bytes_done"].as_u64(), j["progress"]["files_done"].as_u64()), (Some(0), Some(0)), "{j}");
    let r = c.post("/api/v1/setup/move-here/finish", json!({ "recovery_key": old.recovery, "confirm": true })).await;
    assert_eq!((r.status, r.code()), (409, Some("not_ready".into())), "{}", r.text());
    assert!(matches!(moving::pull::pending(&k.data, &k.keys), moving::pull::Pending::Moving { forced: false, .. }));

    // another old server: give that move up first
    let r = c.post("/api/v1/setup/move-here", json!({ "from": "127.0.0.2:10100", "code": new_code(&old) })).await;
    assert_eq!((r.status, r.code()), (409, Some("setup_busy".into())), "{}", r.text());
    // the same old server with a new code: the same job carries on
    let r = c.post("/api/v1/setup/move-here", json!({ "from": old.address, "code": new_code(&old) })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let j = r.json();
    assert_eq!((j["id"].clone(), j["started_ms"].clone()), (json!(id), started), "{j}");
    assert!(j["problem"].is_null(), "{j}");

    // the copy runs, the old server answers: it finishes by itself, no finish with the recovery key
    let (d, ks) = (k.data.clone(), k.keys.clone());
    let run = tokio::task::spawn_blocking(move || {
        let mut o = pull_opts();
        o.stop_before_switch = true;
        moving::pull::run(&d, &ks, &o)
    });
    let t0 = std::time::Instant::now();
    while moving::pull::status(&k.data, &k.keys).unwrap().is_some_and(|s| s.progress.files_done == 0) {
        assert!(t0.elapsed() < Duration::from_secs(30));
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let r = c.post("/api/v1/setup/move-here/finish", json!({ "recovery_key": old.recovery, "confirm": true })).await;
    assert_eq!((r.status, r.code()), (409, Some("not_ready".into())), "{}", r.text());
    // then cancelled on the old server part way: stopped, with something copied
    moving::old::cancel(&old.data, &old.vault).unwrap();
    assert_eq!(run.await.unwrap().unwrap_err().code, "cancelled");
    let j = c.get("/api/v1/setup/job").await.json();
    assert_eq!((j["state"].as_str(), j["problem"]["code"].as_str()), (Some("stopped"), Some("cancelled")), "{j}");
    assert!(j["progress"]["files_done"].as_u64().is_some_and(|n| n > 0), "{j}");

    // a key that doesn't read: 400, not counted as a try
    for _ in 0..=web::moving::MAX_WRONG_CODES {
        let r = c.post("/api/v1/setup/move-here/finish", json!({ "recovery_key": "garbage", "confirm": true })).await;
        assert_eq!((r.status, r.code()), (400, Some("bad_input".into())), "{}", r.text());
        assert!(r.json()["error"]["fields"]["recovery_key"].is_string(), "{}", r.text());
    }
    // the old server is gone for good: finished with its recovery key
    let wrong = "SMRK-0000-0000-0000-0000-0000-0000-0000-0000-0000-0000-0000-0000-0000";
    for _ in 0..web::moving::MAX_WRONG_CODES {
        let r = c.post("/api/v1/setup/move-here/finish", json!({ "recovery_key": wrong, "confirm": true })).await;
        assert_eq!((r.status, r.code()), (422, Some("wrong_recovery_key".into())), "{}", r.text());
        assert!(r.json()["error"]["fields"]["recovery_key"].is_string(), "{}", r.text());
    }
    let r = c.post("/api/v1/setup/move-here/finish", json!({ "recovery_key": old.recovery, "confirm": true })).await;
    assert_eq!((r.status, r.code()), (429, Some("throttled".into())), "{}", r.text());
    assert!(r.header("retry-after").is_some());
    // a minute later (the server's clock moves on)
    k.s.advance(61_000);
    let r = c.post("/api/v1/setup/move-here/finish", json!({ "recovery_key": old.recovery, "confirm": true })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!((r.json()["state"].as_str(), r.json()["id"].as_str()), (Some("switching"), Some(id.as_str())));
    let r = c.delete("/api/v1/setup/job", json!({})).await;
    assert_eq!((r.status, r.code()), (409, Some("finished".into())), "{}", r.text());
    // the run switches next (the server starts again for it)
    assert!(matches!(moving::pull::pending(&k.data, &k.keys), moving::pull::Pending::Moving { forced: true, .. }));
    let p = run_move(&k.data, &k.keys).await.unwrap_err();
    assert_eq!(p.code, moving::pull::SWITCH_NEXT);
    let audit = audit_codes(&k);
    for a in ["setup-move-started", "setup-move-resumed", "setup-move-finished-forced"] {
        assert!(audit.contains(a), "{a}: {audit}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_move_given_up_is_recorded() {
    let old = old_server().await;
    let k = kit(false).await;
    let mut c = k.s.client();
    assert_eq!(c.post("/api/v1/setup/claim", json!({ "setup_code": k.s.setup_code })).await.status, 200);
    let r = c.post("/api/v1/setup/move-here", json!({ "from": old.address, "code": new_code(&old) })).await;
    assert_eq!(r.status, 201, "{}", r.text());
    let id = r.json()["id"].clone();
    // the same code again: the same job (200)
    let r = c.get("/api/v1/setup/job").await;
    assert_eq!((r.status, r.json()["id"].clone()), (200, id.clone()));
    let r = c.delete("/api/v1/setup/job", json!({})).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.json(), json!({ "deleted": true, "id": id }));
    assert_eq!(c.get("/api/v1/setup/job").await.code(), Some("no_such_job".into()));
    assert!(audit_codes(&k).contains("setup-move-cancelled"));
    // the old server was told: its move ended
    assert!(moving::old::status(&old.data, &old.vault).unwrap().code.is_none());
}

fn notice<'a>(list: &'a Value, code: &str) -> Option<&'a Value> {
    list["items"].as_array().unwrap().iter().find(|n| n["code"] == code)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_moved_server_reads_but_takes_no_changes() {
    let k = kit(true).await;
    let mut admin = k.s.admin().await;
    let mut watch = k.s.watch(&mut admin, &[]).await;
    let n = admin.get("/api/v1/notices").await.json();
    assert!(notice(&n, "move_code_waiting").is_none() && notice(&n, "moved").is_none(), "{n}");

    // a move code waiting
    let r = admin.post("/api/v1/system/move", json!({ "password": ADMIN_PASSWORD })).await;
    assert_eq!(r.status, 201, "{}", r.text());
    let expires = r.json()["code"]["expires_ms"].clone();
    let n = admin.get("/api/v1/notices").await.json();
    let w = notice(&n, "move_code_waiting").unwrap_or_else(|| panic!("{n}"));
    assert_eq!(w["detail"], json!({ "expires_ms": expires }));
    assert_eq!(w["level"], "info");
    // the move is an admin's to read, like /system/move
    assert_eq!(watch.get("/api/v1/system/move").await.status, 403);
    let n = watch.get("/api/v1/notices").await.json();
    assert!(notice(&n, "move_code_waiting").is_none(), "{n}");

    // moved: the new server is switching
    let v = k.s.app.vault.clone();
    moving::old::mark_moved_for_tests(&k.data, &v, "192.0.2.5:10100", false).unwrap();
    let m = admin.get("/api/v1/system/move").await.json();
    assert_eq!(m["state"], "moved", "{m}");
    let n = admin.get("/api/v1/notices").await.json();
    let mv = notice(&n, "moved").unwrap_or_else(|| panic!("{n}"));
    assert_eq!(mv["detail"]["to"], "192.0.2.5:10100");
    assert!(mv["detail"]["switched_ms"].is_null() && mv["detail"]["at_ms"].is_u64(), "{mv}");
    assert!(notice(&n, "move_code_waiting").is_none(), "{n}");
    let n = watch.get("/api/v1/notices").await.json();
    assert!(notice(&n, "moved").is_none() && notice(&n, "moving").is_none(), "{n}");

    // every change answers 409 moved, for every role, reading still works
    for (method, path, body) in [
        ("PATCH", "/api/v1/settings", json!({ "units": "metric" })),
        ("POST", "/api/v1/places", json!({ "name": "Made up", "lat": 1.0, "lon": 1.0, "radius_m": 50 })),
        ("POST", "/api/v1/accounts", json!({ "name": "madeup", "role": "watch" })),
        ("POST", "/api/v1/system/recovery-key/new", json!({ "password": ADMIN_PASSWORD })),
        ("DELETE", "/api/v1/places/1", json!({})),
    ] {
        let b = serde_json::to_vec(&body).unwrap();
        let h = [("Content-Type", "application/json"), ("X-Sentry-Mate", "1")];
        let r = admin.request(method, path, Some(&b), &h).await;
        assert_eq!((r.status, r.code()), (409, Some("moved".into())), "{method} {path}: {}", r.text());
    }
    let r = watch.post("/api/v1/trips", json!({ "name": "Made up" })).await;
    assert_eq!((r.status, r.code()), (409, Some("moved".into())), "{}", r.text());
    assert_eq!(admin.get("/api/v1/accounts").await.status, 200);
    assert_eq!(admin.get("/api/v1/audit").await.status, 200);
    // what still works: export files, the person's preferences, signing out and in, the move itself
    let r = admin.post("/api/v1/system/exports", json!({ "password": ADMIN_PASSWORD })).await;
    assert_eq!(r.status, 201, "{}", r.text());
    let r = admin.put("/api/v1/me/preferences", json!({})).await;
    assert_ne!(r.code(), Some("moved".into()), "{}", r.text());
    let r = watch.delete("/api/v1/session", json!({})).await;
    assert!(r.status < 300, "{}", r.text());
    let r = admin.post("/api/v1/system/move", json!({ "password": ADMIN_PASSWORD })).await;
    assert_eq!((r.status, r.code()), (409, Some("moved".into())), "a new code: undo first: {}", r.text());

    // done: the new server took over
    moving::old::mark_moved_for_tests(&k.data, &v, "192.0.2.5:10100", true).unwrap();
    assert_eq!(admin.get("/api/v1/system/move").await.json()["state"], "done");
    let n = admin.get("/api/v1/notices").await.json();
    let mv = notice(&n, "moved").unwrap_or_else(|| panic!("{n}"));
    assert!(mv["message"].as_str().unwrap().contains("turn it off"), "{mv}");
    assert!(mv["detail"]["switched_ms"].is_u64(), "{mv}");

    // undo from the web: changes are taken again
    let r = admin.post("/api/v1/system/move/undo", json!({ "password": ADMIN_PASSWORD, "confirm": true })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let u = r.json();
    assert_eq!(u["state"], "none", "{u}");
    assert!(u["last_undone_ms"].is_u64(), "{u}");
    let n = admin.get("/api/v1/notices").await.json();
    assert!(notice(&n, "moved").is_none(), "{n}");
    let r = admin.patch("/api/v1/settings", json!({})).await;
    assert_ne!(r.code(), Some("moved".into()), "{}", r.text());
    assert!(audit_codes(&k).contains("move-undone"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_moved_in_server_gives_the_certificate_it_serves() {
    use sha2::Digest as _;
    let k = kit(false).await;
    let mut c = k.s.client();
    assert_eq!(c.post("/api/v1/setup/claim", json!({ "setup_code": k.s.setup_code })).await.status, 200);
    // a forced switch to a copy with no tls folder: the server made a new certificate at start
    std::fs::create_dir_all(k.data.join("tls")).unwrap();
    std::fs::write(k.data.join("tls/cert.pem"), "-----BEGIN CERTIFICATE-----\nAAEC\n-----END CERTIFICATE-----\n")
        .unwrap();
    let from = moving::client::Address { host: "192.0.2.5".into(), port: 10100 };
    let old_cert = "08".repeat(32);
    moving::pull::mark_moved_in_for_tests(&k.data, &k.s.app.vault, from, &old_cert, true).unwrap();
    let j = c.get("/api/v1/setup/job").await;
    assert_eq!((j.status, j.json()["state"].as_str()), (200, Some("done")), "{}", j.text());
    let res = &j.json()["result"];
    assert_eq!(res["forced"], true, "{res}");
    assert_eq!(res["cert_sha256"], hex::encode(sha2::Sha256::digest([0u8, 1, 2])), "{res}");
}
