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
