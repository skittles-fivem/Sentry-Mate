#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! A whole log, end to end: a synthetic candump log made with the DBC's own signal layouts (so the frames are what
//! the car would send), packed with xz, read back into samples, facts and the VIN, and made into a drive.

use dbc::{Mux, Profile, Signal};
use model::Gear;
use std::fmt::Write;
use timeline::{Closed, Input, Params, SampleParams, read_log};

/// 2026-01-01T00:00:00Z in µs.
const T0_US: u64 = 1_767_225_600_000_000;

/// The signal of a series in the Model 3/Y set.
fn signal(series: &str) -> &'static Signal {
    Profile::Tesla3Y.dbc().messages.values().flat_map(|m| m.signals.iter()).find(|s| s.series == series).unwrap()
}

/// Puts a value into a frame's bytes, the way the decoder reads it back.
fn put(bytes: &mut [u8], sig: &Signal, value: f64) {
    let mut raw = ((value - sig.offset) / sig.factor).round() as i64;
    if raw < 0 {
        raw += 1i64 << sig.length;
    }
    let mut set = |bit: u32, on: bool| {
        if on {
            bytes[(bit >> 3) as usize] |= 1 << (bit & 7);
        }
    };
    if sig.little {
        for i in 0..sig.length {
            set(sig.start + i, (raw >> i) & 1 == 1);
        }
    } else {
        let mut bit = sig.start;
        for i in (0..sig.length).rev() {
            set(bit, (raw >> i) & 1 == 1);
            bit = if bit & 7 == 0 { bit + 15 } else { bit - 1 };
        }
    }
}

/// One frame of the message carrying these series (all of one message), as a candump line.
fn frame(t_us: u64, values: &[(&str, f64)]) -> String {
    let first = signal(values[0].0);
    let msg = &Profile::Tesla3Y.dbc().messages[&first.message_id];
    let mut bytes = vec![0u8; msg.size as usize];
    for &(series, v) in values {
        let sig = signal(series);
        assert_eq!(sig.message_id, msg.id, "{series} is in another message");
        if let (Mux::Value(m), Some(i)) = (sig.mux, msg.mux) {
            put(&mut bytes, &msg.signals[i], m as f64);
        }
        put(&mut bytes, sig, v);
    }
    let iface = match msg.bus.as_str() {
        "VehicleBus" => "can0",
        "ChassisBus" => "can1",
        other => panic!("{other}"),
    };
    let hex: String = bytes.iter().map(|b| format!("{b:02X}")).collect();
    format!("({}.{:06}) {iface} {:03X}#{hex}\n", t_us / 1_000_000, t_us % 1_000_000, msg.id)
}

/// A broken or crafted log whose frame times jump back and forth between two seconds would make a sample per frame;
/// a log gives at most `MAX_LOG_SAMPLES`, and its facts still see every frame.
#[test]
fn frame_times_going_back_and_forth_make_a_bounded_number_of_samples() {
    let mut log = String::new();
    let frames = 3 * timeline::MAX_LOG_SAMPLES;
    for k in 0..frames as u64 {
        let t = T0_US + (k % 2) * 1_000_000 + k;
        writeln!(log, "({}.{:06}) can0 33A#0000000000470000", t / 1_000_000, t % 1_000_000).unwrap();
    }
    writeln!(log, "({}.000000) can1 132#0000000000000000", T0_US / 1_000_000 + 5).unwrap();
    let side = serde_json::json!({ "closed_because": "rotate" });
    let read = read_log(&xz(&log), &side, Profile::Tesla3Y, SampleParams::default()).unwrap();
    assert!(read.samples.len() <= timeline::MAX_LOG_SAMPLES + 1, "{}", read.samples.len());
    assert_eq!(read.facts.buses, vec!["can0", "can1"], "the facts read the whole log");
    assert_eq!(read.facts.last_frame_ms, Some(T0_US / 1000 + 5000));
}

fn xz(text: &str) -> Vec<u8> {
    let mut out = Vec::new();
    lzma_rs::xz_compress(&mut text.as_bytes(), &mut out).unwrap();
    out
}

