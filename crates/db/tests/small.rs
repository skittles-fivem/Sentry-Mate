//! Finished days in their smallest form (docs/plans/storage.md, "The stored series"): the small columns give back
//! every bit of every value; what the quick encoder wrote before still reads; a folder an older server left (days
//! and month packs at the cold level) is written again by the compaction pass and reads the same, bit for bit;
//! a crash at every step and a restart lose nothing; a damaged pack is reported with its name and stays as it is.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::*;
use db::segment::{Tuning, decode, decode_fields, describe, encode, encode_small, encode_tuned, same_points};
use db::{Crash, DAY_MS, Db, Error, Options, Point, SeriesCompacted};
use model::{CarId, ChargeKind, ChargePoint, Gear, Sample, Source};
use std::path::{Path, PathBuf};

const KM_PER_MILE: f64 = 1.609_344;

/// Every part of the small encoder alone, none, and all.
fn tunings() -> Vec<Tuning> {
    let n = Tuning::NONE;
    vec![
        n,
        Tuning { units: true, ..n },
        Tuning { exceptions: true, ..n },
        Tuning { dod: true, ..n },
        Tuning { runs: true, ..n },
        Tuning { bit_deltas: true, ..n },
        Tuning { planes: true, ..n },
        Tuning { byte_runs: true, ..n },
        Tuning { units: true, exceptions: true, runs: true, ..n },
        Tuning::ALL,
    ]
}

/// Encodes every way and checks each gives the points back bit for bit, whole and field by field.
fn round_trips<P: Point>(points: &[P], what: &str) {
    let quick = encode(points);
    assert!(same_points(&decode::<P>(&quick).unwrap(), points), "{what}: quick");
    let small = encode_small(points);
    assert!(same_points(&decode::<P>(&small).unwrap(), points), "{what}: small");
    for t in tunings() {
        let body = encode_tuned(points, &t);
        let back = decode::<P>(&body).unwrap_or_else(|e| panic!("{what}: {t:?}: {e}"));
        assert!(same_points(&back, points), "{what}: {t:?}");
    }
    // the columns alone, as a chart reads them
    let names: Vec<&str> = P::columns().iter().map(|c| c.name()).collect();
    let fields = decode_fields::<P>(&small, &names).unwrap();
    assert_eq!(fields.t_ms, points.iter().map(|p| p.t_ms()).collect::<Vec<_>>());
    for (col, (_, values)) in P::columns().iter().zip(&fields.values) {
        let want: Vec<Option<u64>> = points.iter().map(|p| col.value(p).map(f64::to_bits)).collect();
        let got: Vec<Option<u64>> = values.iter().map(|v| v.map(f64::to_bits)).collect();
        assert_eq!(got, want, "{what}: field {}", col.name());
    }
}

/// Floats that are easy to lose: both zeros, infinities, NaNs with their payloads, the smallest and largest.
fn odd_values() -> Vec<f64> {
    vec![
        0.0,
        -0.0,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NAN,
        -f64::NAN,
        f64::from_bits(0x7ff8_0000_0000_0001),
        f64::from_bits(0x7ff0_0000_dead_beef),
        f64::from_bits(0xfff4_0000_0000_0000),
        f64::MIN_POSITIVE,
        f64::from_bits(1),
        -f64::from_bits(0x000f_ffff_ffff_ffff),
        f64::MAX,
        f64::MIN,
        f64::EPSILON,
        1e-300,
        9_007_199_254_740_993.0,
        -2.5,
    ]
}

#[test]
fn the_small_columns_give_back_every_bit_of_odd_values() {
    round_trips::<Sample>(&[], "no points");
    round_trips::<ChargePoint>(&[], "no points");
    round_trips(&[Sample { t_ms: 7, ..Sample::default() }], "one point with nothing");
    round_trips(&[Sample { t_ms: u64::MAX / 2, soc: Some(-0.0), gear: Some(Gear::R), ..Sample::default() }], "one");
    round_trips(&[ChargePoint { t_ms: 0, power_kw: Some(f64::NAN), ..ChargePoint::default() }], "one NaN");
    let odd = odd_values();
    // every odd value in every float field, alone in its column and among ordinary values
    for among in [0usize, 1, 40] {
        let mut pts: Vec<Sample> = Vec::new();
        for (i, &v) in odd.iter().enumerate() {
            for k in 0..=among {
                let x = if k == 0 { v } else { 20.0 + (i * among + k) as f64 * 0.5 };
                pts.push(Sample {
                    t_ms: pts.len() as u64 * 1000,
                    lat: Some(x),
                    odometer_km: Some(x),
                    range_km: Some(if k == 0 { v } else { (200.0 + k as f64) * KM_PER_MILE }),
                    soc: (k % 3 != 1).then_some(x),
                    battery_voltage_v: Some(x),
                    battery_current_a: Some(2.0),
                    battery_power_kw: Some(x * 2.0 / 1000.0),
                    tpms_bar: [Some(x), None, Some(-x), (k == 0).then_some(x)],
                    ..Sample::default()
                });
            }
        }
        round_trips(&pts, &format!("odd values among {among}"));
        let curve: Vec<ChargePoint> = pts
            .iter()
            .map(|s| ChargePoint {
                t_ms: s.t_ms,
                power_kw: s.lat,
                soc: s.soc,
                range_km: s.range_km,
                ..Default::default()
            })
            .collect();
        round_trips(&curve, &format!("odd values among {among}, a curve"));
    }
    // times of any kind: far apart, the largest
    let times = [0, 1, 2, 1000, 1_794_787_200_000, 1_794_787_200_001, u64::MAX - 1, u64::MAX];
    let pts: Vec<Sample> = times.iter().map(|&t| Sample { t_ms: t, soc: Some(50.0), ..Sample::default() }).collect();
    round_trips(&pts, "odd times");
}

