//! End to end: a data folder seeded with made-up cars (synth), `serve` started in this process on free ports, the
//! first admin set up with the setup code from the keys folder, the history read through `/api/v1` once the engine
//! has decoded the logs, and the built-in pack served with its CSP. The client pins the server's own certificate.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;
use rustls::pki_types::ServerName;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// A port nothing listens on now (bound and let go: another test may take it meanwhile, which is rare).
pub(crate) fn free_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap().port()
}

pub(crate) struct Answer {
    pub(crate) status: u16,
    headers: Vec<(String, String)>,
    pub(crate) body: Vec<u8>,
}

impl Answer {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    pub(crate) fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    pub(crate) fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

/// An HTTPS client that trusts only the server's certificate and keeps the session cookie.
pub(crate) struct Client {
    pub(crate) port: u16,
    pub(crate) tls: Arc<rustls::ClientConfig>,
    pub(crate) cookie: Option<String>,
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
    pub(crate) async fn request(&mut self, method: &str, path: &str, body: Option<Value>) -> Answer {
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

    pub(crate) async fn get(&mut self, path: &str) -> Answer {
        self.request("GET", path, None).await
    }
}

/// Waits until something listens on the port (the server has started).
pub(crate) async fn listening(port: u16, server: &tokio::task::JoinHandle<Result<(), String>>) {
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
            awake_min: None,
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
    let code = setup_code(&keys).await;
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
#[ignore = "slow: full tests (cargo test -- --include-ignored)"]
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
            awake_min: None,
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
    let code = setup_code(&keys).await;
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

    // naming a key: unnamed, it has an automatic label; a name is trimmed, 1 to 60 characters, null clears it
    assert_eq!(items[0]["device"], Value::Null, "{}", keys_list.text());
    assert!(
        items[0]["device_label"].as_str().is_some_and(|l| l.starts_with("CAN Bridge key ")),
        "{}",
        keys_list.text()
    );
    let key_path = format!("/api/v1/can-keys/{key}");
    let r = c.request("PATCH", &key_path, Some(json!({ "name": json!(5) }))).await;
    assert_eq!((r.status, r.json()["error"]["fields"]["name"].is_string()), (400, true), "{}", r.text());
    for bad in [json!("  "), json!("x".repeat(61))] {
        let r = c.request("PATCH", &key_path, Some(json!({ "name": bad }))).await;
        assert_eq!((r.status, r.json()["error"]["code"].clone()), (422, json!("invalid")), "{}", r.text());
    }
    let r = c.request("PATCH", &key_path, Some(json!({ "name": json!("  Screenmate in the test car ") }))).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(
        (r.json()["device"].clone(), r.json()["device_label"].clone()),
        (json!("Screenmate in the test car"), json!("Screenmate in the test car")),
        "{}",
        r.text()
    );
    let r = c.request("PATCH", &key_path, Some(json!({ "name": json!(null) }))).await;
    assert_eq!(r.json()["device"], Value::Null, "{}", r.text());
    let r = c.request("PATCH", &key_path, Some(json!({ "name": json!("Screenmate in the test car") }))).await;
    assert_eq!(r.status, 200, "{}", r.text());

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
    // with the three fields of the CAN Bridge app, as /network has the address and the fingerprint
    let net = c.get("/api/v1/network").await.json();
    assert!(net["certificate"]["sha256"].as_str().is_some_and(|s| s.len() == 95), "{net}");
    assert!(net["upload_address"].as_str().is_some_and(|a| a.ends_with("/api/can/logs")), "{net}");
    let app_of = |made: &Value| {
        json!({
            "address": net["upload_address"],
            "token": made["secret"],
            "certificate_sha256": net["certificate"]["sha256"],
        })
    };
    assert_eq!(made["app"], app_of(&made), "{made}");
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
    assert_eq!(r.json()["import_running"], json!(false), "{}", r.text());
    // an import that is not finished shows there with its fix, until it is discarded
    let r = c.request("POST", "/api/v1/imports", Some(json!({ "name": "made-up.sql.gz", "bytes_total": 1000 }))).await;
    assert_eq!(r.status, 201, "{}", r.text());
    let import = r.json()["id"].clone();
    let st = c.get("/api/v1/setup/status").await.json();
    let fix = json!({ "cause": "import_running", "link": "/settings/import", "import": import });
    assert_eq!(st["import_running"], json!(true), "{st}");
    assert!(st["fixes"].as_array().unwrap().contains(&fix), "{st}");
    let r = c.request("DELETE", &format!("/api/v1/imports/{}", import.as_str().unwrap()), None).await;
    assert!(r.status < 300, "{}", r.text());
    let st = c.get("/api/v1/setup/status").await.json();
    assert_eq!(st["import_running"], json!(false), "{st}");
    assert!(st["fixes"].as_array().unwrap().iter().all(|f| f["cause"] != json!("import_running")), "{st}");

    // the audit log says which key went to which car and which token was made or revoked (by id and car), not just
    // the route: moving a key redirects whose history a car's signed logs count for
    let owner = items[0]["car"].as_str().unwrap().to_string();
    let other = ids.iter().find(|i| **i != owner).unwrap().clone();
    let r = c.request("PATCH", &format!("/api/v1/can-keys/{key}"), Some(json!({ "car": other }))).await;
    assert_eq!((r.status, r.json()["car"].clone()), (200, json!(other)), "{}", r.text());
    let r = c.request("PATCH", &format!("/api/v1/can-keys/{key}"), Some(json!({ "car": owner }))).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.json()["device"], json!("Screenmate in the test car"), "the name goes with the key: {}", r.text());
    let health = c.get(&format!("/api/v1/cars/{owner}/health")).await.json();
    let named = health["keys"].as_array().unwrap().iter().find(|k| k["key"] == json!(key)).cloned().unwrap();
    assert_eq!(named["device"], json!("Screenmate in the test car"), "{health}");
    let tokens = c.get(&format!("/api/v1/cars/{owner}/upload-tokens")).await.json();
    let tied = tokens["items"].as_array().unwrap().iter().find(|t| t["key"] == json!(key)).cloned();
    if let Some(t) = tied {
        assert_eq!(t["device"], json!("Screenmate in the test car"), "{tokens}");
    }
    let r = c.request("POST", &format!("/api/v1/cars/{other}/upload-tokens"), None).await;
    assert_eq!(r.status, 201, "{}", r.text());
    let net = c.get("/api/v1/network").await.json();
    assert_eq!(
        r.json()["app"],
        json!({
            "address": net["upload_address"],
            "token": r.json()["secret"],
            "certificate_sha256": net["certificate"]["sha256"],
        }),
        "{}",
        r.text()
    );
    let tid = r.json()["id"].as_str().unwrap().to_string();
    let r = c.request("DELETE", &format!("/api/v1/cars/{other}/upload-tokens/{tid}"), None).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let audit = c.get("/api/v1/audit?limit=200").await;
    assert_eq!(audit.status, 200, "{}", audit.text());
    let rows = audit.json()["items"].as_array().cloned().unwrap();
    let has = |action: &str, target: String, car: &Value| {
        rows.iter().any(|e| e["action"] == action && e["target"] == json!(target) && e["car"] == *car)
    };
    let (o, t) = (json!(owner), json!(other));
    assert!(has("key-unassigned", format!("key:{owner}:{key}"), &o), "the car the key left: {rows:?}");
    assert!(has("key-assigned", format!("key:{other}:{key}"), &t), "the car the key went to: {rows:?}");
    assert!(has("key-assigned", format!("key:{owner}:{key}"), &o), "and back: {rows:?}");
    assert!(has("key-preferred", format!("key:{owner}:{key}"), &o), "{rows:?}");
    assert!(has("can-key-renamed", format!("key:{owner}:{key}"), &o), "{rows:?}");
    let renames: Vec<&str> =
        rows.iter().filter(|e| e["action"] == "can-key-renamed").filter_map(|e| e["summary"].as_str()).collect();
    assert_eq!(renames.len(), 3, "named, cleared, named again: {renames:?}");
    assert!(renames.iter().any(|s| s.contains("Screenmate in the test car") && s.starts_with("Named")), "{renames:?}");
    assert!(has("token-created", format!("token:{other}:{tid}"), &t), "{rows:?}");
    assert!(has("token-revoked", format!("token:{other}:{tid}"), &t), "{rows:?}");
    let new_id = made["id"].as_str().unwrap();
    assert!(has("token-created", format!("token:{new_id}"), &Value::Null), "a new car's token: {rows:?}");
    assert!(has("token-revoked", format!("token:{new_id}"), &Value::Null), "{rows:?}");
    let moved = rows.iter().find(|e| e["action"] == "key-assigned" && e["car"] == t).unwrap();
    // by the cars' names (each car's own name, else "car c2")
    let said = moved["summary"].as_str().unwrap();
    assert!(said.starts_with(&format!("Moved the CAN Bridge key {key} from ")) && said.contains(" to "), "{moved}");
    assert!(
        !rows.iter().any(|e| e["summary"].as_str().is_some_and(|s| s.contains("{key}") || s.contains("{token}"))),
        "no entry with only the route: {rows:?}"
    );

