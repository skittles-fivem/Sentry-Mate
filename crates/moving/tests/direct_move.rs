//! The direct move with two servers in one process, on loopback, each with its own folders: the old one takes
//! uploads over TLS all through the move, the new one pulls everything and takes over. All made up: no real car
//! keys, tokens or logs.
#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]

mod common;

use common::{APP, KEY, Server, empty, fill, log_name, server};
use ingest::server::{Identity, serve_tls, tls_dir};
use ingest::{Judge, Store, Tokens, Uploads};
use moving::client::{Address, Client};
use moving::old::Offer;
use moving::pull::{self, Options};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use vault::Vault;

/// A running server on 127.0.0.1: the upload API and the move routes on one TLS port.
struct Running {
    offer: Arc<Offer>,
    up: Arc<Uploads>,
    addr: SocketAddr,
    fingerprint: String,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl Running {
    fn stop(self) {
        for t in &self.tasks {
            t.abort();
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        for t in &self.tasks {
            t.abort();
        }
    }
}

async fn run_server(data: &Path, vault: Arc<Vault>, port: u16) -> Running {
    let id = Identity::load_or_create(&tls_dir(data), &["localhost".to_string()], &vault).unwrap();
    let gate = gate::Gate::open(data).unwrap();
    let store = Store::with_limits(data.join("can"), vault.clone(), 0, 200_000, |_| u64::MAX);
    let tokens = Tokens::open(data.join("tokens.json"), vault.clone()).await.unwrap();
    let up = Uploads::with_gate(store.clone(), tokens, Judge::new(store), gate.clone(), Duration::from_secs(2));
    let (d, v, g, fp) = (data.to_path_buf(), vault.clone(), gate.clone(), id.fingerprint());
    let offer = tokio::task::spawn_blocking(move || Offer::open(&d, v, g, APP, &fp)).await.unwrap().unwrap();
    let app = ingest::router(up.clone()).merge(moving::old::router(offer.clone()));
    let listener = bind(port).await;
    let addr = listener.local_addr().unwrap();
    let tls = id.tls_config().unwrap();
    let server = tokio::spawn(async move {
        let _ = serve_tls(listener, tls, app).await;
    });
    let ticker = offer.spawn_ticker();
    Running { offer, up, addr, fingerprint: id.fingerprint(), tasks: vec![server, ticker] }
}

/// A listener on 127.0.0.1 (the port may still be held a moment by a server just stopped: its task ends a little
/// after it is aborted).
async fn bind(port: u16) -> tokio::net::TcpListener {
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        match tokio::net::TcpListener::bind(("127.0.0.1", port)).await {
            Ok(l) => return l,
            Err(e) if Instant::now() < until => {
                let _ = e;
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(e) => panic!("{e}"),
        }
    }
}

fn opts() -> Options {
    let mut o = Options::new(APP);
    o.min_free = 0;
    o.free_bytes = Box::new(|_| u64::MAX);
    o.connections = 3;
    o.retry_most = Duration::from_millis(200);
    o.between_rounds = Duration::from_millis(200);
    o.timeout = Duration::from_secs(20);
    o
}

fn code_of(s: &Server) -> String {
    moving::old::make_code(&s.data, &s.vault, false, false).unwrap().code.to_string()
}

/// One chunk to a server over TLS, as the CAN Bridge sends it: the status it answered.
fn upload(addr: SocketAddr, fp: &str, token: &str, name: &str, whole: &[u8], from: usize, to: usize) -> u16 {
    let Ok(mut c) = Client::pinned(Address { host: "127.0.0.1".into(), port: addr.port() }, fp) else { return 0 };
    c.set_timeout(Duration::from_secs(30));
    let sha = hex::encode(Sha256::digest(whole));
    let (auth, off, len) = (format!("Bearer {token}"), from.to_string(), whole.len().to_string());
    let h = [
        ("Authorization", auth.as_str()),
        ("Upload-Offset", off.as_str()),
        ("Upload-Length", len.as_str()),
        ("Upload-SHA256", sha.as_str()),
    ];
    match c.request("POST", &format!("/api/can/logs/{name}"), &h, &whole[from..to]) {
        Ok(r) => {
            let s = r.status;
            let _ = r.bytes(1 << 20);
            s
        }
        Err(_) => 0,
    }
}

fn log_bytes(i: u32) -> Vec<u8> {
    (0..30_000 + i * 37).map(|b| ((b * 13 + i) % 251) as u8).collect()
}

/// Uploads logs one after another, each in two chunks, until told to stop, and remembers what was answered.
struct Uploader {
    stop: Arc<AtomicBool>,
    /// For each log: the bytes a server acknowledged (2xx) so far.
    acked: Arc<Mutex<BTreeMap<String, usize>>>,
    statuses: Arc<Mutex<BTreeMap<u16, usize>>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Uploader {
    fn start(target: Arc<Mutex<(SocketAddr, String)>>, token: String, first: u32) -> Uploader {
        let stop = Arc::new(AtomicBool::new(false));
        let acked = Arc::new(Mutex::new(BTreeMap::new()));
        let statuses = Arc::new(Mutex::new(BTreeMap::new()));
        let (s, a, st) = (stop.clone(), acked.clone(), statuses.clone());
        let thread = std::thread::spawn(move || {
            let mut i = first;
            while !s.load(Ordering::Relaxed) {
                let name = log_name(i);
                let bytes = log_bytes(i);
                let half = bytes.len() / 2;
                let mut at = a.lock().unwrap().get(&name).copied().unwrap_or(0);
                while at < bytes.len() && !s.load(Ordering::Relaxed) {
                    let to = if at < half { half } else { bytes.len() };
                    let (addr, fp) = target.lock().unwrap().clone();
                    let code = upload(addr, &fp, &token, &name, &bytes, at, to);
                    *st.lock().unwrap().entry(code).or_default() += 1;
                    if code == 200 || code == 201 {
                        at = to;
                        a.lock().unwrap().insert(name.clone(), at);
                    } else {
                        std::thread::sleep(Duration::from_millis(100));
                    }
                }
                if at == bytes.len() {
                    i += 1;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        });
        Uploader { stop, acked, statuses, thread: Some(thread) }
    }

    fn finish(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            t.join().unwrap();
        }
    }
}

async fn token_for(s: &Server) -> String {
    let tokens = Tokens::open(s.data.join("tokens.json"), s.vault.clone()).await.unwrap();
    // a replacement for the car's token: the car's key has logs here, so only a token made for it may take it
    tokens.create_for("Model Y test", &[KEY.to_string()]).await.unwrap().0
}

async fn token_list(data: &Path, v: Arc<Vault>) -> Vec<(String, String, bool, Option<String>)> {
    let t = Tokens::open(data.join("tokens.json"), v).await.unwrap();
    t.list().await.into_iter().map(|t| (t.id, t.car, t.revoked, t.vehicle_key)).collect()
}

fn address(r: &Running) -> String {
    format!("127.0.0.1:{}", r.addr.port())
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    tokio::task::spawn_blocking(f).await.unwrap()
}

fn new_vault(data: &Path, keys: &Path) -> Arc<Vault> {
    Arc::new(Vault::open(data, keys).unwrap())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_whole_move_while_a_bridge_keeps_uploading() {
    let old = server();
    fill(&old).await;
    let token = token_for(&old).await;
    let o = run_server(&old.data, old.vault.clone(), 0).await;
    let target = Arc::new(Mutex::new((o.addr, o.fingerprint.clone())));
    let mut bridge = Uploader::start(target.clone(), token.clone(), 100);
    // a few logs arrive before the move starts
    let t0 = Instant::now();
    while bridge.acked.lock().unwrap().len() < 3 {
        assert!(t0.elapsed() < Duration::from_secs(30), "the bridge's uploads don't arrive");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let code = code_of(&old);
    let (_dir, data, keys) = empty();
    let addr = address(&o);
    let (d, k, c) = (data.clone(), keys.clone(), code.clone());
    blocking(move || pull::begin(&d, &k, &addr, &c, &opts())).await.unwrap();
    assert!(matches!(pull::pending(&data, &keys), pull::Pending::Moving { phase: pull::Phase::Copying, .. }));
    let (d, k) = (data.clone(), keys.clone());
    let done = blocking(move || pull::run(&d, &k, &opts())).await.unwrap();
    assert!(o.offer.is_moved(), "the old server took the finish");
    assert!(done.same_machine, "127.0.0.1 is this machine");
    assert_eq!(done.cert_sha256, o.fingerprint);
    // the old server answers every upload path `503 moved` from now on
    let status = upload(o.addr, &o.fingerprint, &token, &log_name(999), &log_bytes(999), 0, 10);
    assert_eq!(status, 503);
    // the new server starts on what was moved and the bridge is pointed at it
    let v = new_vault(&data, &keys);
    assert_eq!(v.key_id(), old.vault.key_id(), "the same data key");
    let n = run_server(&data, v.clone(), 0).await;
    assert_eq!(n.fingerprint, o.fingerprint, "the bridge's pinned certificate keeps working");
    *target.lock().unwrap() = (n.addr, n.fingerprint.clone());
    let acked_at_move = bridge.acked.lock().unwrap().clone();
    let t0 = Instant::now();
    while bridge.acked.lock().unwrap().len() < acked_at_move.len() + 3 {
        assert!(t0.elapsed() < Duration::from_secs(30), "the bridge's uploads don't reach the new server");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    bridge.finish();
    // the old server hears that the new one serves: "done"
    let (d, v2) = (data.clone(), v.clone());
    assert!(blocking(move || pull::confirm(&d, &v2, Duration::from_secs(10))).await.unwrap());
    let st = moving::old::status(&old.data, &old.vault).unwrap();
    assert!(st.moved.as_ref().unwrap().switched_ms.is_some());
    let (d, v2) = (data.clone(), v.clone());
    assert!(!blocking(move || pull::confirm(&d, &v2, Duration::from_secs(10))).await.unwrap(), "told once");
    // the bridge only ever saw 200, 201 and 503 (and nothing acknowledged was lost)
    let statuses = bridge.statuses.lock().unwrap().clone();
    assert!(statuses.keys().all(|s| [200, 201, 503].contains(s)), "{statuses:?}");
    // an upload that came during the pause waited at the gate and was told to try later, never answered 2xx
    assert!(statuses.contains_key(&503), "{statuses:?}");
    let store = Store::with_limits(data.join("can"), v.clone(), 0, 200_000, |_| u64::MAX);
    let acked = bridge.acked.lock().unwrap().clone();
    for (name, got) in &acked {
        let s = store.state_of(name).await.unwrap();
        assert!(s.received as usize >= *got, "{name}: {} < {got}", s.received);
    }
    // everything the old server had at the move is on the new one, the same bytes and times
    let before = common::carried(&old.data);
    let after = common::carried(&data);
    for (p, (bytes, mt)) in &before {
        // an upload in progress at the move may have finished on the new server since (checked above)
        if p.starts_with("can/") && !p.contains("/uploading/") && !p.contains(".waiting") && !after.contains_key(p) {
            panic!("{p} is missing on the new server");
        }
        if let Some((b2, mt2)) = after.get(p)
            && !p.ends_with("counts.json")
            && !p.contains("trust")
            && !p.contains("uploading")
            && !p.contains(".waiting")
        {
            assert_eq!((bytes, mt), (b2, mt2), "{p}");
        }
    }
    assert_eq!(token_list(&old.data, old.vault.clone()).await, token_list(&data, v.clone()).await, "the same tokens");
    // the upload that was in progress on the old server finishes on the new one
    let big: Vec<u8> = (0..300_000u32).map(|b| (b * 7 % 253) as u8).collect();
    let sha = hex::encode(Sha256::digest(&big));
    let r = store
        .receive(&log_name(11), bytes::Bytes::copy_from_slice(&big[200_000..]), 200_000, 300_000, &sha)
        .await
        .unwrap();
    assert!(r.complete);
    // the move's staging is gone, the note is there
    assert!(!data.join(pull::STAGING).exists() && !keys.join(pull::STAGING).exists());
    let note = pull::moved_in(&data, &v).unwrap();
    assert!(note.confirmed);
    assert_eq!(moving::import::imported(&data, &v), None);
    // each server's log of the move: its steps in order, each with a time, nothing secret
    let new_log: Vec<String> = note.log.iter().map(|l| l.text.clone()).collect();
    let old_log: Vec<String> = moving::old::log(&old.data, &old.vault).iter().map(|l| l.text.clone()).collect();
    let steps = |log: &[String], want: &[&str]| {
        let mut at = 0;
        for w in want {
            match log[at..].iter().position(|l| l.contains(w)) {
                Some(i) => at += i + 1,
                None => panic!("no \"{w}\" (in order) in {log:#?}"),
            }
        }
    };
    steps(
        &new_log,
        &[
            "Moving here from",
            "Connected:",
            "Round 1: copying",
            "Copying file 1 of",
            "Round 1 done",
            "Last-round pause",
            "Last round:",
            "Switching",
            "Switched:",
        ],
    );
    steps(&old_log, &["A move code was made", "connected", "Sent ", "Last-round pause", "copy matches", "Done:"]);
    let data_text = data.to_string_lossy().to_string();
    for l in new_log.iter().chain(&old_log) {
        for secret in [code.as_str(), token.as_str(), data_text.as_str(), "can/", ".log.xz"] {
            assert!(!l.contains(secret), "{l}");
        }
        assert!(!l.split(|c: char| !c.is_ascii_hexdigit()).any(|w| w.len() >= 32), "{l}");
    }
    assert!(note.log.iter().chain(&moving::old::log(&old.data, &old.vault)).all(|l| l.at_ms > 1_700_000_000_000));
    n.stop();
    o.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wrong_expired_reused_and_too_old_codes_are_refused() {
    let old = server();
    fill(&old).await;
    let o = run_server(&old.data, old.vault.clone(), 0).await;
    let addr = address(&o);
    // no code made yet: there is no move here
    let (_d0, data, keys) = empty();
    let other = moving::code::MoveCode::random().unwrap().text().to_string();
    let (d, k, a, c) = (data.clone(), keys.clone(), addr.clone(), other.clone());
    let p = blocking(move || pull::begin(&d, &k, &a, &c, &opts())).await.unwrap_err();
    assert_eq!(p.code, "no_move", "{p}");
    assert!(!data.join(pull::STAGING).exists(), "nothing staged for a claim that failed");
    // a typo is found before anything is sent
    let code = code_of(&old);
    let typo = format!("{}{}", &code[..code.len() - 1], if code.ends_with('0') { '1' } else { '0' });
    let (d, k, a) = (data.clone(), keys.clone(), addr.clone());
    let p = blocking(move || pull::begin(&d, &k, &a, &typo, &opts())).await.unwrap_err();
    assert_eq!(p.code, "wrong_code");
    assert!(p.message.contains("typo"), "{p}");
    // another code: the old server refuses it and counts it
    let (d, k, a, c) = (data.clone(), keys.clone(), addr.clone(), other.clone());
    let p = blocking(move || pull::begin(&d, &k, &a, &c, &opts())).await.unwrap_err();
    assert_eq!(p.code, "wrong_code", "{p}");
    assert_eq!(moving::old::status(&old.data, &old.vault).unwrap().code.unwrap().wrong, 1);
    // an older new server: refused, and the code still works
    let mut older = opts();
    older.app = "0.3.9".into();
    let (d, k, a, c) = (data.clone(), keys.clone(), addr.clone(), code.clone());
    let p = blocking(move || pull::begin(&d, &k, &a, &c, &older)).await.unwrap_err();
    assert_eq!(p.code, "too_old", "{p}");
    assert!(moving::old::status(&old.data, &old.vault).unwrap().code.unwrap().claimed.is_none());
    // the right code is taken
    let (d, k, a, c) = (data.clone(), keys.clone(), addr.clone(), code.clone());
    blocking(move || pull::begin(&d, &k, &a, &c, &opts())).await.unwrap();
    let claimed = moving::old::status(&old.data, &old.vault).unwrap().code.unwrap().claimed.unwrap();
    assert_eq!(claimed.from, "127.0.0.1");
    // used once: a second new server is refused, and told when and by whom
    let (_d2, data2, keys2) = empty();
    let (d, k, a, c) = (data2.clone(), keys2.clone(), addr.clone(), code.clone());
    let p = blocking(move || pull::begin(&d, &k, &a, &c, &opts())).await.unwrap_err();
    assert_eq!(p.code, "code_used", "{p}");
    assert!(p.message.contains("127.0.0.1"), "{p}");
    // `move code` refuses while the move is under way; --replace ends it
    assert!(matches!(moving::old::make_code(&old.data, &old.vault, false, false), Err(moving::Error::MoveUnderWay(_))));
    let replaced = moving::old::make_code(&old.data, &old.vault, false, true).unwrap();
    // the first new server is told its move was cancelled; it keeps what it has
    let (d, k) = (data.clone(), keys.clone());
    let p = blocking(move || pull::run(&d, &k, &opts())).await.unwrap_err();
    assert_eq!(p.code, "cancelled", "{p}");
    assert!(data.join(pull::STAGING).exists(), "the copy is kept after a cancel on the old server");
    assert!(matches!(pull::pending(&data, &keys), pull::Pending::Moving { cancelled: true, .. }));
    // an expired code
    moving::old::expire_code_now(&old.data, &old.vault).unwrap();
    let (d, k, a, c) = (data2.clone(), keys2.clone(), addr.clone(), replaced.code.to_string());
    let p = blocking(move || pull::begin(&d, &k, &a, &c, &opts())).await.unwrap_err();
    assert_eq!(p.code, "code_expired", "{p}");
    // a new code from the same server carries on with the kept copy, and finishes
    let again = code_of(&old);
    let (d, k, a, c) = (data.clone(), keys.clone(), addr.clone(), again);
    blocking(move || pull::begin(&d, &k, &a, &c, &opts())).await.unwrap();
    let (d, k) = (data.clone(), keys.clone());
    blocking(move || pull::run(&d, &k, &opts())).await.unwrap();
    assert!(o.offer.is_moved());
    o.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ten_wrong_codes_from_one_address_must_wait() {
    let old = server();
    let o = run_server(&old.data, old.vault.clone(), 0).await;
    code_of(&old);
    let addr = address(&o);
    let mut last = String::new();
    for _ in 0..11 {
        let (_d, data, keys) = empty();
        let c = moving::code::MoveCode::random().unwrap().text().to_string();
        let a = addr.clone();
        last = blocking(move || pull::begin(&data, &keys, &a, &c, &opts())).await.unwrap_err().code;
    }
    assert_eq!(last, "slow_down");
    assert_eq!(moving::old::status(&old.data, &old.vault).unwrap().code.unwrap().wrong, 10);
    o.stop();
}

/// A server on the given port that answers a claim with a made-up answer (it holds no code).
async fn fake_server(port: u16, hits: Arc<Mutex<usize>>) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let made = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
    let id = Identity {
        chain: vec![made.cert.der().clone()],
        key: rustls::pki_types::PrivateKeyDer::Pkcs8(made.key_pair.serialize_der().into()),
    };
    let fp = id.fingerprint();
    let app = axum::Router::new().fallback(move |req: axum::extract::Request| {
        let (hits, fp) = (hits.clone(), fp.clone());
        async move {
            *hits.lock().unwrap() += 1;
            let _ = req;
            axum::Json(serde_json::json!({
                "session": hex::encode([1u8; 32]), "server_proof": hex::encode([2u8; 32]), "app": APP,
                "cert_sha256": fp, "files": 1, "bytes": 1, "recovery": "{}"
            }))
        }
    });
    let listener = bind(port).await;
    let addr = listener.local_addr().unwrap();
    let tls = id.tls_config().unwrap();
    (
        addr,
        tokio::spawn(async move {
            let _ = serve_tls(listener, tls, app).await;
        }),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_fake_old_server_is_refused_and_nothing_is_sent() {
    // a server that answers the claim but can't prove it made the code
    let hits = Arc::new(Mutex::new(0));
    let (fake, task) = fake_server(0, hits.clone()).await;
    let (_d, data, keys) = empty();
    let code = moving::code::MoveCode::random().unwrap().text().to_string();
    let a = format!("127.0.0.1:{}", fake.port());
    let (d, k) = (data.clone(), keys.clone());
    let p = blocking(move || pull::begin(&d, &k, &a, &code, &opts())).await.unwrap_err();
    assert_eq!(p.code, "not_proven", "{p}");
    assert!(p.message.contains("Nothing was sent"), "{p}");
    assert!(!data.join(pull::STAGING).exists());
    task.abort();

    // after the claim, the old certificate is pinned: another server at the old address gets nothing
    let old = server();
    fill(&old).await;
    let o = run_server(&old.data, old.vault.clone(), 0).await;
    let port = o.addr.port();
    let code = code_of(&old);
    let (_d2, data, keys) = empty();
    let (d, k, a, c) = (data.clone(), keys.clone(), address(&o), code);
    blocking(move || pull::begin(&d, &k, &a, &c, &opts())).await.unwrap();
    o.stop();
    let hits = Arc::new(Mutex::new(0));
    let (_, fake_task) = fake_server(port, hits.clone()).await;
    let mut o2 = opts();
    let stop = o2.stop.clone();
    o2.retry_most = Duration::from_millis(100);
    let (d, k) = (data.clone(), keys.clone());
    let pull = tokio::task::spawn_blocking(move || pull::run(&d, &k, &o2));
    tokio::time::sleep(Duration::from_secs(2)).await;
    stop.store(true, Ordering::Relaxed);
    let p = pull.await.unwrap().unwrap_err();
    assert_eq!(p.code, "stopped");
    assert_eq!(*hits.lock().unwrap(), 0, "no request reached a server with another certificate");
    match pull::status(&data, &keys).unwrap().unwrap().problem {
        Some(p) => assert_eq!(p.code, "not_proven", "{p:?}"),
        None => panic!("the problem is saved"),
    }
    fake_task.abort();
    // the real old server comes back at its address: the move carries on and finishes
    tokio::time::sleep(Duration::from_millis(200)).await;
    let o = run_server(&old.data, old.vault.clone(), port).await;
    let (d, k) = (data.clone(), keys.clone());
    blocking(move || pull::run(&d, &k, &opts())).await.unwrap();
    assert!(o.offer.is_moved());
    o.stop();
}

/// A TCP proxy to `to` that cuts the first `cuts` connections after a few hundred KB from the server.
async fn cutting_proxy(to: SocketAddr, cuts: usize) -> (SocketAddr, Arc<Mutex<usize>>, tokio::task::JoinHandle<()>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let cut = Arc::new(Mutex::new(0usize));
    let c2 = cut.clone();
    let task = tokio::spawn(async move {
        let mut n = 0usize;
        loop {
            let Ok((mut a, _)) = listener.accept().await else { return };
            n += 1;
            let limit = if n <= cuts { Some(80_000 + (n * 37_000) % 300_000) } else { None };
            let c3 = c2.clone();
            tokio::spawn(async move {
                let Ok(mut b) = tokio::net::TcpStream::connect(to).await else { return };
                let (mut ar, mut aw) = a.split();
                let (mut br, mut bw) = b.split();
                let up = async {
                    let _ = tokio::io::copy(&mut ar, &mut bw).await;
                };
                let down = async {
                    let mut buf = vec![0u8; 16 * 1024];
                    let mut sent = 0usize;
                    loop {
                        let Ok(k) = br.read(&mut buf).await else { return };
                        if k == 0 {
                            return;
                        }
                        if aw.write_all(&buf[..k]).await.is_err() {
                            return;
                        }
                        sent += k;
                        if limit.is_some_and(|l| sent >= l) {
                            *c3.lock().unwrap() += 1;
                            return;
                        }
                    }
                };
                tokio::select! {
                    _ = up => {}
                    _ = down => {}
                }
            });
        }
    });
    (addr, cut, task)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cut_connections_and_restarts_of_both_servers_carry_on() {
    let old = server();
    fill(&old).await;
    // a bigger log, so a cut lands in the middle of a file, and one bigger than a piece
    {
        let store = common::store_of(&old);
        for (i, size) in [(40u32, 3_000_000usize), (41, 17 * 1024 * 1024 + 5)] {
            let big: Vec<u8> = (0..size).map(|b| (b % 241) as u8 ^ (i as u8)).collect();
            let sha = hex::encode(Sha256::digest(&big));
            let mut off = 0;
            while off < big.len() {
                let end = (off + 190_000).min(big.len());
                store
                    .receive(
                        &log_name(i),
                        bytes::Bytes::copy_from_slice(&big[off..end]),
                        off as u64,
                        big.len() as u64,
                        &sha,
                    )
                    .await
                    .unwrap();
                off = end;
            }
        }
    }
    let o = run_server(&old.data, old.vault.clone(), 0).await;
    let port = o.addr.port();
    let (proxy, cuts, ptask) = cutting_proxy(o.addr, 6).await;
    let code = code_of(&old);
    let (_d, data, keys) = empty();
    let (d, k, a, c) = (data.clone(), keys.clone(), format!("127.0.0.1:{}", proxy.port()), code);
    blocking(move || pull::begin(&d, &k, &a, &c, &opts())).await.unwrap();
    // the old server restarts after the claim: the session is kept
    o.stop();
    let o = run_server(&old.data, old.vault.clone(), port).await;
    // the first copy never counts a file twice (a cut connection asks again only for what it didn't settle)
    let over = Arc::new(Mutex::new(None::<(u64, u64)>));
    let watch = {
        let (d, k, over) = (data.clone(), keys.clone(), over.clone());
        tokio::spawn(async move {
            loop {
                if let Ok(Some(s)) = pull::status(&d, &k) {
                    let p = s.progress;
                    if p.round == 1 && p.files_done > p.files_total {
                        *over.lock().unwrap() = Some((p.files_done, p.files_total));
                    }
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
    };
    // the new server stops part way through the first copy (as a restart would), then carries on
    let o1 = opts();
    let stop = o1.stop.clone();
    let (d, k) = (data.clone(), keys.clone());
    let first = tokio::task::spawn_blocking(move || pull::run(&d, &k, &o1));
    let t0 = Instant::now();
    loop {
        if let Some(s) = pull::status(&data, &keys).unwrap()
            && s.progress.bytes_done > 1_000_000
        {
            break;
        }
        assert!(t0.elapsed() < Duration::from_secs(60), "the copy doesn't start");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    stop.store(true, Ordering::Relaxed);
    let r = first.await.unwrap();
    assert!(matches!(&r, Err(p) if p.code == "stopped") || r.is_ok(), "{:?}", r.err());
    if r.is_err() {
        // the old server restarts too, then the new one carries on to the end
        o.stop();
        let o = run_server(&old.data, old.vault.clone(), port).await;
        let (d, k) = (data.clone(), keys.clone());
        blocking(move || pull::run(&d, &k, &opts())).await.unwrap();
        assert!(o.offer.is_moved());
        o.stop();
    } else {
        o.stop();
    }
    assert!(*cuts.lock().unwrap() > 0, "the proxy cut connections");
    // the log says what it waited for and that it carried on, once each time, not once per try
    let log: Vec<String> = pull::status(&data, &keys)
        .unwrap()
        .map(|s| s.log)
        .or_else(|| pull::moved_in(&data, &new_vault(&data, &keys)).map(|n| n.log))
        .unwrap_or_default()
        .into_iter()
        .map(|l| l.text)
        .collect();
    assert!(log.iter().any(|l| l.starts_with("Waiting: ")), "{log:#?}");
    assert!(log.iter().any(|l| l.contains("again: carrying on where it stopped")), "{log:#?}");
    assert!(log.len() < 200, "{log:#?}");
    ptask.abort();
    watch.abort();
    assert_eq!(*over.lock().unwrap(), None, "files counted twice in the first copy (done, total)");
    let v = new_vault(&data, &keys);
    let store = Store::with_limits(data.join("can"), v, 0, 200_000, |_| u64::MAX);
    for (i, size) in [(40u32, 3_000_000usize), (41, 17 * 1024 * 1024 + 5)] {
        let big: Vec<u8> = (0..size).map(|b| (b % 241) as u8 ^ (i as u8)).collect();
        assert_eq!(store.read(&log_name(i)).await.unwrap().unwrap().as_slice(), big.as_slice(), "{i}");
    }
    let before = common::carried(&old.data);
    let after = common::carried(&data);
    assert_eq!(before.keys().collect::<Vec<_>>(), after.keys().collect::<Vec<_>>());
    assert_eq!(before, after, "every file the same bytes and times");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_moved_state_survives_a_restart_and_undo_takes_uploads_again() {
    let old = server();
    fill(&old).await;
    let token = token_for(&old).await;
    let o = run_server(&old.data, old.vault.clone(), 0).await;
    let port = o.addr.port();
    let code = code_of(&old);
    let (_d, data, keys) = empty();
    let (d, k, a, c) = (data.clone(), keys.clone(), address(&o), code);
    blocking(move || pull::begin(&d, &k, &a, &c, &opts())).await.unwrap();
    let (d, k) = (data.clone(), keys.clone());
    blocking(move || pull::run(&d, &k, &opts())).await.unwrap();
    o.stop();
    // restarted: still moved, uploads answered 503, writes refused
    let o = run_server(&old.data, old.vault.clone(), port).await;
    assert!(o.offer.is_moved());
    assert!(o.up.gate.closed().is_some());
    assert_eq!(upload(o.addr, &o.fingerprint, &token, &log_name(500), &log_bytes(500), 0, 100), 503);
    assert!(moving::old::refuse_if_moved(&old.data, &old.vault).is_err());
    assert!(matches!(moving::old::make_code(&old.data, &old.vault, false, false), Err(moving::Error::Move(_))));
    assert!(matches!(moving::old::cancel(&old.data, &old.vault), Err(moving::Error::Finished(_))));
    // a command from another process can't get the gate either
    let g = gate::Gate::open(&old.data).unwrap();
    assert!(g.enter(Duration::from_millis(100)).is_err());
    drop(g);
    // the new server's confirmation still arrives after the restart
    let v = new_vault(&data, &keys);
    let (d, v2) = (data.clone(), v.clone());
    assert!(blocking(move || pull::confirm(&d, &v2, Duration::from_secs(10))).await.unwrap());
    // undo: this server takes uploads again (the running one sees it within seconds)
    let undone = moving::old::undo(&old.data, &old.vault).unwrap().unwrap();
    assert!(undone.switched_ms.is_some());
    let offer = o.offer.clone();
    blocking(move || offer.tick_now()).await;
    assert!(!o.offer.is_moved());
    assert!(o.up.gate.closed().is_none());
    assert_eq!(upload(o.addr, &o.fingerprint, &token, &log_name(500), &log_bytes(500), 0, 100), 200);
    assert!(moving::old::undo(&old.data, &old.vault).unwrap().is_none());
    o.stop();
    // and after a restart it stays undone
    let o = run_server(&old.data, old.vault.clone(), port).await;
    assert!(!o.offer.is_moved());
    o.stop();
    let _ = KEY;
    let _: PathBuf = data;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cancel_on_the_new_server_removes_the_copy_and_tells_the_old_one() {
    let old = server();
    fill(&old).await;
    let o = run_server(&old.data, old.vault.clone(), 0).await;
    let code = code_of(&old);
    let (_d, data, keys) = empty();
    let (d, k, a, c) = (data.clone(), keys.clone(), address(&o), code);
    blocking(move || pull::begin(&d, &k, &a, &c, &opts())).await.unwrap();
    assert!(moving::old::status(&old.data, &old.vault).unwrap().code.unwrap().claimed.is_some());
    // the copy runs (here: stopped part way, as a server would be) while the owner cancels
    let (d, k) = (data.clone(), keys.clone());
    assert!(blocking(move || pull::cancel(&d, &k)).await.unwrap());
    assert!(!data.join(pull::STAGING).exists() && !keys.join(pull::STAGING).exists(), "the copy is removed");
    assert!(matches!(pull::pending(&data, &keys), pull::Pending::None));
    let st = moving::old::status(&old.data, &old.vault).unwrap();
    assert!(st.code.is_none(), "the old server's move ended");
    assert!(st.last_cancelled.unwrap().1.contains("new server"));
    // nothing of the owner's is left on the new server: it can take a move again
    moving::import::unused(&data, &keys).unwrap();
    o.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn with_the_old_server_gone_the_copy_opens_with_the_recovery_key() {
    let old = server();
    fill(&old).await;
    let o = run_server(&old.data, old.vault.clone(), 0).await;
    let code = code_of(&old);
    let (_d, data, keys) = empty();
    let (d, k, a, c) = (data.clone(), keys.clone(), address(&o), code);
    blocking(move || pull::begin(&d, &k, &a, &c, &opts())).await.unwrap();
    // claimed, nothing copied yet: nothing to switch to
    let e = pull::force_finish(&data, &keys, &old.recovery).unwrap_err();
    assert!(e.to_string().contains("nothing was copied"), "{e}");
    assert!(matches!(pull::pending(&data, &keys), pull::Pending::Moving { forced: false, .. }));
    // one round copied, then the old server dies before the finish
    let o1 = opts();
    let stop = o1.stop.clone();
    let (d, k) = (data.clone(), keys.clone());
    let run = tokio::task::spawn_blocking(move || pull::run(&d, &k, &o1));
    let t0 = Instant::now();
    while pull::status(&data, &keys).unwrap().is_some_and(|s| s.progress.files_done < 5) {
        assert!(t0.elapsed() < Duration::from_secs(30));
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    // the copy runs and the old server answers: it finishes by itself, no switch part way
    let e = pull::force_finish(&data, &keys, &old.recovery).unwrap_err();
    assert!(e.to_string().contains("under way"), "{e}");
    assert!(matches!(pull::pending(&data, &keys), pull::Pending::Moving { forced: false, .. }));
    o.stop();
    tokio::time::sleep(Duration::from_millis(300)).await;
    stop.store(true, Ordering::Relaxed);
    let r = run.await.unwrap();
    // the old server stopped long before the pause's 2 s settle ended: the move can't have finished
    assert_eq!(r.unwrap_err().code, "stopped");
    // a wrong recovery key is refused, nothing changed
    let wrong = "SMRK-0000-0000-0000-0000-0000-0000-0000-0000-0000-0000-0000-0000-0000";
    assert!(pull::force_finish(&data, &keys, wrong).is_err());
    assert!(data.join(pull::STAGING).exists());
    pull::force_finish(&data, &keys, &old.recovery).unwrap();
    let (d, k) = (data.clone(), keys.clone());
    let done = blocking(move || pull::run(&d, &k, &opts())).await.unwrap();
    assert!(done.forced);
    let v = new_vault(&data, &keys);
    assert_eq!(v.key_id(), old.vault.key_id());
    assert!(pull::moved_in(&data, &v).unwrap().forced);
    // nothing to tell the old server: it doesn't know it moved
    let (d, v2) = (data.clone(), v.clone());
    assert!(!blocking(move || pull::confirm(&d, &v2, Duration::from_secs(1))).await.unwrap());
    // the old recovery key opens the data here too
    std::fs::remove_file(keys.join(vault::KEY_FILE)).unwrap();
    Vault::restore(&data, &keys, &old.recovery).unwrap();
    assert!(!moving::old::is_moved(&old.data), "the old server doesn't know");
}

/// A new server that stops reading a fetch answer under the pause (its disk hangs, its container is paused): the old
/// server's fetch task waits forever on the full connection, the request counts as busy, so the pause's timers never
/// run and the old server answers every upload `503 moving` for as long as that connection stays open, far past the
/// 60 s quiet limit and the 15 minutes at most.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "slow: full tests (cargo test -- --include-ignored)"]
async fn a_fetch_the_new_server_stops_reading_does_not_hold_the_pause_for_ever() {
    use moving::proto::{self, ClaimAnswer, ClaimRequest, FetchRequest, PauseAnswer, StatusAnswer, Want};
    let old = server();
    // one big file: more than the channel and the socket buffers hold
    let big = old.data.join("big.bin");
    std::fs::write(&big, vec![0u8; 60 << 20]).unwrap();
    let md = std::fs::metadata(&big).unwrap();
    let (len, mt) = (md.len(), moving::tree::mtime_us(&md));
    let o = run_server(&old.data, old.vault.clone(), 0).await;
    let code = moving::code::MoveCode::parse(&code_of(&old)).unwrap();
    let (addr, fp) = (Address { host: "127.0.0.1".into(), port: o.addr.port() }, o.fingerprint.clone());
    let auth = *code.keys().auth;
    // the claim and the pause, as the new server makes them
    let session = blocking({
        let addr = addr.clone();
        move || {
            let mut c = Client::any(addr);
            let (_, exporter) = c.seen().unwrap();
            let req = ClaimRequest {
                v: 1,
                claim: hex::encode([5u8; 16]),
                app: APP.into(),
                proof: hex::encode(proto::client_proof(&auth, &exporter)),
            };
            let body = serde_json::to_vec(&req).unwrap();
            let r = c.request("POST", "/api/move/v1/claim", &[("Content-Type", "application/json")], &body).unwrap();
            assert_eq!(r.status, 200);
            r.json::<ClaimAnswer>().unwrap().session
        }
    })
    .await;
    let bearer = format!("Bearer {session}");
    let (a2, f2, b2) = (addr.clone(), fp.clone(), bearer.clone());
    let pause: PauseAnswer = blocking(move || {
        let mut c = Client::pinned(a2, &f2).unwrap();
        let r = c.request("POST", "/api/move/v1/pause", &[("Authorization", &b2)], b"").unwrap();
        assert_eq!(r.status, 200);
        r.json().unwrap()
    })
    .await;
    assert!(!pause.pause.is_empty());
    // a fetch of the big file whose answer is never read
    let (a2, f2, b2) = (addr.clone(), fp.clone(), bearer.clone());
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let stalled = std::thread::spawn(move || {
        let mut c = Client::pinned(a2, &f2).unwrap();
        let want = Want { p: "big.bin".into(), from: 0, to: len, v: proto::plain_version(len, mt), tail: None };
        let body = serde_json::to_vec(&FetchRequest { want: vec![want] }).unwrap();
        let r = c
            .request(
                "POST",
                "/api/move/v1/fetch",
                &[("Authorization", &b2), ("Content-Type", "application/json")],
                &body,
            )
            .unwrap();
        assert_eq!(r.status, 200);
        started_tx.send(()).unwrap();
        // the new server hangs: the connection stays open, nothing is read
        std::thread::sleep(Duration::from_secs(90));
        drop(r);
    });
    started_rx.recv().unwrap();
    // well past the 60 s quiet limit, with the ticker running
    tokio::time::sleep(moving::old::PAUSE_QUIET + Duration::from_secs(8)).await;
    let (a2, f2, b2) = (addr.clone(), fp.clone(), bearer.clone());
    let st: StatusAnswer = blocking(move || {
        let mut c = Client::pinned(a2, &f2).unwrap();
        let r = c.request("GET", "/api/move/v1/status", &[("Authorization", &b2)], b"").unwrap();
        r.json().unwrap()
    })
    .await;
    assert_ne!(
        st.state, "paused",
        "the pause still holds the old server's uploads although the new server sent nothing for over a minute"
    );
    o.stop();
    let _ = stalled.join();
}

/// A move whose session the old server no longer knows (its move state was removed) stops with `bad_session`; a new
/// code from the same old server carries it on with the same job id and the copy kept. A run told to stop before the
/// switch leaves it for the next run, and the note after the switch keeps the job id.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_move_that_lost_its_session_carries_on_with_a_new_code() {
    let old = server();
    fill(&old).await;
    let o = run_server(&old.data, old.vault.clone(), 0).await;
    let code = code_of(&old);
    let (_d, data, keys) = empty();
    let (d, k, a, c) = (data.clone(), keys.clone(), address(&o), code);
    blocking(move || pull::begin(&d, &k, &a, &c, &opts())).await.unwrap();
    let first = pull::status(&data, &keys).unwrap().unwrap();
    assert_eq!(first.job_id.len(), 8);
    assert_ne!(first.job_id, first.code_id);
    // part of the copy, then stopped (as a server is)
    let o1 = opts();
    let stop = o1.stop.clone();
    let (d, k) = (data.clone(), keys.clone());
    let run = tokio::task::spawn_blocking(move || pull::run(&d, &k, &o1));
    let t0 = Instant::now();
    while pull::status(&data, &keys).unwrap().is_some_and(|s| s.progress.files_done < 3) {
        assert!(t0.elapsed() < Duration::from_secs(30));
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    stop.store(true, Ordering::Relaxed);
    let _ = run.await.unwrap();
    let copied = pull::status(&data, &keys).unwrap().unwrap().progress.files_done;
    assert!(copied >= 3);
    // the old server forgets the move (its move state removed, a new code made): the next run stops with
    // bad_session, waiting for the owner
    std::fs::remove_file(old.data.join(moving::old::DIR).join("code.sealed")).unwrap();
    let code = code_of(&old);
    let (d, k) = (data.clone(), keys.clone());
    let p = blocking(move || pull::run(&d, &k, &opts())).await.unwrap_err();
    assert_eq!(p.code, "bad_session");
    assert!(!p.retry);
    let st = pull::status(&data, &keys).unwrap().unwrap();
    assert_eq!(st.problem.as_ref().map(|p| p.code.as_str()), Some("bad_session"));
    assert!(matches!(pull::pending(&data, &keys), pull::Pending::Moving { forced: false, .. }));
    // a new code from the same server carries it on: the same job, the copy kept
    let (d, k, a, c) = (data.clone(), keys.clone(), address(&o), code.clone());
    blocking(move || pull::begin(&d, &k, &a, &c, &opts())).await.unwrap();
    let st = pull::status(&data, &keys).unwrap().unwrap();
    assert_eq!(st.job_id, first.job_id);
    assert_eq!(st.started_ms, first.started_ms);
    assert_ne!(st.code_id, first.code_id);
    assert!(st.problem.is_none());
    assert!(st.progress.files_done >= copied, "the copy is kept");
    // the same code again changes nothing
    let (d, k, a, c) = (data.clone(), keys.clone(), address(&o), code);
    blocking(move || pull::begin(&d, &k, &a, &c, &opts())).await.unwrap();
    // a run that stops before the switch
    let mut o2 = opts();
    o2.stop_before_switch = true;
    let (d, k) = (data.clone(), keys.clone());
    let p = blocking(move || pull::run(&d, &k, &o2)).await.unwrap_err();
    assert_eq!(p.code, pull::SWITCH_NEXT);
    let st = pull::status(&data, &keys).unwrap().unwrap();
    assert_eq!(st.phase, pull::Phase::Switching);
    assert!(st.problem.is_none(), "stopping before the switch is no problem");
    assert!(!st.finishing);
    let (d, k) = (data.clone(), keys.clone());
    let done = blocking(move || pull::run(&d, &k, &opts())).await.unwrap();
    assert!(done.same_machine && !done.forced);
    let v = new_vault(&data, &keys);
    let note = pull::moved_in(&data, &v).unwrap();
    assert_eq!(note.job_id, first.job_id);
    assert_eq!(note.started_ms, first.started_ms);
    assert!(note.same_machine);
    assert_eq!(note.cert_sha256, o.fingerprint);
    // the old server: moved
    let st = moving::old::status(&old.data, &old.vault).unwrap();
    assert!(st.moved.is_some());
    assert!(st.last_undone_ms.is_none());
    let before = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
    moving::old::undo(&old.data, &old.vault).unwrap().unwrap();
    let st = moving::old::status(&old.data, &old.vault).unwrap();
    assert!(st.last_undone_ms.is_some_and(|t| t >= before));
    o.stop();
}
