#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! A made-up car's logs go through what a real car's go through: `logfmt` reads them, `trust` verifies them and
//! their chain, `dbc` decodes the values put in, and the `timeline` finds the drives and charges that happened.

use sha2::{Digest, Sha256};
use synth::{CarModel, Formats, LogFile, Rates, Report, Scenario};

/// Two days, light rates (quick in a debug build), every kind of trouble, a switch of format part way.
fn run(s: &Scenario) -> (Vec<LogFile>, Report) {
    let mut logs = s.logs();
    let files: Vec<LogFile> = logs.by_ref().collect();
    (files, logs.report())
}

fn scenario() -> Scenario {
    let base =
        Scenario::new(11).days(2).rates(Rates::Light).xz_preset(1).with_trouble(true).formats(Formats::Switch(20));
    // and the Screenmate off for three minutes during the second day's first drive (the plan, not the bridge, decides
    // when the car drives: its truth is known before any log is made)
    let day2 = synth::DEFAULT_START_MS + 86_400_000;
    let d = base.truth().drives.into_iter().find(|d| d.start_ms > day2).unwrap();
    let trouble = synth::Trouble { offline: vec![(d.start_ms + 120_000, d.start_ms + 300_000)], ..Default::default() };
    base.trouble(trouble)
}

#[test]
fn logfmt_reads_them_in_both_formats() {
    let (files, report) = run(&scenario());
    assert!(files.len() > 100, "{}", files.len());
    let (mut candump, mut columns) = (0, 0);
    for f in &files {
        let mut frames = 0u64;
        let mut lines = Vec::new();
        let sum = logfmt::read_log(&f.bytes, logfmt::Limits::default(), |l| {
            if l.first() == Some(&b'(') {
                frames += 1;
            }
            lines.push(String::from_utf8(l.to_vec()).unwrap());
        })
        .unwrap();
        assert_eq!(sum.format, f.format, "{}", f.name);
        assert_eq!(sum.skipped_long_lines, 0);
        assert_eq!(frames, f.frames, "{}", f.name);
        assert_eq!(f.sidecar["frames"], f.frames);
        assert_eq!(f.sidecar["format"], f.format.name());
        assert!(lines[0].starts_with("# canbridge v1 app=0.2."), "{}", lines[0]);
        assert!(lines.last().unwrap().ends_with(" end"), "{}", f.name);
        // every line is one the reader knows
        for l in &lines {
            assert!(!matches!(logfmt::Line::parse(l.as_bytes()), logfmt::Line::Other(_)), "{l}");
        }
        match f.format {
            logfmt::Format::Candump => candump += 1,
            logfmt::Format::Columns => columns += 1,
        }
        assert!(f.end_ms >= f.start_ms && f.end_ms - f.start_ms <= 120_000, "{}: {} ms", f.name, f.end_ms - f.start_ms);
        assert!(f.name.ends_with(".log.xz") && trust::LogName::parse(&f.name).is_some_and(|n| !n.sidecar));
    }
    // the first 20 logs the bridge wrote are text, the lost one among them
    let mut written: Vec<&str> =
        files.iter().map(|f| f.name.as_str()).chain(report.lost.iter().map(String::as_str)).collect();
    written.sort_by_key(|n| n.split('_').nth(1).map(str::to_string));
    let lost_text = written[..20].iter().filter(|n| report.lost.iter().any(|l| l == *n)).count();
    assert_eq!(candump + lost_text, 20);
    assert!(columns > 50);
    // a log a minute while awake; a log closed quiet before each sleep
    assert!(files.iter().filter(|f| f.closed_because == "rotate").count() > files.len() / 2);
    assert!(files.iter().any(|f| f.closed_because == "quiet"));
    assert!(files.iter().any(|f| f.closed_because == "flush"), "the app update");
    assert!(files.iter().any(|f| f.closed_because == "stop"), "the Screenmate turned off");
}

