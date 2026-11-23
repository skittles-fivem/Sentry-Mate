//! Finished months packed into one file a series: no value changes, a snapshot's bytes come back as they were,
//! folders written before there were packs read the same, later writes land, crashes at every step leave every day
//! readable, and a pack that doesn't read is reported with its name (and nothing is removed for it).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::*;
use db::{Crash, DAY_MS, Db, Error, Fields, Options, SeriesCompacted};
use model::{CarId, ChargePoint, Sample, Source};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const H: u64 = 3_600_000;
/// 2024-02-01 00:00 UTC (a leap year's February).
const FEB1: u64 = 1_706_745_600_000;
/// 2024-03-01 and 2024-04-10.
const MAR1: u64 = FEB1 + 29 * DAY_MS;
const APR10: u64 = MAR1 + 40 * DAY_MS;

fn feb(day: u64) -> u64 {
    FEB1 + (day - 1) * DAY_MS
}

fn quiet() -> Options {
    // nothing folds by itself: tests decide when
    Options { fold_min_bytes: 1 << 30, ..Options::default() }
}

/// A made-up charge curve: a point every 10 s from `t0`.
fn curve(t0: u64, n: u64) -> Vec<ChargePoint> {
    (0..n)
        .map(|i| ChargePoint {
            t_ms: t0 + i * 10_000,
            power_kw: Some(dbc(11.0 - i as f64 * 0.01, 0.01, 0.0)),
            voltage_v: Some(dbc(390.0 + i as f64 * 0.02, 0.01, 0.0)),
            current_a: Some(16.0),
            soc: Some(40.0 + (i / 20) as f64 * 0.5),
            battery_temp_c: (i % 3 == 0).then_some(25.5),
            range_km: None,
        })
        .collect()
}

fn seg(t: &TestDb, car: CarId, series: &str) -> PathBuf {
    t.data.join("db/cars").join(car.to_string()).join("seg").join(series)
}

/// The names in a series' folder with this ending, in order.
fn names(dir: &Path, ending: &str) -> Vec<String> {
    let mut v: Vec<String> = match std::fs::read_dir(dir) {
        Ok(rd) => rd.map(|e| e.unwrap().file_name().to_string_lossy().to_string()).collect(),
        Err(_) => Vec::new(),
    };
    v.retain(|n| n.ends_with(ending));
    v.sort();
    v
}

/// A sealed file's plaintext.
fn opened(t: &TestDb, file: &Path) -> Vec<u8> {
    let label = file.strip_prefix(&t.data).unwrap().to_string_lossy().replace('\\', "/");
    t.vault.open_sealed(&label, &std::fs::read(file).unwrap()).unwrap().to_vec()
}

/// What a day's snapshot file holds after its header: (generation, level, body).
fn snap_parts(t: &TestDb, file: &Path) -> (u64, u8, Vec<u8>) {
    let p = opened(t, file);
    assert_eq!(&p[..5], b"SMDB\x03");
    (u64::from_be_bytes(p[7..15].try_into().unwrap()), p[15], p[16..].to_vec())
}

/// Every loose snapshot of a series, by day name.
fn snaps(t: &TestDb, dir: &Path) -> BTreeMap<String, (u64, u8, Vec<u8>)> {
    names(dir, ".snap").into_iter().map(|n| (n[..10].to_string(), snap_parts(t, &dir.join(&n)))).collect()
}

/// A pack read by the format its docs give, on its own: day name -> (generation, level, body).
fn pack_parts(t: &TestDb, file: &Path) -> BTreeMap<String, (u64, u8, Vec<u8>)> {
    let p = opened(t, file);
    assert_eq!(&p[..5], b"SMDB\x0b");
    let count = p[7] as usize;
    let mut at = 8 + count * 25;
    let mut out = BTreeMap::new();
    for i in 0..count {
        let e = &p[8 + i * 25..8 + (i + 1) * 25];
        let day = u32::from_be_bytes(e[..4].try_into().unwrap()) as u64;
        let generation = u64::from_be_bytes(e[4..12].try_into().unwrap());
        let len = u32::from_be_bytes(e[13..17].try_into().unwrap()) as usize;
        // the day's name from the db's own listing of dates would be circular: work it out from FEB1
        let name = date_of(day);
        out.insert(name, (generation, e[12], p[at..at + len].to_vec()));
        at += len;
    }
    assert_eq!(at, p.len(), "the bodies fill the pack");
    out
}

/// `YYYY-MM-DD` of a day number, for 2024-02-01 .. 2024-04-30 (all these tests pack).
fn date_of(day: u64) -> String {
    let d = day.checked_sub(FEB1 / DAY_MS).expect("a day of 2024");
    match d {
        0..=28 => format!("2024-02-{:02}", d + 1),
        29..=59 => format!("2024-03-{:02}", d - 28),
        60..=89 => format!("2024-04-{:02}", d - 59),
        _ => panic!("day {day} is outside the test's months"),
    }
}

