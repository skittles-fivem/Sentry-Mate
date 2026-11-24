//! Publishing against a small in-process broker (MQTT 3.1.1 over TLS with an rcgen certificate, or plain), which
//! records what it receives and can misbehave on purpose: refuse connections for a while, or cut a connection
//! before acknowledging anything. All data here is synthetic.

#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]

use bytes::BytesMut;
use model::{Drive, Gear, Now, Sample, Source, State};
use mqtt::{CarInfo, Event, Limits, Mqtt, Settings};
use rumqttc::mqttbytes::v4::{ConnAck, ConnectReturnCode, Packet, PubAck};
use rumqttc::{LastWill, QoS};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_rustls::rustls;

#[derive(Debug, Clone)]
struct Got {
    /// Which connection (0 = the first) it came on.
    conn: usize,
    topic: String,
    payload: String,
    retain: bool,
    qos: QoS,
}

#[derive(Debug, Clone)]
struct Hello {
    client_id: String,
    clean_session: bool,
    keep_alive: u16,
    login: Option<(String, String)>,
    will: Option<LastWill>,
}

#[derive(Default)]
struct Log {
    hellos: Vec<Hello>,
    got: Vec<Got>,
    disconnects: usize,
    tls_failures: usize,
}

#[derive(Clone, Copy, PartialEq)]
enum Tls {
    None,
    V13,
    V12Only,
}

/// How the broker misbehaves on purpose: it acknowledges only the first `acked` publishes of a connection and closes
/// it after `after` publishes, `linger` later (so the acknowledgements it sent reach the client first). Only the
/// first connection, unless `every`.
#[derive(Clone, Copy)]
struct Cut {
    after: usize,
    acked: usize,
    linger: Duration,
    every: bool,
}

impl Cut {
    /// The first connection acknowledges nothing and is closed after `after` publishes.
    fn first(after: usize) -> Cut {
        Cut { after, acked: 0, linger: Duration::ZERO, every: false }
    }
}

struct Broker {
    addr: SocketAddr,
    log: Arc<Mutex<Log>>,
    /// While false, connections are closed as soon as they are accepted.
    open: Arc<AtomicBool>,
    fingerprint: String,
}

impl Broker {
    async fn start(tls: Tls, cut: Option<Cut>) -> Broker {
        let made = rcgen::generate_simple_self_signed(vec!["localhost".into(), "127.0.0.1".into()]).unwrap();
        let fingerprint = mqtt::security::fingerprint(made.cert.der());
        let acceptor = (tls != Tls::None).then(|| {
            let provider = Arc::new(rustls::crypto::ring::default_provider());
            let versions: &[&rustls::SupportedProtocolVersion] =
                if tls == Tls::V12Only { &[&rustls::version::TLS12] } else { &[&rustls::version::TLS13] };
            let key = rustls::pki_types::PrivateKeyDer::Pkcs8(made.key_pair.serialize_der().into());
            let config = rustls::ServerConfig::builder_with_provider(provider)
                .with_protocol_versions(versions)
                .unwrap()
                .with_no_client_auth()
                .with_single_cert(vec![made.cert.der().clone()], key)
                .unwrap();
            tokio_rustls::TlsAcceptor::from(Arc::new(config))
        });
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let log = Arc::new(Mutex::new(Log::default()));
        let open = Arc::new(AtomicBool::new(true));
        let (log2, open2) = (log.clone(), open.clone());
        tokio::spawn(async move {
            let mut conns = 0usize;
            loop {
                let Ok((tcp, _)) = listener.accept().await else { return };
                if !open2.load(Ordering::SeqCst) {
                    drop(tcp);
                    continue;
                }
                let cut = cut.filter(|c| c.every || conns == 0);
                let (log, conn) = (log2.clone(), conns);
                conns += 1;
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    match acceptor {
                        None => serve(tcp, log, conn, cut).await,
                        Some(a) => match a.accept(tcp).await {
                            Ok(s) => serve(s, log, conn, cut).await,
                            Err(_) => log.lock().unwrap().tls_failures += 1,
                        },
                    }
                });
            }
        });
        Broker { addr, log, open, fingerprint }
    }

    fn got(&self) -> Vec<Got> {
        self.log.lock().unwrap().got.clone()
    }

    fn hellos(&self) -> Vec<Hello> {
        self.log.lock().unwrap().hellos.clone()
    }

    /// The last payload on each topic.
    fn latest(&self) -> HashMap<String, String> {
        self.got().into_iter().map(|g| (g.topic, g.payload)).collect()
    }
}

