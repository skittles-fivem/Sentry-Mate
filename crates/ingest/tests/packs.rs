//! Day packs: a finished day's logs and sidecars in one sealed file, every byte as uploaded, whatever happens while
//! it is made. Made-up keys and logs only.
#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]

use axum::body::{Body, Bytes};
use axum::http::{Request, StatusCode};
use base64::Engine;
use http_body_util::BodyExt;
use ingest::store::{FileState, PACKS, PackError, PackStop, QUARANTINE, State, UPLOADING};
use ingest::{Conn, Judge, Store, Tokens, Uploads, router};
use p256::ecdsa::{Signature, SigningKey, signature::Signer};
use p256::pkcs8::EncodePublicKey;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tower::ServiceExt;

const KEY: &str = "abcdef123456";
const DAY1: &str = "2026-11-22";
const DAY2: &str = "2026-11-23";
/// 2026-11-24T00:00:00Z: both days are over.
const AFTER: u64 = 1_795_478_400_000;
/// 2026-11-23T00:00:00Z: only the first day is over.
const AFTER_DAY1: u64 = AFTER - 86_400_000;

struct T {
    dir: tempfile::TempDir,
    vault: Arc<vault::Vault>,
    store: Store,
}

impl T {
    fn car(&self) -> PathBuf {
        self.dir.path().join("data/can").join(KEY)
    }

    /// Another server started on the same folder: nothing in memory.
    fn restarted(&self) -> Store {
        Store::with_limits(self.dir.path().join("data/can"), self.vault.clone(), 0, 200_000, |_| u64::MAX)
    }
}

fn setup() -> T {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let (v, _) = vault::Vault::create(&data, &dir.path().join("keys"), vault::Kdf::FAST_FOR_TESTS).unwrap();
    let vault = Arc::new(v);
    let store = Store::with_limits(data.join("can"), vault.clone(), 0, 200_000, |_| u64::MAX);
    T { dir, vault, store }
}

/// Log `i` of `day` (`20261122` or `20261123`).
fn name_on(day: &str, i: u64) -> String {
    format!("{KEY}_{}T{:02}{:02}00Z_{i}.log.xz", day.replace('-', ""), 10 + i / 60, i % 60)
}

fn name(i: u64) -> String {
    name_on(DAY1, i)
}

fn side(n: &str) -> String {
    format!("{n}.json")
}

fn sha(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}

/// Made-up bytes that don't compress, as an xz log's don't.
fn log_bytes(i: u64, len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(len + 32);
    let mut block = Sha256::digest(format!("made-up CAN log {i}")).to_vec();
    while out.len() < len {
        out.extend_from_slice(&block);
        block = Sha256::digest(&block).to_vec();
    }
    out.truncate(len);
    out
}

fn side_bytes(i: u64) -> Vec<u8> {
    serde_json::to_vec(&json!({ "format": "canbridge-columns-xz/2", "note": "made up", "index": i,
        "integrity": { "signed": false, "vehicle_key": KEY, "sha256": sha(&log_bytes(i, 64)) } }))
    .unwrap()
}

async fn put(store: &Store, name: &str, bytes: &[u8]) {
    let r = store.receive(name, Bytes::copy_from_slice(bytes), 0, bytes.len() as u64, &sha(bytes)).await.unwrap();
    assert!(r.complete && !r.already, "{name}");
}

/// A finished file written as the store keeps it, for what an upload refuses (an empty sidecar).
fn plant(t: &T, name: &str, bytes: &[u8]) {
    t.vault.write_file(&t.car().join(name), &format!("can/{KEY}/{name}"), bytes).unwrap();
}

