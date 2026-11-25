//! How fast the pages' API calls answer on a long history (docs/plans/performance.md): a data folder with made-up
//! SMO logs (`seed`) and a long made-up history (`history`: a TeslaMate import, a TeslaFi import, TeslaFi's API and
//! local TeslaMate), `serve` started in this process, and every page's calls timed: cold (the first call after the
//! server started) and warm (the median of the next three).
//!
//! - `perf_pages_stay_within_their_budget` (ignored, run weekly in release by the Full tests workflow): a smaller
//!   history, the time budget asserted.
//! - `measure_pages` (ignored, by hand, `--release`): the owner's size (about 3 million positions over a year);
//!   prints the table. `PERF_DATA=DIR` keeps the data folder there and uses it again on the next run.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::print_stdout)]

use super::e2e::{Client, free_port, listening, setup_code};
use super::*;
use serde_json::json;
use std::time::Instant;

const DAY: u64 = 86_400_000;

/// The history's end: a UTC midnight a few days before now (so "the last 30 days" are mostly inside it).
fn end_ms() -> u64 {
    let now = ingest::store::now_ms();
    (now / DAY - 2) * DAY
}

struct Size {
    smo_days: u32,
    history: history::Options,
}

/// Makes the data folder: made-up SMO logs for the history's last days, then the long history over them.
async fn make(data: &Path, keys: &Path, size: &Size) {
    std::fs::create_dir_all(data).unwrap();
    let (v, _) = Vault::create(data, keys, Kdf::FAST_FOR_TESTS).unwrap();
    let v = Arc::new(v);
    let t = Instant::now();
    {
        let store = Store::new(can_dir(data), v.clone());
        let judge = Judge::new(store.clone());
        let tokens = Tokens::open(tokens_file(data), v.clone()).await.unwrap();
        let o = seed::Options {
            days: size.smo_days,
            cars: 1,
            seed: 7,
            light: true,
            clean: false,
            candump: false,
            with_real_cars: false,
            start_ms: Some(size.history.end_ms - u64::from(size.smo_days) * DAY),
        };
        let gate = gate::Gate::memory();
        let enter = || -> Result<gate::Pass, Box<dyn std::error::Error>> { Ok(gate.enter(GATE_WAIT)?) };
        seed::run(&store, &judge, &tokens, &o, &enter, &mut Vec::new()).await.unwrap();
    }
    println!("perf: {} days of SMO logs in {:.1} s", size.smo_days, t.elapsed().as_secs_f64());
    let t = Instant::now();
    let db = db::Db::open(data, v.clone(), db::Options::default()).unwrap();
    let engine = engine::Engine::new(db, Store::new(can_dir(data), v), engine::Options::default());
    engine.start().unwrap();
    let c = history::run(&engine, &size.history).await.unwrap();
    println!("perf: history {c:?} in {:.1} s", t.elapsed().as_secs_f64());
}

/// A server on the data folder, signed in as its admin.
struct Server {
    task: tokio::task::JoinHandle<Result<(), String>>,
    c: Client,
}

async fn start(data: &Path, keys: &Path, first: bool) -> Server {
    let (upload, web_port) = (free_port(), free_port());
    let (d, k) = (data.to_path_buf(), keys.to_path_buf());
    let task = tokio::spawn(async move {
        let net = Net { bind: "127.0.0.1".into(), upload, web: web_port };
        serve(&d, &k, &net, &[], &MoveEnv::default(), accounts::Hashing::FAST_FOR_TESTS)
            .await
            .map_err(|e| e.to_string())
    });
    listening(web_port, &task).await;
    let v = Vault::open(data, keys).unwrap();
    let id = Identity::load_or_create(&tls_dir(data), &[], &v).unwrap();
    let mut roots = rustls::RootCertStore::empty();
    for c in &id.chain {
        roots.add(c.clone()).unwrap();
    }
    let tls = Arc::new(
        rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_protocol_versions(&[&rustls::version::TLS13])
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth(),
    );
    let mut c = Client { port: web_port, tls, cookie: None };
    let account = json!({ "name": "owner", "password": "correct horse battery staple" });
    if first {
        let code = setup_code(keys).await;
        let mut body = account.clone();
        body["code"] = json!(code);
        let r = c.request("POST", "/api/v1/setup", Some(body)).await;
        assert_eq!(r.status, 200, "setup: {}", r.text());
    } else {
        let r = c.request("POST", "/api/v1/session", Some(account)).await;
        assert_eq!(r.status, 200, "sign in: {}", r.text());
    }
    Server { task, c }
}

