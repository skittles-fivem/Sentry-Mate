//! A fake TeslaFi on this machine: `feed.php` over TLS (1.3, or 1.2 only) and HTTP/2 (or HTTP/1.1 only), answering
//! as TeslaFi does (`text/html` whatever the body, `200` with an `unauthorized` envelope for a token it does not
//! take). Every request is recorded. All tokens, VINs and positions are made up.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use http_body_util::Full;
use hyper::body::{Bytes, Incoming};
use hyper::{Request, Response};
use hyper_util::rt::{TokioExecutor, TokioIo};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

pub const TOKEN: &str = "0000madeup0000aaaabbbbccccdddd";
pub const VIN: &str = "5YJ3E1EA0PF000123";

#[derive(Debug, Clone)]
pub struct Seen {
    pub method: String,
    pub path: String,
    pub query: String,
    pub authorization: Option<String>,
    pub user_agent: String,
    pub version: String,
    pub at: std::time::Instant,
}

/// A canned answer: status, headers, body.
pub type Canned = (u16, Vec<(&'static str, String)>, String);

#[derive(Default)]
pub struct State {
    pub seen: Vec<Seen>,
    pub tokens: Vec<String>,
    /// The answer to the latest read (no command) and to `lastGood`.
    pub latest: Value,
    pub last_good: Value,
    /// Answers given first, whatever is asked: status, headers, body.
    pub canned: VecDeque<Canned>,
}

pub struct Fake {
    pub port: u16,
    pub pem: String,
    pub state: Arc<Mutex<State>>,
}

pub fn driving() -> Value {
    json!({
        "data_id": "1001", "Date": "2026-07-04 14:05:09", "state": "online", "carState": "Driving",
        "shift_state": "D", "speed": "50", "latitude": "12.5", "longitude": "-45.25", "odometer": "10000.0",
        "battery_level": "64", "battery_range": "200.0", "inside_temp": "21.5", "outside_temp": null,
        "charging_state": "Disconnected", "car_version": "2026.32.5 abcdef", "vin": VIN,
        "display_name": "Made-up car", "tesla_request_counter": {"commands": 0, "wakes": 0}
    })
}

pub fn sleeping() -> Value {
    json!({"data_id": "1002", "Date": "2026-07-04 15:00:00", "state": "asleep", "carState": "Sleeping",
           "vin": null, "battery_level": null})
}

fn certs() -> (String, Vec<CertificateDer<'static>>, PrivateKeyDer<'static>) {
    let ck = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
    let pem = ck.cert.pem();
    let der = ck.cert.der().clone();
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(ck.key_pair.serialize_der()));
    (pem, vec![der], key)
}

fn reply(status: u16, headers: &[(&'static str, String)], body: String) -> Response<Full<Bytes>> {
    let mut r = Response::builder().status(status).header("content-type", "text/html; charset=UTF-8");
    for (k, v) in headers {
        r = r.header(*k, v);
    }
    r.body(Full::new(Bytes::from(body))).unwrap()
}

fn handle(st: &Arc<Mutex<State>>, seen: Seen) -> Response<Full<Bytes>> {
    let mut s = st.lock().unwrap();
    s.seen.push(seen.clone());
    if let Some((status, headers, body)) = s.canned.pop_front() {
        return reply(status, &headers, body);
    }
    if seen.path != "/feed.php" || seen.method != "GET" {
        return reply(404, &[], "<html>not found</html>".into());
    }
    let Some(token) = seen.authorization.as_deref().and_then(|a| a.strip_prefix("Bearer ")) else {
        return reply(
            401,
            &[],
            json!({"success": false, "error": {"message": "Unauthorized", "code": 401}}).to_string(),
        );
    };
    if !s.tokens.iter().any(|t| t == token) {
        return reply(200, &[], json!({"response": {"reason": "", "result": "unauthorized"}}).to_string());
    }
    match seen.query.as_str() {
        "" => reply(200, &[], s.latest.to_string()),
        "command=lastGood" => reply(200, &[], s.last_good.to_string()),
        _ => reply(200, &[], "This command is not enabled for API use.".into()),
    }
}

impl Fake {
    /// `tls13`: offer TLS 1.3 (else 1.2 only); `h2`: offer HTTP/2 (else HTTP/1.1 only).
    pub async fn start(tls13: bool, h2: bool) -> Fake {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let (pem, certs, key) = certs();
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let versions: &[&rustls::SupportedProtocolVersion] =
            if tls13 { &[&rustls::version::TLS13] } else { &[&rustls::version::TLS12] };
        let mut cfg = rustls::ServerConfig::builder_with_provider(provider)
            .with_protocol_versions(versions)
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .unwrap();
        cfg.alpn_protocols = if h2 { vec![b"h2".to_vec()] } else { vec![b"http/1.1".to_vec()] };
        let acceptor = TlsAcceptor::from(Arc::new(cfg));
        let state = Arc::new(Mutex::new(State {
            tokens: vec![TOKEN.to_string()],
            latest: driving(),
            last_good: driving(),
            ..State::default()
        }));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let st = state.clone();
        tokio::spawn(async move {
            loop {
                let Ok((tcp, _)) = listener.accept().await else { return };
                let (acceptor, st) = (acceptor.clone(), st.clone());
                tokio::spawn(async move {
                    let Ok(tls) = acceptor.accept(tcp).await else { return };
                    let svc = hyper::service::service_fn(move |r: Request<Incoming>| {
                        let st = st.clone();
                        async move {
                            let header = |n: &str| r.headers().get(n).and_then(|v| v.to_str().ok()).map(str::to_string);
                            let seen = Seen {
                                method: r.method().to_string(),
                                path: r.uri().path().to_string(),
                                query: r.uri().query().unwrap_or_default().to_string(),
                                authorization: header("authorization"),
                                user_agent: header("user-agent").unwrap_or_default(),
                                version: format!("{:?}", r.version()),
                                at: std::time::Instant::now(),
                            };
                            Ok::<_, std::convert::Infallible>(handle(&st, seen))
                        }
                    });
                    if h2 {
                        let _ = hyper::server::conn::http2::Builder::new(TokioExecutor::new())
                            .serve_connection(TokioIo::new(tls), svc)
                            .await;
                    } else {
                        let _ =
                            hyper::server::conn::http1::Builder::new().serve_connection(TokioIo::new(tls), svc).await;
                    }
                });
            }
        });
        Fake { port, pem, state }
    }

    pub fn endpoint(&self) -> teslafi::Endpoint {
        teslafi::Endpoint::fake("localhost", self.port, &self.pem)
    }

    pub fn seen(&self) -> Vec<Seen> {
        self.state.lock().unwrap().seen.clone()
    }

    pub fn can(&self, status: u16, headers: Vec<(&'static str, String)>, body: String) {
        self.state.lock().unwrap().canned.push_back((status, headers, body));
    }

    pub fn set(&self, latest: Value, last_good: Value) {
        let mut s = self.state.lock().unwrap();
        s.latest = latest;
        s.last_good = last_good;
    }
}

/// A vault in a temporary folder.
pub fn vault() -> (tempfile::TempDir, std::path::PathBuf, Arc<vault::Vault>) {
    let t = tempfile::tempdir().unwrap();
    let data = t.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let (v, _) = vault::Vault::create(&data, &t.path().join("keys"), vault::Kdf::FAST_FOR_TESTS).unwrap();
    (t, data, Arc::new(v))
}

pub type Points = (u32, Vec<model::Sample>, Vec<tesla::ApiFact>);

/// A sink that keeps what it is given.
#[derive(Default)]
pub struct MemSink {
    pub points: Mutex<Vec<Points>>,
    pub new_cars: Mutex<Vec<tesla::NewCar>>,
    pub changed: Mutex<Vec<teslafi::AccountState>>,
    pub off_can: Mutex<Vec<(u32, u64, model::OffCan)>>,
    /// The accounts TeslaFi began limiting, with the end of the wait.
    pub limited: Mutex<Vec<(String, u64)>>,
    /// The cars whose stored data was dated again: the car, the old zone, the new one.
    pub rezoned: Mutex<Vec<(u32, String, String)>>,
    /// Dating stored data again fails while this is set.
    pub rezone_fails: Mutex<bool>,
}

impl teslafi::Sink for MemSink {
    fn new_car(&self, car: &tesla::NewCar) -> Result<u32, tesla::SinkError> {
        let mut n = self.new_cars.lock().unwrap();
        n.push(car.clone());
        Ok(100 + u32::try_from(n.len()).unwrap())
    }
    fn points(&self, car: u32, samples: &[model::Sample], facts: &[tesla::ApiFact]) -> Result<(), tesla::SinkError> {
        self.points.lock().unwrap().push((car, samples.to_vec(), facts.to_vec()));
        Ok(())
    }
    fn off_can(&self, car: u32, at_ms: u64, values: &model::OffCan) -> Result<(), tesla::SinkError> {
        self.off_can.lock().unwrap().push((car, at_ms, values.clone()));
        Ok(())
    }
    fn account_changed(&self, account: &teslafi::Account) {
        self.changed.lock().unwrap().push(account.state);
    }
    fn rate_limited(&self, account: &teslafi::Account, until_ms: u64) {
        self.limited.lock().unwrap().push((account.id.clone(), until_ms));
    }
    fn rezone(&self, _account: &teslafi::Account, car: u32, from: &str, to: &str) -> Result<u64, tesla::SinkError> {
        if *self.rezone_fails.lock().unwrap() {
            return Err(tesla::SinkError("made-up failure".into()));
        }
        self.rezoned.lock().unwrap().push((car, from.to_string(), to.to_string()));
        Ok(1)
    }
}

/// Lines written by `tracing`, kept for checking.
#[derive(Clone, Default)]
pub struct LogBuf(pub Arc<Mutex<Vec<u8>>>);

impl std::io::Write for LogBuf {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogBuf {
    type Writer = LogBuf;
    fn make_writer(&'a self) -> LogBuf {
        self.clone()
    }
}

impl LogBuf {
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}