#[test]
fn a_log_becomes_samples_facts_a_vin_and_a_drive() {
    // a made-up VIN (no real car's): Model Y, 2024, plant B
    let vin = b"XP7YGCEK0RB000042";
    let mut log = String::from("# canbridge v1 app=0.2.150 build=150 device=Screenmate_One\n");
    let mut pages = [[0u8; 8]; 3];
    pages[0][0] = 0x10;
    pages[0][5..8].copy_from_slice(&vin[0..3]);
    pages[1][0] = 0x11;
    pages[1][1..8].copy_from_slice(&vin[3..10]);
    pages[2][0] = 0x12;
    pages[2][1..8].copy_from_slice(&vin[10..17]);
    for (i, p) in pages.iter().enumerate() {
        let hex: String = p.iter().map(|b| format!("{b:02X}")).collect();
        writeln!(log, "({}.{:06}) can0 405#{hex}", T0_US / 1_000_000, 10_000 + i * 1000).unwrap();
    }
    // 50 s at 10 frames a second: in D at 36 km/h, the pack giving 10 kW, the odometer counting
    for k in 0..500u64 {
        let t = T0_US + 50_000 + k * 100_000;
        let odo = 12_345.0 + 36.0 * (k as f64 / 10.0) / 3600.0;
        log += &frame(t, &[("DI_gear", 4.0)]);
        log += &frame(t + 1000, &[("DI_vehicleSpeed", 36.0)]);
        log += &frame(t + 2000, &[("BattVoltage132", 400.0), ("SmoothBattCurrent132", 25.0)]);
        log += &frame(t + 3000, &[("UI_odometer", odo)]);
        log += &frame(t + 4000, &[("BMS_uiChargeStatus", 0.0)]);
        if k % 10 == 0 {
            log += &frame(t + 5000, &[("UI_SOC", 80.0), ("UI_Range", 200.0)]);
        }
        if k == 250 {
            log += "# dropped 3 chunks of frames: logger fell behind\n";
        }
    }
    let side = serde_json::json!({
        "format": "canbridge-candump-xz/1",
        "start_wall_ms": T0_US / 1000,
        "end_wall_ms": T0_US / 1000 + 50_100,
        "closed_because": "rotate",
    });
    let read = read_log(&xz(&log), &side, Profile::Tesla3Y, SampleParams::default()).unwrap();

    let v = read.vin.unwrap();
    assert_eq!((v.vin_prefix.as_str(), v.model, v.year, v.via), ("XP7YGCEK0RB", Some('Y'), Some(2024), 0x405));
    let f = &read.facts;
    assert_eq!(f.closed, Closed::Rotate);
    assert_eq!(f.buses, vec!["can0", "can1"]);
    assert_eq!(f.dropped, 1);
    assert_eq!(f.frames(), Some((T0_US / 1000 + 10, T0_US / 1000 + 49_954)));

    // seconds 1 to 49 (the first is cut)
    assert_eq!(read.samples.len(), 49);
    let s = &read.samples[10];
    assert_eq!(s.t_ms, T0_US / 1000 + 11_000);
    assert_eq!(s.gear, Some(Gear::D));
    assert!((s.speed_kmh.unwrap() - 36.0).abs() < 0.05, "{:?}", s.speed_kmh);
    assert!((s.battery_power_kw.unwrap() - 10.0).abs() < 1e-6, "positive out of the pack");
    assert_eq!(s.soc, Some(80.0));
    assert!((s.range_km.unwrap() - 200.0 * 1.609344).abs() < 1e-9);
    assert_eq!(s.charging, Some(false));
    assert!(s.odometer_km.unwrap() > 12_345.0);

    let logs = [read.facts.clone()];
    let input = Input {
        car: 9,
        from_ms: T0_US / 1000,
        to_ms: T0_US / 1000 + 60_000,
        samples: &read.samples,
        logs: &logs,
        holes: &[],
    };
    let t = timeline::build(&input, &Params::default());
    assert_eq!(t.drives.len(), 1);
    let d = &t.drives[0];
    // UI_odometer counts in 0.1 km: 48 s at 36 km/h is 0.48 km
    assert!((0.35..=0.55).contains(&d.distance_km.unwrap()), "{:?}", d.distance_km);
    assert!((d.energy_used_kwh.unwrap() - 49.0 * 10.0 / 3600.0).abs() < 1e-6);
    assert_eq!(t.losses.len(), 1);
    assert_eq!(t.losses[0].dropped, 1);
}