/// Two days of logs and sidecars: ordinary ones, a 1-byte log, an empty sidecar, a log with no sidecar and a
/// sidecar with no log. Returns every name with its bytes.
async fn fill(t: &T) -> Vec<(String, Vec<u8>)> {
    let mut files = Vec::new();
    for (day, count) in [(DAY1, 6u64), (DAY2, 3)] {
        for i in 0..count {
            let n = name_on(day, i);
            files.push((side(&n), side_bytes(i)));
            files.push((n, log_bytes(i, 3000 + 517 * i as usize)));
        }
    }
    files.push((name(20), vec![0x5a]));
    files.push((name(21), log_bytes(21, 70_000)));
    files.push((side(&name(22)), side_bytes(22)));
    for (n, b) in &files {
        put(&t.store, n, b).await;
    }
    let empty = side(&name(21));
    plant(t, &empty, b"");
    files.push((empty, Vec::new()));
    files.sort();
    files
}

/// Everything the store says about `names`, to compare before and after packing.
async fn answers(
    store: &Store,
    names: &[String],
) -> Vec<(String, Option<Vec<u8>>, Option<(String, u64)>, bool, FileState)> {
    let states = store.status(names, Some(KEY)).await;
    let mut out = Vec::new();
    for (n, (sn, st)) in names.iter().zip(states) {
        assert_eq!(*n, sn);
        assert_eq!(store.state_of(n).await.unwrap(), st);
        out.push((
            n.clone(),
            store.read(n).await.unwrap().map(|b| b.to_vec()),
            store.hash(n).await.ok(),
            store.exists(n),
            st,
        ));
    }
    out
}

async fn assert_all_readable(store: &Store, files: &[(String, Vec<u8>)]) {
    for (n, b) in files {
        assert_eq!(store.read(n).await.unwrap().unwrap().as_slice(), b.as_slice(), "{n}");
        assert_eq!(store.hash(n).await.unwrap(), (sha(b), b.len() as u64), "{n}");
        assert!(store.exists(n), "{n}");
        let st = store.state_of(n).await.unwrap();
        assert_eq!((st.state, st.length), (State::Complete, Some(b.len() as u64)), "{n}");
    }
}

/// The finished logs and sidecars still in the car's folder itself.
fn loose(car: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(car)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|n| trust::LogName::parse(n).is_some())
        .collect();
    v.sort();
    v
}

fn pack_files(car: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(car.join(PACKS))
        .into_iter()
        .flatten()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    v.sort();
    v
}

#[tokio::test]
async fn every_file_reads_the_same_before_and_after_packing() {
    let t = setup();
    let files = fill(&t).await;
    let mut names: Vec<String> = files.iter().map(|(n, _)| n.clone()).collect();
    names.push(name(55)); // never uploaded
    names.push(side(&name(20))); // a log with no sidecar
    let before = answers(&t.store, &names).await;
    let count = t.store.log_count(KEY).await.unwrap();
    assert_eq!(count, 11);
    assert!(before.iter().any(|a| a.4.state == State::Missing));
    let stored_before: u64 = loose(&t.car()).iter().map(|n| std::fs::metadata(t.car().join(n)).unwrap().len()).sum();

    let told = std::cell::RefCell::new(Vec::new());
    let r = t.store.pack_finished_with_progress(AFTER, &|| false, &|p| told.borrow_mut().push(p.clone()));
    assert_eq!((r.days, r.files, r.failed, r.stopped), (2, files.len(), 0, false), "{r:?}");
    // the progress: the days counted first, then one more after each day, with what was freed so far (a
    // small day can take a little more in its pack than it freed, so that need not only grow)
    let told = told.into_inner();
    assert_eq!(told.iter().map(|p| (p.days_done, p.days_total)).collect::<Vec<_>>(), [(0, 2), (1, 2), (2, 2)]);
    assert_eq!(told[0].saved_bytes, 0);
    assert_eq!(told[2].saved_bytes, r.before_bytes - r.after_bytes);
    assert!(told.iter().all(|p| p.failed == 0 && p.last_error.is_none()));
    assert_eq!(r.before_bytes, stored_before);
    assert!(r.after_bytes < r.before_bytes, "{r:?}");
    assert_eq!(loose(&t.car()), Vec::<String>::new(), "every finished file is in its day pack only");
    assert_eq!(pack_files(&t.car()), vec![format!("{DAY1}.pack"), format!("{DAY2}.pack")]);
    let sizes = t.store.pack_sizes(KEY);
    assert_eq!(sizes.iter().map(|s| s.0.as_str()).collect::<Vec<_>>(), vec![DAY1, DAY2]);
    assert_eq!(sizes.iter().map(|s| s.1).sum::<u64>(), r.after_bytes);

    assert_eq!(answers(&t.store, &names).await, before, "the same answers from the packs");
    assert_all_readable(&t.store, &files).await;
    assert_eq!(t.store.log_count(KEY).await.unwrap(), count, "packing is not a change in how many logs a car has");

    // a server started again: the same, and a day read in a row opens its pack once
    let again = t.restarted();
    assert_eq!(answers(&again, &names).await, before);
    assert_eq!(again.pack_opens(), 2, "two days, each pack unsealed once");
    assert_eq!(again.log_count(KEY).await.unwrap(), count);

    // a second pass has nothing to do
    let r = again.pack_finished(AFTER, &|| false);
    assert_eq!((r.days, r.files, r.before_bytes, r.after_bytes, r.failed), (0, 0, 0, 0, 0));

    // a car stored before counts were kept (or whose count was lost): the packed logs are counted
    std::fs::remove_file(t.car().join("counts.json")).unwrap();
    let recount = t.restarted();
    assert_eq!(recount.log_count(KEY).await.unwrap(), count);
    assert_eq!(recount.folder_scans(), 1);
}

