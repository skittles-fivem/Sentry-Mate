#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! The subscriber against a tiny hand-rolled MQTT 3.1.1 broker on this machine: it connects with no last will,
//! subscribes to `<prefix>/cars/+/#` only, never publishes, and a matched car's values reach the sink as samples
//! and state facts. A password over plain TCP is refused before anything is stored or connects. A broker that refuses
//! the connection, refuses the password or goes quiet is told to the sink once (for a notice). Every value is made
//! up.

use model::{CarId, Sample};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tesla::ApiState;
use tmlocal::{
    ApiFact, Draft, Match, MqttConfig, Password, Secret, Service, ServiceError, Sink, SinkError, Status, Trouble,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// One `points` call.
type Points = (CarId, Vec<Sample>, Vec<ApiFact>);

#[derive(Default)]
struct TestSink {
    points: Mutex<Vec<Points>>,
    /// The troubles of each status told by `changed`.
    told: Mutex<Vec<Vec<Trouble>>>,
}

impl TestSink {
    fn told(&self) -> Vec<Vec<Trouble>> {
        self.told.lock().unwrap().clone()
    }
}

impl Sink for TestSink {
    fn new_car(&self, _car: &tesla::NewCar) -> Result<CarId, SinkError> {
        Ok(99)
    }
    fn points(&self, car: CarId, samples: &[Sample], facts: &[ApiFact]) -> Result<(), SinkError> {
        self.points.lock().unwrap().push((car, samples.to_vec(), facts.to_vec()));
        Ok(())
    }
    fn catch_up(&self, _dump: &std::path::Path, _cars: &[(i64, CarId)]) -> Result<tmlocal::catchup::Counts, SinkError> {
        Ok(tmlocal::catchup::Counts::default())
    }
    fn changed(&self, status: &Status) {
        self.told.lock().unwrap().push(status.problems().into_iter().map(|p| p.trouble).collect());
    }
}

/// How the made-up broker answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Takes the connection and sends car 1's values.
    Normal,
    /// Takes the connection and the subscription, then sends nothing.
    Quiet,
    /// Refuses the user name or password (CONNACK 5, not authorized).
    Refuse,
}

/// What the broker saw of the client.
#[derive(Default, Debug)]
struct Seen {
    /// The packet type of everything the client sent (the high nibble of the first byte).
    types: Vec<u8>,
    connect_flags: Option<u8>,
    filters: Vec<String>,
}

async fn read_packet(s: &mut TcpStream) -> Option<(u8, Vec<u8>)> {
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

fn publish(topic: &str, payload: &str) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&u16::try_from(topic.len()).unwrap().to_be_bytes());
    body.extend_from_slice(topic.as_bytes());
    body.extend_from_slice(payload.as_bytes());
    let mut out = vec![0x30, u8::try_from(body.len()).unwrap()];
    out.extend(body);
    out
}

/// A broker on 127.0.0.1 for one client at a time: CONNACK, SUBACK, then made-up car 1 values, the battery level
/// going up every 200 ms.
async fn broker() -> (u16, Arc<Mutex<Seen>>, Arc<Mutex<usize>>) {
    broker_with(Mode::Normal).await
}

