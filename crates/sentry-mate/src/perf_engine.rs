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
                tesla_sentry: false,
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

/// 2026-11-01T00:00:00Z: where the owner-shaped logs start ([`measure_owner_rebuild`]).
const OWNER_LOGS_START_MS: u64 = 1_793_491_200_000;
/// 2026-02-01T00:00:00Z: where its imported TeslaMate history starts.
const OWNER_IMPORT_START_MS: u64 = 1_769_904_000_000;

/// The owner's shape of history, made up and small on disk: `logs` tiny logs (3 s of frames every 69 s of a car in
/// sentry mode: about 1,250 a day), the Tesla API over the same days (a `vehicle_data` answer every 15 to 30 s,
/// every second while driving), and a TeslaMate import from February 2026 through the same days. Returns the data
/// folder's base (`data`, `keys`) and the car's id.
async fn owner_history(base: &Path, logs: usize) -> model::CarId {
    let (data, keys) = (base.join("data"), base.join("keys"));
    std::fs::create_dir_all(&data).unwrap();
    let (v, _) = Vault::create(&data, &keys, Kdf::FAST_FOR_TESTS).unwrap();
    let v = Arc::new(v);
    let days = u32::try_from(logs.div_ceil(1200)).unwrap();
    let t = Instant::now();
    {
        let store = Store::new(can_dir(&data), v.clone());
        let judge = Judge::new(store.clone());
        let gate = gate::Gate::memory();
        let enter = || -> Result<gate::Pass, Box<dyn std::error::Error>> { Ok(gate.enter(GATE_WAIT)?) };
        let s = synth::Scenario::new(7)
            .start(OWNER_LOGS_START_MS)
            .days(days)
            .rates(synth::Rates::Light)
            .stays_awake(true)
            .snippets(69_000, 3_000);
        let mut n = 0;
        for log in s.logs().take(logs) {
            seed::put(&store, &judge, &enter, &log.name, &log.bytes).await.unwrap();
            seed::put(&store, &judge, &enter, &log.sidecar_name(), &log.sidecar_bytes()).await.unwrap();
            n += 1;
        }
        assert_eq!(n, logs, "{days} days give fewer logs");
    }
    println!("owner shape, {logs} logs: {days} days of tiny logs made in {:.1} s", t.elapsed().as_secs_f64());
    let db = db::Db::open(&data, v.clone(), db::Options::default()).unwrap();
    let e = engine::Engine::new(db, Store::new(can_dir(&data), v.clone()), engine::Options::default());
    let t = Instant::now();
    e.start().unwrap();
    e.idle().await;
    println!("owner shape, {logs} logs: decoded at the start in {:.1} s", t.elapsed().as_secs_f64());
    let end_ms = OWNER_LOGS_START_MS + u64::from(days) * 86_400_000;
    let import_days = u32::try_from((end_ms - OWNER_IMPORT_START_MS) / 86_400_000).unwrap();
    let o = history::Options {
        days: import_days,
        end_ms,
        seed: 5,
        drives: 4 * import_days,
        charges: import_days,
        drive_step_s: 1,
        teslafi_days: 0,
        api_days: 0,
        tesla_days: days,
        tesla_sentry: true,
    };
    let t = Instant::now();
    let c = history::run(&e, &o).await.unwrap();
    e.idle().await;
    println!(
        "owner shape, {logs} logs: {import_days} days of TeslaMate import ({} positions, {} drives) and {days} days of \
         the Tesla API ({} samples) put in in {:.1} s",
        c.positions,
        c.drives,
        c.tesla_samples,
        t.elapsed().as_secs_f64()
    );
    let car = e.db().fleet(&db::FleetQuery { search: None, limit: 10, after: None }).unwrap().cars[0].car.id;
    assert_eq!(e.stats().failures, 0);
    car
}

