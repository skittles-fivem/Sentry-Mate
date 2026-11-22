#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! The made-up modules' firmware fingerprints come back out of the logs: on `can0` only, the old values before the
//! software update and the new ones after it, a corrupt frame seen once.

use synth::{Firmware, Rates, Scenario};

#[test]
fn fingerprints_before_and_after_the_update_on_can0_only() {
    let mut fw = Firmware::made_up();
    // the gateway copies carry another battery CRC: never read
    fw.can1 = Some(vec![("BMS_appCrc", 0x0bad_0bad)]);
    let s0 = Scenario::new(5).days(1).rates(Rates::Light).xz_preset(0);
    let update = s0.truth().updates[0];
    // two corrupt frames: one in the morning drive's first minutes, one after the update
    let drive = s0.truth().drives[0].clone();
    fw.corrupt_at_ms = vec![drive.start_ms + 90_000, update.1 + 600_000];
    let s = s0.firmware(fw.clone());
    let mut before = std::collections::BTreeMap::<(String, u64), u32>::new();
    let mut after = before.clone();
    let mut hints = 0;
    for f in s.logs() {
        let r =
            timeline::read_log(&f.bytes, &f.sidecar, dbc::Profile::Tesla3Y, timeline::SampleParams::default()).unwrap();
        hints += r.firmware.hints;
        for seen in r.firmware.seen {
            assert!(seen.first_ms <= seen.last_ms);
            let side = if seen.last_ms < update.0 {
                &mut before
            } else if seen.first_ms >= update.1 {
                &mut after
            } else {
                continue;
            };
            *side.entry((seen.field, seen.value)).or_default() += seen.count;
        }
    }
    assert_eq!(hints, 0, "this bridge writes no hint lines");
    let at = |m: &std::collections::BTreeMap<(String, u64), u32>, f: &str, v: u64| {
        m.get(&(f.to_string(), v)).copied().unwrap_or(0)
    };
    // every field of the made-up set, many times
    for (name, v) in fw.values(false) {
        assert!(at(&before, name, v) > 20, "{name}: {}", at(&before, name, v));
    }
    for (name, v) in fw.values(true) {
        assert!(at(&after, name, v) > 20, "{name} after: {}", at(&after, name, v));
    }
    // the changed fields: the old values not after, the new ones not before
    for (name, v) in &fw.after {
        assert_eq!(at(&before, name, *v), 0, "{name}");
        let old = fw.before.iter().find(|(n, _)| n == name).unwrap().1;
        assert_eq!(at(&after, name, old), 0, "{name}");
    }
    // the chassis bus copies' value never counted
    assert!(before.keys().chain(after.keys()).all(|(_, v)| *v != 0x0bad_0bad));
    // the corrupt frames: values seen once each (fields of the flipped frame), none of them a real value
    let known: Vec<u64> = fw.values(false).iter().chain(fw.values(true).iter()).map(|x| x.1).collect();
    let odd: Vec<(&(String, u64), &u32)> =
        before.iter().chain(after.iter()).filter(|((_, v), _)| !known.contains(v)).collect();
    assert!(!odd.is_empty() && odd.iter().all(|(_, n)| **n == 1), "{odd:?}");
}

#[test]
fn without_firmware_the_logs_are_as_they_were() {
    let a: Vec<Vec<u8>> = Scenario::new(5).days(1).rates(Rates::Light).xz_preset(0).logs().map(|l| l.bytes).collect();
    let fw = Scenario::new(5).days(1).rates(Rates::Light).xz_preset(0).firmware(Firmware::made_up());
    let b: Vec<Vec<u8>> = fw.logs().map(|l| l.bytes).collect();
    assert_eq!(a.len(), b.len());
    assert_ne!(a, b);
    let r = timeline::read_log(&a[a.len() / 2], &serde_json::json!({}), dbc::Profile::Tesla3Y, Default::default());
    assert!(r.unwrap().firmware.seen.is_empty());
}
