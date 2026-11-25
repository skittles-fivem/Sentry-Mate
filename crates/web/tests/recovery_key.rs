#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! The recovery key from the web pages through the real router (web::testkit): shown again from its file behind the
//! password, replaced by a new one that opens the vault (the old one stops), and the file removed by "I have saved
//! it", with the notice and the audit log following.

use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use web::testkit::{ADMIN_PASSWORD, Client, TestServer};

struct Kit {
    s: TestServer,
    data: PathBuf,
    keys: PathBuf,
    first_key: String,
    dir: tempfile::TempDir,
}

/// A server whose keys folder holds the recovery key file a first start writes. The data folder's vault file is the
/// server's own (so a new key opens the server's data key).
async fn kit() -> Kit {
    let dir = tempfile::tempdir().unwrap();
    let (data, keys) = (dir.path().join("data"), dir.path().join("keys"));
    std::fs::create_dir_all(&data).unwrap();
    std::fs::create_dir_all(&keys).unwrap();
    let site = Arc::new(
        web::moving::MoveSite::new(data.clone(), keys.clone(), env!("CARGO_PKG_VERSION"))
            .with_kdf(vault::Kdf::FAST_FOR_TESTS),
    );
    let notices = Arc::new(web::resources::notices::Sources {
        recovery_file: Some(keys.join(moving::import::RECOVERY_FILE)),
        ..Default::default()
    });
    let s = TestServer::start_with(move |b| b.service(site).service(notices)).await;
    std::fs::copy(s.dir.path().join("data").join(vault::VAULT_FILE), data.join(vault::VAULT_FILE)).unwrap();
    // the key a first start would have written (a made-up one: the server's own was not kept by the test kit)
    let rk = s.app.vault.new_recovery_key(&data, vault::Kdf::FAST_FOR_TESTS).unwrap();
    moving::import::write_recovery_file(&keys, &rk).unwrap();
    let first_key = rk.as_str().to_string();
    Kit { s, data, keys, first_key, dir }
}

fn codes(notices: &Value) -> Vec<String> {
    notices["items"].as_array().unwrap().iter().map(|n| n["code"].as_str().unwrap().to_string()).collect()
}

async fn audit(admin: &mut Client) -> String {
    admin.get("/api/v1/audit?limit=50").await.text()
}

/// Whether `key` opens the vault of the data folder (on a copy, so nothing here changes).
fn opens(k: &Kit, key: &str) -> bool {
    let n = k.dir.path().join(format!("try-{}", rand_name()));
    let (d, ks) = (n.join("data"), n.join("keys"));
    std::fs::create_dir_all(&d).unwrap();
    std::fs::create_dir_all(&ks).unwrap();
    std::fs::copy(k.data.join(vault::VAULT_FILE), d.join(vault::VAULT_FILE)).unwrap();
    match vault::Vault::restore(&d, &ks, key) {
        Ok(v) => {
            assert_eq!(v.key_id(), k.s.app.vault.key_id());
            true
        }
        Err(_) => false,
    }
}