/// The logs a second and ms a log at the start, middle and end of a day (its first, middle and last third of
/// logs: the owner's shape has a log every 69 s from midnight), from the logs done as time went (`marks`).
fn within_day(marks: &[(Instant, u64)]) -> String {
    const PER_DAY: f64 = 86_400.0 / 69.0;
    let mut parts = [(0.0f64, 0u64); 3];
    for w in marks.windows(2) {
        let ((t0, n0), (t1, n1)) = (w[0], w[1]);
        if n1 <= n0 {
            continue;
        }
        let (d0, d1) = ((n0 as f64 / PER_DAY).floor(), (n1 as f64 / PER_DAY).floor());
        if d0 != d1 {
            continue;
        }
        let at = (n0 as f64 + n1 as f64) / 2.0 / PER_DAY - d0;
        let i = ((at * 3.0) as usize).min(2);
        parts[i].0 += (t1 - t0).as_secs_f64();
        parts[i].1 += n1 - n0;
    }
    let words: Vec<String> = ["start", "middle", "end"]
        .iter()
        .zip(parts)
        .map(|(name, (s, n))| {
            let n = n.max(1) as f64;
            format!("{name} {:.1} logs/s ({:.1} ms a log)", n / s.max(1e-9), s * 1000.0 / n)
        })
        .collect();
    format!("within a day: {}", words.join(", "))
}