#[tokio::test]
async fn a_packed_name_uploaded_again_answers_as_a_stored_one() {
    let t = setup();
    let files = fill(&t).await;
    t.store.pack_finished(AFTER, &|| false);
    let store = t.restarted();
    let (n, b) = files.iter().find(|(n, _)| *n == name(2)).unwrap();
    // the same bytes: already here
    let r = store.receive(n, Bytes::copy_from_slice(b), 0, b.len() as u64, &sha(b)).await.unwrap();
    assert!(r.complete && r.already && !r.replaced);
    assert_eq!(r.received, b.len() as u64);
    // other bytes: refused, as for any finished file
    let other = log_bytes(99, b.len());
    let e = store.receive(n, Bytes::copy_from_slice(&other), 0, other.len() as u64, &sha(&other)).await.unwrap_err();
    assert_eq!(e.code, "exists_different");
    let e = store.receive(n, Bytes::from_static(b"x"), 0, 1, &sha(b"x")).await.unwrap_err();
    assert_eq!(e.code, "exists_different");
    // its sidecar too
    let (sn, sb) = files.iter().find(|(n, _)| *n == side(&name(2))).unwrap();
    let r = store.receive(sn, Bytes::copy_from_slice(sb), 0, sb.len() as u64, &sha(sb)).await.unwrap();
    assert!(r.already && !r.orphan);
    assert_eq!(loose(&t.car()), Vec::<String>::new(), "nothing was written beside the pack");
    assert_eq!(store.log_count(KEY).await.unwrap(), 11);
    // a sidecar arriving for a packed log is no orphan
    let r = store
        .receive(&side(&name(20)), Bytes::from(side_bytes(20)), 0, side_bytes(20).len() as u64, &sha(&side_bytes(20)))
        .await
        .unwrap();
    assert!(r.complete && !r.orphan);
}

