//! Test helpers: a local HTTPS server (its own CA, so the real TLS path with certificate checks is tested; there is
//! no internet in tests), the zip writer, and synthetic GeoNames files. Every position in them is made up.
#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used, dead_code)]

#[path = "../../src/geocode/zip_writer.rs"]
pub mod zip_writer;

use places::HttpOptions;
use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, KeyPair, KeyUsagePurpose};
use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// A request the server saw.
#[derive(Debug, Clone)]
pub struct Hit {
    pub at: Instant,
    pub path: String,
    /// Header names lower case.
    pub headers: Vec<(String, String)>,
}

pub struct Reply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// Wait this long before answering.
    pub delay: Duration,
}

impl Reply {
    pub fn ok(body: impl Into<Vec<u8>>) -> Reply {
        Reply { status: 200, headers: vec![], body: body.into(), delay: Duration::ZERO }
    }

    pub fn status(status: u16) -> Reply {
        Reply { status, headers: vec![], body: b"{}".to_vec(), delay: Duration::ZERO }
    }
}

type Handler = dyn Fn(&str) -> Reply + Send + Sync;

pub struct MockServer {
    pub url: String,
    pub ca_pem: String,
    hits: Arc<Mutex<Vec<Hit>>>,
    stop: Arc<AtomicBool>,
    addr: SocketAddr,
}

impl MockServer {
    pub fn start(handler: impl Fn(&str) -> Reply + Send + Sync + 'static) -> MockServer {
        let ca_key = KeyPair::generate().unwrap();
        let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca_params.distinguished_name.push(DnType::CommonName, "places test CA");
        ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::DigitalSignature];
        let ca = ca_params.self_signed(&ca_key).unwrap();
        let leaf_key = KeyPair::generate().unwrap();
        let leaf =
            CertificateParams::new(vec!["127.0.0.1".to_string()]).unwrap().signed_by(&leaf_key, &ca, &ca_key).unwrap();
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let config = Arc::new(
            rustls::ServerConfig::builder_with_provider(provider)
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_no_client_auth()
                .with_single_cert(
                    vec![leaf.der().clone()],
                    PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(leaf_key.serialize_der())),
                )
                .unwrap(),
        );
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let hits = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let handler: Arc<Handler> = Arc::new(handler);
        {
            let (hits, stop) = (hits.clone(), stop.clone());
            std::thread::spawn(move || {
                for tcp in listener.incoming() {
                    if stop.load(Ordering::SeqCst) {
                        break;
                    }
                    let Ok(tcp) = tcp else { continue };
                    let (config, hits, handler) = (config.clone(), hits.clone(), handler.clone());
                    std::thread::spawn(move || serve(tcp, config, hits, handler));
                }
            });
        }
        MockServer { url: format!("https://127.0.0.1:{}", addr.port()), ca_pem: ca.pem(), hits, stop, addr }
    }

    pub fn hits(&self) -> Vec<Hit> {
        self.hits.lock().unwrap().clone()
    }

    /// Client options that trust this server's CA (and nothing else), with no proxy.
    pub fn http(&self) -> HttpOptions {
        HttpOptions { proxy: None, trust_pem: Some(self.ca_pem.clone()), timeout_ms: 5_000 }
    }
}

impl Drop for MockServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.addr);
    }
}

fn serve(tcp: TcpStream, config: Arc<rustls::ServerConfig>, hits: Arc<Mutex<Vec<Hit>>>, handler: Arc<Handler>) {
    let _ = tcp.set_read_timeout(Some(Duration::from_secs(5)));
    let Ok(conn) = rustls::ServerConnection::new(config) else { return };
    let mut tls = rustls::StreamOwned::new(conn, tcp);
    let mut head = Vec::new();
    let mut buf = [0u8; 4096];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") && head.len() < 65_536 {
        match tls.read(&mut buf) {
            Ok(0) | Err(_) => return,
            Ok(n) => head.extend_from_slice(&buf[..n]),
        }
    }
    let text = String::from_utf8_lossy(&head).into_owned();
    let mut lines = text.split("\r\n");
    let path = lines.next().and_then(|l| l.split(' ').nth(1)).unwrap_or("").to_string();
    let headers = lines
        .take_while(|l| !l.is_empty())
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    hits.lock().unwrap().push(Hit { at: Instant::now(), path: path.clone(), headers });
    let reply = handler(&path);
    if !reply.delay.is_zero() {
        std::thread::sleep(reply.delay);
    }
    let mut out =
        format!("HTTP/1.1 {} X\r\nContent-Length: {}\r\nConnection: close\r\n", reply.status, reply.body.len());
    for (k, v) in &reply.headers {
        out.push_str(&format!("{k}: {v}\r\n"));
    }
    out.push_str("\r\n");
    let _ = tls.write_all(out.as_bytes());
    let _ = tls.write_all(&reply.body);
    let _ = tls.flush();
    tls.conn.send_close_notify();
    let _ = tls.flush();
}

/// A row of a GeoNames town list (the 19 columns of its `geoname` table).
pub fn town_row(id: u32, name: &str, lat: f64, lon: f64, cc: &str, admin1: &str) -> String {
    format!(
        "{id}\t{name}\t{name}\t{name}x,{name}y\t{lat}\t{lon}\tP\tPPL\t{cc}\t\t{admin1}\t\t\t\t1200\t\t300\tEtc/UTC\t2024-01-01\n"
    )
}

/// A tiny made-up world: towns in two made-up countries (user-assigned codes XA and XB).
pub fn tiny_world() -> (String, String) {
    let cities = [
        town_row(1, "Alphaville", 10.0, 20.0, "XA", "01"),
        town_row(2, "Betatown", 10.2, 20.0, "XA", "01"),
        town_row(3, "Gammaburg", -16.5, 179.95, "XB", "07"),
        town_row(4, "Deltaport", -16.5, -179.9, "XB", "07"),
        town_row(5, "Nowhereton", 40.0, -30.0, "XA", "99"),
        // Broken rows, skipped.
        "6\tNoPosition\tx\t\tabc\t20.0\tP\tPPL\tXA\t\t01\n".to_string(),
        "7\tTooFewColumns\t10.0\t20.0\n".to_string(),
        town_row(8, "BadCountry", 1.0, 1.0, "xa", "01"),
        town_row(9, "OffTheMap", 95.0, 1.0, "XA", "01"),
    ]
    .concat();
    let admin1 = "XA.01\tRegion One\tRegion One\t100\nXB.07\tSeventh Isles\tSeventh Isles\t107\n".to_string();
    (cities, admin1)
}

/// A small, repeatable pseudo-random sequence.
pub struct Rng(pub u64);

impl Rng {
    pub fn next_f64(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }

    pub fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.next_f64()
    }
}

/// Metres per degree of latitude on Sentry Mate's 6,371 km sphere.
pub const M_PER_DEG: f64 = 6_371_000.0 * std::f64::consts::PI / 180.0;
