//! The charge costs TeslaFi lists, on a virtual clock against a fake TeslaFi: off by default, the log read back
//! in chunks until it is empty, each charge kept once, then a daily check, all between the reads of the car and
//! between the polls, without a cap of their own. Everything is made up.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::{TOKEN, VIN, vault};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use teslafi::api::Poll;
use teslafi::{Account, AccountState, Cadence, Feed, FeedError, Match, Poller, Read, Secret, Store};

const NOW: u64 = 1_800_000_000_000;
const DAY: u64 = 86_400_000;

#[derive(Clone, Default)]
struct Fake {
    reads: Arc<Mutex<u64>>,
    charges: Arc<Mutex<Vec<(i64, i64)>>>,
    /// The made-up charges: day since the epoch, cost.
    log: Arc<Mutex<Vec<(i64, f64)>>>,
    /// Answer the charge log with a 429 (the wait TeslaFi asks, in s).
    limit: Arc<Mutex<Option<Option<u64>>>>,
    /// Answer a window holding more charges than this as too large (as past the client's cap).
    most: Arc<Mutex<Option<usize>>>,
    /// A day whose log alone is too large: every window holding it is.
    huge_day: Arc<Mutex<Option<i64>>>,
}

impl Feed for Fake {
    async fn read(&self, token: &Secret, _what: Read) -> Result<Poll, FeedError> {
        assert_eq!(token.expose(), TOKEN);
        *self.reads.lock().unwrap() += 1;
        let v = json!({"data_id": "1", "Date": "2027-01-15 08:00:00", "carState": "Sleeping", "state": "asleep",
                       "vin": VIN});
        match v {
            Value::Object(o) => Ok(o),
            _ => unreachable!(),
        }
    }

    async fn charges(&self, token: &Secret, from: i64, to: i64) -> Result<Vec<Value>, FeedError> {
        assert_eq!(token.expose(), TOKEN);
        assert!(from <= to);
        self.charges.lock().unwrap().push((from, to));
        if let Some(retry_after_s) = *self.limit.lock().unwrap() {
            return Err(FeedError::RateLimited { retry_after_s });
        }
        let held = self.log.lock().unwrap().iter().filter(|(d, _)| (from..=to).contains(d)).count();
        if self.most.lock().unwrap().is_some_and(|m| held > m)
            || self.huge_day.lock().unwrap().is_some_and(|d| (from..=to).contains(&d))
        {
            return Err(FeedError::Http(teslafi::http::HttpError::TooLarge));
        }
        Ok(self
            .log
            .lock()
            .unwrap()
            .iter()
            .filter(|(d, _)| (from..=to).contains(d))
            .map(|(d, cost)| {
                json!({"id": d * 10, "startDate": format!("{} 10:00:00", teslafi::costs::date(*d)),
                       "endDate": format!("{} 10:30:00", teslafi::costs::date(*d)), "locationName": "Made-up Supercharger",
                       "cost": cost, "totalEnergyAdded": 30.0})
            })
            .collect())
    }
}

#[derive(Default)]
struct Sink {
    kept: Mutex<BTreeMap<String, tesla::invoices::Found>>,
}

impl teslafi::Sink for Sink {
    fn new_car(&self, _car: &tesla::NewCar) -> Result<u32, tesla::SinkError> {
        Ok(1)
    }
    fn points(&self, _c: u32, _s: &[model::Sample], _f: &[tesla::ApiFact]) -> Result<(), tesla::SinkError> {
        Ok(())
    }
    fn off_can(&self, _c: u32, _t: u64, _v: &model::OffCan) -> Result<(), tesla::SinkError> {
        Ok(())
    }
    fn invoices(&self, found: &[tesla::invoices::Found]) -> Result<u64, tesla::SinkError> {
        let mut k = self.kept.lock().unwrap();
        Ok(found.iter().filter(|f| k.insert(f.key.clone(), (*f).clone()).is_none()).count() as u64)
    }
}

