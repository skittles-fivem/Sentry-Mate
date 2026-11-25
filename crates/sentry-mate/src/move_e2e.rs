//! A move from the web pages between two servers running in this process (`serve` on free ports, each with its own
//! folders), with every input the old server had: the Tesla API and TeslaFi (each waiting out a saved wait, behind a
//! made-up proxy that writes down every connection, so nothing leaves the machine), the local TeslaMate and the MQTT
//! publisher (both on a made-up MQTT broker that writes down who connects and what is published). It checks the
//! move is one to one: the old server stops every input before it answers the finish, clears what it published,
//! writes nothing more and starts nothing again when it restarts; the new server starts the same inputs from where
//! the old one left them (their saved waits and counts), and each server's move log has the steps and nothing
//! secret. Every name, key and token is made up.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::e2e::{Answer, Client, any_cert, free_port, listening, set_up, setup_code};
use super::*;
use serde_json::{Value, json};
use std::sync::Mutex;
use std::time::Instant;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const PASSWORD: &str = "correct horse battery staple";
const TESLA_TOKEN: &str = "made-up-refresh-token-0042";
const TESLA_ACCESS: &str = "made-up-access-token-0042";
const TESLAFI_TOKEN: &str = "made-up-teslafi-token-0042";
/// The MQTT publisher's client id and topic prefix.
const CLIENT_ID: &str = "made-up-publisher";
const PREFIX: &str = "madeup";

// ------------------------------------------------------------------------------------------- a made-up MQTT broker

#[derive(Debug, Clone)]
enum Ev {
    Connect(String),
    Publish { topic: String, empty: bool, retain: bool },
    Subscribe,
    Closed,
}

/// What the broker saw: (when, connection number, what).
type Seen = Arc<Mutex<Vec<(Instant, usize, Ev)>>>;

async fn read_packet(s: &mut tokio::net::TcpStream) -> Option<(u8, Vec<u8>)> {
    let first = s.read_u8().await.ok()?;
    let (mut len, mut mul) = (0usize, 1usize);
    loop {
        let b = s.read_u8().await.ok()?;
        len += usize::from(b & 0x7f) * mul;
        if b & 0x80 == 0 {
            break;
        }
        mul *= 128;
    }
    let mut body = vec![0u8; len];
    s.read_exact(&mut body).await.ok()?;
    Some((first, body))
}

fn str_at(b: &[u8], at: usize) -> Option<(String, usize)> {
    let n = usize::from(u16::from_be_bytes([*b.get(at)?, *b.get(at + 1)?]));
    let s = String::from_utf8_lossy(b.get(at + 2..at + 2 + n)?).into_owned();
    Some((s, at + 2 + n))
}

/// A broker on 127.0.0.1 that takes every client (MQTT 3.1.1): CONNACK, SUBACK, PUBACK, PINGRESP; it sends nothing
/// of its own.
async fn broker() -> (u16, Seen) {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let s2 = seen.clone();
    tokio::spawn(async move {
        let mut n = 0usize;
        while let Ok((mut c, _)) = l.accept().await {
            n += 1;
            let (me, seen) = (n, s2.clone());
            tokio::spawn(async move {
                let say = |e: Ev| seen.lock().unwrap().push((Instant::now(), me, e));
                while let Some((first, body)) = read_packet(&mut c).await {
                    match first >> 4 {
                        1 => {
                            // "MQTT", level, flags, keep alive (10 bytes), then the client id
                            say(Ev::Connect(str_at(&body, 10).map(|x| x.0).unwrap_or_default()));
                            let _ = c.write_all(&[0x20, 0x02, 0x00, 0x00]).await;
                        }
                        3 => {
                            let qos = (first >> 1) & 3;
                            let Some((topic, at)) = str_at(&body, 0) else { break };
                            let at = if qos > 0 { at + 2 } else { at };
                            say(Ev::Publish { topic, empty: body.len() <= at, retain: first & 1 == 1 });
                            if qos == 1 {
                                let _ = c.write_all(&[0x40, 0x02, body[at - 2], body[at - 1]]).await;
                            }
                        }
                        8 => {
                            say(Ev::Subscribe);
                            let _ = c.write_all(&[0x90, 0x03, body[0], body[1], 0x00]).await;
                        }
                        12 => {
                            let _ = c.write_all(&[0xd0, 0x00]).await;
                        }
                        14 => break,
                        _ => {}
                    }
                }
                say(Ev::Closed);
            });
        }
    });
    (port, seen)
}

