//! A damaged table is read as far as it reads, put aside whole (never deleted) and written again with what was read.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::*;
use db::{Error, Options, Query, Table};
use model::{Drive, Source};
use vault::stream::{CHUNK, HEADER, TAG};

const H: u64 = 3_600_000;

fn quiet() -> Options {
    Options { fold_min_bytes: 1 << 30, ..Options::default() }
}

/// Rows whose values do not compress away, so the snapshot spans several 64 KiB chunks.
fn bulk(car: u32, n: u64, rng: &mut Rng) -> Vec<Drive> {
    (0..n)
        .map(|i| {
            let mut d = drive(car, i + 1, DAY0 + i * H, 30, 2.0, Source::Live);
            d.distance_km = Some(rng.f64() * 100.0);
            d.start_odometer_km = Some(rng.f64() * 100_000.0);
            d.end_odometer_km = Some(rng.f64() * 100_000.0);
            d.energy_used_kwh = Some(rng.f64() * 30.0);
            d.speed_max_kmh = Some(rng.f64() * 160.0);
            d.power_max_kw = Some(rng.f64() * 200.0);
            d.power_min_kw = Some(-rng.f64() * 80.0);
            d.outside_temp_avg_c = Some(rng.f64() * 30.0);
            d.inside_temp_avg_c = Some(rng.f64() * 30.0);
            d.start_soc = Some(rng.f64() * 100.0);
            d.end_soc = Some(rng.f64() * 100.0);
            d.start_address = Some(format!("Made-up Street {} {:x}", rng.u64() % 500, rng.u64()));
            d.end_address = Some(format!("Made-up Lane {} {:x}", rng.u64() % 500, rng.u64()));
            d
        })
        .collect()
}

struct Case {
    t: TestDb,
    car: u32,
    rows: Vec<Drive>,
    snap: std::path::PathBuf,
    log: std::path::PathBuf,
}

/// A car with a table folded into a snapshot of several chunks (`journal`: more rows after it, in the journal).
fn case(journal: usize) -> Case {
    let t = test_db(quiet());
    let car = t.db.create_car(new_car("Made-up car", &[])).unwrap().id;
    let mut rng = Rng::new(7);
    let rows = bulk(car, 1200, &mut rng);
    t.db.upsert(car, &rows).unwrap();
    t.db.settle(car, 0).unwrap();
    let dir = t.data.join("db/cars").join(car.to_string());
    let (snap, log) = (dir.join("drives.snap"), dir.join("drives.log"));
    let chunks = (std::fs::metadata(&snap).unwrap().len() as usize - HEADER).div_ceil(CHUNK + TAG);
    assert!(chunks >= 4, "{chunks} chunks: the snapshot should span several");
    let mut all = rows;
    for i in 0..journal {
        let d = drive(car, 5000 + i as u64, DAY0 + (2000 + i as u64) * H, 20, 1.5, Source::Teslamate);
        t.db.upsert(car, std::slice::from_ref(&d)).unwrap();
        all.push(d);
    }
    Case { t, car, rows: all, snap, log }
}

fn reopened(c: &Case) -> db::Db {
    c.t.try_reopen(quiet()).unwrap()
}

#[test]
fn a_clean_table_salvages_whole() {
    let c = case(3);
    let s = reopened(&c).salvage_table::<Drive>(c.car).unwrap();
    assert!(!s.lost_something(), "{:?}", s.why());
    assert!(s.snapshot.whole && s.snapshot.lost_from_ms.is_none());
    assert_eq!((s.rows.len(), s.journal.applied), (c.rows.len(), 3));
    let mut want = c.rows.clone();
    want.sort_by_key(|d| (d.start_ms, d.id));
    assert_eq!(s.rows, want);
}

#[test]
fn a_snapshot_cut_in_half_gives_its_first_rows_and_says_where_the_loss_begins() {
    let c = case(0);
    let b = std::fs::read(&c.snap).unwrap();
    std::fs::write(&c.snap, &b[..b.len() / 2]).unwrap();
    let db = reopened(&c);
    assert!(matches!(db.list::<Drive>(c.car, &Query::default()), Err(Error::Damaged { .. })));
    let s = db.salvage_table::<Drive>(c.car).unwrap();
    assert!(s.lost_something());
    assert!(!s.snapshot.whole && s.snapshot.found);
    assert!(s.rows.len() > 100 && s.rows.len() < c.rows.len(), "{} rows", s.rows.len());
    // whole rows, in order, from the start: a prefix of what was there
    assert_eq!(s.rows[..], c.rows[..s.rows.len()]);
    assert_eq!(s.lost_from_ms(), s.rows.last().map(|d| d.start_ms));
    // nothing was changed
    assert_eq!(std::fs::read(&c.snap).unwrap().len(), b.len() / 2);
}

