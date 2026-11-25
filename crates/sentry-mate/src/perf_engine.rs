//! How long the engine takes over made-up SMO logs (docs/plans/performance.md, "CAN logs through the engine" and
//! "A full rebuild"): every log decoded as it would arrive, a car rebuilt whole, and the engine started again on a
//! folder where everything is done.
//!
//! `PERF_DAYS=30 cargo test --release -p sentry-mate measure_engine -- --ignored --nocapture` (`PERF_REALISTIC=1`:
//! frames at the car's rates instead of once a second; `PERF_ENGINE_DATA=DIR` keeps the seeded folder).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::print_stdout)]

use super::*;
use std::time::Instant;

/// This process's memory now and at its highest (MB), from `/proc/self/status`.
fn memory_mb() -> (f64, f64) {
    let text = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let kb = |key: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(key)?.split_whitespace().next()?.parse::<f64>().ok())
            .map_or(0.0, |kb| kb / 1024.0)
    };
    (kb("VmRSS:"), kb("VmHWM:"))
}

/// CPU seconds this process used so far (user and system).
fn cpu_s() -> f64 {
    let stat = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
    let after = stat.rsplit_once(')').map_or("", |x| x.1);
    let f: Vec<&str> = after.split_whitespace().collect();
    let ticks = |i: usize| f.get(i).and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0);
    (ticks(11) + ticks(12)) / 100.0
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "measurement by hand: cargo test --release -p sentry-mate measure_engine -- --ignored --nocapture"]
async fn measure_engine() {
    let days: u32 = std::env::var("PERF_DAYS").ok().and_then(|d| d.parse().ok()).unwrap_or(30);
    let light = std::env::var_os("PERF_REALISTIC").is_none();
    let tmp = tempfile::tempdir().unwrap();
    let base = std::env::var_os("PERF_ENGINE_DATA").map_or_else(|| tmp.path().to_path_buf(), PathBuf::from);
    let (data, keys) = (base.join("data"), base.join("keys"));
    let v = if data.exists() {
        Arc::new(Vault::open(&data, &keys).unwrap())
    } else {
        std::fs::create_dir_all(&data).unwrap();
        let (v, _) = Vault::create(&data, &keys, Kdf::FAST_FOR_TESTS).unwrap();
        let v = Arc::new(v);
        let store = Store::new(can_dir(&data), v.clone());
        let judge = Judge::new(store.clone());
        let tokens = Tokens::open(tokens_file(&data), v.clone()).await.unwrap();
        let o = seed::Options {
            days,
            cars: 1,
            seed: 7,
            light,
            clean: false,
            candump: false,
            with_real_cars: false,
            start_ms: None,
            awake_min: None,
        };
        let gate = gate::Gate::memory();
        let enter = || -> Result<gate::Pass, Box<dyn std::error::Error>> { Ok(gate.enter(GATE_WAIT)?) };
        let t = Instant::now();
        seed::run(&store, &judge, &tokens, &o, &enter, &mut Vec::new()).await.unwrap();
        println!("engine: {days} days of logs seeded in {:.1} s", t.elapsed().as_secs_f64());
        v
    };
    let open = |v: Arc<Vault>| {
        let db = db::Db::open(&data, v.clone(), db::Options::default()).unwrap();
        engine::Engine::new(db, Store::new(can_dir(&data), v), engine::Options::default())
    };
    let line = |what: &str, t: Instant, cpu: f64, before: &engine::EngineStats, e: &engine::Engine| {
        let s = e.stats();
        let (rss, peak) = memory_mb();
        let logs = (s.logs_decoded + s.logs_left_out) - (before.logs_decoded + before.logs_left_out);
        let secs = t.elapsed().as_secs_f64();
        println!(
            "engine: {what}: {secs:.2} s, {:.2} s of CPU, {logs} logs ({:.2} ms a log; reading them {} ms, their \
             windows {} ms of it), {rss:.0} MB now, {peak:.0} MB at most",
            cpu_s() - cpu,
            if logs > 0 { secs * 1000.0 / logs as f64 } else { 0.0 },
            s.decode_ms_total - before.decode_ms_total,
            s.rebuild_ms_total - before.rebuild_ms_total,
        );
    };

    // every log, as on the first start over a full store
    let (t, cpu) = (Instant::now(), cpu_s());
    let e = open(v.clone());
    let zero = e.stats();
    e.start().unwrap();
    e.idle().await;
    line("the logs found at the start", t, cpu, &zero, &e);
    let car = e.db().fleet(&db::FleetQuery { search: None, limit: 10, after: None }).unwrap().cars[0].car.id;

    // the car rebuilt whole
    let (t, cpu, before) = (Instant::now(), cpu_s(), e.stats());
    e.rebuild(engine::Scope::car(car)).unwrap();
    e.idle().await;
    line("the car rebuilt whole", t, cpu, &before, &e);
    drop(e);

    // started again: nothing to do
    let (t, cpu) = (Instant::now(), cpu_s());
    let e = open(v);
    let zero = e.stats();
    e.start().unwrap();
    e.idle().await;
    line("started again with nothing to do", t, cpu, &zero, &e);
}
