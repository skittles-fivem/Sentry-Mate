#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! Sentry Six Manager's CAN export through the real router (web::testkit): a zip in the Manager's layout of
//! made-up logs signed by a made-up bridge key (one of them changed after it was signed) uploaded in chunks, told
//! apart from a TeslaMate backup by its contents, checked (nothing stored, no cars to match), imported (the good logs
//! stored and judged as uploads are, the changed one refused), and imported again (nothing doubled). A zip that is
//! not a CAN export fails with a clear error.

use engine::Engine;
use import::zip::ManagerZip;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use web::testkit::{Client, TestServer};

const KEY: &str = "abcdef012345";
/// 2026-01-01T00:00:00Z.
const T0_MS: u64 = 1_767_225_600_000;

struct Kit {
    s: TestServer,
    engine: Engine,
    store: ingest::Store,
    _dir: tempfile::TempDir,
}

async fn kit() -> Kit {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let (v, _) = vault::Vault::create(&data, &dir.path().join("keys"), vault::Kdf::FAST_FOR_TESTS).unwrap();
    let v = Arc::new(v);
    let db = db::Db::open(&data, v.clone(), db::Options::default()).unwrap();
    let store = ingest::Store::with_limits(data.join("can"), v, 0, 200_000, |_| u64::MAX);
    let engine = Engine::new(db, store.clone(), engine::Options::default());
    engine.start().unwrap();
    let e2 = engine.clone();
    let imports = Arc::new(web::resources::imports::Imports::new());
    imports.set_gate(gate::Gate::memory());
    let s = TestServer::start_with(move |b| b.service(Arc::new(e2)).service(imports)).await;
    Kit { s, engine, store, _dir: dir }
}

/// `n` one-minute logs (a few made-up frames each), signed in order; the one at `tamper` changed after signing.
fn export(n: u64, tamper: Option<usize>) -> (Vec<u8>, Vec<String>) {
    let mut signer = synth::Signer::from_seed(b"web can import test");
    let mut z = ManagerZip::new(Vec::new());
    let mut names = Vec::new();
    let mut listed = Vec::new();
    for i in 0..n {
        let start = T0_MS + i * 60_000;
        let text = format!("# canbridge v1 app=test\n({}.010000) can0 118#0000000000000000\n", start / 1000);
        let mut xz = synth::xz::pack(text.as_bytes(), 1).unwrap();
        let name = format!("{KEY}_20260101T00{i:02}00Z_{}.log.xz", i + 1);
        let side = json!({ "format": "canbridge-candump-xz/1", "start_wall_ms": start, "end_wall_ms": start + 60_000,
                           "closed_because": "rotate", "integrity": signer.sign(KEY, &name, &xz) });
        if tamper == Some(i as usize) {
            let at = xz.len() / 2;
            xz[at] ^= 1;
        }
        z.entry(&format!("{KEY}/{name}"), &xz).unwrap();
        z.entry(&format!("{KEY}/{name}.json"), &serde_json::to_vec_pretty(&side).unwrap()).unwrap();
        z.entry(&format!("{KEY}/{}", name.strip_suffix(".xz").unwrap()), text.as_bytes()).unwrap();
        listed.push(json!({ "name": name, "startedAt": "2026-01-01T00:00:00.000Z", "bytes": xz.len(),
                            "sidecar": true, "format": "canbridge-candump-xz/1", "check": "verified" }));
        names.push(name);
    }
    z.entry("ids.csv", b"bus,id_hex,message,unknown,frames,hz,first_seen_utc,last_seen_utc,minutes_seen\r\n").unwrap();
    let manifest = json!({ "vehicleKey": KEY, "from": "2026-01-01T00:00:00.000Z", "to": "2026-01-01T01:00:00.000Z",
                           "madeAt": "2026-01-02T00:00:00.000Z", "logs": listed, "ids": { "count": 0, "unknown": 0 },
                           "note": "made-up" });
    z.entry("manifest.json", serde_json::to_string_pretty(&manifest).unwrap().as_bytes()).unwrap();
    (z.finish().unwrap(), names)
}