/// A full rebuild's time per log as the history grows, in the owner's shape ([`owner_history`]; docs/plans/
/// performance.md, "A full rebuild in the owner's shape"): ms a log, and of it the stages (the full rebuild's own
/// counters, [`engine::Engine::work_stats`]).
///
/// `PERF_LOGS=1000,5000,10000,20000 cargo test --release -p sentry-mate measure_owner_rebuild -- --ignored
/// --nocapture` (the sizes; 1,000 and 5,000 when not given); `PERF_OWNER_DATA=DIR` keeps each size's seeded folder
/// there and starts from a copy of it the next time; `PERF_FULL_BY_DAY=0` rebuilds the windows as before the full
/// rebuild built them once per day of logs ([`engine::Options::full_by_day`]); `PERF_LIVE=1` feeds the server's
/// engine a Tesla API answer a second while the full rebuild runs, as a live input keeps arriving on a server.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "measurement by hand: cargo test --release -p sentry-mate measure_owner_rebuild -- --ignored --nocapture"]
async fn measure_owner_rebuild() {
    let sizes: Vec<usize> = std::env::var("PERF_LOGS")
        .ok()
        .map(|v| v.split(',').filter_map(|x| x.trim().parse().ok()).collect())
        .unwrap_or_else(|| vec![1000, 5000]);
    let mut table = Vec::new();
    for logs in sizes {
        let tmp = tempfile::tempdir().unwrap();
        // (a seeded history kept for the next run: PERF_OWNER_DATA, copied before each rebuild)
        match std::env::var_os("PERF_OWNER_DATA").map(|d| PathBuf::from(d).join(logs.to_string())) {
            Some(kept) => {
                if !kept.join("data").exists() {
                    // (seeded beside it and moved in place whole: a run cut off leaves nothing to reuse)
                    let part = kept.with_extension("part");
                    let _ = std::fs::remove_dir_all(&part);
                    owner_history(&part, logs).await;
                    std::fs::rename(&part, &kept).unwrap();
                }
                copy_dir(&kept, tmp.path());
            }
            None => {
                owner_history(tmp.path(), logs).await;
            }
        }
        let (data, keys) = (tmp.path().join("data"), tmp.path().join("keys"));
        let v = Arc::new(Vault::open(&data, &keys).unwrap());
        let db = db::Db::open(&data, v.clone(), db::Options::default()).unwrap();
        // (`PERF_FULL_BY_DAY=0`: the windows as before, a window per log beside the API)
        let by_day = std::env::var("PERF_FULL_BY_DAY").map_or(true, |v| v != "0");
        // (`PERF_HOLD=0`: the records each log changes written for every log, as before they were held a day)
        let hold = std::env::var("PERF_HOLD").map_or(true, |v| v != "0");
        let opts = engine::Options { full_by_day: by_day, full_hold_records: hold, ..engine::Options::default() };
        let e = engine::Engine::new(db, Store::new(can_dir(&data), v), opts);
        e.start().unwrap();
        e.idle().await;
        let car = e.db().fleet(&db::FleetQuery { search: None, limit: 10, after: None }).unwrap().cars[0].car.id;
        let (t, cpu, before) = (Instant::now(), cpu_s(), e.work_stats());
        e.full_rebuild(Some(car), engine::full::Reason::Owner, None).unwrap();
        // (`PERF_LIVE=1`: a live input meanwhile, an answer a second after the history's end)
        let live = std::env::var_os("PERF_LIVE").is_some().then(|| {
            let e = e.clone();
            tokio::spawn(async move {
                let days = u64::try_from(logs.div_ceil(1200)).unwrap();
                let mut t = OWNER_LOGS_START_MS + days * 86_400_000;
                loop {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    let s =
                        model::Sample { t_ms: t, soc: Some(60.0), odometer_km: Some(12_345.0), ..Default::default() };
                    let span = timeline::ApiSpan {
                        start_ms: t,
                        end_ms: t + 1000,
                        state: timeline::ApiState::Online,
                        data: true,
                        inputs: model::Inputs::NONE,
                    };
                    e.feed_points(model::Input::TeslaApi, car, vec![s], vec![span]);
                    t += 1000;
                }
            })
        });
        let mut last = before.clone();
        let mut decoding = (None, None);
        // (the logs done as time goes, for the speed at the start, middle and end of a day)
        let mut marks: Vec<(Instant, u64)> = Vec::new();
        let r = loop {
            let r = e.full_rebuild_status().unwrap().unwrap();
            if r.phase == engine::full::Phase::Decoding {
                // (the shadow's counters go with it at the phase's end, before the record says so)
                let w = e.work_stats();
                if w.decode_ms_total >= last.decode_ms_total {
                    last = w;
                }
                if std::env::var_os("PERF_DEBUG").is_some() {
                    println!("{:?} {} {:?}", r.phase, r.logs_done, last);
                }
                decoding.0.get_or_insert_with(Instant::now);
                decoding.1 = Some(Instant::now());
                marks.push((Instant::now(), r.logs_done));
            }
            if !r.running() {
                break r;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        };
        if let Some(l) = live {
            l.abort();
        }
        e.idle().await;
        let secs = t.elapsed().as_secs_f64();
        let n = r.logs_done.max(1) as f64;
        let per = |a: u64, b: u64| (a - b) as f64 / n;
        let s = &last;
        let decode_ms = per(s.decode_ms_total, before.decode_ms_total);
        let read = per(s.read_us_total, before.read_us_total) / 1000.0;
        let samples = per(s.samples_us_total, before.samples_us_total) / 1000.0;
        let now = per(s.now_us_total, before.now_us_total) / 1000.0;
        let windows = per(s.rebuild_ms_total, before.rebuild_ms_total);
        let dec_s = match decoding {
            (Some(a), Some(b)) => (b - a).as_secs_f64(),
            _ => 0.0,
        };
        let row = format!(
            "| {logs} | {secs:.0} s | {:.1} | {:.1} | {read:.1} | {samples:.1} | {now:.1} | {:.1} | {windows:.1} | {} |",
            secs * 1000.0 / n,
            dec_s * 1000.0 / n,
            decode_ms - read - samples - now,
            (s.windows_rebuilt - before.windows_rebuilt),
        );
        let parts: Vec<String> = [
            "find",
            "read",
            "fuse",
            "build",
            "rows",
            "rest",
            "(of find: entries",
            "nearest)",
            "(of rows: costs",
            "replaced",
            "cuts)",
            "(of rest: state now)",
        ]
        .iter()
        .zip(s.window_parts_us.iter().zip(before.window_parts_us))
        .map(|(name, (a, b))| format!("{name} {:.1}", per(*a, b) / 1000.0))
        .collect();
        println!("owner shape: {logs} logs: of the windows (ms a log): {}; {:.0} s decoding", parts.join(", "), dec_s);
        println!(
            "owner shape: {logs} logs: a full rebuild in {secs:.1} s, {:.1} s of CPU, {:?}, {} logs\n{row}",
            cpu_s() - cpu,
            r.status,
            r.logs_done
        );
        println!("owner shape: {logs} logs: {}", within_day(&marks));
        table.push(row);
        assert_eq!(r.status, engine::full::FullStatus::Done);
    }
    println!(
        "| logs | full rebuild | ms a log | of it decoding (ms a log) | read | samples | now | other of the log | \
         windows | windows rebuilt |"
    );
    for row in table {
        println!("{row}");
    }
}