async fn broker_with(mode: Mode) -> (u16, Arc<Mutex<Seen>>, Arc<Mutex<usize>>) {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Seen::default()));
    let accepted = Arc::new(Mutex::new(0usize));
    let (s2, a2) = (seen.clone(), accepted.clone());
    tokio::spawn(async move {
        while let Ok((mut c, _)) = l.accept().await {
            *a2.lock().unwrap() += 1;
            let seen = s2.clone();
            tokio::spawn(async move {
                let mut subscribed = false;
                let mut soc = 50u32;
                loop {
                    let next = tokio::time::timeout(Duration::from_millis(200), read_packet(&mut c)).await;
                    match next {
                        Ok(None) => return,
                        Ok(Some((first, body))) => {
                            let kind = first >> 4;
                            seen.lock().unwrap().types.push(kind);
                            match kind {
                                1 => {
                                    // CONNECT: "MQTT", level 4, then the flags
                                    seen.lock().unwrap().connect_flags = body.get(7).copied();
                                    let code = if mode == Mode::Refuse { 0x05 } else { 0x00 };
                                    c.write_all(&[0x20, 0x02, 0x00, code]).await.unwrap();
                                }
                                8 => {
                                    let n = usize::from(u16::from_be_bytes([body[2], body[3]]));
                                    let filter = String::from_utf8_lossy(&body[4..4 + n]).to_string();
                                    seen.lock().unwrap().filters.push(filter);
                                    c.write_all(&[0x90, 0x03, body[0], body[1], 0x00]).await.unwrap();
                                    if mode == Mode::Quiet {
                                        continue;
                                    }
                                    for (t, p) in [
                                        ("display_name", "Made-up Car"),
                                        ("state", "online"),
                                        ("latitude", "45.000100"),
                                        ("longitude", "-93.000200"),
                                        ("speed", "0"),
                                        ("odometer", "12345.6"),
                                        ("locked", "true"),
                                    ] {
                                        c.write_all(&publish(&format!("teslamate/cars/1/{t}"), p)).await.unwrap();
                                    }
                                    // another prefix: not ours
                                    c.write_all(&publish("other/cars/2/speed", "10")).await.unwrap();
                                    subscribed = true;
                                }
                                12 => c.write_all(&[0xd0, 0x00]).await.unwrap(),
                                14 => return,
                                _ => {}
                            }
                        }
                        Err(_) if subscribed => {
                            soc += 1;
                            let m = publish("teslamate/cars/1/battery_level", &soc.to_string());
                            if c.write_all(&m).await.is_err() {
                                return;
                            }
                        }
                        Err(_) => {}
                    }
                }
            });
        }
    });
    (port, seen, accepted)
}

fn service(dir: &tempfile::TempDir) -> (Service<TestSink>, Arc<TestSink>) {
    service_with(dir, tmlocal::QUIET_AFTER_MS)
}

fn service_with(dir: &tempfile::TempDir, quiet_after_ms: u64) -> (Service<TestSink>, Arc<TestSink>) {
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let (v, _) = vault::Vault::create(&data, &dir.path().join("keys"), vault::Kdf::FAST_FOR_TESTS).unwrap();
    let sink = Arc::new(TestSink::default());
    let opts = tmlocal::Options {
        connect_timeout: Duration::from_secs(3),
        backoff: (Duration::from_millis(200), Duration::from_secs(1)),
        test_listen: Duration::from_millis(800),
        quiet_after_ms,
        ..tmlocal::Options::default()
    };
    (Service::new(&data, Arc::new(v), sink.clone(), opts), sink)
}

fn draft(port: u16) -> Draft {
    Draft { mqtt: MqttConfig { host: "127.0.0.1".into(), port, ..MqttConfig::default() }, ..Draft::default() }
}

