//! How fast uploaded logs are judged and answered `verified` on a large backlog (docs/plans/performance.md, "CAN log
//! checks on a backlog"): a car resends a long chain of signed logs out of order, 16 at a time as the CAN Bridge app
//! does, asks `POST /api/can/logs/status` about what it sent, and meanwhile sends new logs of its live chain; beside it
//! the background decoding looks every judged log's record up and a rebuild reads the car's whole book again and
//! again, as a full rebuild and the decode queue do on the server.
//!
//! - `a_backlog_is_judged_and_answered` (with the tests): 600 logs, every one verified, every live log answered
//!   `verified` within the target.
//! - `measure_backlog` (ignored, by hand, `--release`): `JUDGE_LOGS` logs (20,000) of `JUDGE_LOG_KB` KB (16); prints
//!   the table.
//!
//! Made-up keys and logs only.
#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used, clippy::print_stdout)]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::Engine;
use http_body_util::BodyExt;
use ingest::{Conn, Judge, Store, Tokens, Uploads, router};
use p256::ecdsa::{Signature, SigningKey, signature::Signer};
use p256::pkcs8::EncodePublicKey;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use std::net::{IpAddr, Ipv4Addr};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tower::ServiceExt;

const KEY: &str = "feedc0de2468";
const IP: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 44));
/// Uploads at once (the app's `maxParallel`).
const PARALLEL: usize = 16;

struct Bench {
    _dir: tempfile::TempDir,
    up: Arc<Uploads>,
    token: String,
    signing: SigningKey,
    der: Vec<u8>,
    log_len: usize,
}

fn name(i: u64) -> String {
    // one log a minute from a made-up start, as the bridge names them
    let t = 1_780_000_000u64 + i * 60;
    let (d, s) = (t / 86_400, t % 86_400);
    // days since 1970-01-01 to a civil date (Howard Hinnant's algorithm)
    let z = d as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{KEY}_{year:04}{month:02}{day:02}T{:02}{:02}{:02}Z_{:06}.log.xz",
        s / 3600,
        s % 3600 / 60,
        s % 60,
        i % 1_000_000
    )
}

impl Bench {
    async fn new(wait_ms: u64, judge: impl FnOnce(Judge) -> Judge) -> Bench {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        std::fs::create_dir_all(&data).unwrap();
        let (v, _) = vault::Vault::create(&data, &dir.path().join("keys"), vault::Kdf::FAST_FOR_TESTS).unwrap();
        let v = Arc::new(v);
        let tokens = Tokens::open(data.join("tokens.json"), v.clone()).await.unwrap();
        let (token, _) = tokens.create("Made-up car").await.unwrap();
        let store = Store::with_limits(data.join("can"), v, 0, 10_000_000, |_| u64::MAX);
        let up = Uploads::new(store.clone(), tokens, judge(Judge::new(store).wait_ms(wait_ms)));
        let signing = SigningKey::from_slice(&Sha256::digest(b"made-up bridge key for the backlog")).unwrap();
        let der = signing.verifying_key().to_public_key_der().unwrap().as_bytes().to_vec();
        let log_len = std::env::var("JUDGE_LOG_KB").ok().and_then(|v| v.parse::<usize>().ok()).unwrap_or(16) * 1024;
        Bench { _dir: dir, up, token, signing, der, log_len }
    }

