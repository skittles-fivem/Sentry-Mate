#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used, clippy::result_large_err)]
//! Uploaded UI packs whose name or zip is an edge case. Made-up names only.

use serde_json::json;
use web::testkit::{ADMIN_PASSWORD, Client, Res, TestServer};

fn zip(files: &[(&str, &[u8])]) -> Vec<u8> {
    let list: Vec<(String, &[u8])> = files.iter().map(|(n, d)| (n.to_string(), *d)).collect();
    web::packs::zip::write(&list)
}

fn pack_zip(name: &str) -> Vec<u8> {
    let ui = json!({ "name": name, "version": "1", "api": 1 }).to_string();
    zip(&[("ui.json", ui.as_bytes()), ("web/index.html", b"<!doctype html><title>t</title>")])
}

async fn upload(c: &mut Client, zip: &[u8], headers: &[(&str, &str)]) -> Res {
    let mut h = vec![("Content-Type", "application/zip")];
    h.extend_from_slice(headers);
    c.request("POST", "/api/v1/ui-packs", Some(zip), &h).await
}

/// `PUT /ui-packs/chosen` is a fixed route, so no pack may have the id `chosen`; one that has it (installed
/// before the name was refused) can still be removed.
#[tokio::test]
async fn no_pack_is_named_chosen_and_one_that_was_can_be_removed() {
    let s = TestServer::start(&[1]).await;
    let mut admin = s.admin().await;
    for name in ["Chosen", "chosen!", "  CHOSEN  "] {
        let r = upload(&mut admin, &pack_zip(name), &[("X-Sentry-Mate-Password", ADMIN_PASSWORD)]).await;
        assert_eq!((r.status, r.code().as_deref()), (422, Some("invalid")), "{name}: {}", r.text());
        assert!(r.json()["error"]["fields"]["name"].is_string(), "{}", r.text());
    }
    let r = admin.get("/api/v1/ui-packs").await;
    assert!(!r.text().contains("\"chosen\":\"chosen\"") && !r.text().contains("\"id\":\"chosen\""), "{}", r.text());
    // nothing to remove yet
    let r = admin.delete("/api/v1/ui-packs/chosen", json!({ "confirm_password": ADMIN_PASSWORD })).await;
    assert_eq!((r.status, r.code().as_deref()), (404, Some("no_such_pack")), "{}", r.text());

    // a pack with that id from before
    let mut old = web::packs::check_zip(&pack_zip("Earlier")).unwrap_or_else(|_| panic!("a good zip"));
    old.meta.id = "chosen".into();
    old.meta.name = "Chosen".into();
    assert!(s.app.packs.install(old, 1).is_ok());
    assert!(admin.get("/api/v1/ui-packs").await.text().contains("\"id\":\"chosen\""));
    let r = admin.delete("/api/v1/ui-packs/chosen", json!({})).await;
    assert_eq!(r.code().as_deref(), Some("password_needed"), "{}", r.text());
    let r = admin.delete("/api/v1/ui-packs/chosen", json!({ "confirm_password": ADMIN_PASSWORD })).await;
    assert_eq!(r.status, 200, "an uploaded pack must be removable whatever its id: {}", r.text());
    assert_eq!(r.json(), json!({ "deleted": true, "id": "chosen" }));
    let log = admin.get("/api/v1/audit").await.json();
    assert_eq!(
        (log["items"][0]["action"].as_str(), log["items"][0]["target"].as_str()),
        (Some("ui-pack-removed"), Some("ui-pack:chosen")),
        "{log}"
    );
    assert!(!admin.get("/api/v1/ui-packs").await.text().contains("\"id\":\"chosen\""));
    // the choice for everyone still answers there
    let r = admin.put("/api/v1/ui-packs/chosen", json!({ "pack": "nope", "confirm_password": ADMIN_PASSWORD })).await;
    assert_eq!(r.status, 422, "{}", r.text());
}

/// A zip without ui.json is told so, whatever else is in it.
#[tokio::test]
async fn a_zip_without_ui_json_is_refused_for_that() {
    let s = TestServer::start(&[1]).await;
    let mut admin = s.admin().await;
    let page: &[u8] = b"<!doctype html><title>t</title>";
    let cases: [&[(&str, &[u8])]; 4] = [
        &[("web/index.html", page)],
        &[("web/index.html", page), ("web/a.js", b"1")],
        &[("web/index.html", page), ("readme.txt", b"hello")],
        &[("night/web/index.html", page)],
    ];
    for files in cases {
        let r = upload(&mut admin, &zip(files), &[("X-Sentry-Mate-Password", ADMIN_PASSWORD)]).await;
        assert_eq!(r.status, 422, "{}", r.text());
        assert_eq!(r.json()["error"]["fields"]["zip"], json!("The pack has no ui.json."), "{files:?}");
    }
    // one folder around ui.json and web/ is still fine, and a stray file is still named
    let ui = json!({ "name": "Wrapped", "version": "1", "api": 1 }).to_string();
    let wrapped = zip(&[("night/ui.json", ui.as_bytes()), ("night/web/index.html", page)]);
    let r = upload(&mut admin, &wrapped, &[("X-Sentry-Mate-Password", ADMIN_PASSWORD)]).await;
    assert_eq!((r.status, r.json()["id"].as_str()), (201, Some("wrapped")), "{}", r.text());
    let stray = zip(&[("ui.json", ui.as_bytes()), ("web/index.html", page), ("readme.txt", b"hello")]);
    let r = upload(&mut admin, &stray, &[("X-Sentry-Mate-Password", ADMIN_PASSWORD)]).await;
    assert_eq!(r.json()["error"]["fields"]["zip"], json!("readme.txt: a pack holds only ui.json and the web/ folder."));
}