/// One made-up column of `n` values of a random kind.
fn random_column(r: &mut Rng, n: usize) -> Vec<Option<f64>> {
    let kind = r.u64() % 12;
    let k = (r.u64() % 7) as i32;
    let odd = odd_values();
    let mut walk = r.f64() * 1000.0;
    let present = r.u64() % 4;
    (0..n)
        .map(|i| {
            walk += r.normal();
            let v = match kind {
                // a decimal column, a DBC factor's, a factor found in the data
                0 => (walk * 10f64.powi(k)).round() / 10f64.powi(k),
                1 => (walk * 100.0).round() * 0.01,
                2 => (walk * 30.0).round() * 0.0333,
                // miles and °F as the inputs convert them
                3 => (walk * 10f64.powi(k)).round() / 10f64.powi(k) * KM_PER_MILE,
                4 => ((walk * 10.0).round() / 10.0 - 32.0) * 5.0 / 9.0,
                // any digits: a ramp, noise
                5 => 12_000.0 + i as f64 * 0.013_777_1,
                6 => walk,
                // mostly one value
                7 => (i / 50) as f64,
                8 => 21.5,
                // a scaled column with odd values in it
                9 if r.u64().is_multiple_of(40) => *r.pick(&odd),
                9 => (walk * 10.0).round() / 10.0,
                // only odd values; whole numbers and their negative zero
                10 => *r.pick(&odd),
                _ if r.u64().is_multiple_of(5) => -0.0,
                _ => walk.round(),
            };
            match present {
                0 => Some(v),
                1 => (!r.u64().is_multiple_of(10)).then_some(v),
                2 => (i % 60 == 0).then_some(v),
                _ => (r.u64().is_multiple_of(2)).then_some(v),
            }
        })
        .collect()
}

#[test]
fn random_days_come_back_bit_for_bit() {
    let mut r = Rng::new(20_261_123);
    for round in 0..120 {
        let n = match round % 6 {
            0 => (r.u64() % 4) as usize,
            1..=3 => (r.u64() % 200) as usize,
            _ => (r.u64() % 3000) as usize,
        };
        let mut t = 1_794_787_200_000u64;
        let mut pts: Vec<Sample> = (0..n)
            .map(|_| {
                t += [1000, 1000, 1000, 5000, 30_000, 60_000, 1][(r.u64() % 7) as usize] + r.u64() % 3;
                Sample {
                    t_ms: t,
                    gear: (!r.u64().is_multiple_of(9)).then_some(*r.pick(&[
                        Gear::P,
                        Gear::P,
                        Gear::P,
                        Gear::D,
                        Gear::R,
                        Gear::N,
                    ])),
                    charging: (r.u64().is_multiple_of(3)).then_some(r.u64().is_multiple_of(7)),
                    charge_kind: (r.u64().is_multiple_of(50)).then_some(*r.pick(&[ChargeKind::Ac, ChargeKind::Dc])),
                    ..Sample::default()
                }
            })
            .collect();
        for col in Sample::columns() {
            if let db::segment::Col::F64 { set, .. } = col {
                if r.u64().is_multiple_of(5) {
                    continue;
                }
                for (p, v) in pts.iter_mut().zip(random_column(&mut r, n)) {
                    if let Some(v) = v {
                        set(p, v);
                    }
                }
            }
        }
        // sometimes the power is exactly what V and I give
        if round % 4 == 0 {
            for p in pts.iter_mut() {
                p.battery_power_kw = p.battery_voltage_v.zip(p.battery_current_a).map(|(v, a)| v * a / 1000.0);
            }
        }
        round_trips(&pts, &format!("round {round}"));
        let curve: Vec<ChargePoint> = pts
            .iter()
            .map(|s| ChargePoint {
                t_ms: s.t_ms,
                power_kw: s.charge_power_kw,
                voltage_v: s.charger_voltage_v,
                current_a: s.charger_current_a,
                soc: s.soc,
                battery_temp_c: s.battery_temp_c,
                range_km: s.range_km,
            })
            .collect();
        round_trips(&curve, &format!("round {round}, a curve"));
    }
}

fn encoding_of<P: Point>(body: &[u8], name: &str) -> &'static str {
    describe::<P>(body).unwrap().1.iter().find(|c| c.name == name).unwrap().encoding
}

