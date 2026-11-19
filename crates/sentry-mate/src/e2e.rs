//! End to end: a data folder seeded with made-up cars (synth), `serve` started in this process on free ports, the
//! first admin set up with the setup code from the keys folder, the history read through `/api/v1` once the engine
//! has decoded the logs, and the built-in pack served with its CSP. The client pins the server's own certificate.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;
use rustls::pki_types::ServerName;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// A port nothing listens on now (bound and let go: another test may take it meanwhile, which is rare).
fn free_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap().port()
}

struct Answer {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Answer {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

/// An HTTPS client that trusts only the server's certificate and keeps the session cookie.
struct Client {
    port: u16,
    tls: Arc<rustls::ClientConfig>,
    cookie: Option<String>,
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
    async fn request(&mut self, method: &str, path: &str, body: Option<Value>) -> Answer {
        let tcp = tokio::net::TcpStream::connect(("127.0.0.1", self.port)).await.unwrap();
        let name = ServerName::try_from("localhost").unwrap();
        let mut tls = tokio_rustls::TlsConnector::from(self.tls.clone()).connect(name, tcp).await.unwrap();
        let body = body.map(|b| serde_json::to_vec(&b).unwrap()).unwrap_or_default();
        let mut req = format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n");
        if method != "GET" {
            req.push_str(&format!(
                "X-Sentry-Mate: 1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n",
                body.len()
            ));
        }
        if let Some(c) = &self.cookie {
            req.push_str(&format!("Cookie: {c}\r\n"));
        }
        req.push_str("\r\n");
        tls.write_all(req.as_bytes()).await.unwrap();
        tls.write_all(&body).await.unwrap();
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
            if k.eq_ignore_ascii_case("set-cookie")
                && let Some(pair) = v.split(';').next()
                && pair.starts_with(accounts::COOKIE)
            {
                self.cookie = (!v.contains("Max-Age=0")).then(|| pair.to_string());
            }
        }
        Answer { status, headers, body }
    }

