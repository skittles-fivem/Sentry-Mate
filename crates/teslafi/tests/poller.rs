//! The poller with a scripted TeslaFi and its own clock: the pace by what the car does, slower while SMO or the Tesla
//! API is live, the same poll never handed on twice, the back-off and TeslaFi's wait, the daily cap, a refused token
//! stopping the account, and a car TeslaFi names that is not this account's waiting for an admin. Made-up data only.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::{MemSink, TOKEN, VIN, vault};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use teslafi::api::Poll;
use teslafi::{Account, AccountState, Cadence, Feed, FeedError, Match, Mode, Poller, Read, Secret, Store};

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

fn poll(id: u64, date: &str, car_state: &str, extra: Value) -> Value {
    let mut v = json!({"data_id": id.to_string(), "Date": date, "carState": car_state, "vin": VIN,
                       "latitude": "12.5", "longitude": "-45.25", "battery_level": "70", "odometer": "1000"});
    if let (Value::Object(o), Value::Object(e)) = (&mut v, extra) {
        o.extend(e);
    }
    v
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

fn poller(store: &Store, id: &str, script: &Script, sink: &Arc<MemSink>) -> Poller<Script, MemSink> {
    Poller::new(store.clone(), id, script.clone(), sink.clone(), Cadence::default(), chrono_tz::Tz::UTC).unwrap()
}

#[tokio::test]
async fn the_pace_follows_the_car_and_a_poll_goes_on_once() {
    let (_t, data, v) = vault();
    let store = Store::new(&data, v);
    let id = account(&store, true);
    let (script, sink) = (Script::default(), Arc::new(MemSink::default()));
    let mut p = poller(&store, &id, &script, &sink);

    script.push(Ok(poll(1, "2027-01-15 08:00:00", "Driving", json!({"shift_state": "D", "speed": "30"}))));
    let next = p.tick(NOW).await.unwrap();
    assert_eq!(next, NOW + 60_000, "driving: a minute");
    assert_eq!(p.status().mode, Mode::Driving);
    // the same poll again (TeslaFi did not poll the car meanwhile): not handed on again
    script.push(Ok(poll(1, "2027-01-15 08:00:00", "Driving", json!({"shift_state": "D", "speed": "30"}))));
    assert_eq!(p.tick(next).await.unwrap(), next + 60_000);
    script.push(Ok(poll(2, "2027-01-15 08:01:00", "Idling", json!({"shift_state": "P"}))));
    let next = p.tick(next + 60_000).await.unwrap();
    assert_eq!(p.status().mode, Mode::Idle);
    assert_eq!(next, NOW + 120_000 + 180_000, "parked: three minutes");
    // a sleeping car: ten minutes, its poll has no values (only the state)
    script.push(Ok(json!({"data_id": "3", "Date": "2027-01-15 09:00:00", "carState": "Sleeping", "state": "asleep"})));
    let next = p.tick(next).await.unwrap();
    assert_eq!(p.status().mode, Mode::Asleep);
    assert_eq!(next, NOW + 300_000 + 600_000);
    // SMO or the Tesla API live for the car: ten minutes at least, even driving
    *sink.other_live.lock().unwrap() = true;
    script.push(Ok(poll(4, "2027-01-15 10:00:00", "Driving", json!({"shift_state": "D"}))));
    let t = next;
    assert_eq!(p.tick(t).await.unwrap(), t + 600_000);

    let pts = sink.points.lock().unwrap().clone();
    assert_eq!(pts.len(), 4, "polls 1, 2, 3 and 4, each once: {pts:?}");
    assert!(pts.iter().all(|x| x.0 == 7));
    // TeslaFi's poll time (UTC here), on whole seconds
    assert_eq!(pts[0].1[0].t_ms, 1_800_000_000_000, "2027-01-15 08:00:00 UTC");
    assert!(pts[2].1.is_empty(), "a sleeping poll has no sample");
    assert_eq!(pts[2].2[0].state, tesla::ApiState::Asleep);
    assert_eq!(pts[0].2[0].state, tesla::ApiState::Online);
    assert_eq!(pts[0].1[0].odometer_km, Some(1609.344));
    // only reads were asked, never anything else
    assert!(script.asked.lock().unwrap().iter().all(|r| *r == Read::Latest));
}

#[tokio::test]
async fn errors_back_off_teslafis_wait_is_kept_and_a_refused_token_stops_it() {
    let (_t, data, v) = vault();
    let store = Store::new(&data, v);
    let id = account(&store, true);
    let (script, sink) = (Script::default(), Arc::new(MemSink::default()));
    let mut p = poller(&store, &id, &script, &sink);

    script.push(Err(FeedError::Server(502)));
    assert_eq!(p.tick(NOW).await.unwrap(), NOW + 60_000);
    assert_eq!(p.status().mode, Mode::Backoff);
    script.push(Err(FeedError::NotData));
    assert_eq!(p.tick(NOW + 60_000).await.unwrap(), NOW + 60_000 + 120_000, "doubling");
    // asking early asks nothing
    let asked = script.asked.lock().unwrap().len();
    assert_eq!(p.tick(NOW + 100_000).await.unwrap(), NOW + 180_000);
    assert_eq!(script.asked.lock().unwrap().len(), asked);
    // TeslaFi's own wait, kept across a restart
    script.push(Err(FeedError::RateLimited { retry_after_s: Some(900) }));
    let until = p.tick(NOW + 180_000).await.unwrap();
    assert_eq!(until, NOW + 180_000 + 900_000);
    let mut again = poller(&store, &id, &script, &sink);
    assert_eq!(again.tick(NOW + 200_000).await.unwrap(), until, "a restart does not ask early");
    // a good answer: back to the normal pace
    script.push(Ok(poll(1, "2027-01-15 08:00:00", "Idling", json!({}))));
    assert_eq!(again.tick(until).await.unwrap(), until + 180_000);
    assert_eq!(again.status().state, AccountState::Ok);
    // refused: the account stops, the admin is told, nothing is asked until a new token
    script.push(Err(FeedError::Refused));
    again.tick(until + 180_000).await.unwrap();
    assert_eq!(again.status().state, AccountState::Refused);
    assert_eq!(again.status().mode, Mode::Off);
    assert_eq!(again.status().last_error.unwrap().code, "token_refused");
    assert_eq!(sink.changed.lock().unwrap().last(), Some(&AccountState::Refused));
    let asked = script.asked.lock().unwrap().len();
    for k in 1..5 {
        again.tick(until + 180_000 + k * 3_600_000).await.unwrap();
    }
    assert_eq!(script.asked.lock().unwrap().len(), asked);
    // a new token (an admin's change) starts it again
    store.update(&id, None, Some(&Secret::new(TOKEN)), |a| a.state = AccountState::Ok).unwrap();
    script.push(Ok(poll(2, "2027-01-15 09:00:00", "Idling", json!({}))));
    again.tick(until + 6 * 3_600_000).await.unwrap();
    assert_eq!(script.asked.lock().unwrap().len(), asked + 1);
}

#[tokio::test]
async fn at_most_the_daily_cap_and_a_gap_between_requests() {
    let (_t, data, v) = vault();
    let store = Store::new(&data, v);
    let id = account(&store, true);
    let (script, sink) = (Script::default(), Arc::new(MemSink::default()));
    let cad = Cadence { daily_cap: 3, driving_ms: 1_000, ..Cadence::default() };
    let mut p = Poller::new(store.clone(), &id, script.clone(), sink.clone(), cad, chrono_tz::Tz::UTC).unwrap();
    let day = NOW - NOW % 86_400_000;
    let mut t = day + 1000;
    for i in 0..3 {
        script.push(Ok(poll(i + 1, "2027-01-15 08:00:00", "Driving", json!({"shift_state": "D"}))));
        t = p.tick(t).await.unwrap();
    }
    assert_eq!(t, day + 1000 + 2 * 10_000 + 10_000, "never two requests less than 10 s apart");
    assert_eq!(p.tick(t).await.unwrap(), day + 86_400_000, "the cap: nothing until midnight UTC");
    assert_eq!(script.asked.lock().unwrap().len(), 3);
    assert_eq!(p.status().last_error.unwrap().code, "daily_cap");
}

#[tokio::test]
async fn a_car_not_matched_is_not_read_and_another_vin_waits_for_an_admin() {
    let (_t, data, v) = vault();
    let store = Store::new(&data, v);
    let id = account(&store, false);
    let (script, sink) = (Script::default(), Arc::new(MemSink::default()));
    let mut p = poller(&store, &id, &script, &sink);
    p.tick(NOW).await.unwrap();
    assert!(script.asked.lock().unwrap().is_empty(), "nothing matched: nothing read");
    assert_eq!(p.status().mode, Mode::Off);

    store.update(&id, None, None, |a| a.cars[0].matched = Some(Match::Car { car: 7 })).unwrap();
    // TeslaFi now sends polls of another car (the owner changed cars at TeslaFi)
    let other = "7SAYGDEE0PF000999";
    let mut v = poll(1, "2027-01-15 08:00:00", "Idling", json!({}));
    v["vin"] = json!(other);
    script.push(Ok(v));
    p.tick(NOW + 1000).await.unwrap();
    let a = store.get(&id).unwrap();
    assert_eq!(a.cars.len(), 2);
    assert_eq!(a.cars[1].vehicle, "fv2");
    assert_eq!(a.cars[1].vin_prefix.as_deref(), Some("7SAYGDEE0PF"));
    assert_eq!(a.cars[1].matched, None);
    assert!(sink.points.lock().unwrap().is_empty(), "another car's poll never goes to car 7");
    assert_eq!(p.status().vehicle.as_deref(), Some("fv2"));
}

#[tokio::test]
async fn a_new_account_whose_car_sleeps_asks_once_for_the_last_poll_awake() {
    let (_t, data, v) = vault();
    let store = Store::new(&data, v);
    let a = Account {
        id: String::new(),
        label: "x".into(),
        added_ms: NOW,
        version: 0,
        paused: false,
        time_zone: None,
        state: AccountState::Ok,
        last_ok_ms: None,
        last_error: None,
        cars: Vec::new(),
    };
    let id = store.add(a, &Secret::new(TOKEN)).unwrap().id;
    let (script, sink) = (Script::default(), Arc::new(MemSink::default()));
    let mut p = poller(&store, &id, &script, &sink);
    script.push(Ok(json!({"data_id": "5", "Date": "2027-01-15 09:00:00", "carState": "Sleeping", "vin": null})));
    let t = p.tick(NOW).await.unwrap();
    script.push(Ok(poll(4, "2027-01-15 08:00:00", "Idling", json!({}))));
    p.tick(t).await.unwrap();
    assert_eq!(*script.asked.lock().unwrap(), vec![Read::Latest, Read::LastGood]);
    assert_eq!(store.get(&id).unwrap().cars.len(), 1, "the car named by the last poll awake");
    assert!(sink.points.lock().unwrap().is_empty());
}