#[tokio::test]
async fn only_days_that_are_over_are_packed_and_uploads_under_way_are_left() {
    let t = setup();
    let files = fill(&t).await;
    // an upload of the first day still arriving, and a bad copy kept aside
    let part = name(40);
    let whole = log_bytes(40, 500);
    let r = t.store.receive(&part, Bytes::copy_from_slice(&whole[..200]), 0, 500, &sha(&whole)).await.unwrap();
    assert!(!r.complete);
    std::fs::create_dir_all(t.car().join(QUARANTINE)).unwrap();
    t.vault.write_file(&t.car().join(QUARANTINE).join(name(41)), &format!("can/{KEY}/{}", name(41)), b"bad").unwrap();

    let r = t.store.pack_finished(AFTER_DAY1, &|| false);
    assert_eq!((r.days, r.failed), (1, 0), "{r:?}");
    assert_eq!(pack_files(&t.car()), vec![format!("{DAY1}.pack")]);
    let day2: Vec<String> = files.iter().map(|f| f.0.clone()).filter(|n| n.contains("20261123T")).collect();
    assert_eq!(loose(&t.car()), day2, "the day not over yet stays as it is");
    assert_all_readable(&t.store, &files).await;
    let st = t.store.state_of(&part).await.unwrap();
    assert_eq!((st.state, st.received), (State::Partial, 200));
    assert!(t.car().join(UPLOADING).join(format!("{part}.part")).exists());
    assert_eq!(t.store.quarantined(KEY).await, vec![name(41)]);
    assert!(!t.store.exists(&name(41)), "a bad copy kept aside is not a stored log");
    // the upload finishes after its day was packed: it is in the folder until the next pass
    let r = t.store.receive(&part, Bytes::copy_from_slice(&whole[200..]), 200, 500, &sha(&whole)).await.unwrap();
    assert!(r.complete);
    assert_eq!(t.store.read(&part).await.unwrap().unwrap().as_slice(), whole.as_slice());

    // a pass told to stop does nothing more
    let r = t.store.pack_finished(AFTER, &|| true);
    assert!(r.stopped && r.days == 0);
    assert!(loose(&t.car()).contains(&part));
    assert!(matches!(t.store.pack_day("no/such", DAY1), Err(PackError::BadName)));
    assert!(matches!(t.store.pack_day(KEY, "2026-13-01"), Err(PackError::BadName)));
    assert!(matches!(t.store.pack_day(KEY, "20261122"), Err(PackError::BadName)));
}

#[tokio::test]
async fn a_file_in_the_folder_wins_and_a_later_pass_merges_it_into_the_days_pack() {
    let t = setup();
    let mut files = fill(&t).await;
    t.store.pack_finished(AFTER, &|| false);
    let first_pack = std::fs::read(t.car().join(PACKS).join(format!("{DAY1}.pack"))).unwrap();

    // a log of the packed day that arrives late, and a file beside the pack under a packed name
    let late = (name(30), log_bytes(30, 4000));
    put(&t.store, &late.0, &late.1).await;
    let late_side = (side(&late.0), side_bytes(30));
    put(&t.store, &late_side.0, &late_side.1).await;
    let newer = log_bytes(77, 1234);
    plant(&t, &name(3), &newer);
    assert_eq!(t.store.read(&name(3)).await.unwrap().unwrap().as_slice(), newer.as_slice(), "the folder wins");
    assert_eq!(t.store.hash(&name(3)).await.unwrap(), (sha(&newer), 1234));
    assert_eq!(t.store.state_of(&name(3)).await.unwrap().length, Some(1234));
    assert_eq!(t.restarted().read(&name(3)).await.unwrap().unwrap().as_slice(), newer.as_slice());
    files.iter_mut().find(|f| f.0 == name(3)).unwrap().1 = newer;
    files.push(late);
    files.push(late_side);
    assert_all_readable(&t.store, &files).await;
    assert_eq!(t.store.log_count(KEY).await.unwrap(), 12);

    let r = t.store.pack_day(KEY, DAY1).unwrap();
    assert_eq!((r.days, r.files), (1, 3), "{r:?}");
    assert!(r.before_bytes > first_pack.len() as u64);
    assert_eq!(loose(&t.car()), Vec::<String>::new());
    assert_ne!(std::fs::read(t.car().join(PACKS).join(format!("{DAY1}.pack"))).unwrap(), first_pack);
    assert_all_readable(&t.store, &files).await;
    assert_all_readable(&t.restarted(), &files).await;
    assert_eq!(t.restarted().log_count(KEY).await.unwrap(), 12);
    // nothing to pack: the pack is not written again
    let stamp = std::fs::metadata(t.car().join(PACKS).join(format!("{DAY1}.pack"))).unwrap().modified().unwrap();
    assert_eq!(t.store.pack_day(KEY, DAY1).unwrap().files, 0);
    assert_eq!(std::fs::metadata(t.car().join(PACKS).join(format!("{DAY1}.pack"))).unwrap().modified().unwrap(), stamp);
}

