#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! The kept upload of an import, packed by compaction (the `kept` crate, docs/plans/storage.md): only once the
//! import is over; read, checked and imported from its pack as from the upload; stopped at every step and carried
//! on; a damaged pack named, nothing removed for it; a discard removes everything.

use engine::Engine;
use engine::compact::Compacted;
use model::CarId;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::Arc;
use web::testkit::{Client, TestServer};

const FIXTURE: &[u8] = include_bytes!("../../import/tests/fixtures/teslamate-4.4.0.sql");

struct Kit {
    s: TestServer,
    imports: Arc<web::resources::imports::Imports>,
    engine: Engine,
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
    let store = ingest::Store::new(data.join("can"), v);
    let engine = Engine::new(db.clone(), store, engine::Options::default());
    engine.start().unwrap();
    let (e2, d2) = (engine.clone(), db.clone());
    let imports = Arc::new(web::resources::imports::Imports::new());
    let i2 = imports.clone();
    let s = TestServer::start_with(move |b| b.cars(Arc::new(EngineCars(d2))).service(Arc::new(e2)).service(i2)).await;
    // as the server does at its start: the web side's step of every compaction pass
    engine.compact_step(imports.kept_step(&s.app.dir, s.app.vault.clone()));
    Kit { s, imports, engine, _dir: dir }
}

impl Kit {
    fn folder(&self, id: &str) -> PathBuf {
        self.s.app.dir.join("imports").join(id)
    }

    fn files(&self, id: &str) -> Vec<String> {
        let Ok(rd) = std::fs::read_dir(self.folder(id)) else { return Vec::new() };
        let mut v: Vec<String> = rd.map(|f| f.unwrap().file_name().into_string().unwrap()).collect();
        v.sort();
        v
    }

    /// A whole compaction pass, as "Compact now" runs it.
    async fn compact(&self) -> Compacted {
        let e = self.engine.clone();
        tokio::task::spawn_blocking(move || e.compact(&|| false, false).unwrap()).await.unwrap()
    }

    /// Puts the import back to a state, as its job on the disk, and forgets what the server knew: a restart.
    fn restart_as(&self, id: &str, status: &str) {
        let (path, label) = (self.folder(id).join("job"), format!("web/imports/{id}/job"));
        let raw = self.s.app.vault.read_file(&path, &label).unwrap().unwrap();
        let mut job: Value = serde_json::from_slice(&raw).unwrap();
        job["status"] = json!(status);
        job["error"] = Value::Null;
        self.s.app.vault.write_file(&path, &label, &serde_json::to_vec(&job).unwrap()).unwrap();
        self.imports.forget_jobs();
    }
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
        assert!(j["status"] != "failed" || status == "failed", "{j}");
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("{id} never got {status}");
}

/// Matches every imported car still open as a new car, and runs the import to its end.
async fn import(c: &mut Client, id: &str) -> Value {
    import_with(c, id, json!({})).await
}

