#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! PATCH /accounts/{account}: a change refused is no change at all, and two changes from one version are one 200
//! and one 409. Made-up names and cars only.

use serde_json::{Value, json};
use std::sync::Arc;
use web::testkit::{ADMIN_PASSWORD, TestServer, WATCH};

async fn server() -> TestServer {
    let cars: std::collections::HashSet<u32> = [1, 2].into_iter().collect();
    TestServer::start_with(move |b| b.cars(Arc::new(web::FixedCars(cars)))).await
}

async fn watch_account(admin: &mut web::testkit::Client) -> Value {
    let list = admin.get("/api/v1/accounts").await.json();
    list["items"].as_array().unwrap().iter().find(|a| a["name"] == WATCH).unwrap().clone()
}

#[tokio::test]
async fn a_refused_password_changes_neither_role_nor_cars() {
    let s = server().await;
    let mut admin = s.admin().await;
    let mut watch = s.watch(&mut admin, &[1]).await;
    let w = watch_account(&mut admin).await;
    let id = w["id"].as_str().unwrap().to_string();
    let r = admin
        .patch(
            &format!("/api/v1/accounts/{id}"),
            json!({ "version": w["version"], "role": "admin", "cars": "all", "password": "x".repeat(1025),
                    "confirm_password": ADMIN_PASSWORD }),
        )
        .await;
    assert_eq!((r.status, r.json()["error"]["fields"]["password"].is_string()), (422, true), "{}", r.text());
    let after = watch_account(&mut admin).await;
    assert_eq!((&after["role"], &after["cars"], &after["version"]), (&w["role"], &w["cars"], &w["version"]));
    assert_eq!(watch.get("/api/v1/me").await.json()["role"], "watch");
    let audit = admin.get("/api/v1/audit?limit=500").await.json();
    let actions: Vec<&str> = audit["items"].as_array().unwrap().iter().filter_map(|e| e["action"].as_str()).collect();
    assert!(!actions.contains(&"role-changed") && !actions.contains(&"cars-changed"), "{actions:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn changes_from_one_version_give_one_success() {
    let s = server().await;
    let mut admin = s.admin().await;
    let _watch = s.watch(&mut admin, &[1]).await;
    let w = watch_account(&mut admin).await;
    let path = format!("/api/v1/accounts/{}", w["id"].as_str().unwrap());
    let mut tasks = Vec::new();
    for i in 0..8 {
        // each from its own session, as two admins' forms would be
        let mut c = s.client();
        let r = c.post("/api/v1/session", json!({ "name": web::testkit::ADMIN, "password": ADMIN_PASSWORD })).await;
        assert_eq!(r.status, 200, "{}", r.text());
        let (path, version) = (path.clone(), w["version"].clone());
        let cars = if i % 2 == 0 { json!(["c2"]) } else { json!(["c1", "c2"]) };
        tasks.push(tokio::spawn(async move {
            let r =
                c.patch(&path, json!({ "version": version, "cars": cars, "confirm_password": ADMIN_PASSWORD })).await;
            (r.status, r.code())
        }));
    }
    let mut got = Vec::new();
    for t in tasks {
        got.push(t.await.unwrap());
    }
    let ok = got.iter().filter(|g| g.0 == 200).count();
    let stale = got.iter().filter(|g| g.1.as_deref() == Some("stale_version")).count();
    assert_eq!((ok, stale), (1, 7), "{got:?}");
}

#[tokio::test]
async fn the_audit_log_says_which_cars_an_account_was_given() {
    let s = server().await;
    let mut admin = s.admin().await;
    let _watch = s.watch(&mut admin, &[1]).await;
    let w = watch_account(&mut admin).await;
    let r = admin
        .patch(
            &format!("/api/v1/accounts/{}", w["id"].as_str().unwrap()),
            json!({ "version": w["version"], "cars": ["c2"], "confirm_password": ADMIN_PASSWORD }),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    let audit = admin.get("/api/v1/audit?limit=500").await.json();
    let said: Vec<&str> = audit["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["action"] == "cars-changed")
        .filter_map(|e| e["summary"].as_str())
        .collect();
    assert_eq!(said, [format!("Changed which cars {WATCH} sees: c2 (was c1).")], "{audit}");
}

/// An admin's own new password and another change in one request: both are made (each step checks the password
/// typed, which is the old one until the last step).
#[tokio::test]
async fn ones_own_new_password_and_cars_in_one_change() {
    let s = server().await;
    let mut admin = s.admin().await;
    let list = admin.get("/api/v1/accounts").await.json();
    let me = list["items"].as_array().unwrap().iter().find(|a| a["name"] == web::testkit::ADMIN).unwrap().clone();
    let r = admin
        .patch(
            &format!("/api/v1/accounts/{}", me["id"].as_str().unwrap()),
            json!({ "version": me["version"], "cars": ["c1"], "password": "a brand new long passphrase 1",
                    "confirm_password": ADMIN_PASSWORD }),
        )
        .await;
    assert_eq!((r.status, &r.json()["cars"]), (200, &json!(["c1"])), "{}", r.text());
    let mut c = s.client();
    let r = c
        .post("/api/v1/session", json!({ "name": web::testkit::ADMIN, "password": "a brand new long passphrase 1" }))
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
}
