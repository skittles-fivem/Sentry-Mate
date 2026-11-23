//! How long an hour of driving takes to read (docs/plans/performance.md, "CAN logs"): the 60 one-minute logs of a
//! made-up drive at the car's real rates, as candump text and as columns. Each is timed three ways: unpacked only,
//! unpacked and each line parsed, and read whole into samples ([`timeline::read_log`]).
//!
//! `cargo test --release -p timeline --test decode_speed -- --ignored --nocapture`
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::print_stdout)]

use std::time::Instant;

/// The busiest 60 logs in a row of a made-up day (a drive), with their sidecars.
fn hour(formats: synth::Formats) -> Vec<(Vec<u8>, serde_json::Value)> {
    let logs: Vec<synth::LogFile> =
        synth::Scenario::new(7).days(1).rates(synth::Rates::Realistic).formats(formats).logs().collect();
    let sizes: Vec<usize> = logs.iter().map(|l| l.bytes.len()).collect();
    let n = 60.min(logs.len());
    let start = (0..=logs.len() - n).max_by_key(|&i| sizes[i..i + n].iter().sum::<usize>()).unwrap();
    logs.into_iter()
        .skip(start)
        .take(n)
        .map(|l| {
            let side = serde_json::from_slice(&l.sidecar_bytes()).unwrap();
            (l.bytes, side)
        })
        .collect()
}

fn best_ms(mut f: impl FnMut()) -> f64 {
    (0..3)
        .map(|_| {
            let t = Instant::now();
            f();
            t.elapsed().as_secs_f64() * 1000.0
        })
        .fold(f64::INFINITY, f64::min)
}

#[test]
#[ignore = "a measurement, run by hand in release"]
fn measure_an_hour_of_logs() {
    for (name, formats) in [("candump", synth::Formats::Candump), ("columns", synth::Formats::Columns)] {
        let logs = hour(formats);
        let packed: usize = logs.iter().map(|l| l.0.len()).sum();
        let (mut frames, mut bytes) = (0u64, 0u64);
        for (xz, _) in &logs {
            logfmt::read_log(xz, logfmt::Limits::default(), |l| {
                bytes += l.len() as u64 + 1;
                frames += u64::from(matches!(logfmt::Line::parse(l), logfmt::Line::Frame(_)));
            })
            .unwrap();
        }
        let unpack = best_ms(|| {
            for (xz, _) in &logs {
                logfmt::read_log(xz, logfmt::Limits::default(), |_| {}).unwrap();
            }
        });
        let parse = best_ms(|| {
            for (xz, _) in &logs {
                logfmt::read_log(xz, logfmt::Limits::default(), |l| {
                    std::hint::black_box(logfmt::Line::parse(l));
                })
                .unwrap();
            }
        });
        let mut samples = 0;
        let whole = best_ms(|| {
            samples = 0;
            for (xz, side) in &logs {
                let r = timeline::read_log(xz, side, dbc::Profile::Tesla3Y, timeline::SampleParams::default()).unwrap();
                samples += r.samples.len();
            }
        });
        println!(
            "{name}: {} logs, {:.1} MB packed, {:.0} MB as text, {frames} frames ({:.0} a second), {samples} samples: \
             unpack {unpack:.0} ms, unpack and parse {parse:.0} ms, read whole {whole:.0} ms ({:.0} ns a frame)",
            logs.len(),
            packed as f64 / 1e6,
            bytes as f64 / 1e6,
            frames as f64 / (logs.len() as f64 * 60.0),
            whole * 1e6 / frames as f64,
        );
    }
}

/// Feeding a frame takes only the wanted signals out of it; the samples are those of every signal decoded and given
/// by its series name (a made-up day with trouble, both formats, the Screenmate's own position on its internal bus).
#[test]
fn feeding_frames_gives_what_every_decoded_signal_gives() {
    let params = timeline::SampleParams::default();
    let profile = dbc::Profile::Tesla3Y;
    let logs = synth::Scenario::new(11).days(1).with_trouble(true).formats(synth::Formats::Switch(40)).logs();
    let (mut n, mut samples) = (0, 0);
    for log in logs {
        let side: serde_json::Value = serde_json::from_slice(&log.sidecar_bytes()).unwrap();
        let read = timeline::read_log(&log.bytes, &side, profile, params).unwrap();
        let mut decoder = dbc::Decoder::new(profile, &side);
        let mut builder = timeline::SampleBuilder::new(profile, params);
        logfmt::read_log(&log.bytes, logfmt::Limits::default(), |l| {
            if let logfmt::Line::Frame(f) = logfmt::Line::parse(l) {
                builder.frame(&f);
                for (sig, v) in decoder.decode(&f) {
                    builder.signal(f.time_us, sig, v);
                }
            }
        })
        .unwrap();
        let (by_name, vin) = builder.finish();
        assert_eq!(read.samples, by_name, "{}", log.name);
        assert_eq!(read.vin, vin);
        n += 1;
        samples += by_name.len();
    }
    assert!(n > 100 && samples > 5_000, "{n} logs, {samples} samples");
}