    // a watch account's set-up status speaks only of its own cars: not of a key no car has (another car's, here),
    // nor of the tokens made for a new car
    let r = c.request("PATCH", &format!("/api/v1/can-keys/{key}"), Some(json!({ "car": null }))).await;
    assert_eq!((r.status, r.json()["car"].clone()), (200, Value::Null), "{}", r.text());
    let r = c.request("POST", "/api/v1/upload-tokens", None).await;
    assert_eq!(r.status, 201, "{}", r.text());
    let admin_status = c.get("/api/v1/setup/status").await.json();
    assert_eq!(admin_status["keys_waiting"], json!(1), "{admin_status}");
    assert!(admin_status["fixes"].as_array().unwrap().iter().any(|f| f["key"] == json!(key)), "{admin_status}");
    let r = c
        .request(
            "POST",
            "/api/v1/accounts",
            Some(json!({ "name": "watcher", "password": "another correct horse battery", "role": "watch",
                         "cars": [other], "confirm_password": "correct horse battery staple" })),
        )
        .await;
    assert_eq!(r.status, 201, "{}", r.text());
    let mut w = Client { port: web_port, tls: c.tls.clone(), cookie: None };
    let r = w
        .request(
            "POST",
            "/api/v1/session",
            Some(json!({ "name": "watcher", "password": "another correct horse battery" })),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(w.get("/api/v1/can-keys").await.status, 403);
    let ws = w.get("/api/v1/setup/status").await;
    assert_eq!(ws.status, 200, "{}", ws.text());
    let body = ws.json();
    assert!(!ws.text().contains(&key), "the waiting key's id: {body}");
    assert_eq!((body["cars"].clone(), body["keys_waiting"].clone()), (json!(1), json!(0)), "{body}");
    assert!(!body["fixes"].as_array().unwrap().iter().any(|f| f["cause"] == "key_waiting"), "{body}");
    assert!(
        body["tokens"]["count"].as_u64() < admin_status["tokens"]["count"].as_u64(),
        "only its own car's tokens: {body} {admin_status}"
    );
    assert!(body["tokens"]["count"].as_u64().unwrap() <= 1, "{body}");
    server.abort();
}

/// Takes any certificate (the test compares the one served by its SHA-256 instead).
#[derive(Debug)]
struct AnyCert;

impl rustls::client::danger::ServerCertVerifier for AnyCert {
    fn verify_server_cert(
        &self,
        _: &rustls::pki_types::CertificateDer<'_>,
        _: &[rustls::pki_types::CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        _: &[u8],
        _: &rustls::pki_types::CertificateDer<'_>,
        _: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::General("TLS 1.3 only".into()))
    }
    fn verify_tls13_signature(
        &self,
        m: &[u8],
        c: &rustls::pki_types::CertificateDer<'_>,
        d: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            m,
            c,
            d,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider().signature_verification_algorithms.supported_schemes()
    }
}

fn any_cert() -> Arc<rustls::ClientConfig> {
    Arc::new(
        rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_protocol_versions(&[&rustls::version::TLS13])
            .unwrap()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AnyCert))
            .with_no_client_auth(),
    )
}

