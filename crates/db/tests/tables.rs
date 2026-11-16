//! A car's tables: round trips, paging both ways, rebuilt windows that keep TeslaMate rows, summaries kept up to
//! date, and folding journals into snapshots without losing anything.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::*;
use db::{Options, Query, StateRow, Table, Tally};
use model::{Charge, Drive, Source, State};

const H: u64 = 3_600_000;

#[test]
fn rows_round_trip_and_survive_a_restart() {
    let mut t = test_db(Options::default());
    let car = t.db.create_car(new_car("Model Y", &["key-a"])).unwrap().id;
    let d = drive(car, 7, DAY0 + 8 * H, 35, 31.4, Source::Can);
    t.db.upsert(car, std::slice::from_ref(&d)).unwrap();
    assert_eq!(t.db.get::<Drive>(car, 7).unwrap(), Some(d.clone()));
    let c = charge(car, 9, DAY0 + H, 180, 22.5, Source::Can);
    t.db.upsert(car, std::slice::from_ref(&c)).unwrap();
    let s = state(car, 1, DAY0, DAY0 + 8 * H, State::Asleep, Source::Can);
    t.db.upsert(car, std::slice::from_ref(&s)).unwrap();
    let u = db::UpdateRow {
        id: 3,
        update: model::Update {
            car,
            start_ms: DAY0 + 2 * H,
            end_ms: None,
            version: "2026.38.4".into(),
            source: Source::Can,
        },
    };
    t.db.upsert(car, std::slice::from_ref(&u)).unwrap();
    t.reopen(Options::default());
    assert_eq!(t.db.get::<Drive>(car, 7).unwrap(), Some(d));
    assert_eq!(t.db.get::<Charge>(car, 9).unwrap(), Some(c));
    assert_eq!(t.db.get::<StateRow>(car, 1).unwrap(), Some(s));
    assert_eq!(t.db.get::<db::UpdateRow>(car, 3).unwrap(), Some(u));
    assert_eq!(t.db.get::<Drive>(car, 8).unwrap(), None);
    // a row for another car is refused, and an unknown car too
    assert!(t.db.upsert(car, &[drive(car + 1, 1, DAY0, 1, 1.0, Source::Can)]).is_err());
    assert!(matches!(t.db.upsert(99, &[drive(99, 1, DAY0, 1, 1.0, Source::Can)]), Err(db::Error::NoSuchCar(99))));
}

#[test]
fn listing_pages_through_a_time_range_both_ways() {
    let t = test_db(Options::default());
    let car = t.db.create_car(new_car("Model 3", &[])).unwrap().id;
    let drives: Vec<Drive> = (0..250).map(|i| drive(car, 1000 + i, DAY0 + i * H, 30, 10.0, Source::Can)).collect();
    t.db.upsert(car, &drives).unwrap();
    // newest first, 40 at a time
    let mut seen = Vec::new();
    let mut q = Query::default().limit(40);
    loop {
        let page = t.db.list::<Drive>(car, &q).unwrap();
        seen.extend(page.items.iter().map(|d| d.id));
        match page.next {
            Some(c) => {
                // cursors survive being written in a URL
                q.after = Some(c.to_string().parse().unwrap());
            }
            None => break,
        }
    }
    assert_eq!(seen, (1000..1250).rev().collect::<Vec<_>>());
    // oldest first in a window: a drive that started before it but runs into it counts
    let q = Query::between(DAY0 + 10 * H + 15 * 60_000, DAY0 + 20 * H).oldest_first().limit(5);
    let page = t.db.list::<Drive>(car, &q).unwrap();
    assert_eq!(page.items.iter().map(|d| d.id).collect::<Vec<_>>(), [1010, 1011, 1012, 1013, 1014]);
    let page = t.db.list::<Drive>(car, &q.clone().after(page.next)).unwrap();
    assert_eq!(page.items.iter().map(|d| d.id).collect::<Vec<_>>(), [1015, 1016, 1017, 1018, 1019]);
    assert_eq!(page.next, None);
    // an empty or backwards window
    assert!(t.db.list::<Drive>(car, &Query::between(DAY0 + 900 * H, DAY0 + 901 * H)).unwrap().items.is_empty());
    assert!(t.db.list::<Drive>(car, &Query::between(DAY0 + 5 * H, DAY0)).unwrap().items.is_empty());
    // only one source
    t.db.upsert(car, &[drive(car, 5, DAY0 - 5 * H, 30, 1.0, Source::Teslamate)]).unwrap();
    let q = Query { source: Some(Source::Teslamate), ..Query::default() };
    assert_eq!(t.db.list::<Drive>(car, &q).unwrap().items.len(), 1);
}

