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

/// What [`logfmt::read_lines_while`] gives (a columns log's frames made straight from the columns) is what
/// [`logfmt::Line::parse`] gives for every line of [`logfmt::read_log_while`], with the same summary: a made-up
/// day in both formats, and columns bodies with keys and lines of every odd kind, at several line limits and
/// stopped early.
#[test]
fn lines_read_straight_from_columns_are_the_lines_parsed_from_text() {
    fn both(xz: &[u8], limits: logfmt::Limits, stop_after: usize) -> usize {
        let mut by_text = Vec::new();
        let a = logfmt::read_log_while(xz, limits, |l| {
            by_text.push(format!("{:?}", logfmt::Line::parse(l)));
            by_text.len() < stop_after
        });
        let mut straight = Vec::new();
        let b = logfmt::read_lines_while(xz, limits, |l| {
            straight.push(format!("{l:?}"));
            straight.len() < stop_after
        });
        assert_eq!(by_text, straight);
        assert_eq!(a.map_err(|e| e.to_string()), b.map_err(|e| e.to_string()));
        straight.len()
    }
    let mut n = 0;
    for formats in [synth::Formats::Candump, synth::Formats::Columns] {
        for log in synth::Scenario::new(3).days(1).with_trouble(true).formats(formats).logs() {
            n += both(&log.bytes, logfmt::Limits::default(), usize::MAX);
        }
    }
    assert!(n > 100_000, "{n}");
    let data64 = "AB".repeat(64);
    let long_key = format!("can0 123{}", " x".repeat(28));
    let odd: Vec<String> = vec![
        "# canbridge v1 app=test".into(),
        "(1767225600.000001) can0 123#".into(),
        "(1767225600.000002) can1 7FF#0102030405060708".into(),
        "(1767225600.000003) can0 1ABCDEF0#DEADBEEF".into(),
        "(1767225600.000004) can0 1abcdef0#00".into(),
        "(1767225600.000005) can255 123#00".into(),
        "(1767225600.000006) can256 123#00".into(),
        "(1767225600.000007) can007 123#00".into(),
        "(1767225600.000008) vcan0 123#00".into(),
        "(1767225600.000009) can0 12#00".into(),
        "(1767225600.000010) can0 1234#00".into(),
        "(1767225600.000011) can0 12G#00".into(),
        "(1767225600.000012) can0  123#00".into(),
        "(1767225600.000013) can0 123 #00".into(),
        "(1767225600.000014) can0#00".into(),
        "(1767225600.000015) can 123#00".into(),
        "(1767225600.000016) can0 123) x#00".into(),
        format!("(1767225600.000017) can0 123#{data64}"),
        format!("(1767225600.000018) {long_key}#{data64}"),
        "# clock wall=1767225600.000000 mono=5000000000".into(),
        "(0.000000) can0 123#FF".into(),
        "(999999999999.999999) can3 04F#0011".into(),
        "(1767225601.000000) can0 123#0A".into(),
        "".into(),
        "# dropped 3 frames".into(),
        "(1767225602.000000) can0 123#0B".into(),
    ];
    let body = synth::columns::encode(odd.iter().map(|l| l.as_bytes()));
    let xz = synth::xz::pack(&body, 6).unwrap();
    // (the body holds these as frames: the odd keys are keys, not notes)
    let mut text = Vec::new();
    logfmt::read_log(&xz, logfmt::Limits::default(), |l| text.push(String::from_utf8(l.to_vec()).unwrap())).unwrap();
    assert_eq!(text, odd.iter().filter(|l| !l.is_empty()).cloned().collect::<Vec<_>>());
    for max_line in [512, 200, 60, 40, 30, 0] {
        for stop_after in [usize::MAX, 1, 2, 5, 19] {
            let limits = logfmt::Limits { max_line, ..logfmt::Limits::default() };
            both(&xz, limits, stop_after);
        }
    }
    let few = logfmt::Limits { max_frames: 5, ..logfmt::Limits::default() };
    both(&xz, few, usize::MAX);
}