/// The SHA-256 of the certificate a port serves, as `/network` writes it (`AB:CD:...`).
async fn served(port: u16) -> String {
    use sha2::Digest as _;
    let tcp = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let tls = tokio_rustls::TlsConnector::from(any_cert())
        .connect(ServerName::try_from("localhost").unwrap(), tcp)
        .await
        .unwrap();
    let der = tls.get_ref().1.peer_certificates().unwrap()[0].clone();
    web::resources::certificate::colon_hex(&sha2::Sha256::digest(der.as_ref()))
}

fn start_serving(data: &Path, keys: &Path) -> (u16, u16, tokio::task::JoinHandle<Result<(), String>>) {
    start_serving_as(data, keys, &["first.example"])
}

/// [`start_serving`] with these `--names`.
fn start_serving_as(
    data: &Path,
    keys: &Path,
    names: &[&str],
) -> (u16, u16, tokio::task::JoinHandle<Result<(), String>>) {
    let (upload, web_port) = (free_port(), free_port());
    let (d, k) = (data.to_path_buf(), keys.to_path_buf());
    let names: Vec<String> = names.iter().map(|n| n.to_string()).collect();
    let server = tokio::spawn(async move {
        let net = Net { bind: "127.0.0.1".into(), upload, web: web_port };
        serve(&d, &k, &net, &names, &MoveEnv::default(), accounts::Hashing::FAST_FOR_TESTS)
            .await
            .map_err(|e| e.to_string())
    });
    (upload, web_port, server)
}

