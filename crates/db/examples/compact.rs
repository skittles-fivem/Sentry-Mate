//! Runs the series compaction pass on a car of a data folder (a copy: it changes the folder) and checks that no
//! point changed, bit for bit: `compact DATA KEYS CAR [BEFORE_MS] [MAX_DAYS] [GROUP_BYTES]`. Prints the steps, the
//! time the car's lock was held at most, and how long every series reads before and after.
#![allow(clippy::unwrap_used, clippy::print_stdout)]

use db::{Db, Options, Point};
use model::{CarId, ChargePoint, Sample, Stream};
use std::sync::Arc;
use std::time::Instant;

/// A series' points and its days' stamps.
type Series<P> = (Vec<P>, Vec<(u64, u64)>);

fn all<P: Point>(db: &Db, car: CarId) -> Vec<Series<P>> {
    Stream::ALL
        .into_iter()
        .map(|s| (db.read_points::<P>(car, s, 0, u64::MAX).unwrap(), db.day_stamps::<P>(car, s).unwrap()))
        .collect()
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (data, keys) = (std::path::Path::new(&a[1]), std::path::Path::new(&a[2]));
    let mut opts = Options::default();
    if let Some(g) = a.get(6) {
        opts.group_bytes = g.parse().unwrap();
    }
    let open = || Db::open(data, Arc::new(vault::Vault::open(data, keys).unwrap()), opts.clone()).unwrap();
    let car: CarId = a[3].parse().unwrap();
    let before_ms: u64 = a.get(4).map_or(u64::MAX / 2, |b| b.parse().unwrap());
    let max_days: usize = a.get(5).map_or(31, |m| m.parse().unwrap());
    let db = open();
    let t = Instant::now();
    let (samples, curves) = (all::<Sample>(&db, car), all::<ChargePoint>(&db, car));
    let points: usize =
        samples.iter().map(|s| s.0.len()).sum::<usize>() + curves.iter().map(|s| s.0.len()).sum::<usize>();
    println!("{points} points read in {:?} (cold caches of this process)", t.elapsed());
    println!("pending: {}", db.series_pending(car, before_ms).unwrap());
    let (mut steps, mut folded, mut packed, mut again, mut longest) = (0, 0, 0, 0, std::time::Duration::ZERO);
    let t = Instant::now();
    loop {
        let s = Instant::now();
        let c = db.compact_series(car, before_ms, max_days).unwrap();
        longest = longest.max(s.elapsed());
        if std::env::var_os("STEPS").is_some() {
            println!("{:?} {c:?}", s.elapsed());
        }
        steps += 1;
        folded += c.days_folded;
        packed += c.days_packed;
        again += c.days_recoded;
        if !c.more {
            break;
        }
    }
    println!(
        "{steps} steps in {:?} (longest {longest:?}): {folded} days folded, {packed} packed, {again} written again",
        t.elapsed()
    );
    println!("pending: {}", db.series_pending(car, before_ms).unwrap());
    drop(db);
    let db = open();
    let t = Instant::now();
    let (samples2, curves2) = (all::<Sample>(&db, car), all::<ChargePoint>(&db, car));
    println!("read again in {:?} (a new process's caches)", t.elapsed());
    let same = db::segment::same_points::<Sample>;
    assert!(samples.iter().zip(&samples2).all(|(x, y)| same(&x.0, &y.0)), "a sample changed");
    let same = db::segment::same_points::<ChargePoint>;
    assert!(curves.iter().zip(&curves2).all(|(x, y)| same(&x.0, &y.0)), "a charge point changed");
    println!("every point is the same, bit for bit");
    // a second pass has nothing to do and keeps every day's stamp
    let t = Instant::now();
    assert_eq!(db.compact_series(car, before_ms, max_days).unwrap(), db::SeriesCompacted::default());
    println!("a pass with nothing to do: {:?}", t.elapsed());
    let (samples3, curves3) = (all::<Sample>(&db, car), all::<ChargePoint>(&db, car));
    assert!(
        samples2.iter().zip(&samples3).all(|(x, y)| x.1 == y.1)
            && curves2.iter().zip(&curves3).all(|(x, y)| x.1 == y.1)
    );
}