#[tokio::test]
async fn a_bad_copy_that_is_only_in_a_pack_is_replaced() {
    let t = setup();
    let mut files = fill(&t).await;
    t.store.pack_finished(AFTER, &|| false);
    let store = t.restarted();
    let n = name(4);
    let bad = files.iter().find(|f| f.0 == n).unwrap().1.clone();
    let good = log_bytes(404, 9000);

    // in two chunks: until the last byte the packed copy is what the name holds
    let r =
        store.receive_replacing(&n, Bytes::copy_from_slice(&good[..5000]), 0, 9000, &sha(&good), true).await.unwrap();
    assert!(!r.complete);
    assert_eq!(store.read(&n).await.unwrap().unwrap().as_slice(), bad.as_slice());
    assert_eq!(store.state_of(&n).await.unwrap().state, State::Complete);
    let r = store
        .receive_replacing(&n, Bytes::copy_from_slice(&good[5000..]), 5000, 9000, &sha(&good), true)
        .await
        .unwrap();
    assert!(r.complete && r.replaced && !r.already);
    assert_eq!(store.read(&n).await.unwrap().unwrap().as_slice(), good.as_slice(), "the new copy wins over the pack");
    assert_eq!(store.hash(&n).await.unwrap(), (sha(&good), 9000));
    assert_eq!(store.quarantined(KEY).await, vec![n.clone()]);
    assert_eq!(store.read_quarantined(&n).await.unwrap().unwrap().as_slice(), bad.as_slice(), "taken out of the pack");
    assert_eq!(store.log_count(KEY).await.unwrap(), 11, "a replacement is not a new log");
    assert_eq!(loose(&t.car()), vec![n.clone()]);

    // the next pass puts the new copy into the pack in place of the bad one
    files.iter_mut().find(|f| f.0 == n).unwrap().1 = good.clone();
    let r = store.pack_finished(AFTER, &|| false);
    assert_eq!((r.days, r.files, r.failed), (1, 1, 0), "{r:?}");
    assert_eq!(loose(&t.car()), Vec::<String>::new());
    assert_all_readable(&store, &files).await;
    assert_all_readable(&t.restarted(), &files).await;
    assert_eq!(store.read_quarantined(&n).await.unwrap().unwrap().as_slice(), bad.as_slice());
    assert_eq!(store.drop_quarantined(&n).await, 1);
    // the same bytes again: already here
    let r = store.receive_replacing(&n, Bytes::copy_from_slice(&good), 0, 9000, &sha(&good), true).await.unwrap();
    assert!(r.already && !r.replaced);
}

#[tokio::test]
async fn a_pass_cut_short_anywhere_loses_nothing_and_the_next_one_finishes() {
    for stop in [PackStop::TempWritten, PackStop::Renamed, PackStop::Removed(0), PackStop::Removed(5)] {
        let t = setup();
        let files = fill(&t).await;
        t.store.pack_stop_at(Some(stop));
        let r = t.store.pack_finished(AFTER, &|| false);
        assert_eq!((r.failed, r.files), (2, 0), "{stop:?}: {r:?}");

        // what the crash left
        let left = loose(&t.car());
        let tmp = t.car().join(PACKS).join(format!("{DAY1}.pack.tmp"));
        let pack = t.car().join(PACKS).join(format!("{DAY1}.pack"));
        match stop {
            PackStop::TempWritten => {
                assert!(tmp.exists() && !pack.exists());
                assert_eq!(left.len(), files.len());
            }
            PackStop::Renamed | PackStop::Removed(0) => {
                assert!(!tmp.exists() && pack.exists());
                assert_eq!(left.len(), files.len());
            }
            PackStop::Removed(n) => {
                assert!(!tmp.exists() && pack.exists());
                // n files of each day were removed
                assert_eq!(left.len(), files.len() - 2 * n, "{left:?}");
            }
        }

        // a server started on that folder reads every file, and a pass finishes the work
        let again = t.restarted();
        assert_all_readable(&again, &files).await;
        assert_eq!(again.log_count(KEY).await.unwrap(), 11);
        let r = again.pack_finished(AFTER, &|| false);
        assert_eq!((r.days, r.failed, r.files), (2, 0, left.len()), "{stop:?}: {r:?}");
        assert_eq!(loose(&t.car()), Vec::<String>::new(), "{stop:?}");
        assert_eq!(pack_files(&t.car()), vec![format!("{DAY1}.pack"), format!("{DAY2}.pack")], "{stop:?}");
        assert_all_readable(&again, &files).await;
        assert_all_readable(&t.restarted(), &files).await;
        assert_eq!(t.restarted().log_count(KEY).await.unwrap(), 11);
    }
}

