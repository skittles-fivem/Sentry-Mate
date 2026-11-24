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
    Account, AccountCar, AccountState, ApiError, AuthError, CarState, Credentials, FullVin, Match, Mode, Poller,
    Secret, Store, Tesla, Tokens, Vehicle,
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
    #[allow(dead_code)]
    cadence: tesla::Cadence,
    _t: tempfile::TempDir,
    store: Store,
    tesla: FakeTesla,
    sink: Arc<MemSink>,
    streams: FakeStreams,
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
    Rig { cadence, _t: t, store, tesla, sink, streams, p, now: T0 }
}

impl Rig {
    /// A new poller for the same account, as after a restart or a settings change.
    fn restart(&mut self) {
        self.p = Poller::new(
            self.store.clone(),
            "ta1",
            self.tesla.clone(),
            self.sink.clone(),
            self.streams.clone(),
            self.cadence.clone(),
        )
        .unwrap();
    }

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

#[tokio::test]
async fn an_asleep_car_gets_only_the_list_every_30_seconds() {
    let mut r = rig(Some(Match::Car { car: 7 }), tesla::Cadence::default());
    r.run(2 * 3_600_000).await;
    let lists = r.tesla.calls("list");
    assert!(r.tesla.calls("data").is_empty() && r.tesla.calls("data+location").is_empty());
    assert!((239..=241).contains(&lists.len()), "{} lists", lists.len());
    assert!(lists.windows(2).all(|w| w[1] - w[0] == 30_000));
    // It said asleep, each look joined to the one before.
    let pts = r.sink.points.lock().unwrap();
    let last = pts.last().unwrap();
    assert_eq!(last.0, 7);
    assert_eq!(last.2[0].state, tesla::ApiState::Asleep);
    assert_eq!(last.2[0].end_ms - last.2[0].start_ms, 30_000);
    assert!(r.tesla.calls("refresh").is_empty());
}

#[tokio::test]
async fn a_parked_car_is_left_to_fall_asleep() {
    let mut r = rig(Some(Match::Car { car: 7 }), tesla::Cadence::default());
    r.tesla.set(|w| w.state = Some(CarState::Online));
    r.run(60 * MIN).await;
    let data = r.tesla.calls("data");
    let first = r.tesla.calls("data+location");
    // The first look after it woke asks for the position; then not (parked).
    assert_eq!(first.len(), 1);
    // About ten minutes of looks once a minute, then 21 quiet minutes, then one look, then quiet again.
    let quiet: Vec<u64> = data.windows(2).map(|w| w[1] - w[0]).filter(|g| *g > MIN).collect();
    assert!(quiet.iter().all(|g| *g == 21 * MIN), "{quiet:?}");
    assert!(!quiet.is_empty());
    let before_quiet = data.iter().filter(|t| **t <= T0 + 10 * MIN).count();
    assert!((9..=11).contains(&before_quiet), "{before_quiet}");
    let st = r.p.status();
    assert!(matches!(st.cars[0].mode, Mode::LettingSleep | Mode::OnlineIdle));
    // The list kept running meanwhile, and once it says asleep, no more data calls at all.
    r.tesla.set(|w| w.state = Some(CarState::Asleep));
    let calls_before = r.tesla.calls("data").len();
    r.run(3 * 3_600_000).await;
    assert_eq!(r.tesla.calls("data").len(), calls_before);
    assert_eq!(r.p.status().cars[0].mode, Mode::WatchingList);
    // Values went to the sink once (they did not change).
    let metas = r.sink.metas.lock().unwrap();
    assert_eq!(metas.len(), 1);
    assert_eq!(metas[0].1.car_version.as_deref(), Some("2026.32.5"));
}

#[tokio::test]
async fn something_keeping_the_car_awake_is_watched_without_letting_it_sleep() {
    let mut r = rig(Some(Match::Car { car: 7 }), tesla::Cadence::default());
    let mut d = parked_data();
    d["vehicle_state"]["sentry_mode"] = json!(true);
    r.tesla.set(|w| {
        w.state = Some(CarState::Online);
        w.data = Some(d);
    });
    r.run(40 * MIN).await;
    let data = r.tesla.calls("data");
    assert!(data.windows(2).all(|w| w[1] - w[0] == MIN), "{data:?}");
    assert_eq!(r.p.status().cars[0].mode, Mode::OnlineIdle);
}

#[tokio::test]
async fn charging_every_5_seconds_whatever_else_is_live() {
    let mut r = rig(Some(Match::Car { car: 7 }), tesla::Cadence::default());
    let mut d = parked_data();
    d["charge_state"] = json!({"charging_state": "Charging", "charger_power": 11, "battery_level": 50});
    r.tesla.set(|w| {
        w.state = Some(CarState::Online);
        w.data = Some(d);
    });
    r.run(10 * MIN).await;
    let all: Vec<u64> = [r.tesla.calls("data"), r.tesla.calls("data+location")].concat();
    let mut all = all;
    all.sort_unstable();
    assert!(all.windows(2).all(|w| w[1] - w[0] == 5_000), "{all:?}");
    // Charging: the position is asked for.
    assert!(r.tesla.calls("data").is_empty());
    let n = all.len();
    // SMO live or not, the charging pace stays (no input waits on another): 30 more minutes
    r.run(40 * MIN).await;
    let later = r.tesla.calls("data+location").len() - n;
    assert!(later >= 355, "{later} looks in 30 min: no slower pace while another input is live");
    let pts = r.sink.points.lock().unwrap();
    assert!(pts.iter().any(|(_, s, _)| s.first().is_some_and(|s| s.charge_power_kw == Some(11.0))));
}

#[tokio::test]
async fn a_drive_opens_the_stream_and_its_samples_reach_the_sink() {
    let mut r = rig(Some(Match::Car { car: 7 }), tesla::Cadence::default());
    let mut d = parked_data();
    d["drive_state"]["shift_state"] = json!("D");
    d["drive_state"]["speed"] = json!(30);
    r.tesla.set(|w| {
        w.state = Some(CarState::Online);
        w.data = Some(d);
    });
    r.run(2 * MIN).await;
    {
        let s = r.streams.0.lock().unwrap();
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].0, "501");
        let sample = model::Sample { t_ms: T0 + 61_000, speed_kmh: Some(50.0), ..model::Sample::default() };
        s[0].1.send(StreamEvent::Samples(vec![sample])).unwrap();
    }
    r.run(3 * MIN).await;
    assert!(r.p.status().cars[0].streaming);
    let pts = r.sink.points.lock().unwrap().clone();
    assert!(pts.iter().any(|(_, s, f)| f.is_empty() && s.iter().any(|x| x.speed_kmh == Some(50.0))));
    // With the stream up, vehicle_data every 2.5 s beside it.
    let looks = r.tesla.calls("data+location");
    assert!(looks.windows(2).all(|w| w[1] - w[0] == 2_500), "{looks:?}");
    // The stream ends: polling goes on at the same pace, and a new stream on the next look.
    r.streams.0.lock().unwrap()[0].1.send(StreamEvent::Ended("quiet")).unwrap();
    r.run(4 * MIN).await;
    assert_eq!(r.streams.0.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn the_token_is_refreshed_ahead_and_stored_before_use() {
    let mut r = rig(Some(Match::Car { car: 7 }), tesla::Cadence::default());
    r.tesla.set(|w| w.state = Some(CarState::Online));
    // 8 h of access token: refreshed 30 min before it ends, again 8 h later.
    r.run(17 * 3_600_000).await;
    let refreshes = r.tesla.calls("refresh");
    assert_eq!(refreshes.len(), 2, "{refreshes:?}");
    // At the first look once under 30 minutes are left (looks come once a minute here).
    let due = T0 + 8 * 3_600_000 - 30 * MIN;
    assert!((due..due + 2 * MIN).contains(&refreshes[0]), "{refreshes:?}");
    let c = r.store.credentials("ta1").unwrap();
    assert_eq!(c.refresh.expose(), "rt-2");
    assert!(r.tesla.0.lock().unwrap().stored_before_use);
}

#[tokio::test]
async fn a_401_refreshes_once_and_a_refused_refresh_stops_the_account() {
    let mut r = rig(Some(Match::Car { car: 7 }), tesla::Cadence::default());
    // Tesla stops taking the access token: one refresh, then on.
    r.tesla.set(|w| w.valid_access = "something-else".into());
    r.run(5 * MIN).await;
    assert_eq!(r.tesla.calls("refresh").len(), 1);
    assert_eq!(r.p.status().state, AccountState::Ok);
    // Now Tesla refuses the refresh token too: sign-in needed, and not one more call.
    r.tesla.set(|w| {
        w.valid_access = "gone".into();
        w.valid_refresh = "gone".into();
    });
    r.run(10 * MIN).await;
    let n = r.tesla.0.lock().unwrap().calls.len();
    assert_eq!(r.p.status().state, AccountState::SignInNeeded);
    assert_eq!(r.store.get("ta1").unwrap().state, AccountState::SignInNeeded);
    assert_eq!(r.store.get("ta1").unwrap().last_error.unwrap().code, "sign_in_needed");
    assert!(r.sink.changed.lock().unwrap().contains(&AccountState::SignInNeeded));
    r.run(6 * 3_600_000).await;
    assert_eq!(r.tesla.0.lock().unwrap().calls.len(), n, "calls after a refused sign-in");
    assert!(r.p.status().cars.iter().all(|c| c.mode == Mode::Off));
}

#[tokio::test]
async fn waits_as_tesla_asks_and_backs_off_on_errors() {
    let mut r = rig(Some(Match::Car { car: 7 }), tesla::Cadence::default());
    r.tesla.set(|w| {
        w.list_errors.push_back(ApiError::RateLimited { retry_after_s: Some(120) });
        w.list_errors.push_back(ApiError::RateLimited { retry_after_s: None });
        w.list_errors.push_back(ApiError::Tesla { status: 503 });
        w.list_errors.push_back(ApiError::Tesla { status: 503 });
        w.list_errors.push_back(ApiError::Tesla { status: 503 });
    });
    r.run(30 * MIN).await;
    let l = r.tesla.calls("list");
    let gaps: Vec<u64> = l.windows(2).map(|w| w[1] - w[0]).take(5).collect();
    assert_eq!(gaps, vec![120_000, 300_000, 30_000, 60_000, 120_000]);
}

#[tokio::test]
async fn a_408_is_taken_as_asleep_and_the_car_is_not_woken() {
    let mut r = rig(Some(Match::Car { car: 7 }), tesla::Cadence::default());
    r.tesla.set(|w| {
        w.state = Some(CarState::Online);
        w.data_errors.push_back(ApiError::Unavailable);
    });
    r.run(30 * 1000).await;
    assert_eq!(r.p.status().cars[0].mode, Mode::WatchingList);
    r.tesla.set(|w| w.state = Some(CarState::Asleep));
    r.run(30 * MIN).await;
    assert_eq!(r.tesla.calls("data+location").len(), 1);
}

#[tokio::test]
async fn forbidden_and_closed_endpoints_stop_polling() {
    let mut r = rig(Some(Match::Car { car: 7 }), tesla::Cadence::default());
    r.tesla.set(|w| w.list_errors.push_back(ApiError::Forbidden));
    r.run(59 * MIN).await;
    assert_eq!(r.tesla.calls("list").len(), 1);
    assert_eq!(r.p.status().state, AccountState::Error);
    assert_eq!(r.p.status().last_error.unwrap().code, "forbidden");
    r.run(60 * MIN + 10_000).await;
    assert_eq!(r.tesla.calls("list").len(), 2, "tried again after an hour");
    assert_eq!(r.p.status().state, AccountState::Ok);
    r.tesla.set(|w| w.list_errors.push_back(ApiError::EndpointGone));
    r.run(63 * MIN).await;
    let n = r.tesla.calls("list").len();
    r.run(10 * 3_600_000).await;
    assert_eq!(r.tesla.calls("list").len(), n);
    assert_eq!(r.p.status().last_error.unwrap().code, "endpoint_gone");
}

#[tokio::test]
async fn unmatched_cars_are_not_polled_and_new_ones_become_cars() {
    let mut r = rig(None, tesla::Cadence::default());
    r.run(30 * MIN).await;
    assert!(r.tesla.0.lock().unwrap().calls.is_empty());
    r.store.update("ta1", None, |a, _| a.cars[0].matched = Some(Match::New)).unwrap();
    r.run(32 * MIN).await;
    let made = r.sink.new_cars.lock().unwrap().clone();
    assert_eq!(made.len(), 1);
    assert_eq!(made[0].vin_prefix.as_deref(), Some("5YJ3E1EA0PF"));
    assert_eq!(r.store.get("ta1").unwrap().cars[0].matched, Some(Match::Car { car: 101 }));
    assert!(!r.tesla.calls("list").is_empty());
    // Paused: nothing.
    r.store.update("ta1", None, |a, _| a.paused = true).unwrap();
    let n = r.tesla.0.lock().unwrap().calls.len();
    r.run(3 * 3_600_000).await;
    assert_eq!(r.tesla.0.lock().unwrap().calls.len(), n);
    // Removed: the poller ends.
    r.store.remove("ta1").unwrap();
    assert!(r.p.tick(r.now).await.is_none());
}

/// The looks at the car (`vehicle_data`, with the position or not), in order.
fn looks(r: &Rig) -> Vec<u64> {
    let mut all = [r.tesla.calls("data"), r.tesla.calls("data+location")].concat();
    all.sort_unstable();
    all
}

#[tokio::test]
async fn each_mode_reads_at_its_interval_and_slower_ones_are_kept() {
    let slow = tesla::Intervals { driving_ms: 10_000, charging_ms: 20_000, idle_ms: 120_000, list_ms: 90_000 };
    for (i, (driving, charging, idle, list)) in
        [(tesla::Intervals::DEFAULT, (2_500, 5_000, 60_000, 30_000)), (slow, (10_000, 20_000, 120_000, 90_000))]
    {
        // asleep: only the list
        let mut r = rig(Some(Match::Car { car: 7 }), i.cadence());
        r.run(30 * MIN).await;
        let l = r.tesla.calls("list");
        assert!(l.windows(2).all(|w| w[1] - w[0] == list), "{l:?}");
        assert!(looks(&r).is_empty());
        // online and idle (before letting it sleep)
        let mut r = rig(Some(Match::Car { car: 7 }), i.cadence());
        r.tesla.set(|w| w.state = Some(CarState::Online));
        r.run(9 * MIN).await;
        let t = looks(&r);
        assert!(t.len() >= 2 && t.windows(2).all(|w| w[1] - w[0] == idle), "{t:?}");
        // charging
        let mut r = rig(Some(Match::Car { car: 7 }), i.cadence());
        let mut d = parked_data();
        d["charge_state"] = json!({"charging_state": "Charging", "charger_power": 7, "battery_level": 40});
        r.tesla.set(|w| {
            w.state = Some(CarState::Online);
            w.data = Some(d);
        });
        r.run(5 * MIN).await;
        let t = looks(&r);
        assert!(t.windows(2).all(|w| w[1] - w[0] == charging), "{t:?}");
        assert_eq!(r.p.status().cars[0].mode, Mode::Charging);
        // driving, the stream open beside it
        let mut r = rig(Some(Match::Car { car: 7 }), i.cadence());
        let mut d = parked_data();
        d["drive_state"]["shift_state"] = json!("D");
        r.tesla.set(|w| {
            w.state = Some(CarState::Online);
            w.data = Some(d);
        });
        r.run(5 * MIN).await;
        let t = looks(&r);
        assert!(t.len() > 20 && t.windows(2).all(|w| w[1] - w[0] == driving), "{t:?}");
        assert!(r.p.status().cars[0].streaming);
        assert_eq!(r.p.status().cars[0].mode, Mode::Driving);
    }
}

#[tokio::test]
async fn the_floors_hold_whatever_is_asked() {
    let wild = tesla::Intervals { driving_ms: 1, charging_ms: 1_000, idle_ms: 10_000, list_ms: 0 };
    let c = wild.cadence();
    assert_eq!((c.driving_ms, c.charging_ms, c.idle_ms, c.list_ms), (2_500, 5_000, 60_000, 30_000));
    let huge = tesla::Intervals { driving_ms: u64::MAX, ..tesla::Intervals::DEFAULT };
    assert_eq!(huge.clamped(), tesla::Intervals { driving_ms: 600_000, ..tesla::Intervals::DEFAULT });
    assert_eq!(tesla::Intervals::DEFAULT.cadence(), tesla::Cadence::default());
    assert_eq!(tesla::Intervals::of(&tesla::Cadence::default()), tesla::Intervals::DEFAULT);
    assert_eq!(tesla::Intervals::FLOOR, tesla::Intervals::DEFAULT);
}

#[tokio::test]
async fn no_cap_of_its_own_a_day_of_driving_makes_every_request() {
    let mut r = rig(Some(Match::Car { car: 7 }), tesla::Cadence::default());
    let mut d = parked_data();
    d["drive_state"]["shift_state"] = json!("D");
    r.tesla.set(|w| {
        w.state = Some(CarState::Online);
        w.data = Some(d);
    });
    // from midnight UTC to midnight: every 2.5 s, all day
    let start = (T0 / 86_400_000 + 1) * 86_400_000 - T0;
    r.run(start).await;
    let before = looks(&r).len();
    r.run(start + 86_400_000).await;
    let t = looks(&r);
    let day = &t[before..];
    assert!(day.len() >= 34_559, "{} looks in a day", day.len());
    // (a token refresh takes a turn: that look comes a moment later)
    assert!(day.windows(2).all(|w| (2_500..=2_502).contains(&(w[1] - w[0]))));
    let st = r.p.status();
    assert!(st.polls_today > 0 && st.last_error.is_none(), "{st:?}");
    assert!(st.rate_limited_until_ms.is_none());
    assert_eq!(st.cars[0].mode, Mode::Driving);
    assert!(r.sink.limited.lock().unwrap().is_empty());
}

#[tokio::test]
async fn requests_go_one_at_a_time_with_no_gap_of_their_own() {
    // the list says online: the first look follows at once, not 2.5 s later
    let mut r = rig(Some(Match::Car { car: 7 }), tesla::Cadence::default());
    r.tesla.set(|w| w.state = Some(CarState::Online));
    r.run(MIN).await;
    let (l, d) = (r.tesla.calls("list"), looks(&r));
    assert!(d[0] > l[0] && d[0] - l[0] < 100, "{l:?} {d:?}");
    // never two at the same instant
    let mut all: Vec<u64> = r.tesla.0.lock().unwrap().calls.iter().map(|c| c.0).collect();
    all.sort_unstable();
    assert!(all.windows(2).all(|w| w[1] > w[0]));
}

#[tokio::test]
async fn a_429_while_driving_waits_as_before() {
    let mut r = rig(Some(Match::Car { car: 7 }), tesla::Cadence::default());
    let mut d = parked_data();
    d["drive_state"]["shift_state"] = json!("D");
    r.tesla.set(|w| {
        w.state = Some(CarState::Online);
        w.data = Some(d);
    });
    r.run(MIN).await;
    // Retry-After under a minute: a minute; none: 5 min; then the driving pace again
    r.tesla.set(|w| {
        w.data_errors.push_back(ApiError::RateLimited { retry_after_s: Some(3) });
        w.data_errors.push_back(ApiError::RateLimited { retry_after_s: None });
    });
    r.run(10 * MIN).await;
    let t = looks(&r);
    let gaps: Vec<u64> = t.windows(2).map(|w| w[1] - w[0]).filter(|g| *g != 2_500).collect();
    assert_eq!(gaps, vec![60_000, 300_000], "{t:?}");
    // each wait told once: the second 429 came after the first wait ended, so it is a new episode
    let limited = r.sink.limited.lock().unwrap().clone();
    assert_eq!(limited.len(), 2, "{limited:?}");
    assert!(limited.iter().all(|(a, _)| a == "ta1"));
    // the wait is saved: a restart during one keeps it, and the status says until when
    r.tesla.set(|w| w.data_errors.push_back(ApiError::RateLimited { retry_after_s: Some(600) }));
    r.run(11 * MIN).await;
    let n = looks(&r).len();
    let until = r.sink.limited.lock().unwrap().last().unwrap().1;
    assert_eq!(r.p.status().rate_limited_until_ms, Some(until));
    assert_eq!(r.p.status().cars[0].mode, Mode::Backoff);
    assert_eq!(r.p.status().last_error.unwrap().code, "rate_limited");
    r.restart();
    r.run(20 * MIN).await;
    assert_eq!(looks(&r).len(), n, "nothing asked during Tesla's wait, restarted or not");
    assert_eq!(r.p.status().rate_limited_until_ms, Some(until));
    assert_eq!(r.sink.limited.lock().unwrap().len(), 3, "the restart is not a new episode");
    r.run(25 * MIN).await;
    assert!(looks(&r).len() > n);
    assert!(r.p.status().rate_limited_until_ms.is_none(), "the wait is over");
}

#[tokio::test]
async fn a_refused_stream_refreshes_once_then_polling_goes_on_alone() {
    let mut r = rig(Some(Match::Car { car: 7 }), tesla::Cadence::default());
    let mut d = parked_data();
    d["drive_state"]["shift_state"] = json!("D");
    r.tesla.set(|w| {
        w.state = Some(CarState::Online);
        w.data = Some(d);
    });
    r.run(MIN).await;
    assert_eq!(r.streams.0.lock().unwrap().len(), 1);
    r.streams.0.lock().unwrap()[0].1.send(StreamEvent::AuthFailed).unwrap();
    r.streams.0.lock().unwrap()[0].1.send(StreamEvent::Ended("refused")).unwrap();
    r.run(3 * MIN).await;
    assert_eq!(r.tesla.calls("refresh").len(), 1);
    assert_eq!(r.streams.0.lock().unwrap().len(), 2, "tried again with the new token");
    r.streams.0.lock().unwrap()[1].1.send(StreamEvent::AuthFailed).unwrap();
    r.streams.0.lock().unwrap()[1].1.send(StreamEvent::Ended("refused")).unwrap();
    r.run(10 * MIN).await;
    assert_eq!(r.streams.0.lock().unwrap().len(), 2, "no third try in this drive");
    assert_eq!(r.tesla.calls("refresh").len(), 1);
    let looks = r.tesla.calls("data+location");
    let late: Vec<u64> = looks.iter().copied().filter(|t| *t > T0 + 4 * MIN).collect();
    assert!(late.windows(2).all(|w| w[1] - w[0] == 2_500), "{late:?}");
}

#[tokio::test]
async fn a_short_lived_token_is_not_refreshed_over_and_over() {
    let mut r = rig(Some(Match::Car { car: 7 }), tesla::Cadence::default());
    // Tesla gives five-minute tokens, and the one held now is about to end.
    r.tesla.set(|w| w.expires_in_s = Some(300));
    r.store.update("ta1", None, |_, c| c.access_expires_ms = T0 + MIN).unwrap();
    r.restart();
    r.run(10 * MIN).await;
    let refreshes = r.tesla.calls("refresh").len();
    let lists = r.tesla.calls("list").len();
    // One at once, then one near the end of each five-minute token.
    assert!(refreshes <= 4, "{refreshes} refreshes in 10 min");
    assert!(lists >= 8, "{lists} lists in 10 min");
    // The token in use never ran out.
    let c = r.store.credentials("ta1").unwrap();
    assert!(c.access_expires_ms > r.now);
}

#[tokio::test]
async fn tesla_s_wait_outlives_a_restart() {
    let mut r = rig(Some(Match::Car { car: 7 }), tesla::Cadence::default());
    r.tesla.set(|w| w.list_errors.push_back(ApiError::RateLimited { retry_after_s: Some(3600) }));
    r.run(1000).await;
    assert_eq!(r.tesla.calls("list").len(), 1);
    // The poller starts again (a new proxy, a new pace, a restart): Tesla's hour still holds.
    r.restart();
    r.run(59 * MIN).await;
    assert_eq!(r.tesla.calls("list").len(), 1, "asked again inside Tesla's wait");
    r.run(60 * MIN + 30_000).await;
    assert_eq!(r.tesla.calls("list").len(), 2);
    // Saving the pacing on each request does not change the account's version an admin edits with.
    let v = r.store.get("ta1").unwrap().version;
    r.run(65 * MIN).await;
    assert!(r.tesla.calls("list").len() >= 6);
    assert_eq!(r.store.get("ta1").unwrap().version, v);
}

#[tokio::test]
async fn a_closed_endpoint_and_the_day_s_count_outlive_a_restart() {
    let mut r = rig(Some(Match::Car { car: 7 }), tesla::Cadence::default());
    r.run(30 * MIN).await;
    let n = r.p.status().polls_today;
    assert_eq!(n, r.tesla.calls("list").len() as u64);
    r.restart();
    assert_eq!(r.p.status().polls_today, n, "the day's count was forgotten");

    let mut r = rig(Some(Match::Car { car: 7 }), tesla::Cadence::default());
    r.tesla.set(|w| w.list_errors.push_back(ApiError::EndpointGone));
    r.run(5 * MIN).await;
    assert_eq!(r.tesla.calls("list").len(), 1);
    r.restart();
    r.run(3 * 3_600_000).await;
    assert_eq!(r.tesla.calls("list").len(), 1, "asked a closed endpoint again");
}

#[tokio::test]
async fn a_huge_retry_after_neither_panics_nor_drops_the_wait() {
    let mut r = rig(Some(Match::Car { car: 7 }), tesla::Cadence::default());
    r.tesla.set(|w| w.list_errors.push_back(ApiError::RateLimited { retry_after_s: Some(u64::MAX) }));
    let next = r.p.tick(T0).await.unwrap();
    assert!(next >= T0 + 3_600_000, "no wait: {}", next.saturating_sub(T0));
    r.run(3 * 3_600_000).await;
    assert_eq!(r.tesla.calls("list").len(), 1);
}