fn rig(cadence: Cadence) -> (tempfile::TempDir, Store, Fake, Arc<Sink>, Poller<Fake, Sink>) {
    let (t, data, v) = vault();
    let store = Store::new(&data, v);
    let car = teslafi::AccountCar {
        vehicle: "fv1".into(),
        vin_prefix: Some("5YJ3E1EA0PF".into()),
        vin_tag: Some(store.vin_tag(&tesla::FullVin::new(VIN))),
        model: Some('3'),
        year: Some(2023),
        display_name: None,
        matched: Some(Match::Car { car: 7 }),
    };
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
    let id = store.add(a, &Secret::new(TOKEN)).unwrap().id;
    let fake = Fake::default();
    let today = (NOW / DAY) as i64;
    *fake.log.lock().unwrap() = vec![(today - 3, 12.5), (today - 40, 9.0), (today - 200, 7.25)];
    let sink = Arc::new(Sink::default());
    let p = Poller::new(store.clone(), &id, fake.clone(), sink.clone(), cadence, chrono_tz::Tz::UTC).unwrap();
    (t, store, fake, sink, p)
}

async fn run(p: &mut Poller<Fake, Sink>, from: u64, to: u64) {
    let mut now = from;
    while now < to {
        let next = p.tick(now).await.unwrap();
        assert!(next > now);
        now = next;
    }
}

#[tokio::test]
async fn off_until_turned_on() {
    let (_t, _s, fake, sink, mut p) = rig(Cadence::default());
    run(&mut p, NOW, NOW + DAY).await;
    assert!(fake.charges.lock().unwrap().is_empty());
    assert!(sink.kept.lock().unwrap().is_empty());
}

#[tokio::test]
async fn the_log_is_read_back_once_then_daily() {
    let (_t, store, fake, sink, mut p) = rig(Cadence { invoices: true, ..Cadence::default() });
    run(&mut p, NOW, NOW + 3_600_000).await;
    let asked = fake.charges.lock().unwrap().clone();
    // three chunks with charges (today back to 270 days), two empty ones, then done
    assert_eq!(asked.len(), 5, "{asked:?}");
    assert!(asked.windows(2).all(|w| w[1].1 + 1 == w[0].0), "chunks back to back: {asked:?}");
    let kept = sink.kept.lock().unwrap().clone();
    assert_eq!(kept.len(), 3);
    let one = kept.values().find(|f| f.session.total == Some(12.5)).unwrap();
    assert!(one.key.starts_with("teslafi-"));
    assert_eq!((one.car, one.vehicle.as_deref()), (Some(7), Some("fv1")));
    assert_eq!(one.session.end_ms - one.session.start_ms, 30 * 60_000);
    let c = store.pacing("fa1").unwrap().costs;
    assert!(c.backfill_done);
    assert_eq!(c.found, 3);
    // the car is still read at its pace meanwhile
    assert!(*fake.reads.lock().unwrap() >= 5);
    // a day on: the last two weeks, once
    run(&mut p, NOW + 3_600_000, NOW + DAY + 2 * 3_600_000).await;
    let asked = fake.charges.lock().unwrap().clone();
    assert_eq!(asked.len(), 6);
    assert_eq!(asked[5].1 - asked[5].0, teslafi::costs::CHECK_DAYS);
    assert_eq!(sink.kept.lock().unwrap().len(), 3, "each charge once");
}

#[tokio::test]
async fn the_costs_have_no_daily_cap_of_their_own() {
    let (_t, _s, fake, _sink, mut p) = rig(Cadence { invoices: true, ..Cadence::default() });
    // a charge in every chunk back to 2012: more than the 60 requests a day an older server allowed them
    let today = (NOW / DAY) as i64;
    *fake.log.lock().unwrap() = (0..70).map(|k| (today - 80 * k, 5.0)).collect();
    run(&mut p, NOW, NOW + 6 * 3_600_000).await;
    let n = fake.charges.lock().unwrap().len();
    assert!(n > 60, "{n} requests");
}

#[tokio::test]
async fn teslafis_429_on_the_costs_is_waited_out_like_the_polls() {
    let (_t, store, fake, _sink, mut p) = rig(Cadence { invoices: true, ..Cadence::default() });
    *fake.limit.lock().unwrap() = Some(Some(600));
    run(&mut p, NOW, NOW + 60_000).await;
    assert_eq!(fake.charges.lock().unwrap().len(), 1);
    let until = store.pacing("fa1").unwrap().rate_limited_until_ms;
    assert!(until >= NOW + 600_000, "nothing is asked before TeslaFi's wait ends: {until}");
}

