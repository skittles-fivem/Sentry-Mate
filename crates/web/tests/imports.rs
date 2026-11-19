#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! TeslaMate imports through the real router (web::testkit), over an engine with one made-up car: a made-up
//! TeslaMate backup (the import crate's fixture: two cars with fake VINs) uploaded in chunks, checked, its cars
//! matched (one to the car here by its VIN prefix, one as a new car), imported, and discarded; roles, CSRF and the
//! upload's errors.

use base64::Engine as _;
use engine::Engine;
use model::{Car, CarId, VinInfo};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use web::testkit::{Client, TestServer};

const BACKUP: &[u8] = include_bytes!("../../import/tests/fixtures/teslamate-4.4.0.sql");

struct Kit {
    s: TestServer,
    engine: Engine,
    car: CarId,
    _dir: tempfile::TempDir,
}

struct EngineCars(db::Db);

impl web::CarDirectory for EngineCars {
    fn exists(&self, car: CarId) -> bool {
        self.0.car(car).ok().flatten().is_some()
    }
}

async fn kit() -> Kit {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let (v, _) = vault::Vault::create(&data, &dir.path().join("keys"), vault::Kdf::FAST_FOR_TESTS).unwrap();
    let v = Arc::new(v);
    let db = db::Db::open(&data, v.clone(), db::Options::default()).unwrap();
    // the made-up Model 3 of the backup is already here (by its VIN prefix)
    let car = db
        .create_car(Car {
            id: 0,
            name: "Here".into(),
            vehicle_keys: Vec::new(),
            preferred_key: None,
            vin: Some(VinInfo {
                vin_prefix: "5YJ3E1EA0KF".into(),
                model: Some('3'),
                year: Some(2019),
                plant: Some('F'),
                via: 0x405,
            }),
            profile_setting: None,
            time_zone: "Europe/Amsterdam".into(),
            efficiency_wh_km: None,
            created_ms: 1_790_000_000_000,
        })
        .unwrap();
    let store = ingest::Store::new(data.join("can"), v);
    let engine = Engine::new(db.clone(), store, engine::Options::default());
    let (e2, d2) = (engine.clone(), db.clone());
    let s = TestServer::start_with(move |b| {
        b.cars(Arc::new(EngineCars(d2)))
            .service(Arc::new(e2))
            .service(Arc::new(web::resources::imports::Imports::new()))
    })
    .await;
    Kit { s, engine, car: car.id, _dir: dir }
}

async fn chunk(c: &mut Client, id: &str, off: usize, bytes: &[u8], sha: Option<String>) -> web::testkit::Res {
    let sha = sha.unwrap_or_else(|| hex::encode(Sha256::digest(bytes)));
    let (o, l) = (off.to_string(), BACKUP.len().to_string());
    c.request(
        "PATCH",
        &format!("/api/v1/imports/{id}"),
        Some(bytes),
        &[
            ("Content-Type", "application/offset+octet-stream"),
            ("Upload-Offset", &o),
            ("Upload-Length", &l),
            ("Upload-SHA256", &sha),
        ],
    )
    .await
}