/// The made-up car, once the engine has nothing left to do for it.
async fn settled_car(c: &mut Client) -> String {
    for _ in 0..3000 {
        let r = c.get("/api/v1/cars").await;
        if let Some(car) = r.json()["items"][0]["id"].as_str() {
            let h = c.get(&format!("/api/v1/cars/{car}/health")).await.json();
            if h["logs_waiting"].as_u64() == Some(0) && h["rebuild_pending"] == json!(false) {
                return car.to_string();
            }
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    panic!("the engine never settled");
}

/// Every page's calls, as the packs make them (`{c}` the car, `{y}` a year before the end, `{m}` 30 days, `{d}` a
/// day, `{to}` the end).
const PAGES: &[(&str, &[&str])] = &[
    (
        "car",
        &[
            "/api/v1/cars/{c}",
            "/api/v1/cars/{c}/now",
            "/api/v1/cars/{c}/activity?from_ms={d}&to_ms={to}",
            "/api/v1/cars/{c}/series?fields=soc_pct,range_km,speed_kph,power_kw,outside_c&from_ms={d}&to_ms={to}&max_points=600",
            "/api/v1/cars/{c}/timeline?limit=20",
            "/api/v1/cars/{c}/health",
            "/api/v1/cars/{c}/totals",
            "/api/v1/cars/{c}/statistics?period=month&limit=12",
        ],
    ),
    (
        "timeline",
        &[
            "/api/v1/cars/{c}/timeline?from_ms={m}&to_ms={to}&limit=50",
            "/api/v1/cars/{c}/states?from_ms={m}&to_ms={to}",
            "/api/v1/cars/{c}/activity?from_ms={m}&to_ms={to}",
        ],
    ),
    (
        "drives",
        &[
            "/api/v1/cars/{c}/drives?limit=50",
            "/api/v1/cars/{c}/drives?from_ms={y}&to_ms={to}&limit=50",
            "/api/v1/cars/{c}/drive-stats?from_ms={y}&to_ms={to}",
            "/api/v1/cars/{c}/gaps?limit=50",
        ],
    ),
    (
        "charges",
        &[
            "/api/v1/cars/{c}/charges?limit=50",
            "/api/v1/cars/{c}/charging-stats?from_ms={y}&to_ms={to}",
            "/api/v1/charging-stats/places?cars={c}",
            "/api/v1/cars/{c}/charging-stats/dc-curve?from_ms={y}&to_ms={to}",
        ],
    ),
    (
        "battery",
        &[
            "/api/v1/cars/{c}/battery",
            "/api/v1/cars/{c}/battery/capacity?group=day&x=odometer&limit=500",
            "/api/v1/cars/{c}/battery/capacity?group=month&x=odometer&limit=500",
            "/api/v1/cars/{c}/battery-level?from_ms={y}&to_ms={to}&bucket=day",
            "/api/v1/cars/{c}/projected-range?from_ms={y}&to_ms={to}",
            "/api/v1/cars/{c}/parked?limit=12",
            "/api/v1/cars/{c}/settings",
            "/api/v1/cars/{c}/efficiency",
        ],
    ),
    (
        "efficiency",
        &["/api/v1/cars/{c}/efficiency?from_ms={y}&to_ms={to}", "/api/v1/cars/{c}/temperatures?from_ms={y}&to_ms={to}"],
    ),
    (
        "statistics",
        &[
            "/api/v1/cars/{c}/statistics?period=month",
            "/api/v1/cars/{c}/statistics?period=year",
            "/api/v1/cars/{c}/statistics?period=all",
            "/api/v1/cars/{c}/mileage?group=month",
        ],
    ),
    (
        "travel",
        &[
            "/api/v1/visits?cars={c}&limit=50",
            "/api/v1/visits/days?cars={c}",
            "/api/v1/tracks?cars={c}&from_ms={m}&to_ms={to}&max_points=2000",
        ],
    ),
    ("software", &["/api/v1/cars/{c}/updates", "/api/v1/cars/{c}/firmware", "/api/v1/cars/{c}/firmware/history"]),
    ("places", &["/api/v1/places", "/api/v1/places/suggestions"]),
    ("health", &["/api/v1/cars/{c}/health", "/api/v1/cars/{c}/storage", "/api/v1/cars/{c}/gaps"]),
    (
        "series",
        &[
            "/api/v1/cars/{c}/series?fields=soc_pct,range_km,speed_kph,power_kw,outside_c&from_ms={d}&to_ms={to}&max_points=1000",
            "/api/v1/cars/{c}/series?fields=soc_pct,range_km,speed_kph,power_kw,outside_c&from_ms={m}&to_ms={to}&max_points=1000",
            "/api/v1/cars/{c}/series?fields=soc_pct,range_km,speed_kph,power_kw,outside_c&from_ms={y}&to_ms={to}&max_points=1000",
            "/api/v1/cars/{c}/series?fields=soc_pct,range_km,speed_kph,power_kw,outside_c&from_ms=0&to_ms={to}&max_points=1000",
        ],
    ),
];

fn fill(path: &str, car: &str, end: u64) -> String {
    path.replace("{c}", car)
        .replace("{y}", &(end - 365 * DAY).to_string())
        .replace("{m}", &(end - 30 * DAY).to_string())
        .replace("{d}", &(end - DAY).to_string())
        .replace("{to}", &end.to_string())
}

/// One call's time (ms); a call that fails fails the test.
async fn time(c: &mut Client, path: &str) -> f64 {
    let t = Instant::now();
    let r = c.get(path).await;
    let ms = t.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(r.status, 200, "{path}: {}", r.text());
    ms
}

#[derive(Default)]
struct Row {
    page: &'static str,
    path: String,
    cold: f64,
    warm: f64,
}

/// Every page's calls once (cold), then three more times (warm: the median); and each page's calls at once, as a
/// page makes them, warm (its slowest call is the page's time).
async fn measure(c: &mut Client, car: &str, end: u64) -> (Vec<Row>, Vec<(&'static str, f64)>) {
    let mut rows = Vec::new();
    for (page, paths) in PAGES {
        for p in *paths {
            let path = fill(p, car, end);
            let cold = time(c, &path).await;
            rows.push(Row { page, path, cold, warm: 0.0 });
        }
    }
    for r in &mut rows {
        let mut w = [time(c, &r.path).await, time(c, &r.path).await, time(c, &r.path).await];
        w.sort_by(f64::total_cmp);
        r.warm = w[1];
    }
    let mut pages = Vec::new();
    for (page, paths) in PAGES {
        let t = Instant::now();
        // as the packs' client sends them: at most two of the answers that decode stored logs at once
        let turns = Arc::new(tokio::sync::Semaphore::new(2));
        let calls = paths.iter().map(|p| {
            let path = fill(p, car, end);
            let mut c2 = Client { port: c.port, tls: c.tls.clone(), cookie: c.cookie.clone() };
            let turns = turns.clone();
            async move {
                let _turn = match heavy(&path) {
                    true => Some(turns.acquire_owned().await.unwrap()),
                    false => None,
                };
                let r = c2.get(&path).await;
                assert_eq!(r.status, 200, "{path}: {}", r.text());
            }
        });
        futures_join(calls).await;
        pages.push((*page, t.elapsed().as_secs_f64() * 1000.0));
    }
    (rows, pages)
}

/// The calls the packs' client sends two at a time (`HEAVY_PATH` in ui/shared/api.js).
fn heavy(path: &str) -> bool {
    let p = path.split('?').next().unwrap_or(path);
    let Some(rest) = p.strip_prefix("/api/v1/cars/").and_then(|r| r.split_once('/')).map(|x| x.1) else { return false };
    matches!(
        rest,
        "signals"
            | "signals/latest"
            | "battery"
            | "battery/capacity"
            | "battery-level"
            | "projected-range"
            | "efficiency"
    )
}

async fn futures_join<F: std::future::Future<Output = ()> + Send + 'static>(fs: impl Iterator<Item = F>) {
    let hs: Vec<_> = fs.map(tokio::spawn).collect();
    for h in hs {
        h.await.unwrap();
    }
}

fn table(rows: &[Row], restart: &[f64], pages: &[(&str, f64)]) -> String {
    let mut s = String::from(
        "| Page | Call | Cold, first start (ms) | Cold, after a restart (ms) | Warm (ms) |\n|---|---|---:|---:|---:|\n",
    );
    for (r, again) in rows.iter().zip(restart) {
        let path = r.path.split_once("/api/v1/").map_or(r.path.as_str(), |x| x.1);
        let path = path.split("from_ms=").next().unwrap_or(path).trim_end_matches(['&', '?']);
        s.push_str(&format!("| {} | `{}` | {:.0} | {:.0} | {:.0} |\n", r.page, path, r.cold, again, r.warm));
    }
    s.push_str("\n| Page | All its calls at once, warm (ms) |\n|---|---:|\n");
    for (p, ms) in pages {
        s.push_str(&format!("| {p} | {ms:.0} |\n"));
    }
    s
}

/// Times every page on the data folder: a first start (nothing kept from before), then a restart.
async fn run(data: &Path, keys: &Path, fresh: bool) -> (Vec<Row>, Vec<f64>, Vec<(&'static str, f64)>) {
    let end = end_ms();
    let mut s = start(data, keys, fresh).await;
    let car = settled_car(&mut s.c).await;
    let (rows, pages) = measure(&mut s.c, &car, end).await;
    s.task.abort();
    let _ = s.task.await;
    let mut s = start(data, keys, false).await;
    let _ = settled_car(&mut s.c).await;
    let mut restart = Vec::new();
    for r in &rows {
        restart.push(time(&mut s.c, &r.path).await);
    }
    s.task.abort();
    let _ = s.task.await;
    (rows, restart, pages)
}

/// Warm answers within this (ms), on 4 cores in a release build.
const WARM_MS: f64 = 300.0;
/// Cold answers within this (ms).
const COLD_MS: f64 = 2000.0;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "perf: weekly in release (cargo test --release -p sentry-mate perf_ -- --ignored)"]
async fn perf_pages_stay_within_their_budget() {
    // `PERF_SMOKE_DATA=DIR` keeps the data folder there and uses it again (while working on the pages)
    let dir = tempfile::tempdir().unwrap();
    let base = std::env::var_os("PERF_SMOKE_DATA").map_or_else(|| dir.path().to_path_buf(), PathBuf::from);
    let (data, keys) = (base.join("data"), base.join("keys"));
    let fresh = !data.exists();
    let end = end_ms();
    let size = Size {
        smo_days: 4,
        history: history::Options {
            days: 120,
            end_ms: end,
            seed: 3,
            drives: 450,
            charges: 170,
            drive_step_s: 2,
            teslafi_days: 30,
            api_days: 10,
            tesla_days: 0,
        },
    };
    if fresh {
        make(&data, &keys, &size).await;
    }
    let (rows, restart, pages) = run(&data, &keys, fresh).await;
    println!("{}", table(&rows, &restart, &pages));
    if cfg!(debug_assertions) {
        println!("perf: a debug build; the budget is for a release build and is not checked");
        return;
    }
    let mut over = Vec::new();
    for (r, again) in rows.iter().zip(&restart) {
        if r.warm > WARM_MS || r.cold > COLD_MS || *again > COLD_MS {
            over.push(format!(
                "{} {}: cold {:.0}, after a restart {:.0}, warm {:.0} ms",
                r.page, r.path, r.cold, again, r.warm
            ));
        }
    }
    for (p, ms) in &pages {
        if *ms > WARM_MS {
            over.push(format!("page {p}: its calls at once took {ms:.0} ms warm"));
        }
    }
    assert!(over.is_empty(), "over the budget:\n{}", over.join("\n"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "measurement by hand: cargo test --release -p sentry-mate measure_pages -- --ignored --nocapture"]
async fn measure_pages() {
    let tmp = tempfile::tempdir().unwrap();
    let base = std::env::var_os("PERF_DATA").map_or_else(|| tmp.path().to_path_buf(), PathBuf::from);
    let (data, keys) = (base.join("data"), base.join("keys"));
    let fresh = !data.exists();
    if fresh {
        let size = Size { smo_days: 75, history: history::Options::big(end_ms()) };
        make(&data, &keys, &size).await;
    }
    let (rows, restart, pages) = run(&data, &keys, fresh).await;
    println!("{}", table(&rows, &restart, &pages));
}