#[tokio::test]
async fn a_log_too_large_for_one_answer_is_read_in_smaller_windows() {
    let (_t, store, fake, sink, mut p) = rig(Cadence { invoices: true, ..Cadence::default() });
    // two charges a day for the last nine months, then nothing; more than 20 in one answer is too large
    let today = (NOW / DAY) as i64;
    *fake.log.lock().unwrap() = (0..270).flat_map(|d| [(today - d, 4.0), (today - d, 6.5)]).collect();
    *fake.most.lock().unwrap() = Some(20);
    run(&mut p, NOW, NOW + 6 * 3_600_000).await;
    let c = store.pacing("fa1").unwrap().costs;
    assert!(c.backfill_done, "{c:?}");
    assert!(c.last_error.is_none(), "{c:?}");
    assert_eq!(c.chunk_days, None, "90 days, then 45, 22, 11, 5 for the dense stretch, whole chunks after it");
    // every day read once (the ids are one a day, so each day's charges are one key)
    let kept = sink.kept.lock().unwrap();
    let days: std::collections::BTreeSet<i64> = kept.values().map(|f| (f.session.start_ms / DAY) as i64).collect();
    assert_eq!(days.len(), 270);
    // and it stopped after two chunks' worth of empty days, not two small empty windows
    let asked = fake.charges.lock().unwrap().clone();
    let oldest = asked.iter().map(|w| w.0).min().unwrap();
    assert!(oldest <= today - 269 - 2 * teslafi::costs::CHUNK_DAYS + 5, "read back to {oldest}");
}

#[tokio::test]
async fn a_day_too_large_to_read_is_passed_over_and_stays_said() {
    let (_t, store, fake, sink, mut p) = rig(Cadence { invoices: true, ..Cadence::default() });
    let today = (NOW / DAY) as i64;
    *fake.log.lock().unwrap() = vec![(today - 3, 12.5), (today - 40, 9.0), (today - 120, 7.25)];
    let huge = today - 50;
    *fake.huge_day.lock().unwrap() = Some(huge);
    run(&mut p, NOW, NOW + 12 * 3_600_000).await;
    let c = store.pacing("fa1").unwrap().costs;
    assert!(c.backfill_done, "{c:?}");
    assert_eq!(c.skipped_days, vec![huge]);
    // the windows that worked after it did not clear what was not read
    let e = c.last_error.expect("the day passed over is said");
    assert_eq!(e.code, "too_large");
    assert!(e.message.contains(&teslafi::costs::date(huge)), "{}", e.message);
    assert_eq!(sink.kept.lock().unwrap().len(), 3, "the other days' costs are read");
    // the daily check (the last two weeks) works and still says it
    run(&mut p, NOW + 12 * 3_600_000, NOW + DAY + 12 * 3_600_000).await;
    let c = store.pacing("fa1").unwrap().costs;
    assert_eq!(c.skipped_days, vec![huge]);
    assert_eq!(c.last_error.map(|e| e.code).as_deref(), Some("too_large"));
}

#[tokio::test]
async fn the_window_shrinks_for_a_dense_stretch_and_grows_back_after_it() {
    let (_t, store, fake, _sink, mut p) = rig(Cadence { invoices: true, ..Cadence::default() });
    let today = (NOW / DAY) as i64;
    // three charges a day for twenty days a month back, one a week for two years before that
    let mut log: Vec<(i64, f64)> =
        (30..50).flat_map(|d| [(today - d, 4.0), (today - d, 5.0), (today - d, 6.0)]).collect();
    log.extend((0..104).map(|w| (today - 60 - 7 * w, 8.0)));
    *fake.log.lock().unwrap() = log;
    *fake.most.lock().unwrap() = Some(20);
    run(&mut p, NOW, NOW + DAY).await;
    let c = store.pacing("fa1").unwrap().costs;
    assert!(c.backfill_done, "{c:?}");
    assert!(c.last_error.is_none(), "{c:?}");
    let asked = fake.charges.lock().unwrap().clone();
    let sizes: Vec<i64> = asked.iter().map(|(a, b)| b - a + 1).collect();
    assert!(sizes.iter().any(|n| *n <= 11), "it shrank for the dense stretch: {sizes:?}");
    // once past it the windows are whole chunks again, not a day or a week (a few fit at the size below one found too
    // large before it grows back to it)
    let past = asked.iter().position(|(a, _)| *a < today - 180).unwrap();
    assert!(sizes[past..].iter().all(|n| *n >= 45), "it grew back: {sizes:?}");
    assert_eq!(c.chunk_days, None);
    assert!(asked.len() < 40, "{} requests for two years", asked.len());
}

