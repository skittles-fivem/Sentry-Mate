//! A fake Tesla on this machine: the token endpoint (with single-use refresh tokens and PKCE), the product list
//! and vehicle data over HTTP/2 and TLS 1.3, and a streaming websocket. Every request is recorded. All tokens,
//! VINs and positions are made up.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use futures_util::{SinkExt, StreamExt};
use http_body_util::{BodyExt, Full};
use hyper::body::{Bytes, Incoming};
use hyper::{Request, Response};
use hyper_util::rt::{TokioExecutor, TokioIo};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;
use tokio_tungstenite::tungstenite::Message;

pub const VIN_A: &str = "5YJ3E1EA0PF000001";
pub const VIN_B: &str = "7SAYGDEE0PF000002";

#[derive(Debug, Clone)]
pub struct Seen {
    pub method: String,
    pub path: String,
    pub query: String,
    pub bearer: Option<String>,
    pub body: String,
    pub user_agent: String,
    pub version: String,
}

/// A canned answer for the next request whose path starts with `prefix`.
pub struct Canned {
    pub prefix: &'static str,
    pub status: u16,
    pub headers: Vec<(&'static str, String)>,
    pub body: String,
}

#[derive(Default)]
pub struct State {
    pub seen: Vec<Seen>,
    /// Refresh tokens Tesla would take now (each once).
    pub refresh: Vec<String>,
    /// Access tokens Tesla takes.
    pub access: Vec<String>,
    pub issued: u32,
    /// Leave the refresh token out of refresh answers.
    pub omit_refresh: bool,
    pub expires_in: u64,
    /// The sign-in code Tesla would take, and the PKCE challenge it was given for.
    pub code: Option<(String, String)>,
    pub products: Value,
    pub data: HashMap<String, Value>,
    pub canned: VecDeque<Canned>,
}

pub struct Fake {
    pub port: u16,
    pub stream_port: u16,
    pub pem: String,
    pub state: Arc<Mutex<State>>,
    pub stream_script: Arc<Mutex<Vec<StreamStep>>>,
    pub subscribes: Arc<Mutex<Vec<Value>>>,
}

#[derive(Debug, Clone)]
pub enum StreamStep {
    /// Send this JSON as a text message.
    Send(Value),
    /// Wait for the next message from the client.
    Expect,
    Sleep(u64),
    Close,
}

pub fn products() -> Value {
    json!([
        {"id": 1001, "id_s": "1001", "vehicle_id": 501, "vin": VIN_A, "display_name": "Made-up three",
         "state": "online"},
        {"energy_site_id": 77, "resource_type": "battery"},
        {"id": 1002, "id_s": "1002", "vehicle_id": 502, "vin": VIN_B, "display_name": "Made-up why",
         "state": "asleep"}
    ])
}

pub fn parked_data() -> Value {
    json!({
        "id_s": "1001", "state": "online", "display_name": "Made-up three",
        "drive_state": {"timestamp": 1_800_000_000_000u64, "shift_state": null, "speed": null, "power": 0,
                        "latitude": 12.5, "longitude": -45.25, "heading": 10},
        "charge_state": {"battery_level": 70, "battery_range": 200, "charging_state": "Disconnected"},
        "climate_state": {"inside_temp": 20, "outside_temp": 10, "is_climate_on": false},
        "vehicle_state": {"odometer": 1000, "car_version": "2026.32.5 0123456789ab", "sentry_mode": false},
        "vehicle_config": {"car_type": "model3"}
    })
}

fn certs() -> (String, Vec<CertificateDer<'static>>, PrivateKeyDer<'static>) {
    let ck = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
    let pem = ck.cert.pem();
    let der = ck.cert.der().clone();
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(ck.key_pair.serialize_der()));
    (pem, vec![der], key)
}

fn server_config(
    certs: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
    alpn: &[u8],
) -> rustls::ServerConfig {
    // An empty `alpn`: the server names no protocol at all (an HTTP/1.1-only server).
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut cfg = rustls::ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .unwrap();
    if !alpn.is_empty() {
        cfg.alpn_protocols = vec![alpn.to_vec()];
    }
    cfg
}

fn form(body: &str) -> HashMap<String, String> {
    body.split('&')
        .filter_map(|p| p.split_once('='))
        .map(|(k, v)| (k.to_string(), percent_encoding::percent_decode_str(v).decode_utf8_lossy().into_owned()))
        .collect()
}

