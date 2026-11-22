//! The checks of a token count as requests: one read, and the last poll awake only the cadence's gap later; the
//! poller's first read keeps the same gap after them, and the day's count holds them. A refused or unreachable token
//! check changes nothing of the account. Against the fake TeslaFi; made-up tokens and VINs only.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::{Fake, MemSink, TOKEN, sleeping, vault};
use std::sync::Arc;
use std::time::Duration;
use teslafi::{Cadence, Change, Options, Secret, Service};

const NOW: u64 = 1_800_000_000_000;
const GAP: u64 = 400;

async fn wait_for(f: &Fake, n: usize) {
    for _ in 0..300 {
        if f.seen().len() >= n {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("only {} requests", f.seen().len());
}

#[tokio::test]
async fn the_checks_keep_the_gap_and_count_for_the_day() {
    let f = Fake::start(true, true).await;
    // a sleeping car, TeslaFi's last poll awake naming none either: the account is kept without a car
    f.set(sleeping(), sleeping());
    let (_t, data, v) = vault();
    let sink = Arc::new(MemSink::default());
    let cadence = Cadence { min_gap_ms: GAP, ..Cadence::default() };
    let mut opts = Options::default().with_endpoint(f.endpoint());
    opts.cadence = cadence;
    let s = Service::new(&data, v, sink, opts);

    let a = s.add("x", Secret::new(TOKEN), None, NOW).await.unwrap();
    assert!(a.cars.is_empty());
    let seen = f.seen();
    assert_eq!(seen.iter().map(|r| r.query.as_str()).collect::<Vec<_>>(), ["", "command=lastGood"]);
    assert!(seen[1].at - seen[0].at >= Duration::from_millis(GAP), "the check's two reads the gap apart");
    let pacing = s.store().pacing(&a.id).unwrap();
    assert_eq!(pacing.polls_today, 2, "the check's reads count for the day");
    assert!(pacing.last_request_ms.is_some());

    // the poller's first read: the gap after the check's last
    wait_for(&f, 3).await;
    let seen = f.seen();
    assert!(seen[2].at - seen[1].at >= Duration::from_millis(GAP - 20), "{:?}", seen[2].at - seen[1].at);

    // a new token, checked: the poller's next read waits the gap after the check again
    f.state.lock().unwrap().tokens.push("0000madeup0000other".into());
    wait_for(&f, 4).await; // the poller's last poll awake, asked once
    let n = f.seen().len();
    let version = s.account(&a.id).unwrap().version;
    s.replace_token(&a.id, version, Secret::new("0000madeup0000other")).await.unwrap();
    let after = f.seen();
    assert!(after.len() >= n + 2);
    let check_last = after[n + 1].at;
    wait_for(&f, n + 3).await;
    let seen = f.seen();
    assert!(seen[n + 2].at - check_last >= Duration::from_millis(GAP - 20), "{:?}", seen[n + 2].at - check_last);
    s.stop();
}

#[tokio::test]
async fn a_change_with_a_token_teslafi_refuses_changes_nothing() {
    let f = Fake::start(true, true).await;
    let (_t, data, v) = vault();
    let opts = Options::default().with_endpoint(f.endpoint());
    let s = Service::new(&data, v, Arc::new(MemSink::default()), opts);
    let a = s.add("Before", Secret::new(TOKEN), None, NOW).await.unwrap();
    s.stop();
    let a = s.account(&a.id).unwrap();
    let change =
        Change { label: Some("Renamed".into()), paused: Some(true), time_zone: Some(Some("Asia/Tokyo".into())) };
    let e = s.change(&a.id, a.version, change, Some(Secret::new("0000wrong0000"))).await.unwrap_err();
    assert_eq!(e.code(), "teslafi_refused");
    let b = s.account(&a.id).unwrap();
    assert_eq!(b, a, "nothing changed");
    assert_eq!(s.store().token(&a.id).unwrap().expose(), TOKEN);
    // a good one: everything at once, in one version
    let change = Change { label: Some("Renamed".into()), paused: Some(true), time_zone: None };
    let c = s.change(&a.id, a.version, change, Some(Secret::new(TOKEN))).await.unwrap();
    assert_eq!((c.label.as_str(), c.paused, c.version), ("Renamed", true, a.version + 1));
    s.stop();
}
