//! The poller's cadence on a virtual clock, against a fake Tesla with no network: it never wakes a car, lets it
//! fall asleep, keeps to the caps and stops on a refused sign-in. Everything is made up.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::{MemSink, parked_data, vault};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use tesla::stream::{StreamEvent, StreamHandle, Streamer};
use tesla::{
    Account, AccountCar, AccountState, ApiError, AuthError, CarState, Credentials, FullVin, Match, Poller, Secret,
    Store, Tesla, Tokens, Vehicle,
};
use tokio::sync::mpsc;

const T0: u64 = 1_800_000_000_000;
const MIN: u64 = 60_000;

#[derive(Default)]
struct World {
    calls: Vec<(u64, String)>,
    now: u64,
    state: Option<CarState>,
    data: Option<Value>,
    data_errors: VecDeque<ApiError>,
    list_errors: VecDeque<ApiError>,
    refresh_errors: VecDeque<AuthError>,
    issued: u32,
    /// The refresh token Tesla takes now.
    valid_refresh: String,
    valid_access: String,
    /// The store, to check a new refresh token is saved before its access token is used.
    store: Option<Store>,
    stored_before_use: bool,
    /// The access token's lifetime Tesla answers (default 8 h).
    expires_in_s: Option<u64>,
}

#[derive(Clone, Default)]
struct FakeTesla(Arc<Mutex<World>>);

impl FakeTesla {
    fn calls(&self, what: &str) -> Vec<u64> {
        self.0.lock().unwrap().calls.iter().filter(|(_, w)| w == what).map(|(t, _)| *t).collect()
    }
    fn set(&self, f: impl FnOnce(&mut World)) {
        f(&mut self.0.lock().unwrap());
    }
    fn check_access(w: &mut World, access: &Secret) -> Result<(), ApiError> {
        if access.expose() != w.valid_access {
            return Err(ApiError::Unauthorized);
        }
        if let Some(s) = &w.store {
            let c = s.credentials("ta1").unwrap();
            w.stored_before_use &= c.refresh.expose() == w.valid_refresh;
        }
        Ok(())
    }
}

impl Tesla for FakeTesla {
    async fn products(&self, access: &Secret) -> Result<Vec<Vehicle>, ApiError> {
        let mut w = self.0.lock().unwrap();
        let now = w.now;
        w.calls.push((now, "list".into()));
        Self::check_access(&mut w, access)?;
        if let Some(e) = w.list_errors.pop_front() {
            return Err(e);
        }
        Ok(vec![Vehicle {
            id: "1001".into(),
            vehicle_id: Some("501".into()),
            vin: Some(FullVin::new(common::VIN_A)),
            display_name: Some("Made-up three".into()),
            state: w.state.unwrap_or(CarState::Asleep),
        }])
    }

    async fn vehicle_data(&self, access: &Secret, id: &str, location: bool) -> Result<Value, ApiError> {
        let mut w = self.0.lock().unwrap();
        let now = w.now;
        w.calls.push((now, if location { "data+location".into() } else { "data".into() }));
        assert_eq!(id, "1001");
        Self::check_access(&mut w, access)?;
        // A car that sleeps is never asked (that would be a wake-up attempt).
        assert_eq!(w.state, Some(CarState::Online), "vehicle_data asked of a car that is not online");
        if let Some(e) = w.data_errors.pop_front() {
            return Err(e);
        }
        let mut d = w.data.clone().unwrap_or_else(parked_data);
        d["drive_state"]["timestamp"] = json!(now);
        Ok(d)
    }

    async fn refresh(&self, refresh: &Secret) -> Result<Tokens, AuthError> {
        let mut w = self.0.lock().unwrap();
        let now = w.now;
        w.calls.push((now, "refresh".into()));
        if let Some(e) = w.refresh_errors.pop_front() {
            return Err(e);
        }
        if refresh.expose() != w.valid_refresh {
            return Err(AuthError::Refused);
        }
        w.issued += 1;
        w.valid_refresh = format!("rt-{}", w.issued);
        w.valid_access = format!("at-{}", w.issued);
        Ok(Tokens {
            access: Secret::new(w.valid_access.clone()),
            refresh: Some(Secret::new(w.valid_refresh.clone())),
            expires_in_s: w.expires_in_s.unwrap_or(8 * 3600),
        })
    }
}

/// Streams that the test feeds by hand.
#[derive(Clone, Default)]
struct FakeStreams(Arc<Mutex<Vec<Opened>>>);

/// A stream the poller opened: its tag, and the way to feed it.
type Opened = (String, mpsc::UnboundedSender<StreamEvent>);

impl Streamer for FakeStreams {
    fn open(&self, tag: &str, _access: &Secret) -> Option<StreamHandle> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.0.lock().unwrap().push((tag.to_string(), tx));
        Some(StreamHandle::new(rx, None))
    }
}

struct Rig {
    _cadence: tesla::Cadence,
    _t: tempfile::TempDir,
    store: Store,
    tesla: FakeTesla,
    sink: Arc<MemSink>,
    _streams: FakeStreams,
    p: Poller<FakeTesla, MemSink, FakeStreams>,
    now: u64,
}