#[test]
fn miles_fahrenheit_and_columns_with_odd_values_stay_scaled() {
    // a day as the Tesla API gives it: miles at its own digits, times the km in a mile; a power of -0.0 now and then
    let mut pts: Vec<Sample> = (0..2000u64)
        .map(|i| Sample {
            t_ms: i * 1000,
            speed_kmh: Some((30 + i % 40) as f64 * KM_PER_MILE),
            odometer_km: Some((10_000_000_000 + i * 7919) as f64 / 1e6 * KM_PER_MILE),
            range_km: Some((25_000 - i / 10) as f64 / 100.0 * KM_PER_MILE),
            battery_power_kw: Some(if i % 97 == 0 { -0.0 } else { (i % 30) as f64 }),
            outside_temp_c: Some((((500 + i / 100) as f64 / 10.0) - 32.0) * 5.0 / 9.0),
            soc: Some(70.0),
            ..Sample::default()
        })
        .collect();
    pts[1234].soc = Some(f64::NAN);
    let (quick, small) = (encode(&pts), encode_small(&pts));
    for name in ["odometer_km", "range_km", "battery_power_kw", "outside_temp_c", "soc"] {
        assert_eq!(encoding_of::<Sample>(&quick, name), "xor", "{name}: the quick encoder keeps the bits");
    }
    for name in ["speed_kmh", "odometer_km", "range_km"] {
        assert!(encoding_of::<Sample>(&small, name).starts_with("miles"), "{name}");
    }
    assert!(encoding_of::<Sample>(&small, "outside_temp_c").starts_with("fahrenheit"));
    assert_eq!(encoding_of::<Sample>(&small, "battery_power_kw"), "scaled, exceptions");
    assert_eq!(encoding_of::<Sample>(&small, "soc"), "scaled, exceptions");
    assert!(same_points(&decode::<Sample>(&small).unwrap(), &pts));
    let packed = |b: &[u8]| db::compress(b, db::Level::SMALL).unwrap().len();
    assert!(packed(&small) * 3 < packed(&quick), "{} -> {}", packed(&quick), packed(&small));
    // an odometer with any digits that climbs evenly: its bits as integers
    let ramp: Vec<Sample> = (0..2000u64)
        .map(|i| Sample { t_ms: i * 1000, odometer_km: Some(12_000.0 + i as f64 * 0.013_777_1), ..Sample::default() })
        .collect();
    let (quick, small) = (encode(&ramp), encode_small(&ramp));
    assert!(encoding_of::<Sample>(&small, "odometer_km").starts_with("bits"));
    assert!(packed(&small) * 2 < packed(&quick), "{} -> {}", packed(&quick), packed(&small));
}

#[test]
fn a_small_body_cut_or_changed_is_refused_or_read_never_a_panic() {
    let mut r = Rng::new(5);
    let pts: Vec<Sample> = (0..300u64)
        .map(|i| Sample {
            t_ms: i * 1000,
            range_km: Some((250 - i / 10) as f64 * KM_PER_MILE),
            soc: Some(if i == 100 { f64::NAN } else { 60.0 }),
            odometer_km: Some(12_000.0 + i as f64 * 0.013_777_1),
            ideal_range_km: Some(r.normal()),
            gear: Some(if i < 200 { Gear::D } else { Gear::P }),
            speed_kmh: (i % 2 == 0).then_some((i / 30) as f64),
            ..Sample::default()
        })
        .collect();
    let body = encode_small(&pts);
    for cut in 0..body.len() {
        assert!(decode::<Sample>(&body[..cut]).is_err(), "cut at {cut}");
    }
    for at in 0..body.len() {
        for bit in [1u8, 0x10, 0x80] {
            let mut bad = body.clone();
            bad[at] ^= bit;
            // (a changed value can still be a value: the packs' hashes and the vault catch that)
            let _ = decode::<Sample>(&bad);
            let _ = decode_fields::<Sample>(&bad, &["soc", "range_km", "gear"]);
        }
    }
}

// ---- what the quick encoder wrote before there was a small one ----

/// Made-up points with every kind of column the quick encoder writes: decimals, a DBC factor, a factor found in the
/// data, raw bits (a conversion from miles, a third, a NaN and a -0.0), power worked out from V and I, fields that
/// are there only sometimes, the byte columns.
fn fixture_samples() -> Vec<Sample> {
    (0..16u64)
        .map(|i| {
            let v = 400.0 + (i % 7) as f64 * 0.01;
            let a = (i % 5) as f64 * 0.1 - 2.0;
            Sample {
                t_ms: 1_794_787_200_000 + i * 1000 + if i > 9 { 120_000 } else { 0 },
                lat: Some((12_301_000.0 + i as f64 * 17.0) / 1e6),
                lon: Some((-45_610_000.0 - i as f64 * 31.0) / 1e6),
                elevation_m: Some(40.0 + (i % 3) as f64),
                speed_kmh: Some((4014.0 + i as f64 * 3.0) * 0.08 - 40.0),
                gear: Some([Gear::P, Gear::D, Gear::D, Gear::R, Gear::N][(i % 5) as usize]),
                odometer_km: Some((12_000_000.0 + i as f64 * 137.0) * 0.001),
                soc: Some(71.0 - (i / 6) as f64),
                range_km: Some((256.0 - (i / 4) as f64) * 1.609_344),
                ideal_range_km: Some((i as f64 + 1.0) / 3.0),
                battery_voltage_v: Some(v),
                battery_current_a: Some(a),
                battery_power_kw: Some(v * a / 1000.0),
                outside_temp_c: match i % 4 {
                    0 => Some(-0.0),
                    1 => Some(f64::NAN),
                    2 => Some(12.5),
                    _ => None,
                },
                charging: (i % 2 == 0).then_some(i % 4 == 0),
                charge_kind: (i % 8 == 0).then_some(if i == 0 { ChargeKind::Ac } else { ChargeKind::Dc }),
                lv_voltage_v: Some((28_859.0 + (i % 3) as f64) * 0.000_544),
                tpms_bar: [Some(2.9), None, (i == 3).then_some(116.0 * 0.025), Some(3.0)],
                ..Sample::default()
            }
        })
        .collect()
}