/// Everything read from a car's CAN series, many ways.
#[derive(Debug, PartialEq)]
struct Reads {
    samples: Vec<Vec<Sample>>,
    curves: Vec<Vec<ChargePoint>>,
    fields: Vec<Fields>,
    curve_fields: Vec<Fields>,
    sample_days: Vec<String>,
    curve_days: Vec<String>,
}

fn windows(first: u64, days: u64) -> Vec<(u64, u64)> {
    let mut w = vec![(0, u64::MAX), (first, first + days * DAY_MS), (first + 3 * DAY_MS + 5, first + 80 * DAY_MS)];
    for d in 0..days {
        let s = first + d * DAY_MS;
        w.push((s, s + DAY_MS));
        // across midnight, and a few minutes inside the day
        w.push((s + 23 * H, s + DAY_MS + 2 * H));
        w.push((s + H + 100_000, s + H + 400_000));
    }
    w
}

fn reads_in(db: &Db, car: CarId, first: u64, days: u64) -> Reads {
    let w = windows(first, days);
    Reads {
        samples: w.iter().map(|&(a, b)| db.read_points::<Sample>(car, Source::Live, a, b).unwrap()).collect(),
        curves: w.iter().map(|&(a, b)| db.read_points::<ChargePoint>(car, Source::Live, a, b).unwrap()).collect(),
        fields: w
            .iter()
            .map(|&(a, b)| db.read_fields::<Sample>(car, Source::Live, a, b, &["speed_kmh", "soc", "gear"]).unwrap())
            .collect(),
        curve_fields: w
            .iter()
            .map(|&(a, b)| db.read_fields::<ChargePoint>(car, Source::Live, a, b, &["power_kw", "soc"]).unwrap())
            .collect(),
        sample_days: db.days::<Sample>(car, Source::Live).unwrap().into_iter().map(|d| d.date).collect(),
        curve_days: db.days::<ChargePoint>(car, Source::Live).unwrap().into_iter().map(|d| d.date).collect(),
    }
}

fn reads(db: &Db, car: CarId) -> Reads {
    reads_in(db, car, FEB1, 33)
}

/// Runs the pass to its end, a few days a step.
fn compact(db: &Db, car: CarId, before: u64) -> SeriesCompacted {
    let mut total = SeriesCompacted::default();
    for _ in 0..1000 {
        let c = db.compact_series(car, before, 3).unwrap();
        total.days_folded += c.days_folded;
        total.days_packed += c.days_packed;
        if !c.more {
            assert_eq!(db.series_pending(car, before).unwrap(), 0, "nothing is left after the last step");
            return total;
        }
        assert!(c.days_folded + c.days_packed > 0, "a step that says more did something");
    }
    panic!("the pass never ends");
}

/// A whole day of samples (cold at once), `n` of them from 01:00.
fn whole_day(db: &Db, car: CarId, day_ms: u64, n: u64) -> Vec<Sample> {
    let pts = samples(day_ms + H, n);
    db.write_points(car, Source::Live, day_ms, day_ms + DAY_MS, &pts).unwrap();
    pts
}

/// February and the start of March 2024 with gaps, a day of one point, a day written a window at a time, an emptied
/// day, the leap day and the month's boundary; both series.
fn history(t: &TestDb) -> CarId {
    let car = t.db.create_car(new_car("Model Y", &[])).unwrap().id;
    for (day, n) in [(1, 600), (2, 1), (28, 900), (29, 300), (30, 450), (31, 200)] {
        // feb(30) and feb(31) are March 1 and 2
        whole_day(&t.db, car, feb(day), n);
    }
    // a day written as the timeline does: a minute at a time, across midnight into the 6th
    let pts = samples(feb(5) + 23 * H, 2 * 3600);
    for minute in pts.chunks(60) {
        t.db.write_points(car, Source::Live, minute[0].t_ms, minute[0].t_ms + 60_000, minute).unwrap();
    }
    // a day written and emptied again
    whole_day(&t.db, car, feb(10), 50);
    t.db.write_points::<Sample>(car, Source::Live, feb(10), feb(11), &[]).unwrap();
    for (day, n) in [(1, 40), (5, 1), (29, 25), (30, 10)] {
        let c = curve(feb(day) + 2 * H, n);
        t.db.write_points(car, Source::Live, feb(day) + H, feb(day) + 20 * H, &c).unwrap();
    }
    car
}

