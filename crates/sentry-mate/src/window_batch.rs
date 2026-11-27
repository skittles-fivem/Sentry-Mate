//! The timeline is the same whether the windows a car's stored logs change are rebuilt after every log or once per
//! run of logs ([`engine::Options::window_batch`]): the same made-up logs go through two engines, one of each kind,
//! and everything stored for the car is compared, after every step: its drives, charges, state spans, software
//! updates, charge curves and samples, its summary and newest values, and the engine's own record of it.
//!
//! The logs come in several ways: all there at the start; with gaps that are filled later (every seventh log and a
//! stretch of two hours, older than logs already worked on); the second half before the first; and each time the
//! car is then rebuilt whole.
//!
//! Logs that fill a gap before rows already stored are why the engine rebuilds per run only where nothing is
//! stored yet at or after the logs: without that, a week of these logs gave a drive with the same times and values
//! and another id (the id of another of the pieces it was first seen as). With it, such logs get a window each.
//!
//! A rebuild of a period ([`periods`]) clears the period first, so nothing worked out from logs is stored in it, and
//! its logs are rebuilt per run. Where a drive or charge goes on across the period's end, its part after the end is
//! kept, and a window that reaches it before the drive's head is decoded makes a drive or charge of it: a week's
//! middle third (by count of logs) ends 18 minutes into a charge, and rebuilt per run of 64 all through the charge
//! took the id of its part after the end where a window per log kept the id of its start. So the logs from an hour
//! before the earliest such start get a window each, and the ones before it, which carry none of its samples, are
//! rebuilt per run ([`cut_periods`]: ends at every point of a drive and of a charge, gaps, two cars, a restart, a
//! start that moves earlier; each also asserts that fewer windows were rebuilt, so that the fast path is known to
//! have run).
//!
//! A full rebuild rebuilds its windows once per day of logs, beside API inputs too ([`by_day`]): its shadow has no
//! rows yet and its newest data is its newest log's, so none of its windows runs on ahead of the logs to the API's
//! newest sample, as a window of a car rebuilt in place beside an API does.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::print_stdout)]

use super::*;
use model::{Charge, ChargePoint, Drive, Sample, Source, Stream};

/// Every row of a table, oldest first, as text.
fn rows<T: db::Item + std::fmt::Debug>(db: &db::Db, car: model::CarId) -> Vec<String> {
    let mut out = Vec::new();
    let mut q = db::Query::default().oldest_first().limit(db::MAX_PAGE);
    loop {
        let page = db.list::<T>(car, &q).unwrap();
        out.extend(page.items.iter().map(|r| format!("{r:?}")));
        match page.next {
            Some(n) => q = q.after(Some(n)),
            None => return out,
        }
    }
}

/// Everything stored for the cars of a data folder (an engine's, a full rebuild's shadow), in named parts (each
/// car's, in the order of their ids).
fn stored_in(db: &db::Db) -> Vec<(String, Vec<String>)> {
    let fleet = db.fleet(&db::FleetQuery { search: None, limit: 10, after: None }).unwrap();
    assert!(!fleet.cars.is_empty(), "a made-up car");
    let mut cars: Vec<&db::FleetEntry> = fleet.cars.iter().collect();
    cars.sort_by_key(|c| c.car.id);
    let mut out = Vec::new();
    for entry in cars {
        let car = entry.car.id;
        let mut meta = engine::records::car_meta(db, car).unwrap();
        // (when the engine last worked, by this machine's clock: not the car's history)
        meta.last_done_ms = None;
        meta.last_took_ms = None;
        // (when the logs were stored: each side stored its own)
        meta.last_arrived_ms = None;
        meta.api_received_ms = None;
        meta.teslafi_received_ms = None;
        meta.tmlocal_received_ms = None;
        let points = |v: Vec<Sample>| v.iter().map(|p| format!("{p:?}")).collect::<Vec<_>>();
        let parts = vec![
            ("drives", rows::<Drive>(db, car)),
            ("charges", rows::<Charge>(db, car)),
            ("states", rows::<db::StateRow>(db, car)),
            ("updates", rows::<db::UpdateRow>(db, car)),
            (
                "charge points",
                db.read_points::<ChargePoint>(car, Source::Live, 0, u64::MAX)
                    .unwrap()
                    .iter()
                    .map(|p| format!("{p:?}"))
                    .collect(),
            ),
            ("samples", points(db.read_points::<Sample>(car, Stream::Smo, 0, u64::MAX).unwrap())),
            ("summary", vec![format!("{:?}", entry.summary)]),
            ("car", vec![format!("{:?} {:?} {:?}", entry.car.name, entry.car.vin, entry.car.vehicle_keys)]),
            ("record", vec![format!("{meta:?}")]),
        ];
        out.extend(parts.into_iter().map(|(name, v)| (format!("car {car} {name}"), v)));
    }
    out
}

/// The engine's records of the cars of a data folder ([`engine::records::stored_records`]: the meta record, the
/// windows noted dirty, each day's record of its logs and its firmware notes), when each log was worked on by
/// this machine's clock left out.
fn records_in(db: &db::Db) -> Vec<String> {
    let fleet = db.fleet(&db::FleetQuery { search: None, limit: 10, after: None }).unwrap();
    let mut cars: Vec<model::CarId> = fleet.cars.iter().map(|c| c.car.id).collect();
    cars.sort_unstable();
    let mut out = Vec::new();
    for car in cars {
        for (key, mut v) in engine::records::stored_records(db, car).unwrap() {
            if let Some(m) = v.as_object_mut() {
                for f in ["last_done_ms", "last_took_ms", "last_arrived_ms", "api_received_ms", "teslafi_received_ms"] {
                    m.remove(f);
                }
                m.remove("tmlocal_received_ms");
                if let Some(logs) = m.get_mut("logs").filter(|_| key.contains("/day/") && !key.contains("/fw/")) {
                    for e in logs.as_object_mut().into_iter().flat_map(|l| l.values_mut()) {
                        e.as_object_mut().map(|e| e.remove("done_ms"));
                    }
                }
            }
            out.push(format!("{key} {v}"));
        }
    }
    out
}

/// [`records_in`] of two data folders the same; returns how many records.
fn records_same(what: &str, a: &db::Db, b: &db::Db) -> usize {
    let (x, y) = (records_in(a), records_in(b));
    if let Some(i) = (0..x.len().max(y.len())).find(|&i| x.get(i) != y.get(i)) {
        panic!("{what}: the records differ at {i} of {} and {}:\n {:?}\n {:?}", x.len(), y.len(), x.get(i), y.get(i));
    }
    x.len()
}

fn same(what: &str, a: &engine::Engine, b: &engine::Engine) -> usize {
    same_in(what, a.db(), b.db())
}

/// [`same`], of two data folders (two full rebuilds' shadows).
fn same_in(what: &str, a: &db::Db, b: &db::Db) -> usize {
    let (x, y) = (stored_in(a), stored_in(b));
    assert_eq!(x.len(), y.len(), "{what}: the same cars");
    let mut n = 0;
    for ((name, p), (_, q)) in x.iter().zip(&y) {
        if let Some(i) = (0..p.len().max(q.len())).find(|&i| p.get(i) != q.get(i)) {
            panic!(
                "{what}: {name} differ at {i} of {} and {}:\n after every log: {:?}\n once per run:    {:?}",
                p.len(),
                q.len(),
                p.get(i),
                q.get(i)
            );
        }
        n += p.len();
    }
    n
}

/// One server's data folder and engine.
struct Side {
    _dir: tempfile::TempDir,
    store: Store,
    judge: Judge,
    gate: gate::Gate,
    engine: engine::Engine,
}

impl Side {
    async fn new(window_batch: usize) -> Side {
        Side::with(engine::Options { window_batch, ..engine::Options::default() }).await
    }

    async fn with(opts: engine::Options) -> Side {
        let dir = tempfile::tempdir().unwrap();
        let (data, keys) = (dir.path().join("data"), dir.path().join("keys"));
        std::fs::create_dir_all(&data).unwrap();
        let (v, _) = Vault::create(&data, &keys, Kdf::FAST_FOR_TESTS).unwrap();
        let v = Arc::new(v);
        let store = Store::new(can_dir(&data), v.clone());
        let judge = Judge::new(store.clone());
        let db = db::Db::open(&data, v.clone(), db::Options::default()).unwrap();
        let engine = engine::Engine::new(db, Store::new(can_dir(&data), v), opts);
        engine.start().unwrap();
        engine.idle().await;
        Side { _dir: dir, store, judge, gate: gate::Gate::memory(), engine }
    }

    /// Stores these logs as uploads are stored, then has the engine compare the store with what it worked on.
    async fn take(&self, logs: &[&synth::LogFile]) {
        let enter = || -> Result<gate::Pass, Box<dyn std::error::Error>> { Ok(self.gate.enter(GATE_WAIT)?) };
        for log in logs {
            seed::put(&self.store, &self.judge, &enter, &log.name, &log.bytes).await.unwrap();
            seed::put(&self.store, &self.judge, &enter, &log.sidecar_name(), &log.sidecar_bytes()).await.unwrap();
        }
        self.engine.reconcile(None);
        self.engine.idle().await;
    }

    /// Stores these logs as uploads are stored, and nothing more (the engine is not told).
    async fn put(&self, logs: &[&synth::LogFile]) {
        let enter = || -> Result<gate::Pass, Box<dyn std::error::Error>> { Ok(self.gate.enter(GATE_WAIT)?) };
        for log in logs {
            seed::put(&self.store, &self.judge, &enter, &log.name, &log.bytes).await.unwrap();
            seed::put(&self.store, &self.judge, &enter, &log.sidecar_name(), &log.sidecar_bytes()).await.unwrap();
        }
    }

    /// The cars, in the order of their ids.
    fn cars(&self) -> Vec<model::CarId> {
        let fleet = self.engine.db().fleet(&db::FleetQuery { search: None, limit: 10, after: None }).unwrap();
        let mut cars: Vec<model::CarId> = fleet.cars.iter().map(|c| c.car.id).collect();
        cars.sort_unstable();
        cars
    }

