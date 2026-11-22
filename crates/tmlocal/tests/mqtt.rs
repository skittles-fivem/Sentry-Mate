#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! The subscriber against a tiny hand-rolled MQTT 3.1.1 broker on this machine: it connects with no last will,
//! subscribes to `<prefix>/cars/+/#` only, never publishes, and a matched car's values reach the sink as samples
//! and state facts. A password over plain TCP is refused before anything is stored or connects. Every value is made
//! up.

use model::{CarId, Sample};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tesla::ApiState;
use tmlocal::{ApiFact, Draft, Match, MqttConfig, Password, Secret, Service, ServiceError, Sink, SinkError};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// One `points` call.
type Points = (CarId, Vec<Sample>, Vec<ApiFact>);

#[derive(Default)]
struct TestSink {
    points: Mutex<Vec<Points>>,
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
                                    c.write_all(&[0x20, 0x02, 0x00, 0x00]).await.unwrap();
                                }
                                8 => {
                                    let n = usize::from(u16::from_be_bytes([body[2], body[3]]));
                                    let filter = String::from_utf8_lossy(&body[4..4 + n]).to_string();
                                    seen.lock().unwrap().filters.push(filter);
                                    c.write_all(&[0x90, 0x03, body[0], body[1], 0x00]).await.unwrap();
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
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let (v, _) = vault::Vault::create(&data, &dir.path().join("keys"), vault::Kdf::FAST_FOR_TESTS).unwrap();
    let sink = Arc::new(TestSink::default());
    let opts = tmlocal::Options {
        connect_timeout: Duration::from_secs(3),
        backoff: (Duration::from_millis(200), Duration::from_secs(1)),
        test_listen: Duration::from_millis(800),
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
