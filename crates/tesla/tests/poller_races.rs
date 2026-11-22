//! The poller against changes that land while a request is out: an admin's new match, the stream's last seconds,
//! and errors that make it wait. A fake Tesla, no network; everything is made up.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod common;
use common::{MemSink, parked_data, vault};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tesla::stream::{StreamEvent, StreamHandle, Streamer};
use tesla::*;
use tokio::sync::mpsc;
const T0: u64 = 1_800_000_000_000;

#[derive(Default)]
struct W {
    store: Option<Store>,
    lists: u32,
    data: Option<Value>,
    data_error: Option<u16>,
    during_data: Option<Box<dyn FnMut() + Send>>,
}
#[derive(Clone, Default)]
struct F(Arc<Mutex<W>>);
impl Tesla for F {
    async fn products(&self, _a: &Secret) -> Result<Vec<Vehicle>, ApiError> {
        let mut w = self.0.lock().unwrap();
        w.lists += 1;
        if w.lists == 1 {
            // The admin re-matches the vehicle to car 9 while this request is out.
            let s = w.store.clone().unwrap();
            let v = s.get("ta1").unwrap().version;
            s.update("ta1", Some(v), |a, _| a.cars[0].matched = Some(Match::Car { car: 9 })).unwrap();
        }
        Ok(vec![Vehicle {
            id: "1001".into(),
            vehicle_id: Some("501".into()),
            vin: None,
            display_name: Some("Made-up three".into()),
            state: CarState::Online,
        }])
    }
    async fn vehicle_data(&self, _a: &Secret, _id: &str, _l: bool) -> Result<Value, ApiError> {
        let mut w = self.0.lock().unwrap();
        if let Some(f) = w.during_data.as_mut() {
            f();
        }
        if let Some(status) = w.data_error {
            return Err(ApiError::Tesla { status });
        }
        Ok(w.data.clone().unwrap_or_else(parked_data))
    }
    async fn refresh(&self, _r: &Secret) -> Result<Tokens, AuthError> {
        Err(AuthError::Refused)
    }
}
#[derive(Clone, Default)]
struct S(Arc<Mutex<Vec<mpsc::UnboundedSender<StreamEvent>>>>);
impl Streamer for S {
    fn open(&self, _t: &str, _a: &Secret) -> Option<StreamHandle> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.0.lock().unwrap().push(tx);
        Some(StreamHandle::new(rx, None))
    }
}
fn setup(matched: Match) -> (tempfile::TempDir, Store, F, Arc<MemSink>, S, Poller<F, MemSink, S>) {
    let (t, data, v) = vault();
    let store = Store::new(&data, v);
    let account = Account {
        id: String::new(),
        label: "T".into(),
        region: Region::Global,
        redirect: Redirect::App,
        added_ms: T0,
        version: 0,
        paused: false,
        location: Location::default(),
        state: AccountState::Ok,
        last_ok_ms: None,
        last_error: None,
        cars: vec![AccountCar {
            vehicle: "tv1".into(),
            tesla_id: "1001".into(),
            tesla_vehicle_id: Some("501".into()),
            vin_prefix: None,
            vin_tag: None,
            model: None,
            year: None,
            display_name: Some("Made-up three".into()),
            matched: Some(matched),
        }],
    };
    store
        .add(
            account,
            &Credentials {
                refresh: Secret::new("rt"),
                access: Some(Secret::new("at")),
                access_expires_ms: T0 + 8 * 3_600_000,
            },
        )
        .unwrap();
    let f = F::default();
    f.0.lock().unwrap().store = Some(store.clone());
    let sink = Arc::new(MemSink::default());
    let s = S::default();
    let p = Poller::new(store.clone(), "ta1", f.clone(), sink.clone(), s.clone(), Cadence::default()).unwrap();
    (t, store, f, sink, s, p)
}

#[tokio::test]
async fn a_match_made_while_a_request_is_out_is_followed() {
    let (_t, store, _f, sink, _s, mut p) = setup(Match::Car { car: 7 });
    let mut now = T0;
    for _ in 0..20 {
        now = p.tick(now).await.unwrap();
    }
    assert_eq!(store.get("ta1").unwrap().cars[0].matched, Some(Match::Car { car: 9 }));
    let cars: Vec<u32> = sink.points.lock().unwrap().iter().map(|x| x.0).collect();
    assert!(cars.iter().skip(1).all(|c| *c == 9), "data still goes to the old car: {cars:?}");
}

#[tokio::test]
async fn the_stream_s_last_seconds_are_kept_when_the_car_parks() {
    let (_t, _store, f, sink, s, mut p) = setup(Match::Car { car: 7 });
    f.0.lock().unwrap().lists = 5;
    let mut driving = parked_data();
    driving["drive_state"]["shift_state"] = json!("D");
    driving["drive_state"]["speed"] = json!(30);
    driving["drive_state"]["timestamp"] = json!(T0);
    f.0.lock().unwrap().data = Some(driving);
    let mut now = T0;
    for _ in 0..3 {
        now = p.tick(now).await.unwrap();
    }
    assert_eq!(s.0.lock().unwrap().len(), 1, "stream opened");
    // Car parks; the stream's last records arrive while the vehicle_data request is out.
    let mut parked = parked_data();
    parked["drive_state"]["timestamp"] = json!(T0 + 120_000);
    let tx = s.0.lock().unwrap()[0].clone();
    {
        let mut w = f.0.lock().unwrap();
        w.data = Some(parked);
        w.during_data = Some(Box::new(move || {
            let smp = model::Sample { t_ms: T0 + 119_000, speed_kmh: Some(3.0), ..Default::default() };
            let _ = tx.send(StreamEvent::Samples(vec![smp]));
        }));
    }
    for _ in 0..40 {
        now = p.tick(now + 1).await.unwrap();
        if now > T0 + 10 * 60_000 {
            break;
        }
    }
    let got = sink.points.lock().unwrap().iter().flat_map(|x| x.1.clone()).any(|x| x.t_ms == T0 + 119_000);
    assert!(got, "the stream sample of the last second before parking was dropped");
}

#[tokio::test]
async fn a_car_waiting_after_errors_says_backoff() {
    let (_t, _store, f, _sink, _s, mut p) = setup(Match::Car { car: 7 });
    f.0.lock().unwrap().lists = 5;
    let mut driving = parked_data();
    driving["drive_state"]["shift_state"] = json!("D");
    driving["drive_state"]["speed"] = json!(30);
    driving["drive_state"]["timestamp"] = json!(T0);
    f.0.lock().unwrap().data = Some(driving);
    let mut now = T0;
    for _ in 0..3 {
        now = p.tick(now).await.unwrap();
    }
    assert_eq!(p.status().cars[0].mode, Mode::Driving);
    // Tesla answers 503: the account waits, and says so.
    f.0.lock().unwrap().data_error = Some(503);
    let mut waits = 0;
    for _ in 0..30 {
        let at = now;
        now = p.tick(now).await.unwrap();
        let st = p.status();
        if st.last_error.is_some() {
            waits += 1;
            assert_eq!(st.cars[0].mode, Mode::Backoff, "{st:?}");
            assert!(st.cars[0].next_poll_ms.unwrap() > at, "{st:?}");
        }
    }
    assert!(waits > 0, "the poller never waited");
    // Tesla answers again: back to driving.
    f.0.lock().unwrap().data_error = None;
    for _ in 0..30 {
        now = p.tick(now).await.unwrap();
    }
    assert_eq!(p.status().cars[0].mode, Mode::Driving);
}