#[test]
fn trust_verifies_every_log_and_the_chain_shows_only_the_lost_one() {
    let s = scenario();
    let (files, report) = run(&s);
    assert_eq!(report.lost.len(), 1);
    assert_eq!(report.dropped.len(), 1);
    assert_eq!(report.not_recorded.len(), 1);
    let mut entries = Vec::new();
    for f in &files {
        let sha = hex::encode(Sha256::digest(&f.bytes));
        let side: serde_json::Value = serde_json::from_slice(&f.sidecar_bytes()).unwrap();
        let check = trust::check(&f.name, &sha, f.bytes.len() as u64, &side);
        let trust::Check::Signed(link) = check else { panic!("{}: {check:?}", f.name) };
        assert!(trust::same_fingerprint(&link.key_fingerprint, &report.fingerprint));
        assert_eq!(
            trust::judge(&trust::Check::Signed(link.clone()), Some(&report.fingerprint)),
            trust::Verdict::Verified
        );
        entries.push(trust::Entry { link, failed: None });
    }
    let chain = trust::chain(&entries);
    assert!(chain.restarts.is_empty(), "{:?}", chain.restarts);
    assert_eq!(chain.issues.len(), 1, "{:?}", chain.issues);
    let trust::ChainIssue::Gap { from, to, .. } = &chain.issues[0] else { panic!("{:?}", chain.issues) };
    assert_eq!(from, to);
    assert_eq!(chain.count, files.len() as u64);
    // a changed byte fails
    let f = &files[3];
    let mut bytes = f.bytes.clone();
    bytes[40] ^= 1;
    let sha = hex::encode(Sha256::digest(&bytes));
    assert!(matches!(trust::check(&f.name, &sha, bytes.len() as u64, &f.sidecar), trust::Check::Bad { .. }));
}

#[test]
fn the_same_seed_gives_the_same_bytes_and_another_seed_others() {
    let s = Scenario::new(5).days(1).rates(Rates::Light).xz_preset(0).software_update(false);
    let a: Vec<(String, Vec<u8>, Vec<u8>)> =
        s.logs().map(|f| (f.name.clone(), f.bytes.clone(), f.sidecar_bytes())).collect();
    let b: Vec<(String, Vec<u8>, Vec<u8>)> =
        s.logs().map(|f| (f.name.clone(), f.bytes.clone(), f.sidecar_bytes())).collect();
    assert!(!a.is_empty());
    assert!(a == b);
    let c: Vec<String> = Scenario::new(6).days(1).rates(Rates::Light).xz_preset(0).logs().map(|f| f.name).collect();
    assert_ne!(a.iter().map(|x| x.0.clone()).collect::<Vec<_>>(), c);
}

#[test]
fn the_vin_reads_as_the_model_and_is_clearly_made_up() {
    for (m, letter) in [(CarModel::Y, 'Y'), (CarModel::Three, '3')] {
        let s = Scenario::new(1).model(m).car(4);
        let vin = s.vin();
        assert!(vin.starts_with("5YJ") && vin.contains("FAKE"), "{vin}");
        let info = timeline::decode_vin(&vin, 0x405).unwrap();
        assert_eq!(info.model, Some(letter));
        assert_eq!(info.year, Some(2024));
        // and it comes in the logs' 0x405 pages
        let pages = synth::vin_pages(&vin);
        let mut c = timeline::VinCollector::new();
        for p in &pages {
            c.add(0x405, p);
        }
        assert_eq!(c.vin().map(|v| v.vin_prefix.clone()), Some(vin[..11].to_string()));
    }
}

#[test]
fn realistic_rates_while_driving_and_charging() {
    // one day: two commutes, half an hour of an overnight charge
    let s = Scenario::new(2).days(1).errands(false).software_update(false).xz_preset(0);
    let t0 = std::time::Instant::now();
    let files: Vec<LogFile> = s.logs().collect();
    eprintln!("{} logs in {:?}", files.len(), t0.elapsed());
    let full: Vec<&LogFile> = files.iter().filter(|f| f.closed_because == "rotate").collect();
    assert!(!full.is_empty());
    let busiest = full.iter().map(|f| f.frames).max().unwrap();
    // 0x118 and 0x132 at 100 Hz, 0x257 at 50 Hz, the rest at 1 to 10 Hz
    assert!((15_000..25_000).contains(&busiest), "{busiest} frames in a minute");
    let mut ids = std::collections::BTreeSet::new();
    logfmt::read_log(&full[0].bytes, logfmt::Limits::default(), |l| {
        if let logfmt::Line::Frame(f) = logfmt::Line::parse(l) {
            ids.insert((f.bus, f.id));
        }
    })
    .unwrap();
    assert!(ids.contains(&(0, 0x405)) && ids.contains(&(1, 0x3F3)) && ids.contains(&(3, 0x04F)), "{ids:x?}");
}
