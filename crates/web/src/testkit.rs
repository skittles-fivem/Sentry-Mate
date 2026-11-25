#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]
//! A real server for tests (feature `testkit`): a temporary data folder, a vault, accounts with fast hashing, the
//! server's TLS certificate (`ingest::server::Identity`), [`crate::router`] served by [`crate::serve`] on
//! 127.0.0.1, and an HTTPS client that keeps the session cookie and sends the CSRF header on changes.
//!
//! ```ignore
//! let s = TestServer::start(&[1, 2]).await;           // cars c1 and c2 exist
//! let mut admin = s.admin().await;                       // the first admin, set up and signed in
//! let mut watch = s.watch(&mut admin, &[1]).await;       // a watch account that sees c1 only
//! let r = watch.get("/api/v1/cars/c2/drives").await;
//! assert_eq!((r.status, r.code()), (404, Some("no_such_car".into())));
//! ```

use crate::{AppState, Builder, FixedCars};
use model::CarId;
use rustls::pki_types::ServerName;
use serde_json::{Value, json};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub const ADMIN: &str = "owner";
pub const ADMIN_PASSWORD: &str = "correct horse battery staple";
pub const WATCH: &str = "family";
pub const WATCH_PASSWORD: &str = "tulips grow quietly in april";

pub struct TestServer {
    pub app: AppState,
    pub addr: SocketAddr,
    pub tls: Arc<rustls::ClientConfig>,
    pub setup_code: String,
    /// The server's clock (ms since 1970): starts at the real time, moves only with [`TestServer::advance`].
    pub clock_ms: Arc<std::sync::atomic::AtomicU64>,
    pub dir: tempfile::TempDir,
    task: tokio::task::JoinHandle<std::io::Result<()>>,
}

/// What [`TestServer::assemble`] made.
struct Assembled {
    app: AppState,
    addr: SocketAddr,
    tls: Arc<rustls::ClientConfig>,
    setup_code: String,
    task: tokio::task::JoinHandle<std::io::Result<()>>,
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl TestServer {
    /// A server where the cars `cars` exist.
    pub async fn start(cars: &[CarId]) -> TestServer {
        let cars: std::collections::HashSet<CarId> = cars.iter().copied().collect();
        TestServer::start_with(move |b| b.cars(Arc::new(FixedCars(cars)))).await
    }

    /// A server whose state `f` finishes building (cars, services...).
    pub async fn start_with(f: impl FnOnce(Builder) -> Builder) -> TestServer {
        TestServer::start_full(f, axum::Router::new()).await
    }

    /// The same, with extra routes behind the guard (a resource module's `routes()`, or a probe in a test).
    pub async fn start_full(f: impl FnOnce(Builder) -> Builder, routes: axum::Router<AppState>) -> TestServer {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("temp dir: {e}"));
        let (data, keys) = (dir.path().join("data"), dir.path().join("keys"));
        std::fs::create_dir_all(&data).unwrap_or_else(|e| panic!("{e}"));
        let (v, _) = vault::Vault::create(&data, &keys, vault::Kdf::FAST_FOR_TESTS).unwrap_or_else(|e| panic!("{e}"));
        let clock_ms = Arc::new(std::sync::atomic::AtomicU64::new((accounts::system_clock())()));
        let up = TestServer::assemble(dir.path(), Arc::new(v), clock_ms.clone(), f, routes, "127.0.0.1:0").await;
        TestServer { app: up.app, addr: up.addr, tls: up.tls, setup_code: up.setup_code, clock_ms, dir, task: up.task }
    }