async fn serve<S: AsyncRead + AsyncWrite + Unpin>(mut s: S, log: Arc<Mutex<Log>>, conn: usize, cut: Option<Cut>) {
    let mut buf = BytesMut::new();
    let mut publishes = 0usize;
    let mut chunk = vec![0u8; 16 * 1024];
    loop {
        loop {
            let packet = match Packet::read(&mut buf, 1 << 20) {
                Ok(p) => p,
                Err(rumqttc::mqttbytes::Error::InsufficientBytes(_)) => break,
                Err(_) => return,
            };
            let reply = match packet {
                Packet::Connect(c) => {
                    log.lock().unwrap().hellos.push(Hello {
                        client_id: c.client_id,
                        clean_session: c.clean_session,
                        keep_alive: c.keep_alive,
                        login: c.login.map(|l| (l.username, l.password)),
                        will: c.last_will,
                    });
                    Some(Packet::ConnAck(ConnAck::new(ConnectReturnCode::Success, false)))
                }
                Packet::Publish(p) => {
                    log.lock().unwrap().got.push(Got {
                        conn,
                        topic: p.topic.clone(),
                        payload: String::from_utf8(p.payload.to_vec()).unwrap(),
                        retain: p.retain,
                        qos: p.qos,
                    });
                    publishes += 1;
                    match cut {
                        Some(c) if publishes >= c.after => {
                            tokio::time::sleep(c.linger).await;
                            return;
                        }
                        Some(c) if publishes > c.acked => None,
                        _ => (p.qos == QoS::AtLeastOnce).then(|| Packet::PubAck(PubAck::new(p.pkid))),
                    }
                }
                Packet::PingReq => Some(Packet::PingResp),
                Packet::Disconnect => {
                    log.lock().unwrap().disconnects += 1;
                    return;
                }
                _ => None,
            };
            if let Some(r) = reply {
                let mut out = BytesMut::new();
                r.write(&mut out, 1 << 20).unwrap();
                if s.write_all(&out).await.is_err() || s.flush().await.is_err() {
                    return;
                }
            }
        }
        match s.read(&mut chunk).await {
            Ok(0) | Err(_) => return,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
}

fn settings(b: &Broker) -> Settings {
    Settings {
        enabled: true,
        broker: format!("mqtts://127.0.0.1:{}", b.addr.port()),
        cert_sha256: Some(b.fingerprint.clone()),
        username: Some("ha".into()),
        password: Some("synthetic-password".into()),
        client_id: "sentry-mate-test".into(),
        discovery: true,
        limits: Limits { reconnect_min_ms: 50, reconnect_max_ms: 200, connect_timeout_s: 2, ..Limits::default() },
        ..Settings::default()
    }
}

async fn wait_for(what: &str, cond: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// A parked car with made-up values (and a made-up position).
fn now(car: u32, soc: f64) -> Now {
    Now {
        range_estimated_km: None,
        healthy: None,
        since_ms: None,
        trim: None,
        off_can: Box::default(),
        car,
        at_ms: 1_794_150_000_000,
        state: Some(State::Online),
        driving: false,
        place: None,
        sample: Sample {
            lat: Some(1.234_567),
            lon: Some(2.345_678),
            soc: Some(soc),
            range_km: Some(300.0),
            gear: Some(Gear::P),
            odometer_km: Some(1000.0),
            outside_temp_c: Some(12.0),
            tpms_bar: [Some(2.9); 4],
            ..Sample::default()
        },
    }
}

fn drive(car: u32) -> Drive {
    Drive {
        id: 1,
        car,
        start_ms: 1_794_150_000_000,
        end_ms: 1_794_150_600_000,
        start: None,
        end: None,
        start_place: None,
        end_place: None,
        start_address: None,
        end_address: None,
        start_odometer_km: Some(1000.0),
        end_odometer_km: Some(1010.0),
        distance_km: Some(10.0),
        speed_max_kmh: Some(80.0),
        power_max_kw: None,
        power_min_kw: None,
        energy_used_kwh: Some(1.5),
        start_soc: Some(80.0),
        end_soc: Some(78.0),
        start_range_km: None,
        end_range_km: None,
        outside_temp_avg_c: None,
        inside_temp_avg_c: None,
        ascent_m: None,
        descent_m: None,
        source: Source::Live,
        inputs: model::Inputs::NONE,
        rules_version: 1,
    }
}

fn info(name: &str) -> CarInfo {
    CarInfo { name: name.into(), model: Some("Model Y 2022".into()) }
}

#[tokio::test]
async fn publishes_over_pinned_tls() {
    let b = Broker::start(Tls::V13, None).await;
    let m = Mqtt::start(&settings(&b)).unwrap().unwrap();
    m.publish_now(&info("One"), &now(1, 80.5), Some("Home"));
    m.publish_now(&info("Two"), &now(2, 55.0), None);
    m.publish_event(Event::DriveEnded { drive: Box::new(drive(1)), start_place: Some("Home".into()), end_place: None });
    wait_for("both cars and the event", || {
        let l = b.latest();
        l.contains_key("sentry-mate/cars/c2/soc") && l.contains_key("sentry-mate/cars/c1/event")
    })
    .await;

    let hello = &b.hellos()[0];
    assert_eq!(hello.client_id, "sentry-mate-test");
    assert!(hello.clean_session);
    assert_eq!(hello.keep_alive, 30);
    assert_eq!(hello.login, Some(("ha".into(), "synthetic-password".into())));
    let will = hello.will.clone().unwrap();
    assert_eq!(
        (will.topic.as_str(), &will.message[..], will.qos, will.retain),
        ("sentry-mate/status", &b"offline"[..], QoS::AtLeastOnce, true)
    );

    let got = b.got();
    assert_eq!((got[0].topic.as_str(), got[0].payload.as_str(), got[0].retain), ("sentry-mate/status", "online", true));
    assert!(got.iter().all(|g| g.qos == QoS::AtLeastOnce), "QoS 1 throughout");
    let event = got.iter().find(|g| g.topic == "sentry-mate/cars/c1/event").unwrap();
    assert!(!event.retain, "events are moments, not retained");
    let v: serde_json::Value = serde_json::from_str(&event.payload).unwrap();
    assert_eq!((v["event_type"].as_str(), v["distance"].as_f64()), (Some("drive_ended"), Some(10.0)));
    for g in got.iter().filter(|g| g.topic.starts_with("sentry-mate/cars/") && !g.topic.ends_with("/event")) {
        assert!(g.retain, "{} is retained", g.topic);
    }
    let l = b.latest();
    assert_eq!(l["sentry-mate/cars/c1/soc"], "80.5");
    assert_eq!(l["sentry-mate/cars/c1/place"], "Home");
    assert_eq!(l["sentry-mate/cars/c1/state"], "online");
    assert_eq!(l["sentry-mate/cars/c1/latitude"], "", "location publishing is off");
    assert!(got.iter().all(|g| !g.payload.contains("1.234567") && !g.payload.contains("2.345678")), "no position");
    let config: serde_json::Value = serde_json::from_str(&l["homeassistant/sensor/sentry-mate_c2/soc/config"]).unwrap();
    assert_eq!(config["device"]["name"], "Two");
    assert_eq!(l["homeassistant/device_tracker/sentry-mate_c1/location/config"], "");

    wait_for("acknowledgements", || m.stats().delivered as usize == b.got().len()).await;
    let s = m.stats();
    assert!(s.connected && s.connects == 1 && s.last_error.is_none(), "{s:?}");
    assert_eq!(s.broker, format!("mqtts://127.0.0.1:{} (TLS, pinned)", b.addr.port()));
    assert_eq!((s.states_waiting, s.events_waiting), (0, 0));

    // the same values again: nothing new to say
    let before = b.got().len();
    m.publish_now(&info("One"), &now(1, 80.5), Some("Home"));
    m.publish_now(&info("One"), &now(1, 80.0), Some("Home"));
    wait_for("the changed value", || b.latest()["sentry-mate/cars/c1/soc"] == "80").await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let after: Vec<_> = b.got()[before..].iter().map(|g| g.topic.clone()).collect();
    assert_eq!(after, ["sentry-mate/cars/c1/soc"], "only what changed is sent again");

    m.stop().await;
    wait_for("the clean disconnect", || b.log.lock().unwrap().disconnects > 0).await;
    let log = b.log.lock().unwrap();
    let last = log.got.last().unwrap();
    assert_eq!((last.topic.as_str(), last.payload.as_str(), last.retain), ("sentry-mate/status", "offline", true));
    assert_eq!(log.disconnects, 1, "a clean disconnect");
}

#[tokio::test]
async fn wrong_pin_is_refused() {
    let b = Broker::start(Tls::V13, None).await;
    let mut s = settings(&b);
    s.cert_sha256 = Some("00".repeat(32));
    let m = Mqtt::start(&s).unwrap().unwrap();
    m.publish_now(&info("One"), &now(1, 80.0), None);
    wait_for("a refused connection", || m.stats().last_error.is_some()).await;
    let err = m.stats().last_error.unwrap();
    assert!(err.contains("does not match the pinned SHA-256") && err.contains(&b.fingerprint), "{err}");
    assert_eq!(m.stats().last_error_code, Some("pin_mismatch"));
    assert!(b.hellos().is_empty() && b.got().is_empty(), "nothing, not even the password, was sent");
    assert_eq!(m.stats().states_waiting, 1, "kept for when the setup is fixed");
    m.stop().await;
}

#[tokio::test]
async fn without_a_pin_a_self_signed_broker_is_refused() {
    let b = Broker::start(Tls::V13, None).await;
    let mut s = settings(&b);
    s.cert_sha256 = None;
    s.broker = format!("mqtts://localhost:{}", b.addr.port());
    let m = Mqtt::start(&s).unwrap().unwrap();
    wait_for("a refused connection", || m.stats().last_error.is_some()).await;
    let err = m.stats().last_error.unwrap();
    assert!(err.contains("not from a public CA"), "{err}");
    assert_eq!(m.stats().last_error_code, Some("tls_failed"));
    assert!(b.hellos().is_empty());
    m.stop().await;
}

#[tokio::test]
async fn tls_12_only_when_allowed() {
    let b = Broker::start(Tls::V12Only, None).await;
    let mut s = settings(&b);
    let m = Mqtt::start(&s).unwrap().unwrap();
    wait_for("a refused connection", || m.stats().last_error.is_some()).await;
    let err = m.stats().last_error.unwrap();
    assert!(err.contains("doesn't offer TLS 1.3"), "{err}");
    assert!(b.hellos().is_empty());
    m.stop().await;

    s.allow_tls12 = true;
    let m = Mqtt::start(&s).unwrap().unwrap();
    m.publish_now(&info("One"), &now(1, 80.0), None);
    wait_for("a TLS 1.2 connection", || b.latest().contains_key("sentry-mate/cars/c1/soc")).await;
    m.stop().await;
}

#[tokio::test]
async fn plain_mqtt_only_when_allowed_and_never_with_a_password() {
    let b = Broker::start(Tls::None, None).await;
    let mut s = settings(&b);
    s.broker = format!("mqtt://127.0.0.1:{}", b.addr.port());
    s.cert_sha256 = None;
    assert_eq!(Mqtt::start(&s).err(), Some(mqtt::Error::Unencrypted("mqtt".into())));
    s.allow_unencrypted = true;
    assert_eq!(Mqtt::start(&s).err(), Some(mqtt::Error::PasswordUnencrypted));
    s.password = None;
    let m = Mqtt::start(&s).unwrap().unwrap();
    assert!(m.stats().broker.ends_with("(UNENCRYPTED)"));
    m.publish_now(&info("One"), &now(1, 80.0), None);
    wait_for("a plain connection", || b.latest().contains_key("sentry-mate/cars/c1/soc")).await;
    assert_eq!(b.hellos()[0].login, Some(("ha".into(), String::new())));
    m.stop().await;
}

#[tokio::test]
async fn off_and_filtered() {
    let b = Broker::start(Tls::V13, None).await;
    let mut s = settings(&b);
    s.enabled = false;
    assert!(Mqtt::start(&s).unwrap().is_none(), "off: nothing starts");
    s.enabled = true;
    s.cars = Some([2].into_iter().collect());
    s.discovery = false;
    let m = Mqtt::start(&s).unwrap().unwrap();
    m.publish_now(&info("One"), &now(1, 80.0), None);
    m.publish_event(Event::DriveStarted { drive: Box::new(drive(1)), start_place: None });
    m.publish_now(&info("Two"), &now(2, 60.0), None);
    wait_for("car 2", || b.latest().contains_key("sentry-mate/cars/c2/soc")).await;
    m.forget_car(1);
    wait_for("car 1 cleared", || b.latest().contains_key("sentry-mate/cars/c1/soc")).await;
    let got = b.got();
    assert!(
        got.iter().filter(|g| g.topic.starts_with("sentry-mate/cars/c1/")).all(|g| g.payload.is_empty()),
        "car 1 is only ever cleared"
    );
    assert!(got.iter().any(|g| g.topic == "homeassistant/sensor/sentry-mate_c1/soc/config" && g.payload.is_empty()));
    assert!(!got.iter().any(|g| g.topic.starts_with("homeassistant/") && !g.payload.is_empty()), "discovery is off");
    m.stop().await;
}

#[tokio::test]
async fn location_when_turned_on() {
    let b = Broker::start(Tls::V13, None).await;
    let mut s = settings(&b);
    s.publish_location = true;
    let m = Mqtt::start(&s).unwrap().unwrap();
    m.publish_now(&info("One"), &now(1, 80.0), None);
    wait_for("the position", || b.latest().contains_key("sentry-mate/cars/c1/location")).await;
    let l = b.latest();
    assert_eq!(
        (l["sentry-mate/cars/c1/latitude"].as_str(), l["sentry-mate/cars/c1/longitude"].as_str()),
        ("1.234567", "2.345678")
    );
    let tracker: serde_json::Value =
        serde_json::from_str(&l["homeassistant/device_tracker/sentry-mate_c1/location/config"]).unwrap();
    assert_eq!(tracker["json_attributes_topic"], "sentry-mate/cars/c1/location");
    m.stop().await;
}

#[tokio::test]
async fn unconfirmed_messages_are_sent_again_after_a_drop() {
    // the first connection takes 5 messages, acknowledges none, and drops
    let b = Broker::start(Tls::V13, Some(Cut::first(5))).await;
    let m = Mqtt::start(&settings(&b)).unwrap().unwrap();
    m.publish_now(&info("One"), &now(1, 80.0), None);
    m.publish_event(Event::DriveStarted { drive: Box::new(drive(1)), start_place: None });
    wait_for("the second connection's values", || {
        b.got().iter().any(|g| g.conn == 1 && g.topic == "sentry-mate/cars/c1/soc")
    })
    .await;
    wait_for("the event, again", || b.got().iter().any(|g| g.conn == 1 && g.topic == "sentry-mate/cars/c1/event"))
        .await;
    let got = b.got();
    assert!(got.iter().filter(|g| g.conn == 0).count() <= 5);
    let second: Vec<_> = got.iter().filter(|g| g.conn == 1).collect();
    assert_eq!(second[0].payload, "online");
    assert!(second.iter().any(|g| g.topic == "homeassistant/sensor/sentry-mate_c1/soc/config"), "all of it again");
    let s = m.stats();
    assert_eq!(s.connects, 2);
    assert_eq!(s.states_waiting + s.events_waiting, 0);
    m.stop().await;
}

#[tokio::test]
async fn only_unconfirmed_messages_are_sent_again() {
    // the first connection acknowledges "online" and the first two events, takes a third, and drops
    let cut = Cut { after: 4, acked: 3, linger: Duration::from_millis(200), every: false };
    let b = Broker::start(Tls::V13, Some(cut)).await;
    b.open.store(false, Ordering::SeqCst);
    let m = Mqtt::start(&settings(&b)).unwrap().unwrap();
    for id in 1..=5u64 {
        let mut d = drive(1);
        d.id = id;
        m.publish_event(Event::DriveStarted { drive: Box::new(d), start_place: None });
    }
    b.open.store(true, Ordering::SeqCst);
    let ids = |conn: usize| -> Vec<u64> {
        b.got()
            .iter()
            .filter(|g| g.conn == conn && g.topic.ends_with("/event"))
            .map(|g| {
                serde_json::from_str::<serde_json::Value>(&g.payload).unwrap()["drive"]
                    .as_str()
                    .unwrap()
                    .parse()
                    .unwrap()
            })
            .collect()
    };
    wait_for("the last event on the second connection", || ids(1).contains(&5)).await;
    assert_eq!(ids(0), [1, 2, 3], "the first connection took three events");
    assert_eq!(ids(1), [3, 4, 5], "confirmed ones are not sent again, the rest are, in order");
    assert_eq!(m.stats().connects, 2);
    m.stop().await;
}

#[tokio::test]
async fn a_broker_that_drops_each_connection_at_once_is_not_hammered() {
    // e.g. another client with the same id, or a broker that refuses what we publish by closing the link: each
    // connection is accepted, then closed after its first message. The wait must keep doubling (50, 100, 200,
    // 400, 800 ms: about 5 connections in 1.5 s), not start again at 50 ms because the broker said yes first.
    let cut = Cut { after: 1, acked: 0, linger: Duration::ZERO, every: true };
    let b = Broker::start(Tls::V13, Some(cut)).await;
    let mut s = settings(&b);
    s.limits.reconnect_max_ms = 1_600;
    let m = Mqtt::start(&s).unwrap().unwrap();
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    let tries = b.hellos().len();
    assert!((3..=8).contains(&tries), "{tries} connections in 1.5 s");
    assert!(m.stats().last_error.is_some());
    m.stop().await;
}

#[tokio::test]
async fn a_copy_of_the_pinned_certificate_without_its_key_is_refused() {
    // someone in the middle has the broker's certificate (it is public) but not its private key: the pin matches,
    // and the handshake's signature, made with another key, must not
    let real = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let other = rcgen::KeyPair::generate().unwrap();
    let key = rustls::pki_types::PrivateKeyDer::Pkcs8(other.serialize_der().into());
    let signer = rustls::crypto::ring::sign::any_supported_type(&key).unwrap();
    let certified = rustls::sign::CertifiedKey::new(vec![real.cert.der().clone()], signer);
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .with_no_client_auth()
        .with_cert_resolver(Arc::new(rustls::sign::SingleCertAndKey::from(certified)));
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let got_bytes = Arc::new(AtomicBool::new(false));
    let got = got_bytes.clone();
    tokio::spawn(async move {
        while let Ok((tcp, _)) = listener.accept().await {
            if let Ok(mut s) = acceptor.accept(tcp).await {
                let mut buf = [0u8; 64];
                if matches!(s.read(&mut buf).await, Ok(n) if n > 0) {
                    got.store(true, Ordering::SeqCst);
                }
            }
        }
    });
    let s = Settings {
        enabled: true,
        broker: format!("mqtts://127.0.0.1:{port}"),
        cert_sha256: Some(mqtt::security::fingerprint(real.cert.der())),
        username: Some("ha".into()),
        password: Some("synthetic-password".into()),
        limits: Limits { reconnect_min_ms: 50, reconnect_max_ms: 200, connect_timeout_s: 2, ..Limits::default() },
        ..Settings::default()
    };
    let m = Mqtt::start(&s).unwrap().unwrap();
    wait_for("a refused connection", || m.stats().last_error.is_some()).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let st = m.stats();
    assert!(!st.connected && st.connects == 0, "{st:?}");
    assert!(!got_bytes.load(Ordering::SeqCst), "nothing, not even the password, was sent");
    let err = st.last_error.unwrap_or_default();
    assert!(err.contains("not signed with the key of the broker's certificate"), "{err}");
    m.stop().await;
}

#[tokio::test]
async fn names_too_long_for_a_packet_dont_stop_publishing() {
    // a name longer than a packet may be (256 KiB) would fail the connection; since what wasn't confirmed is sent
    // first again, that one car would stop publishing for every car, for good
    let b = Broker::start(Tls::V13, None).await;
    let m = Mqtt::start(&settings(&b)).unwrap().unwrap();
    let long = "x".repeat(300 * 1024);
    m.publish_now(&info(&long), &now(1, 80.0), Some(&long));
    m.publish_event(Event::DriveEnded {
        drive: Box::new(drive(1)),
        start_place: Some(long.clone()),
        end_place: Some(long.clone()),
    });
    m.publish_now(&info("Two"), &now(2, 60.0), None);
    wait_for("both cars and the event", || {
        let l = b.latest();
        ["sentry-mate/cars/c1/soc", "sentry-mate/cars/c2/soc", "sentry-mate/cars/c1/event"]
            .iter()
            .all(|t| l.contains_key(*t))
    })
    .await;
    let l = b.latest();
    assert_eq!(l["sentry-mate/cars/c1/place"].len(), mqtt::render::MAX_NAME, "cut to fit");
    let config: serde_json::Value = serde_json::from_str(&l["homeassistant/sensor/sentry-mate_c1/soc/config"]).unwrap();
    assert_eq!(config["device"]["name"].as_str().map(str::len), Some(mqtt::render::MAX_NAME));
    assert_eq!(m.stats().connects, 1, "the link never dropped");
    m.stop().await;
}

// `#[tokio::test]` runs on one thread: the publisher shares it with every other task
#[tokio::test]
async fn going_through_many_cars_lets_other_tasks_run() {
    let b = Broker::start(Tls::V13, None).await;
    let mut s = settings(&b);
    s.discovery = false;
    let m = Arc::new(Mqtt::start(&s).unwrap().unwrap());
    let cars = 2_000u32;
    for car in 0..cars {
        m.publish_now(&info("Car"), &now(car, 50.0), None);
    }
    // the last message of the last car is in and acknowledged: the publisher is idle, waiting for work
    let last = format!("sentry-mate/cars/c{}/location", cars - 1);
    wait_for("every car", || {
        let got = b.got();
        got.iter().any(|g| g.topic == last) && m.stats().delivered as usize == got.len()
    })
    .await;
    // the same values again: nothing to send, but going through 2,000 cars must not hold the thread until done
    for car in 0..cars {
        m.publish_now(&info("Car"), &now(car, 50.0), None);
    }
    let m2 = m.clone();
    let waiting_when_we_ran = tokio::spawn(async move { m2.stats().states_waiting }).await.unwrap();
    assert!(waiting_when_we_ran > 0, "another task ran only once the publisher had gone through every car");
    wait_for("the queue to drain", || m.stats().states_waiting == 0).await;
    let before = b.got().len();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(b.got().len(), before, "nothing new, nothing sent");
    Arc::into_inner(m).unwrap().stop().await;
}

#[tokio::test]
async fn coalesces_2000_cars_while_the_broker_is_away() {
    let b = Broker::start(Tls::V13, None).await;
    b.open.store(false, Ordering::SeqCst);
    let mut s = settings(&b);
    s.discovery = false;
    let m = Mqtt::start(&s).unwrap().unwrap();

    let cars = 2_000u32;
    let t = Instant::now();
    for round in 0..5 {
        for car in 0..cars {
            m.publish_now(&info("Car"), &now(car, f64::from(round)), None);
        }
    }
    let queued = t.elapsed();
    assert!(queued < Duration::from_secs(2), "callers never wait: {queued:?}");
    let st = m.stats();
    assert_eq!(st.states_waiting, cars as usize, "one state per car while away, not 10,000");
    assert!(!st.connected);

    b.open.store(true, Ordering::SeqCst);
    let t = Instant::now();
    wait_for("every car", || {
        let got = b.got();
        got.iter().filter(|g| g.topic.ends_with("/soc")).count() >= cars as usize
    })
    .await;
    let sent = t.elapsed();
    let got = b.got();
    let mut socs: HashMap<&str, Vec<&str>> = HashMap::new();
    for g in got.iter().filter(|g| g.topic.ends_with("/soc")) {
        socs.entry(g.topic.as_str()).or_default().push(g.payload.as_str());
    }
    assert_eq!(socs.len(), cars as usize);
    assert!(socs.values().all(|v| v == &["4"]), "each car's newest state, once");
    eprintln!(
        "2,000 cars x 5 updates queued in {queued:?}; {} messages over TLS in {sent:?} after the broker came back",
        got.len()
    );

    // a second round: only what changed goes out
    let before = b.got().len();
    for car in 0..cars {
        let mut n = now(car, 4.0);
        n.sample.outside_temp_c = Some(13.0);
        m.publish_now(&info("Car"), &n, None);
    }
    wait_for("the new temperatures", || {
        b.got()[before..].iter().filter(|g| g.topic.ends_with("/outside_temp")).count() >= cars as usize
    })
    .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let round2 = b.got().len() - before;
    assert_eq!(round2, cars as usize, "one changed value per car, nothing else");
    m.stop().await;
}

#[tokio::test]
async fn the_test_button_publishes_one_message_and_says_why_not() {
    let b = Broker::start(Tls::V13, None).await;
    let s = settings(&b);
    let p = mqtt::probe::probe(&s).await;
    assert!(p.ok, "{p:?}");
    assert_eq!(p.topic.as_deref(), Some("sentry-mate/test"));
    assert!(p.latency_ms.is_some() && p.tls);
    let got = b.got();
    assert_eq!(got.len(), 1, "one message: {got:?}");
    assert_eq!((got[0].topic.as_str(), got[0].retain), ("sentry-mate/test", false));
    let hello = &b.hellos()[0];
    assert_eq!(hello.client_id, "sentry-mate-test-test", "its own client id");
    assert!(hello.will.is_none(), "no last will: the test never says offline");
    // a wrong pin: nothing sent
    let wrong = Settings { cert_sha256: Some("00".repeat(32)), ..settings(&b) };
    let p = mqtt::probe::probe(&wrong).await;
    assert_eq!(p.error, Some("pin_mismatch"), "{p:?}");
    assert!(p.message.contains(&b.fingerprint), "{}", p.message);
    assert_eq!(b.hellos().len(), 1);
}
