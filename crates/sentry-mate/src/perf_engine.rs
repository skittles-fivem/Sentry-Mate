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

/// Copies a data folder (the made-up history seeded once, then worked on by each kind of engine).
fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let (src, dst) = (e.path(), to.join(e.file_name()));
        if e.file_type().unwrap().is_dir() {
            copy_dir(&src, &dst);
        } else {
            std::fs::copy(&src, &dst).unwrap();
        }
    }
}

/// A full rebuild of one car and the car rebuilt whole, with the logs read ahead on the other cores and without
/// (docs/plans/performance.md, "A full rebuild on every core"): where a log's time goes, and the rate.
///
/// `PERF_DAYS=10 cargo test --release -p sentry-mate measure_full_rebuild -- --ignored --nocapture`; frames at the
/// car's real rates unless `PERF_LIGHT=1`; `PERF_API=1` adds TeslaFi's API and local TeslaMate over the same days (a
/// car with an API input gets a window per log); `PERF_HISTORY=big` instead the owner's size of imported history
/// before them (`history::Options::big`: 330 days, 3 million positions, the APIs' last 30 days);
/// `PERF_READ_AHEAD=0,3` the threads tried.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "measurement by hand: cargo test --release -p sentry-mate measure_full_rebuild -- --ignored --nocapture"]
async fn measure_full_rebuild() {
    let days: u32 = std::env::var("PERF_DAYS").ok().and_then(|d| d.parse().ok()).unwrap_or(10);
    let light = std::env::var_os("PERF_LIGHT").is_some();
    let api = std::env::var_os("PERF_API").is_some();
    let big = std::env::var_os("PERF_HISTORY").is_some_and(|v| v == "big");
    let tries: Vec<usize> = std::env::var("PERF_READ_AHEAD")
        .ok()
        .map(|v| v.split(',').filter_map(|x| x.trim().parse().ok()).collect())
        .unwrap_or_else(|| vec![0, engine::Options::default().read_ahead]);
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().join("seeded");
    let (data, keys) = (base.join("data"), base.join("keys"));
    std::fs::create_dir_all(&data).unwrap();
    let (v, _) = Vault::create(&data, &keys, Kdf::FAST_FOR_TESTS).unwrap();
    let v = Arc::new(v);
    {
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
        let mut said = Vec::new();
        seed::run(&store, &judge, &tokens, &o, &enter, &mut said).await.unwrap();
        print!("{}", String::from_utf8_lossy(&said));
        println!("full rebuild: {days} days of logs seeded in {:.1} s", t.elapsed().as_secs_f64());
    }
    if api || big {
        let db = db::Db::open(&data, v.clone(), db::Options::default()).unwrap();
        let e = engine::Engine::new(db, Store::new(can_dir(&data), v.clone()), engine::Options::default());
        e.start().unwrap();
        e.idle().await;
        let end_ms = synth::DEFAULT_START_MS + u64::from(days) * 86_400_000;
        let o = if big {
            history::Options::big(end_ms)
        } else {
            history::Options {
                days,
                end_ms,
                seed: 5,
                drives: 6 * days,
                charges: 2 * days,
                drive_step_s: 5,
                teslafi_days: 0,
                api_days: days,
                tesla_days: 0,
            }
        };
        let t = Instant::now();
        let c = history::run(&e, &o).await.unwrap();
        e.idle().await;
        println!(
            "full rebuild: {} days of imports and APIs put in ({} positions, {} drives; TeslaFi's API and local \
             TeslaMate over the last {} days) in {:.1} s",
            o.days,
            c.positions,
            c.drives,
            o.api_days,
            t.elapsed().as_secs_f64()
        );
    }
    drop(v);
    for (k, &read_ahead) in tries.iter().enumerate() {
        let side = tmp.path().join(format!("side{k}"));
        copy_dir(&base, &side);
        let (data, keys) = (side.join("data"), side.join("keys"));
        let v = Arc::new(Vault::open(&data, &keys).unwrap());
        let db = db::Db::open(&data, v.clone(), db::Options::default()).unwrap();
        let opts = engine::Options { read_ahead, ..engine::Options::default() };
        let e = engine::Engine::new(db, Store::new(can_dir(&data), v), opts);
        let line = |what: &str, t: Instant, cpu: f64, logs: u64, b: &engine::EngineStats, e: &engine::Engine| {
            let s = e.stats();
            let secs = t.elapsed().as_secs_f64();
            let per = |x: u64| if logs > 0 { x as f64 / logs as f64 } else { 0.0 };
            println!(
                "read ahead {read_ahead}: {what}: {secs:.1} s, {:.1} s of CPU, {logs} logs, {:.2} logs/s, {:.1} ms a \
                 log; of a log (ms): all but its window {:.1} (read and decoded {:.1}, samples written {:.1}, values \
                 now {:.1}), windows {:.1}",
                cpu_s() - cpu,
                logs as f64 / secs,
                secs * 1000.0 / logs.max(1) as f64,
                per(s.decode_ms_total - b.decode_ms_total),
                per(s.read_us_total - b.read_us_total) / 1000.0,
                per(s.samples_us_total - b.samples_us_total) / 1000.0,
                per(s.now_us_total - b.now_us_total) / 1000.0,
                per(s.rebuild_ms_total - b.rebuild_ms_total),
            );
        };
        let (t, cpu, zero) = (Instant::now(), cpu_s(), e.stats());
        e.start().unwrap();
        e.idle().await;
        let s = e.stats();
        line("the logs found at the start", t, cpu, s.logs_decoded + s.logs_left_out, &zero, &e);
        let car = e.db().fleet(&db::FleetQuery { search: None, limit: 10, after: None }).unwrap().cars[0].car.id;

        // the car rebuilt whole (the same decoding as a full rebuild's, on the engine whose counters are read)
        let (t, cpu, before) = (Instant::now(), cpu_s(), e.stats());
        e.rebuild(engine::Scope::car(car)).unwrap();
        e.idle().await;
        let s = e.stats();
        let logs = s.logs_decoded + s.logs_left_out - before.logs_decoded - before.logs_left_out;
        line("the car rebuilt whole", t, cpu, logs, &before, &e);

        // a full rebuild, phase by phase
        let (t, cpu) = (Instant::now(), cpu_s());
        // (when, for a trace of the process: `strace -f -ttt -e trace=fsync,fdatasync` counts its syncs)
        let unix_ms = || std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis();
        let from_ms = unix_ms();
        e.full_rebuild(Some(car), engine::full::Reason::Owner, None).unwrap();
        let mut phases: Vec<(engine::full::Phase, Instant)> = Vec::new();
        let r = loop {
            let r = e.full_rebuild_status().unwrap().unwrap();
            if phases.last().is_none_or(|p| p.0 != r.phase) {
                phases.push((r.phase, Instant::now()));
            }
            if !r.running() {
                break r;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        e.idle().await;
        let secs = t.elapsed().as_secs_f64();
        let mut words = Vec::new();
        for w in phases.windows(2) {
            words.push(format!("{:?} {:.1} s", w[0].0, (w[1].1 - w[0].1).as_secs_f64()));
        }
        println!(
            "read ahead {read_ahead}: a full rebuild: {secs:.1} s, {:.1} s of CPU, {} logs, {:.2} logs/s ({}), {:?} \
             (unix ms {from_ms} to {})",
            cpu_s() - cpu,
            r.logs_done,
            r.logs_done as f64 / secs,
            words.join(", "),
            r.status,
            unix_ms(),
        );
        drop(e);
    }
}