    /// Builds the state over the folders in `root` (`data`, `keys`), serves it on `bind`.
    async fn assemble(
        root: &std::path::Path,
        v: Arc<vault::Vault>,
        clock_ms: Arc<std::sync::atomic::AtomicU64>,
        f: impl FnOnce(Builder) -> Builder,
        routes: axum::Router<AppState>,
        bind: &str,
    ) -> Assembled {
        let (data, keys) = (root.join("data"), root.join("keys"));
        let c2 = clock_ms.clone();
        let clock: accounts::Clock = Arc::new(move || c2.load(std::sync::atomic::Ordering::Relaxed));
        let audit = Arc::new(
            accounts::Audit::open(data.join("audit"), v.clone(), clock.clone()).unwrap_or_else(|e| panic!("{e}")),
        );
        let options = accounts::Options {
            hashing: accounts::Hashing::FAST_FOR_TESTS,
            clock: clock.clone(),
            ..accounts::Options::default()
        };
        let acc = Arc::new(
            accounts::Accounts::open(data.join("accounts"), v.clone(), audit, options)
                .unwrap_or_else(|e| panic!("{e}")),
        );
        // the release check never reaches GitHub from a test: a stand-in that answers nothing, unless `f` gives one
        let b = AppState::builder(acc, v.clone(), data.join("web"))
            .setup_code_file(keys.join("SETUP-CODE.txt"))
            .clock(clock.clone())
            .release_check(Arc::new(FakeGitHub::default()), release::Build::local());
        let app = f(b).build().unwrap_or_else(|e| panic!("{e}"));
        let setup_code = app.announce_setup().unwrap_or_else(|e| panic!("{e}")).unwrap_or_default();
        let id = ingest::server::Identity::load_or_create(&data.join("tls"), &[], &v).unwrap_or_else(|e| panic!("{e}"));
        let server_tls = id.tls_config().unwrap_or_else(|e| panic!("{e}"));
        let mut roots = rustls::RootCertStore::empty();
        for c in &id.chain {
            roots.add(c.clone()).unwrap_or_else(|e| panic!("{e}"));
        }
        let tls = Arc::new(
            rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_protocol_versions(&[&rustls::version::TLS13])
                .unwrap_or_else(|e| panic!("{e}"))
                .with_root_certificates(roots)
                .with_no_client_auth(),
        );
        let listener = tokio::net::TcpListener::bind(bind).await.unwrap_or_else(|e| panic!("{e}"));
        let addr = listener.local_addr().unwrap_or_else(|e| panic!("{e}"));
        let task = tokio::spawn(crate::serve(listener, server_tls, crate::router_with(app.clone(), routes)));
        Assembled { app, addr, tls, setup_code, task }
    }

    /// Stops the server and starts it again over the same folders, the vault, accounts and files reopened, the clock
    /// and the address kept (what a restart of the program does). `f` builds the state again; nothing of the old one
    /// is kept in memory.
    pub async fn restart(&mut self, f: impl FnOnce(Builder) -> Builder) {
        self.restart_full(f, axum::Router::new()).await;
    }

    /// [`TestServer::restart`], with extra routes.
    pub async fn restart_full(&mut self, f: impl FnOnce(Builder) -> Builder, routes: axum::Router<AppState>) {
        self.task.abort();
        let _ = (&mut self.task).await;
        let (data, keys) = (self.dir.path().join("data"), self.dir.path().join("keys"));
        let v = vault::Vault::open(&data, &keys).unwrap_or_else(|e| panic!("{e}"));
        let up = TestServer::assemble(
            self.dir.path(),
            Arc::new(v),
            self.clock_ms.clone(),
            f,
            routes,
            &self.addr.to_string(),
        )
        .await;
        // the old state goes now, with what it held (a snapshot's lock)
        self.app = up.app;
        self.tls = up.tls;
        self.setup_code = up.setup_code;
        self.task = up.task;
    }

    /// Moves the server's clock on.
    pub fn advance(&self, ms: u64) {
        self.clock_ms.fetch_add(ms, std::sync::atomic::Ordering::Relaxed);
    }

    /// A client with no session.
    pub fn client(&self) -> Client {
        Client { addr: self.addr, tls: self.tls.clone(), cookies: Vec::new() }
    }

    /// The first admin ([`ADMIN`]), set up with the setup code and signed in.
    pub async fn admin(&self) -> Client {
        let mut c = self.client();
        let r = c
            .post("/api/v1/setup", json!({ "code": self.setup_code, "name": ADMIN, "password": ADMIN_PASSWORD }))
            .await;
        assert_eq!(r.status, 200, "setup: {}", r.text());
        c
    }

    /// A new session of the first admin ([`ADMIN`]) on a server that already has it (after a restart, say).
    pub async fn sign_in_admin(&self) -> Client {
        let mut c = self.client();
        let r = c.post("/api/v1/session", json!({ "name": ADMIN, "password": ADMIN_PASSWORD })).await;
        assert_eq!(r.status, 200, "sign-in: {}", r.text());
        c
    }