/// A new certificate from the web pages: admin only, the password again, the saved host names in it, served on both
/// ports at once without a restart, its files the ones on disk after a restart, and an audit entry.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_new_certificate_from_the_web_pages_is_served_on_both_ports_at_once() {
    const PASSWORD: &str = "correct horse battery staple";
    let dir = tempfile::tempdir().unwrap();
    let (data, keys) = (dir.path().join("data"), dir.path().join("keys"));
    std::fs::create_dir_all(&data).unwrap();
    Vault::create(&data, &keys, Kdf::FAST_FOR_TESTS).unwrap();
    let (upload, web_port, server) = start_serving(&data, &keys);
    listening(web_port, &server).await;
    let mut c = Client { port: web_port, tls: any_cert(), cookie: None };
    let code = setup_code(&keys).await;
    let r =
        c.request("POST", "/api/v1/setup", Some(json!({ "code": code, "name": "owner", "password": PASSWORD }))).await;
    assert_eq!(r.status, 200, "setup: {}", r.text());

    let n = c.get("/api/v1/network").await.json();
    let old = n["certificate"]["sha256"].as_str().unwrap().to_string();
    assert_eq!(old.len(), 95, "colon hex: {n}");
    assert_eq!(n["certificate"]["names"], json!(["localhost", "first.example"]), "{n}");
    assert_eq!(n["upload_address"], json!(format!("https://first.example:{upload}/api/can/logs")));
    assert_eq!((served(web_port).await, served(upload).await), (old.clone(), old.clone()));

    // the saved host names go into the next one
    let s = c.get("/api/v1/settings").await.json();
    let r = c
        .request(
            "PATCH",
            "/api/v1/settings",
            Some(json!({ "version": s["version"], "host_names": ["nas.example", "192.0.2.57"] })),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.text());

    // the password again, and an admin's
    let r = c.request("POST", "/api/v1/network/certificate", Some(json!({}))).await;
    assert_eq!(r.status, 400, "{}", r.text());
    let r = c.request("POST", "/api/v1/network/certificate", Some(json!({ "password": "wrong one" }))).await;
    assert_eq!((r.status, r.json()["error"]["code"].clone()), (403, json!("wrong_password")), "{}", r.text());
    let r = c
        .request(
            "POST",
            "/api/v1/accounts",
            Some(json!({ "name": "watcher", "password": "another correct horse battery", "role": "watch",
                         "cars": [], "confirm_password": PASSWORD })),
        )
        .await;
    assert_eq!(r.status, 201, "{}", r.text());
    let mut w = Client { port: web_port, tls: any_cert(), cookie: None };
    let r = w
        .request(
            "POST",
            "/api/v1/session",
            Some(json!({ "name": "watcher", "password": "another correct horse battery" })),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    let r = w
        .request("POST", "/api/v1/network/certificate", Some(json!({ "password": "another correct horse battery" })))
        .await;
    assert_eq!(r.status, 403, "{}", r.text());
    assert_eq!(served(web_port).await, old, "nothing changed");

    let r = c.request("POST", "/api/v1/network/certificate", Some(json!({ "password": PASSWORD }))).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let made = r.json();
    let new = made["sha256"].as_str().unwrap().to_string();
    assert_ne!(new, old);
    assert_eq!(made["names"], json!(["localhost", "nas.example", "192.0.2.57"]), "{made}");
    assert_eq!(made["upload_address"], json!(format!("https://nas.example:{upload}/api/can/logs")));
    // both ports serve it now, without a restart
    assert_eq!(served(web_port).await, new, "the web port");
    assert_eq!(served(upload).await, new, "the upload port");
    let n = c.get("/api/v1/network").await.json();
    assert_eq!(n["certificate"], json!({ "names": made["names"], "sha256": new, "own": false }));
    // in the audit log
    let a = c.get("/api/v1/audit").await.json();
    let entry = a["items"].as_array().unwrap().iter().find(|e| e["action"] == "certificate-made").cloned();
    let entry = entry.unwrap_or_else(|| panic!("no audit entry: {a}"));
    assert_eq!((entry["account"].clone(), entry["target"].clone()), (json!("owner"), json!("server:certificate")));
    assert!(entry["summary"].as_str().unwrap().contains("nas.example"), "{entry}");
    // what the command prints
    let v = Vault::open(&data, &keys).unwrap();
    let fp = Identity::load_or_create(&tls_dir(&data), &[], &v).unwrap().fingerprint();
    assert_eq!(new.replace(':', "").to_ascii_lowercase(), fp);

    // after a restart: the same new certificate (the names given at start don't make another). The engine of the
    // stopped server still holds this data folder in the test's process, so the restart runs on a copy of it.
    server.abort();
    let _ = server.await;
    fn copy(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).unwrap();
        for e in std::fs::read_dir(from).unwrap() {
            let e = e.unwrap();
            if e.file_type().unwrap().is_dir() {
                copy(&e.path(), &to.join(e.file_name()));
            } else {
                std::fs::copy(e.path(), to.join(e.file_name())).unwrap();
            }
        }
    }
    let again = tempfile::tempdir().unwrap();
    let (data, keys) = (again.path().join("data"), again.path().join("keys"));
    copy(&dir.path().join("data"), &data);
    copy(&dir.path().join("keys"), &keys);
    let (upload, web_port, server) = start_serving(&data, &keys);
    listening(web_port, &server).await;
    listening(upload, &server).await;
    assert_eq!((served(web_port).await, served(upload).await), (new.clone(), new));
    server.abort();
}