async fn upload(c: &mut Client, name: &str, bytes: &[u8], step: usize) -> String {
    let r = c.post("/api/v1/imports", json!({ "name": name, "bytes_total": bytes.len() })).await;
    assert_eq!(r.status, 201, "{}", r.text());
    let made = r.json();
    let id = made["id"].as_str().unwrap().to_string();
    if name.ends_with(".zip") {
        assert_eq!(made["compression"], "zip");
    }
    assert_eq!(made["kind"], Value::Null, "known once checked");
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

async fn wait_for(c: &mut Client, id: &str, status: &str) -> Value {
    for _ in 0..600 {
        let j = c.get(&format!("/api/v1/imports/{id}")).await.json();
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_manager_can_export_is_checked_imported_and_never_doubled() {
    let k = kit().await;
    let mut admin = k.s.admin().await;
    let (zip, names) = export(5, Some(3));
    let id = upload(&mut admin, "can-abcdef012345-20260101T0000-to-20260101T0100.zip", &zip, 4096).await;
    let ready = wait_for(&mut admin, &id, "ready").await;
    assert_eq!(ready["kind"], "sentry_manager_can", "{ready}");
    assert_eq!(ready["cars_to_match"], 0);
    let rep = &ready["report"];
    assert_eq!(rep["logs"]["read"], 5, "{rep:#}");
    assert_eq!(rep["logs"]["to_import"], 4);
    assert_eq!(rep["logs"]["refused"], 1);
    assert_eq!(rep["refused"]["failed_check"], 1);
    assert_eq!(rep["refused_logs"][0]["name"], names[3]);
    assert_eq!(rep["cars"][0]["key"], KEY);
    assert_eq!(rep["files"]["candump_text"], 5);
    assert_eq!(rep["manifest"]["vehicle_key"], KEY);
    assert!(!k.store.exists(&names[0]), "the check stores nothing");
    let cars = admin.get(&format!("/api/v1/imports/{id}/cars")).await;
    assert_eq!(cars.status, 200);
    assert_eq!(cars.json()["items"], json!([]));

    let r = admin.post(&format!("/api/v1/imports/{id}/start"), json!({})).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let done = wait_for(&mut admin, &id, "done").await;
    k.engine.idle().await;
    let rep = &done["report"];
    assert_eq!(rep["imported"], true);
    assert_eq!(rep["logs"]["imported"], 4, "{rep:#}");
    assert_eq!(rep["logs"]["refused"], 1);
    assert_eq!(done["bytes_read"], zip.len());
    for (i, n) in names.iter().enumerate() {
        assert_eq!(k.store.exists(n), i != 3, "{n}");
    }
    let rec = k.engine.judge().record(KEY, &names[0]).await.unwrap();
    assert_eq!(rec.status, ingest::judge::Status::Verified);
    assert!(!k._dir.path().join("data/web/imports").join(&id).join("spool").exists(), "the spool is gone");

    // the same zip again: everything is here already
    let id2 = upload(&mut admin, "again.zip", &zip, 1 << 20).await;
    wait_for(&mut admin, &id2, "ready").await;
    admin.post(&format!("/api/v1/imports/{id2}/start"), json!({})).await;
    let again = wait_for(&mut admin, &id2, "done").await;
    assert_eq!(again["report"]["logs"]["imported"], 0, "{:#}", again["report"]);
    assert_eq!(again["report"]["logs"]["already_here"], 4);
    assert_eq!(k.store.log_count(KEY).await.unwrap(), 4);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_zip_that_is_not_a_can_export_and_a_zip_that_is_not_a_zip_fail_clearly() {
    let k = kit().await;
    let mut admin = k.s.admin().await;
    let mut z = ManagerZip::new(Vec::new());
    z.entry("holiday/notes.txt", b"nothing to see").unwrap();
    let junk = z.finish().unwrap();
    let id = upload(&mut admin, "photos.zip", &junk, 1 << 20).await;
    let f = wait_for(&mut admin, &id, "failed").await;
    assert_eq!(f["error"]["code"], "not_a_can_export", "{f}");
    assert!(f["error"]["message"].as_str().unwrap().contains("CAN export bundle"), "{f}");
    let id = upload(&mut admin, "named.zip", b"-- this is SQL, not a zip\n", 1 << 20).await;
    let f = wait_for(&mut admin, &id, "failed").await;
    assert_eq!(f["error"]["code"], "unreadable", "{f}");
    assert!(f["error"]["message"].as_str().unwrap().contains("not a zip"), "{f}");
}
