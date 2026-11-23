#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! TeslaFi imports through the real router (web::testkit), over an engine with one made-up car: two made-up months
//! of TeslaFi's raw data in a zip, uploaded, recognised by their header, checked (the car matched by its VIN prefix,
//! the times read in the car's time zone), imported with the source `import:teslafi`, and imported again (nothing is
//! added); TeslaFi's other lists and bad settings refused.

#[path = "../../import/tests/tf/mod.rs"]
mod tf;

use engine::Engine;
use model::{Car, CarId, VinInfo};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use web::testkit::{Client, TestServer};

struct Kit {
    s: TestServer,
    car: CarId,
    _engine: Engine,
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
    let car = db
        .create_car(Car {
            id: 0,
            name: "Here".into(),
            vehicle_keys: Vec::new(),
            preferred_key: None,
            vin: Some(VinInfo {
                vin_prefix: "5YJ3E1EA1PF".into(),
                model: Some('3'),
                year: Some(2023),
                plant: Some('F'),
                via: 0x405,
            }),
            profile_setting: None,
            time_zone: "America/New_York".into(),
            time_zone_typed: Some(true),
            efficiency_wh_km: None,
            created_ms: 1_790_000_000_000,
            inputs: Default::default(),
        })
        .unwrap();
    let store = ingest::Store::new(data.join("can"), v);
    let engine = Engine::new(db.clone(), store, engine::Options::default());
    engine.start().unwrap();
    let (e2, d2) = (engine.clone(), db.clone());
    let imports = Arc::new(web::resources::imports::Imports::new());
    let s =
        TestServer::start_with(move |b| b.cars(Arc::new(EngineCars(d2))).service(Arc::new(e2)).service(imports)).await;
    Kit { s, car: car.id, _engine: engine, _dir: dir }
}

