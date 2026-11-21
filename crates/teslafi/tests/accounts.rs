//! Adding, pausing and removing a TeslaFi account against a fake TeslaFi: the token is sealed on disk, never shown and
//! never logged; nothing connects before an account is added. Alone in its own test binary, so the captured log is
//! this test's only. Made-up tokens and VINs only.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::{Fake, MemSink, TOKEN, VIN, vault};
use std::sync::Arc;
use teslafi::{AccountState, Change, Match, Options, Secret, Service};

const NOW: u64 = 1_800_000_000_000;

#[tokio::test]
async fn add_pause_match_and_remove_with_the_token_sealed_and_never_logged() {
    let logs = common::LogBuf::default();
    let sub = tracing_subscriber::fmt().with_max_level(tracing::Level::TRACE).with_writer(logs.clone()).finish();
    let _g = tracing::subscriber::set_default(sub);

    let f = Fake::start(true, true).await;
    let (_t, data, v) = vault();
    let sink = Arc::new(MemSink::default());
    let s = Service::new(&data, v.clone(), sink.clone(), Options::default().with_endpoint(f.endpoint()));
    // off until a token is added: no task, no client, no host
    s.start();
    assert_eq!(s.clients_built(), 0);
    assert!(s.hosts().is_empty() && s.status().is_empty());
    assert!(f.seen().is_empty());

    // a token TeslaFi does not take: refused, nothing stored
    let e = s.add("x", Secret::new("0000wrong0000"), None, NOW).await.unwrap_err();
    assert_eq!(e.code(), "teslafi_refused");
    assert!(s.store().ids().is_empty());
    assert_eq!(s.add("x", Secret::new("has space"), None, NOW).await.unwrap_err().code(), "invalid_token");
    let bad_zone = s.add("x", Secret::new(TOKEN), Some("Mars/Base".into()), NOW).await.unwrap_err();
    assert_eq!(bad_zone.code(), "invalid_time_zone");

    let a = s.add("  Family\u{7} ", Secret::new(TOKEN), Some("America/Chicago".into()), NOW).await.unwrap();
    assert_eq!((a.id.as_str(), a.label.as_str(), a.state), ("fa1", "Family", AccountState::Ok));
    assert_eq!(a.cars.len(), 1);
    let car = &a.cars[0];
    assert_eq!(car.vehicle, "fv1");
    assert_eq!(car.vin_prefix.as_deref(), Some("5YJ3E1EA0PF"), "11 characters at most");
    assert_eq!((car.model, car.year), (Some('3'), Some(2023)));
    assert_eq!(car.matched, None, "an admin confirms the match");
    assert_eq!(s.hosts(), vec!["localhost".to_string()]);
    assert_eq!(s.clients_built(), 2, "one for each check with TeslaFi (the refused one too)");
    // the same car on a second account: refused
    assert_eq!(s.add("again", Secret::new(TOKEN), None, NOW).await.unwrap_err().code(), "account_exists");

    // on disk the file is sealed: no token, no VIN, not even the label
    let raw = std::fs::read(data.join("teslafi/accounts/fa1.json")).unwrap();
    let raw = String::from_utf8_lossy(&raw);
    for needle in [TOKEN, "5YJ3E1EA0PF", "Family", VIN] {
        assert!(!raw.contains(needle), "{needle} is readable on disk");
    }
    assert_eq!(s.store().token("fa1").unwrap().expose(), TOKEN);
    let shown = serde_json::to_string(&s.accounts()).unwrap();
    for needle in [TOKEN, "000123"] {
        assert!(!shown.contains(needle), "{needle} in {shown}");
    }

    // nothing is handed on before the match; then the poller reads and hands the poll to the car
    let a = s.set_matches("fa1", a.version, &[("fv1".into(), Some(Match::Car { car: 7 }))]).unwrap();
    assert_eq!(s.set_matches("fa1", a.version - 1, &[]).unwrap_err().code(), "stale");
    assert_eq!(s.set_matches("fa1", a.version, &[("fv9".into(), None)]).unwrap_err().code(), "no_such_vehicle");
    for _ in 0..200 {
        if !sink.points.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let pts = sink.points.lock().unwrap().clone();
    assert_eq!(pts.len(), 1, "{pts:?}");
    assert_eq!(pts[0].0, 7);
    assert_eq!(pts[0].1[0].lat, Some(12.5));
    let st = s.status();
    assert_eq!(st[0].car, Some(7));
    assert_eq!(st[0].tls, Some("1.3"));

    // paused: nothing is asked
    let n = f.seen().len();
    // (the poller wrote the account's state meanwhile: the version moved)
    let version = s.account("fa1").unwrap().version;
    let a = s.update("fa1", version, Change { paused: Some(true), ..Change::default() }).unwrap();
    assert!(a.paused);
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    assert_eq!(f.seen().len(), n);

    // a new token is checked before it is kept
    let e = s.replace_token("fa1", a.version, Secret::new("0000wrong0000")).await.unwrap_err();
    assert_eq!(e.code(), "teslafi_refused");
    assert_eq!(s.store().token("fa1").unwrap().expose(), TOKEN);

    // removed: the token is gone at once
    s.remove("fa1").unwrap();
    assert!(!data.join("teslafi/accounts/fa1.json").exists());
    assert_eq!(s.remove("fa1").unwrap_err().code(), "no_such_teslafi_account");
    assert!(s.hosts().is_empty());
    s.stop();

    // every request went to the fake TeslaFi's feed, the token in the header only
    assert!(f.seen().iter().all(|r| r.path == "/feed.php" && !r.query.contains("wake") && !r.query.contains("token")));
    let text = logs.text();
    assert!(text.contains("teslafi account added"), "{text}");
    for needle in [TOKEN, "0000wrong0000", "Bearer", "PF000123"] {
        assert!(!text.contains(needle), "{needle} in the log:\n{text}");
    }
}