/// A fresh server set up with `PASSWORD` as the owner's: its client.
async fn set_up(keys: &Path, web_port: u16, password: &str) -> Client {
    let mut c = Client { port: web_port, tls: any_cert(), cookie: None };
    // the code is written just after the port opens
    let file = keys.join(site::SETUP_FILE);
    for _ in 0..600 {
        if file.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let code = std::fs::read_to_string(&file).unwrap().trim().to_string();
    let r =
        c.request("POST", "/api/v1/setup", Some(json!({ "code": code, "name": "owner", "password": password }))).await;
    assert_eq!(r.status, 200, "setup: {}", r.text());
    c
}

/// `--names` as typed (spaces, a wildcard, a name with a space): only the host names and addresses go into the
/// certificate, the address as an IP address, and a new certificate made with no host names saved takes those, not
/// the rest.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn names_given_at_start_that_are_not_host_names_stay_out_of_the_certificate() {
    const PASSWORD: &str = "correct horse battery staple";
    let dir = tempfile::tempdir().unwrap();
    let (data, keys) = (dir.path().join("data"), dir.path().join("keys"));
    std::fs::create_dir_all(&data).unwrap();
    Vault::create(&data, &keys, Kdf::FAST_FOR_TESTS).unwrap();
    let (_, web_port, server) = start_serving_as(&data, &keys, &["nas.lan", " 192.0.2.5", "bad name", "*.lan"]);
    listening(web_port, &server).await;
    let mut c = set_up(&keys, web_port, PASSWORD).await;
    let want = json!(["localhost", "nas.lan", "192.0.2.5"]);
    assert_eq!(c.get("/api/v1/network").await.json()["certificate"]["names"], want);
    assert_eq!(c.get("/api/v1/settings").await.json()["host_names"], json!(["nas.lan", "192.0.2.5"]));
    let r = c.request("POST", "/api/v1/network/certificate", Some(json!({ "password": PASSWORD }))).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.json()["names"], want);
    let pem = std::fs::read_to_string(tls_dir(&data).join("cert.pem")).unwrap();
    let der = first_der(&pem);
    let has = |b: &[u8]| der.windows(b.len()).any(|w| w == b);
    assert!(has(&[0x87, 4, 192, 0, 2, 5]), "the address as an IP address");
    assert!(!has(b"bad name") && !has(b"*.lan") && !has(b"192.0.2.5"));
    server.abort();
}