fn fixture_curve() -> Vec<ChargePoint> {
    (0..8u64)
        .map(|i| ChargePoint {
            t_ms: 1_794_787_200_000 + i * 15_000,
            power_kw: Some((1100.0 - i as f64) * 0.01),
            voltage_v: Some(390.0 + i as f64 * 0.5),
            current_a: Some(16.0),
            soc: Some(40.0 + (i / 3) as f64 * 0.5),
            battery_temp_c: (i % 3 == 0).then_some(25.5),
            range_km: None,
        })
        .collect()
}

/// The columns of [`fixture_samples`] and [`fixture_curve`] as the encoder of 2026-11-22 wrote them (commit
/// cf81f68, before there were other encodings than XOR, DIV, MUL, FACTOR and RAW).
const OLD_SAMPLES: &str = concat!(
    "1080b0ba9abc68d00f000000000000000080d30effd20e000000001e010101061390cbdd0b22222222222222222222222222",
    "222202010106139fd0bf2b3d3d3d3d3d3d3d3d3d3d3d3d3d3d3d030004010100105002020302020302020302020302020305",
    "01020212a0b7033030303030303030303030303030300601001001040402030104040203010404020301070101032280ecb8",
    "0b92029202920292029202920292029202920292029202920292029202920208010100118e01000000000001000000000001",
    "00000009000a010209218080a0cafd17000000ff8fe5fe0b000000ff8fe5fe0b000000ff8fe5fe0b0000000b01007c010f09",
    "090901090909090909090909093fd55555555555553015555555555555055555555555550ffffffffffffe7ffaaaaaaaaaaa",
    "ab02aaaaaaaaaaab07fffffffffffe0d55555555555502aaaaaaaaaaab07fffffffffffe1d55555555555501555555555555",
    "03fffffffffffe06aaaaaaaaaaab015555555555550c000d0101021280f1040202020202020b0202020202020b020e010101",
    "10270202020207020202020702020202070f03100011027777002308070707070707070707070780fff83fd1c029fff83fd1",
    "c029fff83fd1c029fff83fd1120013025555000801000100010001001402010100020102150016001700180019001a010106",
    "22c0b5fc0ec008c008ff10c008c008ff10c008c008ff10c008c008ff10c008c008ff101b010101103a000000000000000000",
    "0000000000001c001d0208000201013a1e0101001006000000000000000000000000000000",
);
const OLD_CURVE: &str = concat!(
    "0880b0ba9abc68b0ea010000000000000601010202099811010101010101010201010109f83c0a0a0a0a0a0a0a0301010008",
    "20000000000000000401010109a00600000a00000a00050249010104fe0300000600",
);

#[test]
fn bodies_written_before_there_was_a_small_encoder_still_read() {
    let (samples, curve) = (hex::decode(OLD_SAMPLES).unwrap(), hex::decode(OLD_CURVE).unwrap());
    assert!(same_points(&decode::<Sample>(&samples).unwrap(), &fixture_samples()));
    assert!(same_points(&decode::<ChargePoint>(&curve).unwrap(), &fixture_curve()));
    // days still being written are encoded as they always were, byte for byte
    assert_eq!(encode(&fixture_samples()), samples);
    assert_eq!(encode(&fixture_curve()), curve);
    // and the small encoder makes the same points of them
    let small = encode_small(&decode::<Sample>(&samples).unwrap());
    assert_ne!(small, samples);
    assert!(same_points(&decode::<Sample>(&small).unwrap(), &fixture_samples()));
}

// ---- a folder an older server left, written again by the pass ----

const H: u64 = 3_600_000;
/// 2024-02-01 00:00 UTC, 2024-03-01, and a time in April (February and March are over then).
const FEB1: u64 = 1_706_745_600_000;
const MAR1: u64 = FEB1 + 29 * DAY_MS;
const APR10: u64 = MAR1 + 40 * DAY_MS;

/// As servers before the small form wrote (and nothing folds by itself: the tests decide when).
fn old() -> Options {
    Options { fold_min_bytes: 1 << 30, small_columns: false, ..Options::default() }
}

fn new() -> Options {
    Options { fold_min_bytes: 1 << 30, ..Options::default() }
}

/// The small form with each day by itself in its month's pack.
fn new_alone() -> Options {
    Options { group_bytes: 0, ..new() }
}

fn seg(t: &TestDb, car: CarId, series: &str) -> PathBuf {
    t.data.join("db/cars").join(car.to_string()).join("seg").join(series)
}

fn names(dir: &Path, ending: &str) -> Vec<String> {
    let mut v: Vec<String> = match std::fs::read_dir(dir) {
        Ok(rd) => rd.map(|e| e.unwrap().file_name().to_string_lossy().to_string()).collect(),
        Err(_) => Vec::new(),
    };
    v.retain(|n| n.ends_with(ending));
    v.sort();
    v
}

fn opened(t: &TestDb, file: &Path) -> Vec<u8> {
    let label = file.strip_prefix(&t.data).unwrap().to_string_lossy().replace('\\', "/");
    t.vault.open_sealed(&label, &std::fs::read(file).unwrap()).unwrap().to_vec()
}

/// The level of every day a series' folder holds: of its own snapshots, and of its packs' days.
fn levels(t: &TestDb, dir: &Path) -> (Vec<u8>, Vec<u8>) {
    let loose: Vec<u8> = names(dir, ".snap").iter().map(|n| opened(t, &dir.join(n))[15]).collect();
    let mut packed = Vec::new();
    for n in names(dir, ".pack") {
        let p = opened(t, &dir.join(n));
        packed.extend((0..p[7] as usize).map(|i| p[8 + i * 25 + 12]));
    }
    (loose, packed)
}