    /// Log `i`'s bytes: made up, different for every log, as hard to pack as an xz file.
    fn log(&self, i: u64) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.log_len + 32);
        let mut block = Sha256::digest(i.to_le_bytes());
        while out.len() < self.log_len {
            out.extend_from_slice(&block);
            block = Sha256::digest(block);
        }
        out.truncate(self.log_len);
        out
    }

    /// Log `i`'s sidecar, signed after log `i - 1`.
    fn sidecar(&self, i: u64, log: &[u8]) -> Vec<u8> {
        let b64 = base64::engine::general_purpose::STANDARD;
        let n = name(i);
        let (prev_name, prev_sha) = if i == 0 {
            ("-".to_string(), trust::GENESIS_SHA.to_string())
        } else {
            (name(i - 1), hex::encode(Sha256::digest(self.log(i - 1))))
        };
        let sha = hex::encode(Sha256::digest(log));
        let msg = trust::signed_message(KEY, &n, &sha, log.len() as u64, i, &prev_name, &prev_sha);
        let sig: Signature = self.signing.sign(msg.as_bytes());
        serde_json::to_vec(&json!({ "format": "canbridge-columns-xz/2", "integrity": {
            "signed": true, "version": trust::VERSION, "algorithm": trust::ALGORITHM,
            "public_key": b64.encode(&self.der), "key_fingerprint": trust::fingerprint(&self.der), "vehicle_key": KEY,
            "sha256": sha, "length": log.len(), "index": i, "prev_name": prev_name, "prev_sha256": prev_sha,
            "signature": b64.encode(sig.to_der().as_bytes()) } }))
        .unwrap()
    }

    async fn call(&self, path: &str, headers: &[(&str, String)], body: Vec<u8>) -> (StatusCode, Value) {
        let mut b =
            Request::builder().method("POST").uri(path).header("authorization", format!("Bearer {}", self.token));
        for (k, v) in headers {
            b = b.header(*k, v);
        }
        let mut req = b.body(Body::from(body)).unwrap();
        req.extensions_mut().insert(Conn { tls: true, ip: IP });
        let res = router(self.up.clone()).oneshot(req).await.unwrap();
        let status = res.status();
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    async fn put(&self, name: &str, bytes: &[u8]) {
        let sha = hex::encode(Sha256::digest(bytes));
        let h =
            [("upload-offset", "0".to_string()), ("upload-length", bytes.len().to_string()), ("upload-sha256", sha)];
        // (a server error is told to send again, as the app does)
        let (mut s, mut b) = self.call(&format!("/api/can/logs/{name}"), &h, bytes.to_vec()).await;
        for _ in 0..3 {
            if !s.is_server_error() {
                break;
            }
            (s, b) = self.call(&format!("/api/can/logs/{name}"), &h, bytes.to_vec()).await;
        }
        assert_eq!(s, StatusCode::CREATED, "{name}: {b}");
    }

    /// Log `i` and its sidecar (in either order).
    async fn send(&self, i: u64, sidecar_first: bool) {
        let log = self.log(i);
        let side = self.sidecar(i, &log);
        let n = name(i);
        if sidecar_first {
            self.put(&format!("{n}.json"), &side).await;
            self.put(&n, &log).await;
        } else {
            self.put(&n, &log).await;
            self.put(&format!("{n}.json"), &side).await;
        }
    }

    /// The `check` of each name.
    async fn checks(&self, names: &[String]) -> HashMap<String, String> {
        let body = serde_json::to_vec(&json!({ "names": names })).unwrap();
        let (s, b) = self.call("/api/can/logs/status", &[("content-type", "application/json".into())], body).await;
        assert_eq!(s, StatusCode::OK, "{b}");
        let files = b["files"].as_object().cloned().unwrap_or_default();
        files.into_iter().filter_map(|(n, v)| Some((n, v["check"].as_str()?.to_string()))).collect()
    }
}

/// A simple shuffle (the order the backlog is sent in), the same on every run.
fn shuffled(n: u64, seed: u64) -> Vec<u64> {
    let mut v: Vec<u64> = (0..n).collect();
    let mut x = seed | 1;
    for i in (1..v.len()).rev() {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        v.swap(i, (x % (i as u64 + 1)) as usize);
    }
    v
}

