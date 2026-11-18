#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! A scenario's first days are the same whatever its length, the bridge's trouble happens on time even while the car
//! sleeps, every kind of trouble asked for really happens, and the made-up fleet stays in the open sea.

use sha2::{Digest, Sha256};
use synth::{Formats, LogFile, Rates, Report, Scenario, Trouble};

const MIN: u64 = 60_000;

fn run(s: &Scenario) -> (Vec<LogFile>, Report) {
    let mut logs = s.logs();
    let files: Vec<LogFile> = logs.by_ref().collect();
    (files, logs.report())
}

fn light(seed: u64) -> Scenario {
    Scenario::new(seed).rates(Rates::Light).xz_preset(0)
}

/// The lines of a log.
fn lines(f: &LogFile) -> Vec<String> {
    let mut out = Vec::new();
    logfmt::read_log(&f.bytes, logfmt::Limits::default(), |l| out.push(String::from_utf8(l.to_vec()).unwrap()))
        .unwrap();
    out
}

/// The time of a log's last frame, ms.
fn last_frame_ms(f: &LogFile) -> Option<u64> {
    lines(f).iter().rev().find_map(|l| {
        let t = l.strip_prefix('(')?.split(')').next()?;
        let (s, us) = t.split_once('.')?;
        Some(s.parse::<u64>().ok()? * 1000 + us.parse::<u64>().ok()? / 1000)
    })
}

#[test]
fn a_shorter_run_is_the_start_of_a_longer_one_byte_for_byte() {
    for s in [light(21).with_trouble(true).formats(Formats::Switch(7)), light(22).battery(30.0)] {
        let (one, r1) = run(&s.clone().days(1));
        let (two, r2) = run(&s.clone().days(2));
        assert!(one.len() > 20 && two.len() > one.len() + 20, "{} {}", one.len(), two.len());
        for (a, b) in one.iter().zip(&two) {
            assert_eq!(a.name, b.name);
            assert!(a.bytes == b.bytes, "{}", a.name);
            assert_eq!(a.sidecar, b.sidecar, "{}", a.name);
        }
        // the trouble of the first day is the same
        assert_eq!(r1.lost, r2.lost[..r1.lost.len()]);
        assert_eq!(r1.not_recorded, r2.not_recorded[..r1.not_recorded.len()]);
        assert_eq!(r1.app_updates, r2.app_updates[..r1.app_updates.len()]);
        let end = s.clone().days(1).end_ms();
        let early = |t: &Trouble| t.lose_log_at.iter().copied().filter(|&x| x < end).collect::<Vec<_>>();
        assert_eq!(early(&r1.trouble), early(&r2.trouble));
        // and the drives that ended on the first day
        let d1: Vec<_> = r1.truth.drives.iter().filter(|d| d.end_ms < end).collect();
        let d2: Vec<_> = r2.truth.drives.iter().filter(|d| d.end_ms < end).collect();
        assert_eq!(d1, d2);
    }
}

#[test]
fn every_kind_of_trouble_happens_even_on_a_single_day() {
    // one day (the home charge after midnight), and a full battery (no home charge at all)
    for s in [light(1).days(1).with_trouble(true), light(1).days(2).battery(95.0).with_trouble(true)] {
        let (files, r) = run(&s);
        assert_eq!(r.trouble.lose_log_at.len(), 1, "{:?}", r.trouble);
        assert_eq!(r.lost.len(), 1, "{:?}", r.trouble);
        assert_eq!(r.dropped.len(), 1, "{:?}", r.trouble);
        assert_eq!(r.not_recorded.len(), 1, "{:?}", r.trouble);
        // measured from its real edges (it ends at the first frame after)
        let ((a, b), (want_a, want_b)) = (r.not_recorded[0], r.trouble.not_recorded[0]);
        assert!(a == want_a && b >= want_b && b - want_b < 100, "{:?} vs {:?}", r.not_recorded, r.trouble);
        assert_eq!(r.app_updates.len(), 1, "{:?}", r.trouble);
        assert!(files.iter().any(|f| f.closed_because == "flush"));
        let note = files.iter().flat_map(lines).find(|l| l.starts_with("# not recorded")).unwrap();
        assert!(
            note.starts_with("# not recorded: paused 360 s") || note.starts_with("# not recorded: paused 361 s"),
            "{note}"
        );
        // the logs after the update come from the next build
        let at = r.app_updates[0];
        assert!(files.iter().filter(|f| f.start_ms > at).all(|f| f.sidecar["app_build"] == 151));
        assert!(files.iter().filter(|f| f.end_ms <= at).all(|f| f.sidecar["app_build"] == 150));
    }
}