#[test]
fn packing_changes_no_value_and_gives_back_every_snapshots_bytes() {
    let mut t = test_db(quiet());
    let car = history(&t);
    assert!(t.db.settle(car, APR10).unwrap().days > 0);
    let (sdir, cdir) = (seg(&t, car, "samples.can"), seg(&t, car, "charge.can"));
    let before = reads(&t.db, car);
    assert_eq!(
        before.sample_days,
        [
            "2024-02-01",
            "2024-02-02",
            "2024-02-05",
            "2024-02-06",
            "2024-02-28",
            "2024-02-29",
            "2024-03-01",
            "2024-03-02"
        ]
    );
    assert_eq!(before.curve_days, ["2024-02-01", "2024-02-05", "2024-02-29", "2024-03-01"]);
    assert_eq!(before.samples[0].len(), 600 + 1 + 900 + 300 + 450 + 200 + 7200);
    let (sample_snaps, curve_snaps) = (snaps(&t, &sdir), snaps(&t, &cdir));
    assert_eq!(sample_snaps.len(), 8);
    assert!(sample_snaps.values().all(|s| s.1 == 2), "settled: every day is in the small form");
    assert_eq!(t.db.series_pending(car, APR10).unwrap(), 12);
    // days of a month that is not over stay as they are
    assert_eq!(t.db.compact_series(car, MAR1 - 1, 1000).unwrap(), SeriesCompacted::default());
    assert_eq!(t.db.series_pending(car, MAR1 - 1).unwrap(), 0);

    let done = compact(&t.db, car, APR10);
    assert_eq!((done.days_folded, done.days_packed), (0, 12));
    assert_eq!(names(&sdir, ""), ["2024-02.pack", "2024-03.pack"]);
    assert_eq!(names(&cdir, ""), ["2024-02.pack", "2024-03.pack"]);
    assert_eq!(reads(&t.db, car), before);
    // every snapshot's generation, level and bytes are in the packs as they were
    let mut packed = pack_parts(&t, &sdir.join("2024-02.pack"));
    assert_eq!(packed.len(), 6);
    packed.extend(pack_parts(&t, &sdir.join("2024-03.pack")));
    assert_eq!(packed, sample_snaps);
    let mut packed = pack_parts(&t, &cdir.join("2024-02.pack"));
    packed.extend(pack_parts(&t, &cdir.join("2024-03.pack")));
    assert_eq!(packed, curve_snaps);
    // a packed day's size is its body's
    for d in t.db.days::<Sample>(car, Source::Live).unwrap() {
        assert_eq!(d.bytes, sample_snaps[&d.date].2.len() as u64, "{}", d.date);
    }
    // a pass again finds nothing; so does a restart
    assert_eq!(t.db.compact_series(car, APR10, 1000).unwrap(), SeriesCompacted::default());
    t.reopen(quiet());
    assert_eq!(reads(&t.db, car), before);
    assert_eq!(t.db.series_pending(car, APR10).unwrap(), 0);
    // and the files are owner-only like every other
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(sdir.join("2024-02.pack")).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}

#[test]
fn a_folder_written_before_there_were_packs_reads_the_same_before_and_after_packing() {
    // written only by the paths that were there before packs: windows a minute at a time, whole days, an import
    let mut t = test_db(Options { fold_min_bytes: 6_000, ..Options::default() });
    let car = t.db.create_car(new_car("Model 3", &[])).unwrap().id;
    let mut imported = Vec::new();
    for d in [0u64, 1, 2, 9, 15, 16, 40] {
        let pts = samples(DAY0 + d * DAY_MS + 23 * H, 4500 + d * 30);
        for w in pts.chunks(300) {
            t.db.write_points(car, Source::Live, w[0].t_ms, w[0].t_ms + 300_000, w).unwrap();
        }
        let old = samples(DAY0 + d * DAY_MS + 5 * H, 90);
        t.db.write_points(car, Source::Teslamate, DAY0 + d * DAY_MS, DAY0 + (d + 1) * DAY_MS, &old).unwrap();
        imported.extend(old);
        let c = curve(DAY0 + d * DAY_MS + 3 * H, 30 + d);
        t.db.write_points(car, Source::Live, c[0].t_ms, c[0].t_ms + H, &c).unwrap();
    }
    let dir = t.data.join("db/cars").join(car.to_string());
    let files = |ending: &str| all_files(&dir).iter().filter(|f| f.to_string_lossy().ends_with(ending)).count();
    assert_eq!(files(".pack"), 0);
    assert!(files(".log") > 0 && files(".snap") > 0, "snapshots and journals, as an older server left them");
    let read = |db: &Db| {
        let import: Vec<Sample> = db.read_points(car, Source::Teslamate, 0, u64::MAX).unwrap();
        (reads_in(db, car, DAY0, 45), import)
    };
    let before = read(&t.db);
    assert_eq!(before.1, imported);
    assert_eq!(before.0.sample_days.len(), 11);
    // opened again (as by the server that knows packs): the same, and still no pack
    t.reopen(quiet());
    assert_eq!(read(&t.db), before);
    assert_eq!(files(".pack"), 0);
    // packed a little at a time: the same after every step
    let end = DAY0 + 80 * DAY_MS;
    assert!(t.db.series_pending(car, end).unwrap() > 0);
    let mut steps = 0;
    loop {
        let c = t.db.compact_series(car, end, 2).unwrap();
        assert_eq!(read(&t.db), before, "after step {steps}");
        steps += 1;
        if !c.more {
            break;
        }
    }
    assert!(steps > 3, "the work came in steps");
    assert_eq!(t.db.series_pending(car, end).unwrap(), 0);
    assert_eq!((files(".snap"), files(".log"), files(".tmp")), (0, 0, 0));
    // November and December of 2026, three series
    assert_eq!(files(".pack"), 6);
    t.reopen(quiet());
    assert_eq!(read(&t.db), before);
}

