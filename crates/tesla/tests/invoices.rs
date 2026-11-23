//! Supercharger invoices on a virtual clock, against a fake Tesla with made-up sessions and made-up PDFs: off by
//! default, the whole history paged once, every PDF downloaded once, the daily check, a restart in the middle of the
//! backfill, the caps, and pages numbered from 1. Everything is made up.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::{parked_data, vault};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use tesla::invoices::{Found, Page};
use tesla::stream::{StreamHandle, Streamer};
use tesla::{
    Account, AccountCar, AccountState, ApiError, AuthError, CarState, Credentials, FullVin, Match, Poller, Secret,
    Store, Tesla, Tokens, Vehicle,
};

const T0: u64 = 1_800_000_000_000;
const DAY: u64 = 86_400_000;

#[derive(Default)]
struct World {
    /// The made-up history, newest first.
    sessions: Vec<Value>,
    /// Pages are numbered from this.
    first_page: u32,
    history_calls: Vec<u32>,
    pdf_calls: Vec<String>,
    /// Ids Tesla gives no PDF for.
    no_pdf: Vec<String>,
}

#[derive(Clone, Default)]
struct FakeTesla(Arc<Mutex<World>>);

fn record(n: usize) -> Value {
    let start = 1_700_000_000 + (1000 - n as i64) * 86_400;
    let t = |s: i64| {
        let d = s.div_euclid(86_400);
        let secs = s.rem_euclid(86_400);
        // days since the epoch to a civil date (made-up times, UTC)
        let z = d + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let day = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        let y = yoe + era * 400 + i64::from(m <= 2);
        format!("{y:04}-{m:02}-{day:02}T{:02}:{:02}:{:02}Z", secs / 3600, secs / 60 % 60, secs % 60)
    };
    json!({
        "sessionId": 5_000_000 + n, "vin": common::VIN_A, "siteLocationName": format!("Made-up Supercharger {n}"),
        "chargeStartDateTime": t(start), "chargeStopDateTime": t(start + 1800), "countryCode": "US",
        "fees": [{"feeType": "CHARGING", "currencyCode": "USD", "rateBase": 0.4, "usageBase": 30, "totalDue": 12.0,
                  "netDue": 12.0, "uom": "kwh"}],
        "invoices": [{"fileName": format!("MADE-UP-{n}.pdf"), "contentId": format!("made-up-{n}")}],
    })
}

impl Tesla for FakeTesla {
    async fn products(&self, _access: &Secret) -> Result<Vec<Vehicle>, ApiError> {
        Ok(vec![Vehicle {
            id: "1001".into(),
            vehicle_id: Some("501".into()),
            vin: Some(FullVin::new(common::VIN_A)),
            display_name: Some("Made-up three".into()),
            state: CarState::Asleep,
        }])
    }

    async fn vehicle_data(&self, _access: &Secret, _id: &str, _location: bool) -> Result<Value, ApiError> {
        Ok(parked_data())
    }

    async fn refresh(&self, _refresh: &Secret) -> Result<Tokens, AuthError> {
        Err(AuthError::Refused)
    }

    async fn charging_history(&self, _access: &Secret, vin: &FullVin, page: u32) -> Result<Page, ApiError> {
        assert_eq!(vin.expose(), common::VIN_A);
        let mut w = self.0.lock().unwrap();
        w.history_calls.push(page);
        if page < w.first_page {
            return Err(ApiError::Tesla { status: 400 });
        }
        let at = (page - w.first_page) as usize * tesla::invoices::PAGE_SIZE as usize;
        let data: Vec<Value> = w.sessions.iter().skip(at).take(tesla::invoices::PAGE_SIZE as usize).cloned().collect();
        Ok(tesla::invoices::page(&json!({ "data": data })).unwrap())
    }

    async fn invoice_pdf(&self, _access: &Secret, id: &str, vin: Option<&FullVin>) -> Result<Vec<u8>, ApiError> {
        assert_eq!(vin.map(FullVin::expose), Some(common::VIN_A));
        let mut w = self.0.lock().unwrap();
        w.pdf_calls.push(id.to_string());
        if w.no_pdf.iter().any(|x| x == id) {
            return Err(ApiError::NotFound);
        }
        Ok(format!("%PDF-1.4\n% made-up invoice {id}\n").into_bytes())
    }
}