    /// A rebuild of `[from_ms, to_ms)` of a car (or of every car); returns how many windows it rebuilt.
    async fn rebuild_period(&self, car: Option<model::CarId>, from_ms: u64, to_ms: u64) -> u64 {
        let before = self.engine.stats().windows_rebuilt;
        self.engine.rebuild(engine::Scope { car, from_ms: Some(from_ms), to_ms: Some(to_ms) }).unwrap();
        self.engine.idle().await;
        self.engine.stats().windows_rebuilt - before
    }

    async fn rebuild(&self) {
        let car =
            self.engine.db().fleet(&db::FleetQuery { search: None, limit: 10, after: None }).unwrap().cars[0].car.id;
        self.engine.rebuild(engine::Scope::car(car)).unwrap();
        self.engine.idle().await;
    }
}

/// The logs in `steps` (each a choice of the scenario's logs), one step after the other, through both kinds of
/// engine; compared after each step and after a rebuild of the car.
///
/// Returns how many windows each engine rebuilt.
async fn both(what: &str, logs: &[synth::LogFile], steps: &[&dyn Fn(usize) -> bool], batch: usize) -> (u64, u64) {
    let (each, run) = (Side::new(1).await, Side::new(batch).await);
    for (k, step) in steps.iter().enumerate() {
        let part: Vec<&synth::LogFile> = logs.iter().enumerate().filter(|(i, _)| step(*i)).map(|(_, l)| l).collect();
        assert!(!part.is_empty(), "{what}: step {k} has logs");
        each.take(&part).await;
        run.take(&part).await;
        let n = same(&format!("{what}, step {k}"), &each.engine, &run.engine);
        println!("{what}, step {k}: {} logs, {n} stored things the same", part.len());
    }
    each.rebuild().await;
    run.rebuild().await;
    let n = same(&format!("{what}, rebuilt"), &each.engine, &run.engine);
    assert!(n > 1_000, "{what}: {n}");
    assert_eq!(each.engine.stats().failures + run.engine.stats().failures, 0);
    (each.engine.stats().windows_rebuilt, run.engine.stats().windows_rebuilt)
}