#[test]
fn the_stamps_of_untouched_days_stay_when_other_days_go_into_the_pack() {
    let t = test_db(quiet());
    let car = history(&t);
    compact(&t.db, car, APR10);
    let stamps =
        |db: &Db| -> BTreeMap<u64, u64> { db.day_stamps::<Sample>(car, Source::Live).unwrap().into_iter().collect() };
    let s0 = stamps(&t.db);
    assert_eq!(s0.len(), 8);
    // a window into one packed day: only that day's stamp moves
    let late = samples(feb(2) + 5 * H, 30);
    t.db.write_points(car, Source::Live, feb(2) + 5 * H, feb(2) + 6 * H, &late).unwrap();
    let s1 = stamps(&t.db);
    for (day, st) in &s0 {
        assert_eq!(s1[day] != *st, *day == feb(2), "day {day}");
    }
    // folded and taken back into the pack, with a day the month didn't have: the pack is another file, the other
    // days' stamps are what they were
    whole_day(&t.db, car, feb(7), 80);
    let pack = seg(&t, car, "samples.can").join("2024-02.pack");
    let file0 = std::fs::read(&pack).unwrap();
    let done = compact(&t.db, car, APR10);
    assert_eq!((done.days_folded, done.days_packed), (1, 2));
    assert_ne!(std::fs::read(&pack).unwrap(), file0);
    let s2 = stamps(&t.db);
    assert_eq!(s2.len(), 9);
    for (day, st) in &s0 {
        assert_eq!(s2[day] != *st, *day == feb(2), "day {day}");
    }
    assert_ne!(s2[&feb(2)], s1[&feb(2)]);
}

#[test]
fn writes_into_packed_days_land_and_go_back_into_the_pack() {
    let mut t = test_db(quiet());
    let car = history(&t);
    compact(&t.db, car, APR10);
    let sdir = seg(&t, car, "samples.can");
    let all = |db: &Db| db.read_points::<Sample>(car, Source::Live, 0, u64::MAX).unwrap();
    let base = all(&t.db);

    // a window into a packed day: a journal next to the pack
    let late = samples(feb(1) + 10 * H, 120);
    t.db.write_points(car, Source::Live, feb(1) + 10 * H, feb(1) + 11 * H, &late).unwrap();
    assert_eq!(names(&sdir, ".log"), ["2024-02-01.log"]);
    assert!(names(&sdir, ".snap").is_empty());
    let mut want = base.clone();
    let at = want.partition_point(|s| s.t_ms < late[0].t_ms);
    want.splice(at..at, late.clone());
    assert_eq!(all(&t.db), want);
    let day: Vec<Sample> = t.db.read_points(car, Source::Live, feb(1), feb(2)).unwrap();
    assert_eq!(day.len(), 720);
    let f = t.db.read_fields::<Sample>(car, Source::Live, feb(1), feb(2), &["soc"]).unwrap();
    assert_eq!(f.t_ms, day.iter().map(|s| s.t_ms).collect::<Vec<_>>());
    t.reopen(quiet());
    assert_eq!(all(&t.db), want);
    // a second window into it carries on in the same journal
    t.db.write_points::<Sample>(car, Source::Live, feb(1) + 10 * H, feb(1) + 10 * H + 60_000, &[]).unwrap();
    want.retain(|s| !(feb(1) + 10 * H..feb(1) + 10 * H + 60_000).contains(&s.t_ms));
    assert_eq!(all(&t.db), want);
    // folded: the day's own snapshot (the next generation) wins over the pack's entry
    assert_eq!(t.db.series_pending(car, APR10).unwrap(), 1);
    assert_eq!(t.db.settle(car, APR10).unwrap().days, 1);
    assert_eq!((names(&sdir, ".log").len(), names(&sdir, ".snap")), (0, vec!["2024-02-01.snap".to_string()]));
    let in_pack = pack_parts(&t, &sdir.join("2024-02.pack"));
    let own = snap_parts(&t, &sdir.join("2024-02-01.snap"));
    assert_eq!(own.0, in_pack["2024-02-01"].0 + 1);
    assert_eq!(all(&t.db), want);
    // and the next pass takes it into the pack
    let done = compact(&t.db, car, APR10);
    assert_eq!((done.days_folded, done.days_packed), (0, 1));
    assert!(names(&sdir, ".snap").is_empty());
    assert_eq!(pack_parts(&t, &sdir.join("2024-02.pack"))["2024-02-01"], own);
    assert_eq!(all(&t.db), want);

    // a whole day written again
    let again = samples(feb(28) + 7 * H, 33);
    t.db.write_points(car, Source::Live, feb(28), feb(29), &again).unwrap();
    want.retain(|s| !(feb(28)..feb(29)).contains(&s.t_ms));
    let at = want.partition_point(|s| s.t_ms < again[0].t_ms);
    want.splice(at..at, again);
    assert_eq!(all(&t.db), want);
    t.reopen(quiet());
    assert_eq!(all(&t.db), want);
    assert_eq!(compact(&t.db, car, APR10).days_packed, 1);
    assert_eq!(all(&t.db), want);
    assert_eq!(names(&sdir, ""), ["2024-02.pack", "2024-03.pack"]);
}