fn rig(matched: Option<Match>, cadence: tesla::Cadence) -> Rig {
    let (t, data, v) = vault();
    let store = Store::new(&data, v);
    let account = Account {
        id: String::new(),
        label: "Test".into(),
        region: tesla::Region::Global,
        redirect: tesla::Redirect::App,
        added_ms: T0,
        version: 0,
        paused: false,
        location: tesla::Location::default(),
        state: AccountState::Ok,
        last_ok_ms: None,
        last_error: None,
        cars: vec![AccountCar {
            vehicle: "tv1".into(),
            tesla_id: "1001".into(),
            tesla_vehicle_id: Some("501".into()),
            vin_prefix: Some("5YJ3E1EA0PF".into()),
            vin_tag: None,
            model: Some('3'),
            year: Some(2023),
            display_name: Some("Made-up three".into()),
            matched,
        }],
    };
    let creds = Credentials {
        refresh: Secret::new("rt-0"),
        access: Some(Secret::new("at-0")),
        access_expires_ms: T0 + 8 * 3_600_000,
    };
    let a = store.add(account, &creds).unwrap();
    assert_eq!(a.id, "ta1");
    let tesla = FakeTesla::default();
    tesla.set(|w| {
        w.valid_refresh = "rt-0".into();
        w.valid_access = "at-0".into();
        w.state = Some(CarState::Asleep);
        w.store = Some(store.clone());
        w.stored_before_use = true;
        w.now = T0;
    });
    let sink = Arc::new(MemSink::default());
    let streams = FakeStreams::default();
    let p = Poller::new(store.clone(), "ta1", tesla.clone(), sink.clone(), streams.clone(), cadence.clone()).unwrap();
    Rig { _cadence: cadence, _t: t, store, tesla, sink, _streams: streams, p, now: T0 }
}

impl Rig {
    /// Runs the poller on the virtual clock until `until` (ms after T0).
    async fn run(&mut self, until: u64) {
        let end = T0 + until;
        while self.now < end {
            self.tesla.set(|w| w.now = self.now);
            let next = self.p.tick(self.now).await.expect("account still there");
            assert!(next > self.now, "the poller must move on");
            self.now = next.min(end);
        }
    }
}

fn looks(r: &Rig) -> Vec<u64> {
    let mut all = [r.tesla.calls("data"), r.tesla.calls("data+location")].concat();
    all.sort_unstable();
    all
}

/// With the list read slowed to 10 min (allowed up to 60) an asleep car must still be recorded as asleep.
/// Looks further apart than a fixed join window would each be a zero-length fact, which the engine drops.
#[tokio::test]
async fn a_slow_list_read_still_records_the_asleep_time() {
    let slow = tesla::Intervals { list_ms: 10 * MIN, ..tesla::Intervals::DEFAULT };
    let mut r = rig(Some(Match::Car { car: 7 }), slow.cadence());
    r.run(90 * MIN).await;
    let facts: Vec<tesla::ApiFact> = r.sink.points.lock().unwrap().iter().flat_map(|p| p.2.clone()).collect();
    assert!(!facts.is_empty(), "the poller handed on nothing");
    assert!(
        facts.iter().any(|f| f.state == tesla::ApiState::Asleep && f.end_ms > f.start_ms),
        "every asleep look is zero-length (dropped by the engine): {facts:?}"
    );
}

/// Signing in again during a 429 wait ends the back-off and the 'rate limited until' mark with it.
#[tokio::test]
async fn signing_in_again_ends_the_rate_limited_status() {
    let mut r = rig(Some(Match::Car { car: 7 }), tesla::Cadence::default());
    r.tesla.set(|w| w.list_errors.push_back(ApiError::RateLimited { retry_after_s: Some(3600) }));
    r.run(1000).await;
    assert!(r.p.status().rate_limited_until_ms.is_some());
    // the admin signs in again with a new refresh token
    r.tesla.set(|w| {
        w.valid_refresh = "rt-new".into();
        w.valid_access = "at-new".into();
    });
    r.store
        .update("ta1", None, |_, c| {
            *c = Credentials {
                refresh: Secret::new("rt-new"),
                access: Some(Secret::new("at-new")),
                access_expires_ms: T0 + 8 * 3_600_000,
            }
        })
        .unwrap();
    r.run(10 * MIN).await;
    assert!(r.tesla.calls("list").len() > 5, "polling went on at once: {:?}", r.tesla.calls("list"));
    assert_eq!(r.p.status().rate_limited_until_ms, None, "still says Tesla is limiting requests");
}

/// The server clock is set back an hour while a car charges: reads must go on.
#[tokio::test]
async fn a_clock_set_back_does_not_stop_the_polling() {
    let mut r = rig(Some(Match::Car { car: 7 }), tesla::Cadence::default());
    let mut d = parked_data();
    d["charge_state"] = json!({"charging_state": "Charging", "charger_power": 7, "battery_level": 40});
    r.tesla.set(|w| {
        w.state = Some(CarState::Online);
        w.data = Some(d);
    });
    r.run(2 * 60 * MIN).await;
    let n = looks(&r).len();
    r.now -= 60 * MIN; // NTP corrects the clock by an hour
    r.run(60 * MIN + 10 * MIN).await; // ten minutes of the new time
    let after = looks(&r).len() - n;
    assert!(after > 50, "only {after} reads in 10 minutes of a charging car after the clock went back");
}
