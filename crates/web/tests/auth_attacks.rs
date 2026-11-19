#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used, clippy::result_large_err)]
//! Attacks on the sign-in rules through the API (a real TLS server): each test fails without its fix. Made-up names
//! and cars only.

use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use web::testkit::{ADMIN, ADMIN_PASSWORD, Client, TestServer};
use web::{Event, FleetChange};

async fn server() -> TestServer {
    let cars: std::collections::HashSet<u32> = [1, 2].into_iter().collect();
    TestServer::start_with(move |b| b.cars(Arc::new(web::FixedCars(cars)))).await
}

/// Turns two-step sign-in on for the client's account; returns its secret and recovery codes.
async fn two_step_on(s: &TestServer, c: &mut Client, password: &str) -> (Vec<u8>, Vec<String>) {
    let r = c.post("/api/v1/me/two-step", json!({})).await;
    assert_eq!(r.status, 201, "{}", r.text());
    let secret = accounts::totp::base32_decode(r.json()["secret"].as_str().unwrap()).unwrap();
    let code = code_now(s, &secret);
    let r = c.post("/api/v1/me/two-step/confirm", json!({ "password": password, "code": code })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let codes = r.json()["recovery_codes"].as_array().unwrap().iter().map(|c| c.as_str().unwrap().into()).collect();
    (secret, codes)
}

fn code_now(s: &TestServer, secret: &[u8]) -> String {
    accounts::totp::code_for_step(secret, accounts::totp::step_at(s.clock_ms.load(Ordering::Relaxed)))
}

#[tokio::test]
async fn an_open_stream_ends_when_the_two_step_rule_cuts_its_account_off() {
    let s = server().await;
    let mut admin = s.admin().await;
    let mut watch = s.watch(&mut admin, &[1]).await;
    let (_, codes) = two_step_on(&s, &mut admin, ADMIN_PASSWORD).await;
    let events = s.app.events.clone();
    // the watch account's page is open; then the admin requires two-step sign-in for everyone, and a car changes
    let change = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        let r = admin
            .put(
                "/api/v1/two-step-policy",
                json!({ "everyone": true, "confirm_password": ADMIN_PASSWORD, "code": codes[0] }),
            )
            .await;
        assert_eq!(r.status, 200, "{}", r.text());
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        events.publish(Event::Fleet { car: 1, change: FleetChange::Renamed, name: Some("Renamed-after-rule".into()) });
        let whole = Arc::new(json!({ "state": "driving", "speed_kmh": { "value": 88, "at_ms": 1 } }));
        events.publish(Event::Now { car: 1, whole: whole.clone(), delta: whole });
    });
    let text = watch.events_for(8000).await;
    change.await.unwrap();
    assert!(text.contains("retry: 3000"), "the stream opened: {text}");
    assert!(!text.contains("Renamed-after-rule"), "a car change reached a cut-off account: {text}");
    assert!(!text.contains("speed_kmh"), "live values reached a cut-off account: {text}");
    assert!(text.ends_with("\r\n0\r\n\r\n"), "the stream ended: {text}");
    // and a new request is refused, as before
    let r = watch.get("/api/v1/events").await;
    assert_eq!((r.status, r.code().as_deref()), (403, Some("two_step_setup")));
}

#[tokio::test]
async fn an_admin_with_two_step_sign_in_gives_a_code_for_account_changes() {
    let s = server().await;
    let mut admin = s.admin().await;
    let mut watch = s.watch(&mut admin, &[1]).await;
    let (secret, codes) = two_step_on(&s, &mut admin, ADMIN_PASSWORD).await;
    two_step_on(&s, &mut watch, web::testkit::WATCH_PASSWORD).await;
    let list = admin.get("/api/v1/accounts").await.json();
    let w = list["items"].as_array().unwrap().iter().find(|a| a["name"] != ADMIN).unwrap().clone();
    let id = w["id"].as_str().unwrap().to_string();
    let wrong_code = |r: &web::testkit::Res, what: &str| {
        assert_eq!((r.status, r.code().as_deref()), (401, Some("wrong_code")), "{what}: {}", r.text());
    };
    // the admin's browser and password, but not the phone: nothing changes
    let intruder = json!({ "name": "intruder", "password": "an intruder passphrase", "role": "admin",
                           "confirm_password": ADMIN_PASSWORD });
    wrong_code(&admin.post("/api/v1/accounts", intruder.clone()).await, "a new admin");
    let off = json!({ "confirm_password": ADMIN_PASSWORD });
    wrong_code(&admin.delete(&format!("/api/v1/accounts/{id}/two-step"), off).await, "another's two-step off");
    let rule = json!({ "everyone": true, "confirm_password": ADMIN_PASSWORD });
    wrong_code(&admin.put("/api/v1/two-step-policy", rule).await, "the rule");
    let reset =
        json!({ "version": w["version"], "password": "a takeover passphrase", "confirm_password": ADMIN_PASSWORD });
    wrong_code(&admin.patch(&format!("/api/v1/accounts/{id}"), reset).await, "another's password");
    // (a right code now and then: missing codes count against the code throttle)
    let r =
        admin.post("/api/v1/me/two-step/recovery-codes", json!({ "password": ADMIN_PASSWORD, "code": codes[9] })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let codes: Vec<String> =
        r.json()["recovery_codes"].as_array().unwrap().iter().map(|c| c.as_str().unwrap().into()).collect();
    let own = json!({ "password": ADMIN_PASSWORD, "new_password": "a takeover passphrase" });
    wrong_code(&admin.put("/api/v1/me/password", own).await, "own password");
    wrong_code(&admin.post("/api/v1/me/two-step/recovery-codes", json!({ "password": ADMIN_PASSWORD })).await, "codes");
    wrong_code(&admin.delete("/api/v1/me/two-step", json!({ "password": ADMIN_PASSWORD })).await, "own two-step off");
    assert_eq!(admin.get("/api/v1/accounts").await.json()["total"], 2);
    assert_eq!(admin.get(&format!("/api/v1/accounts/{id}")).await.json()["two_step"], true);
    assert_eq!(admin.get("/api/v1/two-step-policy").await.json()["everyone"], false);

    // with the code
    let mut with = intruder.clone();
    with["code"] = Value::from(codes[0].clone());
    with["name"] = Value::from("helper");
    assert_eq!(admin.post("/api/v1/accounts", with).await.status, 201);
    // several changes in one request take one code (in the header here)
    s.advance(31_000);
    let code = code_now(&s, &secret);
    let r = admin
        .request(
            "PATCH",
            &format!("/api/v1/accounts/{id}"),
            Some(
                json!({ "version": w["version"], "role": "admin", "cars": ["c1", "c2"],
                        "confirm_password": ADMIN_PASSWORD })
                .to_string()
                .as_bytes(),
            ),
            &[("content-type", "application/json"), (web::accounts_admin::CODE_HEADER, &code)],
        )
        .await;
    assert_eq!((r.status, r.json()["role"].as_str()), (200, Some("admin")), "{}", r.text());
    assert_eq!(r.json()["cars"], json!(["c1", "c2"]));
    let off = json!({ "confirm_password": ADMIN_PASSWORD, "code": codes[1] });
    let r = admin.delete(&format!("/api/v1/accounts/{id}/two-step"), off).await;
    assert_eq!((r.status, &r.json()["two_step"]), (200, &json!(false)), "{}", r.text());
}