#[test]
fn a_day_emptied_after_it_was_packed_does_not_come_back() {
    let mut t = test_db(quiet());
    let car = history(&t);
    compact(&t.db, car, APR10);
    let sdir = seg(&t, car, "samples.can");
    let all = |db: &Db| db.read_points::<Sample>(car, Source::Live, 0, u64::MAX).unwrap();
    let mut want = all(&t.db);
    let dates = |db: &Db| -> Vec<String> {
        let days = db.days::<Sample>(car, Source::Live).unwrap();
        days.into_iter()
            .filter(|d| {
                !db.read_points::<Sample>(car, Source::Live, d.start_ms, d.start_ms + DAY_MS).unwrap().is_empty()
            })
            .map(|d| d.date)
            .collect()
    };

    // a whole day deleted: the pack still has it, an empty snapshot stands in front
    t.db.write_points::<Sample>(car, Source::Live, feb(28), feb(29), &[]).unwrap();
    want.retain(|s| !(feb(28)..feb(29)).contains(&s.t_ms));
    assert!(pack_parts(&t, &sdir.join("2024-02.pack")).contains_key("2024-02-28"));
    assert_eq!(all(&t.db), want);
    assert!(t.db.read_points::<Sample>(car, Source::Live, feb(28), feb(29)).unwrap().is_empty());
    // a day emptied by a window that covers its points but not the day (a journal first)
    t.db.write_points::<Sample>(car, Source::Live, feb(29) + 30 * 60_000, feb(29) + 5 * H, &[]).unwrap();
    want.retain(|s| !(feb(29)..feb(30)).contains(&s.t_ms));
    assert_eq!(all(&t.db), want);
    // settling, a restart, deleting again, settling again: never back
    t.db.settle(car, APR10).unwrap();
    assert_eq!(all(&t.db), want);
    t.reopen(quiet());
    assert_eq!(all(&t.db), want);
    t.db.delete_window(car, feb(28), feb(30)).unwrap();
    assert_eq!(t.db.settle(car, APR10).unwrap().days, 0);
    assert_eq!(all(&t.db), want);
    assert!(!dates(&t.db).iter().any(|d| d == "2024-02-28" || d == "2024-02-29"));
    // the pass drops them from the pack and removes the empty snapshots
    // (the two days of samples, and the charge curve of the 29th that went with them)
    let done = compact(&t.db, car, APR10);
    assert_eq!(done.days_packed, 3);
    assert!(names(&sdir, ".snap").is_empty() && names(&sdir, ".log").is_empty());
    let packed = pack_parts(&t, &sdir.join("2024-02.pack"));
    assert_eq!(
        packed.keys().map(String::as_str).collect::<Vec<_>>(),
        ["2024-02-01", "2024-02-02", "2024-02-05", "2024-02-06"]
    );
    assert_eq!(all(&t.db), want);
    let listed: Vec<String> = t.db.days::<Sample>(car, Source::Live).unwrap().into_iter().map(|d| d.date).collect();
    assert_eq!(listed, ["2024-02-01", "2024-02-02", "2024-02-05", "2024-02-06", "2024-03-01", "2024-03-02"]);
    // a day written anew after it was dropped starts over
    let anew = whole_day(&t.db, car, feb(28), 12);
    assert_eq!(t.db.read_points::<Sample>(car, Source::Live, feb(28), feb(29)).unwrap(), anew);

    // a whole month emptied: its pack goes
    t.db.delete_window(car, MAR1, MAR1 + 31 * DAY_MS).unwrap();
    assert!(t.db.read_points::<Sample>(car, Source::Live, MAR1, MAR1 + 31 * DAY_MS).unwrap().is_empty());
    assert!(sdir.join("2024-03.pack").exists());
    compact(&t.db, car, APR10);
    assert_eq!(names(&sdir, ""), ["2024-02.pack"]);
    assert!(names(&seg(&t, car, "charge.can"), "").iter().all(|n| n == "2024-02.pack"));
    t.reopen(quiet());
    assert!(t.db.read_points::<Sample>(car, Source::Live, MAR1, MAR1 + 31 * DAY_MS).unwrap().is_empty());
    assert_eq!(t.db.series_pending(car, APR10).unwrap(), 0);
}