#[derive(Debug)]
struct Run {
    logs: u64,
    /// From the first upload to the last log of the backlog stored.
    upload_s: f64,
    /// From the first upload until every log of the backlog answered `verified`.
    all_verified_s: f64,
    /// Logs answered `verified` per second over that time.
    per_s: f64,
    /// For the live logs (sent in order while the backlog goes): from the log stored to `verified`, ms.
    live_ms: Vec<u64>,
    /// For the live logs: the upload of the log and then its sidecar, which answers once the log is judged, ms.
    live_send_ms: Vec<u64>,
    status_calls: u64,
    status_ms_max: u64,
    status_ms_median: u64,
    book_reads: u64,
    lookups: u64,
}

fn pct(v: &[u64], p: f64) -> u64 {
    if v.is_empty() {
        return 0;
    }
    let mut s = v.to_vec();
    s.sort_unstable();
    s[((s.len() as f64 - 1.0) * p).round() as usize]
}

/// How long a whole run may take, and how long it may go without one more log verified, before it is called stuck.
struct Limits {
    total: Duration,
    stall: Duration,
}

/// The test: a backlog of 600 logs takes seconds on one core; ten minutes is a hang.
const TEST_LIMITS: Limits = Limits { total: Duration::from_secs(600), stall: Duration::from_secs(60) };
/// By hand with 20,000 logs on whatever machine.
const MEASURE_LIMITS: Limits = Limits { total: Duration::from_secs(3 * 3600), stall: Duration::from_secs(600) };

/// The logs asked about and not answered `verified`, each with its own record and the record of the log before it:
/// what a log stuck behind another one looks like.
async fn stuck_report(b: &Bench, left: &[String]) -> String {
    let mut out = format!("{} logs not verified; first {}:\n", left.len(), left.len().min(8));
    for n in left.iter().take(8) {
        let index = n.rsplit('_').next().and_then(|t| t.strip_suffix(".log.xz")).and_then(|t| t.parse::<u64>().ok());
        let prev = index.filter(|i| *i > 0).map(|i| name(i - 1));
        let own = b.up.judge.record(KEY, n).await.map(|r| (r.status, r.reason));
        let before = match &prev {
            Some(p) => b.up.judge.record(KEY, p).await.map(|r| (r.status, r.reason)),
            None => None,
        };
        out.push_str(&format!("  {n}: {own:?}; before it {prev:?}: {before:?}\n"));
    }
    out
}

/// Stops the test at once when a task of it ended in a panic (or, for those that run to the end, when one ended).
fn task_ended(ended: Option<Result<(), tokio::task::JoinError>>, what: &str) {
    if let Some(r) = ended {
        r.unwrap_or_else(|e| panic!("the {what} task failed: {e}"));
    }
}

