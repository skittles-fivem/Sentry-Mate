//! The subscriber to TeslaMate's MQTT broker. Read only, by construction: it subscribes to `<prefix>/cars/+/#` at
//! QoS 0 and nothing else, never publishes (the client handle is used for that one SUBSCRIBE), sets no last will and
//! asks for a clean session, so the broker keeps nothing of it. TLS through rustls ([`crate::tls`]): webpki roots
//! and the host name, or the certificate's pinned SHA-256. On any error the connection is dropped whole and tried
//! again after a wait that doubles from [`RECONNECT_MIN`] to [`RECONNECT_MAX`] (back to the start once a connection
//! lasted [`STABLE_AFTER`]).

use crate::store::{MqttConfig, Secret};
use crate::tls;
use rumqttc::{
    AsyncClient, ConnectReturnCode, ConnectionError, Event, MqttOptions, NetworkOptions, Packet, QoS, Transport,
};
use std::time::{Duration, Instant};

pub const RECONNECT_MIN: Duration = Duration::from_secs(1);
pub const RECONNECT_MAX: Duration = Duration::from_secs(60);
pub const STABLE_AFTER: Duration = Duration::from_secs(60);

/// Why a connection failed: the API's code and plain words (never a password).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    /// `connection_refused`, `timeout`, `auth_failed`, `tls_failed` or `pin_mismatch`.
    pub code: &'static str,
    pub message: String,
}

/// Where to connect and as whom.
#[derive(Debug, Clone)]
pub struct Broker {
    pub host: String,
    pub port: u16,
    pub tls: bool,
    pub pin: Option<[u8; 32]>,
    pub username: Option<String>,
    pub password: Option<Secret>,
    pub prefix: String,
}

impl Broker {
    pub fn new(c: &MqttConfig, password: Option<Secret>) -> Broker {
        Broker {
            host: c.host.clone(),
            port: c.port,
            tls: c.tls,
            pin: c.pin_sha256.as_deref().and_then(tls::parse_pin),
            username: c.username.clone().filter(|u| !u.is_empty()),
            password,
            prefix: c.prefix.clone(),
        }
    }

    /// The one subscription: every value of every car under the prefix.
    pub fn filter(&self) -> String {
        format!("{}/cars/+/#", self.prefix)
    }

    /// `host:port`, as the page and `/network` show it.
    pub fn address(&self) -> String {
        if self.host.contains(':') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }

    fn options(&self, client_id: &str, connect_timeout: Duration) -> Result<(MqttOptions, NetworkOptions), Failure> {
        let mut o = MqttOptions::new(client_id, self.host.clone(), self.port);
        o.set_keep_alive(Duration::from_secs(30)).set_clean_session(true);
        // a TeslaMate value is a short text; bounded so a wrong broker can't hand us megabytes
        o.set_max_packet_size(64 * 1024, 64 * 1024);
        if let Some(u) = &self.username {
            o.set_credentials(u.clone(), self.password.as_ref().map(|p| p.expose().to_string()).unwrap_or_default());
        }
        if self.tls {
            let c = tls::client_config(self.pin)
                .map_err(|e| Failure { code: "tls_failed", message: format!("TLS: {e}") })?;
            o.set_transport(Transport::tls_with_config(rumqttc::TlsConfiguration::Rustls(c)));
        }
        let mut net = NetworkOptions::new();
        net.set_connection_timeout(connect_timeout.as_secs().max(1));
        Ok((o, net))
    }
}

/// A client id of its own for each connection (a test beside the running subscriber keeps both connected).
fn client_id(what: &str) -> String {
    let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    format!("sentry-mate-{what}-{:06x}", (n as u64 ^ u64::from(std::process::id())) & 0xff_ffff)
}

fn tls_error(e: &ConnectionError) -> Option<Option<&rustls::Error>> {
    use rumqttc::TlsError;
    match e {
        ConnectionError::Tls(TlsError::TLS(t)) => Some(Some(t)),
        ConnectionError::Tls(TlsError::Io(io)) => Some(io.get_ref().and_then(|r| r.downcast_ref::<rustls::Error>())),
        ConnectionError::Tls(_) => Some(None),
        ConnectionError::Io(io) => io.get_ref().and_then(|r| r.downcast_ref::<rustls::Error>()).map(Some),
        _ => None,
    }
}

/// A failed connection as the API says it.
pub fn classify(e: &ConnectionError) -> Failure {
    let (code, message) = match tls_error(e) {
        Some(Some(t)) if tls::is_pin_mismatch(t) => ("pin_mismatch", t.to_string()),
        Some(Some(rustls::Error::InvalidCertificate(rustls::CertificateError::UnknownIssuer))) => (
            "tls_failed",
            "the broker's certificate is not from a public CA (self-signed?): pin its SHA-256 instead".to_string(),
        ),
        Some(Some(t)) => ("tls_failed", format!("TLS: {t}")),
        Some(None) => ("tls_failed", format!("TLS: {e}")),
        None => match e {
            ConnectionError::ConnectionRefused(
                ConnectReturnCode::BadUserNamePassword | ConnectReturnCode::NotAuthorized,
            ) => ("auth_failed", "the broker refused the user name or password".to_string()),
            ConnectionError::NetworkTimeout | ConnectionError::FlushTimeout => {
                ("timeout", "the broker did not answer in time".to_string())
            }
            ConnectionError::Io(io) if io.kind() == std::io::ErrorKind::TimedOut => {
                ("timeout", "the broker did not answer in time".to_string())
            }
            other => ("connection_refused", other.to_string()),
        },
    };
    Failure { code, message }
}

