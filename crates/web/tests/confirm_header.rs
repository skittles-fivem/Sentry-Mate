#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used, clippy::result_large_err)]
//! The acting admin's own password in a header, where the body is not JSON (the UI pack upload). Made-up names only.

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

/// The pack upload's body is the zip, so the admin's password comes in a header: as UTF-8 text, or
/// percent-encoded in `X-Sentry-Mate-Password-Encoded` (what a browser can send for any password).
#[tokio::test]
async fn an_admin_whose_password_is_not_ascii_can_upload_a_pack() {
    let s = TestServer::start(&[1]).await;
    let mut admin = s.admin().await;
    let password = " pässwort-länger-zwölf-鍵 ";
    let r = admin
        .post(
            "/api/v1/accounts",
            json!({ "name": "second", "password": password, "role": "admin", "cars": "all",
                    "confirm_password": ADMIN_PASSWORD }),
        )
        .await;
    assert_eq!(r.status, 201, "{}", r.text());
    let mut second = s.client();
    let r = second.post("/api/v1/session", json!({ "name": "second", "password": password })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let r = second.put("/api/v1/two-step-policy", json!({ "everyone": false, "confirm_password": password })).await;
    assert_eq!(r.status, 200, "{}", r.text());

    // percent-encoded (as encodeURIComponent writes it): any password, its spaces at the ends too
    let encoded = "%20p%C3%A4sswort-l%C3%A4nger-zw%C3%B6lf-%E9%8D%B5%20";
    let r = upload(&mut second, &pack_zip("Umlaut"), &[("X-Sentry-Mate-Password-Encoded", encoded)]).await;
    assert_eq!(r.status, 201, "{}", r.text());
    // a wrong one is wrong, one that is not percent-encoded UTF-8 is a bad request
    let r = upload(&mut second, &pack_zip("Umlaut"), &[("X-Sentry-Mate-Password-Encoded", "p%C3%A4sswort")]).await;
    assert_eq!((r.status, r.code().as_deref()), (403, Some("wrong_password")), "{}", r.text());
    let r = upload(&mut second, &pack_zip("Umlaut"), &[("X-Sentry-Mate-Password-Encoded", "p%E4sswort")]).await;
    assert_eq!((r.status, r.code().as_deref()), (400, Some("bad_input")), "{}", r.text());
    assert!(r.json()["error"]["fields"]["confirm_password"].is_string(), "{}", r.text());

    // the plain header carries UTF-8 (HTTP trims the spaces at its ends, so this account's own needs the encoded one)
    let r = upload(&mut second, &pack_zip("Umlaut"), &[("X-Sentry-Mate-Password", password)]).await;
    assert_eq!((r.status, r.code().as_deref()), (403, Some("wrong_password")), "{}", r.text());
    let plain = "pässwort-länger-zwölf-鍵";
    let r = second.put("/api/v1/me/password", json!({ "password": password, "new_password": plain })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let mut second = s.client();
    let r = second.post("/api/v1/session", json!({ "name": "second", "password": plain })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let r = upload(&mut second, &pack_zip("Umlaut"), &[("X-Sentry-Mate-Password", plain)]).await;
    assert_eq!(r.status, 201, "the upload takes this admin's password as UTF-8: {}", r.text());
    // and a JSON route takes either header as well
    let r = second
        .request(
            "DELETE",
            "/api/v1/ui-packs/umlaut",
            None,
            &[("X-Sentry-Mate-Password-Encoded", "p%C3%A4sswort-l%C3%A4nger-zw%C3%B6lf-%E9%8D%B5")],
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
}
