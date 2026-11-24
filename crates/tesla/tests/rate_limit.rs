//! Tesla does the limiting: a 429 from a fake Tesla on this machine is waited out as it asks, told to the sink once,
//! and shown in the status with the time of the next try. Everything is made up.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::{Fake, MemSink, vault};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tesla::{Match, Options, Secret, Service};

fn now_ms() -> u64 {
    u64::try_from(SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis()).unwrap()
}

#[tokio::test]
async fn a_429_is_waited_out_told_once_and_shown_with_the_next_try() {
    let f = Fake::start(true).await;
    f.add_refresh("made-up-refresh-0");
    let (_t, data, v) = vault();
    let sink = Arc::new(MemSink::default());
    let s = Service::new(&data, v, sink.clone(), Options::default().with_endpoints(f.endpoints()));
    let a = s
        .add_with_refresh_token("Family", tesla::Region::Global, Secret::new("made-up-refresh-0"), now_ms())
        .await
        .unwrap();
    // the next list is answered 429 with Tesla's wait
    f.can("/api/1/products", 429, vec![("retry-after", "120".into())], String::new());
    let before = now_ms();
    s.set_matches(&a.id, a.version, &[("tv1".into(), Some(Match::Car { car: 7 }))]).unwrap();
    let mut limited = Vec::new();
    for _ in 0..300 {
        limited = sink.limited.lock().unwrap().clone();
        if !limited.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(limited.len(), 1, "told once");
    let (account, until) = limited[0].clone();
    assert_eq!(account, a.id);
    assert!((before + 120_000..now_ms() + 120_000 + 1).contains(&until), "{until}");
    // nothing more asked meanwhile; the status says until when
    let asked = f.seen().iter().filter(|x| x.path.starts_with("/api/1/products")).count();
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(f.seen().iter().filter(|x| x.path.starts_with("/api/1/products")).count(), asked);
    let st = s.status();
    assert_eq!(st[0].rate_limited_until_ms, Some(until));
    assert_eq!(st[0].last_error.as_ref().unwrap().code, "rate_limited");
    assert_eq!(st[0].cars[0].mode, tesla::Mode::Backoff);
    assert_eq!(st[0].cars[0].next_poll_ms, Some(until));
    assert_eq!(sink.limited.lock().unwrap().len(), 1);
    s.stop();
}