/// Whether every day is as small as it goes: a day of its own in the small form (2), a packed day together with
/// its neighbours (3 the first of a run, 4 another) or, where days are not packed together, in the small form.
fn all_small(t: &TestDb, dir: &Path, together: bool) -> bool {
    let (loose, packed) = levels(t, dir);
    loose.iter().all(|l| *l == 2) && packed.iter().all(|l| if together { *l == 3 || *l == 4 } else { *l == 2 })
}

fn bytes_of(dir: &Path) -> u64 {
    std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().metadata().unwrap().len()).sum()
}

/// A made-up day as the Tesla API gives one (miles times the km in a mile, a -0.0, a NaN): `n` samples from 01:00.
fn api_day(day_ms: u64, n: u64) -> Vec<Sample> {
    (0..n)
        .map(|i| Sample {
            t_ms: day_ms + H + i * 1000,
            lat: Some((12_301_000 + i * 3) as f64 / 1e6),
            lon: Some(-45.61),
            speed_kmh: Some((20 + i % 50) as f64 * KM_PER_MILE),
            odometer_km: Some((9_000_000_000 + day_ms / 1000 + i * 4001) as f64 / 1e6 * KM_PER_MILE),
            range_km: Some((25_000 - i / 7) as f64 / 100.0 * KM_PER_MILE),
            ideal_range_km: Some(12_000.0 + i as f64 * 0.013_777_1),
            battery_power_kw: Some(if i % 31 == 0 { -0.0 } else { (i % 12) as f64 }),
            outside_temp_c: (i % 60 == 0).then_some(if i == 120 { f64::NAN } else { 11.5 }),
            soc: Some(70.0 - (i / 400) as f64),
            gear: Some(if i % 500 < 400 { Gear::D } else { Gear::P }),
            ..Sample::default()
        })
        .collect()
}

fn curve(day_ms: u64, n: u64) -> Vec<ChargePoint> {
    (0..n)
        .map(|i| ChargePoint {
            t_ms: day_ms + 2 * H + i * 10_000,
            power_kw: Some(dbc(11.0 - i as f64 * 0.01, 0.01, 0.0)),
            voltage_v: Some(390.0),
            current_a: Some(16.0),
            soc: Some(40.0 + (i / 20) as f64 * 0.5),
            battery_temp_c: (i % 3 == 0).then_some(25.5),
            range_km: Some((200.0 + i as f64) * KM_PER_MILE),
        })
        .collect()
}

/// Every point of both series, as read.
struct Reads {
    samples: Vec<Sample>,
    curve: Vec<ChargePoint>,
    one_day: Vec<Sample>,
}

fn reads(db: &Db, car: CarId) -> Reads {
    Reads {
        samples: db.read_points(car, Source::Live, 0, u64::MAX).unwrap(),
        curve: db.read_points(car, Source::Live, 0, u64::MAX).unwrap(),
        one_day: db.read_points(car, Source::Live, FEB1 + 4 * DAY_MS, FEB1 + 5 * DAY_MS).unwrap(),
    }
}

fn assert_same(a: &Reads, b: &Reads, when: &str) {
    assert!(!a.samples.is_empty() && !a.curve.is_empty() && !a.one_day.is_empty());
    assert!(same_points(&a.samples, &b.samples), "{when}: samples");
    assert!(same_points(&a.curve, &b.curve), "{when}: the curve");
    assert!(same_points(&a.one_day, &b.one_day), "{when}: a day");
}

/// What an older server left: February (packed), the first days of March (packed) and April (days of their own,
/// its month not over), all cold in the quick columns; one day with a late window still in its journal.
fn old_folder() -> (TestDb, CarId) {
    let t = test_db(old());
    let car = t.db.create_car(new_car("Model Y", &[])).unwrap().id;
    for d in [0, 1, 4, 5, 12, 27, 28, 29, 30, 60, 61] {
        let day = FEB1 + d * DAY_MS;
        let pts = api_day(day, 300 + d * 40);
        t.db.write_points(car, Source::Live, day, day + DAY_MS, &pts).unwrap();
        if d % 4 == 0 {
            t.db.write_points(car, Source::Live, day, day + DAY_MS, &curve(day, 30 + d)).unwrap();
        }
    }
    loop {
        let c = t.db.compact_series(car, APR10, 5).unwrap();
        assert_eq!(c.days_recoded, 0);
        if !c.more {
            break;
        }
    }
    // a window written into a packed day after that
    let late = api_day(FEB1 + 5 * DAY_MS + 10 * H, 20);
    t.db.write_points(car, Source::Live, late[0].t_ms, late[0].t_ms + 60_000, &late).unwrap();
    let sdir = seg(&t, car, "samples.can");
    assert_eq!(names(&sdir, ".pack"), ["2024-02.pack", "2024-03.pack"]);
    assert_eq!(names(&sdir, ".snap"), ["2024-04-01.snap", "2024-04-02.snap"]);
    assert_eq!(names(&sdir, ".log"), ["2024-02-06.log"]);
    let (loose, packed) = levels(&t, &sdir);
    assert!(loose.iter().chain(&packed).all(|l| *l == 1), "every day is cold, as before");
    (t, car)
}