/// The first certificate of a PEM file, DER.
fn first_der(pem: &str) -> Vec<u8> {
    use base64::Engine as _;
    let body = pem.split("-----BEGIN CERTIFICATE-----").nth(1).unwrap().split("-----END CERTIFICATE-----").next();
    let b64: String = body.unwrap().chars().filter(|c| !c.is_whitespace()).collect();
    base64::engine::general_purpose::STANDARD.decode(b64).unwrap()
}

/// With the owner's own certificate, `/network` says so and a new certificate is refused before the password is
/// checked, so a two-step code is never used up on it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_owners_own_certificate_is_refused_before_the_password() {
    const PASSWORD: &str = "correct horse battery staple";
    let dir = tempfile::tempdir().unwrap();
    let (data, keys) = (dir.path().join("data"), dir.path().join("keys"));
    std::fs::create_dir_all(&data).unwrap();
    Vault::create(&data, &keys, Kdf::FAST_FOR_TESTS).unwrap();
    let mut own = rcgen::CertificateParams::new(vec!["nas.lan".to_string()]).unwrap();
    own.distinguished_name = rcgen::DistinguishedName::new();
    own.distinguished_name.push(rcgen::DnType::CommonName, "nas.lan");
    let kp = rcgen::KeyPair::generate().unwrap();
    let cert = own.self_signed(&kp).unwrap();
    std::fs::create_dir_all(tls_dir(&data)).unwrap();
    std::fs::write(tls_dir(&data).join("cert.pem"), cert.pem()).unwrap();
    std::fs::write(tls_dir(&data).join("key.pem"), kp.serialize_pem()).unwrap();
    let (_, web_port, server) = start_serving(&data, &keys);
    listening(web_port, &server).await;
    let mut c = set_up(&keys, web_port, PASSWORD).await;
    let n = c.get("/api/v1/network").await.json();
    assert_eq!(n["certificate"]["own"], json!(true), "{n}");
    for pw in ["wrong one", PASSWORD] {
        let r = c.request("POST", "/api/v1/network/certificate", Some(json!({ "password": pw }))).await;
        assert_eq!((r.status, r.json()["error"]["code"].clone()), (409, json!("own_certificate")), "{}", r.text());
    }
    assert!(!tls_dir(&data).join("key.pem").exists(), "sealed at start");
    server.abort();
}

/// The one-time set-up code, once the server has written it (a busy machine may take a moment after the port opens).
pub(crate) async fn setup_code(keys: &std::path::Path) -> String {
    let path = keys.join(site::SETUP_FILE);
    for _ in 0..300 {
        if let Ok(text) = std::fs::read_to_string(&path)
            && !text.trim().is_empty()
        {
            return text.trim().to_string();
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("no set-up code in {} after 30 s", path.display());
}