fn rand_name() -> u64 {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    N.fetch_add(1, Ordering::Relaxed)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn show_new_and_saved() {
    let k = kit().await;
    let file = k.keys.join(moving::import::RECOVERY_FILE);
    let mut admin = k.s.admin().await;
    let mut watch = k.s.watch(&mut admin, &[]).await;
    let pw = json!({ "password": ADMIN_PASSWORD });
    assert!(opens(&k, &k.first_key));

    // admins only, signed in, with the CSRF header
    for path in ["show", "new", "saved"] {
        let p = format!("/api/v1/system/recovery-key/{path}");
        let r = watch.post(&p, pw.clone()).await;
        assert_eq!((r.status, r.code()), (403, Some("not_allowed".into())), "{path}: {}", r.text());
        assert_eq!(k.s.client().post(&p, pw.clone()).await.status, 401, "{path}");
        let body = serde_json::to_vec(&pw).unwrap();
        let r = admin
            .request("POST", &p, Some(&body), &[("Content-Type", "application/json"), ("X-Sentry-Mate", "0")])
            .await;
        assert_eq!((r.status, r.code()), (403, Some("csrf".into())), "{path}");
        assert_eq!(admin.get(&p).await.status, 405, "{path}");
    }
    assert_eq!(codes(&admin.get("/api/v1/notices").await.json()), ["recovery_key_unsaved"]);

    // show: the password again, and nothing else in the body
    let r = admin.post("/api/v1/system/recovery-key/show", json!({})).await;
    assert_eq!((r.status, r.code()), (400, Some("bad_input".into())));
    let r = admin.post("/api/v1/system/recovery-key/show", json!({ "password": ADMIN_PASSWORD, "more": 1 })).await;
    assert_eq!((r.status, r.code()), (400, Some("bad_input".into())));
    let r = admin.post("/api/v1/system/recovery-key/show", json!({ "password": "not it at all" })).await;
    assert_eq!((r.status, r.code()), (403, Some("wrong_password".into())), "{}", r.text());
    assert_eq!(r.json()["error"]["fields"]["password"], "Not right.");
    assert!(!r.text().contains(&k.first_key));
    let r = admin.post("/api/v1/system/recovery-key/show", pw.clone()).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.json(), json!({ "recovery_key": k.first_key }));
    assert!(audit(&mut admin).await.contains("recovery-key-shown"));

    // new: the same checks, then a new key in the file that opens the vault; the old one stops
    let r = admin.post("/api/v1/system/recovery-key/new", json!({ "password": "not it at all" })).await;
    assert_eq!((r.status, r.code()), (403, Some("wrong_password".into())));
    let r = admin.post("/api/v1/system/recovery-key/new", json!({ "password": ADMIN_PASSWORD, "kdf": 1 })).await;
    assert_eq!((r.status, r.code()), (400, Some("bad_input".into())));
    assert_eq!(moving::import::recovery_from_file(&file).unwrap().as_str(), k.first_key);
    let r = admin.post("/api/v1/system/recovery-key/new", pw.clone()).await;
    assert_eq!(r.status, 201, "{}", r.text());
    let new_key = r.json()["recovery_key"].as_str().unwrap().to_string();
    assert!(new_key.starts_with("SMRK-") && new_key != k.first_key, "{new_key}");
    assert_eq!(r.json()["made_ms"], json!(k.s.app.now()));
    assert_eq!(moving::import::recovery_from_file(&file).unwrap().as_str(), new_key);
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.starts_with("Sentry Mate recovery key:") && text.contains("then delete this file."), "{text}");
    assert!(opens(&k, &new_key));
    assert!(!opens(&k, &k.first_key));
    let r = admin.post("/api/v1/system/recovery-key/show", pw.clone()).await;
    assert_eq!(r.json()["recovery_key"], json!(new_key));
    assert!(audit(&mut admin).await.contains("recovery-key-new"));

    // saved: only no fields; the file and the notice go; show then says so
    let r = admin.post("/api/v1/system/recovery-key/saved", json!({ "password": ADMIN_PASSWORD })).await;
    assert_eq!((r.status, r.code()), (400, Some("bad_input".into())));
    assert!(file.exists());
    let r = admin.request("POST", "/api/v1/system/recovery-key/saved", None, &[("X-Sentry-Mate", "1")]).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.json(), json!({ "saved": true }));
    assert!(!file.exists());
    assert_eq!(codes(&admin.get("/api/v1/notices").await.json()), Vec::<String>::new());
    let r = admin.post("/api/v1/system/recovery-key/saved", json!({})).await;
    assert_eq!((r.status, r.json()), (200, json!({ "saved": true })), "a missing file is fine");
    let r = admin.post("/api/v1/system/recovery-key/show", pw.clone()).await;
    assert_eq!((r.status, r.code()), (409, Some("recovery_key_saved".into())), "{}", r.text());
    let log = audit(&mut admin).await;
    assert!(log.contains("recovery-key-saved") && log.contains("its file was removed"), "{log}");
    assert!(!log.contains(&new_key) && !log.contains(&k.first_key), "no key in the audit log");

    // a new key after that puts the file and the notice back
    let r = admin.post("/api/v1/system/recovery-key/new", pw.clone()).await;
    assert_eq!(r.status, 201, "{}", r.text());
    assert!(file.exists());
    assert_eq!(codes(&admin.get("/api/v1/notices").await.json()), ["recovery_key_unsaved"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_new_key_waits_for_a_move_and_asks_for_the_two_step_code() {
    let k = kit().await;
    let file = k.keys.join(moving::import::RECOVERY_FILE);
    let mut admin = k.s.admin().await;
    let pw = json!({ "password": ADMIN_PASSWORD });

    // a move coming in here: no new key
    std::fs::create_dir_all(k.data.join(moving::pull::STAGING)).unwrap();
    let r = admin.post("/api/v1/system/recovery-key/new", pw.clone()).await;
    assert_eq!((r.status, r.code()), (409, Some("setup_busy".into())), "{}", r.text());
    assert_eq!(moving::import::recovery_from_file(&file).unwrap().as_str(), k.first_key);
    std::fs::remove_dir(k.data.join(moving::pull::STAGING)).unwrap();

    // two-step sign-in on: the code too
    let r = admin.post("/api/v1/me/two-step", json!({})).await;
    assert_eq!(r.status, 201, "{}", r.text());
    let secret = accounts::totp::base32_decode(r.json()["secret"].as_str().unwrap()).unwrap();
    let code_now =
        || accounts::totp::code_for_step(&secret, accounts::totp::step_at(k.s.clock_ms.load(Ordering::Relaxed)));
    let r = admin.post("/api/v1/me/two-step/confirm", json!({ "password": ADMIN_PASSWORD, "code": code_now() })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    k.s.advance(60_000);
    for path in ["show", "new"] {
        let p = format!("/api/v1/system/recovery-key/{path}");
        let r = admin.post(&p, json!({ "password": ADMIN_PASSWORD, "two_step": "000000" })).await;
        assert_eq!((r.status, r.code()), (403, Some("wrong_two_step".into())), "{path}: {}", r.text());
        assert_eq!(r.json()["error"]["fields"]["two_step"], "Not right.");
        k.s.advance(60_000);
    }
    let r = admin
        .post("/api/v1/system/recovery-key/show", json!({ "password": ADMIN_PASSWORD, "two_step": code_now() }))
        .await;
    assert_eq!((r.status, r.json()), (200, json!({ "recovery_key": k.first_key })), "{}", r.text());
    k.s.advance(60_000);
    let r = admin
        .post("/api/v1/system/recovery-key/new", json!({ "password": ADMIN_PASSWORD, "two_step": code_now() }))
        .await;
    assert_eq!(r.status, 201, "{}", r.text());
    assert!(opens(&k, r.json()["recovery_key"].as_str().unwrap()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_key_file_that_cannot_be_written_leaves_the_old_key_working() {
    let k = kit().await;
    let file = k.keys.join(moving::import::RECOVERY_FILE);
    let mut admin = k.s.admin().await;
    let pw = json!({ "password": ADMIN_PASSWORD });
    let vault_before = std::fs::read(k.data.join(vault::VAULT_FILE)).unwrap();

    // a folder where the key file goes: it can't be written (nor read, to put it back), so the vault keeps its key
    let kept = std::fs::read(&file).unwrap();
    std::fs::remove_file(&file).unwrap();
    std::fs::create_dir(&file).unwrap();
    let r = admin.post("/api/v1/system/recovery-key/new", pw.clone()).await;
    assert_eq!((r.status, r.code()), (500, Some("server_error".into())), "{}", r.text());
    assert!(std::fs::read(k.data.join(vault::VAULT_FILE)).unwrap() == vault_before, "the vault file is the same");
    assert!(opens(&k, &k.first_key), "the old key still opens the vault");
    std::fs::remove_dir(&file).unwrap();
    std::fs::write(&file, kept).unwrap();
    let r = admin.post("/api/v1/system/recovery-key/show", pw.clone()).await;
    assert_eq!((r.status, r.json()), (200, json!({ "recovery_key": k.first_key })), "{}", r.text());
    assert_eq!(codes(&admin.get("/api/v1/notices").await.json()), ["recovery_key_unsaved"]);
    assert!(!audit(&mut admin).await.contains("recovery-key-new"));

    // once it can be written, a new key works as before
    let r = admin.post("/api/v1/system/recovery-key/new", pw.clone()).await;
    assert_eq!(r.status, 201, "{}", r.text());
    let new_key = r.json()["recovery_key"].as_str().unwrap().to_string();
    assert_eq!(moving::import::recovery_from_file(&file).unwrap().as_str(), new_key);
    assert!(opens(&k, &new_key) && !opens(&k, &k.first_key));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wrong_passwords_are_throttled_like_sign_in() {
    let k = kit().await;
    let mut admin = k.s.admin().await;
    let mut last = None;
    for _ in 0..30 {
        let r = admin.post("/api/v1/system/recovery-key/show", json!({ "password": "not it at all" })).await;
        if r.status == 429 {
            last = Some(r);
            break;
        }
        assert_eq!((r.status, r.code()), (403, Some("wrong_password".into())));
    }
    let r = last.expect("throttled at last");
    assert_eq!(r.code(), Some("throttled".into()));
    assert!(r.header("retry-after").is_some(), "{}", r.text());
    let r = admin.post("/api/v1/system/recovery-key/show", json!({ "password": ADMIN_PASSWORD })).await;
    assert_eq!(r.status, 429, "the right password waits too: {}", r.text());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_the_move_side_it_is_not_ready() {
    let s = TestServer::start(&[]).await;
    let mut admin = s.admin().await;
    let r = admin.post("/api/v1/system/recovery-key/saved", json!({})).await;
    assert_eq!((r.status, r.code()), (503, Some("not_ready".into())), "{}", r.text());
}
