//! The poller with a scripted TeslaFi and its own clock: the pace by what the car does (never slower because another
//! input is live), the same poll never handed on twice, the back-off and TeslaFi's wait, the daily cap, a refused token
//! stopping the account, and a car TeslaFi names that is not this account's waiting for an admin. Made-up data only.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::{MemSink, TOKEN, VIN, vault};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use teslafi::api::Poll;
use teslafi::{Account, AccountState, Feed, FeedError, Match, Pace, Poller, Read, Secret, Store};

const NOW: u64 = 1_800_000_000_000;

#[derive(Clone, Default)]
struct Script {
    answers: Arc<Mutex<VecDeque<Result<Value, FeedError>>>>,
    asked: Arc<Mutex<Vec<Read>>>,
}

impl Script {
    fn push(&self, a: Result<Value, FeedError>) {
        self.answers.lock().unwrap().push_back(a);
    }
}

impl Feed for Script {
    async fn read(&self, token: &Secret, what: Read) -> Result<Poll, FeedError> {
        assert_eq!(token.expose(), TOKEN);
        self.asked.lock().unwrap().push(what);
        match self.answers.lock().unwrap().pop_front() {
            Some(Ok(Value::Object(o))) => Ok(o),
            Some(Ok(_)) => Err(FeedError::NotData),
            Some(Err(e)) => Err(e),
            None => Err(FeedError::Server(500)),
        }
    }
}

/// An account with its car matched to car 7.
fn account(store: &Store, matched: bool) -> String {
    let mut car = teslafi::AccountCar {
        vehicle: "fv1".into(),
        vin_prefix: Some("5YJ3E1EA0PF".into()),
        vin_tag: Some(store.vin_tag(&tesla::FullVin::new(VIN))),
        model: Some('3'),
        year: Some(2023),
        display_name: None,
        matched: None,
    };
    if matched {
        car.matched = Some(Match::Car { car: 7 });
    }
    let a = Account {
        id: String::new(),
        label: "Made-up".into(),
        added_ms: NOW,
        version: 0,
        paused: false,
        time_zone: Some("UTC".into()),
        state: AccountState::Ok,
        last_ok_ms: None,
        last_error: None,
        cars: vec![car],
    };
    store.add(a, &Secret::new(TOKEN)).unwrap().id
}

/// At the 'gentle' pace a sleeping car is read every 20 min: the looks still join into asleep spans (a join
/// window of a fixed 15 min made every asleep look zero-length, and the engine dropped it).
#[tokio::test]
async fn the_gentle_pace_still_records_the_asleep_time() {
    let (_t, data, v) = vault();
    let store = Store::new(&data, v);
    let id = account(&store, true);
    let (script, sink) = (Script::default(), Arc::new(MemSink::default()));
    let mut p =
        Poller::new(store.clone(), &id, script.clone(), sink.clone(), Pace::Gentle.cadence(), chrono_tz::Tz::UTC)
            .unwrap();
    let mut t = NOW;
    for i in 0..6u64 {
        let date = format!("2027-01-15 0{}:{:02}:00", 8 + (i * 20) / 60, (i * 20) % 60);
        script.push(Ok(
            json!({"data_id": (10 + i).to_string(), "Date": date, "carState": "Sleeping", "state": "asleep"}),
        ));
        t = p.tick(t).await.unwrap();
    }
    let facts: Vec<tesla::ApiFact> = sink.points.lock().unwrap().iter().flat_map(|x| x.2.clone()).collect();
    assert_eq!(facts.len(), 6, "{facts:?}");
    assert!(
        facts.iter().any(|f| f.state == tesla::ApiState::Asleep && f.end_ms > f.start_ms),
        "every asleep look is zero-length: {facts:?}"
    );
}

/// The server's clock set back three hours: the reads go on, as for the Tesla API.
#[tokio::test]
async fn a_clock_set_back_does_not_stop_the_reads() {
    let (_t, data, v) = vault();
    let store = Store::new(&data, v);
    let id = account(&store, true);
    let (script, sink) = (Script::default(), Arc::new(MemSink::default()));
    let mut p =
        Poller::new(store.clone(), &id, script.clone(), sink.clone(), Pace::Gentle.cadence(), chrono_tz::Tz::UTC)
            .unwrap();
    let asleep = |id: usize| json!({ "data_id": id.to_string(), "Date": "2027-01-15 08:00:00", "carState": "Sleeping", "state": "asleep" });
    script.push(Ok(asleep(10)));
    p.tick(NOW).await.unwrap();
    let back = NOW - 3 * 3_600_000;
    script.push(Ok(asleep(11)));
    let mut t = p.tick(back).await.unwrap();
    let mut reads = script.asked.lock().unwrap().len();
    while t < back + 3_600_000 {
        script.push(Ok(asleep(20 + reads)));
        t = p.tick(t).await.unwrap();
        reads = script.asked.lock().unwrap().len();
    }
    // the gentle pace reads a sleeping car every 20 minutes: the first read, then two more within the hour
    assert!(reads >= 3, "only {reads} reads in the hour after the clock went back");
}