#[test]
fn a_snapshot_with_one_bad_chunk_gives_the_rows_before_it() {
    let c = case(0);
    let mut b = std::fs::read(&c.snap).unwrap();
    // the second chunk
    b[HEADER + CHUNK + TAG + 100] ^= 0x40;
    std::fs::write(&c.snap, &b).unwrap();
    let s = reopened(&c).salvage_table::<Drive>(c.car).unwrap();
    assert!(!s.snapshot.whole);
    assert!(s.rows.len() > 50 && s.rows.len() < c.rows.len() / 2, "{} rows", s.rows.len());
    assert_eq!(s.rows[..], c.rows[..s.rows.len()]);
}

#[test]
fn a_snapshot_whose_first_chunk_is_bad_gives_nothing_of_it_but_the_journal_still_applies() {
    let c = case(2);
    let mut b = std::fs::read(&c.snap).unwrap();
    b[HEADER + 50] ^= 0x40;
    std::fs::write(&c.snap, &b).unwrap();
    let s = reopened(&c).salvage_table::<Drive>(c.car).unwrap();
    assert_eq!((s.snapshot.rows, s.snapshot.lost_from_ms), (0, Some(0)));
    assert_eq!(s.rows.len(), 2, "the journal's two rows");
    assert_eq!(s.journal.applied, 2);
}

#[test]
fn a_bad_journal_record_loses_only_its_change() {
    let c = case(5);
    let b = std::fs::read(&c.log).unwrap();
    // records: the header, then one per change; damage the third record's ciphertext (not its length, not its tag)
    let rec = |at: usize| (at + 4 + u32::from_be_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]) as usize, at + 4);
    let first = vault::records::HEADER;
    let (second, _) = rec(first);
    let (third, _) = rec(second);
    let (_, third_body) = rec(third);
    let mut bad = b.clone();
    bad[third_body + 30] ^= 0x40;
    std::fs::write(&c.log, &bad).unwrap();
    let db = reopened(&c);
    assert!(matches!(db.list::<Drive>(c.car, &Query::default()), Err(Error::Damaged { .. })));
    let s = db.salvage_table::<Drive>(c.car).unwrap();
    assert!(s.snapshot.whole);
    assert_eq!((s.journal.lost_entries, s.journal.applied), (1, 4));
    assert_eq!(s.rows.len(), c.rows.len() - 1);
    assert!(s.lost_something());
    assert_eq!(s.lost_from_ms(), Some(0), "when the lost change was is not known");
}

#[test]
fn a_table_put_aside_is_kept_and_written_again_with_what_was_read() {
    let c = case(0);
    let b = std::fs::read(&c.snap).unwrap();
    std::fs::write(&c.snap, &b[..b.len() / 2]).unwrap();
    let db = reopened(&c);
    assert!(db.count::<Drive>(c.car).is_err());
    assert_eq!(db.damaged().len(), 1);
    let s = db.salvage_table::<Drive>(c.car).unwrap();
    let files = |db: &db::Db| db.backup(std::io::sink()).unwrap().files;
    let before = files(&db);
    let aside = db.set_aside_table(c.car, Table::Drives, "test").unwrap().expect("a folder");
    let folder = c.t.data.join(&aside);
    assert!(aside.starts_with(&format!("db/cars/{}/aside/", c.car)), "{aside}");
    assert_eq!(std::fs::read(folder.join("drives.snap")).unwrap().len(), b.len() / 2, "kept as it was");
    assert!(folder.join("NOTE.txt").exists());
    assert!(!c.snap.exists());
    assert!(db.damaged().is_empty(), "no longer listed once put aside");
    // the table is empty and not an error; a table with no files while the summary counts rows is written this way
    db.restore_table(c.car, &s.rows).unwrap();
    assert_eq!(db.count::<Drive>(c.car).unwrap(), s.rows.len());
    // a backup (and so an export, and a move) carries the files put aside, and their note, beside the new table
    assert_eq!(files(&db), before + 2);
    assert_eq!(db.entry(c.car).unwrap().unwrap().summary.tally.can.drives, s.rows.len() as i64);
    drop(db);
    // and it is the table after a restart
    let again = reopened(&c);
    assert_eq!(again.count::<Drive>(c.car).unwrap(), s.rows.len());
    assert!(again.damaged().is_empty());
    // nothing there to put aside the second time
    assert_eq!(again.set_aside_table(c.car, Table::Charges, "none").unwrap(), None);
}