async fn wait_for(c: &mut Client, id: &str, status: &str) -> Value {
    for _ in 0..400 {
        let r = c.get(&format!("/api/v1/imports/{id}")).await;
        assert_eq!(r.status, 200, "{}", r.text());
        let j = r.json();
        if j["status"] == status {
            return j;
        }
        assert_ne!(j["status"], "failed", "{j}");
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("{id} never got {status}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn upload_check_match_import_and_discard() {
    let k = kit().await;
    let mut admin = k.s.admin().await;
    let mut watch = k.s.watch(&mut admin, &[k.car]).await;

    // admin only, CSRF on changes
    assert_eq!(watch.get("/api/v1/imports").await.status, 403);
    assert_eq!(watch.post("/api/v1/imports", json!({ "bytes_total": 10 })).await.status, 403);
    let body = serde_json::to_vec(&json!({ "bytes_total": 10 })).unwrap();
    let r = admin
        .request(
            "POST",
            "/api/v1/imports",
            Some(&body),
            &[("Content-Type", "application/json"), ("X-Sentry-Mate", "0")],
        )
        .await;
    assert_eq!((r.status, r.code()), (403, Some("csrf".into())));
    let r = admin.post("/api/v1/imports", json!({})).await;
    assert_eq!((r.status, r.code()), (400, Some("bad_input".into())));
    let r = admin.post("/api/v1/imports", json!({ "bytes_total": 0 })).await;
    assert_eq!(r.status, 422);
    let r = admin.post("/api/v1/imports", json!({ "bytes_total": 5, "compression": "zip" })).await;
    assert_eq!(r.status, 422);

    // the upload
    let empty = admin.get("/api/v1/imports").await.json();
    assert_eq!(empty, json!({ "items": [], "next": null }));
    let r = admin.post("/api/v1/imports", json!({ "name": "teslamate.bck", "bytes_total": BACKUP.len() })).await;
    assert_eq!(r.status, 201, "{}", r.text());
    let j = r.json();
    for key in
        ["id", "name", "compression", "status", "pass", "bytes_read", "bytes_total", "uploaded_bytes", "started_ms"]
    {
        assert!(j.get(key).is_some(), "{key}: {j}");
    }
    assert_eq!((j["status"].as_str(), j["compression"].as_str()), (Some("uploading"), Some("plain")));
    let id = j["id"].as_str().unwrap().to_string();
    assert_eq!(j["upload_url"], json!(format!("/api/v1/imports/{id}")));
    assert!(j["chunk_max_bytes"].as_u64().unwrap() < 1 << 20);
    let r = admin.post("/api/v1/imports", json!({ "bytes_total": 5 })).await;
    assert_eq!((r.status, r.code()), (409, Some("import_running".into())), "one import at a time");
    let r = admin.get(&format!("/api/v1/imports/{id}/cars")).await;
    assert_eq!((r.status, r.code()), (409, Some("not_checked".into())));

    let half = BACKUP.len() / 2;
    let r = chunk(&mut admin, &id, 5, &BACKUP[..10], None).await;
    assert_eq!((r.status, r.code()), (409, Some("wrong_offset".into())));
    assert_eq!(r.json()["error"]["offset"], json!(0));
    assert_eq!(r.header("upload-offset"), Some("0"));
    let r = chunk(&mut admin, &id, 0, &BACKUP[..half], Some(hex::encode([0u8; 32]))).await;
    assert_eq!((r.status, r.code()), (422, Some("checksum_mismatch".into())));
    let r = chunk(&mut admin, &id, 0, &BACKUP[..half], None).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.json()["uploaded_bytes"], json!(half));
    // the SHA-256 in base64 works too
    let rest = &BACKUP[half..];
    let b64 = base64::engine::general_purpose::STANDARD.encode(Sha256::digest(rest));
    let r = chunk(&mut admin, &id, half, rest, Some(b64)).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.json()["status"], "checking");
    let r = chunk(&mut admin, &id, BACKUP.len(), b"x", None).await;
    assert_eq!((r.status, r.code()), (409, Some("not_uploading".into())));

    // the check: the cars and what they bring, nothing stored yet
    let ready = wait_for(&mut admin, &id, "ready").await;
    assert!(ready["checked_ms"].is_u64(), "{ready}");
    assert_eq!(ready["report"]["imported"], json!(false));
    assert!(ready["report"]["tables"]["drives"]["read"].as_u64().unwrap() > 0, "{ready}");
    assert_eq!(ready["cars_to_match"], json!(1), "the car here is matched at first: {ready}");
    let cars = admin.get(&format!("/api/v1/imports/{id}/cars")).await.json();
    let items = cars["items"].as_array().unwrap().clone();
    assert_eq!(items.len(), 2, "{cars}");
    let three = items.iter().find(|c| c["vin_prefix"] == "5YJ3E1EA0KF").unwrap();
    let why = items.iter().find(|c| c["vin_prefix"] == "7SAYGDEE0PF").unwrap();
    let here = format!("c{}", k.car);
    assert_eq!((three["suggested"].as_str(), three["match"].as_str()), (Some(here.as_str()), Some(here.as_str())));
    assert!(why["match"].is_null() && why["suggested"].is_null(), "{why}");
    assert!(three["drives"].as_u64().unwrap() > 0, "{three}");
    for key in ["tm_car", "name", "vin_prefix", "drives", "charges", "first_ms", "last_ms", "match", "suggested"] {
        assert!(three.get(key).is_some(), "{key}: {three}");
    }
    let r = admin.post(&format!("/api/v1/imports/{id}/start"), json!({})).await;
    assert_eq!((r.status, r.code()), (409, Some("cars_unmatched".into())));

    // matching: a car the account doesn't know is refused, one car here takes one imported car
    let tm_why = why["tm_car"].clone();
    let r = admin
        .put(&format!("/api/v1/imports/{id}/cars"), json!({ "items": [{ "tm_car": tm_why, "match": "c99" }] }))
        .await;
    assert_eq!(r.status, 422, "{}", r.text());
    let r = admin
        .put(&format!("/api/v1/imports/{id}/cars"), json!({ "items": [{ "tm_car": tm_why, "match": here }] }))
        .await;
    assert_eq!(r.status, 422, "{}", r.text());
    let r = admin
        .put(&format!("/api/v1/imports/{id}/cars"), json!({ "items": [{ "tm_car": "999", "match": "new" }] }))
        .await;
    assert_eq!(r.status, 422, "{}", r.text());
    let r = admin
        .put(&format!("/api/v1/imports/{id}/cars"), json!({ "items": [{ "tm_car": tm_why, "match": "new" }] }))
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(admin.get(&format!("/api/v1/imports/{id}")).await.json()["cars_to_match"], json!(0));

    // the import
    let r = admin.post(&format!("/api/v1/imports/{id}/start"), json!({ "currency": "eur" })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.json()["status"], "importing");
    let done = wait_for(&mut admin, &id, "done").await;
    assert_eq!(done["report"]["imported"], json!(true));
    assert!(done["report"]["tables"]["drives"]["imported"].as_u64().unwrap() > 0, "{done}");
    let per_car = done["report"]["cars"].as_array().unwrap();
    assert_eq!(per_car.len(), 2);
    assert!(per_car.iter().all(|c| c["imported"].as_u64().unwrap() > 0), "{done}");
    let r = admin.post(&format!("/api/v1/imports/{id}/start"), json!({})).await;
    assert_eq!((r.status, r.code()), (409, Some("not_ready".into())));
    // TeslaMate's geofences came as places, priced in the currency the import was started with: the price per kWh
    // as a plan, the price per minute (with its fee) beside a plan of none per kWh; each says when it came
    let places = admin.get("/api/v1/places?sort=name").await;
    assert_eq!(places.status, 200, "{}", places.text());
    let items = places.json()["items"].as_array().unwrap().clone();
    let named = |n: &str| items.iter().find(|p| p["name"] == n).cloned().unwrap_or_else(|| panic!("{n}: {items:?}"));
    let home = named("Home");
    assert_eq!(home["price"]["currency"], "EUR", "{home}");
    assert_eq!(home["price"]["per_kwh"], json!(0.25), "{home}");
    let sc = named("Testville Supercharger");
    assert_eq!(sc["price"]["currency"], "EUR", "{sc}");
    assert_eq!(
        (sc["price"]["per_minute"].clone(), sc["price"]["session_fee"].clone()),
        (json!(0.1), json!(1.5)),
        "{sc}"
    );
    assert!(sc["price"]["per_kwh"].is_null(), "{sc}");
    assert!(home["created_ms"].is_u64() && sc["created_ms"].is_u64(), "{items:?}");
    // the history is there: the car here has imported drives, and a new car was made
    let drives = admin.get(&format!("/api/v1/cars/{here}/drives?source=teslamate")).await;
    assert_eq!(drives.status, 200, "{}", drives.text());
    assert!(!drives.json()["items"].as_array().unwrap().is_empty(), "{}", drives.text());
    let all = engine::query::all_cars(&k.engine).unwrap();
    assert_eq!(all.len(), 2, "{all:?}");
    let list = admin.get("/api/v1/imports").await.json();
    assert_eq!(list["items"].as_array().unwrap().len(), 1);
    assert_eq!(list["items"][0]["status"], "done");
    let audit = admin.get("/api/v1/audit").await.json().to_string();
    for a in ["import-created", "import-uploaded", "import-cars", "import-started"] {
        assert!(audit.contains(a), "{a}: {audit}");
    }

    // discarding: the record goes, the history stays
    let r = watch.delete(&format!("/api/v1/imports/{id}"), json!({})).await;
    assert_eq!(r.status, 403);
    let r = admin.delete(&format!("/api/v1/imports/{id}"), json!({})).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.json()["deleted"], json!(true));
    assert_eq!(admin.get(&format!("/api/v1/imports/{id}")).await.code(), Some("no_such_import".into()));
    assert_eq!(engine::query::all_cars(&k.engine).unwrap().len(), 2);

    // a file that is not a backup fails its check
    let junk = b"hello, this is not a backup\n";
    let r = admin.post("/api/v1/imports", json!({ "name": "notes.txt", "bytes_total": junk.len() })).await;
    let id = r.json()["id"].as_str().unwrap().to_string();
    let sha = hex::encode(Sha256::digest(junk));
    let l = junk.len().to_string();
    let r = admin
        .request(
            "PATCH",
            &format!("/api/v1/imports/{id}"),
            Some(junk),
            &[("Upload-Offset", "0"), ("Upload-Length", &l), ("Upload-SHA256", &sha)],
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    let mut failed = Value::Null;
    for _ in 0..400 {
        failed = admin.get(&format!("/api/v1/imports/{id}")).await.json();
        if failed["status"] == "failed" {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(failed["status"], "failed", "{failed}");
    assert!(failed["error"]["code"].is_string() && failed["error"]["message"].is_string(), "{failed}");
    // a failed import is finished: another may start
    let r = admin.post("/api/v1/imports", json!({ "bytes_total": 3 })).await;
    assert_eq!(r.status, 201, "{}", r.text());
}

/// Uploads `bytes` as one import in chunks of `step` and waits for its check.
async fn upload(c: &mut Client, bytes: &[u8], step: usize) -> String {
    let r = c.post("/api/v1/imports", json!({ "name": "twins.sql", "bytes_total": bytes.len() })).await;
    assert_eq!(r.status, 201, "{}", r.text());
    let id = r.json()["id"].as_str().unwrap().to_string();
    let mut off = 0;
    while off < bytes.len() {
        let part = &bytes[off..(off + step).min(bytes.len())];
        let (o, l, sha) = (off.to_string(), bytes.len().to_string(), hex::encode(Sha256::digest(part)));
        let r = c
            .request(
                "PATCH",
                &format!("/api/v1/imports/{id}"),
                Some(part),
                &[("Upload-Offset", &o), ("Upload-Length", &l), ("Upload-SHA256", &sha)],
            )
            .await;
        assert_eq!(r.status, 200, "{}", r.text());
        off += part.len();
    }
    id
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_imported_cars_never_go_into_one_and_chunks_take_turns() {
    let k = kit().await;
    let mut admin = k.s.admin().await;
    let here = format!("c{}", k.car);

    // the same chunk sent eight times at once: one is stored, the others hear where the upload is, none fails
    let r = admin.post("/api/v1/imports", json!({ "name": "race.sql", "bytes_total": BACKUP.len() })).await;
    let id = r.json()["id"].as_str().unwrap().to_string();
    let part = &BACKUP[..BACKUP.len() / 3];
    let mut tries = Vec::new();
    for _ in 0..8 {
        let (mut c, id, part) = (admin.clone(), id.clone(), part.to_vec());
        tries.push(tokio::spawn(async move { chunk(&mut c, &id, 0, &part, None).await }));
    }
    let mut codes = Vec::new();
    for t in tries {
        let r = t.await.unwrap();
        codes.push((r.status, r.code()));
    }
    assert_eq!(codes.iter().filter(|c| c.0 == 200).count(), 1, "{codes:?}");
    assert!(codes.iter().all(|c| c.0 == 200 || *c == (409, Some("wrong_offset".into()))), "{codes:?}");
    let j = admin.get(&format!("/api/v1/imports/{id}")).await.json();
    assert_eq!(j["uploaded_bytes"], json!(part.len()), "{j}");
    assert_eq!(admin.delete(&format!("/api/v1/imports/{id}"), json!({})).await.status, 200);

    // a backup whose two cars share the car here's VIN prefix (same model, year and plant): neither is suggested
    let twins = String::from_utf8(BACKUP.to_vec()).unwrap().replace("7SAYGDEE0PF000001", "5YJ3E1EA0KF000999");
    let id = upload(&mut admin, twins.as_bytes(), 200_000).await;
    let ready = wait_for(&mut admin, &id, "ready").await;
    assert_eq!(ready["cars_to_match"], json!(2), "{ready}");
    let cars = admin.get(&format!("/api/v1/imports/{id}/cars")).await.json();
    let items = cars["items"].as_array().unwrap();
    assert_eq!(items.len(), 2, "{cars}");
    assert!(items.iter().all(|c| c["vin_prefix"] == "5YJ3E1EA0KF"), "{cars}");
    assert!(items.iter().all(|c| c["suggested"].is_null() && c["match"].is_null()), "{cars}");
    let r = admin.post(&format!("/api/v1/imports/{id}/start"), json!({})).await;
    assert_eq!((r.status, r.code()), (409, Some("cars_unmatched".into())));
    // both into the car here is refused; one there and one new imports
    let (a, b) = (items[0]["tm_car"].clone(), items[1]["tm_car"].clone());
    let r = admin
        .put(
            &format!("/api/v1/imports/{id}/cars"),
            json!({ "items": [{ "tm_car": a, "match": here }, { "tm_car": b, "match": here }] }),
        )
        .await;
    assert_eq!((r.status, r.code()), (422, Some("invalid".into())), "{}", r.text());
    let r = admin
        .put(
            &format!("/api/v1/imports/{id}/cars"),
            json!({ "items": [{ "tm_car": a, "match": here }, { "tm_car": b, "match": "new" }] }),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    let r = admin.post(&format!("/api/v1/imports/{id}/start"), json!({})).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let done = wait_for(&mut admin, &id, "done").await;
    let matched: Vec<&str> =
        done["report"]["cars"].as_array().unwrap().iter().map(|c| c["match"].as_str().unwrap()).collect();
    assert_eq!(matched.iter().filter(|m| **m == here).count(), 1, "{done}");
    assert_eq!(engine::query::all_cars(&k.engine).unwrap().len(), 2);
    // started without a currency: the places come, their prices do not (no price without its currency)
    let places = admin.get("/api/v1/places").await.json();
    let items = places["items"].as_array().unwrap();
    assert_eq!(items.len(), 2, "{places}");
    assert!(items.iter().all(|p| p["price"].is_null()), "{places}");
}