/// Runs the pass to its end, a few days a step; what it did in all.
fn pass(db: &Db, car: CarId, before: u64) -> SeriesCompacted {
    let mut total = SeriesCompacted::default();
    for _ in 0..1000 {
        let c = db.compact_series(car, before, 3).unwrap();
        total.days_folded += c.days_folded;
        total.days_packed += c.days_packed;
        total.days_recoded += c.days_recoded;
        if !c.more {
            assert_eq!(db.series_pending(car, before).unwrap(), 0, "nothing is left after the last step");
            return total;
        }
        assert!(c.days_folded + c.days_packed + c.days_recoded > 0, "a step that says more did something");
    }
    panic!("the pass never ends");
}

/// An older server's folder opened by the new one and passed over: `together`, with a month's days packed
/// together; else each by itself.
fn written_again(together: bool) {
    let opts = || if together { new() } else { new_alone() };
    let (mut t, car) = old_folder();
    let (sdir, cdir) = (seg(&t, car, "samples.can"), seg(&t, car, "charge.can"));
    let before = reads(&t.db, car);
    let (sample_bytes, curve_bytes) = (bytes_of(&sdir), bytes_of(&cdir));
    // the new server reads it as it is
    t.reopen(opts());
    assert_same(&reads(&t.db, car), &before, "opened by the new server");
    assert!(t.db.series_pending(car, APR10).unwrap() >= 11, "the days wait to be written again");
    let done = pass(&t.db, car, APR10);
    // April's days (two of samples, one curve) and the day with a journal were folded, that day packed again, and
    // the packs' days written again: 8 of samples and 4 curves by themselves in the small form, and then all 9 and
    // 4 together with their neighbours
    let again = if together { 12 + 13 } else { 12 };
    assert_eq!((done.days_folded, done.days_packed, done.days_recoded), (4, 1, again), "{done:?}");
    assert!(all_small(&t, &sdir, together), "every day is as small as it goes: {:?}", levels(&t, &sdir));
    assert!(all_small(&t, &cdir, together));
    assert!(names(&sdir, ".log").is_empty() && names(&sdir, ".tmp").is_empty());
    assert_same(&reads(&t.db, car), &before, "after the pass");
    assert!(bytes_of(&sdir) * 2 < sample_bytes, "samples: {sample_bytes} -> {} bytes", bytes_of(&sdir));
    assert!(bytes_of(&cdir) < curve_bytes, "curves: {curve_bytes} -> {} bytes", bytes_of(&cdir));
    // a pass again has nothing to do, and touches no file; so after a restart
    let files = |dir: &Path| -> Vec<(String, std::time::SystemTime)> {
        let modified = |n: &String| std::fs::metadata(dir.join(n)).unwrap().modified().unwrap();
        names(dir, "").into_iter().map(|n| (n.clone(), modified(&n))).collect()
    };
    let was = files(&sdir);
    assert_eq!(t.db.compact_series(car, APR10, 1000).unwrap(), SeriesCompacted::default());
    t.reopen(opts());
    assert_eq!(t.db.series_pending(car, APR10).unwrap(), 0);
    assert_eq!(t.db.compact_series(car, APR10, 1000).unwrap(), SeriesCompacted::default());
    assert_eq!(files(&sdir), was);
    assert_same(&reads(&t.db, car), &before, "after a restart");
    // a write into a day written again lands, and the next pass takes it into the pack again (the days it was
    // packed together with stand by themselves for a moment, and are packed together again: February's 7)
    let late = api_day(FEB1 + 12 * DAY_MS + 9 * H, 15);
    t.db.write_points(car, Source::Live, late[0].t_ms, late[0].t_ms + 60_000, &late).unwrap();
    let with_late = reads(&t.db, car);
    assert_eq!(with_late.samples.len(), before.samples.len() + 15);
    let done = pass(&t.db, car, APR10);
    assert_eq!((done.days_folded, done.days_packed, done.days_recoded), (1, 1, if together { 7 } else { 0 }));
    assert!(all_small(&t, &sdir, together), "{:?}", levels(&t, &sdir));
    t.reopen(opts());
    assert_same(&reads(&t.db, car), &with_late, "after a late write");
    // and an older server's options still read what the new one wrote (the same program, the old way of writing)
    t.reopen(old());
    assert_same(&reads(&t.db, car), &with_late, "read the old way");
    assert_eq!(t.db.series_pending(car, APR10).unwrap(), 0);
}

#[test]
fn a_folder_an_older_server_left_is_written_again_small_and_reads_the_same() {
    written_again(true);
}

#[test]
fn a_folder_an_older_server_left_is_written_again_small_with_each_day_by_itself() {
    written_again(false);
}

/// A pack read by the format its docs give: per day (day number, generation, level, length), and the pack's version.
fn pack_index(t: &TestDb, file: &Path) -> (u16, Vec<(u64, u64, u8, usize)>) {
    let p = opened(t, file);
    assert_eq!(&p[..5], b"SMDB\x0b");
    let entries = (0..p[7] as usize)
        .map(|i| {
            let e = &p[8 + i * 25..8 + (i + 1) * 25];
            let day = u32::from_be_bytes(e[..4].try_into().unwrap()) as u64;
            let len = u32::from_be_bytes(e[13..17].try_into().unwrap()) as usize;
            (day, u64::from_be_bytes(e[4..12].try_into().unwrap()), e[12], len)
        })
        .collect();
    (u16::from_be_bytes([p[5], p[6]]), entries)
}