fn reply(status: u16, headers: &[(&'static str, String)], body: String) -> Response<Full<Bytes>> {
    let mut r = Response::builder().status(status);
    for (k, v) in headers {
        r = r.header(*k, v);
    }
    r.body(Full::new(Bytes::from(body))).unwrap()
}

fn handle(st: &Arc<Mutex<State>>, seen: Seen) -> Response<Full<Bytes>> {
    let mut s = st.lock().unwrap();
    s.seen.push(seen.clone());
    if let Some(i) = s.canned.iter().position(|c| seen.path.starts_with(c.prefix)) {
        let c = s.canned.remove(i).unwrap();
        return reply(c.status, &c.headers, c.body);
    }
    if seen.path == "/oauth2/v3/token" && seen.method == "POST" {
        let f = form(&seen.body);
        if f.get("client_id").map(String::as_str) != Some("ownerapi") {
            return reply(400, &[], json!({"error": "invalid_client"}).to_string());
        }
        let ok = match f.get("grant_type").map(String::as_str) {
            Some("refresh_token") => {
                let t = f.get("refresh_token").cloned().unwrap_or_default();
                let scope_ok = f.get("scope").map(String::as_str) == Some("openid email offline_access");
                match s.refresh.iter().position(|r| *r == t) {
                    Some(i) if scope_ok => {
                        s.refresh.remove(i);
                        true
                    }
                    _ => false,
                }
            }
            Some("authorization_code") => match s.code.clone() {
                Some((code, challenge)) => {
                    let v = f.get("code_verifier").cloned().unwrap_or_default();
                    let ok = f.get("code") == Some(&code)
                        && tesla::auth::challenge(&v) == challenge
                        && f.get("redirect_uri").map(String::as_str) == Some("tesla://auth/callback");
                    if ok {
                        s.code = None;
                    }
                    ok
                }
                None => false,
            },
            _ => false,
        };
        if !ok {
            return reply(401, &[], json!({"error": "login_required"}).to_string());
        }
        s.issued += 1;
        let n = s.issued;
        let (access, refresh) = (format!("fake-access-{n}"), format!("fake-refresh-{n}"));
        s.access.push(access.clone());
        let mut body = json!({"access_token": access, "id_token": "fake-id", "token_type": "Bearer",
                              "expires_in": if s.expires_in == 0 { 28_800 } else { s.expires_in }});
        if !s.omit_refresh {
            s.refresh.push(refresh.clone());
            body["refresh_token"] = json!(refresh);
        }
        return reply(200, &[], body.to_string());
    }
    let authorized = seen.bearer.as_ref().is_some_and(|b| s.access.contains(b));
    if seen.method != "GET" {
        return reply(405, &[], String::new());
    }
    if !authorized {
        return reply(401, &[], json!({"error": "invalid bearer token"}).to_string());
    }
    if seen.path == "/api/1/products" {
        let list = s.products.clone();
        let n = list.as_array().map_or(0, Vec::len);
        return reply(200, &[], json!({"response": list, "count": n}).to_string());
    }
    if let Some(rest) = seen.path.strip_prefix("/api/1/vehicles/")
        && let Some(id) = rest.strip_suffix("/vehicle_data")
    {
        return match s.data.get(id) {
            Some(v) => reply(200, &[], json!({"response": v}).to_string()),
            None => reply(408, &[], json!({"response": null, "error": "vehicle unavailable"}).to_string()),
        };
    }
    reply(404, &[], json!({"response": null, "error": "not_found"}).to_string())
}

impl Fake {
    /// Starts the fake; `h2`: false makes the REST port speak HTTP/1.1 only (the client must refuse it).
    pub async fn start(h2: bool) -> Fake {
        let (pem, certs, key) = certs();
        let state = Arc::new(Mutex::new(State {
            products: products(),
            data: HashMap::from([("1001".to_string(), parked_data())]),
            ..State::default()
        }));
        let alpn: &[u8] = if h2 { b"h2" } else { b"" };
        let acceptor = TlsAcceptor::from(Arc::new(server_config(certs.clone(), key.clone_key(), alpn)));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let st = state.clone();
        tokio::spawn(async move {
            loop {
                let Ok((tcp, _)) = listener.accept().await else { return };
                let acceptor = acceptor.clone();
                let st = st.clone();
                tokio::spawn(async move {
                    let Ok(tls) = acceptor.accept(tcp).await else { return };
                    if !h2 {
                        let _ = hyper::server::conn::http1::Builder::new()
                            .serve_connection(
                                TokioIo::new(tls),
                                hyper::service::service_fn(|_r: Request<Incoming>| async {
                                    Ok::<_, std::convert::Infallible>(reply(200, &[], "{}".into()))
                                }),
                            )
                            .await;
                        return;
                    }
                    let svc = hyper::service::service_fn(move |r: Request<Incoming>| {
                        let st = st.clone();
                        async move {
                            let method = r.method().to_string();
                            let path = r.uri().path().to_string();
                            let query = r.uri().query().unwrap_or_default().to_string();
                            let version = format!("{:?}", r.version());
                            let header = |n: &str| r.headers().get(n).and_then(|v| v.to_str().ok()).map(str::to_string);
                            let bearer =
                                header("authorization").and_then(|a| a.strip_prefix("Bearer ").map(str::to_string));
                            let user_agent = header("user-agent").unwrap_or_default();
                            let body = r.into_body().collect().await.map(|b| b.to_bytes()).unwrap_or_default();
                            let body = String::from_utf8_lossy(&body).into_owned();
                            Ok::<_, std::convert::Infallible>(handle(
                                &st,
                                Seen { method, path, query, bearer, body, user_agent, version },
                            ))
                        }
                    });
                    let _ = hyper::server::conn::http2::Builder::new(TokioExecutor::new())
                        .serve_connection(TokioIo::new(tls), svc)
                        .await;
                });
            }
        });

        // Streaming: HTTP/1.1 upgrade to a websocket, then the script.
        let stream_script: Arc<Mutex<Vec<StreamStep>>> = Arc::new(Mutex::new(Vec::new()));
        let subscribes: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
        let ws_acceptor = TlsAcceptor::from(Arc::new(server_config(certs, key, b"http/1.1")));
        let ws_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let stream_port = ws_listener.local_addr().unwrap().port();
        let (script, subs) = (stream_script.clone(), subscribes.clone());
        tokio::spawn(async move {
            loop {
                let Ok((tcp, _)) = ws_listener.accept().await else { return };
                let (acceptor, script, subs) = (ws_acceptor.clone(), script.clone(), subs.clone());
                tokio::spawn(async move {
                    let Ok(tls) = acceptor.accept(tcp).await else { return };
                    let Ok(mut ws) = tokio_tungstenite::accept_async(tls).await else { return };
                    let steps = script.lock().unwrap().clone();
                    for step in steps {
                        match step {
                            StreamStep::Send(v) => {
                                if ws.send(Message::text(v.to_string())).await.is_err() {
                                    return;
                                }
                            }
                            StreamStep::Expect => match ws.next().await {
                                Some(Ok(Message::Text(t))) => {
                                    subs.lock().unwrap().push(serde_json::from_str(&t).unwrap_or(Value::Null));
                                }
                                _ => return,
                            },
                            StreamStep::Sleep(ms) => tokio::time::sleep(std::time::Duration::from_millis(ms)).await,
                            StreamStep::Close => {
                                let _ = ws.close(None).await;
                                return;
                            }
                        }
                    }
                    // Hold the socket open until the client goes.
                    while let Some(Ok(_)) = ws.next().await {}
                });
            }
        });
        Fake { port, stream_port, pem, state, stream_script, subscribes }
    }

    pub fn endpoints(&self) -> tesla::Endpoints {
        tesla::Endpoints::fake("localhost", self.port, Some(self.stream_port), &self.pem)
    }

    pub fn seen(&self) -> Vec<Seen> {
        self.state.lock().unwrap().seen.clone()
    }

    pub fn add_refresh(&self, t: &str) {
        self.state.lock().unwrap().refresh.push(t.to_string());
    }

    pub fn can(&self, prefix: &'static str, status: u16, headers: Vec<(&'static str, String)>, body: String) {
        self.state.lock().unwrap().canned.push_back(Canned { prefix, status, headers, body });
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

/// What the sink was given in one call: the car, samples, facts.
pub type Points = (u32, Vec<model::Sample>, Vec<tesla::ApiFact>);

/// A sink that keeps what it is given.
#[derive(Default)]
pub struct MemSink {
    pub points: Mutex<Vec<Points>>,
    pub metas: Mutex<Vec<(u32, tesla::ApiMeta)>>,
    pub new_cars: Mutex<Vec<tesla::NewCar>>,
    pub changed: Mutex<Vec<tesla::AccountState>>,
}

impl tesla::Sink for MemSink {
    fn new_car(&self, car: &tesla::NewCar) -> Result<u32, tesla::SinkError> {
        let mut n = self.new_cars.lock().unwrap();
        n.push(car.clone());
        Ok(100 + u32::try_from(n.len()).unwrap())
    }
    fn points(&self, car: u32, samples: &[model::Sample], facts: &[tesla::ApiFact]) -> Result<(), tesla::SinkError> {
        self.points.lock().unwrap().push((car, samples.to_vec(), facts.to_vec()));
        Ok(())
    }
    fn meta(&self, car: u32, meta: &tesla::ApiMeta) -> Result<(), tesla::SinkError> {
        self.metas.lock().unwrap().push((car, meta.clone()));
        Ok(())
    }
    fn account_changed(&self, account: &tesla::Account) {
        self.changed.lock().unwrap().push(account.state);
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
