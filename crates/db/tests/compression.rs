//! Compression of segment days, measured on realistic synthetic days (see `common`).
//!
//! The quick test checks round trips and sizes. The full measurement (every pure-Rust compressor at several levels,
//! bytes per day, encode and decode speed) runs with:
//!
//! ```text
//! cargo test --release -p db --test compression -- --ignored --nocapture
//! ```
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::{DAY0, RealParked, Shape};
use db::Level;
use model::{ChargePoint, Sample};
use std::io::{Read, Write};
use std::time::{Duration, Instant};

fn days() -> Vec<(&'static str, Vec<u8>, usize)> {
    let real = RealParked::load();
    let mut out = Vec::new();
    for (name, shape) in [
        ("driving day", Shape::Driving),
        ("parked, asleep", Shape::ParkedAsleep),
        ("parked, awake 24 h", Shape::ParkedAwake),
    ] {
        let (samples, points) = common::day(shape, DAY0, 7, real.as_ref());
        out.push((name, db::segment::encode(&samples), samples.len()));
        if shape == Shape::Driving {
            out.push(("charge curve of the driving day", db::segment::encode(&points), points.len()));
        }
    }
    out
}

#[test]
fn realistic_days_round_trip_and_stay_small() {
    // with the real parked minute's values where this machine has it, and with the stand-ins (as in CI)
    let real = RealParked::load();
    let cases = [(Shape::Driving, 120_000), (Shape::ParkedAsleep, 12_000), (Shape::ParkedAwake, 160_000)];
    for ((shape, max_bytes), real) in cases.iter().flat_map(|c| [(*c, real.as_ref()), (*c, None)]) {
        let (samples, points) = common::day(shape, DAY0, 3, real);
        let cols = db::segment::encode(&samples);
        let packed = db::compress(&cols, Level::HOT).unwrap();
        let back: Vec<Sample> = db::segment::decode(&db::decompress(&packed).unwrap()).unwrap();
        assert_eq!(back, samples, "{shape:?}");
        // columns alone are already far smaller than the points as JSON
        let json = serde_json::to_vec(&samples).unwrap().len();
        assert!(cols.len() * 4 < json, "{shape:?}: columns {} vs JSON {json}", cols.len());
        println!("{shape:?}, real values {}: {} bytes at the hot level", real.is_some(), packed.len());
        assert!(packed.len() < max_bytes, "{shape:?}: {} bytes", packed.len());
        let back: Vec<ChargePoint> = db::segment::decode(&db::segment::encode(&points)).unwrap();
        assert_eq!(back, points);
    }
}

type Pack = Box<dyn Fn(&[u8]) -> Vec<u8>>;
type Unpack = Box<dyn Fn(&[u8]) -> Vec<u8>>;

fn brotli(q: u32, lgwin: u32) -> (String, Pack, Unpack) {
    (
        format!("brotli q{q} w{lgwin}"),
        Box::new(move |d| {
            let p = brotli::enc::BrotliEncoderParams {
                quality: q as i32,
                lgwin: lgwin as i32,
                size_hint: d.len(),
                ..Default::default()
            };
            let mut out = Vec::new();
            brotli::BrotliCompress(&mut &d[..], &mut out, &p).unwrap();
            out
        }),
        Box::new(|c| {
            let mut out = Vec::new();
            brotli::Decompressor::new(c, 1 << 16).read_to_end(&mut out).unwrap();
            out
        }),
    )
}

fn codecs() -> Vec<(String, Pack, Unpack)> {
    let mut v = vec![(
        "none (columns only)".to_string(),
        Box::new(|d: &[u8]| d.to_vec()) as Pack,
        Box::new(|c: &[u8]| c.to_vec()) as Unpack,
    )];
    for (q, w) in [(1, 22), (4, 22), (6, 22), (9, 22), (10, 22), (10, 24), (11, 22)] {
        v.push(brotli(q, w));
    }
    v.push((
        "lzma-rs xz".into(),
        Box::new(|d| {
            let mut out = Vec::new();
            lzma_rs::xz_compress(&mut &d[..], &mut out).unwrap();
            out
        }),
        Box::new(|c| {
            let mut out = Vec::new();
            lzma_rs::xz_decompress(&mut &c[..], &mut out).unwrap();
            out
        }),
    ));
    v.push((
        "lz4_flex".into(),
        Box::new(lz4_flex::compress_prepend_size),
        Box::new(|c| lz4_flex::decompress_size_prepended(c).unwrap()),
    ));
    for level in [6u8, 9, 10] {
        v.push((
            format!("miniz_oxide deflate {level}"),
            Box::new(move |d| miniz_oxide::deflate::compress_to_vec(d, level)),
            Box::new(|c| miniz_oxide::inflate::decompress_to_vec(c).unwrap()),
        ));
    }
    v
}

/// Runs `f` until about 300 ms have gone by (at least twice); the time of one run.
fn time(mut f: impl FnMut()) -> Duration {
    let start = Instant::now();
    let mut n = 0u32;
    while n < 2 || start.elapsed() < Duration::from_millis(300) {
        f();
        n += 1;
    }
    start.elapsed() / n
}