async fn upload(c: &mut Client, name: &str, bytes: &[u8]) -> String {
    let r = c.post("/api/v1/imports", json!({ "name": name, "bytes_total": bytes.len() })).await;
    assert_eq!(r.status, 201, "{}", r.text());
    let id = r.json()["id"].as_str().unwrap().to_string();
    let mut off = 0;
    while off < bytes.len() {
        let part = &bytes[off..(off + 900_000).min(bytes.len())];
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

async fn wait_for(c: &mut Client, id: &str, status: &str) -> Value {
    for _ in 0..1200 {
        let r = c.get(&format!("/api/v1/imports/{id}")).await;
        assert_eq!(r.status, 200, "{}", r.text());
        let j = r.json();
        if j["status"] == status {
            return j;
        }
        if status != "failed" {
            assert_ne!(j["status"], "failed", "{j}");
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("{id} never got {status}");
}

fn months() -> Vec<u8> {
    let mut g = tf::Gen::new(tf::VIN1);
    let march = tf::month(&mut g, 2023, 3);
    let april = tf::month(&mut g, 2023, 4);
    let mut z = import::zip::ManagerZip::new(Vec::new());
    z.entry("TeslaFi42023.csv", april.as_bytes()).unwrap();
    z.entry("TeslaFi32023.csv", march.as_bytes()).unwrap();
    z.finish().unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_months_of_teslafi_checked_imported_and_imported_again() {
    let k = kit().await;
    let car = format!("c{}", k.car);
    let mut admin = k.s.admin().await;
    let zip = months();

    let id = upload(&mut admin, "teslafi.zip", &zip).await;
    let j = wait_for(&mut admin, &id, "ready").await;
    assert_eq!(j["kind"], "teslafi", "{j}");
    assert_eq!(j["compression"], "zip");
    assert_eq!(j["cars_to_match"], json!(0), "matched by its VIN prefix: {j}");
    let rep = &j["report"];
    assert_eq!(rep["kind"], "teslafi");
    assert_eq!(rep["imported"], json!(false));
    assert_eq!(rep["files"], json!(2));
    assert_eq!(rep["found"]["drives"], json!(122), "{rep}");
    assert_eq!(rep["found"]["charges"], json!(122));
    // 2023-03-12 02:00 and 02:30 never happened in New York (the clocks went forward)
    assert_eq!(rep["rows"]["skipped"], json!({ "dst_gap": 2 }), "{rep}");
    assert_eq!(rep["rows"]["read"].as_u64().unwrap(), rep["rows"]["used"].as_u64().unwrap() + 2);
    assert_eq!(
        rep["time_zone"],
        json!({ "name": "America/New_York", "from": "car", "asked": true, "dst_repeated_rows": 0 })
    );
    assert_eq!(rep["units"]["distance"], "mi");
    assert_eq!(rep["units"]["temperature"], "c");
    assert_eq!(rep["tables"]["drives"]["imported"], json!(0));
    let first = rep["span"]["from_ms"].as_u64().unwrap();
    // 2023-03-01 00:00 in New York (EST) is 05:00 UTC
    assert_eq!(first, 1_677_646_800_000);
    let cars = admin.get(&format!("/api/v1/imports/{id}/cars")).await.json();
    assert_eq!(cars["items"][0]["match"], json!(car), "{cars}");
    assert_eq!(cars["items"][0]["vin_prefix"], "5YJ3E1EA1PF");

    // bad settings, and settings only a TeslaFi import takes
    let r = admin.post(&format!("/api/v1/imports/{id}/start"), json!({ "time_zone": "Mars/Olympus" })).await;
    assert_eq!((r.status, r.code()), (422, Some("invalid".into())), "{}", r.text());
    let r = admin.post(&format!("/api/v1/imports/{id}/start"), json!({ "distance_unit": "parsec" })).await;
    assert_eq!(r.status, 422, "{}", r.text());
    let r = admin.post(&format!("/api/v1/imports/{id}/start"), json!({ "temperature_unit": "kelvin" })).await;
    assert_eq!(r.status, 422, "{}", r.text());

    let r = admin.post(&format!("/api/v1/imports/{id}/start"), json!({ "time_zone": "America/New_York" })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let done = wait_for(&mut admin, &id, "done").await;
    let rep = &done["report"];
    assert_eq!(rep["imported"], json!(true), "{rep}");
    assert_eq!(rep["tables"]["drives"]["imported"], json!(122));
    assert_eq!(rep["cars"][0]["match"], json!(car));

    let d = admin.get(&format!("/api/v1/cars/{car}/drives?source=import:teslafi&limit=5")).await;
    assert_eq!(d.status, 200, "{}", d.text());
    let d = d.json();
    let one = &d["items"][0];
    assert_eq!(one["source"], "teslafi", "{one}");
    assert_eq!(one["sources"], json!([{ "kind": "import", "site": "teslafi", "label": "Imported from TeslaFi" }]));
    assert_eq!(one["source_label"], "Imported from TeslaFi");
    let tm = admin.get(&format!("/api/v1/cars/{car}/drives?source=import:teslamate")).await.json();
    assert!(tm["items"].as_array().unwrap().is_empty(), "{tm}");
    let all = admin.get(&format!("/api/v1/cars/{car}/drives?limit=5")).await.json();
    assert_eq!(all["items"].as_array().unwrap().len(), 5, "no live history: the imported drives are shown");
    let fleet = admin.get("/api/v1/cars").await.json();
    assert!(fleet.to_string().contains("Imported from TeslaFi"), "{fleet}");

    // the same files again: nothing new
    let again = upload(&mut admin, "teslafi.zip", &zip).await;
    let j = wait_for(&mut admin, &again, "ready").await;
    let rep = &j["report"];
    assert_eq!(rep["found"]["drives"], json!(0), "{rep}");
    assert_eq!(rep["rows"]["used"], json!(0));
    assert_eq!(
        rep["rows"]["skipped"]["already_imported"].as_u64().unwrap() + 2,
        rep["rows"]["read"].as_u64().unwrap(),
        "{rep}"
    );
    let r = admin.post(&format!("/api/v1/imports/{again}/start"), json!({})).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let done = wait_for(&mut admin, &again, "done").await;
    assert_eq!(done["report"]["tables"]["drives"]["imported"], json!(0));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn teslafis_drive_list_and_junk_are_refused_with_why() {
    let k = kit().await;
    let mut admin = k.s.admin().await;
    let drives = "Date,\"Start Time\",\"End Time\",\"Driving Time\",\"Starting Address\",\"Ending Address\",\
                  \"Kilometers Driven\",\"Drive Number\"\n2023-03-04,10:15,10:45,0:30,A,B,31,1\n";
    let id = upload(&mut admin, "drives.csv", drives.as_bytes()).await;
    let j = wait_for(&mut admin, &id, "failed").await;
    assert_eq!(j["kind"], "teslafi");
    assert_eq!(j["error"]["code"], "teslafi_summary", "{j}");
    assert!(j["error"]["message"].as_str().unwrap().contains("Download TeslaFi Data"), "{j}");
    let r = admin.delete(&format!("/api/v1/imports/{id}"), json!({})).await;
    assert_eq!(r.status, 200);
    let id = upload(&mut admin, "notes.csv", b"a,b,c\n1,2,3\n").await;
    let j = wait_for(&mut admin, &id, "failed").await;
    assert_ne!(j["kind"], "teslafi", "{j}");
}

async fn drive_starts(c: &mut Client, car: &str) -> Vec<u64> {
    let d = c.get(&format!("/api/v1/cars/{car}/drives?source=import:teslafi&limit=200")).await;
    assert_eq!(d.status, 200, "{}", d.text());
    d.json()["items"].as_array().unwrap().iter().map(|x| x["start_ms"].as_u64().unwrap()).collect()
}

/// A month with one row missing its VIN, zipped after a readme; then the same month read again in another zone.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_blank_vin_a_readme_first_and_a_month_read_again_in_another_zone() {
    let k = kit().await;
    let car = format!("c{}", k.car);
    let mut admin = k.s.admin().await;
    let mut g = tf::Gen::new(tf::VIN1);
    let march = tf::month(&mut g, 2023, 3);
    let mut lines: Vec<String> = march.lines().map(String::from).collect();
    let asleep = lines.iter().skip(300).position(|l| l.contains(",asleep,")).unwrap() + 300;
    lines[asleep] = lines[asleep].replace(tf::VIN1, "");
    let blanked = lines.join("\r\n");
    let mut z = import::zip::ManagerZip::new(Vec::new());
    z.entry("readme.txt", b"TeslaFi data download").unwrap();
    z.entry("TeslaFi32023.csv", blanked.as_bytes()).unwrap();
    let zip = z.finish().unwrap();

    let id = upload(&mut admin, "teslafi.zip", &zip).await;
    let j = wait_for(&mut admin, &id, "ready").await;
    assert_eq!(j["kind"], "teslafi", "the readme is passed over: {j}");
    assert_eq!(j["cars_to_match"], json!(0), "one car, matched: {j}");
    assert_eq!(j["report"]["files_skipped"], json!({ "readme.txt": "not TeslaFi's data" }));
    let cars = admin.get(&format!("/api/v1/imports/{id}/cars")).await.json();
    assert_eq!(cars["items"].as_array().unwrap().len(), 1, "{cars}");
    assert_eq!(cars["items"][0]["match"], json!(car));
    let r = admin.post(&format!("/api/v1/imports/{id}/start"), json!({})).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let done = wait_for(&mut admin, &id, "done").await;
    assert_eq!(done["report"]["tables"]["drives"]["imported"], json!(62), "{done}");
    let ny = drive_starts(&mut admin, &car).await;
    assert_eq!(ny.len(), 62);

    // the same month, the owner saying it was Berlin's time: read again there, its earlier rows replaced
    let again = upload(&mut admin, "TeslaFi32023.csv", march.as_bytes()).await;
    let j = wait_for(&mut admin, &again, "ready").await;
    assert_eq!(j["report"]["rows"]["used"], json!(0), "in the car's zone it is all in: {j}");
    let r = admin.post(&format!("/api/v1/imports/{again}/start"), json!({ "time_zone": "Europe/Berlin" })).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let done = wait_for(&mut admin, &again, "done").await;
    let rep = &done["report"];
    assert_eq!(rep["time_zone"]["name"], "Europe/Berlin");
    assert_eq!(rep["found"]["read_again"], json!(1), "{rep}");
    assert!(rep["warnings"].as_array().unwrap().iter().any(|w| w.as_str().unwrap().contains("America/New_York")));
    // (2023-03-26 02:00 and 02:30 never happened in Berlin)
    assert_eq!(rep["rows"]["skipped"], json!({ "dst_gap": 2 }), "{rep}");
    assert_eq!(rep["tables"]["drives"]["imported"], json!(62));
    // 2023-03-01 00:00 in Berlin (CET) is 2023-02-28 23:00 UTC: the car's figures are this run's, not the check's
    assert_eq!(rep["span"]["from_ms"], json!(1_677_625_200_000u64));
    assert_eq!(rep["cars"][0]["first_ms"], rep["span"]["from_ms"], "{rep}");
    assert_eq!(rep["cars"][0]["last_ms"], rep["span"]["to_ms"], "{rep}");
    let berlin = drive_starts(&mut admin, &car).await;
    assert_eq!(berlin.len(), 62, "no drive at the New York times is left");
    let mut shifted: Vec<u64> = ny.iter().map(|t| t - 6 * 3_600_000).collect();
    let mut b = berlin.clone();
    shifted.sort_unstable();
    b.sort_unstable();
    // (until 12 March New York is 6 h behind Berlin)
    assert_eq!(b[..20], shifted[..20]);
}

/// One chunk of file `k` of a multi-file import.
async fn send_part(c: &mut Client, url: &str, bytes: &[u8], off: usize) -> usize {
    let part = &bytes[off..(off + 900_000).min(bytes.len())];
    let (o, l, sha) = (off.to_string(), bytes.len().to_string(), hex::encode(Sha256::digest(part)));
    let r = c
        .request("PATCH", url, Some(part), &[("Upload-Offset", &o), ("Upload-Length", &l), ("Upload-SHA256", &sha)])
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    off + part.len()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_monthly_csvs_uploaded_side_by_side_are_one_import_and_a_left_out_car_leaves_the_report() {
    let k = kit().await;
    let mut admin = k.s.admin().await;
    let mut g = tf::Gen::new(tf::VIN1);
    let march = tf::month(&mut g, 2023, 3).into_bytes();
    let april = tf::month(&mut g, 2023, 4).into_bytes();
    let r = admin
        .post(
            "/api/v1/imports",
            json!({ "files": [
                { "name": "TeslaFi42023.csv", "bytes_total": april.len() },
                { "name": "TeslaFi32023.csv", "bytes_total": march.len() },
            ] }),
        )
        .await;
    assert_eq!(r.status, 201, "{}", r.text());
    let j = r.json();
    let id = j["id"].as_str().unwrap().to_string();
    assert_eq!(j["name"], "TeslaFi42023.csv and 1 more files");
    let urls: Vec<String> =
        j["files"].as_array().unwrap().iter().map(|f| f["upload_url"].as_str().unwrap().into()).collect();
    assert_eq!(urls.len(), 2);
    // the files' chunks interleaved, as a page sending them side by side does
    let (mut a, mut b) = (0, 0);
    while a < april.len() || b < march.len() {
        if a < april.len() {
            a = send_part(&mut admin, &urls[0], &april, a).await;
        }
        if b < march.len() {
            b = send_part(&mut admin, &urls[1], &march, b).await;
        }
    }
    let j = wait_for(&mut admin, &id, "ready").await;
    let rep = &j["report"];
    assert_eq!(rep["files"], json!(2), "{rep}");
    assert_eq!(rep["found"]["drives"], json!(122), "the same as the two months in one zip: {rep}");
    assert_eq!(rep["uploads"].as_array().unwrap().len(), 2, "{rep}");
    assert_eq!(rep["uploads"][1]["name"], "TeslaFi32023.csv");
    assert_eq!(rep["cars"].as_array().unwrap().len(), 1);

    let cars = admin.get(&format!("/api/v1/imports/{id}/cars")).await.json();
    let tm = cars["items"][0]["tm_car"].clone();
    let r = admin
        .put(&format!("/api/v1/imports/{id}/cars"), json!({ "items": [{ "tm_car": tm, "match": "leave_out" }] }))
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    let j = admin.get(&format!("/api/v1/imports/{id}")).await.json();
    let rep = &j["report"];
    assert_eq!(rep["cars"], json!([]), "a left-out car is not in the report: {rep}");
    assert_eq!(rep["found"]["drives"], json!(0), "nor in its counts: {rep}");
    assert_eq!(rep["tables"]["drives"]["read"], json!(0), "{rep}");
    assert_eq!(rep["cars_count"], json!(0), "{rep}");
    let cars = admin.get(&format!("/api/v1/imports/{id}/cars")).await.json();
    assert_eq!(cars["items"][0]["match"], "leave_out", "still in the matching list: {cars}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn several_files_that_are_not_all_teslafi_are_refused() {
    let k = kit().await;
    let mut admin = k.s.admin().await;
    let mut g = tf::Gen::new(tf::VIN1);
    let march = tf::month(&mut g, 2023, 3).into_bytes();
    let junk = b"-- a TeslaMate backup\nCREATE TABLE x ();\n".to_vec();
    for (files, code) in [
        ([("TeslaFi32023.csv", &march), ("backup.sql", &junk)], "not_a_teslafi_export"),
        ([("backup.sql", &junk), ("TeslaFi32023.csv", &march)], "one_file_only"),
    ] {
        let body: Vec<Value> = files.iter().map(|(n, b)| json!({ "name": n, "bytes_total": b.len() })).collect();
        let j = admin.post("/api/v1/imports", json!({ "files": body })).await.json();
        let id = j["id"].as_str().unwrap().to_string();
        for (i, (_, b)) in files.iter().enumerate() {
            let url = j["files"][i]["upload_url"].as_str().unwrap().to_string();
            let mut off = 0;
            while off < b.len() {
                off = send_part(&mut admin, &url, b, off).await;
            }
        }
        let j = wait_for(&mut admin, &id, "failed").await;
        assert_eq!(j["error"]["code"], code, "{j}");
        assert_eq!(admin.request("DELETE", &format!("/api/v1/imports/{id}"), None, &[]).await.status, 200);
    }
}