/// The same, with what the start is told (a TeslaFi import's time zone).
async fn import_with(c: &mut Client, id: &str, start: Value) -> Value {
    let cars = c.get(&format!("/api/v1/imports/{id}/cars")).await.json();
    let items: Vec<Value> = cars["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|x| x["match"].is_null())
        .map(|x| json!({ "tm_car": x["tm_car"], "match": "new" }))
        .collect();
    if !items.is_empty() {
        let r = c.put(&format!("/api/v1/imports/{id}/cars"), json!({ "items": items })).await;
        assert_eq!(r.status, 200, "{}", r.text());
    }
    let r = c.post(&format!("/api/v1/imports/{id}/start"), start).await;
    assert_eq!(r.status, 200, "{}", r.text());
    wait_for(c, id, "done").await
}

/// Checks the import again, as a restart in the middle of its check does, and imports it again: what the check
/// and the import report. (A second run finds the rows of the first already here, so runs are compared from the
/// second on.)
async fn check_and_import_again(k: &Kit, c: &mut Client, id: &str) -> (Value, Value) {
    check_and_import_again_with(k, c, id, json!({})).await
}

async fn check_and_import_again_with(k: &Kit, c: &mut Client, id: &str, start: Value) -> (Value, Value) {
    k.restart_as(id, "checking");
    let checked = wait_for(c, id, "ready").await;
    let cars = c.get(&format!("/api/v1/imports/{id}/cars")).await.json();
    let done = import_with(c, id, start).await;
    (
        json!({ "report": checked["report"], "bytes_total": checked["bytes_total"], "cars": cars["items"] }),
        done["report"].clone(),
    )
}

fn made_up_dump() -> Vec<u8> {
    let mut out = Vec::new();
    synth::backup::dump(synth::backup::Options { seed: 9, days: 1, cars: 1 }, &mut out).unwrap();
    out
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_finished_import_is_packed_and_then_checks_and_imports_as_it_did_before() {
    let k = kit().await;
    let mut admin = k.s.admin().await;
    let dump = made_up_dump();
    let id = upload(&mut admin, "teslamate.sql", &dump).await;
    wait_for(&mut admin, &id, "ready").await;
    // not over yet: a pass leaves its upload alone
    let pass = k.compact().await;
    assert_eq!((pass.others, pass.problems.len()), (0, 0), "{pass:?}");
    assert_eq!(k.files(&id), ["backup", "job"]);
    import(&mut admin, &id).await;
    // checked and imported once more from the upload as it arrived: what the pack must give too
    let (checked, imported) = check_and_import_again(&k, &mut admin, &id).await;
    let done = admin.get(&format!("/api/v1/imports/{id}")).await.json();
    let before = std::fs::metadata(k.folder(&id).join("backup")).unwrap().len();
    assert!(before > dump.len() as u64);

    // over: the pass packs it
    let pass = k.compact().await;
    assert_eq!((pass.others, pass.stopped, pass.problems.len()), (1, false, 0), "{pass:?}");
    assert_eq!(k.files(&id), ["backup.pack", "job"]);
    let after = std::fs::metadata(k.folder(&id).join("backup.pack")).unwrap().len();
    assert!(after * 12 < before, "{before} -> {after}");
    assert!(pass.saved_bytes() >= before - after - 8192, "{pass:?}");
    let cats = engine::query::storage::categories(&k.engine).unwrap();
    assert!(cats.iter().all(|c| c.key != "imports.uploads"), "the engine's own folder holds no imports here");
    // the job is what it was
    let now = admin.get(&format!("/api/v1/imports/{id}")).await.json();
    assert_eq!(now, done);
    // a pass with nothing left to pack
    let pass = k.compact().await;
    assert_eq!((pass.others, pass.problems.len()), (0, 0), "{pass:?}");

    // checked and imported from its pack, it is the file it was: the same cars, the same rows, the same report
    let (checked_packed, imported_packed) = check_and_import_again(&k, &mut admin, &id).await;
    assert_eq!(checked_packed, checked);
    assert_eq!(imported_packed, imported);
    assert_eq!(k.files(&id), ["backup.pack", "job"]);

    // discarded: everything of it goes
    let r = admin.delete(&format!("/api/v1/imports/{id}"), json!({})).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert!(!k.folder(&id).exists());
    let pass = k.compact().await;
    assert_eq!((pass.others, pass.problems.len()), (0, 0), "{pass:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_pack_stopped_at_any_step_leaves_the_import_readable_and_the_next_pass_finishes() {
    let k = kit().await;
    let mut admin = k.s.admin().await;
    for stop in [kept::Stop::Blocks(1), kept::Stop::TempWritten, kept::Stop::Renamed] {
        let id = upload(&mut admin, "teslamate.sql", FIXTURE).await;
        wait_for(&mut admin, &id, "ready").await;
        import(&mut admin, &id).await;
        let (checked, imported) = check_and_import_again(&k, &mut admin, &id).await;
        k.imports.kept_stop_at(Some(stop));
        let pass = k.compact().await;
        assert!(pass.stopped && pass.others == 0, "{stop:?}: {pass:?}");
        let want: &[&str] = match stop {
            kept::Stop::Renamed => &["backup", "backup.pack", "job"],
            _ => &["backup", "backup.pack.tmp", "job"],
        };
        assert_eq!(k.files(&id), want, "{stop:?}");
        // a restart in the middle of a check: it reads the upload, which wins while it is there
        k.imports.kept_stop_at(None);
        k.restart_as(&id, "checking");
        wait_for(&mut admin, &id, "ready").await;
        let pass = k.compact().await;
        assert_eq!(pass.others, 0, "not over: left as it is");
        assert_eq!(k.files(&id), want, "{stop:?}");
        import(&mut admin, &id).await;
        let again = check_and_import_again(&k, &mut admin, &id).await;
        assert_eq!(again, (checked.clone(), imported.clone()), "{stop:?}");
        // over again: the next pass finishes the job
        let pass = k.compact().await;
        assert_eq!((pass.others, pass.stopped, pass.problems.len()), (1, false, 0), "{stop:?}: {pass:?}");
        assert_eq!(k.files(&id), ["backup.pack", "job"], "{stop:?}");
        let again = check_and_import_again(&k, &mut admin, &id).await;
        assert_eq!(again, (checked, imported), "{stop:?}: from the pack");
        let r = admin.delete(&format!("/api/v1/imports/{id}"), json!({})).await;
        assert_eq!(r.status, 200, "{}", r.text());
        assert!(!k.folder(&id).exists(), "{stop:?}");
    }
    // discarded with a temporary file and both forms in its folder: all of it goes
    for stop in [kept::Stop::TempWritten, kept::Stop::Renamed] {
        let id = upload(&mut admin, "teslamate.sql", FIXTURE).await;
        wait_for(&mut admin, &id, "ready").await;
        import(&mut admin, &id).await;
        k.imports.kept_stop_at(Some(stop));
        k.compact().await;
        k.imports.kept_stop_at(None);
        assert_eq!(k.files(&id).len(), 3);
        let r = admin.delete(&format!("/api/v1/imports/{id}"), json!({})).await;
        assert_eq!(r.status, 200, "{}", r.text());
        assert!(!k.folder(&id).exists());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_failed_import_is_packed_too_and_a_packed_upload_is_left_as_uploaded() {
    let k = kit().await;
    let mut admin = k.s.admin().await;
    // not a backup at all: its import fails, and is over
    let text = "this is not a dump\n".repeat(5000);
    let failed = upload(&mut admin, "notes.sql", text.as_bytes()).await;
    wait_for(&mut admin, &failed, "failed").await;
    // a gzip of the made-up backup: imported, and kept as it was uploaded
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(6));
    std::io::Write::write_all(&mut gz, FIXTURE).unwrap();
    let gz = gz.finish().unwrap();
    let packed = upload(&mut admin, "teslamate.sql.gz", &gz).await;
    wait_for(&mut admin, &packed, "ready").await;
    import(&mut admin, &packed).await;
    let before = std::fs::read(k.folder(&packed).join("backup")).unwrap();
    for round in 0..2 {
        let pass = k.compact().await;
        assert_eq!((pass.others, pass.problems.len()), (u64::from(round == 0), 0), "{pass:?}");
        assert_eq!(k.files(&failed), ["backup.pack", "job"]);
        assert_eq!(k.files(&packed), ["backup", "job"]);
    }
    assert_eq!(std::fs::read(k.folder(&packed).join("backup")).unwrap(), before, "not a byte of it changed");
    // the failed one's bytes are all there
    let mut out = Vec::new();
    let path = k.folder(&failed).join("backup.pack");
    let label = format!("web/imports/{failed}/backup.pack");
    std::io::Read::read_to_end(&mut kept::Reader::open(&k.s.app.vault, &path, &label).unwrap(), &mut out).unwrap();
    assert_eq!(out, text.as_bytes());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_damaged_pack_is_reported_by_its_name_and_nothing_is_removed() {
    let k = kit().await;
    let mut admin = k.s.admin().await;
    let dump = made_up_dump();
    let id = upload(&mut admin, "teslamate.sql", &dump).await;
    wait_for(&mut admin, &id, "ready").await;
    import(&mut admin, &id).await;
    assert_eq!(k.compact().await.others, 1);
    let path = k.folder(&id).join("backup.pack");
    let good = std::fs::read(&path).unwrap();
    let name = format!("web/imports/{id}/backup.pack");
    // cut short: a pass says so, by name, and leaves it
    std::fs::write(&path, &good[..good.len() - 7]).unwrap();
    let pass = k.compact().await;
    assert_eq!(pass.others, 0);
    assert!(pass.problems.len() == 1 && pass.problems[0].starts_with(&name), "{pass:?}");
    assert_eq!(k.files(&id), ["backup.pack", "job"]);
    assert_eq!(std::fs::read(&path).unwrap().len(), good.len() - 7);
    // a flipped byte in its middle: whatever reads it is told the file (and the block, when the file's ends still
    // open), and takes nothing of it as the whole file
    let mut bad = good.clone();
    let at = bad.len() / 2;
    bad[at] ^= 4;
    std::fs::write(&path, &bad).unwrap();
    k.restart_as(&id, "checking");
    let failed = wait_for(&mut admin, &id, "failed").await;
    let message = failed["error"]["message"].as_str().unwrap();
    assert!(message.contains(&format!("{name}: ")), "{message}");
    assert_eq!(std::fs::read(&path).unwrap(), bad);
    // whole again, it is the upload again
    std::fs::write(&path, &good).unwrap();
    k.restart_as(&id, "checking");
    wait_for(&mut admin, &id, "ready").await;
    let pass = k.compact().await;
    assert!(pass.problems.is_empty(), "{pass:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_discard_while_its_upload_is_being_packed_removes_all_of_it() {
    let k = kit().await;
    let mut admin = k.s.admin().await;
    let mut dump = Vec::new();
    synth::backup::dump(synth::backup::Options { seed: 2, days: 9, cars: 1 }, &mut dump).unwrap();
    assert!(dump.len() > 4 << 20, "several blocks to pack: {}", dump.len());
    let id = upload(&mut admin, "teslamate.sql", &dump).await;
    wait_for(&mut admin, &id, "ready").await;
    import(&mut admin, &id).await;
    let e = k.engine.clone();
    let pass = tokio::task::spawn_blocking(move || e.compact(&|| false, false).unwrap());
    // once the pack is being written
    for _ in 0..2000 {
        if k.files(&id).len() > 2 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    let r = admin.delete(&format!("/api/v1/imports/{id}"), json!({})).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert!(!k.folder(&id).exists(), "nothing of it is left, and nothing comes back");
    let pass = pass.await.unwrap();
    assert!(pass.problems.is_empty() && !pass.stopped, "{pass:?}");
    assert!(!k.folder(&id).exists());
    assert_eq!(admin.get(&format!("/api/v1/imports/{id}")).await.status, 404);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn teslafi_files_uploaded_side_by_side_are_each_packed_and_import_as_before() {
    let k = kit().await;
    let mut admin = k.s.admin().await;
    let mut car = synth::backup::Teslafi::new(3);
    let files = [("TeslaFi12024.csv", car.month(2024, 1)), ("TeslaFi22024.csv", car.month(2024, 2))];
    let body: Vec<Value> = files.iter().map(|(n, b)| json!({ "name": n, "bytes_total": b.len() })).collect();
    let r = admin.post("/api/v1/imports", json!({ "files": body })).await;
    assert_eq!(r.status, 201, "{}", r.text());
    let j = r.json();
    let id = j["id"].as_str().unwrap().to_string();
    for (i, (_, text)) in files.iter().enumerate() {
        let (url, bytes) = (j["files"][i]["upload_url"].as_str().unwrap(), text.as_bytes());
        let mut off = 0;
        while off < bytes.len() {
            let part = &bytes[off..(off + 1_000_000).min(bytes.len())];
            let (o, l, sha) = (off.to_string(), bytes.len().to_string(), hex::encode(Sha256::digest(part)));
            let headers = [("Upload-Offset", o.as_str()), ("Upload-Length", &l), ("Upload-SHA256", &sha)];
            let r = admin.request("PATCH", url, Some(part), &headers).await;
            assert_eq!(r.status, 200, "{}", r.text());
            off += part.len();
        }
    }
    let zone = json!({ "time_zone": "Europe/Berlin" });
    wait_for(&mut admin, &id, "ready").await;
    import_with(&mut admin, &id, zone.clone()).await;
    let from_uploads = check_and_import_again_with(&k, &mut admin, &id, zone.clone()).await;
    assert_eq!(k.files(&id), ["backup", "backup.1", "job"]);
    let before: u64 =
        ["backup", "backup.1"].iter().map(|f| std::fs::metadata(k.folder(&id).join(f)).unwrap().len()).sum();
    let pass = k.compact().await;
    assert_eq!((pass.others, pass.stopped, pass.problems.len()), (2, false, 0), "{pass:?}");
    assert_eq!(k.files(&id), ["backup.1.pack", "backup.pack", "job"]);
    let after: u64 =
        ["backup.pack", "backup.1.pack"].iter().map(|f| std::fs::metadata(k.folder(&id).join(f)).unwrap().len()).sum();
    assert!(after * 25 < before, "{before} -> {after}");
    let from_packs = check_and_import_again_with(&k, &mut admin, &id, zone).await;
    assert_eq!(from_packs, from_uploads);
    let r = admin.delete(&format!("/api/v1/imports/{id}"), json!({})).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert!(!k.folder(&id).exists());
}

/// What a damaged drives table gets back from the imports: the drives an import stored are read again from its kept
/// upload (as uploaded and once packed), exactly as they were stored; an upload that is not there is said, with the
/// time its rows covered; a car no import went to has nothing to get back.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_kept_upload_of_an_import_gives_its_drives_back_for_a_repair() {
    let k = kit().await;
    let mut admin = k.s.admin().await;
    let dump = made_up_dump();
    let id = upload(&mut admin, "made-up-backup.sql", &dump).await;
    wait_for(&mut admin, &id, "ready").await;
    import(&mut admin, &id).await;
    let car = engine::query::all_cars(&k.engine).unwrap()[0];
    let mut q = db::Query::default().limit(db::MAX_PAGE);
    q.source = Some(model::Source::Teslamate);
    let mut stored = k.engine.db().list::<model::Drive>(car, &q).unwrap().items;
    stored.sort_by_key(|d| d.id);
    assert!(!stored.is_empty());
    let replay = web::resources::imports::replay(&k.s.app);
    let read = |car: CarId| {
        let (r, e) = (replay.clone(), k.engine.clone());
        async move { tokio::task::spawn_blocking(move || r.drives(&e, car)).await.unwrap() }
    };
    let sorted = |mut d: Vec<model::Drive>| {
        d.sort_by_key(|d| d.id);
        d
    };
    // as uploaded
    let got = read(car).await;
    assert_eq!(sorted(got.drives), stored);
    assert!(got.gone.is_empty() && got.held_out.is_empty());
    // packed
    let pass = k.compact().await;
    assert_eq!((pass.others, pass.problems.len()), (1, 0), "{pass:?}");
    assert_eq!(k.files(&id), ["backup.pack", "job"]);
    let got = read(car).await;
    assert_eq!(sorted(got.drives), stored, "from its pack");
    // another car: no import went to it
    let got = read(car + 100).await;
    assert!(got.drives.is_empty() && got.gone.is_empty());
    // the upload gone (the import is still listed): said, with what it covered
    std::fs::remove_file(k.folder(&id).join("backup.pack")).unwrap();
    let got = read(car).await;
    assert!(got.drives.is_empty());
    assert_eq!(got.gone.len(), 1, "{:?}", got.gone);
    let g = &got.gone[0];
    assert_eq!((g.name.as_str(), g.source), ("made-up-backup.sql", model::Source::Teslamate));
    assert!(g.why.contains("not kept"), "{}", g.why);
    assert!(g.from_ms.is_some() && g.to_ms >= g.from_ms, "{g:?}");
}