#[test]
fn days_packed_together_take_late_writes_new_days_and_emptied_days() {
    // runs of about 30 KB of columns, so February is several
    let opts = || Options { group_bytes: 30_000, ..new() };
    let mut t = test_db(opts());
    let car = t.db.create_car(new_car("Model Y", &[])).unwrap().id;
    let sdir = seg(&t, car, "samples.can");
    let day = |d: u64| FEB1 + (d - 1) * DAY_MS;
    let mut want: std::collections::BTreeMap<u64, Vec<Sample>> = std::collections::BTreeMap::new();
    for d in [1, 2, 3, 4, 7, 8, 9, 10, 11, 20, 21, 22] {
        let pts = api_day(day(d), 500 + d * 20);
        t.db.write_points(car, Source::Live, day(d), day(d) + DAY_MS, &pts).unwrap();
        want.insert(d, pts);
    }
    // one day too large for a run with others
    let big = api_day(day(5), 9000);
    t.db.write_points(car, Source::Live, day(5), day(5) + DAY_MS, &big).unwrap();
    want.insert(5, big);
    let check = |t: &TestDb, want: &std::collections::BTreeMap<u64, Vec<Sample>>, when: &str| {
        for (d, pts) in want {
            let read: Vec<Sample> = t.db.read_points(car, Source::Live, day(*d), day(*d) + DAY_MS).unwrap();
            assert!(same_points(&read, pts), "{when}: day {d}");
            let f = t.db.read_fields::<Sample>(car, Source::Live, day(*d), day(*d) + DAY_MS, &["range_km"]).unwrap();
            assert_eq!(f.values[0].1.len(), pts.len(), "{when}: day {d}, a field");
        }
        let all: Vec<Sample> = t.db.read_points(car, Source::Live, 0, u64::MAX).unwrap();
        assert_eq!(all.len(), want.values().map(Vec::len).sum::<usize>(), "{when}");
        let days: Vec<String> = t.db.days::<Sample>(car, Source::Live).unwrap().into_iter().map(|d| d.date).collect();
        // (a day emptied since it was packed is listed until the pass that drops it from the pack)
        let emptied = usize::from(!want.contains_key(&2) && names(&sdir, ".snap").contains(&"2024-02-02.snap".into()));
        assert_eq!(days.len(), want.len() + emptied, "{when}: {days:?}");
    };
    pass(&t.db, car, APR10);
    check(&t, &want, "packed together");
    let (version, index) = pack_index(&t, &sdir.join("2024-02.pack"));
    assert_eq!(version, 2);
    let firsts = index.iter().filter(|e| e.2 == 3).count();
    assert!(index.iter().all(|e| e.2 == 3 || e.2 == 4) && (3..13).contains(&firsts), "{index:?}");
    // the large day is a run of its own
    let at = index.iter().position(|e| e.0 == day(5) / DAY_MS).unwrap();
    assert!(index[at].2 == 3 && index.get(at + 1).is_none_or(|e| e.2 == 3), "{index:?}");
    // a day's size is its share of its run; the days of a run have the same
    let sizes = t.db.days::<Sample>(car, Source::Live).unwrap();
    assert!(sizes.iter().all(|d| d.bytes > 0 && d.bytes < 30_000));
    t.reopen(opts());
    check(&t, &want, "after a restart");

    // a late window into a day of a run; a day that was not there, between a run's days; a day of a run emptied
    let late = api_day(day(8) + 13 * H, 40);
    t.db.write_points(car, Source::Live, late[0].t_ms, late[0].t_ms + 60_000, &late).unwrap();
    want.get_mut(&8).unwrap().extend(late);
    let between = api_day(day(21) + 2 * H, 7);
    t.db.write_points(car, Source::Live, day(21), day(22), &between).unwrap();
    want.insert(21, between);
    let new_day = api_day(day(15), 300);
    t.db.write_points(car, Source::Live, day(15), day(16), &new_day).unwrap();
    want.insert(15, new_day);
    t.db.write_points::<Sample>(car, Source::Live, day(2), day(3), &[]).unwrap();
    want.remove(&2);
    check(&t, &want, "written after packing");
    t.reopen(opts());
    check(&t, &want, "written after packing, after a restart");
    let done = pass(&t.db, car, APR10);
    assert!(done.days_packed >= 4 && done.days_recoded >= 4, "{done:?}");
    check(&t, &want, "packed together again");
    let (_, index) = pack_index(&t, &sdir.join("2024-02.pack"));
    assert_eq!(index.len(), want.len());
    assert!(index.iter().all(|e| e.2 == 3 || e.2 == 4), "{index:?}");
    assert!(names(&sdir, ".snap").is_empty() && names(&sdir, ".log").is_empty() && names(&sdir, ".tmp").is_empty());
    assert_eq!(t.db.compact_series(car, APR10, 1000).unwrap(), SeriesCompacted::default());
    t.reopen(opts());
    check(&t, &want, "at the end");

    // every day of the month emptied: the pack goes
    for d in want.keys() {
        t.db.write_points::<Sample>(car, Source::Live, day(*d), day(*d) + DAY_MS, &[]).unwrap();
    }
    pass(&t.db, car, APR10);
    assert!(names(&sdir, "").is_empty(), "{:?}", names(&sdir, ""));
    assert!(t.db.read_points::<Sample>(car, Source::Live, 0, u64::MAX).unwrap().is_empty());
}