#[test]
fn settling_folds_late_writes_into_days_it_had_settled() {
    let mut t = test_db(quiet());
    let car = history(&t);
    t.db.settle(car, APR10).unwrap();
    let sdir = seg(&t, car, "samples.can");
    // a late window whose write stops before the car's note of how far it is settled is moved back (the journal
    // entry is on disk; the note still says February is settled)
    let late = samples(feb(2) + 9 * H, 20);
    t.db.crash_at(Some(Crash::BeforeRename));
    let r = t.db.write_points(car, Source::Live, feb(2) + 9 * H, feb(2) + 10 * H, &late);
    assert!(matches!(r, Err(Error::Crashed(Crash::BeforeRename))));
    t.reopen(quiet());
    assert_eq!(names(&sdir, ".log"), ["2024-02-02.log"]);
    let day: Vec<Sample> = t.db.read_points(car, Source::Live, feb(2), feb(3)).unwrap();
    assert_eq!(day.len(), 21);
    // (that day to fold; the other 11 snapshots wait to be packed)
    assert_eq!(t.db.series_pending(car, APR10).unwrap(), 12);
    assert_eq!(t.db.settle(car, APR10).unwrap().days, 1);
    assert!(names(&sdir, ".log").is_empty());
    assert_eq!(snap_parts(&t, &sdir.join("2024-02-02.snap")).1, 2, "settled again");
    assert_eq!(t.db.read_points::<Sample>(car, Source::Live, feb(2), feb(3)).unwrap(), day);
    assert_eq!(t.db.settle(car, APR10).unwrap().days, 0);

    // the same into a day that is in a pack, settled without packing
    compact(&t.db, car, APR10);
    t.db.crash_at(Some(Crash::BeforeRename));
    let later = samples(feb(2) + 12 * H, 5);
    assert!(t.db.write_points(car, Source::Live, feb(2) + 12 * H, feb(2) + 13 * H, &later).is_err());
    t.reopen(quiet());
    assert_eq!(t.db.settle(car, APR10).unwrap().days, 1);
    assert_eq!((names(&sdir, ".log").len(), names(&sdir, ".snap").len()), (0, 1));
    assert_eq!(t.db.read_points::<Sample>(car, Source::Live, feb(2), feb(3)).unwrap().len(), 26);
    assert_eq!(compact(&t.db, car, APR10).days_packed, 1);
    assert_eq!(t.db.read_points::<Sample>(car, Source::Live, feb(2), feb(3)).unwrap().len(), 26);
}

#[test]
fn a_crash_before_the_pack_is_in_place_leaves_the_days_as_they_were() {
    let mut t = test_db(quiet());
    let car = history(&t);
    t.db.settle(car, APR10).unwrap();
    let sdir = seg(&t, car, "samples.can");
    let before = reads(&t.db, car);
    let loose = names(&sdir, ".snap");
    t.db.crash_at(Some(Crash::BeforeRename));
    assert!(matches!(t.db.compact_series(car, APR10, 1000), Err(Error::Crashed(Crash::BeforeRename))));
    assert_eq!(names(&sdir, ".tmp"), ["2024-02.pack.tmp"]);
    assert!(names(&sdir, ".pack").is_empty());
    assert_eq!(names(&sdir, ".snap"), loose);
    assert_eq!(reads(&t.db, car), before);
    t.reopen(quiet());
    assert_eq!(reads(&t.db, car), before);
    // the next pass finishes and leaves nothing behind
    assert_eq!(compact(&t.db, car, APR10).days_packed, 12);
    assert_eq!(names(&sdir, ""), ["2024-02.pack", "2024-03.pack"]);
    assert_eq!(reads(&t.db, car), before);

    // the same with a pack there already: the old pack and the new day's snapshot stay
    let new = whole_day(&t.db, car, feb(7), 70);
    let pack0 = std::fs::read(sdir.join("2024-02.pack")).unwrap();
    let before = reads(&t.db, car);
    t.db.crash_at(Some(Crash::BeforeRename));
    assert!(matches!(t.db.compact_series(car, APR10, 1000), Err(Error::Crashed(Crash::BeforeRename))));
    assert_eq!(std::fs::read(sdir.join("2024-02.pack")).unwrap(), pack0);
    assert_eq!(names(&sdir, ".snap"), ["2024-02-07.snap"]);
    t.reopen(quiet());
    assert_eq!(reads(&t.db, car), before);
    assert_eq!(compact(&t.db, car, APR10).days_packed, 1);
    assert_eq!(names(&sdir, ""), ["2024-02.pack", "2024-03.pack"]);
    assert_eq!(reads(&t.db, car), before);
    assert_eq!(t.db.read_points::<Sample>(car, Source::Live, feb(7), feb(8)).unwrap(), new);
}

