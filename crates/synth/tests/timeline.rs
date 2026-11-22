#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! The timeline finds the drives and charges the made-up car really made, from its logs alone.

use model::{ChargeKind, State};
use synth::{Formats, Rates, Report, Scenario};

fn timeline_of(s: &Scenario) -> (timeline::Timeline, Report) {
    let mut logs = s.logs();
    let (mut samples, mut facts) = (Vec::new(), Vec::new());
    for f in logs.by_ref() {
        let r =
            timeline::read_log(&f.bytes, &f.sidecar, dbc::Profile::Tesla3Y, timeline::SampleParams::default()).unwrap();
        // the VIN's three pages come within a few seconds of frames
        match &r.vin {
            Some(vin) => assert_eq!(vin.model, Some(s.model.letter())),
            None => assert!(f.frames < 200, "{} has {} frames and no VIN", f.name, f.frames),
        }
        samples.extend(r.samples);
        facts.push(r.facts);
    }
    let input = timeline::Input {
        car: 1,
        from_ms: s.start_ms,
        to_ms: s.end_ms(),
        samples: &samples,
        logs: &facts,
        holes: &[],
        api: &[],
        marks: &[],
    };
    (timeline::build(&input, &timeline::Params::default()), logs.report())
}

#[test]
fn the_drives_and_charges_that_happened() {
    let s = Scenario::new(3).days(3).rates(Rates::Light).xz_preset(0).formats(Formats::Columns);
    let (t, report) = timeline_of(&s);
    let truth = &report.truth;
    assert!(truth.drives.len() >= 6, "{:?}", truth.drives.len());
    assert!(truth.charges.iter().any(|c| c.kind == ChargeKind::Dc));
    assert!(truth.charges.iter().any(|c| c.kind == ChargeKind::Ac && c.pauses == 1));
    assert_eq!(
        t.drives.len(),
        truth.drives.len(),
        "{:#?}",
        t.drives.iter().map(|d| (d.start_ms, d.end_ms)).collect::<Vec<_>>()
    );
    for (d, want) in t.drives.iter().zip(&truth.drives) {
        assert!(d.start_ms.abs_diff(want.start_ms) <= 2_000, "drive start {} vs {}", d.start_ms, want.start_ms);
        assert!(d.end_ms.abs_diff(want.end_ms) <= 2_000, "drive end {} vs {}", d.end_ms, want.end_ms);
        let km = d.distance_km.unwrap();
        assert!((km - want.distance_km).abs() < 0.15, "{km} vs {}", want.distance_km);
        assert!((d.start_soc.unwrap() - want.start_soc).abs() <= 1.0);
        assert!((d.end_soc.unwrap() - want.end_soc).abs() <= 1.0);
        assert!((d.speed_max_kmh.unwrap() - want.max_speed_kmh).abs() < 2.0);
        assert!(d.energy_used_kwh.unwrap() > 0.0);
        let (a, b) = (d.start.as_ref().unwrap(), d.end.as_ref().unwrap());
        assert!(a.lat > 0.0 && a.lat < 1.5 && b.lon > 0.0 && b.lon < 1.5, "in the made-up town");
    }
    // one charge each; the short pause is inside its charge
    assert_eq!(t.charges.len(), truth.charges.len(), "{:#?}", t.charges);
    for (c, want) in t.charges.iter().zip(&truth.charges) {
        assert_eq!(c.kind, Some(want.kind));
        assert!(c.start_ms.abs_diff(want.start_ms) <= 3_000, "charge start {} vs {}", c.start_ms, want.start_ms);
        if want.end_ms >= s.end_ms() {
            // still charging at the end: the log being written then is not given, so the logs end up to a minute early
            assert!(c.end_ms <= want.end_ms && want.end_ms - c.end_ms <= 61_000, "{} vs {}", c.end_ms, want.end_ms);
            continue;
        }
        assert!(c.end_ms.abs_diff(want.end_ms) <= 3_000, "charge end {} vs {}", c.end_ms, want.end_ms);
        let kwh = c.energy_added_kwh.unwrap();
        assert!(
            (kwh - want.energy_added_kwh).abs() < 0.03 * want.energy_added_kwh + 0.1,
            "{kwh} vs {}",
            want.energy_added_kwh
        );
        assert!((c.end_soc.unwrap() - want.end_soc).abs() <= 1.0);
    }
    // asleep as long as the car was (but before its first log: nothing says it slept then)
    let asleep: u64 = t.states.iter().filter(|s| s.state == State::Asleep).map(|s| s.end_ms - s.start_ms).sum();
    let slept: u64 = truth.sleeps.iter().filter(|(a, _)| *a > s.start_ms).map(|(a, b)| b - a).sum();
    assert!(asleep.abs_diff(slept) < slept / 50, "asleep {asleep} vs {slept}");
}

#[test]
fn trouble_shows_as_unknown_not_sleep() {
    let s = Scenario::new(3).days(2).rates(Rates::Light).xz_preset(0).with_trouble(true).supercharger_trip(false);
    let (t, report) = timeline_of(&s);
    assert_eq!(report.not_recorded.len(), 1);
    let (a, b) = report.not_recorded[0];
    let unknown =
        t.states.iter().find(|st| st.state == State::Unknown && st.start_ms <= a + 1_000 && st.end_ms + 1_000 >= b);
    assert!(unknown.is_some(), "{a}..{b}: {:#?}", t.states);
    assert!(!t.losses.is_empty(), "the dropped frames");
    assert!(!t.charges.is_empty());
}