/// The whole scenario: `n` logs of the backlog, then live logs every `live_every` while it goes.
async fn run(n: u64, live_every: Duration, limits: Limits) -> Run {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let b = Arc::new(
        Bench::new(10 * 60 * 1000, move |j| {
            j.on_judged(Arc::new(move |judged: &ingest::judge::Judged| {
                let _ = tx.send(judged.name.clone());
            }))
        })
        .await,
    );
    let stop = Arc::new(AtomicBool::new(false));
    let (book_reads, lookups) = (Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)));
    // the engine has a judge of its own over the same store (as the server does): its reads take turns with the
    // uploads' judge through the key's lock file
    let engine = Arc::new(Judge::new(b.up.store.clone()));
    // the decode queue: every judged log's record looked up, one after the other
    // (the decoder and the rebuild run until the end; their ending before it, or a panic, is looked for every round)
    let mut background = tokio::task::JoinSet::new();
    {
        let (engine, lookups) = (engine.clone(), lookups.clone());
        background.spawn(async move {
            while let Some(name) = rx.recv().await {
                engine.record(KEY, &name).await;
                lookups.fetch_add(1, Ordering::Relaxed);
            }
        });
    }
    // a full rebuild: the car's whole book read, then each of its logs' records looked up in turn, again and again
    {
        let (engine, stop, book_reads, lookups) = (engine.clone(), stop.clone(), book_reads.clone(), lookups.clone());
        background.spawn(async move {
            while !stop.load(Ordering::Relaxed) {
                let book = engine.book(KEY).await;
                book_reads.fetch_add(1, Ordering::Relaxed);
                for name in book.logs.keys() {
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    engine.record(KEY, name).await;
                    lookups.fetch_add(1, Ordering::Relaxed);
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        });
    }

    // the newest log of the backlog goes first, so the live chain after it is never waiting for its log before
    let mut order = shuffled(n - 1, 0x5eed);
    order.insert(0, n - 1);
    let queue = Arc::new(Mutex::new(order.into_iter().collect::<std::collections::VecDeque<u64>>()));
    let stored: Arc<Mutex<BTreeMap<String, Instant>>> = Arc::default();
    let t0 = Instant::now();
    let mut senders = tokio::task::JoinSet::new();
    for _ in 0..PARALLEL {
        let (b, queue, stored) = (b.clone(), queue.clone(), stored.clone());
        senders.spawn(async move {
            loop {
                let Some(i) = queue.lock().unwrap().pop_front() else { return };
                b.send(i, false).await;
                stored.lock().unwrap().insert(name(i), Instant::now());
            }
        });
    }
    // the live chain: one new log at a time after the backlog's newest (the app's status calls below time them)
    let live_stored: Arc<Mutex<BTreeMap<String, Instant>>> = Arc::default();
    let mut live = {
        let (b, stop, live_stored) = (b.clone(), stop.clone(), live_stored.clone());
        tokio::spawn(async move {
            let (mut i, mut sends) = (n, Vec::new());
            // (the first live log follows the backlog's newest, sent first)
            tokio::time::sleep(Duration::from_millis(300)).await;
            while !stop.load(Ordering::Relaxed) {
                let at = Instant::now();
                b.send(i, false).await;
                live_stored.lock().unwrap().insert(name(i), Instant::now());
                sends.push(at.elapsed().as_millis() as u64);
                i += 1;
                tokio::time::sleep(live_every).await;
            }
            sends
        })
    };
    // the app asking about what it sent, at most 2000 names a call: the live logs not answered yet, then the
    // backlog's, oldest first (a call every 0.7 s keeps within the token's 1,200 calls in 10 minutes)
    let mut verified_at: HashMap<String, Instant> = HashMap::new();
    let mut live_ms: Vec<u64> = Vec::new();
    let mut upload_done: Option<f64> = None;
    let (mut status_calls, mut status_ms) = (0u64, Vec::new());
    let mut last_progress = Instant::now();
    loop {
        // a task that panicked (a failed upload, say) stops the test here, not hours later
        while let Some(r) = senders.try_join_next() {
            task_ended(Some(r), "sender");
        }
        if let Some(r) = background.try_join_next() {
            task_ended(Some(r), "decoder or rebuild");
            panic!("the decoder or the rebuild ended before the end of the run");
        }
        if live.is_finished() {
            panic!("the live task ended before the end of the run: {:?}", (&mut live).await.err());
        }
        if upload_done.is_none() && senders.is_empty() {
            upload_done = Some(t0.elapsed().as_secs_f64());
        }
        let live_now: BTreeMap<String, Instant> = live_stored.lock().unwrap().clone();
        let mut ask: Vec<String> = live_now.keys().filter(|k| !verified_at.contains_key(*k)).cloned().collect();
        let backlog_left = {
            let s = stored.lock().unwrap();
            let left: Vec<String> = s.keys().filter(|k| !verified_at.contains_key(*k)).cloned().collect();
            let room = 2000usize.saturating_sub(ask.len());
            ask.extend(left.iter().take(room).cloned());
            left.len()
        };
        if backlog_left == 0 && upload_done.is_some() {
            break;
        }
        let stalled = last_progress.elapsed() > limits.stall;
        let late = t0.elapsed() > limits.total;
        if stalled || late {
            let left: Vec<String> = {
                let s = stored.lock().unwrap();
                s.keys().chain(live_now.keys()).filter(|k| !verified_at.contains_key(*k)).cloned().collect()
            };
            let why = if stalled { "no log verified for" } else { "the backlog was not all verified within" };
            let after = if stalled { limits.stall } else { limits.total };
            let stored_now = stored.lock().unwrap().len();
            panic!("{why} {after:?} ({stored_now} stored)\n{}", stuck_report(&b, &left).await);
        }
        if !ask.is_empty() {
            let at = Instant::now();
            let c = b.checks(&ask).await;
            status_ms.push(at.elapsed().as_millis() as u64);
            status_calls += 1;
            for (k, v) in c {
                match v.as_str() {
                    "verified" => {
                        let now = Instant::now();
                        if let Some(t) = live_now.get(&k) {
                            live_ms.push(now.duration_since(*t).as_millis() as u64);
                        }
                        verified_at.insert(k, now);
                        last_progress = now;
                    }
                    "failed" => panic!("{k} failed"),
                    _ => {}
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(700)).await;
    }
    let all_verified_s = t0.elapsed().as_secs_f64();
    stop.store(true, Ordering::Relaxed);
    let live_send_ms = live.await.unwrap();
    // (the rebuild ends by itself; the decoder waits for the next judged log for ever and is stopped here)
    background.abort_all();
    while let Some(r) = background.join_next().await {
        if let Err(e) = r {
            assert!(e.is_cancelled(), "the decoder or the rebuild failed: {e}");
        }
    }
    drop(b.up.judge.book(KEY).await);
    let backlog_verified = stored.lock().unwrap().keys().filter(|k| verified_at.contains_key(*k)).count();
    assert_eq!(backlog_verified as u64, n);
    Run {
        logs: n,
        upload_s: upload_done.unwrap_or(all_verified_s),
        all_verified_s,
        per_s: n as f64 / all_verified_s,
        live_ms,
        live_send_ms,
        status_calls,
        status_ms_max: status_ms.iter().copied().max().unwrap_or(0),
        status_ms_median: pct(&status_ms, 0.5),
        book_reads: book_reads.load(Ordering::Relaxed),
        lookups: lookups.load(Ordering::Relaxed),
    }
}

fn print(r: &Run) {
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
    println!("judge backlog: {} logs, {cores} cores", r.logs);
    println!("  uploads stored         {:>8.1} s", r.upload_s);
    println!("  every log verified     {:>8.1} s  ({:.0} logs/s)", r.all_verified_s, r.per_s);
    println!(
        "  a live log to verified  median {} ms, 95% {} ms, max {} ms ({} logs)",
        pct(&r.live_ms, 0.5),
        pct(&r.live_ms, 0.95),
        r.live_ms.iter().max().unwrap_or(&0),
        r.live_ms.len()
    );
    println!(
        "  a live log and its sidecar uploaded (judged when the sidecar is in)  median {} ms, max {} ms",
        pct(&r.live_send_ms, 0.5),
        r.live_send_ms.iter().max().unwrap_or(&0)
    );
    println!(
        "  status calls {}: median {} ms, max {} ms; whole books read {}, records looked up {} ({:.0}/s)",
        r.status_calls,
        r.status_ms_median,
        r.status_ms_max,
        r.book_reads,
        r.lookups,
        r.lookups as f64 / r.all_verified_s
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_backlog_is_judged_and_answered() {
    let r = tokio::time::timeout(TEST_LIMITS.total, run(600, Duration::from_millis(200), TEST_LIMITS))
        .await
        .expect("the run did not end within its time");
    print(&r);
    assert!(!r.live_ms.is_empty());
    let worst = r.live_ms.iter().max().copied().unwrap_or(0);
    assert!(worst < 5_000, "a live log took {worst} ms to verify");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "by hand, --release: JUDGE_LOGS=20000 JUDGE_LOG_KB=16"]
async fn measure_backlog() {
    let n = std::env::var("JUDGE_LOGS").ok().and_then(|v| v.parse().ok()).unwrap_or(20_000);
    let r = run(n, Duration::from_secs(2), MEASURE_LIMITS).await;
    print(&r);
}
