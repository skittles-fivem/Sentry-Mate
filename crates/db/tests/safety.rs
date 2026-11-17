//! Nothing in the clear (temporary files and backups included), crashes at every step leave the old data or the
//! new, and damaged files or files of another version are reported with their name, never read as empty.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::*;
use db::{Crash, DAY_MS, Error, Options, Query, StateRow};
use model::{Drive, Sample, Source, State};
use std::io::Write;

const H: u64 = 3_600_000;

fn quiet() -> Options {
    // nothing folds by itself: tests decide when
    Options { fold_min_bytes: 1 << 30, ..Options::default() }
}

fn damaged_in(e: Error, name: &str) {
    match e {
        Error::Damaged { file, .. } => assert!(file.ends_with(name), "{file} is not {name}"),
        e => panic!("expected damage in {name}, got {e}"),
    }
}

fn flip(path: &std::path::Path, at: usize) {
    let mut b = std::fs::read(path).unwrap();
    let i = at.min(b.len() - 1);
    b[i] ^= 0x40;
    std::fs::write(path, b).unwrap();
}

#[test]
fn nothing_is_stored_in_the_clear() {
    let t = test_db(Options { fold_min_bytes: 1, ..Options::default() });
    let markers: Vec<String> = [
        "Marker-Car-QZXV",
        "markerkeyqzxv0123",
        "Marker Street 42 QZXV",
        "2099.99.qzxv",
        "Marker Place QZXV",
        "marker.setting.qzxv",
        "marker-value-qzxv",
        "marker-ns-qzxv",
        "marker-kv-key-qzxv",
        "marker-kv-value-qzxv",
        "start_address",
        "rules_version",
        "12.3456789",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let car = t.db.create_car(new_car(&markers[0], &[&markers[1]])).unwrap().id;
    let mut d = drive(car, 1, DAY0, 30, 12.5, Source::Can);
    d.start_address = Some(markers[2].clone());
    t.db.upsert(car, &[d]).unwrap();
    t.db.upsert(car, &[charge(car, 1, DAY0 + H, 60, 20.0, Source::Teslamate)]).unwrap();
    t.db.upsert(car, &[state(car, 1, DAY0, DAY0 + H, State::Online, Source::Can)]).unwrap();
    let u = model::Update { car, start_ms: DAY0, end_ms: None, version: markers[3].clone(), source: Source::Can };
    t.db.upsert(car, &[db::UpdateRow { id: 1, update: u }]).unwrap();
    let mut pts = samples(DAY0, 600);
    for p in &mut pts {
        p.lat = Some(12.345_678_9);
    }
    t.db.write_points(car, Source::Can, DAY0, DAY0 + DAY_MS, &pts).unwrap();
    t.db.write_points(car, Source::Can, DAY0 + DAY_MS, DAY0 + DAY_MS + 60_000, &samples(DAY0 + DAY_MS, 60)).unwrap();
    t.db.set_now(model::Now { car, at_ms: DAY0, sample: pts[0].clone(), ..Default::default() }).unwrap();
    let place = model::Place {
        id: 0,
        name: markers[4].clone(),
        kind: model::PlaceKind::Home,
        lat: 12.345_678_9,
        lon: 1.0,
        radius_m: 200.0,
        price: None,
    };
    t.db.add_place(place).unwrap();
    t.db.set_setting(&markers[5], serde_json::json!(markers[6])).unwrap();
    t.db.kv().put(&markers[7], &markers[8], markers[9].as_bytes()).unwrap();
    // a crash leaves a temporary file behind: it must be sealed too
    t.db.upsert(car, &[drive(car, 2, DAY0 + 2 * H, 30, 1.0, Source::Can)]).unwrap();
    t.db.crash_at(Some(Crash::BeforeRename));
    assert!(t.db.settle(car, DAY0 + 2 * DAY_MS).is_err());
    t.db.backup_to_file(&t.data.join("backup.smdb")).unwrap();
    let files = all_files(&t.data);
    assert!(files.iter().any(|f| f.to_string_lossy().ends_with(".tmp")), "a temporary file was left: {files:?}");
    let mut needles: Vec<Vec<u8>> = markers.iter().map(|m| m.as_bytes().to_vec()).collect();
    needles.push(12.345_678_9f64.to_le_bytes().to_vec());
    needles.push(12.345_678_9f64.to_be_bytes().to_vec());
    for f in &files {
        let b = std::fs::read(f).unwrap();
        for n in &needles {
            assert!(!b.windows(n.len()).any(|w| w == &n[..]), "{} holds {:?} in the clear", f.display(), n);
        }
        if f.starts_with(t.data.join("db")) || f.ends_with("backup.smdb") {
            assert!(b.starts_with(b"SMENC") || b.starts_with(b"SMPRT"), "{} is not sealed", f.display());
        }
    }
}

#[test]
fn a_crash_before_a_rename_leaves_the_old_data() {
    let mut t = test_db(quiet());
    let car = t.db.create_car(new_car("Model Y", &[])).unwrap().id;
    let drives: Vec<Drive> = (0..20).map(|i| drive(car, i, DAY0 + i * H, 30, 2.0, Source::Can)).collect();
    t.db.upsert(car, &drives).unwrap();
    let dir = t.data.join("db/cars").join(car.to_string());
    // folding the journal into a snapshot stops before the rename
    t.db.crash_at(Some(Crash::BeforeRename));
    assert!(matches!(t.db.settle(car, 0), Err(Error::Crashed(Crash::BeforeRename))));
    let tmp = dir.join("drives.snap.tmp");
    assert!(std::fs::read(&tmp).unwrap().starts_with(b"SMENC"));
    t.reopen(quiet());
    assert_eq!(t.db.list::<Drive>(car, &Query::default()).unwrap().items.len(), 20);
    assert!(!tmp.exists(), "cleaned up when the table was next read");
    // a whole day being replaced
    let day1 = samples(DAY0, 100);
    t.db.write_points(car, Source::Can, DAY0, DAY0 + DAY_MS, &day1).unwrap();
    t.db.crash_at(Some(Crash::BeforeRename));
    assert!(t.db.write_points(car, Source::Can, DAY0, DAY0 + DAY_MS, &samples(DAY0 + 5, 7)).is_err());
    t.reopen(quiet());
    assert_eq!(t.db.read_points::<Sample>(car, Source::Can, 0, u64::MAX).unwrap(), day1);
    // a key/value
    t.db.kv().put("ns", "k", b"old").unwrap();
    t.db.crash_at(Some(Crash::BeforeRename));
    assert!(t.db.kv().put("ns", "k", b"new").is_err());
    assert_eq!(t.db.kv().get("ns", "k").unwrap().as_deref(), Some(&b"old"[..]));
    t.db.kv().put("ns", "k", b"new").unwrap();
    assert_eq!(t.db.kv().get("ns", "k").unwrap().as_deref(), Some(&b"new"[..]));
}

#[test]
fn a_crash_while_folding_keeps_everything() {
    let mut t = test_db(quiet());
    let car = t.db.create_car(new_car("Model Y", &[])).unwrap().id;
    let drives: Vec<Drive> = (0..20).map(|i| drive(car, i, DAY0 + i * H, 30, 2.0, Source::Can)).collect();
    t.db.upsert(car, &drives[..10]).unwrap();
    t.db.settle(car, 0).unwrap();
    t.db.upsert(car, &drives[10..]).unwrap();
    t.db.delete::<Drive>(car, &[3]).unwrap();
    let want = t.db.list::<Drive>(car, &Query::default()).unwrap().items;
    assert_eq!(want.len(), 19);
    // the new snapshot is in place, the old journal not yet removed
    t.db.crash_at(Some(Crash::BeforeJournalRemoved));
    assert!(matches!(t.db.settle(car, 0), Err(Error::Crashed(_))));
    let dir = t.data.join("db/cars").join(car.to_string());
    assert!(dir.join("drives.snap").exists() && dir.join("drives.log").exists());
    t.reopen(quiet());
    assert_eq!(t.db.list::<Drive>(car, &Query::default()).unwrap().items, want);
    assert!(!dir.join("drives.log").exists(), "the stale journal is recognised and removed");
    // the same for a segment day folded while being written
    let pts = samples(DAY0 + 1000, 600);
    for w in pts.chunks(60) {
        t.db.write_points(car, Source::Can, w[0].t_ms, w[0].t_ms + 60_000, w).unwrap();
    }
    t.db.crash_at(Some(Crash::BeforeJournalRemoved));
    assert!(t.db.settle(car, DAY0 + DAY_MS).is_err());
    t.reopen(quiet());
    assert_eq!(t.db.read_points::<Sample>(car, Source::Can, 0, u64::MAX).unwrap(), pts);
    // the crash came after the cold snapshot was in place: nothing is left to pack
    assert!(!t.data.join("db/cars").join(car.to_string()).join("seg/samples.can/2026-11-16.log").exists());
    assert_eq!(t.db.settle(car, DAY0 + DAY_MS).unwrap().days, 0);
    assert_eq!(t.db.read_points::<Sample>(car, Source::Can, 0, u64::MAX).unwrap(), pts);
}

#[test]
fn a_half_written_entry_is_dropped_and_the_rest_kept() {
    let mut t = test_db(quiet());
    let car = t.db.create_car(new_car("Model Y", &[])).unwrap().id;
    for i in 0..5 {
        t.db.upsert(car, &[drive(car, i, DAY0 + i * H, 30, 2.0, Source::Can)]).unwrap();
    }
    let log = t.data.join("db/cars").join(car.to_string()).join("drives.log");
    // a crash while an entry was being written: its record is torn
    let mut f = std::fs::OpenOptions::new().append(true).open(&log).unwrap();
    f.write_all(&[0, 0, 1, 0, 7, 7, 7, 7, 7]).unwrap();
    drop(f);
    t.reopen(quiet());
    assert_eq!(t.db.count::<Drive>(car).unwrap(), 5);
    t.db.upsert(car, &[drive(car, 9, DAY0 + 9 * H, 30, 2.0, Source::Can)]).unwrap();
    t.reopen(quiet());
    assert_eq!(t.db.count::<Drive>(car).unwrap(), 6);
}

#[test]
fn reading_a_day_leaves_its_journal_as_it_is_and_the_next_write_carries_on() {
    let t = test_db(quiet());
    let car = t.db.create_car(new_car("Model Y", &[])).unwrap().id;
    let pts = samples(DAY0 + 1000, 180);
    for w in pts[..120].chunks(60) {
        t.db.write_points(car, Source::Can, w[0].t_ms, w[0].t_ms + 60_000, w).unwrap();
    }
    let log = t.data.join("db/cars").join(car.to_string()).join("seg/samples.can/2026-11-16.log");
    // a crash while an entry was being written: its record is torn
    let mut f = std::fs::OpenOptions::new().append(true).open(&log).unwrap();
    f.write_all(&[0, 0, 1, 0, 7, 7, 7, 7, 7]).unwrap();
    drop(f);
    let before = std::fs::read(&log).unwrap();
    // readers see the whole entries and leave the file alone (opened read-only: never cut or changed)
    assert_eq!(t.db.read_points::<Sample>(car, Source::Can, 0, u64::MAX).unwrap(), pts[..120]);
    assert_eq!(t.db.read_fields::<Sample>(car, Source::Can, 0, u64::MAX, &["soc"]).unwrap().t_ms.len(), 120);
    assert_eq!(std::fs::read(&log).unwrap(), before);
    // the next write cuts the torn record off first, then adds its own
    let w = &pts[120..];
    t.db.write_points(car, Source::Can, w[0].t_ms, w[0].t_ms + 60_000, w).unwrap();
    assert!(std::fs::read(&log).unwrap().starts_with(&before[..before.len() - 9]));
    assert_eq!(t.db.read_points::<Sample>(car, Source::Can, 0, u64::MAX).unwrap(), pts);
}

#[test]
fn damaged_files_are_reported_not_emptied() {
    let mut t = test_db(quiet());
    let car = t.db.create_car(new_car("Model Y", &[])).unwrap().id;
    let other = t.db.create_car(new_car("Model 3", &[])).unwrap().id;
    let drives: Vec<Drive> = (0..200).map(|i| drive(car, i, DAY0 + i * H, 30, 2.0, Source::Can)).collect();
    t.db.upsert(car, &drives[..100]).unwrap();
    t.db.settle(car, 0).unwrap();
    for d in &drives[100..110] {
        t.db.upsert(car, std::slice::from_ref(d)).unwrap();
    }
    t.db.write_points(car, Source::Can, DAY0, DAY0 + DAY_MS, &samples(DAY0, 3000)).unwrap();
    let dir = t.data.join("db/cars").join(car.to_string());
    let (snap, log) = (dir.join("drives.snap"), dir.join("drives.log"));
    let day = dir.join("seg/samples.can/2026-11-16.snap");
    let save = |p: &std::path::Path| std::fs::read(p).unwrap();
    let (snap0, log0, day0) = (save(&snap), save(&log), save(&day));
    let list = |t: &TestDb| t.try_reopen(quiet()).unwrap().list::<Drive>(car, &Query::default());

    // a changed byte in the snapshot
    flip(&snap, 2000);
    damaged_in(list(&t).unwrap_err(), "drives.snap");
    std::fs::write(&snap, &snap0).unwrap();
    // cut short
    std::fs::write(&snap, &snap0[..snap0.len() - 10]).unwrap();
    damaged_in(list(&t).unwrap_err(), "drives.snap");
    std::fs::write(&snap, &snap0).unwrap();
    // a changed byte in a record in the middle of the journal
    flip(&log, log0.len() / 2);
    damaged_in(list(&t).unwrap_err(), "drives.log");
    // a record's length broken in the middle (the vault would cut everything after it)
    let mut b = log0.clone();
    // the second record's length: after the header (22 bytes) and the first record
    let second = 22 + 4 + u32::from_be_bytes([b[22], b[23], b[24], b[25]]) as usize;
    b[second] = 0xEE;
    std::fs::write(&log, &b).unwrap();
    damaged_in(list(&t).unwrap_err(), "drives.log");
    std::fs::write(&log, &log0).unwrap();
    // the snapshot lost while its journal follows it
    std::fs::remove_file(&snap).unwrap();
    damaged_in(list(&t).unwrap_err(), "drives.log");
    std::fs::write(&snap, &snap0).unwrap();
    // a file moved under another car's name doesn't open there
    let other_dir = t.data.join("db/cars").join(other.to_string());
    std::fs::create_dir_all(&other_dir).unwrap();
    std::fs::write(other_dir.join("drives.snap"), &snap0).unwrap();
    let e = t.try_reopen(quiet()).unwrap().list::<Drive>(other, &Query::default()).unwrap_err();
    damaged_in(e, "drives.snap");
    std::fs::remove_file(other_dir.join("drives.snap")).unwrap();
    // a segment day
    flip(&day, day0.len() / 2);
    let db = t.try_reopen(quiet()).unwrap();
    damaged_in(db.read_points::<Sample>(car, Source::Can, 0, u64::MAX).unwrap_err(), "2026-11-16.snap");
    damaged_in(db.read_fields::<Sample>(car, Source::Can, 0, u64::MAX, &["soc"]).unwrap_err(), "2026-11-16.snap");
    std::fs::write(&day, &day0).unwrap();
    // the fleet: the db doesn't open
    t.db.settle(car, 0).unwrap();
    let fleet = t.data.join("db/fleet.log");
    let fleet0 = save(&fleet);
    flip(&fleet, fleet0.len() / 2);
    damaged_in(t.try_reopen(quiet()).err().unwrap(), "fleet.log");
    std::fs::write(&fleet, &fleet0).unwrap();
    // all put back: every row is there
    t.reopen(quiet());
    assert_eq!(t.db.count::<Drive>(car).unwrap(), 110);
}

#[test]
fn files_of_another_version_are_reported() {
    let t = test_db(quiet());
    let car = t.db.create_car(new_car("Model Y", &[])).unwrap().id;
    let dir = t.data.join("db/cars").join(car.to_string());
    std::fs::create_dir_all(&dir).unwrap();
    let mut plain = b"SMDB\x01".to_vec();
    plain.extend_from_slice(&99u16.to_be_bytes());
    plain.extend_from_slice(&[0; 9]);
    let label = format!("db/cars/{car}/drives.snap");
    std::fs::write(dir.join("drives.snap"), t.vault.seal(&label, &plain).unwrap()).unwrap();
    match t.db.list::<Drive>(car, &Query::default()) {
        Err(Error::Version { file, found: 99, supported }) => {
            assert_eq!(file, label);
            assert_eq!(supported, db::FORMAT_VERSION);
        }
        r => panic!("expected a version error, got {r:?}"),
    }
    // a journal from a newer version
    let path = dir.join("states.log");
    let mut rec = t.vault.records(&path, &format!("db/cars/{car}/states.log")).unwrap();
    let mut head = vec![3u8];
    head.extend_from_slice(b"SMDB\x02");
    head.extend_from_slice(&7u16.to_be_bytes());
    head.extend_from_slice(&0u64.to_be_bytes());
    rec.append(&head).unwrap();
    drop(rec);
    assert!(matches!(t.db.list::<StateRow>(car, &Query::default()), Err(Error::Version { found: 7, .. })));
    // a segment day
    let seg = dir.join("seg/samples.can");
    std::fs::create_dir_all(&seg).unwrap();
    let mut plain = b"SMDB\x03".to_vec();
    plain.extend_from_slice(&2u16.to_be_bytes());
    plain.extend_from_slice(&[0; 9]);
    let label = format!("db/cars/{car}/seg/samples.can/2026-11-16.snap");
    std::fs::write(seg.join("2026-11-16.snap"), t.vault.seal(&label, &plain).unwrap()).unwrap();
    assert!(matches!(
        t.db.read_points::<Sample>(car, Source::Can, DAY0, DAY0 + DAY_MS),
        Err(Error::Version { found: 2, .. })
    ));
}

#[test]
fn a_lost_table_file_is_reported_not_read_as_empty() {
    let mut t = test_db(quiet());
    let car = t.db.create_car(new_car("Model Y", &[])).unwrap().id;
    let drives: Vec<Drive> = (0..10).map(|i| drive(car, i, DAY0 + i * H, 30, 2.0, Source::Can)).collect();
    t.db.upsert(car, &drives).unwrap();
    t.db.upsert(car, &[state(car, 1, DAY0, DAY0 + H, State::Online, Source::Can)]).unwrap();
    // folded: the snapshot is the only file the drives have
    t.db.settle(car, 0).unwrap();
    let dir = t.data.join("db/cars").join(car.to_string());
    assert!(!dir.join("drives.log").exists());
    std::fs::remove_file(dir.join("drives.snap")).unwrap();
    t.reopen(quiet());
    // it used to read as no drives, and the summary was "mended" to match, hiding the loss for good
    damaged_in(t.db.list::<Drive>(car, &Query::default()).unwrap_err(), "drives.snap");
    damaged_in(t.db.upsert(car, &[drive(car, 50, DAY0, 1, 1.0, Source::Can)]).unwrap_err(), "drives.snap");
    assert_eq!(t.db.entry(car).unwrap().unwrap().summary.tally.can.drives, 10, "the summary still says so");
    // the car's other tables are fine, and a table never written is empty, not lost
    assert_eq!(t.db.count::<StateRow>(car).unwrap(), 1);
    assert_eq!(t.db.count::<model::Charge>(car).unwrap(), 0);
    // emptying a table is not a loss
    let other = t.db.create_car(new_car("Model 3", &[])).unwrap().id;
    t.db.upsert(other, &[drive(other, 1, DAY0, 30, 2.0, Source::Can)]).unwrap();
    t.db.delete::<Drive>(other, &[1]).unwrap();
    t.db.settle(other, 0).unwrap();
    t.reopen(quiet());
    assert_eq!(t.db.count::<Drive>(other).unwrap(), 0);
}

#[test]
fn a_lost_fleet_is_reported_and_no_car_inherits_another_cars_folder() {
    let mut t = test_db(quiet());
    let car = t.db.create_car(new_car("Model Y", &[])).unwrap().id;
    t.db.upsert(car, &[drive(car, 1, DAY0, 30, 2.0, Source::Can)]).unwrap();
    t.db.settle(car, 0).unwrap();
    let fleet = t.data.join("db/fleet.log");
    let saved = std::fs::read(&fleet).unwrap();
    std::fs::remove_file(&fleet).unwrap();
    // opened as empty, the next car would have been given id 1, and car 1's drives with it
    damaged_in(t.try_reopen(quiet()).err().unwrap(), "fleet.snap");
    std::fs::write(&fleet, &saved).unwrap();
    t.reopen(quiet());
    assert_eq!(t.db.count::<Drive>(car).unwrap(), 1);
    // a folder already there for a new car's id is refused, whatever left it
    std::fs::create_dir_all(t.data.join("db/cars").join((car + 1).to_string())).unwrap();
    assert!(matches!(t.db.create_car(new_car("Model 3", &[])), Err(Error::Damaged { .. })));
}

#[cfg(unix)]
#[test]
fn every_file_is_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let t = test_db(quiet());
    let car = t.db.create_car(new_car("Model Y", &[])).unwrap().id;
    t.db.upsert(car, &[drive(car, 1, DAY0, 30, 2.0, Source::Can)]).unwrap();
    t.db.write_points(car, Source::Can, DAY0, DAY0 + 60_000, &samples(DAY0, 60)).unwrap();
    t.db.write_points(car, Source::Can, DAY0 + DAY_MS, DAY0 + 2 * DAY_MS, &samples(DAY0 + DAY_MS, 60)).unwrap();
    t.db.kv().put("ns", "k", b"v").unwrap();
    let files = all_files(&t.data.join("db"));
    assert!(files.iter().any(|f| f.extension().is_some_and(|x| x == "log")));
    for f in files {
        let mode = std::fs::metadata(&f).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "{} is {mode:o}", f.display());
    }
}