#[test]
fn trouble_while_the_car_sleeps_happens_on_time() {
    let base = light(4).days(2).supercharger_trip(false);
    let truth = base.truth();
    // the first night's sleep, hours long
    let &(a, b) = truth.sleeps.iter().find(|(a, b)| b - a > 4 * 3_600_000 && *a > base.start_ms).unwrap();
    let trouble = Trouble {
        offline: vec![(a + 20 * MIN, a + 40 * MIN)],
        not_recorded: vec![(a + 50 * MIN, a + 56 * MIN)],
        app_update_at: vec![(a + 70 * MIN, 25_000)],
        ..Default::default()
    };
    let (files, r) = run(&base.clone().trouble(trouble));
    // the log before the sleep closes quiet, a minute after its last frame, not when the Screenmate goes off
    let before = files.iter().rfind(|f| f.end_ms <= a + 20 * MIN).unwrap();
    assert_eq!(before.closed_because, "quiet", "{}", before.name);
    assert_eq!(before.end_ms, last_frame_ms(before).unwrap() + 60_000, "{}", before.name);
    assert!(files.iter().all(|f| f.closed_because != "stop"), "nothing was open when it went off");
    for f in &files {
        let last = last_frame_ms(f).unwrap_or(f.start_ms);
        assert!(f.end_ms <= last + 60_000 && f.end_ms - f.start_ms <= 120_000, "{}", f.name);
    }
    // the pause is measured from its real edges, the update happens while asleep (no log open: no flush)
    assert_eq!(r.not_recorded, vec![(a + 50 * MIN, a + 56 * MIN)]);
    let note = files.iter().flat_map(lines).find(|l| l.starts_with("# not recorded")).unwrap();
    assert!(note.starts_with("# not recorded: paused 360 s"), "{note}");
    assert_eq!(r.app_updates, vec![a + 70 * MIN]);
    let after = files.iter().find(|f| f.start_ms >= b).unwrap();
    assert_eq!(after.sidecar["app_build"], 151);
    assert!(after.name.ends_with("_1.log.xz"), "the sequence starts again: {}", after.name);
    assert!(b > a + 70 * MIN);
}

#[test]
fn the_same_seed_gives_these_bytes_on_every_machine() {
    let mut h = Sha256::new();
    for f in light(8).days(1).software_update(false).logs() {
        h.update(f.name.as_bytes());
        h.update(&f.bytes);
    }
    // a change here changes every seeded data folder: only with a reason
    assert_eq!(hex::encode(h.finalize()), "90c19c2df0cbcf980de02eb04eccc1cc013299369001f2c7633ea007f3d62053");
}

#[test]
fn the_fleet_stays_in_the_open_sea() {
    let fleet = synth::fleet(3, 10_000, 1);
    assert_eq!(fleet.len(), synth::MAX_FLEET as usize);
    let mut homes = std::collections::BTreeSet::new();
    for s in &fleet {
        for p in s.plan().places {
            assert!((0.0..1.5).contains(&p.at.lat) && (0.0..1.5).contains(&p.at.lon), "car {}: {p:?}", s.car);
        }
        assert!(homes.insert(format!("{:.5} {:.5}", s.home.lat, s.home.lon)), "car {}", s.car);
        assert_eq!(s.vin().len(), 17, "{}", s.vin());
    }
    assert_eq!(Scenario::new(1).car(u32::MAX).vin().len(), 17);
}
