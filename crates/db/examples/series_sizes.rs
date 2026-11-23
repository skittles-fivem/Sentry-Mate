//! Measures ways to store a data folder's time series smaller, losslessly (docs/plans/storage.md, "The stored
//! series"): per stream, what its days take with today's columns and compression and with each candidate, how long
//! a day takes to pack and to read, and what packing a month's days as one stream would give. Reads only; every
//! candidate is decoded again and compared with the points bit for bit.
//!
//! ```bash
//! cargo run --release -p db --example series_sizes -- D/data D/keys            # every stream, the summary
//! cargo run --release -p db --example series_sizes -- D/data D/keys columns    # and each stream's columns
//! cargo run --release -p db --example series_sizes -- D/data D/keys teslamate   # one stream (its folder's name)
//! cargo run --release -p db --example series_sizes -- D/data D/keys odometer teslamate
//! ```
//!
//! The last one asks what a stream's days take if its odometer had other digits than the made-up history gives it
//! (an import's odometer is most of its bytes, and what a real backup's looks like is not known here).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::print_stdout)]

use db::segment::{Tuning, decode, decode_fields, describe, encode, encode_small, encode_tuned, same_points};
use db::{DAY_MS, Db, Level, Point};
use model::{ChargePoint, Sample, Stream};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::sync::Arc;
use std::time::Instant;

const Q10: Level = Level { quality: 10, lgwin: 22 };
const Q11: Level = Level { quality: 11, lgwin: 22 };
const Q11_24: Level = Level { quality: 11, lgwin: 24 };
/// Bytes of columns in a run of days packed together: `db::Options::group_bytes` as it is by default.
const RUN: usize = 2 << 20;

fn brotli(b: &[u8], level: Level) -> Vec<u8> {
    db::compress(b, level).unwrap()
}

/// Raw LZMA at its strongest preset with the longest matches, the smallest of a few literal and position settings.
fn lzma(b: &[u8]) -> Vec<u8> {
    let mut best: Option<Vec<u8>> = None;
    for (lc, lp, pb) in [(3, 0, 2), (0, 0, 0), (0, 3, 3), (3, 0, 0)] {
        let mut o = lzma_rust2::LzmaOptions::with_preset(9);
        (o.lc, o.lp, o.pb, o.nice_len) = (lc, lp, pb, 273);
        let mut w = lzma_rust2::LzmaWriter::new_no_header(Vec::new(), &o, false).unwrap();
        w.write_all(b).unwrap();
        let mut out = w.finish().unwrap();
        out.push((lc * 25 + lp * 5 + pb) as u8);
        if best.as_ref().is_none_or(|x| out.len() < x.len()) {
            best = Some(out);
        }
    }
    best.unwrap()
}

fn unlzma(b: &[u8], len: usize) -> Vec<u8> {
    let (body, code) = b.split_at(b.len() - 1);
    let (lc, lp, pb) = (u32::from(code[0]) / 25, u32::from(code[0]) % 25 / 5, u32::from(code[0]) % 5);
    let dict = lzma_rust2::LzmaOptions::with_preset(9).dict_size;
    let mut r = lzma_rust2::LzmaReader::new(body, len as u64, lc, lp, pb, dict, None).unwrap();
    let mut out = Vec::with_capacity(len);
    r.read_to_end(&mut out).unwrap();
    out
}

/// brotli 11 with `dict` as what came before (a shared dictionary).
fn brotli_dict(data: &[u8], dict: &[u8]) -> usize {
    use brotli::enc::{BrotliEncoderParams, StandardAlloc, interface};
    let params = BrotliEncoderParams { quality: 11, lgwin: 24, size_hint: data.len(), ..Default::default() };
    let mut out = Vec::new();
    let (mut inb, mut outb) = ([0u8; 4096], [0u8; 4096]);
    let mut nop = |_: &mut interface::PredictionModeContextMap<brotli::InputReferenceMut>,
                   _: &mut [interface::StaticCommand],
                   _: interface::InputPair,
                   _: &mut StandardAlloc| ();
    brotli::BrotliCompressCustomIoCustomDict(
        &mut brotli::IoReaderWrapper(&mut &data[..]),
        &mut brotli::IoWriterWrapper(&mut out),
        &mut inb,
        &mut outb,
        &params,
        StandardAlloc::default(),
        &mut nop,
        dict,
        std::io::Error::other("eof"),
    )
    .unwrap();
    out.len() + 1
}