/// A day passed over as too large is asked alone again once the backfill is done, and read once it fits.
#[tokio::test]
async fn a_day_passed_over_is_asked_again_and_read_once_it_fits() {
    let (_t, store, fake, sink, mut p) = rig(Cadence { invoices: true, ..Cadence::default() });
    let today = (NOW / DAY) as i64;
    let huge = today - 50;
    *fake.log.lock().unwrap() = vec![(today - 3, 12.5), (today - 40, 9.0), (huge, 3.0), (today - 120, 7.25)];
    *fake.huge_day.lock().unwrap() = Some(huge);
    run(&mut p, NOW, NOW + 12 * 3_600_000).await;
    let c = store.pacing("fa1").unwrap().costs;
    assert!(c.backfill_done, "{c:?}");
    assert_eq!(c.skipped_days, vec![huge]);
    // TeslaFi now serves that day within the cap; a week of daily checks goes by
    *fake.huge_day.lock().unwrap() = None;
    run(&mut p, NOW + 12 * 3_600_000, NOW + 8 * DAY).await;
    let asked = fake.charges.lock().unwrap().clone();
    let c = store.pacing("fa1").unwrap().costs;
    assert!(
        asked.iter().skip_while(|w| w.0 != huge || w.1 != huge).skip(1).any(|(a, b)| (*a..=*b).contains(&huge)),
        "the skipped day is never asked for again: skipped_days={:?} last_error={:?}",
        c.skipped_days,
        c.last_error.map(|e| e.code)
    );
    assert!(sink.kept.lock().unwrap().values().any(|f| f.session.total == Some(3.0)));
}

/// A day passed over as too large holds charges, so it ends a run of empty days as any day with charges does.
#[tokio::test]
async fn a_day_passed_over_ends_a_run_of_empty_days() {
    let (_t, store, fake, sink, mut p) = rig(Cadence { invoices: true, ..Cadence::default() });
    let today = (NOW / DAY) as i64;
    let huge = today - 150;
    // 150 days with no charge before the huge day would stop nothing; counting through it does
    *fake.log.lock().unwrap() = vec![(today - 3, 12.5), (huge, 3.0), (today - 300, 7.25)];
    *fake.huge_day.lock().unwrap() = Some(huge);
    run(&mut p, NOW, NOW + 2 * DAY).await;
    let c = store.pacing("fa1").unwrap().costs;
    assert!(c.backfill_done, "{c:?}");
    assert_eq!(c.skipped_days, vec![huge]);
    let oldest = fake.charges.lock().unwrap().iter().map(|w| w.0).min().unwrap();
    assert!(
        sink.kept.lock().unwrap().values().any(|f| f.session.total == Some(7.25)),
        "the charge 300 days back is never read: backfill stopped at day today-{} (empty_days={})",
        today - oldest,
        c.empty_days
    );
}

/// The same row read with the same decimal mark for its cost and its energy.
#[test]
fn a_decimal_comma_in_the_energy_and_duration_is_a_decimal_comma() {
    let r = json!({"id": 77, "startDate": "2026-07-04 03:00:00", "cost": "12,50", "totalEnergyAdded": "41,2",
                   "duration": "30,5"});
    let s = teslafi::costs::session(&r, chrono_tz::Tz::UTC).unwrap();
    assert_eq!(s.total, Some(12.5));
    assert_eq!(s.fees[0].usage, Some(41.2), "energy 41,2 kWh");
    assert_eq!(s.end_ms - s.start_ms, 30 * 60_000 + 30_000, "duration 30,5 min");
}

/// A stretch dense all along, where 45 days fit in an answer and 90 do not: the window is not asked at 90 again after
/// every answer of 45 (each such answer is up to 8 MB read and thrown away).
#[tokio::test]
async fn a_window_found_too_large_is_not_asked_again_after_every_answer_that_fits() {
    let (_t, store, fake, _sink, mut p) = rig(Cadence { invoices: true, ..Cadence::default() });
    let today = (NOW / DAY) as i64;
    *fake.log.lock().unwrap() = (0..720).map(|d| (today - d, 4.0)).collect();
    *fake.most.lock().unwrap() = Some(60);
    run(&mut p, NOW, NOW + 6 * 3_600_000).await;
    let c = store.pacing("fa1").unwrap().costs;
    assert!(c.backfill_done, "{c:?}");
    let sizes: Vec<i64> = fake.charges.lock().unwrap().iter().map(|(a, b)| b - a + 1).collect();
    let too_large = sizes.iter().filter(|n| **n > 60).count();
    assert!(too_large * 3 <= sizes.len(), "{too_large} of {} requests too large: {sizes:?}", sizes.len());
}

