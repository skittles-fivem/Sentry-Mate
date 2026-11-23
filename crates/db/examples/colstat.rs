//! Measures what each column of a car's stored days takes (docs/plans/storage.md): `colstat DATA KEYS CAR STREAM
//! [samples|charge]`. Prints, per column, how it is encoded, its bytes in the columns and compressed alone.
#![allow(clippy::unwrap_used, clippy::print_stdout)]

use db::segment::{Point, describe, encode};
use db::{Db, Level, Options};
use model::{CarId, ChargePoint, Sample, Stream};
use std::collections::BTreeMap;
use std::sync::Arc;

fn run<P: Point>(db: &Db, car: CarId, stream: Stream) {
    let mut cols: BTreeMap<String, (usize, usize, usize, usize)> = BTreeMap::new();
    let (mut points, mut plain, mut whole, mut times, mut times_c) = (0usize, 0usize, 0usize, 0usize, 0usize);
    let days = db.days::<P>(car, stream).unwrap();
    for d in &days {
        let pts: Vec<P> = db.read_points(car, stream, d.start_ms, d.start_ms + db::DAY_MS).unwrap();
        let body = encode(&pts);
        points += pts.len();
        plain += body.len();
        whole += db::compress(&body, Level::COLD).unwrap().len();
        let (t, cs) = describe::<P>(&body).unwrap();
        times += t;
        times_c += db::compress(&body[..t + 4], Level::COLD).unwrap().len();
        for c in cs {
            let e = cols.entry(format!("{:<28} {}", c.name, c.encoding)).or_default();
            e.0 += c.values;
            e.1 += c.bitmap.len() + c.payload.len();
            e.2 += db::compress(&c.payload, Level::COLD).unwrap().len();
            e.3 += db::compress(&c.bitmap, Level::COLD).unwrap().len();
        }
    }
    let on_disk: u64 = days.iter().map(|d| d.bytes).sum();
    println!("{} days, {points} points, on disk {on_disk}, columns {plain}, whole days cold {whole}", days.len());
    println!("{:<36} {:>9} {:>10} {:>10} {:>9}", "column", "values", "plain", "packed", "bitmap");
    println!("{:<36} {:>9} {:>10} {:>10}", "(times)", points, times, times_c);
    let mut v: Vec<_> = cols.into_iter().collect();
    v.sort_by_key(|(_, e)| std::cmp::Reverse(e.2 + e.3));
    for (k, e) in v {
        println!("{k:<36} {:>9} {:>10} {:>10} {:>9}", e.0, e.1, e.2, e.3);
    }
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (data, keys) = (std::path::Path::new(&a[1]), std::path::Path::new(&a[2]));
    let vault = Arc::new(vault::Vault::open(data, keys).unwrap());
    let db = Db::open(data, vault, Options::default()).unwrap();
    let car: CarId = a[3].parse().unwrap();
    let stream = Stream::ALL.into_iter().find(|s| s.dir_name() == a[4]).unwrap();
    if a.get(5).is_some_and(|k| k == "charge") {
        run::<ChargePoint>(&db, car, stream);
    } else {
        run::<Sample>(&db, car, stream);
    }
}