#[tokio::test]
async fn a_damaged_pack_is_an_error_and_never_costs_a_file() {
    let t = setup();
    let files = fill(&t).await;
    t.store.pack_finished(AFTER, &|| false);
    let pack = t.car().join(PACKS).join(format!("{DAY1}.pack"));
    let good = std::fs::read(&pack).unwrap();
    // a late log of that day, in the folder
    let late = (name(30), log_bytes(30, 4000));
    put(&t.store, &late.0, &late.1).await;

    for at in [good.len() / 2, good.len() - 1, 3, 40] {
        let mut bad = good.clone();
        bad[at] ^= 0x10;
        std::fs::write(&pack, &bad).unwrap();
        for store in [t.store.clone(), t.restarted()] {
            assert!(store.read(&name(1)).await.is_err(), "flipped at {at}: damaged, not missing");
            assert!(store.hash(&name(1)).await.is_err());
            assert!(store.state_of(&name(1)).await.is_err());
            assert_eq!(store.status(&[name(1)], Some(KEY)).await[0].1.state, State::Invalid);
            // a name never uploaded on that day: not "missing" either, so nothing is sent again over the damage
            assert!(store.state_of(&name(55)).await.is_err());
            let b = log_bytes(1, 10);
            assert!(store.receive(&name(1), Bytes::copy_from_slice(&b), 0, 10, &sha(&b)).await.is_err());
            // the file in the folder still reads, and the other day is untouched
            assert_eq!(store.read(&late.0).await.unwrap().unwrap().as_slice(), late.1.as_slice());
            assert_eq!(store.read(&name_on(DAY2, 1)).await.unwrap().unwrap().len(), 3517);
            // a pass: the day fails, the file stays in the folder, the pack is not written over
            assert!(matches!(store.pack_day(KEY, DAY1), Err(PackError::Damaged(_))), "flipped at {at}");
            let r = store.pack_finished(AFTER, &|| false);
            assert_eq!((r.failed, r.files, r.days), (1, 0, 0), "{r:?}");
            assert_eq!(loose(&t.car()), vec![late.0.clone()]);
            assert_eq!(std::fs::read(&pack).unwrap(), bad);
            assert!(!t.car().join(PACKS).join(format!("{DAY1}.pack.tmp")).exists());
        }
    }
    // a pack of another day put under this day's name doesn't open either (its name is sealed in)
    std::fs::copy(t.car().join(PACKS).join(format!("{DAY2}.pack")), &pack).unwrap();
    assert!(t.restarted().read(&name(1)).await.is_err());
    assert!(matches!(t.restarted().pack_day(KEY, DAY1), Err(PackError::Damaged(_))));
    assert_eq!(loose(&t.car()), vec![late.0.clone()]);

    // the pack put back: everything reads, and the late log is merged
    std::fs::write(&pack, &good).unwrap();
    let store = t.restarted();
    assert_all_readable(&store, &files).await;
    assert_eq!(store.pack_day(KEY, DAY1).unwrap().files, 1);
    assert_all_readable(&store, &files).await;
    assert_eq!(store.read(&late.0).await.unwrap().unwrap().as_slice(), late.1.as_slice());
}