async fn every_way(days: u32, batch: usize) {
    let logs: Vec<synth::LogFile> =
        synth::Scenario::new(7).days(days).rates(synth::Rates::Light).with_trouble(true).logs().collect();
    let n = logs.len();
    assert!(n > 100, "{n} logs");
    // the two hours of logs after the first third
    let stretch = |i: usize| (n / 3..n / 3 + 120).contains(&i);
    let (each, run) = both("in order", &logs, &[&|_| true], batch).await;
    // (and the windows were rebuilt per run there: far fewer of them)
    assert!(run * 3 < each, "{run} windows against {each}");
    both(
        "gaps filled later",
        &logs,
        &[&|i| i % 7 != 3 && !stretch(i), &|i| i % 7 == 3 && !stretch(i), &|i| stretch(i)],
        batch,
    )
    .await;
    both("the second half first", &logs, &[&|i| i >= n / 2, &|i| i < n / 2], batch).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn windows_rebuilt_once_per_run_of_logs_give_the_same_timeline() {
    every_way(1, 16).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "long: cargo test --release -p sentry-mate windows_rebuilt -- --ignored --nocapture"]
async fn windows_rebuilt_once_per_run_of_logs_give_the_same_timeline_over_a_week() {
    for batch in [7, 64, 100_000] {
        every_way(7, batch).await;
    }
}

/// Beside API inputs: a TeslaMate import, TeslaFi's API and local TeslaMate over the same days (made-up, another
/// car's days than the logs': the inputs disagree, which is the hard case), then the rest of the logs, then the car
/// rebuilt whole. The same on both sides because a car with an API input gets a window per log either way: with
/// that condition taken out of the engine (tried by hand), five days gave one drive with the same times and values
/// and another id after the rebuild, at runs of 7 and of 64 logs (a rebuild builds rows from the API's samples
/// ahead of the logs, and a drive keeps the id of the piece of it stored first); with the other condition taken
/// out as well, two days gave a drive without its start and end positions.
async fn beside_apis(days: u32, batch: usize) {
    let logs: Vec<synth::LogFile> =
        synth::Scenario::new(7).days(days).rates(synth::Rates::Light).with_trouble(true).logs().collect();
    let n = logs.len();
    let (each, run) = (Side::new(1).await, Side::new(batch).await);
    let all: Vec<&synth::LogFile> = logs.iter().collect();
    let o = history::Options {
        days,
        end_ms: synth::DEFAULT_START_MS + u64::from(days) * 86_400_000,
        seed: 5,
        drives: 6 * days,
        charges: 2 * days,
        drive_step_s: 5,
        teslafi_days: 1,
        api_days: days,
        tesla_days: 0,
        tesla_sentry: false,
    };
    for (k, side) in [&each, &run].into_iter().enumerate() {
        side.take(&all[..20]).await;
        history::run(&side.engine, &o).await.unwrap();
        side.engine.idle().await;
        println!("side {k}: history in");
    }
    let n0 = same("beside APIs, the history in", &each.engine, &run.engine);
    for (a, b) in [(n / 2, n), (20, n / 2)] {
        each.take(&all[a..b]).await;
        run.take(&all[a..b]).await;
        let k = same(&format!("beside APIs, logs {a}..{b}"), &each.engine, &run.engine);
        println!(
            "beside APIs, logs {a}..{b}: {k} the same (windows {} and {})",
            each.engine.stats().windows_rebuilt,
            run.engine.stats().windows_rebuilt
        );
    }
    // a period whose end cuts a drive: a car with an API input gets a window per log either way (equal counts)
    let car = each.cars()[0];
    let (drives, _) = kinds_of(&each.engine, car);
    let d = drives[drives.len() / 2];
    let to = at(&logs[log_inside(&logs, d.0, d.1).unwrap()]);
    let (w, v) =
        period("beside APIs, a period ending in a drive", &each, &run, Some(0), period_from(&logs, d.0), to).await;
    assert_eq!(w, v, "a car with an API input rebuilds a window per log either way");
    each.rebuild().await;
    run.rebuild().await;
    let k = same("beside APIs, rebuilt", &each.engine, &run.engine);
    println!(
        "beside APIs, rebuilt: {k} the same (windows {} and {}), {n0} before",
        each.engine.stats().windows_rebuilt,
        run.engine.stats().windows_rebuilt
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "long: cargo test --release -p sentry-mate windows_rebuilt -- --ignored --nocapture"]
async fn windows_rebuilt_beside_api_inputs_give_the_same_timeline() {
    beside_apis(2, 64).await;
}

/// A full rebuild cut off while it decodes (the server killed, twice, neither time at the end of a run of logs) and
/// carried on gives the very history an uninterrupted one gives, ids included, and still rebuilds its windows once
/// per run of logs: the rows in its shadow at or after the logs left are only what its own earlier logs gave, as
/// they are in an uninterrupted one at that point.
async fn carried_on(days: u32, batch: usize, cuts: impl Fn(u64) -> (u64, u64)) {
    let logs: Vec<synth::LogFile> =
        synth::Scenario::new(7).days(days).rates(synth::Rates::Light).with_trouble(true).logs().collect();
    let all: Vec<&synth::LogFile> = logs.iter().collect();
    let n = logs.len() as u64;
    let done = |e: engine::Engine| async move {
        for _ in 0..60_000 {
            let r = e.full_rebuild_status().unwrap().unwrap();
            if !r.running() {
                e.idle().await;
                return r;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the full rebuild did not end");
    };
    let car_of = |e: &engine::Engine| {
        e.db().fleet(&db::FleetQuery { search: None, limit: 10, after: None }).unwrap().cars[0].car.id
    };

    let whole = Side::new(batch).await;
    whole.take(&all).await;
    let car = car_of(&whole.engine);
    whole.engine.full_rebuild(Some(car), engine::full::Reason::Owner, None).unwrap();
    let r = done(whole.engine.clone()).await;
    assert_eq!((r.status, r.logs_done), (engine::full::FullStatus::Done, n), "{r:#?}");
    let whole_windows = whole.engine.full_rebuild_windows();

    let cut = Side::new(batch).await;
    cut.take(&all).await;
    let car = car_of(&cut.engine);
    let (first, second) = cuts(n);
    cut.engine.full_rebuild_crash_after_logs(Some(first));
    cut.engine.full_rebuild(Some(car), engine::full::Reason::Owner, None).unwrap();
    cut.engine.idle().await;
    let data = cut._dir.path().join("data");
    let again = |crash: Option<u64>| {
        let v = Arc::new(Vault::open(&data, &cut._dir.path().join("keys")).unwrap());
        let opts = engine::Options { window_batch: batch, ..engine::Options::default() };
        let e = engine::Engine::new(cut.engine.db().clone(), Store::new(can_dir(&data), v), opts);
        e.full_rebuild_crash_after_logs(crash);
        e.start().unwrap();
        e
    };
    let mid = again(Some(second));
    mid.idle().await;
    let r = mid.full_rebuild_status().unwrap().unwrap();
    assert!(r.running() && r.phase == engine::full::Phase::Decoding, "{r:#?}");
    let last = again(None);
    let r = done(last.clone()).await;
    assert_eq!((r.status, r.logs_done, r.logs_total), (engine::full::FullStatus::Done, n, n), "{r:#?}");
    let k = same("a full rebuild carried on twice", &whole.engine, &last);
    assert!(k > 1_000, "{k}");
    assert_eq!(whole.engine.stats().failures + last.stats().failures, 0);
    // the logs left after each restart got a window per run, not one each
    let left = n - first - second;
    let (mid_windows, last_windows) = (mid.full_rebuild_windows(), last.full_rebuild_windows());
    println!(
        "{n} logs: {whole_windows} windows uninterrupted; {mid_windows} for {second} logs and {last_windows} for {left} after the restarts"
    );
    assert!(
        mid_windows * 2 < second.max(8) && last_windows * 3 < left,
        "{mid_windows} of {second}, {last_windows} of {left}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_full_rebuild_carried_on_after_a_restart_gives_the_same_timeline_per_run_of_logs() {
    // cut a third in, then a fifth further on: neither a whole number of runs
    carried_on(1, 16, |n| (n / 3 + 1, n / 5 + 2)).await;
    // cut after a few logs, twice; and half way, then one log on
    carried_on(1, 16, |_| (5, 9)).await;
    carried_on(1, 7, |n| (n / 2, 1)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "long: cargo test --release -p sentry-mate carried_on -- --ignored --nocapture"]
async fn a_full_rebuild_carried_on_after_a_restart_gives_the_same_timeline_over_a_week() {
    for batch in [7, 64] {
        carried_on(7, batch, |n| (n / 3 + 1, n / 5 + 2)).await;
        carried_on(7, batch, |n| (n / 2 + 3, n / 4 + 5)).await;
    }
}

/// A rebuild of a period through both engines, compared after it; returns the windows each rebuilt for it.
async fn period(what: &str, each: &Side, run: &Side, car: Option<usize>, from_ms: u64, to_ms: u64) -> (u64, u64) {
    let cars = each.cars();
    assert_eq!(cars, run.cars(), "{what}");
    let car = car.map(|i| cars[i]);
    let began = Instant::now();
    let w = each.rebuild_period(car, from_ms, to_ms).await;
    let took_each = began.elapsed();
    let began = Instant::now();
    let v = run.rebuild_period(car, from_ms, to_ms).await;
    let took_run = began.elapsed();
    let n = same(what, &each.engine, &run.engine);
    assert!(n > 1_000, "{what}: {n}");
    assert_eq!(each.engine.stats().failures + run.engine.stats().failures, 0, "{what}");
    println!(
        "{what}: {n} stored things the same; windows {w} after every log ({took_each:.2?}), {v} once per run \
         ({took_run:.2?})"
    );
    (w, v)
}

/// Where a log starts, as its name says it (a period that starts there takes that log in).
fn at(log: &synth::LogFile) -> u64 {
    log.start_ms / 1000 * 1000
}

/// The log that starts nearest the middle of `[a, b)`, strictly inside it.
fn log_inside(logs: &[synth::LogFile], a: u64, b: u64) -> Option<usize> {
    let mid = a + (b - a) / 2;
    (0..logs.len()).filter(|&i| at(&logs[i]) > a && at(&logs[i]) < b).min_by_key(|&i| at(&logs[i]).abs_diff(mid))
}

/// A car's drives and charges, (start, end).
fn spans_of(e: &engine::Engine, car: model::CarId) -> Vec<(u64, u64)> {
    let db = e.db();
    let q = db::Query::default().oldest_first().limit(db::MAX_PAGE);
    let mut out: Vec<(u64, u64)> =
        db.list::<Drive>(car, &q).unwrap().items.iter().map(|d| (d.start_ms, d.end_ms)).collect();
    out.extend(db.list::<Charge>(car, &q).unwrap().items.iter().map(|c| (c.start_ms, c.end_ms)));
    out
}

/// Where the logs nearest the middles of a car's drives start, and of its charges', from what is stored.
fn cut_by(e: &engine::Engine, logs: &[synth::LogFile], car: model::CarId) -> (Vec<u64>, Vec<u64>) {
    let db = e.db();
    let q = db::Query::default().oldest_first().limit(db::MAX_PAGE);
    let middles = |spans: Vec<(u64, u64)>| -> Vec<u64> {
        spans.into_iter().filter_map(|(a, b)| log_inside(logs, a, b)).map(|i| at(&logs[i])).collect()
    };
    let drives = middles(db.list::<Drive>(car, &q).unwrap().items.iter().map(|d| (d.start_ms, d.end_ms)).collect());
    let charges = middles(db.list::<Charge>(car, &q).unwrap().items.iter().map(|c| (c.start_ms, c.end_ms)).collect());
    (drives, charges)
}

/// Rebuilds of a period, per log and per run of logs, over one car's history and then two cars': in the middle of
/// the history, at its start and at its end, with a drive and a charge cut at each edge, and over gaps filled later
/// (inside the period and before it), each compared, ids included, after every step.
///
/// Returns the windows each way of a rebuild of about the middle third whose end cuts no drive or charge.
async fn periods(days: u32, batch: usize) -> (u64, u64) {
    let scenario = || synth::Scenario::new(7).days(days).rates(synth::Rates::Light).with_trouble(true);
    let logs: Vec<synth::LogFile> = scenario().logs().collect();
    let n = logs.len();
    assert!(n > 100, "{n} logs");
    let t = |i: usize| at(&logs[i]);
    let all: Vec<&synth::LogFile> = logs.iter().collect();

    let (each, run) = (Side::new(1).await, Side::new(batch).await);
    each.take(&all).await;
    run.take(&all).await;
    same("periods, all logs in", &each.engine, &run.engine);
    period("the middle third", &each, &run, Some(0), t(n / 3), t(2 * n / 3)).await;
    let car = each.cars()[0];
    let busy = spans_of(&each.engine, car);
    let free = |i: usize| (i..n).find(|&k| !busy.iter().any(|&(a, b)| a < t(k) && t(k) < b));
    let clear = |i: usize| free(i).unwrap();
    // (where a log starts at or after `i` that no drive or charge goes on across, or the end of the history)
    let clear_at = |i: usize| free(i).map_or(u64::MAX, t);
    // (by count and by time, the wider: the logs come once a minute while the car charges, and the thirds by
    // count can fall in one charge)
    let (first, last) = (t(0), t(n - 1));
    let by_time = |k: u64| logs.iter().position(|l| at(l) >= first + (last - first) * k / 3).unwrap();
    let (a, b) = (clear(by_time(1).min(n / 3)), clear(by_time(2).max(2 * n / 3)));
    assert!(b > a + 20, "{a} {b}");
    let middle =
        period("about the middle third, its end in no drive or charge", &each, &run, Some(0), t(a), t(b)).await;
    period("the first quarter", &each, &run, Some(0), 0, t(n / 4)).await;
    period("the last quarter", &each, &run, Some(0), t(3 * n / 4), u64::MAX).await;
    period("the middle third again", &each, &run, Some(0), t(n / 3), t(2 * n / 3)).await;
    let (drives, charges) = cut_by(&each.engine, &logs, car);
    println!("drives cut at {drives:?}, charges at {charges:?}");
    assert!(drives.len() >= 2 && !charges.is_empty(), "{drives:?} {charges:?}");
    // from a drive's middle to a charge's, and from a charge's to a drive's
    let (d, c) = (drives[drives.len() / 2], charges[0]);
    period("between the middles of a drive and a charge", &each, &run, Some(0), d.min(c), d.max(c)).await;
    if let Some(&d2) = drives.iter().find(|&&d2| d2 > c) {
        period("from the middle of a charge to the middle of a drive", &each, &run, Some(0), c, d2).await;
    }
    if let Some(&d0) = drives.iter().rev().find(|&&d0| d0 < c) {
        period("from the middle of a drive to the middle of a charge", &each, &run, Some(0), d0, c).await;
    }
    // a drive cut at each edge
    let (d1, d2) = (drives[0], drives[drives.len() - 1]);
    period("from the middle of a drive to the middle of another", &each, &run, Some(0), d1, d2).await;
    // a drive and a charge cut at the start, the end in neither
    let index = |ms: u64| logs.iter().position(|l| at(l) == ms).unwrap();
    let end = clear_at(index(d1) + n / 10);
    period("from the middle of a drive to where nothing goes on", &each, &run, Some(0), d1, end).await;
    let end = clear_at(index(c) + n / 10);
    period("from the middle of a charge to where nothing goes on", &each, &run, Some(0), c, end).await;
    each.rebuild().await;
    run.rebuild().await;
    same("periods, rebuilt whole", &each.engine, &run.engine);

    // gaps, filled later: inside the period (the one whose end cuts nothing), then before it (worked on as they
    // come, then stored and left for the rebuild of the period to take in)
    let (from, to) = (t(a), t(b));
    let inside = |i: usize| (a..b).contains(&i) && i % 7 == 3;
    let before = |i: usize| (n / 6..n / 6 + 40).contains(&i);
    let pick = |f: &dyn Fn(usize) -> bool| -> Vec<&synth::LogFile> {
        logs.iter().enumerate().filter(|(i, _)| f(*i)).map(|(_, l)| l).collect()
    };
    let (each, run) = (Side::new(1).await, Side::new(batch).await);
    for side in [&each, &run] {
        side.take(&pick(&|i| !inside(i) && !before(i))).await;
    }
    same("gaps, the rest in", &each.engine, &run.engine);
    period("gaps, the period with gaps inside and before", &each, &run, Some(0), from, to).await;
    for side in [&each, &run] {
        side.take(&pick(&inside)).await;
    }
    same("gaps, inside filled", &each.engine, &run.engine);
    period("gaps, the period with gaps before", &each, &run, Some(0), from, to).await;
    for side in [&each, &run] {
        side.put(&pick(&before)).await;
    }
    period("gaps, the period with logs before it waiting", &each, &run, Some(0), from, to).await;
    period("gaps, the period again", &each, &run, Some(0), from, to).await;
    period("gaps, filled, the middle third", &each, &run, Some(0), t(n / 3), t(2 * n / 3)).await;
    // logs stored inside the period and left for its rebuild
    let (each, run) = (Side::new(1).await, Side::new(batch).await);
    for side in [&each, &run] {
        side.take(&pick(&|i| !inside(i))).await;
        side.put(&pick(&inside)).await;
    }
    period("gaps inside, stored and left for the period's rebuild", &each, &run, Some(0), from, to).await;

    // two cars: each car's period, and one of every car
    let other: Vec<synth::LogFile> = scenario().car(1).logs().collect();
    let two = |window_batch| engine::Options { window_batch, new_keys: engine::NewKeys::NewCar, ..Default::default() };
    let (each, run) = (Side::with(two(1)).await, Side::with(two(batch)).await);
    let other_logs: Vec<&synth::LogFile> = other.iter().collect();
    for side in [&each, &run] {
        side.take(&all).await;
        side.take(&other_logs).await;
    }
    assert_eq!(each.cars().len(), 2, "two cars");
    same("two cars in", &each.engine, &run.engine);
    period("two cars, the first's middle third", &each, &run, Some(0), from, to).await;
    let m = other.len();
    let (from1, to1) = (at(&other[m / 4]), at(&other[m / 2]));
    period("two cars, the second's second quarter", &each, &run, Some(1), from1, to1).await;
    period("two cars, every car's middle third", &each, &run, None, from, to).await;
    middle
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_rebuild_of_a_period_gives_the_same_timeline_per_run_of_logs() {
    let (each, run) = periods(1, 16).await;
    // (and its windows were rebuilt per run: far fewer of them)
    assert!(run * 3 < each, "{run} windows against {each}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "long: cargo test --release -p sentry-mate a_rebuild_of_a_period -- --ignored --nocapture"]
async fn a_rebuild_of_a_period_gives_the_same_timeline_over_a_week() {
    for (days, batch) in [(2, 16), (7, 7), (7, 64), (7, 100_000)] {
        let (each, run) = periods(days, batch).await;
        println!("{days} days, runs of {batch}: the middle third's windows {each} after every log, {run} once per run");
    }
}

/// Drives or charges, each (start, end).
type Spans = Vec<(u64, u64)>;

/// A car's drives, and its charges.
fn kinds_of(e: &engine::Engine, car: model::CarId) -> (Spans, Spans) {
    let db = e.db();
    let q = db::Query::default().oldest_first().limit(db::MAX_PAGE);
    (
        db.list::<Drive>(car, &q).unwrap().items.iter().map(|d| (d.start_ms, d.end_ms)).collect(),
        db.list::<Charge>(car, &q).unwrap().items.iter().map(|c| (c.start_ms, c.end_ms)).collect(),
    )
}

/// A rebuild of a period whose end cuts a drive or a charge, through both engines: compared after it, and the
/// windows rebuilt once per run of logs must be fewer than after every log (the engine's fast path ran: where it
/// does not, both sides rebuild the same number of windows and a comparison proves nothing about it).
async fn cut(what: &str, each: &Side, run: &Side, car: Option<usize>, from_ms: u64, to_ms: u64) -> (u64, u64) {
    let (w, v) = period(what, each, run, car, from_ms, to_ms).await;
    assert!(v < w, "{what}: {v} windows once per run against {w} after every log: the fast path did not run");
    (w, v)
}

/// The same where the engine keeps a window per log: logs stored and not worked on yet start outside the
/// period, so the rebuild works on them too; equal counts.
async fn cut_left(what: &str, each: &Side, run: &Side, car: Option<usize>, from_ms: u64, to_ms: u64) {
    let (w, v) = period(what, each, run, car, from_ms, to_ms).await;
    assert_eq!(w, v, "{what}: a window per log either way");
}

/// Where a period starts that is rebuilt per log from the hour before a drive or charge starting at `s`: a dozen
/// logs before that.
fn period_from(logs: &[synth::LogFile], s: u64) -> u64 {
    let i = logs.iter().position(|l| at(l) + 3_600_000 >= s).unwrap();
    at(&logs[i.saturating_sub(12)])
}

/// The ends to try in a drive or a charge `(s, e)`: one log after its start, its middle, its last log (a charge:
/// 18 minutes in, the length of the week's known case, as well), and exactly its start and its end. `quick`: the middle of a drive, or 18 minutes into a charge, only.
fn ends_in(logs: &[synth::LogFile], (s, e): (u64, u64), charge: bool, quick: bool) -> Vec<(&'static str, u64)> {
    let inside: Vec<u64> = logs.iter().map(at).filter(|&x| x > s && x < e).collect();
    assert!(inside.len() >= 3, "{} logs in ({s}, {e})", inside.len());
    let mut ends = vec![
        ("one log after the start", inside[0]),
        ("the middle", at(&logs[log_inside(logs, s, e).unwrap()])),
        ("the last log", *inside.last().unwrap()),
    ];
    if charge && let Some(&x) = inside.iter().find(|&&x| x >= s + 18 * 60_000) {
        ends.push(("18 minutes in", x));
    }
    if quick {
        ends.retain(|(name, _)| if charge { *name == "18 minutes in" } else { *name == "the middle" });
    } else {
        ends.push(("exactly at the start", s));
        ends.push(("exactly at the end", e));
    }
    ends
}

/// Rebuilds of periods ending in the drive or charge `span` of the car, each starting a dozen logs before the hour before it. `left`: the starts of logs stored and not worked on.
async fn cut_ends(
    what: &str,
    (each, run): (&Side, &Side),
    logs: &[synth::LogFile],
    car: usize,
    (span, charge): ((u64, u64), bool),
    quick: bool,
    left: &[u64],
) {
    for (name, to) in ends_in(logs, span, charge, quick) {
        // (a dozen logs before the hour before the drive or charge: the logs a window per log is rebuilt for before
        // it, and the engine goes per log only from there)
        let from = period_from(logs, span.0);
        let kind = if charge { "charge" } else { "drive" };
        let what = format!("{what}, a period ending {name} of a {kind}");
        if left.iter().any(|&x| x < from || x >= to) {
            cut_left(&what, each, run, Some(car), from, to).await;
        } else {
            cut(&what, each, run, Some(car), from, to).await;
        }
    }
}

/// The longest drive and the longest charge a car's history holds.
fn longest(e: &engine::Engine, car: model::CarId) -> ((u64, u64), (u64, u64)) {
    let (d, c) = kinds_of(e, car);
    // (not one in the first hours: the periods start before it)
    let first = kinds_of(e, car).0.iter().chain(&kinds_of(e, car).1).map(|s| s.0).min().unwrap_or(0) + 4 * 3_600_000;
    let long = |v: Vec<(u64, u64)>| v.into_iter().filter(|s| s.0 > first).max_by_key(|&(a, b)| b - a).unwrap();
    (long(d), long(c))
}

/// A stored drive or charge of the car that goes on across `t`.
fn span_across(e: &engine::Engine, car: model::CarId, t: u64, charge: bool) -> (u64, u64) {
    let (d, c) = kinds_of(e, car);
    (if charge { c } else { d }).into_iter().find(|&(a, b)| a < t && t < b).expect("a span across it")
}

/// Rebuilds of a period whose end cuts a drive or a charge, per log and per run of logs: everything stored
/// compared, ids included, after every step, and the windows per run fewer than per log.
async fn cut_periods(days: u32, batch: usize) {
    let scenario = || synth::Scenario::new(7).days(days).rates(synth::Rates::Light).with_trouble(true);
    let logs: Vec<synth::LogFile> = scenario().logs().collect();
    let n = logs.len();
    let t = |i: usize| at(&logs[i]);
    let all: Vec<&synth::LogFile> = logs.iter().collect();
    let pick = |f: &dyn Fn(usize) -> bool| -> Vec<&synth::LogFile> {
        logs.iter().enumerate().filter(|(i, _)| f(*i)).map(|(_, l)| l).collect()
    };
    let pair = || async { (Side::new(1).await, Side::new(batch).await) };
    let (each, run) = pair().await;
    each.take(&all).await;
    run.take(&all).await;
    let car = each.cars()[0];
    let (drive, charge) = longest(&each.engine, car);
    let (d_mid, c_mid) = (drive.0 + (drive.1 - drive.0) / 2, charge.0 + (charge.1 - charge.0) / 2);
    println!("{n} logs; the longest drive {drive:?}, the longest charge {charge:?}");
    let spans = [("a drive", drive, false), ("a charge", charge, true)];

    // every end, in a drive and in a charge
    cut_ends("all logs in", (&each, &run), &logs, 0, (drive, false), false, &[]).await;
    cut_ends("all logs in", (&each, &run), &logs, 0, (charge, true), false, &[]).await;
    // the middle third by count (the week's known case ends 18 minutes into a charge)
    let (w, v) = period("the middle third", &each, &run, Some(0), t(n / 3), t(2 * n / 3)).await;
    // (a period that starts within an hour of the cut drive or charge gets a window per log all through)
    assert!(v <= w && (days > 1 || v * 2 < w), "the middle third: {v} windows against {w}");
    // both edges cut: from inside a drive to inside a charge and back, and from a drive to another
    let (a, b) = (d_mid.min(c_mid), d_mid.max(c_mid));
    cut("both edges cut, a drive and a charge", &each, &run, Some(0), a, b).await;
    let (drives, _) = kinds_of(&each.engine, car);
    let inside = |(x, y): (u64, u64)| at(&logs[log_inside(&logs, x, y).unwrap()]);
    cut("both edges cut, two drives", &each, &run, Some(0), inside(drives[0]), inside(drives[drives.len() - 1])).await;
    each.rebuild().await;
    run.rebuild().await;
    same("cut periods, rebuilt whole", &each.engine, &run.engine);

    // gaps inside the logs of the drive or charge: left out, filled later, stored and left for the rebuild
    for (what, (s, e), charge) in spans {
        let gap = |i: usize| t(i) > s && t(i) < e && i % 3 == 1;
        let across = |side: &Side| span_across(&side.engine, side.cars()[0], s + (e - s) / 2, charge);
        let (each, run) = pair().await;
        for side in [&each, &run] {
            side.take(&pick(&|i| !gap(i))).await;
        }
        let q = (&each, &run);
        cut_ends(&format!("gaps left out of {what}"), q, &logs, 0, (across(&each), charge), true, &[]).await;
        for side in [&each, &run] {
            side.take(&pick(&gap)).await;
        }
        cut_ends(&format!("gaps filled later in {what}"), q, &logs, 0, (across(&each), charge), true, &[]).await;
        let (each, run) = pair().await;
        for side in [&each, &run] {
            side.take(&pick(&|i| !gap(i))).await;
            side.put(&pick(&gap)).await;
        }
        let left: Vec<u64> = (0..n).filter(|&i| gap(i)).map(t).collect();
        cut_ends(&format!("gaps in {what} stored"), (&each, &run), &logs, 0, (across(&each), charge), true, &left)
            .await;
    }

    // gaps before the period (the forty logs that end where its first log is), then stored, then filled
    let first = logs.iter().position(|l| at(l) + 3_600_000 >= charge.0).unwrap().saturating_sub(12);
    assert!(first >= 40, "{first}");
    let before = |i: usize| (first - 40..first).contains(&i);
    let (each, run) = pair().await;
    for side in [&each, &run] {
        side.take(&pick(&|i| !before(i))).await;
    }
    cut_ends("gaps before", (&each, &run), &logs, 0, (charge, true), true, &[]).await;
    for side in [&each, &run] {
        side.put(&pick(&before)).await;
    }
    let left: Vec<u64> = (0..n).filter(|&i| before(i)).map(t).collect();
    cut_ends("gaps before, stored", (&each, &run), &logs, 0, (charge, true), true, &left).await;
    for side in [&each, &run] {
        side.take(&pick(&before)).await;
    }
    cut_ends("gaps before, filled", (&each, &run), &logs, 0, (charge, true), true, &[]).await;

    // a start that moves earlier: the first logs of the drive or charge are stored and left for the rebuild, so
    // the stored one starts later than the one the rebuild makes
    for (what, (s, e), charge) in spans {
        for lead in if charge { [(e - s) / 4, (e - s) * 3 / 4] } else { [(e - s) / 6, (e - s) / 3] } {
            let head = |i: usize| t(i) + 5 * 60_000 > s && t(i) < s + lead;
            let (each, run) = pair().await;
            for side in [&each, &run] {
                side.take(&pick(&|i| !head(i))).await;
                side.put(&pick(&head)).await;
            }
            let span = span_across(&each.engine, car, e - (e - s) / 8, charge);
            println!("{what}: stored from {}, made from {s} ({} ms earlier)", span.0, span.0 - s);
            let name = format!("the start of {what} moving earlier by {} min", (span.0 - s) / 60_000);
            let left: Vec<u64> = (0..n).filter(|&i| head(i)).map(t).collect();
            cut_ends(&name, (&each, &run), &logs, 0, (span, charge), true, &left).await;
            // a start that moved earlier by more than the hour: the period starts before the real start, so logs
            // that carry the head of the drive or charge lie before the stored start's hour
            if span.0 - s > 3_600_000 {
                let to = ends_in(&logs, span, charge, true)[0].1;
                let name = format!("{name}, the period starting before the real start");
                let from = period_from(&logs, s);
                if left.iter().any(|&x| x < from || x >= to) {
                    cut_left(&name, &each, &run, Some(0), from, to).await;
                } else {
                    cut(&name, &each, &run, Some(0), from, to).await;
                }
            }
        }
    }

    // two cars: each car's period, and one of every car
    let other: Vec<synth::LogFile> = scenario().car(1).logs().collect();
    let two = |window_batch| engine::Options { window_batch, new_keys: engine::NewKeys::NewCar, ..Default::default() };
    let (each, run) = (Side::with(two(1)).await, Side::with(two(batch)).await);
    let other_logs: Vec<&synth::LogFile> = other.iter().collect();
    for side in [&each, &run] {
        side.take(&all).await;
        side.take(&other_logs).await;
    }
    assert_eq!(each.cars().len(), 2, "two cars");
    cut_ends("two cars, the first", (&each, &run), &logs, 0, (charge, true), true, &[]).await;
    let (d1, c1) = longest(&each.engine, each.cars()[1]);
    cut_ends("two cars, the second", (&each, &run), &other, 1, (c1, true), true, &[]).await;
    cut_ends("two cars, the second", (&each, &run), &other, 1, (d1, false), true, &[]).await;
    cut("two cars, every car, ending in the first's drive", &each, &run, None, period_from(&logs, drive.0), d_mid)
        .await;
    cut("two cars, every car, ending in the first's charge", &each, &run, None, period_from(&logs, charge.0), c_mid)
        .await;
    let to1 = c1.0 + (c1.1 - c1.0) / 2;
    cut("two cars, every car, ending in the second's charge", &each, &run, None, period_from(&other, c1.0), to1).await;

    // a restart between the clearing and the tail, and one in the tail: the logs left are worked on by an engine
    // started over the same data, from the mark the rebuild kept
    let (from, to) = (period_from(&logs, charge.0), c_mid);
    let count = logs.iter().filter(|l| at(l) >= from && at(l) < to).count() as u64;
    for stop in [count / 4, count / 2, count - 6] {
        let (each, run) = pair().await;
        each.take(&all).await;
        run.take(&all).await;
        let w = each.rebuild_period(Some(each.cars()[0]), from, to).await;
        let begun = run.engine.stats().windows_rebuilt;
        run.engine.rebuild_crash_after_logs(Some(stop));
        let car = run.cars()[0];
        run.rebuild_period(Some(car), from, to).await;
        let first = run.engine.stats().windows_rebuilt - begun;
        let data = run._dir.path().join("data");
        let vault = Arc::new(Vault::open(&data, &run._dir.path().join("keys")).unwrap());
        let opts = engine::Options { window_batch: batch, ..engine::Options::default() };
        let again = engine::Engine::new(run.engine.db().clone(), Store::new(can_dir(&data), vault), opts);
        again.start().unwrap();
        again.idle().await;
        let k = same(&format!("a restart after {stop} of {count} logs"), &each.engine, &again);
        let last = again.stats().windows_rebuilt;
        println!(
            "a restart after {stop} of {count} logs: {k} the same; windows {w} per log, {first} and {last} per run"
        );
        assert!(first + last < w, "{first} + {last} windows against {w}: the fast path did not run");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_rebuild_of_a_period_cutting_a_drive_or_charge_gives_the_same_timeline_per_run_of_logs() {
    cut_periods(1, 16).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "long: cargo test --release -p sentry-mate a_rebuild_of_a_period_cutting -- --ignored --nocapture"]
async fn a_rebuild_of_a_period_cutting_a_drive_or_charge_gives_the_same_timeline_over_days() {
    for (days, batch) in [(2, 16), (7, 7), (7, 64), (7, 100_000)] {
        cut_periods(days, batch).await;
    }
}

/// Waits for the engine's full rebuild to end.
async fn full_done(e: &engine::Engine) -> engine::full::FullRecord {
    for _ in 0..60_000 {
        let r = e.full_rebuild_status().unwrap().unwrap();
        if !r.running() {
            e.idle().await;
            return r;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the full rebuild did not end");
}

/// The logs read and decoded ahead on other cores ([`engine::Options::read_ahead`]) give the very history that
/// reading each in its turn gives: the same made-up logs through an engine that reads each log in place and one with
/// `threads` threads reading ahead, at runs of `batch` logs, everything stored compared (ids included) after every
/// step: the logs at the start (the first logs give the car its VIN, so the logs read ahead before it was seen were
/// decoded with another DBC set and are decoded again in place), gaps filled later, the car rebuilt whole, then a
/// full rebuild, beside API inputs when `apis`. Returns how many logs the second engine took from its threads.
async fn read_ahead(days: u32, batch: usize, threads: usize, apis: bool) -> u64 {
    let logs: Vec<synth::LogFile> =
        synth::Scenario::new(7).days(days).rates(synth::Rates::Light).with_trouble(true).logs().collect();
    let n = logs.len();
    let all: Vec<&synth::LogFile> = logs.iter().collect();
    let opts = |read_ahead| engine::Options { window_batch: batch, read_ahead, ..engine::Options::default() };
    let (each, run) = (Side::with(opts(0)).await, Side::with(opts(threads)).await);
    let what = format!("{days} days, runs of {batch}, {threads} threads{}", if apis { ", beside APIs" } else { "" });
    let gap = |i: usize| i % 5 == 2 || (n / 2..n / 2 + 90).contains(&i);
    let first: Vec<&synth::LogFile> = all.iter().enumerate().filter(|(i, _)| !gap(*i)).map(|(_, l)| *l).collect();
    let later: Vec<&synth::LogFile> = all.iter().enumerate().filter(|(i, _)| gap(*i)).map(|(_, l)| *l).collect();
    if apis {
        let o = history::Options {
            days,
            end_ms: synth::DEFAULT_START_MS + u64::from(days) * 86_400_000,
            seed: 5,
            drives: 6 * days,
            charges: 2 * days,
            drive_step_s: 5,
            teslafi_days: 1,
            api_days: days,
            tesla_days: 0,
            tesla_sentry: false,
        };
        for side in [&each, &run] {
            side.take(&first[..20]).await;
            history::run(&side.engine, &o).await.unwrap();
            side.engine.idle().await;
        }
        same(&format!("{what}: the history in"), &each.engine, &run.engine);
    }
    each.take(&first).await;
    run.take(&first).await;
    let k = same(&format!("{what}: the logs at the start"), &each.engine, &run.engine);
    println!("{what}: {} logs at the start, {k} stored things the same", first.len());
    each.take(&later).await;
    run.take(&later).await;
    same(&format!("{what}: the gaps filled"), &each.engine, &run.engine);
    each.rebuild().await;
    run.rebuild().await;
    same(&format!("{what}: rebuilt whole"), &each.engine, &run.engine);
    for side in [&each, &run] {
        let car = side.cars()[0];
        side.engine.full_rebuild(Some(car), engine::full::Reason::Owner, None).unwrap();
        let r = full_done(&side.engine).await;
        assert_eq!((r.status, r.logs_done), (engine::full::FullStatus::Done, n as u64), "{r:#?}");
    }
    let k = same(&format!("{what}: a full rebuild"), &each.engine, &run.engine);
    assert!(k > 1_000, "{what}: {k}");
    assert_eq!(each.engine.stats().failures + run.engine.stats().failures, 0);
    assert_eq!(each.engine.stats().logs_read_ahead, 0);
    let used = run.engine.stats().logs_read_ahead;
    println!("{what}: {k} stored things the same after a full rebuild; {used} logs read ahead were used");
    used
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn logs_read_ahead_on_other_cores_give_the_same_history() {
    // (the full rebuild's logs are decoded by its own engine, whose counts are not this one's: these are the logs
    // at the start, the gaps and the car rebuilt whole)
    let used = read_ahead(1, 16, 3, false).await;
    assert!(used > 50, "{used}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "long: cargo test --release -p sentry-mate logs_read_ahead -- --ignored --nocapture"]
async fn logs_read_ahead_on_other_cores_give_the_same_history_over_a_week() {
    for (days, batch, threads, apis) in [(7, 64, 3, false), (7, 1, 7, false), (3, 64, 3, true), (2, 7, 2, true)] {
        read_ahead(days, batch, threads, apis).await;
    }
}

/// A window's newest values for the live stream (`now_view`, which reads the car's last 30 days of drives,
/// imported ones included) are worked out only while someone listens to the engine's events: nobody listens to a
/// full rebuild's engine over its shadow. The same made-up logs, beside a TeslaMate import and the Tesla API in
/// sentry mode (`how`), through an engine someone listens to and one nobody does, everything stored compared (ids
/// included) after the logs at the start, the gaps filled, the car rebuilt whole and a full rebuild.
async fn unwatched(days: u32, how: Logs) {
    let o = engine::Options::default();
    let (got, _, _) = two_engines(&format!("{days} days ({how:?})"), days, how, o.clone(), o, true).await;
    // (the listened-to engine sent its windows' newest values)
    assert!(got > 10, "{days} days: {got} now events");
}

/// The logs [`two_engines`] are given: the usual made-up days alone, beside a TeslaMate import and the Tesla API in
/// sentry mode, or a car in sentry mode's tiny logs beside them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Logs {
    Alone,
    Apis,
    Sentry,
    /// The same, 3 s every 10 minutes: fewer logs (the tests in the suite).
    SentrySparse,
}

/// The same made-up logs through two engines (with `a` and `b`; someone listens to the first one's events when
/// `listen`), beside a TeslaMate import and the Tesla API in sentry mode unless `how` is alone, everything stored
/// compared (ids included) after the logs at the start, the gaps filled, the car rebuilt whole and a full rebuild.
/// Returns the `now` events the first sent, and its windows bounded near their log and in all (its own, not its
/// full rebuild's).
async fn two_engines(
    what: &str,
    days: u32,
    how: Logs,
    a: engine::Options,
    b: engine::Options,
    listen: bool,
) -> (u64, u64, u64) {
    let apis = how != Logs::Alone;
    let s = synth::Scenario::new(7).days(days).rates(synth::Rates::Light).with_trouble(true);
    // (a car in sentry mode: tiny logs, 3 s of frames every 69 s, as the owner's rebuild speed is measured on)
    let s = match how {
        Logs::Sentry => s.stays_awake(true).snippets(69_000, 3_000),
        Logs::SentrySparse => s.stays_awake(true).snippets(600_000, 3_000),
        Logs::Alone | Logs::Apis => s,
    };
    let logs: Vec<synth::LogFile> = s.logs().collect();
    let n = logs.len();
    let all: Vec<&synth::LogFile> = logs.iter().collect();
    let (watched, alone) = (Side::with(a).await, Side::with(b).await);
    let nows = Arc::new(std::sync::atomic::AtomicU64::new(0));
    // (the events read as they come, as the live stream does)
    let got = nows.clone();
    let reader = listen.then(|| {
        let mut events = watched.engine.subscribe();
        tokio::spawn(async move {
            loop {
                match events.recv().await {
                    Ok(engine::Event::Now { .. }) => {
                        got.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                    Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                }
            }
        })
    });
    let what = format!("{what}{}", if apis { ", beside APIs" } else { "" });
    let gap = |i: usize| i % 5 == 2 || (n / 2..n / 2 + 90).contains(&i);
    let first: Vec<&synth::LogFile> = all.iter().enumerate().filter(|(i, _)| !gap(*i)).map(|(_, l)| *l).collect();
    let later: Vec<&synth::LogFile> = all.iter().enumerate().filter(|(i, _)| gap(*i)).map(|(_, l)| *l).collect();
    if apis {
        let o = history::Options {
            days: days + 20,
            end_ms: synth::DEFAULT_START_MS + u64::from(days) * 86_400_000,
            seed: 5,
            drives: 4 * (days + 20),
            charges: days + 20,
            drive_step_s: 5,
            teslafi_days: 0,
            api_days: 0,
            tesla_days: days,
            tesla_sentry: true,
        };
        for side in [&watched, &alone] {
            side.take(&first[..20]).await;
            history::run(&side.engine, &o).await.unwrap();
            side.engine.idle().await;
        }
        same(&format!("{what}: the history in"), &watched.engine, &alone.engine);
    }
    watched.take(&first).await;
    alone.take(&first).await;
    same(&format!("{what}: the logs at the start"), &watched.engine, &alone.engine);
    watched.take(&later).await;
    alone.take(&later).await;
    same(&format!("{what}: the gaps filled"), &watched.engine, &alone.engine);
    watched.rebuild().await;
    alone.rebuild().await;
    same(&format!("{what}: rebuilt whole"), &watched.engine, &alone.engine);
    for side in [&watched, &alone] {
        let car = side.cars()[0];
        side.engine.full_rebuild(Some(car), engine::full::Reason::Owner, None).unwrap();
        let r = full_done(&side.engine).await;
        assert_eq!((r.status, r.logs_done), (engine::full::FullStatus::Done, n as u64), "{r:#?}");
    }
    let k = same(&format!("{what}: a full rebuild"), &watched.engine, &alone.engine);
    assert!(k > 1_000, "{what}: {k}");
    assert_eq!(watched.engine.stats().failures + alone.engine.stats().failures, 0);
    if let Some(r) = reader {
        r.abort();
    }
    println!(
        "{what}: windows bounded near their log: {} and {} (of {} and {})",
        watched.engine.stats().windows_bounded_near,
        alone.engine.stats().windows_bounded_near,
        watched.engine.stats().windows_rebuilt,
        alone.engine.stats().windows_rebuilt
    );
    let got = nows.load(std::sync::atomic::Ordering::Relaxed);
    println!("{what}: {k} stored things the same on both engines; {got} now events sent by the first");
    let st = watched.engine.stats();
    (got, st.windows_bounded_near, st.windows_rebuilt)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn windows_nobody_listens_to_give_the_same_history() {
    unwatched(1, Logs::SentrySparse).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "long: cargo test --release -p sentry-mate windows_nobody_listens -- --ignored --nocapture"]
async fn windows_nobody_listens_to_give_the_same_history_over_days() {
    for (days, how) in [(4, Logs::Apis), (3, Logs::Alone), (2, Logs::Sentry)] {
        unwatched(days, how).await;
    }
}

/// A window's bounds looked for first in a short stretch around its log ([`engine::Options::window_look_ms`], two
/// hours) give the very history the two days looked at before give: the same made-up logs through an engine of each,
/// beside a TeslaMate import and the Tesla API in sentry mode (a car with a cut point every five minutes), and
/// without them; everything stored compared, ids included.
async fn looked_near(days: u32, how: Logs) {
    let near = engine::Options::default();
    let far = engine::Options { window_look_ms: 2 * 86_400_000, ..engine::Options::default() };
    let what = format!("{days} days ({how:?}), bounds looked for near the log");
    let (_, near, all) = two_engines(&what, days, how, near, far, false).await;
    // (in sentry mode most windows were bounded near their log: the comparison is of that; a car that sleeps has a
    // settled span, often further than two hours away)
    if matches!(how, Logs::Sentry | Logs::SentrySparse) {
        assert!(near * 2 > all, "{what}: {near} of {all} windows bounded near their log");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn window_bounds_looked_for_near_the_log_give_the_same_history() {
    looked_near(1, Logs::SentrySparse).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "long: cargo test --release -p sentry-mate window_bounds_looked_for -- --ignored --nocapture"]
async fn window_bounds_looked_for_near_the_log_give_the_same_history_over_days() {
    for (days, how) in [(7, Logs::Alone), (1, Logs::Sentry), (4, Logs::Sentry), (4, Logs::Apis)] {
        looked_near(days, how).await;
    }
}

/// A full rebuild that rebuilds its windows once per day of logs ([`engine::Options::full_by_day`]) gives the very
/// history the full rebuild before it gives (a window per log beside an API input, one per 64 logs alone): the same
/// made-up logs through an engine of each, then (unless `how` is alone) a TeslaMate import from `back` days before
/// them, a TeslaFi import over its first two days, TeslaFi's API and local TeslaMate over the logs' last `api_days`
/// days and the Tesla API in sentry mode over all of them; the history in use compared, then a full rebuild of the
/// car on each, once per entry of `cuts`: the second one cut off after that many logs (as by a kill) and carried on
/// by a new engine over the same data when given (the last entry only). Each time the two shadows are compared once
/// their logs are done, before the switch gives their rows the ids of the history in use, and the histories after
/// the switch: everything stored, ids included. Returns the windows each kind of full rebuild rebuilt in all.
async fn by_day(days: u32, how: Logs, back: u32, api_days: u32, cuts: &[Option<u64>]) -> (u64, u64) {
    let before = engine::Options { full_by_day: false, ..engine::Options::default() };
    by_day_against(before, false, days, how, back, api_days, cuts).await
}

/// [`by_day`] against the full rebuild `before` gives (its engine's options), the engine's records compared too;
/// `firmware`: the logs carry made-up firmware fingerprints, with a software update among them.
async fn by_day_against(
    before: engine::Options,
    firmware: bool,
    days: u32,
    how: Logs,
    back: u32,
    api_days: u32,
    cuts: &[Option<u64>],
) -> (u64, u64) {
    let apis = how != Logs::Alone;
    let s = synth::Scenario::new(7).days(days).rates(synth::Rates::Light).with_trouble(true);
    let s = match how {
        Logs::Sentry => s.stays_awake(true).snippets(69_000, 3_000),
        Logs::SentrySparse => s.stays_awake(true).snippets(600_000, 3_000),
        Logs::Alone | Logs::Apis => s,
    };
    let s = if firmware { s.firmware(synth::firmware::Firmware::made_up()) } else { s };
    let logs: Vec<synth::LogFile> = s.logs().collect();
    let n = logs.len() as u64;
    let all: Vec<&synth::LogFile> = logs.iter().collect();
    let (old, new) = (Side::with(before).await, Side::with(engine::Options::default()).await);
    let what = format!("{days} days ({how:?})");
    old.take(&all).await;
    new.take(&all).await;
    if apis {
        let o = history::Options {
            days: days + back,
            end_ms: synth::DEFAULT_START_MS + u64::from(days) * 86_400_000,
            seed: 5,
            drives: 4 * (days + back),
            charges: days + back,
            drive_step_s: 5,
            teslafi_days: 2,
            api_days,
            tesla_days: days,
            tesla_sentry: true,
        };
        for side in [&old, &new] {
            history::run(&side.engine, &o).await.unwrap();
            side.engine.idle().await;
        }
    }
    same(&format!("{what}: the history in use"), &old.engine, &new.engine);
    let car = old.cars()[0];
    let at_days = |e: engine::Engine| async move {
        for _ in 0..600_000 {
            let r = e.full_rebuild_status().unwrap().unwrap();
            if r.phase == engine::full::Phase::Days {
                return;
            }
            assert!(r.running(), "{r:#?}");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the full rebuild did not reach its days");
    };
    let shadow = |e: &engine::Engine| e.db().open_shadow(engine::full::SHADOW).unwrap().unwrap();
    let (mut last, mut windows) = (new.engine.clone(), (0, 0));
    for (k, cut) in cuts.iter().enumerate() {
        assert!(cut.is_none() || k + 1 == cuts.len(), "a cut one last");
        assert!(cut.is_none_or(|c| c < n), "a cut before the last log");
        let what =
            format!("{what}, full rebuild {k}{}", cut.map_or_else(String::new, |c| format!(", cut after {c} logs")));
        let w = (old.engine.full_rebuild_windows(), last.full_rebuild_windows());
        let t = Instant::now();
        old.engine.full_rebuild_hold_at(Some(engine::full::Phase::Days));
        old.engine.full_rebuild(Some(car), engine::full::Reason::Owner, None).unwrap();
        at_days(old.engine.clone()).await;
        let took_old = t.elapsed();
        let t = Instant::now();
        let w_new = match cut {
            None => {
                last.full_rebuild_hold_at(Some(engine::full::Phase::Days));
                last.full_rebuild(Some(car), engine::full::Reason::Owner, None).unwrap();
                at_days(last.clone()).await;
                last.full_rebuild_windows() - w.1
            }
            Some(c) => {
                last.full_rebuild_crash_after_logs(Some(*c));
                last.full_rebuild(Some(car), engine::full::Reason::Owner, None).unwrap();
                last.idle().await;
                let cut_windows = last.full_rebuild_windows() - w.1;
                let data = new._dir.path().join("data");
                let v = Arc::new(Vault::open(&data, &new._dir.path().join("keys")).unwrap());
                let e =
                    engine::Engine::new(last.db().clone(), Store::new(can_dir(&data), v), engine::Options::default());
                e.full_rebuild_hold_at(Some(engine::full::Phase::Days));
                e.start().unwrap();
                at_days(e.clone()).await;
                last = e;
                cut_windows + last.full_rebuild_windows()
            }
        };
        let took_new = t.elapsed();
        let w_old = old.engine.full_rebuild_windows() - w.0;
        let k = same_in(&format!("{what}: the shadows"), &shadow(&old.engine), &shadow(&last));
        let rk = records_same(&format!("{what}: the shadows' records"), &shadow(&old.engine), &shadow(&last));
        for e in [&old.engine, &last] {
            e.full_rebuild_hold_at(None);
            let r = full_done(e).await;
            assert_eq!((r.status, r.logs_done), (engine::full::FullStatus::Done, n), "{what}: {r:#?}");
        }
        let j = same(&format!("{what}: switched"), &old.engine, &last);
        let rj = records_same(&format!("{what}: the records switched"), old.engine.db(), last.db());
        assert!(j > 1_000, "{what}: {j}");
        assert_eq!(old.engine.stats().failures + new.engine.stats().failures + last.stats().failures, 0, "{what}");
        println!(
            "{what}: {n} logs, {k} stored things and {rk} records of the shadows the same, {j} and {rj} after the \
             switch; windows {w_old} before ({took_old:.1?}), {w_new} by day ({took_new:.1?})"
        );
        windows = (windows.0 + w_old, windows.1 + w_new);
    }
    windows
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_full_rebuild_by_day_gives_the_history_a_window_per_log_gives() {
    // two days of a car in sentry mode beside every input, a full rebuild twice and once more cut off the next day
    let (before, now) = by_day(2, Logs::SentrySparse, 3, 1, &[None, None, Some(170)]).await;
    assert!(now * 10 < before, "{now} windows against {before}");
    by_day(2, Logs::Alone, 0, 0, &[None]).await;
}

/// A full rebuild by day that holds the records each log changes and writes them once a day
/// ([`engine::Options::full_hold_records`]) gives the records one that writes them for every log gives, and the
/// same history: a car in sentry mode beside every input, twice, then cut off in the middle of its second day (as
/// by a kill: what was held is lost) and carried on by a new engine.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_full_rebuild_holding_its_records_gives_the_records_written_for_every_log() {
    let before = engine::Options { full_hold_records: false, ..engine::Options::default() };
    by_day_against(before, true, 2, Logs::SentrySparse, 3, 1, &[None, Some(220)]).await;
    let before = engine::Options { full_hold_records: false, ..engine::Options::default() };
    by_day_against(before, true, 2, Logs::Alone, 0, 0, &[None, Some(150)]).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "long: cargo test --release -p sentry-mate a_full_rebuild_holding -- --ignored --nocapture"]
async fn a_full_rebuild_holding_its_records_gives_the_records_written_for_every_log_over_days() {
    let before = engine::Options { full_hold_records: false, ..engine::Options::default() };
    by_day_against(before, true, 3, Logs::Sentry, 60, 3, &[None, Some(2_000)]).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "long: cargo test --release -p sentry-mate a_full_rebuild_by_day -- --ignored --nocapture"]
async fn a_full_rebuild_by_day_gives_the_history_a_window_per_log_gives_over_days() {
    by_day(3, Logs::Sentry, 60, 3, &[None, Some(2_000)]).await;
    by_day(5, Logs::Apis, 20, 2, &[None, Some(500)]).await;
    by_day(7, Logs::Alone, 0, 0, &[None, Some(400)]).await;
}

/// A rebuild job to end.
async fn job_ended(e: &engine::Engine, id: u64) -> engine::Job {
    let j = job_over(e, id).await;
    e.idle().await;
    j
}

/// [`job_ended`], the engine's other work not waited for (a full rebuild held at a step, say).
async fn job_over(e: &engine::Engine, id: u64) -> engine::Job {
    for _ in 0..600_000 {
        let j = e.job(id).unwrap();
        if j.status != engine::JobStatus::Running {
            return j;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the job did not end");
}

/// A GPS layout change's narrow rebuild with its windows rebuilt once per day of logs
/// ([`engine::Options::positions_by_day`]) gives the history a window per log gives: the same made-up logs (with
/// the Screenmate's own GPS frames, so each is decoded again) through an engine of each kind, a car in sentry mode
/// beside every input, or alone; a layout that moves every position set, then the DBC's own layout put back,
/// everything stored compared after each, ids and the engine's records included. Returns the windows each rebuilt
/// for the changes.
async fn layout_change(days: u32, how: Logs, back: u32, api_days: u32) -> (u64, u64) {
    let apis = how != Logs::Alone;
    let s = synth::Scenario::new(7).days(days).rates(synth::Rates::Light).with_trouble(true);
    let s = match how {
        Logs::Sentry => s.stays_awake(true).snippets(69_000, 3_000),
        Logs::SentrySparse => s.stays_awake(true).snippets(600_000, 3_000),
        Logs::Alone | Logs::Apis => s,
    };
    let logs: Vec<synth::LogFile> = s.logs().collect();
    let all: Vec<&synth::LogFile> = logs.iter().collect();
    let each = engine::Options { positions_by_day: false, rebuild_settle_ms: 0, ..engine::Options::default() };
    let by_day = engine::Options { rebuild_settle_ms: 0, ..engine::Options::default() };
    let (old, new) = (Side::with(each).await, Side::with(by_day).await);
    let what = format!("{days} days ({how:?})");
    old.take(&all).await;
    new.take(&all).await;
    if apis {
        let o = history::Options {
            days: days + back,
            end_ms: synth::DEFAULT_START_MS + u64::from(days) * 86_400_000,
            seed: 5,
            drives: 4 * (days + back),
            charges: days + back,
            drive_step_s: 5,
            teslafi_days: 2,
            api_days,
            tesla_days: days,
            tesla_sentry: true,
        };
        for side in [&old, &new] {
            history::run(&side.engine, &o).await.unwrap();
            side.engine.idle().await;
        }
    }
    same(&format!("{what}: the history in use"), &old.engine, &new.engine);
    let car = old.cars()[0];
    let moved: model::gps::GpsLayout = "lat 0|28@1- (1e-6,0.001); lon 28|28@1- (1e-6,0.002)".parse().unwrap();
    let dbc: model::gps::GpsLayout = "lat 0|28@1- (1e-6,0); lon 28|28@1- (1e-6,0)".parse().unwrap();
    let mut windows = (0, 0);
    for (k, layout) in [moved, dbc].into_iter().enumerate() {
        let w = (old.engine.stats().windows_rebuilt, new.engine.stats().windows_rebuilt);
        let mut done = Vec::new();
        for side in [&old, &new] {
            let job = side.engine.set_gps_layout(car, Some(layout)).unwrap().unwrap();
            let j = job_ended(&side.engine, job.id).await;
            assert_eq!(j.status, engine::JobStatus::Done, "{what}, change {k}: {j:?}");
            assert!(!side.engine.rebuild_pending(car), "{what}, change {k}");
            done.push(j.done);
        }
        assert_eq!(done[0], done[1], "{what}, change {k}: the same logs");
        assert!(done[0] * 2 > logs.len() as u64, "{what}, change {k}: most logs decoded again, {done:?}");
        let j = same(&format!("{what}, change {k}"), &old.engine, &new.engine);
        let r = records_same(&format!("{what}, change {k}: the records"), old.engine.db(), new.engine.db());
        assert!(j > 1_000, "{what}: {j}");
        assert_eq!(old.engine.stats().failures + new.engine.stats().failures, 0, "{what}");
        let w = (old.engine.stats().windows_rebuilt - w.0, new.engine.stats().windows_rebuilt - w.1);
        println!(
            "{what}, change {k}: {} logs decoded again, {j} stored things and {r} records the same; windows {} a log \
             at a time, {} by day",
            done[0], w.0, w.1
        );
        windows = (windows.0 + w.0, windows.1 + w.1);
    }
    windows
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_gps_layout_change_by_day_gives_the_history_a_window_per_log_gives() {
    let (before, now) = layout_change(2, Logs::SentrySparse, 3, 1).await;
    assert!(now * 10 < before, "{now} windows against {before}");
    layout_change(2, Logs::Alone, 0, 0).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "long: cargo test --release -p sentry-mate a_gps_layout_change_by_day -- --ignored --nocapture"]
async fn a_gps_layout_change_by_day_gives_the_history_a_window_per_log_gives_over_days() {
    layout_change(3, Logs::Sentry, 60, 3).await;
    layout_change(5, Logs::Apis, 20, 2).await;
    layout_change(7, Logs::Alone, 0, 0).await;
}

/// A full rebuild under way to reach its days step (its logs all decoded), held there by
/// [`engine::Engine::full_rebuild_hold_at`].
async fn full_at_days(e: &engine::Engine) {
    for _ in 0..600_000 {
        let r = e.full_rebuild_status().unwrap().unwrap();
        if r.phase == engine::full::Phase::Days {
            return;
        }
        assert!(r.running(), "{r:#?}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the full rebuild did not reach its days");
}

/// Waits until the car has no rebuild pending (the jobs a switch or a setting asked for).
async fn settled(e: &engine::Engine, car: model::CarId) {
    for _ in 0..600_000 {
        e.idle().await;
        if !e.rebuild_pending(car) && e.jobs().iter().all(|j| j.status != engine::JobStatus::Running) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the car's rebuilds did not end");
}

/// A GPS layout set while a full rebuild runs is not lost at its switch: its logs were decoded with the layout of
/// the run's start, so the switch notes the change again and the car's positions are rebuilt with the new layout,
/// to the history of a car whose layout was set with no full rebuild under way (the same made-up logs, with the
/// Screenmate's GPS frames; everything stored compared, ids and the engine's records included).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_gps_layout_set_during_a_full_rebuild_is_kept_at_its_switch() {
    let logs: Vec<synth::LogFile> =
        synth::Scenario::new(7).days(2).rates(synth::Rates::Light).with_trouble(true).logs().collect();
    let all: Vec<&synth::LogFile> = logs.iter().collect();
    let opts = engine::Options { rebuild_settle_ms: 0, ..engine::Options::default() };
    let (during, alone) = (Side::with(opts.clone()).await, Side::with(opts).await);
    during.take(&all).await;
    alone.take(&all).await;
    let car = during.cars()[0];
    let moved: model::gps::GpsLayout = "lat 0|28@1- (1e-6,0.001); lon 28|28@1- (1e-6,0.002)".parse().unwrap();
    during.engine.full_rebuild_hold_at(Some(engine::full::Phase::Days));
    during.engine.full_rebuild(Some(car), engine::full::Reason::Owner, None).unwrap();
    full_at_days(&during.engine).await;
    // (the shadow's logs are decoded: the layout is set on the history in use, and its job done there)
    let job = during.engine.set_gps_layout(car, Some(moved)).unwrap().unwrap();
    assert_eq!(job_over(&during.engine, job.id).await.status, engine::JobStatus::Done);
    during.engine.full_rebuild_hold_at(None);
    let r = full_done(&during.engine).await;
    assert_eq!(r.status, engine::full::FullStatus::Done, "{r:#?}");
    settled(&during.engine, car).await;
    let job = alone.engine.set_gps_layout(car, Some(moved)).unwrap().unwrap();
    assert_eq!(job_ended(&alone.engine, job.id).await.status, engine::JobStatus::Done);
    settled(&alone.engine, car).await;
    let k = same("a layout set during a full rebuild", &alone.engine, &during.engine);
    let rk = records_same("a layout set during a full rebuild: the records", alone.engine.db(), during.engine.db());
    assert!(k > 1_000, "{k}");
    assert_eq!(during.engine.stats().failures + alone.engine.stats().failures, 0);
    println!("a layout set during a full rebuild: {k} stored things and {rk} records the same");
}

/// A full rebuild gives the history built as the data came: the same made-up logs and inputs through two engines,
/// one of them then rebuilt in full; everything stored compared (ids included, the switch keeps them), a car in
/// sentry mode beside every input, and a car alone.
async fn full_against_arrived(days: u32, how: Logs, back: u32, api_days: u32) {
    let apis = how != Logs::Alone;
    let s = synth::Scenario::new(7).days(days).rates(synth::Rates::Light).with_trouble(true);
    let s = match how {
        Logs::Sentry => s.stays_awake(true).snippets(69_000, 3_000),
        Logs::SentrySparse => s.stays_awake(true).snippets(600_000, 3_000),
        Logs::Alone | Logs::Apis => s,
    };
    let logs: Vec<synth::LogFile> = s.logs().collect();
    let all: Vec<&synth::LogFile> = logs.iter().collect();
    let (arrived, full) = (Side::with(engine::Options::default()).await, Side::with(engine::Options::default()).await);
    let what = format!("{days} days ({how:?})");
    for side in [&arrived, &full] {
        side.take(&all).await;
        if apis {
            let o = history::Options {
                days: days + back,
                end_ms: synth::DEFAULT_START_MS + u64::from(days) * 86_400_000,
                seed: 5,
                drives: 4 * (days + back),
                charges: days + back,
                drive_step_s: 5,
                teslafi_days: 2,
                api_days,
                tesla_days: days,
                tesla_sentry: true,
            };
            history::run(&side.engine, &o).await.unwrap();
            side.engine.idle().await;
        }
    }
    same(&format!("{what}: the history in use"), &arrived.engine, &full.engine);
    let car = full.cars()[0];
    full.engine.full_rebuild(Some(car), engine::full::Reason::Owner, None).unwrap();
    let r = full_done(&full.engine).await;
    assert_eq!(r.status, engine::full::FullStatus::Done, "{r:#?}");
    settled(&full.engine, car).await;
    let k = same(&format!("{what}: built as it came, and rebuilt in full"), &arrived.engine, &full.engine);
    assert!(k > 1_000, "{what}: {k}");
    println!("{what}: {k} stored things the same built as they came and rebuilt in full");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_full_rebuild_gives_the_history_built_as_the_data_came() {
    full_against_arrived(2, Logs::SentrySparse, 3, 1).await;
    full_against_arrived(2, Logs::Alone, 0, 0).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "long: cargo test --release -p sentry-mate a_full_rebuild_gives_the_history_built -- --ignored --nocapture"]
async fn a_full_rebuild_gives_the_history_built_as_the_data_came_over_days() {
    full_against_arrived(3, Logs::Sentry, 60, 3).await;
    full_against_arrived(5, Logs::Apis, 20, 2).await;
    full_against_arrived(7, Logs::Alone, 0, 0).await;
}

/// A full rebuild's switch gives the car the Now its new history works out (the position read with the layout in
/// use), not the one worked out with the old decoding: a layout set and its own rebuild let go before it started, then
/// a full rebuild (which reads the logs with it), against a car whose layout change was rebuilt: everything stored the
/// same, its summary (the Now) included.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_full_rebuild_gives_the_car_the_now_of_its_new_history() {
    let logs: Vec<synth::LogFile> =
        synth::Scenario::new(7).days(1).rates(synth::Rates::Light).with_trouble(true).logs().collect();
    let all: Vec<&synth::LogFile> = logs.iter().collect();
    let (full, narrow) = (
        Side::with(engine::Options { rebuild_settle_ms: 600_000, ..engine::Options::default() }).await,
        Side::with(engine::Options { rebuild_settle_ms: 0, ..engine::Options::default() }).await,
    );
    full.take(&all).await;
    narrow.take(&all).await;
    let car = full.cars()[0];
    let moved: model::gps::GpsLayout = "lat 0|28@1- (1e-6,0.001); lon 28|28@1- (1e-6,0.002)".parse().unwrap();
    let job = full.engine.set_gps_layout(car, Some(moved)).unwrap().unwrap();
    assert!(full.engine.cancel(job.id));
    full.engine.full_rebuild(Some(car), engine::full::Reason::Owner, None).unwrap();
    assert_eq!(full_done(&full.engine).await.status, engine::full::FullStatus::Done);
    settled(&full.engine, car).await;
    let job = narrow.engine.set_gps_layout(car, Some(moved)).unwrap().unwrap();
    assert_eq!(job_ended(&narrow.engine, job.id).await.status, engine::JobStatus::Done);
    let k = same("the Now after a full rebuild", &narrow.engine, &full.engine);
    assert!(k > 1_000, "{k}");
}