#[test]
fn a_crash_at_every_step_of_writing_a_folder_again_loses_nothing() {
    for crash in [Crash::BeforeRename, Crash::BeforeJournalRemoved, Crash::BeforeLooseRemoved] {
        let (mut t, car) = old_folder();
        let before = reads(&t.db, car);
        t.reopen(new());
        let (mut crashes, mut steps) = (0, 0);
        loop {
            // every other step stops where it first can; the one after it carries on from what that left
            let armed = steps % 2 == 0;
            t.db.crash_at(armed.then_some(crash));
            steps += 1;
            assert!(steps < 500, "the pass never ends");
            match t.db.compact_series(car, APR10, 1) {
                Ok(c) if !c.more && !armed => break,
                Ok(_) => {}
                Err(Error::Crashed(c)) => {
                    assert_eq!(c, crash);
                    crashes += 1;
                    // a restart: every value is there, from the old files or the new
                    t.reopen(new());
                    assert_same(&reads(&t.db, car), &before, &format!("after crash {crashes} at {crash:?}"));
                }
                Err(e) => panic!("{crash:?}: {e}"),
            }
        }
        // (a rename for every day folded, pack written and note kept; one removal of a packed day's file)
        let at_least = if crash == Crash::BeforeRename { 8 } else { 1 };
        assert!(crashes >= at_least, "{crash:?} stopped the pass {crashes} times");
        t.db.crash_at(None);
        t.reopen(new());
        assert_eq!(t.db.series_pending(car, APR10).unwrap(), 0);
        assert_same(&reads(&t.db, car), &before, &format!("after the pass with crashes at {crash:?}"));
        let sdir = seg(&t, car, "samples.can");
        assert!(all_small(&t, &sdir, true), "{crash:?}: {:?}", levels(&t, &sdir));
        assert!(names(&sdir, ".tmp").is_empty() && names(&sdir, ".log").is_empty());
    }
}

fn damaged_in(e: Error, name: &str) {
    match e {
        Error::Damaged { file, .. } => assert!(file.ends_with(name), "{file} is not {name}"),
        e => panic!("expected damage in {name}, got {e}"),
    }
}

#[test]
fn a_damaged_pack_is_reported_and_kept_before_and_after_it_is_written_again() {
    let (mut t, car) = old_folder();
    let before = reads(&t.db, car);
    let sdir = seg(&t, car, "samples.can");
    let pack = sdir.join("2024-02.pack");
    // an old pack with a flipped byte: the pass names it and leaves every file as it is
    let good = std::fs::read(&pack).unwrap();
    for at in [good.len() / 3, good.len() - 5] {
        let mut bad = good.clone();
        bad[at] ^= 0x04;
        std::fs::write(&pack, &bad).unwrap();
        t.reopen(new());
        let listed = names(&sdir, "");
        damaged_in(t.db.read_points::<Sample>(car, Source::Live, FEB1, FEB1 + DAY_MS).unwrap_err(), "2024-02.pack");
        let mut failed = false;
        for _ in 0..200 {
            match t.db.compact_series(car, APR10, 3) {
                Ok(c) if c.more => {}
                Ok(c) => panic!("the pass finished over a damaged pack: {c:?}"),
                Err(e) => {
                    damaged_in(e, "samples.can/2024-02.pack");
                    failed = true;
                    break;
                }
            }
        }
        assert!(failed);
        assert_eq!(std::fs::read(&pack).unwrap(), bad, "the damaged pack is as it was");
        assert!(names(&sdir, ".tmp").is_empty());
        assert_eq!(names(&sdir, ""), listed, "no file came or went");
    }
    // put back, the pass writes everything again
    std::fs::write(&pack, &good).unwrap();
    t.reopen(new());
    pass(&t.db, car, APR10);
    assert_same(&reads(&t.db, car), &before, "after the pass");
    // a flipped byte in the pack written again: read errors name it, never an empty day, and the pass keeps it
    let good = std::fs::read(&pack).unwrap();
    let late = api_day(FEB1 + 10 * H, 5);
    for at in [30, good.len() / 2, good.len() - 3] {
        let mut bad = good.clone();
        bad[at] ^= 0x40;
        std::fs::write(&pack, &bad).unwrap();
        t.reopen(new());
        damaged_in(t.db.read_points::<Sample>(car, Source::Live, FEB1, FEB1 + DAY_MS).unwrap_err(), "2024-02.pack");
        damaged_in(t.db.read_points::<Sample>(car, Source::Live, 0, u64::MAX).unwrap_err(), "2024-02.pack");
        damaged_in(
            t.db.read_fields::<Sample>(car, Source::Live, FEB1, FEB1 + DAY_MS, &["soc"]).unwrap_err(),
            "2024-02.pack",
        );
        // a write into one of its days doesn't go ahead as if the day were new
        let w = t.db.write_points(car, Source::Live, late[0].t_ms, late[0].t_ms + 60_000, &late);
        damaged_in(w.unwrap_err(), "2024-02.pack");
        damaged_in(t.db.compact_series(car, APR10, 1000).unwrap_err(), "samples.can/2024-02.pack");
        assert_eq!(std::fs::read(&pack).unwrap(), bad);
        assert!(names(&sdir, ".log").is_empty() && names(&sdir, ".tmp").is_empty());
        // March, in its own pack, still reads
        assert!(!t.db.read_points::<Sample>(car, Source::Live, MAR1, MAR1 + 5 * DAY_MS).unwrap().is_empty());
    }
    std::fs::write(&pack, &good).unwrap();
    t.reopen(new());
    assert_same(&reads(&t.db, car), &before, "put back");
}