/// A proxy that writes down every request line (`CONNECT host:port`) and refuses it: nothing leaves the machine.
async fn fake_proxy() -> (u16, Arc<Mutex<Vec<String>>>) {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let s2 = seen.clone();
    tokio::spawn(async move {
        while let Ok((mut c, _)) = l.accept().await {
            let s3 = s2.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut b = [0u8; 1024];
                while !buf.windows(4).any(|w| w == b"\r\n\r\n") && buf.len() < 16_384 {
                    match c.read(&mut b).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => buf.extend_from_slice(&b[..n]),
                    }
                }
                let line = String::from_utf8_lossy(&buf).lines().next().unwrap_or_default().to_string();
                s3.lock().unwrap().push(line);
                let _ = c.write_all(b"HTTP/1.1 502 Bad Gateway\r\ncontent-length: 0\r\n\r\n").await;
            });
        }
    });
    (port, seen)
}

// ------------------------------------------------------------------------------------------- the old server's data

/// The old server's folders before its first start: one made-up car with a light day of logs, the server settings
/// (the proxy) and MQTT publishing, a Tesla account and a TeslaFi account each waiting out a wait they were given
/// (two hours from now), and the local TeslaMate's settings with a catch-up mark.
async fn old_data(data: &Path, keys: &Path, proxy: u16, broker: u16, now: u64) -> (String, String) {
    std::fs::create_dir_all(data).unwrap();
    let (v, _) = Vault::create(data, keys, Kdf::FAST_FOR_TESTS).unwrap();
    let v = Arc::new(v);
    {
        let store = Store::new(can_dir(data), v.clone());
        let judge = Judge::new(store.clone());
        let tokens = Tokens::open(tokens_file(data), v.clone()).await.unwrap();
        let o = seed::Options {
            days: 1,
            cars: 1,
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
    {
        let db = db::Db::open(data, v.clone(), db::Options::default()).unwrap();
        db.set_setting("server", json!({ "outbound": { "proxy": format!("http://127.0.0.1:{proxy}") } })).unwrap();
        db.set_setting(
            "mqtt",
            json!({ "enabled": true, "broker": format!("mqtt://127.0.0.1:{broker}"), "allow_unencrypted": true,
                    "client_id": CLIENT_ID, "prefix": PREFIX, "discovery": false }),
        )
        .unwrap();
    }
    let day = now / 86_400_000;
    let wait = now + 2 * 3_600_000;
    let t = tesla::Store::new(data, v.clone());
    let a = t
        .add(
            tesla::Account {
                id: String::new(),
                label: "Made-up Tesla account".into(),
                region: tesla::Region::Global,
                redirect: tesla::Redirect::App,
                added_ms: now - 86_400_000,
                version: 0,
                paused: false,
                location: tesla::Location::default(),
                state: tesla::AccountState::Ok,
                last_ok_ms: Some(now - 3_600_000),
                last_error: None,
                cars: Vec::new(),
            },
            &tesla::Credentials {
                refresh: tesla::Secret::new(TESLA_TOKEN),
                access: Some(tesla::Secret::new(TESLA_ACCESS)),
                access_expires_ms: 0,
            },
        )
        .unwrap();
    let pacing = tesla::store::Pacing {
        backoff_until_ms: wait,
        rate_limited_until_ms: wait,
        day,
        polls_today: 7,
        ..tesla::store::Pacing::default()
    };
    t.set_pacing(&a.id, &pacing).unwrap();
    let f = teslafi::Store::new(data, v.clone());
    let b = f
        .add(
            teslafi::Account {
                id: String::new(),
                label: "Made-up TeslaFi".into(),
                added_ms: now - 86_400_000,
                version: 0,
                paused: false,
                time_zone: Some("Europe/Berlin".into()),
                state: teslafi::AccountState::Ok,
                last_ok_ms: Some(now - 3_600_000),
                last_error: None,
                cars: Vec::new(),
            },
            &teslafi::Secret::new(TESLAFI_TOKEN),
        )
        .unwrap();
    let fp =
        teslafi::store::Pacing { backoff_until_ms: wait, day, polls_today: 5, ..teslafi::store::Pacing::default() };
    f.set_pacing(&b.id, &fp).unwrap();
    let m = tmlocal::Store::new(data, v);
    m.put(
        0,
        tmlocal::Config {
            version: 0,
            added_ms: now,
            paused: false,
            mqtt: tmlocal::MqttConfig { host: "127.0.0.1".into(), port: broker, ..tmlocal::MqttConfig::default() },
            database: tmlocal::DbConfig::default(),
            cars: Vec::new(),
        },
        &tmlocal::Password::Keep,
        &tmlocal::Password::Keep,
        now,
    )
    .unwrap();
    (a.id, b.id)
}

// ------------------------------------------------------------------------------------------------------- helpers

fn serving(data: &Path, keys: &Path) -> (u16, u16, tokio::task::JoinHandle<Result<(), String>>) {
    let (upload, web_port) = (free_port(), free_port());
    let (d, k) = (data.to_path_buf(), keys.to_path_buf());
    let server = tokio::spawn(async move {
        let net = Net { bind: "127.0.0.1".into(), upload, web: web_port };
        serve(&d, &k, &net, &["localhost".to_string()], &MoveEnv::default(), accounts::Hashing::FAST_FOR_TESTS)
            .await
            .map_err(|e| e.to_string())
    });
    (upload, web_port, server)
}

/// A request that may find the port closed for a moment (a server that restarts to switch).
async fn ask(c: &mut Client, method: &str, path: &str, body: Option<Value>) -> Option<Answer> {
    let r = tokio::time::timeout(Duration::from_secs(20), c.try_request(method, path, body)).await.ok()??;
    Some(r)
}

async fn until<T>(what: &str, secs: u64, mut f: impl AsyncFnMut() -> Option<T>) -> T {
    let t0 = Instant::now();
    loop {
        if let Some(v) = f().await {
            return v;
        }
        assert!(t0.elapsed() < Duration::from_secs(secs), "no {what} after {secs} s");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

fn account<'a>(list: &'a Value, id: &str) -> &'a Value {
    list["items"].as_array().or(list.as_array()).and_then(|a| a.iter().find(|x| x["id"] == id)).unwrap_or(&Value::Null)
}

/// Every file of the data folder but the move's own state and the locks (top-level dot names): its length and time.
fn snapshot(data: &Path) -> Vec<(String, u64, u64)> {
    let mut out = Vec::new();
    fn walk(dir: &Path, rel: &str, out: &mut Vec<(String, u64, u64)>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if rel.is_empty() && name.starts_with('.') {
                continue;
            }
            let p = if rel.is_empty() { name } else { format!("{rel}/{name}") };
            let md = e.metadata().unwrap();
            if md.is_dir() {
                walk(&e.path(), &p, out);
            } else {
                out.push((p, md.len(), moving::tree::mtime_us(&md)));
            }
        }
    }
    walk(data, "", &mut out);
    out.sort();
    out
}

/// Holds the event stream of a signed-in client open for `d` (the live figures' task runs while one is open).
async fn stream_open_for(port: u16, cookie: &str, d: Duration) {
    let tcp = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let name = rustls::pki_types::ServerName::try_from("localhost").unwrap();
    let mut tls = tokio_rustls::TlsConnector::from(any_cert()).connect(name, tcp).await.unwrap();
    let req = format!("GET /api/v1/events HTTP/1.1\r\nHost: localhost\r\nCookie: {cookie}\r\n\r\n");
    tls.write_all(req.as_bytes()).await.unwrap();
    let mut buf = vec![0u8; 4096];
    let _ = tokio::time::timeout(d, async { while tls.read(&mut buf).await.is_ok_and(|n| n > 0) {} }).await;
}

/// The move log's lines carry nothing secret: no code, key, token, session or path.
fn clean_log(log: &Value, secrets: &[&str]) -> Vec<String> {
    let lines: Vec<String> = log.as_array().unwrap().iter().map(|l| l["text"].as_str().unwrap().to_string()).collect();
    assert!(log.as_array().unwrap().iter().all(|l| l["at_ms"].as_u64().is_some_and(|t| t > 1_700_000_000_000)));
    let hex64 = |s: &str| s.split(|c: char| !c.is_ascii_hexdigit()).any(|w| w.len() >= 32);
    for l in &lines {
        for s in secrets {
            assert!(!l.contains(s), "a secret in the move's log: {l}");
        }
        for p in ["SMMV-", "SETUP-", "Bearer", ".sealed", "can/", "tokens/", "/tmp", "/data", "/keys"] {
            assert!(!l.contains(p), "{p} in the move's log: {l}");
        }
        assert!(!hex64(l), "a key or session in the move's log: {l}");
    }
    lines
}

fn has(lines: &[String], what: &str) -> bool {
    lines.iter().any(|l| l.contains(what))
}

// ---------------------------------------------------------------------------------------------------------- the test

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn a_move_is_one_to_one_the_old_server_stops_every_input_and_the_new_one_starts_them() {
    let dir = tempfile::tempdir().unwrap();
    let (od, ok) = (dir.path().join("old/data"), dir.path().join("old/keys"));
    let (nd, nk) = (dir.path().join("new/data"), dir.path().join("new/keys"));
    std::fs::create_dir_all(&nd).unwrap();
    let (proxy, proxied) = fake_proxy().await;
    let (bport, seen) = broker().await;
    let now = ingest::store::now_ms();
    let (tesla_id, teslafi_id) = old_data(&od, &ok, proxy, bport, now).await;

    // the old server, with its inputs running
    let (old_upload, old_web, old_server) = serving(&od, &ok);
    listening(old_web, &old_server).await;
    let mut old = set_up(&ok, old_web, PASSWORD).await;
    until("the old server's inputs", 60, async || {
        let t = old.get("/api/v1/integrations/tesla/accounts").await.json();
        let f = old.get("/api/v1/integrations/teslafi/accounts").await.json();
        let ev = seen.lock().unwrap().clone();
        let publisher = ev.iter().any(|e| matches!(&e.2, Ev::Connect(id) if id == CLIENT_ID));
        let subscriber = ev.iter().any(|e| matches!(e.2, Ev::Subscribe));
        let values =
            ev.iter().any(|e| matches!(&e.2, Ev::Publish { topic, empty: false, .. } if topic.contains("/cars/")));
        (account(&t, &tesla_id)["running"] == true
            && account(&f, &teslafi_id)["running"] == true
            && publisher
            && subscriber
            && values)
            .then_some(())
    })
    .await;
    let t_old = old.get("/api/v1/integrations/tesla/accounts").await.json();
    assert_eq!(account(&t_old, &tesla_id)["polls_today"], 7, "{t_old}");
    let wait = account(&t_old, &tesla_id)["rate_limited_until_ms"].clone();
    assert!(wait.as_u64().is_some_and(|w| w > now), "{t_old}");

    // the move code on the old server, and the move from the new server's first page
    let r = old.request("POST", "/api/v1/system/move", Some(json!({ "password": PASSWORD }))).await;
    assert_eq!(r.status, 201, "{}", r.text());
    let code = r.json()["move_code"].as_str().unwrap().to_string();
    let (_, new_web, new_server) = serving(&nd, &nk);
    listening(new_web, &new_server).await;
    let setup = setup_code(&nk).await;
    let mut claim = Client { port: new_web, tls: any_cert(), cookie: None };
    let r = claim.request("POST", "/api/v1/setup/claim", Some(json!({ "setup_code": setup }))).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let r = claim
        .request(
            "POST",
            "/api/v1/setup/move-here",
            Some(json!({ "from": format!("127.0.0.1:{old_upload}"), "code": code })),
        )
        .await;
    assert!(r.status < 300, "{} {}", r.status, r.text());

    // the new server copies, switches and starts again by itself; the old one says done
    let job = until("the new server's move done", 180, async || {
        let r = ask(&mut claim, "GET", "/api/v1/setup/job", None).await?;
        let j = r.json();
        let started = j["log"].as_array().is_some_and(|l| {
            l.iter().any(|x| x["text"].as_str().is_some_and(|t| t.starts_with("Inputs started here")))
        });
        (j["state"] == "done" && started).then_some(j)
    })
    .await;
    let old_move = until("the old server's move done", 60, async || {
        let r = ask(&mut old, "GET", "/api/v1/system/move", None).await?;
        (r.json()["state"] == "done").then(|| r.json())
    })
    .await;

    // the old server stopped every input: Tesla, TeslaFi, the local TeslaMate, MQTT publishing
    let t = old.get("/api/v1/integrations/tesla/accounts").await.json();
    let f = old.get("/api/v1/integrations/teslafi/accounts").await.json();
    assert_eq!(
        (account(&t, &tesla_id)["running"].clone(), account(&f, &teslafi_id)["running"].clone()),
        (json!(false), json!(false))
    );
    let tm = old.get("/api/v1/integrations/teslamate").await.json();
    assert_eq!(tm["status"]["mqtt"]["state"], "off", "{tm}");

    // the new server runs the same inputs, from where the old one left them: the wait and the day's count
    let mut new = Client { port: new_web, tls: any_cert(), cookie: None };
    let r = new.request("POST", "/api/v1/session", Some(json!({ "name": "owner", "password": PASSWORD }))).await;
    assert_eq!(r.status, 200, "sign-in on the new server: {}", r.text());
    let (t_new, f_new) = until("the new server's inputs", 60, async || {
        let t = new.get("/api/v1/integrations/tesla/accounts").await.json();
        let f = new.get("/api/v1/integrations/teslafi/accounts").await.json();
        (account(&t, &tesla_id)["running"] == true && account(&f, &teslafi_id)["running"] == true).then_some((t, f))
    })
    .await;
    assert_eq!(account(&t_new, &tesla_id)["rate_limited_until_ms"], wait, "the wait came with the data: {t_new}");
    assert_eq!(account(&t_new, &tesla_id)["polls_today"], 7, "{t_new}");
    assert_eq!(account(&f_new, &teslafi_id)["polls_today"], 5, "{f_new}");
    assert!(proxied.lock().unwrap().is_empty(), "both waited out the wait: {:?}", proxied.lock().unwrap());
    let tm_new = new.get("/api/v1/integrations/teslamate").await.json();
    assert_eq!(tm_new["mqtt"], tm["mqtt"], "the same local TeslaMate settings");

    // the broker: the old publisher closed and cleared its retained values before the new one connected; the two
    // subscribers never overlapped
    until("the new server's publisher", 60, async || {
        let ev = seen.lock().unwrap().clone();
        let conns: Vec<usize> =
            ev.iter().filter(|e| matches!(&e.2, Ev::Connect(id) if id == CLIENT_ID)).map(|e| e.1).collect();
        let last = *conns.last()?;
        let values = ev.iter().any(|e| {
            e.1 == last
                && matches!(&e.2, Ev::Publish { topic, empty: false, .. } if topic.starts_with(&format!("{PREFIX}/cars/c")))
        });
        (conns.len() >= 2 && values).then_some(())
    })
    .await;
    let ev = seen.lock().unwrap().clone();
    let of = |want: &dyn Fn(&Ev) -> bool| -> Vec<(Instant, usize)> {
        ev.iter().filter(|e| want(&e.2)).map(|e| (e.0, e.1)).collect()
    };
    let publishers = of(&|e| matches!(e, Ev::Connect(id) if id == CLIENT_ID));
    let cleanup = of(&|e| matches!(e, Ev::Connect(id) if id == &format!("{CLIENT_ID}-cleanup")));
    assert_eq!(cleanup.len(), 1, "one clean-up of the old server's topics: {ev:?}");
    let (old_pub, new_pub) = (publishers[0], *publishers.last().unwrap());
    let cleared: Vec<&(Instant, usize, Ev)> = ev.iter().filter(|e| e.1 == cleanup[0].1).collect();
    let cleared_topics: Vec<String> = cleared
        .iter()
        .filter_map(|e| match &e.2 {
            Ev::Publish { topic, empty, retain } => {
                assert!(*empty && *retain, "the clean-up sends empty retained messages only: {e:?}");
                Some(topic.clone())
            }
            _ => None,
        })
        .collect();
    assert!(cleared_topics.contains(&format!("{PREFIX}/status")), "{cleared_topics:?}");
    assert!(cleared_topics.iter().any(|t| t.starts_with(&format!("{PREFIX}/cars/c"))), "{cleared_topics:?}");
    let old_closed = ev.iter().find(|e| e.1 == old_pub.1 && matches!(e.2, Ev::Closed)).map(|e| e.0);
    let last_clear = cleared.iter().map(|e| e.0).max().unwrap();
    assert!(old_closed.is_some_and(|c| c <= last_clear), "the old publisher closed before the clean-up");
    assert!(last_clear < new_pub.0, "the old server's topics were cleared before the new server connected");
    let new_values = ev.iter().any(|e| {
        e.1 == new_pub.1
            && matches!(&e.2, Ev::Publish { topic, empty: false, .. } if topic.starts_with(&format!("{PREFIX}/cars/c")))
    });
    assert!(new_values, "the new server publishes the cars' values");
    let subs: Vec<usize> = of(&|e| matches!(e, Ev::Subscribe)).iter().map(|x| x.1).collect();
    let first_new_sub = ev.iter().filter(|e| matches!(e.2, Ev::Subscribe)).map(|e| e.0).find(|t| *t > last_clear);
    let first_new_sub = first_new_sub.expect("the new server's local TeslaMate subscribes");
    for c in subs.iter().filter(|c| ev.iter().any(|e| e.1 == **c && matches!(e.2, Ev::Subscribe) && e.0 < last_clear)) {
        let closed = ev.iter().find(|e| e.1 == *c && matches!(e.2, Ev::Closed)).map(|e| e.0);
        assert!(closed.is_some_and(|t| t < first_new_sub), "the old subscriber closed before the new one came");
    }

    // the logs: every step, a time on each line, nothing secret
    let secrets = [code.as_str(), setup.as_str(), TESLA_TOKEN, TESLA_ACCESS, TESLAFI_TOKEN, PASSWORD];
    let dir_text = dir.path().display().to_string();
    let old_log = clean_log(&old_move["log"], &[&secrets[..], &[dir_text.as_str()]].concat());
    for step in [
        "A move code was made",
        "connected",
        "Sent ",
        "Last-round pause",
        "copy matches",
        "Inputs stopped on this server: the Tesla API (1 account), TeslaFi (1 account), the local TeslaMate",
        "MQTT publishing (its retained topics cleared)",
        "Done:",
    ] {
        assert!(has(&old_log, step), "no \"{step}\" in the old server's log: {old_log:#?}");
    }
    let new_log = clean_log(&job["log"], &[&secrets[..], &[dir_text.as_str()]].concat());
    for step in [
        "Moving here from 127.0.0.1",
        "Connected:",
        "Round 1: copying",
        "Copying file ",
        "Round 1 done",
        "Last-round pause",
        "Last round:",
        "Switching to the copied data",
        "Switched:",
        "Inputs started here: the Tesla API (1 account), TeslaFi (1 account), the local TeslaMate, MQTT publishing",
    ] {
        assert!(has(&new_log, step), "no \"{step}\" in the new server's log: {new_log:#?}");
    }
    let order = |l: &[String], a: &str, b: &str| {
        l.iter().position(|x| x.contains(a)).unwrap() < l.iter().position(|x| x.contains(b)).unwrap()
    };
    assert!(order(&new_log, "Connected:", "Last-round pause") && order(&new_log, "Switched:", "Inputs started here"));
    assert!(order(&old_log, "copy matches", "Inputs stopped") && order(&old_log, "Inputs stopped", "Done:"));
    let file_line = new_log.iter().find(|l| l.starts_with("Copying file ")).unwrap();
    assert!(file_line.contains(" of ") && file_line.ends_with(").") && !file_line.contains('/'), "{file_line}");

    // the old server writes nothing more, with a page open on its live stream
    let before = snapshot(&od);
    stream_open_for(old_web, old.cookie.as_deref().unwrap(), Duration::from_secs(3)).await;
    assert_eq!(snapshot(&od), before, "the old server wrote to its data folder after the move");

    // a restart of the old server (on a copy of its folders: the stopped one's engine still holds these) starts none
    // of its inputs again
    old_server.abort();
    let _ = old_server.await;
    let copy = tempfile::tempdir().unwrap();
    let (cd, ck) = (copy.path().join("data"), copy.path().join("keys"));
    copy_dir(&od, &cd);
    copy_dir(&ok, &ck);
    let before = snapshot(&cd);
    let n_events = seen.lock().unwrap().len();
    let (_, again_web, again) = serving(&cd, &ck);
    listening(again_web, &again).await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(snapshot(&cd), before, "the restarted old server wrote to its data folder");
    let mut a = Client { port: again_web, tls: any_cert(), cookie: None };
    let r = a.request("POST", "/api/v1/session", Some(json!({ "name": "owner", "password": PASSWORD }))).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let t = a.get("/api/v1/integrations/tesla/accounts").await.json();
    let f = a.get("/api/v1/integrations/teslafi/accounts").await.json();
    assert_eq!(
        (account(&t, &tesla_id)["running"].clone(), account(&f, &teslafi_id)["running"].clone()),
        (json!(false), json!(false))
    );
    let later: Vec<Ev> = seen.lock().unwrap()[n_events..].iter().map(|e| e.2.clone()).collect();
    assert!(
        !later.iter().any(|e| matches!(e, Ev::Connect(_) | Ev::Subscribe)),
        "the restarted old server connected to the broker: {later:?}"
    );
    assert!(proxied.lock().unwrap().is_empty());
    again.abort();
    new_server.abort();
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let name = e.file_name();
        if name.to_string_lossy() == ".lock" {
            continue;
        }
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &to.join(&name));
        } else {
            std::fs::copy(e.path(), to.join(&name)).unwrap();
        }
    }
}