/// The poller run as `run` does, noting when each charge-log window was asked.
async fn run_timed(p: &mut Poller<Fake, Sink>, fake: &Fake, from: u64, to: u64, at: &mut Vec<(u64, (i64, i64))>) {
    let mut now = from;
    while now < to {
        let before = fake.charges.lock().unwrap().len();
        let next = p.tick(now).await.unwrap();
        let asked = fake.charges.lock().unwrap().clone();
        for w in &asked[before..] {
            at.push((now, *w));
        }
        assert!(next > now);
        now = next;
    }
}

/// A day passed over that TeslaFi still answers too large, asked again each day: the day's check stays a day apart
/// (not put off by a wait for the retry's error) and the error keeps the time the day was first found.
#[tokio::test]
async fn a_day_passed_over_still_too_large_neither_puts_the_daily_check_off_nor_dates_its_error_anew() {
    let cad = Cadence { invoices: true, ..Cadence::default() };
    let check = cad.invoice_check_ms;
    let (_t, store, fake, _sink, mut p) = rig(cad);
    let today = (NOW / DAY) as i64;
    let huge = today - 50;
    *fake.log.lock().unwrap() = vec![(today - 3, 12.5), (today - 40, 9.0), (today - 120, 7.25)];
    *fake.huge_day.lock().unwrap() = Some(huge);
    let mut at = Vec::new();
    run_timed(&mut p, &fake, NOW, NOW + 12 * 3_600_000, &mut at).await;
    let c = store.pacing("fa1").unwrap().costs;
    assert!(c.backfill_done);
    assert_eq!(c.skipped_days, vec![huge]);
    let first_at = c.last_error.clone().unwrap().at_ms;
    let mut seen_at = vec![first_at];
    let mut t = NOW + 12 * 3_600_000;
    for _ in 0..4 {
        run_timed(&mut p, &fake, t, t + DAY, &mut at).await;
        t += DAY;
        seen_at.push(store.pacing("fa1").unwrap().costs.last_error.unwrap().at_ms);
    }
    let checks: Vec<u64> =
        at.iter().filter(|(_, (a, b))| b - a == teslafi::costs::CHECK_DAYS).map(|(t, _)| *t).collect();
    let retries: Vec<u64> = at.iter().filter(|(_, (a, b))| a == b && *a == huge).map(|(t, _)| *t).collect();
    assert!(checks.len() >= 3, "{checks:?}");
    assert!(retries.len() >= 3, "the day passed over is asked again each day: {retries:?}");
    // the checks are a day apart (within a few minutes of tick granularity), not a day and the retry's wait
    for w in checks.windows(2) {
        let gap = w[1] - w[0];
        assert!(gap <= check + 5 * 60_000, "check gap {} min over a day", (gap - check) / 60_000);
    }
    assert!(seen_at.iter().all(|t| *t == first_at), "the error's since moved: {seen_at:?}");
}

/// The server's clock set back 3 days once the costs are read: the next check comes within a day, not 3 days later.
#[tokio::test]
async fn a_clock_set_back_moves_the_costs_check_back_too() {
    let cad = Cadence { invoices: true, ..Cadence::default() };
    let check = cad.invoice_check_ms;
    let (_t, store, fake, _sink, mut p) = rig(cad);
    run(&mut p, NOW, NOW + 3_600_000).await;
    assert!(store.pacing("fa1").unwrap().costs.backfill_done);
    let n = fake.charges.lock().unwrap().len();
    let back = NOW + 3_600_000 - 3 * DAY;
    run(&mut p, back, back + check + 3_600_000).await;
    let after = fake.charges.lock().unwrap().len();
    let c = store.pacing("fa1").unwrap().costs;
    assert!(
        after > n,
        "no costs check within a day and an hour after the clock went back 3 days: next_check_ms is {} h ahead of the clock",
        (c.next_check_ms - back) / 3_600_000
    );
}
