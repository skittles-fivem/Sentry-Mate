//! The poller with a scripted TeslaFi and its own clock: the pace by what the car does (never slower because another
//! input is live), the same poll never handed on twice, the back-off and TeslaFi's wait (told once), no cap of its own, a refused token
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
    // driving again: the driving pace, whatever other input is live for the car (no input waits on another)
    script.push(Ok(poll(4, "2027-01-15 10:00:00", "Driving", json!({"shift_state": "D"}))));
    let t = next;
    assert_eq!(p.tick(t).await.unwrap(), t + 60_000);

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
async fn a_new_poll_hands_on_the_values_not_on_the_can_bus_once() {
    let (_t, data, v) = vault();
    let store = Store::new(&data, v);
    let id = account(&store, true);
    let (script, sink) = (Script::default(), Arc::new(MemSink::default()));
    let mut p = poller(&store, &id, &script, &sink);
    let extra = json!({"locked": "1", "fd_window": "0", "fp_window": "0", "rd_window": "0", "rp_window": "0",
                       "car_version": "2026.32.5 abcdef", "newVersion": "2026.38.1", "newVersionStatus": "Available"});
    script.push(Ok(poll(1, "2027-01-15 08:00:00", "Idling", extra.clone())));
    let next = p.tick(NOW).await.unwrap();
    script.push(Ok(poll(1, "2027-01-15 08:00:00", "Idling", extra)));
    let next = p.tick(next).await.unwrap();
    // a poll with none of them: nothing handed on
    script.push(Ok(poll(2, "2027-01-15 08:05:00", "Idling", json!({}))));
    p.tick(next).await.unwrap();
    let got = sink.off_can.lock().unwrap().clone();
    assert_eq!(got.len(), 1, "once, for the new poll only: {got:?}");
    let (car, at, o) = &got[0];
    assert_eq!((*car, *at), (7, 1_800_000_000_000));
    assert_eq!(o.locked, Some(true));
    assert_eq!(o.windows, [Some(false); 4]);
    assert_eq!(o.version.as_deref(), Some("2026.32.5"));
    assert_eq!((o.update_status.as_deref(), o.update_version.as_deref()), (Some("available"), Some("2026.38.1")));
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
async fn no_daily_cap_only_a_gap_between_requests() {
    let (_t, data, v) = vault();
    let store = Store::new(&data, v);
    let id = account(&store, true);
    let (script, sink) = (Script::default(), Arc::new(MemSink::default()));
    let cad = Cadence { driving_ms: 1_000, ..Cadence::default() };
    let mut p = Poller::new(store.clone(), &id, script.clone(), sink.clone(), cad, chrono_tz::Tz::UTC).unwrap();
    let day = NOW - NOW % 86_400_000;
    let mut t = day + 1000;
    // more requests in a day than the cap of 1,500 an older server kept: TeslaFi does the limiting
    for i in 0..1_700 {
        script.push(Ok(poll(i + 1, "2027-01-15 08:00:00", "Driving", json!({"shift_state": "D"}))));
        let next = p.tick(t).await.unwrap();
        assert_eq!(next, t + 10_000, "never two requests less than 10 s apart, and nothing else holds them back");
        t = next;
    }
    assert_eq!(script.asked.lock().unwrap().len(), 1_700);
    let st = p.status();
    assert_eq!(st.polls_today, 1_700, "the day's count is kept, for showing");
    assert!(st.last_error.is_none(), "{:?}", st.last_error);
    assert_eq!(st.rate_limited_until_ms, None);
}

#[tokio::test]
async fn teslafis_429_is_waited_out_and_told_once_per_episode() {
    let (_t, data, v) = vault();
    let store = Store::new(&data, v);
    let id = account(&store, true);
    let (script, sink) = (Script::default(), Arc::new(MemSink::default()));
    let mut p = poller(&store, &id, &script, &sink);

    // no Retry-After: five minutes; one request, the sink told once
    script.push(Err(FeedError::RateLimited { retry_after_s: None }));
    let until = p.tick(NOW).await.unwrap();
    assert_eq!(until, NOW + 300_000);
    assert_eq!(*sink.limited.lock().unwrap(), vec![(id.clone(), until)]);
    let st = p.status();
    assert_eq!(st.rate_limited_until_ms, Some(until));
    assert_eq!(st.last_error.as_ref().unwrap().code, "rate_limited");
    assert_eq!(st.mode, Mode::Backoff);
    // asking early asks nothing and tells nothing more
    p.tick(NOW + 60_000).await.unwrap();
    assert_eq!(script.asked.lock().unwrap().len(), 1);
    assert_eq!(sink.limited.lock().unwrap().len(), 1);
    // the wait is kept across a restart
    let mut again = poller(&store, &id, &script, &sink);
    assert_eq!(again.tick(NOW + 61_000).await.unwrap(), until);
    assert_eq!(again.status().rate_limited_until_ms, Some(until));
    assert_eq!(sink.limited.lock().unwrap().len(), 1, "the same episode");
    // the wait ends with a good answer
    script.push(Ok(poll(1, "2027-01-15 08:00:00", "Idling", json!({}))));
    again.tick(until).await.unwrap();
    assert_eq!(again.status().rate_limited_until_ms, None);
    // a 429 after that is a new episode, with TeslaFi's own wait (at least a minute)
    script.push(Err(FeedError::RateLimited { retry_after_s: Some(10) }));
    let next = again.tick(until + 180_000).await.unwrap();
    assert_eq!(next, until + 180_000 + 60_000);
    assert_eq!(sink.limited.lock().unwrap().len(), 2);
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

fn no_car_account(store: &Store) -> String {
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
    store.add(a, &Secret::new(TOKEN)).unwrap().id
}

#[tokio::test]
async fn an_awake_car_whose_polls_name_no_car_is_not_asked_every_10_s() {
    let (_t, data, v) = vault();
    let store = Store::new(&data, v);
    let id = no_car_account(&store);
    let (script, sink) = (Script::default(), Arc::new(MemSink::default()));
    let mut p = poller(&store, &id, &script, &sink);
    for i in 0..20 {
        script.push(Ok(json!({"data_id": format!("10{i}"), "Date": "2027-01-15 09:00:00", "carState": "Idling",
                              "state": "online", "vin": null})));
    }
    let mut t = NOW;
    let mut gaps = Vec::new();
    for _ in 0..20 {
        let next = p.tick(t).await.unwrap();
        gaps.push(next - t);
        t = next;
    }
    let asked = script.asked.lock().unwrap().clone();
    // the latest poll, then once the last poll awake 10 s later, then the parked pace
    assert_eq!(&asked[..2], &[Read::Latest, Read::LastGood]);
    assert!(asked[2..].iter().all(|r| *r == Read::Latest), "{asked:?}");
    assert_eq!(gaps[0], 10_000);
    assert!(gaps[1..].iter().all(|g| *g == 180_000), "{gaps:?}");
    assert_eq!(p.status().mode, Mode::Idle);
}

#[tokio::test]
async fn a_zone_east_of_the_old_one_does_not_drop_teslafis_polls() {
    let (_t, data, v) = vault();
    let store = Store::new(&data, v);
    let id = account(&store, true);
    let (script, sink) = (Script::default(), Arc::new(MemSink::default()));
    let mut p = poller(&store, &id, &script, &sink);
    script.push(Ok(poll(1, "2027-01-15 08:00:00", "Driving", json!({"shift_state": "D"}))));
    let mut t = p.tick(NOW).await.unwrap();
    assert_eq!(sink.points.lock().unwrap().len(), 1);

    store.update(&id, None, None, |a| a.time_zone = Some("Asia/Tokyo".into())).unwrap();
    for k in 1..=5u64 {
        script.push(Ok(poll(1 + k, &format!("2027-01-15 08:0{k}:00"), "Driving", json!({"shift_state": "D"}))));
        t = p.tick(t).await.unwrap();
    }
    let pts = sink.points.lock().unwrap().clone();
    assert_eq!(pts.len(), 6, "every new poll after the change goes on: {}", pts.len());
    // read in the new zone: 08:01 in Tokyo is 23:01 UTC the day before
    assert_eq!(pts[1].1[0].t_ms, 1_800_000_000_000 - 9 * 3_600_000 + 60_000);
    // the same poll again is still not handed on twice
    script.push(Ok(poll(6, "2027-01-15 08:05:00", "Driving", json!({"shift_state": "D"}))));
    p.tick(t).await.unwrap();
    assert_eq!(sink.points.lock().unwrap().len(), 6);
}