    /// A watch account ([`WATCH`]) that sees `cars`, made by `admin`, signed in.
    pub async fn watch(&self, admin: &mut Client, cars: &[CarId]) -> Client {
        let list: Vec<String> = cars.iter().map(|c| crate::ids::car(*c)).collect();
        let r = admin
            .post(
                "/api/v1/accounts",
                json!({ "name": WATCH, "password": WATCH_PASSWORD, "role": "watch", "cars": list,
                        "confirm_password": ADMIN_PASSWORD }),
            )
            .await;
        assert_eq!(r.status, 201, "new account: {}", r.text());
        let mut c = self.client();
        let r = c.post("/api/v1/session", json!({ "name": WATCH, "password": WATCH_PASSWORD })).await;
        assert_eq!(r.status, 200, "sign-in: {}", r.text());
        c
    }
}

/// A stand-in for GitHub's API for the release check: the answers it is given by path (with the query), every path
/// asked kept; a path with no answer is "could not connect".
#[derive(Default)]
pub struct FakeGitHub {
    pub answers: std::sync::Mutex<std::collections::HashMap<String, release::Answer>>,
    pub asked: std::sync::Mutex<Vec<String>>,
}

impl FakeGitHub {
    /// Answers `path` with `200` and `body`.
    pub fn answer(&self, path: &str, body: Value) {
        let a = release::Answer { status: 200, body: body.to_string().into(), ..release::Answer::default() };
        self.answers.lock().unwrap().insert(path.to_string(), a);
    }

    pub fn asked(&self) -> Vec<String> {
        self.asked.lock().unwrap().clone()
    }
}

impl release::Fetch for FakeGitHub {
    fn get<'a>(&'a self, path: &'a str, _if_none_match: Option<&'a str>) -> release::http::FetchFuture<'a> {
        self.asked.lock().unwrap().push(path.to_string());
        let a = self.answers.lock().unwrap().get(path).cloned();
        Box::pin(async move { a.ok_or(release::HttpError::Connect) })
    }
}

/// An answer.
#[derive(Debug, Clone)]
pub struct Res {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Res {
    /// The first header of that name (any case).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// `error.code` of an error answer.
    pub fn code(&self) -> Option<String> {
        self.json()["error"]["code"].as_str().map(str::to_string)
    }
}

/// An HTTPS client of the test server: one connection per request, the cookies it was given kept.
#[derive(Clone)]
pub struct Client {
    addr: SocketAddr,
    tls: Arc<rustls::ClientConfig>,
    pub cookies: Vec<(String, String)>,
}

fn dechunk(mut b: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    while let Some(nl) = b.windows(2).position(|w| w == b"\r\n") {
        let size = usize::from_str_radix(String::from_utf8_lossy(&b[..nl]).split(';').next().unwrap_or("").trim(), 16)
            .unwrap_or(0);
        if size == 0 || b.len() < nl + 2 + size {
            break;
        }
        out.extend_from_slice(&b[nl + 2..nl + 2 + size]);
        b = &b[(nl + 4 + size).min(b.len())..];
    }
    out
}

impl Client {
    /// Sends one request; the CSRF header goes with every change unless `headers` names it.
    pub async fn request(&mut self, method: &str, path: &str, body: Option<&[u8]>, headers: &[(&str, &str)]) -> Res {
        let tcp = tokio::net::TcpStream::connect(self.addr).await.unwrap_or_else(|e| panic!("connect: {e}"));
        let name = ServerName::try_from("localhost").unwrap_or_else(|e| panic!("{e}"));
        let mut tls = tokio_rustls::TlsConnector::from(self.tls.clone())
            .connect(name, tcp)
            .await
            .unwrap_or_else(|e| panic!("TLS: {e}"));
        let mut req = format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n");
        if method != "GET" && method != "HEAD" && !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("x-sentry-mate"))
        {
            req.push_str("X-Sentry-Mate: 1\r\n");
        }
        if !self.cookies.is_empty() {
            let c: Vec<String> = self.cookies.iter().map(|(k, v)| format!("{k}={v}")).collect();
            req.push_str(&format!("Cookie: {}\r\n", c.join("; ")));
        }
        for (k, v) in headers {
            req.push_str(&format!("{k}: {v}\r\n"));
        }
        let body = body.unwrap_or(&[]);
        if method != "GET" && method != "HEAD" {
            req.push_str(&format!("Content-Length: {}\r\n", body.len()));
        }
        req.push_str("\r\n");
        tls.write_all(req.as_bytes()).await.unwrap_or_else(|e| panic!("write: {e}"));
        tls.write_all(body).await.unwrap_or_else(|e| panic!("write: {e}"));
        let mut raw = Vec::new();
        let _ = tls.read_to_end(&mut raw).await;
        let split = raw.windows(4).position(|w| w == b"\r\n\r\n").unwrap_or(raw.len());
        let head = String::from_utf8_lossy(&raw[..split]).into_owned();
        let mut lines = head.split("\r\n");
        let status = lines.next().and_then(|l| l.split(' ').nth(1)).and_then(|s| s.parse().ok()).unwrap_or(0);
        let headers: Vec<(String, String)> = lines
            .filter_map(|l| l.split_once(':'))
            .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
            .collect();
        let rest = raw.get(split + 4..).unwrap_or(&[]).to_vec();
        let chunked = headers.iter().any(|(k, v)| k.eq_ignore_ascii_case("transfer-encoding") && v.contains("chunked"));
        let body = if chunked { dechunk(&rest) } else { rest };
        for (k, v) in &headers {
            if k.eq_ignore_ascii_case("set-cookie") {
                let pair = v.split(';').next().unwrap_or("");
                if let Some((name, value)) = pair.split_once('=') {
                    self.cookies.retain(|(n, _)| n != name);
                    let cleared = v.contains("Max-Age=0");
                    if !cleared && !value.is_empty() {
                        self.cookies.push((name.to_string(), value.to_string()));
                    }
                }
            }
        }
        Res { status, headers, body }
    }