#[tokio::test]
async fn a_file_that_does_not_open_is_left_in_the_folder_and_the_rest_is_packed() {
    let t = setup();
    let files = fill(&t).await;
    let broken = name(2);
    let path = t.car().join(&broken);
    let mut b = std::fs::read(&path).unwrap();
    let mid = b.len() / 2;
    b[mid] ^= 1;
    std::fs::write(&path, &b).unwrap();
    let r = t.store.pack_finished(AFTER, &|| false);
    assert_eq!((r.days, r.failed, r.files), (2, 0, files.len() - 1), "{r:?}");
    assert_eq!(loose(&t.car()), vec![broken.clone()]);
    assert_eq!(std::fs::read(&path).unwrap(), b, "left exactly as it was");
    assert!(t.store.read(&broken).await.is_err());
    let rest: Vec<_> = files.into_iter().filter(|f| f.0 != broken).collect();
    assert_all_readable(&t.restarted(), &rest).await;
    let r = t.store.pack_finished(AFTER, &|| false);
    assert_eq!((r.days, r.files), (0, 0), "nothing more to do: {r:?}");
}

#[tokio::test]
async fn a_day_larger_than_a_pack_may_be_keeps_the_rest_in_the_folder() {
    let t = setup();
    let mut files = Vec::new();
    // 5 logs of 7 MiB: four fit in a pack of 32 MiB; and one of 9 MiB, larger than a packed file may be
    for i in 0..5u64 {
        files.push((name(i), log_bytes(i, 7 << 20)));
    }
    files.push((name(9), log_bytes(9, 9 << 20)));
    for (n, b) in &files {
        put(&t.store, n, b).await;
    }
    let r = t.store.pack_day(KEY, DAY1).unwrap();
    assert_eq!((r.days, r.files), (1, 4), "{r:?}");
    assert_eq!(loose(&t.car()), vec![name(4), name(9)]);
    assert_all_readable(&t.restarted(), &files).await;
    let r = t.store.pack_day(KEY, DAY1).unwrap();
    assert_eq!((r.days, r.files), (0, 0), "{r:?}");
    assert_eq!(t.store.log_count(KEY).await.unwrap(), 6);
}

// Through the API, with the judge: a log that failed its check, packed, then sent again.

const IP: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 20));

struct Api {
    _dir: tempfile::TempDir,
    up: Arc<Uploads>,
    token: String,
}

async fn api() -> Api {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let (v, _) = vault::Vault::create(&data, &dir.path().join("keys"), vault::Kdf::FAST_FOR_TESTS).unwrap();
    let v = Arc::new(v);
    let tokens = Tokens::open(data.join("tokens.json"), v.clone()).await.unwrap();
    let (token, _) = tokens.create("Model Y").await.unwrap();
    let store = Store::with_limits(data.join("can"), v, 0, 200_000, |_| u64::MAX);
    let up = Uploads::new(store.clone(), tokens, Judge::new(store).wait_ms(60_000));
    Api { _dir: dir, up, token }
}

