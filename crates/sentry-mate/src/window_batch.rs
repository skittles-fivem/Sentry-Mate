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
//! A rebuild of a period ([`periods`]) clears the period first, so nothing worked out from logs is stored in it,
//! and its logs are rebuilt per run where no drive or charge went on across the period's end. Without that
//! condition two of these periods gave the same times and values and another id: a week's middle third (by count
//! of logs) ends 18 minutes into a charge, and at runs of 64 the charge took the id of its part after the end
//! (rebuilt as a charge of its own on the way) where a window per log kept the id of its start; and a day's
//! period with gaps inside gave a drive cut by the period's end the other id the other way round. The part after
//! the end is kept, and which piece is stored first depends on the windows rebuilt on the way.
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

/// Everything stored for the engine's cars, in named parts (each car's, in the order of their ids).
fn stored(e: &engine::Engine) -> Vec<(String, Vec<String>)> {
    let db = e.db();
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

fn same(what: &str, a: &engine::Engine, b: &engine::Engine) -> usize {
    let (x, y) = (stored(a), stored(b));
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
/// Returns the windows each way of a rebuild of about the middle third whose end cuts no drive or charge (the
/// engine rebuilds a period per run of logs only there).
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
/// sentry mode when `apis`, through an engine someone listens to and one nobody does, everything stored compared
/// (ids included) after the logs at the start, the gaps filled, the car rebuilt whole and a full rebuild.
async fn unwatched(days: u32, apis: bool) {
    let o = engine::Options::default();
    let how = if apis { Logs::Apis } else { Logs::Alone };
    let (got, _, _) = two_engines(&format!("{days} days"), days, how, o.clone(), o, true).await;
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
}

/// The same made-up logs through two engines (with `a` and `b`; someone listens to the first one's events when
/// `listen`), beside a TeslaMate import and the Tesla API in sentry mode when `apis`, everything stored compared
/// (ids included) after the logs at the start, the gaps filled, the car rebuilt whole and a full rebuild. Returns
/// the `now` events the first sent, and its windows bounded near their log and in all (its own, not its full
/// rebuild's).
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
    let s = if how == Logs::Sentry { s.stays_awake(true).snippets(69_000, 3_000) } else { s };
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
    unwatched(1, true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "long: cargo test --release -p sentry-mate windows_nobody_listens -- --ignored --nocapture"]
async fn windows_nobody_listens_to_give_the_same_history_over_days() {
    for (days, apis) in [(4, true), (3, false)] {
        unwatched(days, apis).await;
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
    // (most windows were bounded near their log: the comparison is of that)
    assert!(near * 2 > all, "{what}: {near} of {all} windows bounded near their log");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn window_bounds_looked_for_near_the_log_give_the_same_history() {
    looked_near(1, Logs::Sentry).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "long: cargo test --release -p sentry-mate window_bounds_looked_for -- --ignored --nocapture"]
async fn window_bounds_looked_for_near_the_log_give_the_same_history_over_days() {
    for (days, how) in [(4, Logs::Sentry), (4, Logs::Apis), (7, Logs::Alone)] {
        looked_near(days, how).await;
    }
}