#[test]
fn a_crash_before_the_packed_snapshots_are_removed_leaves_them_for_the_next_pass() {
    let mut t = test_db(quiet());
    let car = history(&t);
    t.db.settle(car, APR10).unwrap();
    let sdir = seg(&t, car, "samples.can");
    let before = reads(&t.db, car);
    let loose = snaps(&t, &sdir);
    t.db.crash_at(Some(Crash::BeforeLooseRemoved));
    assert!(matches!(t.db.compact_series(car, APR10, 1000), Err(Error::Crashed(Crash::BeforeLooseRemoved))));
    // February's days are in both, the same bytes
    assert_eq!(names(&sdir, ".pack"), ["2024-02.pack"]);
    assert_eq!(snaps(&t, &sdir), loose);
    let packed = pack_parts(&t, &sdir.join("2024-02.pack"));
    assert!(packed.iter().all(|(day, parts)| loose[day] == *parts));
    assert_eq!(reads(&t.db, car), before);
    t.reopen(quiet());
    assert_eq!(reads(&t.db, car), before);
    assert_eq!(t.db.series_pending(car, APR10).unwrap(), 12);
    // the next pass removes them without writing the pack again
    let pack0 = std::fs::read(sdir.join("2024-02.pack")).unwrap();
    assert_eq!(compact(&t.db, car, APR10).days_packed, 12);
    assert_eq!(std::fs::read(sdir.join("2024-02.pack")).unwrap(), pack0);
    assert_eq!(names(&sdir, ""), ["2024-02.pack", "2024-03.pack"]);
    assert_eq!(reads(&t.db, car), before);

    // a day emptied, and the crash after the pack dropped it: it stays empty
    t.db.write_points::<Sample>(car, Source::Live, feb(1), feb(2), &[]).unwrap();
    let before = reads(&t.db, car);
    assert!(before.samples[0].iter().all(|s| s.t_ms >= feb(2)));
    t.db.crash_at(Some(Crash::BeforeLooseRemoved));
    assert!(t.db.compact_series(car, APR10, 1000).is_err());
    assert!(!pack_parts(&t, &sdir.join("2024-02.pack")).contains_key("2024-02-01"));
    t.reopen(quiet());
    // the empty snapshot is still listed until the pass removes it; no point is read from it
    let now = reads(&t.db, car);
    assert_eq!((&now.samples, &now.fields, &now.curves), (&before.samples, &before.fields, &before.curves));
    compact(&t.db, car, APR10);
    assert_eq!(names(&sdir, ""), ["2024-02.pack", "2024-03.pack"]);
    assert_eq!(reads(&t.db, car).samples, before.samples);
}

fn damaged_in(e: Error, name: &str) {
    match e {
        Error::Damaged { file, .. } => assert!(file.ends_with(name), "{file} is not {name}"),
        e => panic!("expected damage in {name}, got {e}"),
    }
}

#[test]
fn a_damaged_pack_is_reported_with_its_name_and_nothing_is_removed_for_it() {
    let mut t = test_db(quiet());
    let car = history(&t);
    compact(&t.db, car, APR10);
    let sdir = seg(&t, car, "samples.can");
    // days waiting to go into February's pack: a new one, and one the pack has an older copy of
    let new = whole_day(&t.db, car, feb(7), 70);
    let again = whole_day(&t.db, car, feb(2), 9);
    let before = reads(&t.db, car);
    let pack = sdir.join("2024-02.pack");
    let good = std::fs::read(&pack).unwrap();
    for at in [20, good.len() / 2, good.len() - 3] {
        let mut bad = good.clone();
        bad[at] ^= 0x40;
        std::fs::write(&pack, &bad).unwrap();
        t.reopen(quiet());
        // a packed day doesn't read, by name; never as an empty day
        damaged_in(
            t.db.read_points::<Sample>(car, Source::Live, feb(1), feb(2)).unwrap_err(),
            "samples.can/2024-02.pack",
        );
        damaged_in(t.db.read_points::<Sample>(car, Source::Live, 0, u64::MAX).unwrap_err(), "samples.can/2024-02.pack");
        damaged_in(
            t.db.read_fields::<Sample>(car, Source::Live, feb(5), feb(6), &["soc"]).unwrap_err(),
            "samples.can/2024-02.pack",
        );
        // the days with a snapshot of their own still read
        assert_eq!(t.db.read_points::<Sample>(car, Source::Live, feb(7), feb(8)).unwrap(), new);
        assert_eq!(t.db.read_points::<Sample>(car, Source::Live, feb(2), feb(3)).unwrap(), again);
        // the pass reports it and removes nothing
        damaged_in(t.db.compact_series(car, APR10, 1000).unwrap_err(), "samples.can/2024-02.pack");
        assert_eq!(names(&sdir, ".snap"), ["2024-02-02.snap", "2024-02-07.snap"]);
        assert!(names(&sdir, ".tmp").is_empty());
        assert_eq!(std::fs::read(&pack).unwrap(), bad);
        // nor does a write into one of its days go ahead as if the day were new
        let w = t.db.write_points(car, Source::Live, feb(1) + 20 * H, feb(1) + 21 * H, &samples(feb(1) + 20 * H, 3));
        damaged_in(w.unwrap_err(), "samples.can/2024-02.pack");
        assert!(names(&sdir, ".log").is_empty());
    }
    // a pack cut short, and one that is not a sealed file at all
    for bad in [good[..good.len() - 40].to_vec(), b"not a pack".to_vec(), Vec::new()] {
        std::fs::write(&pack, &bad).unwrap();
        t.reopen(quiet());
        damaged_in(
            t.db.read_points::<Sample>(car, Source::Live, feb(1), feb(2)).unwrap_err(),
            "samples.can/2024-02.pack",
        );
        damaged_in(t.db.compact_series(car, APR10, 1000).unwrap_err(), "samples.can/2024-02.pack");
        assert_eq!(names(&sdir, ".snap").len(), 2);
    }
    // put back: everything reads, and the pass finishes
    std::fs::write(&pack, &good).unwrap();
    t.reopen(quiet());
    assert_eq!(reads(&t.db, car), before);
    assert_eq!(compact(&t.db, car, APR10).days_packed, 2);
    assert_eq!(reads(&t.db, car), before);
}