#[derive(Default)]
struct Sink {
    sessions: Mutex<BTreeMap<String, Found>>,
    pdfs: Mutex<BTreeMap<String, Vec<u8>>>,
}

impl tesla::Sink for Sink {
    fn new_car(&self, _car: &tesla::NewCar) -> Result<u32, tesla::SinkError> {
        Ok(1)
    }
    fn points(&self, _car: u32, _s: &[model::Sample], _f: &[tesla::ApiFact]) -> Result<(), tesla::SinkError> {
        Ok(())
    }
    fn meta(&self, _car: u32, _meta: &tesla::ApiMeta) -> Result<(), tesla::SinkError> {
        Ok(())
    }
    fn invoices(&self, found: &[Found]) -> Result<u64, tesla::SinkError> {
        let mut s = self.sessions.lock().unwrap();
        let mut n = 0;
        for f in found {
            n += u64::from(s.insert(f.key.clone(), f.clone()).is_none());
        }
        Ok(n)
    }
    fn invoice_known(&self, key: &str) -> bool {
        self.sessions.lock().unwrap().contains_key(key)
    }
    fn invoice_pdfs_wanted(&self, _account: &str) -> Vec<(String, String, Option<String>)> {
        let pdfs = self.pdfs.lock().unwrap();
        let mut v: Vec<&Found> = Vec::new();
        let s = self.sessions.lock().unwrap();
        v.extend(s.values().filter(|f| !pdfs.contains_key(&f.key) && f.session.invoice_id.is_some()));
        v.sort_by_key(|f| f.session.start_ms);
        v.iter().map(|f| (f.key.clone(), f.session.invoice_id.clone().unwrap(), f.vehicle.clone())).collect()
    }
    fn invoice_pdf(&self, key: &str, pdf: &[u8]) -> Result<bool, tesla::SinkError> {
        if !pdf.starts_with(b"%PDF-") {
            return Ok(false);
        }
        self.pdfs.lock().unwrap().insert(key.to_string(), pdf.to_vec());
        Ok(true)
    }
}

#[derive(Clone, Default)]
struct NoStreams;

impl Streamer for NoStreams {
    fn open(&self, _tag: &str, _access: &Secret) -> Option<StreamHandle> {
        None
    }
}

struct Rig {
    _t: tempfile::TempDir,
    store: Store,
    tesla: FakeTesla,
    sink: Arc<Sink>,
    cadence: tesla::Cadence,
    p: Poller<FakeTesla, Sink, NoStreams>,
    now: u64,
}

fn rig(sessions: usize, cadence: tesla::Cadence) -> Rig {
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
            matched: Some(Match::Car { car: 7 }),
        }],
    };
    // a token that lasts the whole test: no refresh
    let creds = Credentials {
        refresh: Secret::new("rt-0"),
        access: Some(Secret::new("at-0")),
        access_expires_ms: T0 + 400 * DAY,
    };
    store.add(account, &creds).unwrap();
    let tesla = FakeTesla::default();
    tesla.0.lock().unwrap().sessions = (0..sessions).map(record).collect();
    let sink = Arc::new(Sink::default());
    let p = Poller::new(store.clone(), "ta1", tesla.clone(), sink.clone(), NoStreams, cadence.clone()).unwrap();
    Rig { _t: t, store, tesla, sink, cadence, p, now: T0 }
}

impl Rig {
    async fn run(&mut self, for_ms: u64) {
        let end = self.now + for_ms;
        while self.now < end {
            let next = self.p.tick(self.now).await.expect("account still there");
            assert!(next > self.now);
            self.now = next.min(end);
        }
    }

    fn restart(&mut self) {
        self.p = Poller::new(
            self.store.clone(),
            "ta1",
            self.tesla.clone(),
            self.sink.clone(),
            NoStreams,
            self.cadence.clone(),
        )
        .unwrap();
    }

    fn history(&self) -> Vec<u32> {
        self.tesla.0.lock().unwrap().history_calls.clone()
    }
}

/// Invoices on; the car's list (it sleeps all along) looked at every half hour, to keep the test short.
fn on() -> tesla::Cadence {
    tesla::Cadence { invoices: true, list_ms: 30 * 60_000, ..tesla::Cadence::default() }
}

