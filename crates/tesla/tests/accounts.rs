//! Adding an account by refresh token, against a fake Tesla on this machine: the tokens are sealed on disk,
//! never shown and never logged. Alone in its own test binary, so the captured log is this test's only.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::{Fake, MemSink, VIN_A, vault};
use std::sync::Arc;
use tesla::{AccountState, AuthError, Match, Options, Secret, Service, ServiceError};

const NOW: u64 = 1_800_000_000_000;

fn service(f: &Fake, data: &std::path::Path, v: Arc<vault::Vault>) -> (Service<MemSink>, Arc<MemSink>) {
    let sink = Arc::new(MemSink::default());
    let s = Service::new(data, v, sink.clone(), Options::default().with_endpoints(f.endpoints()));
    (s, sink)
}

#[tokio::test]
async fn add_by_refresh_token_seals_the_tokens_and_never_logs_them() {
    let logs = common::LogBuf::default();
    let sub = tracing_subscriber::fmt().with_max_level(tracing::Level::TRACE).with_writer(logs.clone()).finish();
    let _g = tracing::subscriber::set_default(sub);

    let f = Fake::start(true).await;
    f.add_refresh("made-up-refresh-0");
    let (_t, data, v) = vault();
    let (s, _sink) = service(&f, &data, v.clone());
    let a = s
        .add_with_refresh_token("  Family\u{7}  ", tesla::Region::Global, Secret::new("made-up-refresh-0"), NOW)
        .await
        .unwrap();
    assert_eq!(a.id, "ta1");
    assert_eq!(a.label, "Family");
    assert_eq!(a.state, AccountState::Ok);
    assert_eq!(a.cars.len(), 2);
    assert_eq!(a.cars[0].vehicle, "tv1");
    assert_eq!(a.cars[0].vin_prefix.as_deref(), Some("5YJ3E1EA0PF"));
    assert_eq!(a.cars[0].model, Some('3'));
    assert_eq!(a.cars[0].year, Some(2023));
    assert!(a.cars.iter().all(|c| c.matched.is_none()));
    // The rotated token is the one stored.
    let c = s.store().credentials("ta1").unwrap();
    assert_eq!(c.refresh.expose(), "fake-refresh-1");
    assert_eq!(c.access.as_ref().unwrap().expose(), "fake-access-1");
    assert_eq!(c.access_expires_ms, NOW + 28_800_000);
    // On disk the file is sealed: no token, no VIN, not even the label.
    let raw = std::fs::read(data.join("tesla/accounts/ta1.json")).unwrap();
    let raw = String::from_utf8_lossy(&raw);
    for needle in ["fake-refresh-1", "fake-access-1", "made-up-refresh-0", "5YJ3E1EA0PF", "Family", VIN_A] {
        assert!(!raw.contains(needle), "{needle} is readable on disk");
    }
    // What the server shows holds no token and no VIN serial.
    let shown = serde_json::to_string(&s.accounts()).unwrap();
    for needle in ["fake-refresh", "fake-access", "000001", "000002"] {
        assert!(!shown.contains(needle), "{needle} in {shown}");
    }
    assert_eq!(format!("{c:?}"), "Credentials { access_expires_ms: 1800028800000, .. }");
    // The same cars on a second account: refused, nothing stored.
    f.add_refresh("made-up-refresh-x");
    let e = s
        .add_with_refresh_token("Again", tesla::Region::Global, Secret::new("made-up-refresh-x"), NOW)
        .await
        .unwrap_err();
    assert_eq!(e.code(), "account_exists");
    assert_eq!(s.store().ids(), vec!["ta1".to_string()]);
    // A token Tesla does not take.
    let e = s.add_with_refresh_token("x", tesla::Region::Global, Secret::new("nope"), NOW).await.unwrap_err();
    assert!(matches!(e, ServiceError::Auth(AuthError::Refused)));
    assert_eq!(e.code(), "tesla_refused");

    // Matching: a car can be matched once, and polling starts.
    let a = s.set_matches("ta1", a.version, &[("tv1".into(), Some(Match::Car { car: 7 }))]).unwrap();
    let stale = s.set_matches("ta1", a.version - 1, &[("tv2".into(), Some(Match::LeaveOut))]).unwrap_err();
    assert_eq!(stale.code(), "stale");
    let taken = s.set_matches("ta1", a.version, &[("tv2".into(), Some(Match::Car { car: 7 }))]).unwrap_err();
    assert_eq!(taken.code(), "car_taken");
    let missing = s.set_matches("ta1", a.version, &[("tv9".into(), None)]).unwrap_err();
    assert_eq!(missing.code(), "no_such_vehicle");
    for _ in 0..100 {
        if s.status().first().is_some_and(|st| st.cars.iter().any(|c| c.last_data_ms.is_some())) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let st = s.status();
    assert_eq!(st[0].cars[0].car, Some(7));
    assert!(st[0].cars[0].last_data_ms.is_some(), "{st:?}");
    assert_eq!(s.hosts(), vec!["localhost".to_string()]);

    // Removing it forgets the tokens at once.
    s.remove("ta1").unwrap();
    assert!(!data.join("tesla/accounts/ta1.json").exists());
    assert_eq!(s.remove("ta1").unwrap_err().code(), "no_such_tesla_account");
    s.stop();

    let text = logs.text();
    assert!(text.contains("tesla tokens refreshed"), "{text}");
    for needle in ["fake-refresh", "fake-access", "made-up-refresh", "PF000001", "PF000002", "Bearer"] {
        assert!(!text.contains(needle), "{needle} in the log:\n{text}");
    }
}