#[test]
#[ignore = "a measurement: run with --release -- --ignored --nocapture"]
fn measure_compressors() {
    if cfg!(debug_assertions) {
        println!("(debug build: speeds below mean little; use --release)");
    }
    println!("real parked minute used for parked values: {}", RealParked::load().is_some());
    let codecs = codecs();
    for (name, cols, n) in days() {
        println!("\n{name}: {n} points, columns {} bytes", cols.len());
        println!("{:<28} {:>10} {:>12} {:>12}", "codec", "bytes/day", "encode MB/s", "decode MB/s");
        for (cname, pack, unpack) in &codecs {
            let packed = pack(&cols);
            assert_eq!(unpack(&packed), cols, "{cname}");
            let enc = time(|| {
                std::hint::black_box(pack(&cols));
            });
            let dec = time(|| {
                std::hint::black_box(unpack(&packed));
            });
            let mbs = |d: Duration| cols.len() as f64 / 1e6 / d.as_secs_f64();
            println!("{cname:<28} {:>10} {:>12.1} {:>12.1}", packed.len(), mbs(enc), mbs(dec));
        }
    }
    // the whole read of a day as the db does it: unpack, then decode the columns into points
    let real = RealParked::load();
    println!();
    for (name, shape) in [("driving day", Shape::Driving), ("parked, awake 24 h", Shape::ParkedAwake)] {
        let (samples, _) = common::day(shape, DAY0, 7, real.as_ref());
        let cols = db::segment::encode(&samples);
        for level in [Level::HOT, Level::WARM, Level::COLD] {
            let packed = db::compress(&cols, level).unwrap();
            let read = time(|| {
                let c = db::decompress(&packed).unwrap();
                std::hint::black_box(db::segment::decode::<Sample>(&c).unwrap());
            });
            let write = time(|| {
                std::hint::black_box(db::compress(&db::segment::encode(&samples), level).unwrap());
            });
            let chart = time(|| {
                let c = db::decompress(&packed).unwrap();
                let f = ["speed_kmh", "soc", "battery_power_kw", "outside_temp_c"];
                std::hint::black_box(db::segment::decode_fields::<Sample>(&c, &f).unwrap());
            });
            println!(
                "{name}, {level:?}: {} bytes, write {:.1} ms, read all fields {:.1} ms, read 4 fields {:.1} ms",
                packed.len(),
                write.as_secs_f64() * 1e3,
                read.as_secs_f64() * 1e3,
                chart.as_secs_f64() * 1e3
            );
        }
        let json = serde_json::to_vec(&samples).unwrap();
        let mut w = brotli::CompressorWriter::new(Vec::new(), 1 << 16, 9, 24);
        w.write_all(&json).unwrap();
        println!(
            "{name}: as JSON {} bytes, JSON + brotli 9 {} bytes (columns first: see above)",
            json.len(),
            w.into_inner().len()
        );
    }
}

/// Bytes each field takes in a day (that field alone, columns then brotli at the cold level), and how long decoding
/// takes, to see where a day's bytes and time go.
#[test]
#[ignore = "a measurement: run with --release -- --ignored --nocapture"]
fn measure_columns() {
    let real = RealParked::load();
    for shape in [Shape::Driving, Shape::ParkedAwake] {
        let (samples, _) = common::day(shape, DAY0, 7, real.as_ref());
        let only_times: Vec<Sample> = samples.iter().map(|s| Sample { t_ms: s.t_ms, ..Sample::default() }).collect();
        let base = db::compress(&db::segment::encode(&only_times), Level::COLD).unwrap().len();
        println!("\n{shape:?}: times alone {base} bytes");
        let names = [
            "lat",
            "lon",
            "gps_accuracy_m",
            "elevation_m",
            "speed_kmh",
            "gear",
            "odometer_km",
            "soc",
            "soc_bms",
            "range_km",
            "ideal_range_km",
            "energy_remaining_kwh",
            "battery_voltage_v",
            "battery_current_a",
            "battery_power_kw",
            "battery_temp_c",
            "outside_temp_c",
            "inside_temp_c",
            "charging",
            "charge_kind",
            "charge_power_kw",
            "charger_voltage_v",
            "charger_current_a",
            "total_charge_kwh",
            "total_discharge_kwh",
            "lv_voltage_v",
            "tpms0",
            "tpms1",
            "tpms2",
            "tpms3",
        ];
        for (i, col) in <Sample as db::segment::Point>::columns().iter().enumerate() {
            let one: Vec<Sample> = samples
                .iter()
                .map(|s| {
                    let mut o = Sample { t_ms: s.t_ms, ..Sample::default() };
                    match col {
                        db::segment::Col::F64 { get, set, .. } => {
                            if let Some(v) = get(s) {
                                set(&mut o, v)
                            }
                        }
                        db::segment::Col::U8 { get, set, .. } => {
                            if let Some(v) = get(s) {
                                set(&mut o, v);
                            }
                        }
                    }
                    o
                })
                .collect();
            let cols = db::segment::encode(&one);
            let packed = db::compress(&cols, Level::COLD).unwrap().len();
            println!("{:<22} columns {:>8}  packed {:>7}", names[i], cols.len(), packed.saturating_sub(base));
        }
        let cols = db::segment::encode(&samples);
        let alloc = time(|| {
            std::hint::black_box(vec![Sample::default(); samples.len()]);
        });
        let dec = time(|| {
            std::hint::black_box(db::segment::decode::<Sample>(&cols).unwrap());
        });
        println!("allocating {:.1} ms, decoding columns {:.1} ms", alloc.as_secs_f64() * 1e3, dec.as_secs_f64() * 1e3);
    }
}