#[tokio::test]
async fn nothing_is_asked_until_invoices_are_turned_on() {
    let mut r = rig(10, tesla::Cadence { list_ms: 30 * 60_000, ..tesla::Cadence::default() });
    r.run(2 * DAY).await;
    assert!(r.history().is_empty());
    assert!(r.tesla.0.lock().unwrap().pdf_calls.is_empty());
}

#[tokio::test]
async fn the_whole_history_is_paged_once_and_every_pdf_kept_once() {
    let mut r = rig(120, on());
    r.run(60 * 60_000).await;
    // 120 sessions: pages 0, 1 (full) and 2 (short, the last)
    assert_eq!(r.history(), vec![0, 1, 2]);
    assert_eq!(r.sink.sessions.lock().unwrap().len(), 120);
    assert_eq!(r.sink.pdfs.lock().unwrap().len(), 120);
    let calls = r.tesla.0.lock().unwrap().pdf_calls.len();
    assert_eq!(calls, 120, "each PDF once");
    let p = r.store.invoice_progress("ta1").unwrap();
    assert!(p.backfill["1001"].done);
    assert_eq!(p.backfill_word(&["1001".into()]), "done");
    // no VIN is kept with the account
    assert!(!format!("{p:?}").contains("000001"));
    // a day later: the first page again, one new session on it (a full page), so the next too, nothing new there
    r.tesla.0.lock().unwrap().sessions.insert(0, record(999));
    r.run(DAY + 60 * 60_000).await;
    assert_eq!(r.history(), vec![0, 1, 2, 0, 1]);
    assert_eq!(r.sink.sessions.lock().unwrap().len(), 121);
    assert_eq!(r.sink.pdfs.lock().unwrap().len(), 121);
}

#[tokio::test]
async fn a_restart_goes_on_where_the_backfill_was() {
    let mut r = rig(120, on());
    // the first list, then one page
    let mut steps = 0;
    while r.history().len() < 2 {
        let next = r.p.tick(r.now).await.unwrap();
        r.now = next;
        steps += 1;
        assert!(steps < 100);
    }
    assert_eq!(r.history(), vec![0, 1]);
    r.restart();
    r.run(60 * 60_000).await;
    assert_eq!(r.history(), vec![0, 1, 2], "page 0 and 1 not asked again");
    assert_eq!(r.sink.sessions.lock().unwrap().len(), 120);
}

#[tokio::test]
async fn invoices_keep_to_their_daily_cap_and_leave_the_cap_to_the_cars() {
    let c = tesla::Cadence { invoice_daily_cap: 5, ..on() };
    let mut r = rig(120, c);
    r.run(DAY - (T0 % DAY) - 1).await;
    let asked = r.history().len() + r.tesla.0.lock().unwrap().pdf_calls.len();
    assert_eq!(asked, 5);
    // the next day, five more
    r.run(DAY).await;
    let asked = r.history().len() + r.tesla.0.lock().unwrap().pdf_calls.len();
    assert_eq!(asked, 10);
    // a daily cap nearly spent leaves nothing to the invoices
    let c = tesla::Cadence { daily_cap: 600, invoice_reserve: 600, ..on() };
    let mut r = rig(10, c);
    r.run(DAY).await;
    assert!(r.history().is_empty());
}

#[tokio::test]
async fn pages_numbered_from_one_are_found_and_a_missing_pdf_is_not_asked_forever() {
    let mut r = rig(60, on());
    {
        let mut w = r.tesla.0.lock().unwrap();
        w.first_page = 1;
        w.no_pdf = vec!["made-up-3".into()];
    }
    r.run(60 * 60_000).await;
    assert_eq!(r.history(), vec![0, 1, 2]);
    assert_eq!(r.sink.sessions.lock().unwrap().len(), 60);
    assert_eq!(r.sink.pdfs.lock().unwrap().len(), 59);
    r.run(10 * DAY).await;
    let tries = r.tesla.0.lock().unwrap().pdf_calls.iter().filter(|c| *c == "made-up-3").count();
    assert_eq!(tries as u32, tesla::invoices::PDF_TRIES);
}
