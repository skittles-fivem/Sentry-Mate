#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! The car's firmware fingerprints and the "Tesla update installed" rows found from them, through the real router
//! (web::testkit), over an engine that decoded three runs of a made-up car (synth: a made-up VIN, made-up places and
//! made-up fingerprints): set A, an update to B, an update back to A, an update to B again. The gateway copies on the
//! chassis bus carry other values (never read) and the first run has corrupt frames (never an update).

use engine::Engine;
use model::CarId;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use synth::Firmware;
use web::testkit::{Client, TestServer};

const DAY_MS: u64 = 86_400_000;

struct EngineCars(db::Db);

impl web::CarDirectory for EngineCars {
    fn exists(&self, car: CarId) -> bool {
        self.0.car(car).ok().flatten().is_some()
    }
}

async fn put(store: &ingest::Store, judge: &ingest::Judge, name: &str, bytes: &[u8]) {
    let sha = hex::encode(Sha256::digest(bytes));
    let r = store.receive(name, bytes.to_vec().into(), 0, bytes.len() as u64, &sha).await.unwrap();
    assert!(r.complete, "{name}");
    judge.arrived(name).await.unwrap();
}

async fn ok(c: &mut Client, path: &str) -> Value {
    let r = c.get(path).await;
    assert_eq!(r.status, 200, "{path}: {}", r.text());
    r.json()
}

