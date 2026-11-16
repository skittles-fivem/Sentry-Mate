#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! The decoder gives exactly what Sentry Six Manager's decoder gives (expected files made by
//! `expected-from-sentry-manager.js` from its own code): per series, count, first, last, min, max and sum.

use dbc::{Decoder, Profile};
use logfmt::{Limits, Line};
use std::collections::BTreeMap;

fn decode_all(xz: &[u8], sidecar: &serde_json::Value) -> BTreeMap<String, [f64; 6]> {
    let mut d = Decoder::new(Profile::Tesla3Y, sidecar);
    let mut out: BTreeMap<String, [f64; 6]> = BTreeMap::new();
    logfmt::read_log(xz, Limits::default(), |l| {
        let Line::Frame(f) = Line::parse(l) else { return };
        for (sig, v) in d.decode(&f) {
            let s = out.entry(sig.series.clone()).or_insert([0.0, v, v, v, v, 0.0]);
            s[0] += 1.0;
            s[2] = v;
            if v < s[3] {
                s[3] = v;
            }
            if v > s[4] {
                s[4] = v;
            }
            s[5] += v;
        }
    })
    .unwrap();
    out
}

fn expected(json: &str) -> BTreeMap<String, [f64; 6]> {
    let v: BTreeMap<String, Vec<f64>> = serde_json::from_str(json).unwrap();
    v.into_iter().map(|(k, a)| (k, [a[0], a[1], a[2], a[3], a[4], a[5]])).collect()
}

fn compare(got: &BTreeMap<String, [f64; 6]>, want: &BTreeMap<String, [f64; 6]>) {
    let missing: Vec<_> = want.keys().filter(|k| !got.contains_key(*k)).take(10).collect();
    let extra: Vec<_> = got.keys().filter(|k| !want.contains_key(*k)).take(10).collect();
    assert!(missing.is_empty() && extra.is_empty(), "missing {missing:?}, extra {extra:?}");
    for (k, w) in want {
        assert_eq!(&got[k], w, "{k}: [count, first, last, min, max, sum]");
    }
}

#[test]
fn every_message_of_the_model_3_y_set_decodes_as_sentry_manager_does() {
    let side: serde_json::Value = serde_json::from_str(include_str!("fixtures/synthetic.sidecar.json")).unwrap();
    let got = decode_all(include_bytes!("fixtures/synthetic.candump.xz"), &side);
    let want = expected(include_str!("fixtures/synthetic.expected.json"));
    assert!(want.len() > 1500);
    compare(&got, &want);
    // the Screenmate's GPS: decoded while no car bus sends 0x04F, and only its three signals
    assert_eq!(got["GPSLatitude04F"][0], 12.0);
}

#[test]
fn the_columns_fixture_decodes_as_sentry_manager_does() {
    let got = decode_all(include_bytes!("../../logfmt/tests/fixtures/columns-sample.log.xz"), &serde_json::Value::Null);
    compare(&got, &expected(include_str!("fixtures/columns-sample.expected.json")));
}

#[test]
fn bus_maps_follow_the_sidecar() {
    let side = serde_json::json!({ "bus": { "can0": "chassis", "can1": "vehicle (inferred)", "can3": "screenmate-internal" } });
    let m = dbc::bus_map(&side, Profile::Tesla3Y);
    assert_eq!(m.get("can0").map(String::as_str), Some("ChassisBus"));
    assert_eq!(m.get("can1").map(String::as_str), Some("VehicleBus"));
    assert!(!m.contains_key("can3"));
    assert_eq!(dbc::internal_buses(&side), vec!["can3".to_string()]);
    assert_eq!(
        dbc::bus_map(&serde_json::Value::Null, Profile::TeslaSxLegacy).get("can0").map(String::as_str),
        Some("PowertrainBus")
    );
}

#[test]
fn series_names_are_unique_and_the_legacy_set_parses() {
    let d = Profile::Tesla3Y.dbc();
    let mut seen = std::collections::HashSet::new();
    for m in d.messages.values() {
        for s in &m.signals {
            assert!(seen.insert(s.series.clone()), "{} twice", s.series);
        }
    }
    assert!(d.messages.get(&0x257).is_some_and(|m| m.signals.iter().any(|s| s.name == "DI_vehicleSpeed")));
    let legacy = Profile::TeslaSxLegacy.dbc();
    assert!(
        legacy.messages.len() > 50
            && legacy.messages.values().all(|m| m.bus == "ChassisBus" || m.bus == "PowertrainBus" || !m.bus.is_empty())
    );
}

/// A real log, when given (never committed: real logs hold the car's position):
/// `PARITY_LOG=x.log.xz PARITY_EXPECTED=x.expected.json cargo test -p dbc real_log` (the sidecar next to the log).
#[test]
fn a_real_log_decodes_as_sentry_manager_does_when_given() {
    let (Ok(log), Ok(exp)) = (std::env::var("PARITY_LOG"), std::env::var("PARITY_EXPECTED")) else { return };
    let side: serde_json::Value = serde_json::from_slice(&std::fs::read(format!("{log}.json")).unwrap()).unwrap();
    let got = decode_all(&std::fs::read(&log).unwrap(), &side);
    let want = expected(&std::fs::read_to_string(exp).unwrap());
    compare(&got, &want);
    eprintln!("real log: {} series, {} values, all equal", want.len(), want.values().map(|v| v[0]).sum::<f64>());
}