/// What the subscriber does with what comes in.
pub trait Handler: Send {
    /// The broker accepted the connection (the subscription is on its way).
    fn connected(&mut self, now_ms: u64);
    /// A value: the topic and its payload.
    fn message(&mut self, topic: &str, payload: &[u8], now_ms: u64);
    /// About once a second, connected or not.
    fn tick(&mut self, now_ms: u64);
    /// The connection failed or dropped; the next try is at `retry_ms`.
    fn failed(&mut self, failure: &Failure, retry_ms: u64);
}

pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// One connection, until it fails. `Ok(())` is never returned while the broker keeps it up.
async fn session(
    broker: &Broker,
    connect_timeout: Duration,
    handler: &mut impl Handler,
    up_since: &mut Option<Instant>,
) -> Failure {
    let (options, net) = match broker.options(&client_id("ro"), connect_timeout) {
        Ok(o) => o,
        Err(f) => return f,
    };
    let (client, mut eventloop) = AsyncClient::new(options, 8);
    eventloop.set_network_options(net);
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let filter = broker.filter();
    loop {
        tokio::select! {
            ev = eventloop.poll() => match ev {
                Ok(Event::Incoming(Packet::ConnAck(_))) => {
                    *up_since = Some(Instant::now());
                    if client.try_subscribe(filter.clone(), QoS::AtMostOnce).is_err() {
                        return Failure { code: "connection_refused", message: "the subscription could not be sent".into() };
                    }
                    handler.connected(now_ms());
                }
                Ok(Event::Incoming(Packet::Publish(p))) => handler.message(&p.topic, &p.payload, now_ms()),
                Ok(Event::Incoming(Packet::SubAck(a))) => {
                    if a.return_codes.iter().any(|c| matches!(c, rumqttc::SubscribeReasonCode::Failure)) {
                        return Failure {
                            code: "auth_failed",
                            message: format!("the broker refused the subscription to {filter}"),
                        };
                    }
                }
                Ok(_) => {}
                Err(e) => return classify(&e),
            },
            _ = tick.tick() => handler.tick(now_ms()),
        }
    }
}

/// Runs forever (the service aborts the task): connect, read, and on a failure wait and connect again.
pub async fn run(broker: Broker, connect_timeout: Duration, backoff: (Duration, Duration), mut handler: impl Handler) {
    let (min, max) = backoff;
    let mut delay = min;
    loop {
        let mut up_since = None;
        let f = session(&broker, connect_timeout, &mut handler, &mut up_since).await;
        if up_since.is_some_and(|t| t.elapsed() >= STABLE_AFTER) {
            delay = min;
        }
        tracing::warn!(broker = %broker.address(), code = f.code, "local TeslaMate: MQTT connection failed: {}", f.message);
        handler.failed(&f, now_ms() + u64::try_from(delay.as_millis()).unwrap_or(u64::MAX));
        // ticks go on while waiting (the samples of a dropped connection still go out)
        let until = tokio::time::Instant::now() + delay;
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        loop {
            tokio::select! {
                () = tokio::time::sleep_until(until) => break,
                _ = tick.tick() => handler.tick(now_ms()),
            }
        }
        delay = (delay * 2).min(max);
    }
}

/// How a test of a broker went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probe {
    pub failure: Option<Failure>,
    /// From the start to the broker's CONNACK.
    pub latency_ms: Option<u64>,
    /// TeslaMate's car ids seen while listening.
    pub cars_seen: Vec<String>,
}

/// Connects, subscribes, listens for `listen`, disconnects. Publishes nothing.
pub async fn probe(broker: &Broker, connect_timeout: Duration, listen: Duration) -> Probe {
    let (options, net) = match broker.options(&client_id("test"), connect_timeout) {
        Ok(o) => o,
        Err(f) => return Probe { failure: Some(f), latency_ms: None, cars_seen: Vec::new() },
    };
    let (client, mut eventloop) = AsyncClient::new(options, 8);
    eventloop.set_network_options(net);
    let started = Instant::now();
    let mut latency = None;
    let mut cars: Vec<String> = Vec::new();
    let mut listen_until: Option<tokio::time::Instant> = None;
    let deadline = tokio::time::Instant::now() + connect_timeout + Duration::from_secs(2);
    let failure = loop {
        let until = listen_until.unwrap_or(deadline);
        let ev = tokio::select! {
            ev = eventloop.poll() => ev,
            () = tokio::time::sleep_until(until) => {
                if listen_until.is_some() {
                    break None;
                }
                break Some(Failure { code: "timeout", message: format!("{} did not answer in time", broker.address()) });
            }
        };
        match ev {
            Ok(Event::Incoming(Packet::ConnAck(_))) => {
                latency = Some(u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX));
                if client.try_subscribe(broker.filter(), QoS::AtMostOnce).is_err() {
                    break Some(Failure {
                        code: "connection_refused",
                        message: "the subscription could not be sent".into(),
                    });
                }
                listen_until = Some(tokio::time::Instant::now() + listen);
            }
            Ok(Event::Incoming(Packet::Publish(p))) => {
                if let Some((id, _)) = crate::map::split_topic(&broker.prefix, &p.topic)
                    && !cars.iter().any(|c| c == id)
                    && cars.len() < crate::MAX_CARS
                {
                    cars.push(id.to_string());
                }
            }
            Ok(_) => {}
            Err(e) => break Some(classify(&e)),
        }
    };
    if failure.is_none() {
        let _ = client.try_disconnect();
        let _ = tokio::time::timeout(Duration::from_millis(500), eventloop.poll()).await;
    }
    cars.sort_by_key(|c| c.parse::<u64>().unwrap_or(u64::MAX));
    Probe { failure, latency_ms: latency, cars_seen: cars }
}