/// The three runs' fingerprints: A → B, B → A, A → B.
fn runs() -> Vec<Firmware> {
    let mut first = Firmware::made_up();
    // the chassis bus copies say another battery CRC: never read
    first.can1 = Some(vec![("BMS_appCrc", 0x0bad_0bad)]);
    let a = first.values(false);
    let b = first.values(true);
    let back: Vec<(&'static str, u64)> =
        first.after.iter().map(|(n, _)| (*n, a.iter().find(|(x, _)| x == n).unwrap().1)).collect();
    let second = Firmware { before: b, after: back, can1: None, corrupt_at_ms: Vec::new() };
    let third = Firmware { can1: None, ..Firmware::made_up() };
    vec![first, second, third]
}

fn updates_of(v: &Value) -> Vec<Value> {
    let mut items: Vec<Value> = v["items"].as_array().unwrap().clone();
    items.sort_by_key(|u| u["start_ms"].as_u64().unwrap());
    items
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fingerprints_updates_names_and_a_rebuild_after_loss() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let (v, _) = vault::Vault::create(&data, &dir.path().join("keys"), vault::Kdf::FAST_FOR_TESTS).unwrap();
    let v = Arc::new(v);
    let db = db::Db::open(&data, v.clone(), db::Options::default()).unwrap();
    let store = ingest::Store::with_limits(data.join("can"), v, 0, 2_000_000, |_| u64::MAX);
    let engine = Engine::new(db.clone(), store.clone(), engine::Options { workers: 4, ..engine::Options::default() });
    let judge = ingest::Judge::new(store.clone()).on_judged(engine.listener());
    engine.start().unwrap();

    // three days a few days apart whose run has a software update (the made-up car updates on some days only)
    let run_of = |d: u64| {
        synth::Scenario::new(5)
            .start(synth::DEFAULT_START_MS + d * DAY_MS)
            .days(1)
            .rates(synth::Rates::Light)
            .xz_preset(0)
    };
    let mut days: Vec<u64> = Vec::new();
    for d in 0..60u64 {
        if days.len() < 3 && days.last().is_none_or(|l| d >= l + 2) && run_of(d).truth().updates.len() == 1 {
            days.push(d);
        }
    }
    assert_eq!(days.len(), 3, "{days:?}");
    let mut truths = Vec::new();
    for (i, mut fw) in runs().into_iter().enumerate() {
        let sc = run_of(days[i]);
        let truth = sc.truth();
        if i == 0 {
            // corrupt frames: one in the first drive, one after the update
            fw.corrupt_at_ms = vec![truth.drives[0].start_ms + 90_000, truth.updates[0].1 + 600_000];
        }
        truths.push(truth);
        // the runs do not chain: left unsigned
        for mut log in sc.firmware(fw).logs() {
            if let Some(o) = log.sidecar.as_object_mut() {
                o.remove("integrity");
            }
            put(&store, &judge, &log.name, &log.bytes).await;
            put(&store, &judge, &log.sidecar_name(), &log.sidecar_bytes()).await;
        }
    }
    engine.idle().await;
    let cars = engine::query::all_cars(&engine).unwrap();
    assert_eq!(cars.len(), 1, "{cars:?}");
    let (e2, d2) = (engine.clone(), db.clone());
    let s = TestServer::start_with(move |b| b.cars(Arc::new(EngineCars(d2))).service(Arc::new(e2))).await;
    let car = format!("c{}", cars[0]);
    let mut admin = s.admin().await;

    // the panel: the four modules, the current values (set B, after the third run's update) as lowercase hex
    let fw = ok(&mut admin, &format!("/api/v1/cars/{car}/firmware")).await;
    assert_eq!(fw["car"], json!(car));
    let ids: Vec<&str> = fw["modules"].as_array().unwrap().iter().map(|m| m["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["vcfront", "bms", "dir", "dif"]);
    let names: Vec<&str> = fw["modules"].as_array().unwrap().iter().map(|m| m["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["Front body controller", "Battery management", "Rear drive unit", "Front drive unit"]);
    assert_eq!(fw["modules"][1]["can_id"], json!(0x300));
    let field = |module: usize, name: &str| -> Value {
        fw["modules"][module]["fields"].as_array().unwrap().iter().find(|f| f["field"] == name).cloned().unwrap()
    };
    let bms = field(1, "BMS_appCrc");
    assert_eq!(bms["hex"], json!("5eed1101"), "{fw}");
    assert_eq!(bms["history"].as_array().unwrap().len(), 4, "A, B, A, B: {bms}");
    assert!(bms["first_seen_ms"].as_u64().unwrap() >= truths[2].updates[0].0);
    assert!(bms["last_seen_ms"].as_u64().unwrap() >= bms["first_seen_ms"].as_u64().unwrap());
    // a 56-bit hash: 14 digits; an unchanged field: one value since the first run
    let hash = field(0, "VCFRONT_infoBootGitHash");
    assert_eq!((hash["hex"].as_str(), hash["bits"].as_u64()), (Some("c0ffee00000001"), Some(56)));
    assert_eq!(field(3, "DIF_oilPumpAppCrc")["history"].as_array().unwrap().len(), 1);
    assert!(field(3, "DIF_oilPumpAppCrc")["first_seen_ms"].as_u64().unwrap() < truths[0].updates[0].0);
    // zero fields (the app git hashes) are not sent: not listed
    assert!(fw["modules"].as_array().unwrap().iter().all(|m| {
        m["fields"].as_array().unwrap().iter().all(|f| !f["field"].as_str().unwrap().contains("AppGitHash"))
    }));
    // the chassis bus copies' value and the corrupt frames' never show
    assert!(!fw.to_string().contains("0bad0bad"), "{fw}");
    assert_eq!(fw["updates"], json!(3));
    assert_eq!(fw["bridge_hints"]["count"], json!(0));
    let total_values: usize = fw["modules"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|m| m["fields"].as_array().unwrap().iter())
        .map(|f| f["history"].as_array().unwrap().len())
        .sum();
    assert_eq!(total_values, 24 + 3 * 3, "every made-up field once, the three changed ones three times more");

    // the updates: three, one per run, each between the last drive with the old set and the first with the new
    let ups = updates_of(&ok(&mut admin, &format!("/api/v1/cars/{car}/updates")).await);
    assert_eq!(ups.len(), 3, "{ups:?}");
    for (u, t) in ups.iter().zip(&truths) {
        assert_eq!(u["detected_by"], json!("fingerprint"), "{u}");
        assert_eq!(u["changed_modules"], json!(["vcfront", "bms", "dir"]), "{u}");
        assert_eq!(u["source"], json!("can"));
        assert_eq!(u["sources"][0]["kind"], json!("smo"), "{u}");
        let (from, to) = (u["start_ms"].as_u64().unwrap(), u["end_ms"].as_u64().unwrap());
        assert_eq!(u["between_ms"], json!([from, to]));
        let (a, b) = t.updates[0];
        // the old firmware runs until the update ends (the car sends the old values while it installs): the new set
        // first seen once it is done
        assert!(from < to && from <= b && to >= b, "{from}..{to} around {a}..{b}");
        let before = t.drives.iter().filter(|d| d.end_ms <= a).map(|d| d.end_ms).max().unwrap_or(0);
        let after = t.drives.iter().filter(|d| d.start_ms >= b).map(|d| d.start_ms).min().unwrap_or(u64::MAX);
        assert!(from >= before && to <= after, "{u}");
        assert!(u["version"].is_null() && u["named_by"].is_null(), "{u}");
    }
    let ch = &ups[0]["changes"];
    assert_eq!(
        ch[1],
        json!({ "module": "bms", "module_name": "Battery management", "field": "BMS_appCrc", "old": "5eed0101",
                "new": "5eed1101" }),
        "{ch}"
    );
    assert_eq!(ch.as_array().unwrap().len(), 3);
    assert_eq!(ups[0]["build_id"], ups[2]["build_id"], "the same set after the first and the third");
    assert_ne!(ups[0]["build_id"], ups[1]["build_id"]);

    // the timeline's update rows say the same
    let (a, b) = (synth::DEFAULT_START_MS, synth::DEFAULT_START_MS + (days[2] + 1) * DAY_MS);
    let tl = ok(&mut admin, &format!("/api/v1/cars/{car}/timeline?kinds=update&from_ms={a}&to_ms={b}&limit=50")).await;
    let rows = tl["items"].as_array().unwrap();
    assert_eq!(rows.len(), 3, "{tl}");
    assert!(
        rows.iter().all(|r| r["detected_by"] == "fingerprint" && r["changed_modules"].as_array().unwrap().len() == 3)
    );

    // a watch account reads them, and cannot name one
    let mut watch = s.watch(&mut admin, &[cars[0]]).await;
    assert_eq!(ok(&mut watch, &format!("/api/v1/cars/{car}/firmware")).await, fw);
    let first = ups[0]["id"].as_str().unwrap().to_string();
    let r = watch.patch(&format!("/api/v1/cars/{car}/updates/{first}"), json!({ "version": "2026.2.6" })).await;
    assert_eq!(r.status, 403);
    assert_eq!(watch.get("/api/v1/cars/c999/firmware").await.status, 404);

    // the owner names the first: the third (the same set) gets the name by itself, the second (set A) does not
    let r = admin.patch(&format!("/api/v1/cars/{car}/updates/{first}"), json!({ "version": "2026.2.6" })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let one = r.json();
    assert_eq!((one["version"].as_str(), one["named_by"].as_str()), (Some("2026.2.6"), Some("owner")), "{one}");
    assert_eq!(one["version_source"], json!("owner"));
    let named = updates_of(&ok(&mut admin, &format!("/api/v1/cars/{car}/updates")).await);
    assert_eq!(named[1]["version"], Value::Null);
    assert_eq!(
        (named[2]["version"].as_str(), named[2]["named_by"].as_str(), named[2]["version_source"].as_str()),
        (Some("2026.2.6"), Some("auto"), Some("build_match"))
    );
    let all = ok(&mut admin, &format!("/api/v1/cars/{car}/updates")).await;
    assert_eq!(all["summary"]["current_version"], json!("2026.2.6"));
    let tl = ok(&mut admin, &format!("/api/v1/cars/{car}/timeline?kinds=update&from_ms={a}&to_ms={b}&limit=50")).await;
    let versions: Vec<Value> = {
        let mut r = tl["items"].as_array().unwrap().clone();
        r.sort_by_key(|x| x["start_ms"].as_u64().unwrap());
        r.iter().map(|x| x["version"].clone()).collect()
    };
    assert_eq!(versions, [json!("2026.2.6"), Value::Null, json!("2026.2.6")]);
    let audit = ok(&mut admin, "/api/v1/audit").await;
    assert!(audit.to_string().contains("update-version"), "{audit}");

    // a name typed on the second, a bad one refused
    let second = named[1]["id"].as_str().unwrap().to_string();
    let r = admin.patch(&format!("/api/v1/cars/{car}/updates/{second}"), json!({ "version": "soon" })).await;
    assert_eq!(r.status, 422);
    let r = admin.patch(&format!("/api/v1/cars/{car}/updates/{second}"), json!({ "version": "2026.8.1" })).await;
    assert_eq!(r.status, 200, "{}", r.text());

    // the fingerprint records lost (not the names): a rebuild of the car gives the same panel and updates
    let before_fw = ok(&mut admin, &format!("/api/v1/cars/{car}/firmware")).await;
    let before_ups = updates_of(&ok(&mut admin, &format!("/api/v1/cars/{car}/updates")).await);
    let prefix = format!("car/{}/fw/", cars[0]);
    for k in db.kv().keys("engine").unwrap() {
        if k.starts_with(&prefix) && !k.ends_with("/names") {
            db.kv().delete("engine", &k).unwrap();
        }
    }
    let r = admin.post(&format!("/api/v1/cars/{car}/rebuild"), json!({})).await;
    assert_eq!(r.status, 200, "{}", r.text());
    // (a whole car's rebuild may answer while it runs: waited for here)
    engine.idle().await;
    assert_eq!(ok(&mut admin, &format!("/api/v1/cars/{car}/firmware")).await, before_fw);
    let after_ups = updates_of(&ok(&mut admin, &format!("/api/v1/cars/{car}/updates")).await);
    let strip = |v: &[Value]| -> Vec<Value> {
        v.iter()
            .map(|u| {
                let mut u = u.clone();
                u.as_object_mut().unwrap().remove("duration_ms");
                u
            })
            .collect()
    };
    assert_eq!(strip(&after_ups), strip(&before_ups));
    assert_eq!(after_ups[1]["version"], json!("2026.8.1"));

    // clearing the first's name: the third loses the name it had from it
    let r = admin.patch(&format!("/api/v1/cars/{car}/updates/{first}"), json!({ "version": null })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert!(r.json()["version"].is_null());
    let cleared = updates_of(&ok(&mut admin, &format!("/api/v1/cars/{car}/updates")).await);
    assert!(cleared[0]["version"].is_null() && cleared[2]["version"].is_null(), "{cleared:?}");
    assert_eq!(cleared[1]["version"], json!("2026.8.1"));
}
