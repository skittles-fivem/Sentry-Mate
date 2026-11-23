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

/// Everything stored for the engine's one car, in named parts.
fn stored(e: &engine::Engine) -> Vec<(&'static str, Vec<String>)> {
    let db = e.db();
    let fleet = db.fleet(&db::FleetQuery { search: None, limit: 10, after: None }).unwrap();
    assert_eq!(fleet.cars.len(), 1, "one made-up car");
    let entry = &fleet.cars[0];
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
    vec![
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
    ]
}

fn same(what: &str, a: &engine::Engine, b: &engine::Engine) -> usize {
    let (x, y) = (stored(a), stored(b));
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
        let dir = tempfile::tempdir().unwrap();
        let (data, keys) = (dir.path().join("data"), dir.path().join("keys"));
        std::fs::create_dir_all(&data).unwrap();
        let (v, _) = Vault::create(&data, &keys, Kdf::FAST_FOR_TESTS).unwrap();
        let v = Arc::new(v);
        let store = Store::new(can_dir(&data), v.clone());
        let judge = Judge::new(store.clone());
        let db = db::Db::open(&data, v.clone(), db::Options::default()).unwrap();
        let opts = engine::Options { window_batch, ..engine::Options::default() };
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
