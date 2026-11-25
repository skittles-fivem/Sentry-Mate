#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! An export tells how far it got (counting, collecting, checking) and stops when cancelled.

mod common;

use common::{APP, fill, server};
use sha2::{Digest, Sha256};
use std::time::Duration;

#[tokio::test(flavor = "multi_thread")]
async fn an_export_counts_collects_and_checks_with_its_progress() {
    let old = server();
    fill(&old).await;
    let gate = gate::Gate::open(&old.data).unwrap();
    let watch = moving::Watch::system();
    assert_eq!(watch.seen().phase, moving::Phase::Counting);
    let snap =
        moving::Snapshot::make_watched(&old.data, &old.vault, &gate, APP, Duration::from_secs(5), &watch).unwrap();
    let seen = watch.seen();
    assert_eq!(seen.phase, moving::Phase::Done);
    assert_eq!((seen.files_total, seen.bytes_total), (snap.header().files, snap.header().bytes), "{seen:?}");
    assert!(seen.progress_ms >= seen.started_ms && !seen.cancelled);
    // the checksum is that of the bytes a download gets
    let sha = snap.sha256(&old.vault, &watch).unwrap();
    let seen = watch.seen();
    assert_eq!(seen.phase, moving::Phase::Checking);
    assert_eq!((seen.bytes_done, seen.bytes_total), (snap.size(), snap.size()));
    let mut whole = Vec::new();
    snap.write_to(&old.vault, &mut whole).unwrap();
    assert_eq!(sha, hex::encode(Sha256::digest(&whole)));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cancelled_export_stops_and_leaves_no_snapshot() {
    let old = server();
    fill(&old).await;
    let gate = gate::Gate::open(&old.data).unwrap();
    let watch = moving::Watch::system();
    watch.cancel();
    let r = moving::Snapshot::make_watched(&old.data, &old.vault, &gate, APP, Duration::from_secs(5), &watch);
    assert!(matches!(r, Err(moving::Error::Cancelled)), "{:?}", r.err());
    let left = std::fs::read_dir(old.data.join(moving::export::SNAPSHOTS)).map(|d| d.count()).unwrap_or(0);
    assert_eq!(left, 0, "nothing left behind");
    // the writes go on: the pause was let go
    drop(gate::Gate::open(&old.data).unwrap().pause(Duration::from_secs(1)).unwrap());
    // a checksum stops too
    let w2 = moving::Watch::system();
    let snap = moving::Snapshot::make_watched(&old.data, &old.vault, &gate, APP, Duration::from_secs(5), &w2).unwrap();
    w2.cancel();
    assert!(matches!(snap.sha256(&old.vault, &w2), Err(moving::Error::Cancelled)));
}

fn snapshot_dirs(data: &std::path::Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(data.join(moving::export::SNAPSHOTS))
        .map(|d| d.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    v.sort();
    v
}

#[tokio::test(flavor = "multi_thread")]
async fn a_kept_snapshot_reopens_to_the_same_bytes() {
    let old = server();
    fill(&old).await;
    let gate = gate::Gate::open(&old.data).unwrap();
    let snap = moving::Snapshot::make(&old.data, &old.vault, &gate, APP, Duration::from_secs(5)).unwrap();
    snap.keep_on_drop(true).unwrap();
    let hid = snap.header().id.clone();
    let mut before = Vec::new();
    snap.write_to(&old.vault, &mut before).unwrap();
    let (header, name, skips) = (snap.header().clone(), snap.file_name(), snap.skips().clone());
    drop(snap);
    assert!(snapshot_dirs(&old.data).contains(&hid), "a kept snapshot stays");
    // the web side's state file stands beside the snapshots; their upkeep leaves it alone
    std::fs::write(old.data.join(moving::export::SNAPSHOTS).join("slot.sealed"), b"state").unwrap();
    let again = moving::Snapshot::reopen(&old.data, &old.vault, &hid).unwrap();
    assert_eq!(moving::export::remove_unused_snapshots(&old.data), 0, "the one open holds its lock");
    assert!(snapshot_dirs(&old.data).contains(&"slot.sealed".to_string()));
    assert_eq!((again.header().id.clone(), again.size(), again.file_name()), (hid.clone(), header.size, name));
    assert_eq!(again.skips(), &skips);
    let mut after = Vec::new();
    again.write_to(&old.vault, &mut after).unwrap();
    assert_eq!(after, before, "the same bytes");
    assert_eq!(hex::encode(Sha256::digest(&after)), again.sha256(&old.vault, &moving::Watch::system()).unwrap());
    // not kept again: dropped, it goes
    drop(again);
    assert!(!snapshot_dirs(&old.data).contains(&hid));
}

#[tokio::test(flavor = "multi_thread")]
async fn reopen_refuses_a_held_or_unfinished_snapshot() {
    let old = server();
    fill(&old).await;
    let gate = gate::Gate::open(&old.data).unwrap();
    let snap = moving::Snapshot::make(&old.data, &old.vault, &gate, APP, Duration::from_secs(5)).unwrap();
    snap.keep_on_drop(true).unwrap();
    let hid = snap.header().id.clone();
    // held by the one that made it
    assert!(moving::Snapshot::reopen(&old.data, &old.vault, &hid).is_err());
    drop(snap);
    // not a snapshot's name, and none of that name
    assert!(moving::Snapshot::reopen(&old.data, &old.vault, "../data").is_err());
    assert!(moving::Snapshot::reopen(&old.data, &old.vault, &"0".repeat(32)).is_err());
    // cut short: no plan (it is written last)
    let plan = old.data.join(moving::export::SNAPSHOTS).join(&hid).join("plan.sealed");
    let kept = std::fs::read(&plan).unwrap();
    std::fs::remove_file(&plan).unwrap();
    assert!(moving::Snapshot::reopen(&old.data, &old.vault, &hid).is_err());
    // a plan that doesn't open (changed)
    let mut bad = kept.clone();
    let last = bad.len() - 1;
    bad[last] ^= 1;
    std::fs::write(&plan, &bad).unwrap();
    assert!(moving::Snapshot::reopen(&old.data, &old.vault, &hid).is_err());
    // whole again: it opens
    std::fs::write(&plan, &kept).unwrap();
    assert!(moving::Snapshot::reopen(&old.data, &old.vault, &hid).is_ok());
}

#[tokio::test(flavor = "multi_thread")]
async fn the_start_upkeep_leaves_a_kept_snapshot_and_recovery_removes_the_unwanted_ones() {
    let old = server();
    fill(&old).await;
    let gate = gate::Gate::open(&old.data).unwrap();
    let snap = moving::Snapshot::make(&old.data, &old.vault, &gate, APP, Duration::from_secs(5)).unwrap();
    snap.keep_on_drop(true).unwrap();
    let hid = snap.header().id.clone();
    let other = moving::Snapshot::make(&old.data, &old.vault, &gate, APP, Duration::from_secs(5)).unwrap();
    other.keep_on_drop(true).unwrap();
    let other_hid = other.header().id.clone();
    // the process ends: no lock is held any more, what is kept stays
    drop(snap);
    drop(other);
    // the program's start, before the vault is open (a stale temporary file of the state is not a snapshot)
    std::fs::write(old.data.join(moving::export::SNAPSHOTS).join("slot.7-ab.tmp"), b"x").unwrap();
    moving::import::tidy(&old.data, &old.data.join("keys-not-there"));
    let names = snapshot_dirs(&old.data);
    assert!(names.contains(&hid) && names.contains(&other_hid), "{names:?}");
    assert!(names.contains(&"slot.7-ab.tmp".to_string()), "{names:?}");
    // the web side's recovery keeps the one its state names; the other goes
    assert_eq!(moving::export::remove_stale_kept(&old.data, &[&hid]), 1);
    let names = snapshot_dirs(&old.data);
    assert!(names.contains(&hid) && !names.contains(&other_hid), "{names:?}");
    assert!(moving::Snapshot::reopen(&old.data, &old.vault, &hid).is_ok());
}
