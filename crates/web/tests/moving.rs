#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! Moving a server and first-run set-up through the real router (web::testkit), over the moving crate on a made-up
//! data folder of its own: a move code made, shown once and revoked; an export file made, downloaded whole and in a
//! range, removed; the claim with the set-up code, a move here refused before anything runs, and starting fresh.

use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;
use web::testkit::{ADMIN, ADMIN_PASSWORD, TestServer};

struct Kit {
    s: TestServer,
    keys: PathBuf,
    _dir: tempfile::TempDir,
}

/// A server whose move side works on a made-up data folder (a few sealed files in it, made with the server's vault).
async fn kit() -> Kit {
    kit_with(true).await
}

/// The same; `with_data`: the folder holds a made-up file of the owner's (else it is as new).
async fn kit_with(with_data: bool) -> Kit {
    let dir = tempfile::tempdir().unwrap();
    let (data, keys) = (dir.path().join("data"), dir.path().join("keys"));
    std::fs::create_dir_all(&data).unwrap();
    if with_data {
        std::fs::create_dir_all(data.join("db")).unwrap();
    }
    std::fs::create_dir_all(&keys).unwrap();
    // the folder's own vault file (an export's header carries its recovery slot)
    vault::Vault::create(&data, &keys, vault::Kdf::FAST_FOR_TESTS).unwrap();
    let site = Arc::new(web::moving::MoveSite::new(data.clone(), keys.clone(), env!("CARGO_PKG_VERSION")));
    let s = TestServer::start_with(move |b| b.service(site)).await;
    if with_data {
        s.app.vault.write_file(&data.join("db").join("made-up"), "db/made-up", b"a made-up row").unwrap();
    }
    Kit { s, keys, _dir: dir }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_move_code_and_an_export_file() {
    let k = kit().await;
    let mut admin = k.s.admin().await;
    let mut watch = k.s.watch(&mut admin, &[]).await;

    let m = admin.get("/api/v1/system/move").await;
    assert_eq!(m.status, 200, "{}", m.text());
    assert_eq!(m.json()["state"], "none");
    for key in ["state", "code", "peer", "progress", "moved", "last_cancelled", "skipped", "problem"] {
        assert!(m.json().get(key).is_some(), "{key}");
    }
    assert_eq!(watch.get("/api/v1/system/move").await.status, 403);
    assert_eq!(watch.post("/api/v1/system/move", json!({ "password": "x" })).await.status, 403);

    // the password again
    let r = admin.post("/api/v1/system/move", json!({})).await;
    assert_eq!((r.status, r.code()), (400, Some("bad_input".into())));
    let r = admin.post("/api/v1/system/move", json!({ "password": "not it at all" })).await;
    assert_eq!((r.status, r.code()), (403, Some("wrong_password".into())), "{}", r.text());
    let body = serde_json::to_vec(&json!({ "password": ADMIN_PASSWORD })).unwrap();
    let r = admin
        .request(
            "POST",
            "/api/v1/system/move",
            Some(&body),
            &[("Content-Type", "application/json"), ("X-Sentry-Mate", "0")],
        )
        .await;
    assert_eq!((r.status, r.code()), (403, Some("csrf".into())));
    let r = admin.post("/api/v1/system/move", json!({ "password": ADMIN_PASSWORD })).await;
    assert_eq!(r.status, 201, "{}", r.text());
    let made = r.json();
    assert_eq!(made["state"], "offered");
    let code = made["move_code"].as_str().unwrap().to_string();
    assert!(code.starts_with("SMMV-"), "{code}");
    assert!(moving::code::MoveCode::parse(&code).is_ok());
    let id = made["code"]["id"].as_str().unwrap().to_string();
    let again = admin.get("/api/v1/system/move").await.json();
    assert_eq!(again["code"]["id"], json!(id));
    assert!(again.get("move_code").is_none() && !again.to_string().contains(&code), "shown once: {again}");
    assert_eq!(again["code"]["anywhere"], json!(false));
    // cancel: the code stops working
    let r = admin.delete("/api/v1/system/move", json!({})).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!((r.json()["cancelled"].as_str(), r.json()["state"].as_str()), (Some("revoked"), Some("none")));
    let r = admin.post("/api/v1/system/move/undo", json!({ "password": ADMIN_PASSWORD, "confirm": true })).await;
    assert_eq!((r.status, r.code()), (409, Some("not_moved".into())));
    let r = admin.post("/api/v1/system/move/undo", json!({ "password": ADMIN_PASSWORD })).await;
    assert_eq!(r.status, 400);

    // an export file
    assert_eq!(admin.get("/api/v1/system/exports").await.json(), json!({ "items": [], "next": null }));
    let r = admin.post("/api/v1/system/exports", json!({ "password": "wrong password here" })).await;
    assert_eq!(r.status, 403);
    let r = admin.post("/api/v1/system/exports", json!({ "password": ADMIN_PASSWORD })).await;
    assert_eq!(r.status, 201, "{}", r.text());
    let ex = r.json()["id"].as_str().unwrap().to_string();
    let mut one = serde_json::Value::Null;
    for _ in 0..400 {
        one = admin.get(&format!("/api/v1/system/exports/{ex}")).await.json();
        if one["state"] != "preparing" {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    assert_eq!(one["state"], "ready", "{one}");
    for key in ["file_name", "files", "bytes", "size", "sent_bytes", "made_ms", "expires_ms", "url", "skipped", "error"]
    {
        assert!(one.get(key).is_some(), "{key}: {one}");
    }
    let size = one["size"].as_u64().unwrap();
    assert!(one["files"].as_u64().unwrap() >= 1, "{one}");
    let url = one["url"].as_str().unwrap().to_string();
    assert_eq!(watch.get(&url).await.status, 403);
    let r = admin.get(&url).await;
    assert_eq!(r.status, 200);
    assert_eq!(r.body.len() as u64, size);
    assert!(r.header("content-disposition").unwrap().contains(".smexport"));
    assert_eq!(r.header("accept-ranges"), Some("bytes"));
    let whole = r.body.clone();
    let r = admin.request("GET", &url, None, &[("Range", "bytes=10-")]).await;
    assert_eq!(r.status, 206);
    assert_eq!(r.body, whole[10..].to_vec(), "a resumed download is the rest of the same file");
    assert_eq!(r.header("content-range"), Some(format!("bytes 10-{}/{size}", size - 1).as_str()));
    let r = admin.request("GET", &url, None, &[("Range", &format!("bytes={}-", size + 5))]).await;
    assert_eq!(r.status, 416);
    // the file opens with the recovery key: its header names this server's version
    assert!(String::from_utf8_lossy(&whole[..512.min(whole.len())]).contains(env!("CARGO_PKG_VERSION")));
    let r = admin.delete(&format!("/api/v1/system/exports/{ex}"), json!({})).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(admin.get(&url).await.code(), Some("export_gone".into()));
    assert_eq!(admin.get(&format!("/api/v1/system/exports/{ex}")).await.code(), Some("no_such_export".into()));
    let audit = admin.get("/api/v1/audit").await.json().to_string();
    for a in ["move-code-made", "move-cancelled", "export-made", "export-downloaded", "export-removed"] {
        assert!(audit.contains(a), "{a}: {audit}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_claim_a_move_here_refused_and_starting_fresh() {
    let k = kit().await;
    std::fs::write(k.keys.join(moving::import::RECOVERY_FILE), "MADE-UP-RECOVERY-KEY\n").unwrap();
    let mut c = k.s.client();

    // no claim: nothing
    let r = c.get("/api/v1/setup/job").await;
    assert_eq!((r.status, r.code()), (401, Some("not_claimed".into())));
    let r = c.post("/api/v1/setup/fresh", json!({ "name": ADMIN, "password": ADMIN_PASSWORD })).await;
    assert_eq!((r.status, r.code()), (401, Some("not_claimed".into())));
    let r = c.post("/api/v1/setup/claim", json!({ "setup_code": "ABCDE-FGHJK-LMNPQ-RSTUV" })).await;
    assert_eq!((r.status, r.code()), (403, Some("wrong_setup_code".into())));
    // case, spaces and dashes don't matter
    let typed = k.s.setup_code.to_lowercase().replace('-', " ");
    let r = c.post("/api/v1/setup/claim", json!({ "setup_code": typed })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let a = r.json();
    assert_eq!((a["claimed"].as_bool(), a["state"].as_str()), (Some(true), Some("fresh")));
    assert!(c.cookies.iter().any(|(n, _)| n == web::moving::CLAIM_COOKIE));
    let r = c.get("/api/v1/setup/job").await;
    assert_eq!((r.status, r.code()), (404, Some("no_such_job".into())));

    // a move here: checked before anything runs
    let r = c.post("/api/v1/setup/move-here", json!({ "from": "http://192.168.1.20", "code": "x" })).await;
    assert_eq!(r.status, 400, "{}", r.text());
    let r = c.post("/api/v1/setup/move-here", json!({ "from": "127.0.0.1:1", "code": "SMMV-NOT-A-CODE" })).await;
    assert_eq!(r.status, 400, "{}", r.text());
    assert!(r.json()["error"]["fields"]["code"].is_string(), "{}", r.text());
    // a real code (made on a made-up old server) to an address where nothing answers
    let old = tempfile::tempdir().unwrap();
    let (od, ok) = (old.path().join("data"), old.path().join("keys"));
    std::fs::create_dir_all(&od).unwrap();
    let (ov, _) = vault::Vault::create(&od, &ok, vault::Kdf::FAST_FOR_TESTS).unwrap();
    let made = moving::old::make_code(&od, &ov, false, false).unwrap();
    let r = c.post("/api/v1/setup/move-here", json!({ "from": "127.0.0.1:1", "code": made.code.as_str() })).await;
    assert_eq!((r.status, r.code()), (409, Some("not_empty".into())), "this folder holds data: {}", r.text());
    // into a folder as new: the old server is asked, and nothing answers there
    let n = kit_with(false).await;
    let mut c2 = n.s.client();
    assert_eq!(c2.post("/api/v1/setup/claim", json!({ "setup_code": n.s.setup_code })).await.status, 200);
    let r = c2.post("/api/v1/setup/move-here", json!({ "from": "127.0.0.1:1", "code": made.code.as_str() })).await;
    assert_eq!((r.status, r.code()), (502, Some("old_server_unreachable".into())), "{}", r.text());

    // start fresh: the first admin, signed in, and the recovery key once
    let r = c.post("/api/v1/setup/fresh", json!({ "name": ADMIN, "password": "" })).await;
    assert_eq!(r.status, 422, "{}", r.text());
    let r = c.post("/api/v1/setup/fresh", json!({ "name": ADMIN, "password": ADMIN_PASSWORD })).await;
    assert_eq!(r.status, 201, "{}", r.text());
    let f = r.json();
    assert_eq!(f["status"], "done");
    assert_eq!(f["me"]["name"], ADMIN);
    assert_eq!(f["recovery_key"], "MADE-UP-RECOVERY-KEY");
    let me = c.get("/api/v1/me").await;
    assert_eq!(me.status, 200, "signed in at once: {}", me.text());
    let r = c.post("/api/v1/setup/claim", json!({ "setup_code": k.s.setup_code })).await;
    assert_eq!((r.status, r.code()), (409, Some("setup_done".into())));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ten_wrong_set_up_codes_wait() {
    let k = kit().await;
    let mut c = k.s.client();
    for _ in 0..web::moving::MAX_WRONG_CODES {
        let r = c.post("/api/v1/setup/claim", json!({ "setup_code": "WRONG-WRONG" })).await;
        assert_eq!(r.status, 403);
    }
    let r = c.post("/api/v1/setup/claim", json!({ "setup_code": k.s.setup_code })).await;
    assert_eq!(r.status, 429, "{}", r.text());
    assert!(r.header("retry-after").is_some());
}

/// Waits for the export file `id` to be made.
async fn ready_export(admin: &mut web::testkit::Client, id: &str) -> serde_json::Value {
    for _ in 0..400 {
        let one = admin.get(&format!("/api/v1/system/exports/{id}")).await.json();
        if one["state"] != "preparing" {
            return one;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    panic!("the export file was not made");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_export_file_is_kept_a_day_after_the_last_download_and_a_week_at_most() {
    const DAY: u64 = web::moving::EXPORT_KEPT_MS;
    let k = kit().await;
    let mut admin = k.s.admin().await;
    let r = admin.post("/api/v1/system/exports", json!({ "password": ADMIN_PASSWORD })).await;
    assert_eq!(r.status, 201, "{}", r.text());
    let id = r.json()["id"].as_str().unwrap().to_string();
    let one = ready_export(&mut admin, &id).await;
    assert_eq!(one["state"], "ready", "{one}");
    let made = one["made_ms"].as_u64().unwrap();
    assert_eq!(one["expires_ms"].as_u64(), Some(made + DAY), "not downloaded yet: a day from when it was made");
    let url = one["url"].as_str().unwrap().to_string();
    // each download keeps it a day from then, never past a week from when it was made
    let step = DAY - 3_600_000;
    for i in 1..=8u64 {
        k.s.advance(step);
        let r = admin.request("GET", &url, None, &[("Range", "bytes=0-9")]).await;
        assert_eq!(r.status, 206, "{}", r.text());
        let one = admin.get(&format!("/api/v1/system/exports/{id}")).await.json();
        assert_eq!(one["state"], "ready", "{one}");
        let want = (made + i * step + DAY).min(made + web::moving::EXPORT_KEPT_MOST_MS);
        assert_eq!(one["expires_ms"].as_u64(), Some(want), "download {i}: {one}");
        if want == made + web::moving::EXPORT_KEPT_MOST_MS {
            break;
        }
    }
    let one = admin.get(&format!("/api/v1/system/exports/{id}")).await.json();
    assert_eq!(one["expires_ms"].as_u64(), Some(made + web::moving::EXPORT_KEPT_MOST_MS), "{one}");
    k.s.advance(DAY);
    let one = admin.get(&format!("/api/v1/system/exports/{id}")).await.json();
    assert_eq!(one["state"], "expired", "kept its time: expired, not failed: {one}");
    assert!(one["url"].is_null() && one["expires_ms"].is_null(), "{one}");
    assert_eq!(admin.get(&url).await.code(), Some("export_gone".into()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn get_setup_shows_the_claim_and_the_old_one_call_set_up_shows_the_recovery_key() {
    let k = kit().await;
    std::fs::write(k.keys.join(moving::import::RECOVERY_FILE), "MADE-UP-RECOVERY-KEY\n").unwrap();
    // the code as the log prints it: SETUP- and five groups of four
    let code = k.s.setup_code.clone();
    assert!(code.starts_with("SETUP-") && code.len() == 30, "{code}");
    let mut c = k.s.client();
    let fresh =
        json!({ "needs_setup": true, "state": "fresh", "claimed": false, "claim_expires_ms": null, "job": null });
    assert_eq!(c.get("/api/v1/setup").await.json(), fresh);
    // a cookie that is not the claim's
    let mut forged = k.s.client();
    forged.cookies.push((web::moving::CLAIM_COOKIE.to_string(), "0".repeat(64)));
    assert_eq!(forged.get("/api/v1/setup").await.json(), fresh);

    // the code without SETUP- in front, in lower case, works too
    let bare = code["SETUP-".len()..].to_lowercase();
    let r = c.post("/api/v1/setup/claim", json!({ "setup_code": bare })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let expires = r.json()["claim_expires_ms"].as_u64().unwrap();
    k.s.advance(600_000);
    // GET /setup looks without renewing the claim
    let g = c.get("/api/v1/setup").await.json();
    assert_eq!(g["claimed"], json!(true), "{g}");
    assert_eq!(g["state"], "fresh");
    assert_eq!(g["claim_expires_ms"].as_u64(), Some(expires), "not renewed: {g}");
    assert!(g.get("job").is_some_and(serde_json::Value::is_null), "no move or restore: {g}");
    assert_eq!(k.s.client().get("/api/v1/setup").await.json(), fresh, "another browser holds no claim");
    // an hour past its last use: gone
    k.s.advance(web::moving::CLAIM_MS);
    assert_eq!(c.get("/api/v1/setup").await.json(), fresh);

    // the old one-call set-up: checked input, the code as typed, the recovery key once
    let r = c.post("/api/v1/setup", json!({ "code": code, "name": ADMIN })).await;
    assert_eq!((r.status, r.code()), (400, Some("bad_input".into())), "{}", r.text());
    let r = c.post("/api/v1/setup", json!({ "code": code, "name": ADMIN, "password": ADMIN_PASSWORD, "x": 1 })).await;
    assert_eq!((r.status, r.code()), (400, Some("bad_input".into())), "{}", r.text());
    let r = c.post("/api/v1/setup", json!({ "code": 5, "name": ADMIN, "password": ADMIN_PASSWORD })).await;
    assert_eq!(r.status, 400, "{}", r.text());
    let r = c
        .post(
            "/api/v1/setup",
            json!({ "code": "SETUP-AAAA-BBBB-CCCC-DDDD-EEEE", "name": ADMIN, "password": ADMIN_PASSWORD }),
        )
        .await;
    assert_eq!((r.status, r.code()), (403, Some("wrong_setup_code".into())));
    let typed = code.to_lowercase().replace('-', " ");
    let r = c.post("/api/v1/setup", json!({ "code": typed, "name": ADMIN, "password": ADMIN_PASSWORD })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let f = r.json();
    assert_eq!(f["status"], "done");
    assert_eq!(f["me"]["name"], ADMIN);
    assert_eq!(f["recovery_key"], "MADE-UP-RECOVERY-KEY");
    assert_eq!(c.get("/api/v1/me").await.status, 200, "signed in at once");
    let done =
        json!({ "needs_setup": false, "state": "set_up", "claimed": false, "claim_expires_ms": null, "job": null });
    assert_eq!(c.get("/api/v1/setup").await.json(), done);
    let r = c.post("/api/v1/setup", json!({ "code": code, "name": "second", "password": ADMIN_PASSWORD })).await;
    assert_eq!((r.status, r.code()), (409, Some("setup_done".into())));
}