async fn call(t: &Api, method: &str, path: &str, headers: &[(&str, String)], body: Vec<u8>) -> (StatusCode, Value) {
    let mut b = Request::builder().method(method).uri(path).header("authorization", format!("Bearer {}", t.token));
    for (k, v) in headers {
        b = b.header(*k, v);
    }
    let mut req = b.body(Body::from(body)).unwrap();
    req.extensions_mut().insert(Conn { tls: true, ip: IP });
    let res = router(t.up.clone()).oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

async fn upload(t: &Api, name: &str, bytes: &[u8]) -> (StatusCode, Value) {
    let h =
        [("upload-offset", "0".to_string()), ("upload-length", bytes.len().to_string()), ("upload-sha256", sha(bytes))];
    call(t, "POST", &format!("/api/can/logs/{name}"), &h, bytes.to_vec()).await
}

async fn status(t: &Api, names: &[String]) -> Value {
    let h = [("content-type", "application/json".to_string())];
    let (s, b) =
        call(t, "POST", "/api/can/logs/status", &h, serde_json::to_vec(&json!({ "names": names })).unwrap()).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    b["files"].clone()
}

fn signed_sidecar(key: &SigningKey, name: &str, signed: &[u8], index: u64, prev: (&str, &str)) -> Vec<u8> {
    let b64 = base64::engine::general_purpose::STANDARD;
    let der = key.verifying_key().to_public_key_der().unwrap().as_bytes().to_vec();
    let sha = sha(signed);
    let msg = trust::signed_message(KEY, name, &sha, signed.len() as u64, index, prev.0, prev.1);
    let sig: Signature = key.sign(msg.as_bytes());
    serde_json::to_vec(&json!({ "format": "canbridge-columns-xz/2", "integrity": {
        "signed": true, "version": trust::VERSION, "algorithm": trust::ALGORITHM, "public_key": b64.encode(&der),
        "key_fingerprint": trust::fingerprint(&der), "vehicle_key": KEY, "sha256": sha, "length": signed.len(),
        "index": index, "prev_name": prev.0, "prev_sha256": prev.1, "signature": b64.encode(sig.to_der().as_bytes()) } }))
    .unwrap()
}

#[tokio::test]
async fn through_the_api_a_packed_log_keeps_its_state_and_check_and_a_failed_one_is_replaced() {
    let t = api().await;
    let key = SigningKey::from_slice(&Sha256::digest(b"made-up bridge key for packs")).unwrap();
    let log = |i: u64| log_bytes(i, 2000);
    let mut bad = log(1);
    bad[3] ^= 1;
    for i in 0..3u64 {
        let n = name(i);
        let prev =
            if i == 0 { ("-".to_string(), trust::GENESIS_SHA.to_string()) } else { (name(i - 1), sha(&log(i - 1))) };
        let s = signed_sidecar(&key, &n, &log(i), i, (&prev.0, &prev.1));
        assert_eq!(upload(&t, &side(&n), &s).await.0, StatusCode::CREATED);
        let stored = if i == 1 { bad.clone() } else { log(i) };
        assert_eq!(upload(&t, &n, &stored).await.0, StatusCode::CREATED);
    }
    let names = [name(0), name(1), name(2), side(&name(1))];
    let before = status(&t, &names).await;
    assert_eq!(before[name(0)]["check"], json!("verified"), "{before}");
    assert_eq!(before[name(1)]["check_reason"], json!("mismatch"), "{before}");

    let r = t.up.store.pack_day(KEY, DAY1).unwrap();
    assert_eq!((r.days, r.files), (1, 6));
    assert_eq!(status(&t, &names).await, before, "the same states and checks from the pack");
    // the same bytes again are taken as before; a verified log is never overwritten
    let (s, b) = upload(&t, &name(0), &log(0)).await;
    assert_eq!((s, b["complete"].as_bool()), (StatusCode::CREATED, Some(true)), "{b}");
    let (s, b) = upload(&t, &name(0), &bad).await;
    assert_eq!((s, b["code"].as_str()), (StatusCode::CONFLICT, Some("exists_different")), "{b}");

    // the failed log sent again, right this time
    assert_eq!(upload(&t, &name(1), &log(1)).await.0, StatusCode::CREATED);
    let f = status(&t, &names).await;
    for i in 0..3 {
        assert_eq!(f[name(i)]["check"], json!("verified"), "{f}");
    }
    assert_eq!(t.up.store.read(&name(1)).await.unwrap().unwrap().as_slice(), log(1).as_slice());
    assert!(t.up.store.quarantined(KEY).await.is_empty(), "the bad copy goes once the new one verifies");
    assert_eq!(t.up.store.log_count(KEY).await.unwrap(), 3);

    let r = t.up.store.pack_day(KEY, DAY1).unwrap();
    assert_eq!((r.days, r.files), (1, 1));
    assert_eq!(status(&t, &names).await, f);
    assert_eq!(t.up.store.read(&name(1)).await.unwrap().unwrap().as_slice(), log(1).as_slice());
    // judging every log again reads them from the pack
    t.up.judge.rejudge_all(KEY).await.unwrap();
    assert_eq!(status(&t, &names).await, f);
}