#[test]
fn a_rebuilt_window_replaces_can_rows_and_keeps_teslamate_rows() {
    let mut t = test_db(Options::default());
    let car = t.db.create_car(new_car("Model Y", &[])).unwrap().id;
    let can: Vec<Drive> = (0..10).map(|i| drive(car, i, DAY0 + i * H, 20, 5.0, Source::Can)).collect();
    let imported = drive(car, 100, DAY0 + 3 * H + 60_000, 20, 7.0, Source::Teslamate);
    t.db.upsert(car, &can).unwrap();
    t.db.upsert(car, std::slice::from_ref(&imported)).unwrap();
    t.db.upsert(car, &[state(car, 50, DAY0 + 2 * H, DAY0 + 3 * H, State::Online, Source::Can)]).unwrap();
    t.db.upsert(car, &[state(car, 51, DAY0 + 4 * H, DAY0 + 5 * H, State::Online, Source::Teslamate)]).unwrap();
    // the timeline rebuilds hours 2..6: two new drives instead of four
    let new = vec![
        drive(car, 200, DAY0 + 2 * H, 50, 12.0, Source::Can),
        drive(car, 201, DAY0 + 5 * H, 50, 12.0, Source::Can),
    ];
    let gone = t.db.replace_window(car, DAY0 + 2 * H, DAY0 + 6 * H, &new).unwrap();
    assert_eq!(gone, 4);
    let ids = |db: &db::Db| -> Vec<u64> {
        let mut v: Vec<u64> = db.list::<Drive>(car, &Query::default()).unwrap().items.iter().map(|d| d.id).collect();
        v.sort();
        v
    };
    assert_eq!(ids(&t.db), [0, 1, 6, 7, 8, 9, 100, 200, 201]);
    // rows must start inside the window
    assert!(t.db.replace_window(car, DAY0, DAY0 + H, &[drive(car, 300, DAY0 + 2 * H, 1, 1.0, Source::Can)]).is_err());
    // delete everything CAN made in a window, in every table
    let del = t.db.delete_window(car, DAY0, DAY0 + 6 * H).unwrap();
    assert_eq!((del.drives, del.states), (4, 1));
    assert_eq!(ids(&t.db), [6, 7, 8, 9, 100]);
    assert_eq!(t.db.get::<Drive>(car, 100).unwrap(), Some(imported));
    assert!(t.db.get::<StateRow>(car, 51).unwrap().is_some(), "TeslaMate state kept");
    t.reopen(Options::default());
    assert_eq!(ids(&t.db), [6, 7, 8, 9, 100]);
}