    pub async fn get(&mut self, path: &str) -> Res {
        self.request("GET", path, None, &[]).await
    }

    async fn send_json(&mut self, method: &str, path: &str, body: Value) -> Res {
        let b = serde_json::to_vec(&body).unwrap_or_default();
        self.request(method, path, Some(&b), &[("Content-Type", "application/json")]).await
    }

    pub async fn post(&mut self, path: &str, body: Value) -> Res {
        self.send_json("POST", path, body).await
    }

    pub async fn put(&mut self, path: &str, body: Value) -> Res {
        self.send_json("PUT", path, body).await
    }

    pub async fn patch(&mut self, path: &str, body: Value) -> Res {
        self.send_json("PATCH", path, body).await
    }

    pub async fn delete(&mut self, path: &str, body: Value) -> Res {
        self.send_json("DELETE", path, body).await
    }

    /// Opens the live stream and reads it for `ms` milliseconds; the frames read.
    pub async fn events_for(&mut self, ms: u64) -> String {
        let tcp = tokio::net::TcpStream::connect(self.addr).await.unwrap_or_else(|e| panic!("connect: {e}"));
        let name = ServerName::try_from("localhost").unwrap_or_else(|e| panic!("{e}"));
        let mut tls = tokio_rustls::TlsConnector::from(self.tls.clone())
            .connect(name, tcp)
            .await
            .unwrap_or_else(|e| panic!("TLS: {e}"));
        let c: Vec<String> = self.cookies.iter().map(|(k, v)| format!("{k}={v}")).collect();
        let req = format!("GET /api/v1/events HTTP/1.1\r\nHost: localhost\r\nCookie: {}\r\n\r\n", c.join("; "));
        tls.write_all(req.as_bytes()).await.unwrap_or_else(|e| panic!("write: {e}"));
        let mut raw = Vec::new();
        let _ = tokio::time::timeout(std::time::Duration::from_millis(ms), async {
            let mut buf = [0u8; 4096];
            while let Ok(n) = tls.read(&mut buf).await {
                if n == 0 {
                    break;
                }
                raw.extend_from_slice(&buf[..n]);
            }
        })
        .await;
        String::from_utf8_lossy(&raw).into_owned()
    }
}