#[derive(Default)]
struct Sum {
    by: BTreeMap<&'static str, u64>,
    order: Vec<&'static str>,
}

impl Sum {
    fn add(&mut self, name: &'static str, n: usize) {
        if !self.by.contains_key(name) {
            self.order.push(name);
        }
        *self.by.entry(name).or_default() += n as u64;
    }
}

/// The calendar month a day is in, as a number that orders them.
fn month_of(day: u64) -> i64 {
    let z = day as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2)) * 12 + m
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

/// A column over a stream's days: how often each encoding was picked, its values, its bytes today and small.
type ColumnSum = (BTreeMap<&'static str, usize>, usize, usize, usize);

fn measure<P: Point>(db: &Db, car: model::CarId, stream: Stream, chart: &[&str], columns: bool) {
    let days = db.days::<P>(car, stream).unwrap();
    if days.is_empty() {
        return;
    }
    let mut sum = Sum::default();
    let (mut points, mut plain_old, mut plain_new) = (0usize, 0usize, 0usize);
    // pack and read times: (total, worst) in ms
    let mut times: BTreeMap<&'static str, (f64, f64)> = BTreeMap::new();
    let mut time = |name: &'static str, t: f64| {
        let e = times.entry(name).or_default();
        (e.0, e.1) = (e.0 + t, e.1.max(t));
    };
    let mut cols: BTreeMap<&'static str, ColumnSum> = BTreeMap::new();
    let mut odd: BTreeMap<&'static str, Vec<usize>> = BTreeMap::new();
    let mut months: BTreeMap<i64, Vec<Vec<P>>> = BTreeMap::new();
    for d in &days {
        let pts: Vec<P> = db.read_points(car, stream, d.start_ms, d.start_ms + DAY_MS).unwrap();
        if pts.is_empty() {
            continue;
        }
        points += pts.len();
        // today's
        let t = Instant::now();
        let old = encode(&pts);
        let old_packed = brotli(&old, Q10);
        time("pack: today's", ms(t));
        plain_old += old.len();
        sum.add("today: quick columns, brotli 10", old_packed.len());
        sum.add("quick columns, brotli 11", brotli(&old, Q11).len());
        sum.add("quick columns, brotli 11 window 24", brotli(&old, Q11_24).len());
        sum.add("quick columns, LZMA 9", lzma(&old).len());
        let t = Instant::now();
        let back: Vec<P> = decode(&db::decompress(&old_packed).unwrap()).unwrap();
        time("read a day: today's", ms(t));
        assert!(same_points(&back, &pts));
        let t = Instant::now();
        decode_fields::<P>(&db::decompress(&old_packed).unwrap(), chart).unwrap();
        time("read a chart's fields: today's", ms(t));
        // each part of the small encoder alone, then all
        let n = Tuning::NONE;
        let parts: [(&'static str, Tuning); 9] = [
            ("no part (as quick), brotli 10", n),
            ("+ miles and °F scales, brotli 10", Tuning { units: true, ..n }),
            ("+ exceptions, brotli 10", Tuning { exceptions: true, ..n }),
            ("+ units and exceptions, brotli 10", Tuning { units: true, exceptions: true, ..n }),
            ("+ deltas of deltas, brotli 10", Tuning { dod: true, ..n }),
            ("+ runs of unchanged values, brotli 10", Tuning { runs: true, ..n }),
            ("+ bits as integer deltas, brotli 10", Tuning { bit_deltas: true, ..n }),
            ("+ byte planes, brotli 10", Tuning { planes: true, ..n }),
            ("+ byte runs, brotli 10", Tuning { byte_runs: true, ..n }),
        ];
        for (name, tuning) in parts {
            let body = encode_tuned(&pts, &tuning);
            assert!(same_points(&decode::<P>(&body).unwrap(), &pts), "{name}");
            sum.add(name, brotli(&body, Q10).len());
        }
        let t = Instant::now();
        let new = encode_small(&pts);
        let encode_ms = ms(t);
        time("pack: small columns alone (no compression)", encode_ms);
        assert!(same_points(&decode::<P>(&new).unwrap(), &pts));
        plain_new += new.len();
        sum.add("small columns (all parts), brotli 10", brotli(&new, Q10).len());
        sum.add("small columns, brotli 11", brotli(&new, Q11).len());
        let t = Instant::now();
        let new_packed = brotli(&new, Q11_24);
        time("pack: small columns, brotli 11 window 24", encode_ms + ms(t));
        sum.add("small columns, brotli 11 window 24", new_packed.len());
        let t = Instant::now();
        let x = lzma(&new);
        time("pack: small columns, LZMA 9 (4 settings tried)", encode_ms + ms(t));
        sum.add("small columns, LZMA 9", x.len());
        sum.add("small columns, the smaller of brotli 11 and LZMA 9", x.len().min(new_packed.len()));
        let t = Instant::now();
        let back: Vec<P> = decode(&db::decompress(&new_packed).unwrap()).unwrap();
        time("read a day: small columns, brotli 11", ms(t));
        assert!(same_points(&back, &pts));
        let t = Instant::now();
        let back: Vec<P> = decode(&unlzma(&x, new.len())).unwrap();
        time("read a day: small columns, LZMA 9", ms(t));
        assert!(same_points(&back, &pts));
        let t = Instant::now();
        decode_fields::<P>(&db::decompress(&new_packed).unwrap(), chart).unwrap();
        time("read a chart's fields: small columns, brotli 11", ms(t));
        if columns {
            let (times_old, was) = describe::<P>(&old).unwrap();
            let (_, now) = describe::<P>(&new).unwrap();
            let e = cols.entry("(times)").or_default();
            e.1 += pts.len();
            e.2 += brotli(&old[..times_old.min(old.len())], Q11).len();
            e.3 += brotli(&new[..times_old.min(new.len())], Q11).len();
            for (a, b) in was.iter().zip(&now) {
                if a.values == 0 {
                    continue;
                }
                let e = cols.entry(a.name).or_default();
                *e.0.entry(a.encoding).or_default() += 1;
                *e.0.entry(b.encoding).or_default() += 1;
                e.1 += a.values;
                e.2 += brotli(&[&a.bitmap[..], &a.payload].concat(), Q11).len();
                e.3 += brotli(&[&b.bitmap[..], &b.payload].concat(), Q11).len();
                if a.encoding == "xor" {
                    // how many digits after the point the values need to be written exactly
                    for c in P::columns().iter().filter(|c| c.name() == a.name) {
                        let digits = odd.entry(a.name).or_default();
                        digits.extend(pts.iter().filter_map(|p| c.value(p)).map(|v| {
                            let text = format!("{v}");
                            text.split_once('.').map_or(0, |(_, frac)| frac.len())
                        }));
                    }
                }
            }
        }
        months.entry(month_of(d.start_ms / DAY_MS)).or_default().push(pts);
    }
    // grouping: the days of a month together
    let (mut month_read, mut month_cols_read, mut group8_read) = ((0.0f64, 0.0f64), (0.0f64, 0.0f64), (0.0f64, 0.0f64));
    let (mut run_pack, mut run_read, mut run_chart) = (0.0f64, (0.0f64, 0.0f64, 0usize), (0.0f64, 0.0f64));
    for days in months.values() {
        let bodies: Vec<Vec<u8>> = days.iter().map(|d| encode_small(d)).collect();
        let all: Vec<u8> = bodies.concat();
        let packed = brotli(&all, Q11_24);
        sum.add("a month's days as one stream, brotli 11 window 24", packed.len());
        let t = Instant::now();
        let plain = db::decompress(&packed).unwrap();
        let first = bodies.first().map_or(0, Vec::len);
        decode::<P>(&plain[..first]).unwrap();
        let took = ms(t);
        month_read = (month_read.0 + took, month_read.1.max(took));
        sum.add("a month's days as one stream, LZMA 9", lzma(&all).len());
        for group in bodies.chunks(8) {
            let packed = brotli(&group.concat(), Q11);
            sum.add("8 days as one stream, brotli 11", packed.len());
            let t = Instant::now();
            let plain = db::decompress(&packed).unwrap();
            decode::<P>(&plain[..group[0].len()]).unwrap();
            let took = ms(t);
            group8_read = (group8_read.0 + took, group8_read.1.max(took));
        }
        // what is built: runs of neighbouring days of about 2 MiB of columns, each compressed as one
        let mut runs: Vec<Vec<&Vec<u8>>> = vec![Vec::new()];
        let mut bytes = 0;
        for b in &bodies {
            if bytes > 0 && bytes + b.len() > RUN {
                runs.push(Vec::new());
                bytes = 0;
            }
            bytes += b.len();
            runs.last_mut().unwrap().push(b);
        }
        for run in runs.iter().filter(|r| !r.is_empty()) {
            let all: Vec<u8> = run.iter().flat_map(|b| b.iter().copied()).collect();
            let t = Instant::now();
            let packed = brotli(&all, Q11);
            run_pack += ms(t);
            sum.add("built: runs of about 2 MiB of small columns, each one stream, brotli 11", packed.len());
            let t = Instant::now();
            let plain = db::decompress(&packed).unwrap();
            decode::<P>(&plain[..run[0].len()]).unwrap();
            let took = ms(t);
            run_read = (run_read.0 + took, run_read.1.max(took), run_read.2 + 1);
            let t = Instant::now();
            let plain = db::decompress(&packed).unwrap();
            decode_fields::<P>(&plain[..run[0].len()], chart).unwrap();
            let took = ms(t);
            run_chart = (run_chart.0 + took, run_chart.1.max(took));
        }
        let together: Vec<P> = days.iter().flatten().cloned().collect();
        let body = encode_small(&together);
        let packed = brotli(&body, Q11_24);
        sum.add("a month's points as one run of columns, brotli 11 window 24", packed.len());
        let t = Instant::now();
        decode::<P>(&db::decompress(&packed).unwrap()).unwrap();
        let took = ms(t);
        month_cols_read = (month_cols_read.0 + took, month_cols_read.1.max(took));
        // each day with the month's first day as its dictionary; the first by itself
        let mut with_dict = bodies.first().map_or(0, |b| brotli(b, Q11_24).len());
        let mut with_prev = with_dict;
        for (i, b) in bodies.iter().enumerate().skip(1) {
            with_dict += brotli_dict(b, &bodies[0]);
            with_prev += brotli_dict(b, &bodies[i - 1]);
        }
        sum.add("each day with the month's first day as dictionary, brotli 11", with_dict);
        sum.add("each day with the day before as dictionary, brotli 11", with_prev);
    }
    let n = days.len() as f64;
    println!("\n## {}.{}: {} days, {} points", P::SERIES, stream.dir_name(), days.len(), points);
    println!("\ncolumns before compression: {plain_old} bytes quick, {plain_new} small\n");
    println!("| candidate | bytes | bytes a car-day | against today |\n|---|---:|---:|---:|");
    let today = sum.by["today: quick columns, brotli 10"] as f64;
    for name in &sum.order {
        let b = sum.by[name] as f64;
        println!("| {name} | {b:.0} | {:.0} | {:+.1} % |", b / n, (b / today - 1.0) * 100.0);
    }
    println!("\n| time, ms | a day on average | the worst day |\n|---|---:|---:|");
    for (name, (total, worst)) in &times {
        println!("| {name} | {:.2} | {worst:.2} |", total / n);
    }
    let m = months.len() as f64;
    for (name, (total, worst)) in [
        ("read a day out of a month as one stream (unpack the month, decode a day)", month_read),
        ("read a month kept as one run of columns", month_cols_read),
    ] {
        println!("| {name} | {:.2} | {worst:.2} |", total / m);
    }
    let groups = months.values().map(|d| d.len().div_ceil(8)).sum::<usize>() as f64;
    println!("| read a day out of 8 as one stream | {:.2} | {:.2} |", group8_read.0 / groups, group8_read.1);
    let runs = run_read.2.max(1) as f64;
    println!("| built: read a day (unpack its run, decode the day) | {:.2} | {:.2} |", run_read.0 / runs, run_read.1);
    println!("| built: read a chart's fields of a day | {:.2} | {:.2} |", run_chart.0 / runs, run_chart.1);
    let small_ms = times.get("pack: small columns alone (no compression)").map_or(0.0, |t| t.0);
    println!("| built: pack (small columns, then the runs at brotli 11) | {:.2} | |", (small_ms + run_pack) / n);
    println!("\n{} runs for {} days", run_read.2, days.len());
    if columns {
        println!("\n| column | values | encodings (days) | bytes today | bytes small | digits after the point |");
        println!("|---|---:|---|---:|---:|---|");
        for (name, (enc, values, a, b)) in &cols {
            let enc: Vec<String> = enc.iter().map(|(e, n)| format!("{e} {n}")).collect();
            let digits = odd.get(name).map_or(String::new(), |d| {
                let mut d = d.clone();
                d.sort_unstable();
                match d.as_slice() {
                    [] => String::new(),
                    d => format!("{} to {}, median {}", d[0], d[d.len() - 1], d[d.len() / 2]),
                }
            });
            println!("| {name} | {values} | {} | {a} | {b} | {digits} |", enc.join(", "));
        }
    }
}

/// An odometer value another way (the second number: noise).
type Shape = fn(f64, u64) -> f64;

/// What a stream's days take, today and built, with its odometer as it is and as it might be in a real history.
fn odometers(db: &Db, car: model::CarId, stream: Stream) {
    const KM_PER_MILE: f64 = 1.609_344;
    let days = db.days::<Sample>(car, stream).unwrap();
    if days.is_empty() {
        return;
    }
    let shapes: [(&str, Shape); 6] = [
        ("as the made-up history has it (any digits, climbing evenly)", |v, _| v),
        ("miles with 6 decimals, times 1.609344", |v, _| (v / KM_PER_MILE * 1e6).round() / 1e6 * KM_PER_MILE),
        ("miles with 1 decimal, times 1.609344", |v, _| (v / KM_PER_MILE * 10.0).round() / 10.0 * KM_PER_MILE),
        ("km with 2 decimals", |v, _| (v * 100.0).round() / 100.0),
        ("km with 6 decimals", |v, _| (v * 1e6).round() / 1e6),
        // the last 20 bits of every value made up: nothing to find in them
        ("any digits, no pattern in the last 20 bits", |v, noise| f64::from_bits(v.to_bits() ^ (noise & 0xf_ffff))),
    ];
    println!("\n## samples.{}: what its odometer's digits cost\n", stream.dir_name());
    println!("| the odometer | today, bytes a car-day | built, bytes a car-day | change |\n|---|---:|---:|---:|");
    let mut all: Vec<Vec<Sample>> = Vec::new();
    for d in &days {
        all.push(db.read_points(car, stream, d.start_ms, d.start_ms + DAY_MS).unwrap());
    }
    for (name, shape) in shapes {
        let mut noise = 0x9E37_79B9_7F4A_7C15u64;
        let (mut today, mut built, mut run, mut run_bytes) = (0usize, 0usize, Vec::new(), 0usize);
        for (i, pts) in all.iter().enumerate() {
            let pts: Vec<Sample> = pts
                .iter()
                .map(|p| {
                    noise ^= noise << 13;
                    noise ^= noise >> 7;
                    noise ^= noise << 17;
                    Sample { odometer_km: p.odometer_km.map(|v| shape(v, noise)), ..p.clone() }
                })
                .collect();
            today += brotli(&encode(&pts), Q10).len();
            let small = encode_small(&pts);
            assert!(same_points(&decode::<Sample>(&small).unwrap(), &pts));
            // runs as they are built: within a month, about 2 MiB of columns
            let month_ends = all.get(i + 1).is_none()
                || month_of(days[i].start_ms / DAY_MS) != month_of(days[i + 1].start_ms / DAY_MS);
            if run_bytes > 0 && run_bytes + small.len() > RUN {
                built += brotli(&run, Q11).len();
                (run, run_bytes) = (Vec::new(), 0);
            }
            run_bytes += small.len();
            run.extend_from_slice(&small);
            if month_ends {
                built += brotli(&run, Q11).len();
                (run, run_bytes) = (Vec::new(), 0);
            }
        }
        let n = days.len();
        println!("| {name} | {} | {} | {:+.1} % |", today / n, built / n, (built as f64 / today as f64 - 1.0) * 100.0);
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (Some(data), Some(keys)) = (args.get(1), args.get(2)) else {
        panic!("usage: series_sizes <data folder> <keys folder> [columns | odometer] [stream ...]");
    };
    let columns = args.iter().any(|a| a == "columns");
    let odometer = args.iter().any(|a| a == "odometer");
    let only: Vec<&String> = args.iter().skip(3).filter(|a| *a != "columns" && *a != "odometer").collect();
    let vault = Arc::new(vault::Vault::open(data.as_ref(), keys.as_ref()).unwrap());
    let db = Db::open(data.as_ref(), vault, db::Options::default()).unwrap();
    let cars = db.fleet(&db::FleetQuery { search: None, limit: 100, after: None }).unwrap().cars;
    for car in cars {
        println!("# car {}", car.car.id);
        for stream in Stream::ALL {
            if !only.is_empty() && !only.iter().any(|o| *o == stream.dir_name()) {
                continue;
            }
            if odometer {
                odometers(&db, car.car.id, stream);
                continue;
            }
            measure::<Sample>(
                &db,
                car.car.id,
                stream,
                &["speed_kmh", "soc", "battery_power_kw", "odometer_km"],
                columns,
            );
            measure::<ChargePoint>(&db, car.car.id, stream, &["power_kw", "soc"], columns);
        }
    }
}