    async fn get(&mut self, path: &str) -> Answer {
        self.request("GET", path, None).await
    }
}

/// Waits until something listens on the port (the server has started).
async fn listening(port: u16, server: &tokio::task::JoinHandle<Result<(), String>>) {
    for _ in 0..600 {
        assert!(!server.is_finished(), "serve stopped before it listened");
        if tokio::net::TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("nothing listens on {port}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_seeded_server_shows_its_made_up_car_on_the_web_port() {
    let dir = tempfile::tempdir().unwrap();
    let (data, keys) = (dir.path().join("data"), dir.path().join("keys"));
    std::fs::create_dir_all(&data).unwrap();
    let (v, _) = Vault::create(&data, &keys, Kdf::FAST_FOR_TESTS).unwrap();
    let v = Arc::new(v);

    // one made-up car, one light day without trouble, through ingest's own storage and judge
    {
        let store = Store::new(can_dir(&data), v.clone());
        let judge = Judge::new(store.clone());
        let tokens = Tokens::open(tokens_file(&data), v.clone()).await.unwrap();
        let o = seed::Options {
            days: 1,
            cars: 1,
            seed: 3,
            light: true,
            clean: true,
            candump: false,
            with_real_cars: false,
            start_ms: None,
        };
        let gate = gate::Gate::memory();
        let enter = || -> Result<gate::Pass, Box<dyn std::error::Error>> { Ok(gate.enter(GATE_WAIT)?) };
        seed::run(&store, &judge, &tokens, &o, &enter, &mut Vec::new()).await.unwrap();
    }

    let (upload, web_port) = (free_port(), free_port());
    let (d, k) = (data.clone(), keys.clone());
    let server = tokio::spawn(async move {
        let net = Net { bind: "127.0.0.1".into(), upload, web: web_port };
        serve(&d, &k, &net, &[], &MoveEnv::default(), accounts::Hashing::FAST_FOR_TESTS)
            .await
            .map_err(|e| e.to_string())
    });
    listening(web_port, &server).await;

    // the client trusts only this server's certificate (the one both ports use)
    let id = Identity::load_or_create(&tls_dir(&data), &[], &v).unwrap();
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

    // nothing without signing in; the setup code is in the keys folder (and the log)
    assert_eq!(c.get("/api/v1/cars").await.status, 401);
    let code = std::fs::read_to_string(keys.join(site::SETUP_FILE)).unwrap().trim().to_string();
    assert!(!code.is_empty());
    // the code the start announced stays the one that works (the upkeep makes no other at once)
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(std::fs::read_to_string(keys.join(site::SETUP_FILE)).unwrap().trim(), code);
    let r = c
        .request(
            "POST",
            "/api/v1/setup",
            Some(json!({ "code": code, "name": "owner", "password": "correct horse battery staple" })),
        )
        .await;
    assert_eq!(r.status, 200, "setup: {}", r.text());
    assert!(c.cookie.is_some(), "no session cookie");

    // the engine decodes the seeded logs in the background: the car shows, with drives and charges
    let mut car = None;
    let mut drives = Value::Null;
    for _ in 0..600 {
        let r = c.get("/api/v1/cars").await;
        assert_eq!(r.status, 200, "{}", r.text());
        if let Some(id) = r.json()["items"][0]["id"].as_str() {
            let d = c.get(&format!("/api/v1/cars/{id}/drives")).await;
            assert_eq!(d.status, 200, "{}", d.text());
            if d.json()["items"].as_array().is_some_and(|a| !a.is_empty()) {
                car = Some(id.to_string());
                drives = d.json();
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let car = car.expect("the made-up car's drives never showed");
    assert!(car.starts_with('c'), "{car}");
    assert_eq!(drives["car"], json!(car));
    let first = drives["items"][0]["id"].as_str().unwrap().to_string();
    let r = c.get(&format!("/api/v1/cars/{car}/drives/{first}")).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.json()["id"], json!(first));
    let r = c.get(&format!("/api/v1/cars/{car}/charges")).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert!(r.json()["items"].is_array(), "{}", r.text());
    let r = c.get("/api/v1/charges?cars=all").await;
    assert_eq!(r.status, 200, "{}", r.text());
    let r = c.get(&format!("/api/v1/cars/{car}")).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let r = c.get(&format!("/api/v1/cars/{car}/now")).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let r = c.get(&format!("/api/v1/cars/{car}/timeline")).await;
    assert_eq!(r.status, 200, "{}", r.text());
    // a car that doesn't exist, and a drive that doesn't
    assert_eq!(c.get("/api/v1/cars/c999/drives").await.json()["error"]["code"], json!("no_such_car"));
    assert_eq!(c.get(&format!("/api/v1/cars/{car}/drives/1")).await.json()["error"]["code"], json!("no_such_drive"));
    let r = c.get("/api/v1/about").await;
    assert_eq!(r.json()["api_version"], json!(web::API_VERSION), "{}", r.text());

    // the built-in pack, with its CSP
    let r = c.get("/").await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert!(r.header("content-type").is_some_and(|t| t.starts_with("text/html")), "{:?}", r.headers);
    let csp = r.header("content-security-policy").unwrap_or_default();
    assert!(csp.contains("default-src") && csp.contains("script-src"), "CSP: {csp}");
    assert!(r.text().to_ascii_lowercase().contains("<html"), "{}", r.text());

    // the upload port still answers beside it
    assert!(tokio::net::TcpStream::connect(("127.0.0.1", upload)).await.is_ok());
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_seeded_cars_stay_two_cars_and_their_keys_and_tokens_are_served() {
    let dir = tempfile::tempdir().unwrap();
    let (data, keys) = (dir.path().join("data"), dir.path().join("keys"));
    std::fs::create_dir_all(&data).unwrap();
    let (v, _) = Vault::create(&data, &keys, Kdf::FAST_FOR_TESTS).unwrap();
    let v = Arc::new(v);
    {
        let store = Store::new(can_dir(&data), v.clone());
        let judge = Judge::new(store.clone());
        let tokens = Tokens::open(tokens_file(&data), v.clone()).await.unwrap();
        let o = seed::Options {
            days: 1,
            cars: 2,
            seed: 5,
            light: true,
            clean: true,
            candump: false,
            with_real_cars: false,
            start_ms: None,
        };
        let gate = gate::Gate::memory();
        let enter = || -> Result<gate::Pass, Box<dyn std::error::Error>> { Ok(gate.enter(GATE_WAIT)?) };
        seed::run(&store, &judge, &tokens, &o, &enter, &mut Vec::new()).await.unwrap();
    }
    let (upload, web_port) = (free_port(), free_port());
    let (d, k) = (data.clone(), keys.clone());
    let server = tokio::spawn(async move {
        let net = Net { bind: "127.0.0.1".into(), upload, web: web_port };
        serve(&d, &k, &net, &[], &MoveEnv::default(), accounts::Hashing::FAST_FOR_TESTS)
            .await
            .map_err(|e| e.to_string())
    });
    listening(web_port, &server).await;
    let id = Identity::load_or_create(&tls_dir(&data), &[], &v).unwrap();
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
    let code = std::fs::read_to_string(keys.join(site::SETUP_FILE)).unwrap().trim().to_string();
    let r = c
        .request(
            "POST",
            "/api/v1/setup",
            Some(json!({ "code": code, "name": "owner", "password": "correct horse battery staple" })),
        )
        .await;
    assert_eq!(r.status, 200, "setup: {}", r.text());

    // both cars show, each named by its own VIN, and none of the logs decoded so far is left out as another car's
    // (the merge showed once the first car's VIN was known: a few hundred logs in)
    let mut cars = Value::Null;
    let mut last = (Value::Null, Value::Null);
    for _ in 0..900 {
        let h = c.get("/api/v1/system/health").await;
        assert_eq!(h.status, 200, "{}", h.text());
        assert_eq!(h.json()["engine"]["logs_left_out"], json!(0), "{}", h.text());
        let list = c.get("/api/v1/cars").await.json();
        let both = list["items"].as_array().is_some_and(|a| a.len() == 2 && a.iter().all(|x| x["model"].is_string()));
        let e = &h.json()["engine"];
        if both && (e["logs_decoded"].as_u64().is_some_and(|n| n >= 100) || e["queued"] == json!(0)) {
            cars = list;
            break;
        }
        last = (h.json(), list);
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert!(!cars.is_null(), "the two cars never showed: {last:?}");
    let ids: Vec<String> = cars["items"].as_array().unwrap().iter().map(|x| x["id"].as_str().unwrap().into()).collect();
    assert_eq!(ids.len(), 2, "{cars}");
    let models: Vec<&str> = cars["items"].as_array().unwrap().iter().filter_map(|x| x["model"].as_str()).collect();
    assert_eq!(models.len(), 2, "{cars}");
    assert_ne!(models[0], models[1], "a Model Y and a Model 3: {cars}");

    // the keys, one per car
    let keys_list = c.get("/api/v1/can-keys").await;
    assert_eq!(keys_list.status, 200, "{}", keys_list.text());
    let items = keys_list.json()["items"].as_array().cloned().unwrap();
    assert_eq!(items.len(), 2, "{}", keys_list.text());
    let mut owners: Vec<String> = items.iter().map(|k| k["car"].as_str().unwrap().to_string()).collect();
    owners.sort();
    let mut sorted = ids.clone();
    sorted.sort();
    assert_eq!(owners, sorted);
    assert_eq!(c.get("/api/v1/can-keys?assigned=false").await.json()["items"], json!([]));
    let key = items[0]["key"].as_str().unwrap().to_string();
    let r = c.request("PATCH", &format!("/api/v1/can-keys/{key}"), Some(json!({ "preferred": true }))).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.json()["preferred"], json!(true));
    let r = c.request("PATCH", &format!("/api/v1/can-keys/{key}"), Some(json!({ "car": "c999" }))).await;
    assert_eq!((r.status, r.json()["error"]["code"].clone()), (422, json!("invalid")), "{}", r.text());
    let r = c.request("PATCH", "/api/v1/can-keys/000000000000", Some(json!({ "preferred": false }))).await;
    assert_eq!(r.json()["error"]["code"], json!("no_such_key"), "{}", r.text());

    // tokens: each car's, and one for a new car (its secret shown once)
    let r = c.get(&format!("/api/v1/cars/{}/upload-tokens", ids[0])).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.json()["items"].as_array().map(Vec::len), Some(1), "{}", r.text());
    assert_eq!(c.get("/api/v1/upload-tokens").await.json()["items"], json!([]));
    let r = c.request("POST", "/api/v1/upload-tokens", None).await;
    assert_eq!(r.status, 201, "{}", r.text());
    let made = r.json();
    assert!(made["secret"].as_str().is_some_and(|s| s.starts_with("smt_")), "{made}");
    assert_eq!(made["car"], Value::Null);
    let listed = c.get("/api/v1/upload-tokens").await.json();
    assert_eq!(listed["items"][0]["id"], made["id"], "{listed}");
    assert!(listed["items"][0].get("secret").is_none());
    let r = c.request("DELETE", &format!("/api/v1/upload-tokens/{}", made["id"].as_str().unwrap()), None).await;
    assert_eq!((r.status, r.json()["revoked"].clone()), (200, json!(true)), "{}", r.text());
    let r = c.request("DELETE", "/api/v1/upload-tokens/nope", None).await;
    assert_eq!(r.json()["error"]["code"], json!("no_such_token"), "{}", r.text());
    let r = c.get("/api/v1/setup/status").await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!((r.json()["cars"].clone(), r.json()["keys_waiting"].clone()), (json!(2), json!(0)), "{}", r.text());
    server.abort();
}