async fn until(what: &str, mut f: impl FnMut() -> bool) {
    for _ in 0..100 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("timed out waiting for {what}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn values_reach_the_sink_and_nothing_is_published() {
    let dir = tempfile::tempdir().unwrap();
    let (svc, sink) = service(&dir);
    let (port, seen, _) = broker().await;
    svc.start();
    assert!(!svc.running(), "nothing runs before an admin adds it");

    // the test button: subscribes, sees car 1, publishes nothing, stores nothing
    let t = svc.test(tmlocal::Target::Mqtt, Some(draft(port)), 1).await.unwrap();
    assert!(t.ok, "{t:?}");
    assert_eq!(t.cars_seen, vec!["1".to_string()]);
    assert!(!svc.store().exists());

    let c = svc.put(0, draft(port), 1_000).unwrap();
    assert_eq!((c.version, c.mqtt.password_set), (1, false));
    // car 1 shows up, named, not matched: nothing goes to a car yet
    until("car 1", || svc.config().is_some_and(|c| c.car("1").is_some_and(|k| k.display_name.is_some()))).await;
    let c = svc.config().unwrap();
    assert_eq!(c.car("1").unwrap().display_name.as_deref(), Some("Made-up Car"));
    assert_eq!(c.cars.len(), 1, "another prefix's car is not ours");
    assert_eq!(svc.status().mqtt.state, tmlocal::MqttState::Connected);
    assert!(sink.points.lock().unwrap().is_empty());

    svc.set_matches(c.version, &[("1".into(), Some(Match::Car { car: 7 }))]).unwrap();
    until("samples", || sink.points.lock().unwrap().iter().any(|(_, s, _)| s.iter().any(|s| s.soc.is_some()))).await;
    {
        let points = sink.points.lock().unwrap();
        assert!(points.iter().all(|(car, _, _)| *car == 7));
        let s = points.iter().flat_map(|(_, s, _)| s).find(|s| s.soc.is_some()).unwrap();
        assert_eq!((s.lat, s.lon, s.odometer_km), (Some(45.0001), Some(-93.0002), Some(12345.6)));
        assert_eq!(s.t_ms % 1000, 0);
        let f = points.iter().flat_map(|(_, _, f)| f).next().unwrap();
        assert_eq!(f.state, ApiState::Online);
        // about one sample a second at most, though a value changes every 200 ms
        let mut times: Vec<u64> = points.iter().flat_map(|(_, s, _)| s.iter().map(|s| s.t_ms)).collect();
        times.dedup();
        assert_eq!(times.len(), points.iter().map(|(_, s, _)| s.len()).sum::<usize>());
    }
    let st = svc.status();
    assert!(st.mqtt.messages_today >= 7 && st.mqtt.last_message_ms.is_some(), "{st:?}");

    svc.remove().unwrap();
    assert!(!svc.running() && !svc.store().exists());
    let seen = seen.lock().unwrap();
    assert!(!seen.types.contains(&3), "the subscriber published: {seen:?}");
    assert!(seen.filters.iter().all(|f| f == "teslamate/cars/+/#"), "{seen:?}");
    let flags = seen.connect_flags.unwrap();
    assert_eq!(flags & 0x04, 0, "no last will");
    assert_eq!(flags & 0x02, 0x02, "a clean session");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_broker_in_trouble_is_told_once_and_its_end_too() {
    let dir = tempfile::tempdir().unwrap();
    let (svc, sink) = service(&dir);
    // nothing listens on the port: refused, told once however often it is tried again
    let (_refusing, closed) = refusing_port();
    svc.put(0, draft(closed), 1).unwrap();
    until("refused", || sink.told().iter().any(|t| t == &[Trouble::ConnectionRefused])).await;
    let p = svc.status().mqtt_problem().unwrap();
    assert_eq!(
        (p.trouble, p.error.as_ref().map(|e| e.code.as_str())),
        (Trouble::ConnectionRefused, Some("connection_refused"))
    );
    let since = p.since_ms;
    // the backoff starts at 200 ms: several more tries in this time
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(sink.told().iter().filter(|t| t == &&[Trouble::ConnectionRefused]).count(), 1, "{:?}", sink.told());
    assert_eq!(svc.status().mqtt_problem().unwrap().since_ms, since, "since the first failure");
    assert_eq!(Trouble::ConnectionRefused.target(), tmlocal::Target::Mqtt);

    // the broker refuses the user name or password
    let (port, _, _) = broker_with(Mode::Refuse).await;
    let c = svc.config().unwrap();
    svc.put(c.version, draft(port), 2).unwrap();
    until("auth", || sink.told().last().is_some_and(|t| t == &[Trouble::AuthFailed])).await;

    // a broker that takes the connection: the trouble ends, and that is told too
    let (port, _, _) = broker().await;
    let c = svc.config().unwrap();
    svc.put(c.version, draft(port), 3).unwrap();
    until("connected", || svc.status().mqtt.state == tmlocal::MqttState::Connected).await;
    until("no trouble", || sink.told().last().is_some_and(Vec::is_empty)).await;
    assert!(svc.status().problems().is_empty());
    svc.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_broker_that_goes_quiet_is_told() {
    let dir = tempfile::tempdir().unwrap();
    let (svc, sink) = service_with(&dir, 600);
    let (port, _, _) = broker_with(Mode::Quiet).await;
    svc.put(0, draft(port), 1).unwrap();
    until("quiet", || sink.told().iter().any(|t| t == &[Trouble::Quiet])).await;
    let st = svc.status();
    assert_eq!(st.mqtt.state, tmlocal::MqttState::Connected, "still connected");
    let p = st.mqtt_problem().unwrap();
    assert_eq!((p.trouble, p.error), (Trouble::Quiet, None));
    assert_eq!(p.since_ms, st.mqtt.connected_since_ms, "quiet since it connected: nothing ever came");
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert_eq!(sink.told().iter().filter(|t| t == &&[Trouble::Quiet]).count(), 1, "told once");
    svc.stop();
    assert!(svc.status().problems().is_empty(), "stopped: nothing to say");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_catch_up_is_told() {
    let dir = tempfile::tempdir().unwrap();
    let (svc, sink) = service(&dir);
    let (port, _, _) = broker().await;
    let (_refusing, closed) = refusing_port();
    let mut d = draft(port);
    d.database = tmlocal::DbConfig { enabled: true, host: "127.0.0.1".into(), port: closed, ..Default::default() };
    svc.put(0, d, 1).unwrap();
    until("catch-up failed", || sink.told().iter().any(|t| t.contains(&Trouble::CatchupFailed))).await;
    let p = svc.status().db_problem().unwrap();
    assert_eq!(p.error.map(|e| e.code), Some("connection_refused".to_string()));
    svc.stop();
}

#[test]
fn troubles_of_a_status() {
    let mut st = Status::default();
    assert!(st.problems().is_empty());
    let e = |code: &str| tmlocal::ErrorInfo { code: code.into(), message: "made up".into(), at_ms: 5 };
    st.mqtt.state = tmlocal::MqttState::Error;
    for (code, t) in [
        ("connection_refused", Trouble::ConnectionRefused),
        ("timeout", Trouble::ConnectionRefused),
        ("tls_failed", Trouble::ConnectionRefused),
        ("auth_failed", Trouble::AuthFailed),
        ("pin_mismatch", Trouble::PinMismatch),
    ] {
        st.mqtt.last_error = Some(e(code));
        assert_eq!(st.mqtt_problem().map(|p| (p.trouble, p.since_ms)), Some((t, Some(5))), "{code}");
    }
    // the database: failed, and still while the next catch-up runs; caught up again: none
    st.database.state = tmlocal::DbState::Error;
    st.database.last_error = Some(e("query_failed"));
    assert_eq!(
        st.problems().iter().map(|p| p.trouble).collect::<Vec<_>>(),
        [Trouble::PinMismatch, Trouble::CatchupFailed]
    );
    st.database.state = tmlocal::DbState::Syncing;
    assert_eq!(st.db_problem().map(|p| p.trouble), Some(Trouble::CatchupFailed));
    assert_eq!(Trouble::CatchupFailed.target(), tmlocal::Target::Database);
    st.database.state = tmlocal::DbState::Ok;
    st.database.last_error = None;
    st.mqtt.state = tmlocal::MqttState::Paused;
    assert!(st.problems().is_empty(), "paused: nothing to say");
    let words: Vec<&str> = Trouble::ALL.iter().map(|t| t.word()).collect();
    assert_eq!(words, ["connection_refused", "auth_failed", "pin_mismatch", "quiet", "catchup_failed"]);
}

#[tokio::test]
async fn a_password_over_plain_tcp_is_refused_before_anything_connects() {
    let dir = tempfile::tempdir().unwrap();
    let (svc, _) = service(&dir);
    let (port, _, accepted) = broker().await;
    let mut d = draft(port);
    d.mqtt.username = Some("reader".into());
    d.mqtt_password = Password::Set(Secret::new("made-up-password"));
    let e = svc.put(0, d.clone(), 1).unwrap_err();
    assert_eq!(e.code(), "plaintext_password");
    let ServiceError::Invalid(v) = &e else { panic!("{e:?}") };
    assert_eq!(v[0].field, "mqtt.password");
    let e = svc.test(tmlocal::Target::Mqtt, Some(d.clone()), 1).await.unwrap_err();
    assert_eq!(e.code(), "plaintext_password");
    assert!(!svc.store().exists());
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(*accepted.lock().unwrap(), 0, "nothing connected");

    // allowed for the local network: kept sealed, never in the settings the server shows
    d.mqtt.allow_unencrypted = true;
    let c = svc.put(0, d, 1).unwrap();
    assert!(c.mqtt.password_set);
    assert!(!serde_json::to_string(&c).unwrap().contains("made-up-password"));
    let file = std::fs::read(dir.path().join("data/teslamate-local/config.json")).unwrap();
    assert!(!String::from_utf8_lossy(&file).contains("made-up-password"), "sealed");
    // a kept password goes only to the host it was typed for
    let mut moved = draft(port);
    moved.mqtt.host = "192.0.2.10".into();
    moved.mqtt.username = Some("reader".into());
    moved.mqtt.allow_unencrypted = true;
    assert_eq!(svc.put(c.version, moved, 2).unwrap_err().code(), "password_resend");
    svc.stop();
}

/// A port that refuses connections while the socket lives: bound but never listening, so no other test can take it
/// meanwhile (a port bound and let go could be, with several test binaries running at once: the refusal was then
/// told twice).
fn refusing_port() -> (tokio::net::TcpSocket, u16) {
    let s = tokio::net::TcpSocket::new_v4().unwrap();
    s.bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let port = s.local_addr().unwrap().port();
    (s, port)
}