#[test]
fn summaries_follow_every_change() {
    let mut t = test_db(Options::default());
    let car = t.db.create_car(new_car("Model Y", &["k1"])).unwrap().id;
    let tally = |db: &db::Db| db.entry(car).unwrap().unwrap().summary.tally;
    t.db.upsert(car, &[drive(car, 1, DAY0, 30, 10.5, Source::Can), drive(car, 2, DAY0 + H, 30, 20.25, Source::Can)])
        .unwrap();
    t.db.upsert(car, &[drive(car, 3, DAY0 - H, 30, 99.0, Source::Teslamate)]).unwrap();
    t.db.upsert(car, &[charge(car, 1, DAY0 + 2 * H, 60, 30.0, Source::Can)]).unwrap();
    let s = tally(&t.db);
    assert_eq!((s.can.drives, s.can.distance_m, s.can.charges, s.can.charge_added_wh), (2, 30_750, 1, 30_000));
    assert_eq!((s.teslamate.drives, s.teslamate.distance_m), (1, 99_000));
    assert!((s.can.distance_km() - 30.75).abs() < 1e-9);
    // replacing a row counts it once
    t.db.upsert(car, &[drive(car, 2, DAY0 + H, 30, 1.0, Source::Can)]).unwrap();
    assert_eq!(tally(&t.db).can.distance_m, 11_500);
    t.db.delete::<Drive>(car, &[1, 1, 42]).unwrap();
    assert_eq!(tally(&t.db).can.drives, 1);
    assert_eq!(t.db.entry(car).unwrap().unwrap().summary.last_activity_ms, Some(DAY0 + 3 * H));
    // a summary that missed a write (a crash between the two) is mended when the table is next read
    t.db.crash_at(Some(db::Crash::BeforeSummary));
    assert!(t.db.upsert(car, &[drive(car, 4, DAY0 + 5 * H, 30, 4.0, Source::Can)]).is_err());
    assert_eq!(tally(&t.db).can.drives, 1, "the summary missed it");
    t.reopen(Options::default());
    assert_eq!(tally(&t.db).can.drives, 1, "nothing is opened to list the fleet");
    assert_eq!(t.db.count::<Drive>(car).unwrap(), 3, "the drive was written (with the TeslaMate one)");
    let s = tally(&t.db);
    assert_eq!((s.can.drives, s.can.distance_m), (2, 5_000), "mended once the table was read");
    assert_eq!(s.part(Table::Drives).can.drives, 2);
    assert_eq!(s.part(Table::Charges).can.drives, 0);
    assert_eq!(t.db.entry(car).unwrap().unwrap().summary.last_activity_ms, Some(DAY0 + 5 * H + 30 * 60_000));
    assert_ne!(s, Tally::default());
}

#[test]
fn journals_fold_into_snapshots_without_losing_anything() {
    let opts = Options { fold_min_bytes: 2_000, ..Options::default() };
    let mut t = test_db(opts.clone());
    let car = t.db.create_car(new_car("Model Y", &[])).unwrap().id;
    for i in 0..300 {
        t.db.upsert(car, &[drive(car, i, DAY0 + i * H, 30, 1.0, Source::Can)]).unwrap();
        if i % 3 == 0 {
            t.db.delete::<Drive>(car, &[i / 2]).unwrap();
        }
    }
    let carfiles = t.data.join("db/cars").join(car.to_string());
    let journal = std::fs::metadata(carfiles.join("drives.log")).map_or(0, |m| m.len());
    let snapshot = std::fs::metadata(carfiles.join("drives.snap")).unwrap().len();
    assert!(journal < 2 * snapshot + 70_000, "journal {journal} vs snapshot {snapshot}: it was folded");
    let before = t.db.list::<Drive>(car, &Query::default().limit(1000)).unwrap().items;
    t.reopen(opts);
    let after = t.db.list::<Drive>(car, &Query::default().limit(1000)).unwrap().items;
    assert_eq!(before, after);
    assert_eq!(t.db.count::<Drive>(car).unwrap(), after.len());
    assert_eq!(t.db.entry(car).unwrap().unwrap().summary.tally.can.drives, after.len() as i64);
    // settling folds what is left
    let s = t.db.settle(car, 0).unwrap();
    assert!(s.tables <= 1);
    assert!(!carfiles.join("drives.log").exists());
    assert_eq!(t.db.list::<Drive>(car, &Query::default().limit(1000)).unwrap().items, after);
}