/// On a server whose data moved away, what runs by itself writes nothing and connects nowhere: the upkeep (the
/// accounts' "last seen", the connections overview), the flush on a stop, the invoices' audit, the live figures,
/// the full rebuild's watch, the imported rows' check and the MQTT publisher (which ends at once).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn on_a_moved_server_nothing_that_runs_by_itself_writes() {
    let dir = tempfile::tempdir().unwrap();
    let (data, keys) = (dir.path().join("data"), dir.path().join("keys"));
    let (proxy, proxied) = fake_proxy().await;
    let (bport, seen) = broker().await;
    old_data(&data, &keys, proxy, bport, ingest::store::now_ms()).await;
    let v = Arc::new(Vault::open(&data, &keys).unwrap());
    moving::old::mark_moved_for_tests(&data, &v, "192.0.2.30", true).unwrap();
    let id = Identity::load_or_create(&tls_dir(&data), &[], &v).unwrap();
    let (d, v2, fp) = (data.clone(), v.clone(), id.fingerprint());
    let gate = gate::Gate::open(&data).unwrap();
    let offer = tokio::task::spawn_blocking(move || moving::old::Offer::open(&d, v2, gate, APP_VERSION, &fp))
        .await
        .unwrap()
        .unwrap();
    let store = Store::new(can_dir(&data), v.clone());
    let engine = site::EngineRun::open(&data, v.clone(), store, None, false).unwrap();
    let app = site::web_state(
        &data,
        &keys,
        v.clone(),
        &engine.engine,
        accounts::Hashing::FAST_FOR_TESTS,
        None,
        web::moving::ClaimKeep::default(),
        Some(offer),
        None,
    )
    .unwrap();
    assert!(web::moving::data_moved(&app));
    // the first read of a folder no engine ever ran on makes the database's key names file (a real server made it
    // long before its move)
    let a = app.clone();
    tokio::task::spawn_blocking(move || web::live::Watcher::default().round(&a)).await.unwrap();
    let before = snapshot(&data);
    let a = app.clone();
    tokio::task::spawn_blocking(move || {
        assert!(!site::upkeep_once(&a, true));
        assert_eq!(web::resources::places::audit_invoice_prices(&a), 0);
        let mut w = web::live::Watcher::default();
        w.round(&a);
        w.round(&a);
    })
    .await
    .unwrap();
    site::flush(&app).await;
    for t in [
        web::resources::mqtt::spawn(app.clone()),
        web::resources::full_rebuild::spawn_watch(app.clone()),
        web::resources::imports::spawn_rehome(&app),
    ] {
        tokio::time::timeout(Duration::from_secs(10), t).await.expect("it ends at once").unwrap();
    }
    assert_eq!(snapshot(&data), before, "a moved server wrote to its data folder");
    assert!(seen.lock().unwrap().is_empty(), "a moved server connected to the MQTT broker");
    assert!(proxied.lock().unwrap().is_empty());
}