#[test]
fn a_pack_of_another_version_or_with_a_wrong_index_is_reported() {
    let mut t = test_db(quiet());
    let car = history(&t);
    compact(&t.db, car, APR10);
    let sdir = seg(&t, car, "samples.can");
    whole_day(&t.db, car, feb(7), 70);
    let pack = sdir.join("2024-02.pack");
    let label = format!("db/cars/{car}/seg/samples.can/2024-02.pack");
    let good = opened(&t, &pack);
    let vault = t.vault.clone();
    let put = |plain: &[u8]| std::fs::write(&pack, vault.seal(&label, plain).unwrap()).unwrap();

    // written by a newer Sentry Mate
    let mut newer = good.clone();
    newer[5..7].copy_from_slice(&2u16.to_be_bytes());
    put(&newer);
    t.reopen(quiet());
    for r in [
        t.db.read_points::<Sample>(car, Source::Live, feb(1), feb(2)).map(|_| ()),
        t.db.days::<Sample>(car, Source::Live).map(|_| ()),
        t.db.compact_series(car, APR10, 1000).map(|_| ()),
    ] {
        match r {
            Err(Error::Version { file, found: 2, supported }) => {
                assert_eq!(file, label);
                assert_eq!(supported, db::FORMAT_VERSION);
            }
            r => panic!("expected a version error, got {r:?}"),
        }
    }
    assert_eq!(names(&sdir, ".snap"), ["2024-02-07.snap"]);

    // sealed right, but not what a pack is: another kind, a body changed under its hash, lengths that don't add
    // up, a day of another month, and another month's pack under this name
    let mut kind = good.clone();
    kind[4] = 3;
    let mut body = good.clone();
    let last = body.len() - 1;
    body[last] ^= 1;
    let mut long = good.clone();
    long.push(0);
    let mut day = good.clone();
    day[8..12].copy_from_slice(&((MAR1 / DAY_MS) as u32).to_be_bytes());
    let march = opened(&t, &sdir.join("2024-03.pack"));
    for (what, plain) in [("kind", kind), ("body", body), ("length", long), ("day", day), ("month", march)] {
        put(&plain);
        t.reopen(quiet());
        // the changed body is the month's last day's
        let r = t.db.read_points::<Sample>(car, Source::Live, feb(29), feb(30));
        damaged_in(r.expect_err(what), "samples.can/2024-02.pack");
        damaged_in(t.db.compact_series(car, APR10, 1000).expect_err(what), "samples.can/2024-02.pack");
        assert_eq!(names(&sdir, ".snap"), ["2024-02-07.snap"], "{what}");
        assert!(names(&sdir, ".tmp").is_empty());
    }
    put(&good);
    t.reopen(quiet());
    assert_eq!(compact(&t.db, car, APR10).days_packed, 1);
    assert_eq!(t.db.read_points::<Sample>(car, Source::Live, feb(1), feb(2)).unwrap().len(), 600);
}

#[test]
fn days_read_while_their_month_is_packed_and_written_never_fail_or_go_missing() {
    let t = test_db(quiet());
    let car = t.db.create_car(new_car("Model Y", &[])).unwrap().id;
    let fixed: Vec<Sample> = (1..=20).flat_map(|d| whole_day(&t.db, car, feb(d), 40)).collect();
    let stop = std::sync::atomic::AtomicBool::new(false);
    std::thread::scope(|s| {
        for _ in 0..3 {
            s.spawn(|| {
                let mut n = 0u64;
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    // days 1-20 never change, whatever happens around them and wherever they are kept
                    let (a, b) = if n.is_multiple_of(2) { (feb(1), feb(21)) } else { (0, feb(21)) };
                    assert_eq!(t.db.read_points::<Sample>(car, Source::Live, a, b).unwrap(), fixed);
                    let f = t.db.read_fields::<Sample>(car, Source::Live, feb(3), feb(9), &["soc"]).unwrap();
                    assert_eq!(f.t_ms.len(), 6 * 40);
                    n += 1;
                }
            });
        }
        for round in 0..14u64 {
            // days 21-28 are written, emptied and written again while the month is packed over and over
            let d = 21 + round % 4;
            match round % 3 {
                0 => drop(whole_day(&t.db, car, feb(d), 30 + round)),
                1 => {
                    let w = samples(feb(d) + 15 * H, 20);
                    t.db.write_points(car, Source::Live, feb(d) + 15 * H, feb(d) + 16 * H, &w).unwrap();
                }
                _ => t.db.write_points::<Sample>(car, Source::Live, feb(d), feb(d + 1), &[]).unwrap(),
            }
            compact(&t.db, car, APR10);
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
    });
    assert_eq!(t.db.read_points::<Sample>(car, Source::Live, feb(1), feb(21)).unwrap(), fixed);
}
