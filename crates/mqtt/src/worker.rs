//! The connection, run as one task: connect, publish what waits in the queue, and on any error drop the connection
//! whole and try again after a wait that doubles (reset once a connection has lasted [`STABLE_AFTER`], so a broker
//! that accepts and then drops the connection, e.g. another client with the same id, isn't hammered every second).
//! Each try gets a fresh rumqttc client and event loop, so nothing from a dead connection is replayed out of order.
//!
//! Nothing is lost to a dropped link: the broker acknowledges QoS 1 messages in the order it got them (MQTT 3.1.1,
//! 4.6), so counting its PUBACKs tells exactly which items it has. Items it hasn't confirmed when the link drops go
//! back to the queue (unless a newer state of that car is already waiting). That window is bounded by rumqttc's
//! in-flight limit plus its request channel, a few hundred messages, whatever the number of cars.

use crate::cache::{Sent, SentCache, fingerprint};
use crate::discovery;
use crate::queue::{Item, NowItem, Pending, Queue};
use crate::render::{self, Field, Layout, Message};
use crate::settings::STABLE_AFTER;
use model::CarId;
use rumqttc::{AsyncClient, EventLoop, MqttOptions, NetworkOptions, Outgoing, Packet, QoS};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};
use tokio::sync::{Notify, watch};

/// Requests rumqttc holds before `publish` waits; with the in-flight limit, the most a dropped link can leave
/// unconfirmed.
pub(crate) const CHANNEL: usize = 64;
pub(crate) const INFLIGHT: u16 = 100;

/// Items worked through between yields to the runtime. A car with nothing new sends nothing (no await at all), and
/// handing a message to rumqttc doesn't wait while its channel has room: without this, going through many cars would
/// hold a runtime thread (and every other task on it) until the queue is empty.
const ITEMS_PER_YIELD: u32 = 64;

/// What the callers and the connection task share.
#[derive(Debug)]
pub(crate) struct Shared {
    queue: Mutex<Queue>,
    pub wake: Notify,
    pub connected: AtomicBool,
    pub connects: AtomicU64,
    pub delivered: AtomicU64,
    /// Why the last connection failed or dropped: the page's code (as [`crate::probe`] gives it) and the words.
    last_error: Mutex<Option<(&'static str, String)>>,
}

impl Shared {
    pub fn new(queue: Queue) -> Shared {
        Shared {
            queue: Mutex::new(queue),
            wake: Notify::new(),
            connected: AtomicBool::new(false),
            connects: AtomicU64::new(0),
            delivered: AtomicU64::new(0),
            last_error: Mutex::new(None),
        }
    }

    /// The queue; a panic elsewhere while it was held leaves it usable (it is consistent after every operation).
    pub fn queue(&self) -> MutexGuard<'_, Queue> {
        self.queue.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn last_error(&self) -> MutexGuard<'_, Option<(&'static str, String)>> {
        self.last_error.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Everything the task needs, fixed for its life (new settings: a new publisher).
pub(crate) struct Config {
    pub layout: Layout,
    pub options: MqttOptions,
    pub connect_timeout_s: u64,
    pub cache_cars: usize,
    pub reconnect_min: Duration,
    pub reconnect_max: Duration,
    /// The broker and how it is trusted, for the log.
    pub describe: String,
}

/// One connection's state, shared by its two halves (reading the network, publishing).
#[derive(Default)]
struct Conn {
    up: AtomicBool,
    /// When the broker accepted it.
    up_since: OnceLock<Instant>,
    up_notify: Notify,
    /// PUBACKs received on this connection.
    acked: AtomicU64,
}

enum Ended {
    Lost(&'static str, String),
    Closed,
}

/// Items handed to the connection, each with the number of messages sent up to and including its own.
#[derive(Default)]
struct Flight {
    items: VecDeque<(u64, Item)>,
}

impl Flight {
    fn push(&mut self, last_seq: u64, item: Item) {
        self.items.push_back((last_seq, item));
    }

    /// Lets go of every item whose messages are all acknowledged.
    fn confirm(&mut self, acked: u64) {
        while self.items.front().is_some_and(|(seq, _)| *seq <= acked) {
            self.items.pop_front();
        }
    }

    fn unconfirmed(&mut self, acked: u64) -> Vec<Item> {
        self.confirm(acked);
        self.items.drain(..).map(|(_, item)| item).collect()
    }
}

pub(crate) async fn run(shared: std::sync::Arc<Shared>, cfg: Config, mut stop: watch::Receiver<bool>) {
    let mut cache = SentCache::new(cfg.cache_cars);
    let mut delay = cfg.reconnect_min;
    loop {
        if *stop.borrow() {
            return;
        }
        let (client, mut eventloop) = AsyncClient::new(cfg.options.clone(), CHANNEL);
        let mut net = NetworkOptions::new();
        net.set_connection_timeout(cfg.connect_timeout_s);
        eventloop.set_network_options(net);
        let conn = Conn::default();
        let mut flight = Flight::default();
        let ended = {
            let driver = drive(&mut eventloop, &conn, &shared, &cfg.describe);
            let publisher = publish(&client, &conn, &shared, &cfg.layout, &mut cache, &mut flight, stop.clone());
            tokio::select! {
                e = driver => e,
                () = publisher => Ended::Closed,
            }
        };
        shared.connected.store(false, Ordering::SeqCst);
        let unconfirmed = flight.unconfirmed(conn.acked.load(Ordering::SeqCst));
        shared.queue().requeue(unconfirmed);
        match ended {
            Ended::Closed => return,
            Ended::Lost(code, e) => {
                if conn.up_since.get().is_some_and(|t| t.elapsed() >= STABLE_AFTER) {
                    delay = cfg.reconnect_min;
                }
                tracing::warn!("MQTT {}: {e}; trying again in {:.1} s", cfg.describe, delay.as_secs_f64());
                *shared.last_error() = Some((code, e));
                tokio::select! {
                    () = tokio::time::sleep(delay) => {}
                    r = stop.changed() => if r.is_err() { return },
                }
                delay = (delay * 2).min(cfg.reconnect_max);
            }
        }
    }
}

/// Reads the network until the connection ends; never blocks on the publisher.
async fn drive(eventloop: &mut EventLoop, conn: &Conn, shared: &Shared, describe: &str) -> Ended {
    loop {
        match eventloop.poll().await {
            Ok(rumqttc::Event::Incoming(Packet::ConnAck(_))) => {
                let _ = conn.up_since.set(Instant::now());
                conn.up.store(true, Ordering::SeqCst);
                conn.up_notify.notify_one();
                shared.connected.store(true, Ordering::SeqCst);
                shared.connects.fetch_add(1, Ordering::SeqCst);
                *shared.last_error() = None;
                tracing::info!("MQTT connected to {describe}");
            }
            Ok(rumqttc::Event::Incoming(Packet::PubAck(_))) => {
                conn.acked.fetch_add(1, Ordering::SeqCst);
                shared.delivered.fetch_add(1, Ordering::SeqCst);
            }
            Ok(rumqttc::Event::Outgoing(Outgoing::Disconnect)) => return Ended::Closed,
            Ok(_) => {}
            Err(e) => return Ended::Lost(crate::probe::classify(&e), explain(&e)),
        }
    }
}

/// The reason a connection failed, in words that say what to change where we can tell.
pub(crate) fn explain(e: &rumqttc::ConnectionError) -> String {
    use rumqttc::{ConnectReturnCode as Code, ConnectionError as E, TlsError};
    let tls = match e {
        E::Tls(TlsError::TLS(t)) => Some(t),
        E::Tls(TlsError::Io(io)) => io.get_ref().and_then(|r| r.downcast_ref::<rustls::Error>()),
        _ => None,
    };
    if let Some(t) = tls {
        use rustls::{AlertDescription, CertificateError as C, Error as R};
        return match t {
            R::InvalidCertificate(C::Other(o)) => o.0.to_string(),
            R::InvalidCertificate(C::UnknownIssuer) => {
                "the broker's certificate is not from a public CA (self-signed?): pin its SHA-256 instead".into()
            }
            R::InvalidCertificate(C::NotValidForName | C::NotValidForNameContext { .. }) => {
                "the broker's certificate is not for this host name".into()
            }
            R::InvalidCertificate(C::Expired | C::ExpiredContext { .. }) => {
                "the broker's certificate has expired".into()
            }
            R::InvalidCertificate(C::BadSignature) => {
                "the handshake was not signed with the key of the broker's certificate: something else answers".into()
            }
            R::AlertReceived(AlertDescription::ProtocolVersion) => {
                "the broker doesn't offer TLS 1.3 (allow TLS 1.2 for an old broker)".into()
            }
            other => format!("TLS: {other}"),
        };
    }
    match e {
        E::ConnectionRefused(Code::BadUserNamePassword) => "the broker refused the user name or password".into(),
        E::ConnectionRefused(Code::NotAuthorized) => "the broker refused: not authorized".into(),
        E::ConnectionRefused(Code::BadClientId) => "the broker refused the client id".into(),
        E::ConnectionRefused(Code::RefusedProtocolVersion) => "the broker doesn't speak MQTT 3.1.1".into(),
        other => other.to_string(),
    }
}

/// Publishes what waits, once the broker has accepted the connection. Returns only when told to stop before the
/// connection is up; when stopping with it up, says `offline` and disconnects, and the driver ends the connection.
async fn publish(
    client: &AsyncClient,
    conn: &Conn,
    shared: &Shared,
    layout: &Layout,
    cache: &mut SentCache,
    flight: &mut Flight,
    mut stop: watch::Receiver<bool>,
) {
    loop {
        if *stop.borrow() {
            return;
        }
        if conn.up.load(Ordering::SeqCst) {
            break;
        }
        tokio::select! {
            () = conn.up_notify.notified() => {}
            r = stop.changed() => if r.is_err() { return },
        }
    }
    cache.clear();
    let mut seq = 0u64;
    let status = layout.topics.status();
    send(client, &mut seq, Message { topic: status.clone(), payload: "online".into(), retain: true }).await;
    let mut since_yield = 0u32;
    loop {
        if *stop.borrow() {
            send(client, &mut seq, Message { topic: status, payload: "offline".into(), retain: true }).await;
            let _ = client.disconnect().await;
            return std::future::pending().await;
        }
        // here, before an item is taken, never between taking it and putting it in `flight`: a link that drops while
        // this waits ends the task at this point, and an item held then would be lost
        since_yield += 1;
        if since_yield >= ITEMS_PER_YIELD {
            since_yield = 0;
            tokio::task::yield_now().await;
        }
        flight.confirm(conn.acked.load(Ordering::SeqCst));
        let next = shared.queue().take();
        let Some(item) = next else {
            tokio::select! {
                () = shared.wake.notified() => {}
                r = stop.changed() => if r.is_err() { return std::future::pending().await },
            }
            continue;
        };
        let msgs = messages_for(layout, &item, cache);
        if msgs.is_empty() {
            continue;
        }
        flight.push(seq + msgs.len() as u64, item);
        for m in msgs {
            send(client, &mut seq, m).await;
        }
    }
}

async fn send(client: &AsyncClient, seq: &mut u64, m: Message) {
    // rumqttc refuses only a topic with wildcards, which checked prefixes can't make: such a message is skipped
    if client.publish(m.topic, QoS::AtLeastOnce, m.retain, m.payload).await.is_ok() {
        *seq += 1;
    }
}

fn messages_for(layout: &Layout, item: &Item, cache: &mut SentCache) -> Vec<Message> {
    match item {
        Item::Event(e) => vec![render::event_message(layout, e)],
        Item::State(car, Pending::Forget) => {
            cache.forget(*car);
            let mut msgs = render::clear_messages(layout, *car);
            msgs.extend(discovery::removals(layout, *car));
            msgs
        }
        Item::State(car, Pending::Now(n)) => changed(layout, *car, n, cache.entry(*car)),
    }
}

/// A car's discovery configs (when its name or model is new on this connection) and the values that changed since
/// they were last sent on it.
fn changed(layout: &Layout, car: CarId, n: &NowItem, sent: &mut Sent) -> Vec<Message> {
    let mut out = Vec::new();
    if layout.discovery {
        let h = fingerprint(&n.info);
        if sent.discovery != h {
            out.extend(discovery::configs(layout, car, &n.info));
            sent.discovery = h;
        }
    }
    for f in Field::ALL {
        let slot = &mut sent.fields[f.index()];
        // a value that was sent and is no longer known (the route ended, the charge stopped) is cleared on the
        // broker with an empty retained message, once; one never sent stays unpublished
        let payload = match render::field_payload(layout, f, &n.now, n.place.as_deref()) {
            Some(p) => p,
            None if *slot != 0 => String::new(),
            None => continue,
        };
        let h = fingerprint(&payload);
        if *slot != h {
            *slot = h;
            out.push(Message { topic: layout.topics.field(car, f.key()), payload, retain: true });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::CarInfo;
    use crate::render::tests::{layout, parked};
    use crate::settings::Units;

    fn item(soc: f64) -> NowItem {
        let mut now = parked(7);
        now.sample.soc = Some(soc);
        NowItem { info: CarInfo { name: "Y".into(), model: None }, now, place: Some("Home".into()) }
    }

    fn keys(msgs: &[Message]) -> Vec<&str> {
        msgs.iter().map(|m| m.topic.rsplit('/').next().unwrap_or_default()).collect()
    }

    #[test]
    fn only_changes_are_sent_again() {
        let l = layout(false, Units::default());
        let mut sent = Sent::default();
        let first = changed(&l, 7, &item(80.0), &mut sent);
        assert!(first.iter().any(|m| m.topic.starts_with("homeassistant/")), "discovery the first time");
        assert!(first.iter().any(|m| m.topic == "sentry-mate/cars/c7/soc"));
        assert!(changed(&l, 7, &item(80.0), &mut sent).is_empty(), "nothing new, nothing sent");
        let mut later = item(79.0);
        later.now.at_ms += 60_000;
        assert_eq!(keys(&changed(&l, 7, &later, &mut sent)), ["soc", "updated"]);
        let mut renamed = item(79.0);
        renamed.info.name = "Renamed".into();
        renamed.now.at_ms = later.now.at_ms;
        let again = changed(&l, 7, &renamed, &mut sent);
        assert!(
            !again.is_empty() && again.iter().all(|m| m.topic.starts_with("homeassistant/")),
            "a new name: configs again"
        );
    }

    #[test]
    fn a_value_no_longer_known_is_cleared_once() {
        let l = layout(false, Units::default());
        let mut sent = Sent::default();
        let topic = "sentry-mate/cars/c7/energy_at_destination";
        let of = |msgs: &[Message]| msgs.iter().find(|m| m.topic == topic).map(|m| (m.payload.clone(), m.retain));
        // never navigated: nothing is published for the destination
        assert_eq!(of(&changed(&l, 7, &item(80.0), &mut sent)), None);
        let mut route = item(80.0);
        route.now.sample.extra.navigating = Some(true);
        route.now.sample.extra.energy_at_destination_kwh = Some(42.0);
        assert_eq!(of(&changed(&l, 7, &route, &mut sent)), Some(("42".to_string(), true)));
        // the route ended: the broker's retained 42 is cleared with an empty retained message, once
        let mut ended = item(80.0);
        ended.now.sample.extra.navigating = Some(false);
        assert_eq!(of(&changed(&l, 7, &ended, &mut sent)), Some((String::new(), true)));
        assert_eq!(of(&changed(&l, 7, &ended, &mut sent)), None, "cleared once, not again");
        // and a new route's figure is sent again, the same one too
        assert_eq!(of(&changed(&l, 7, &route, &mut sent)), Some(("42".to_string(), true)));
    }

    /// What a car's newest values cost to turn into messages (docs/plans/performance.md, "MQTT"): by hand,
    /// `cargo test --release -p mqtt measure_a_sample -- --ignored --nocapture`.
    #[test]
    #[ignore = "a measurement, run by hand in release"]
    #[allow(clippy::print_stdout)]
    fn measure_a_sample() {
        let l = layout(true, Units::default());
        let mut sent = Sent::default();
        let first = changed(&l, 7, &item(80.0), &mut sent);
        let (configs, values) = first
            .iter()
            .fold((0, 0), |(c, v), m| if m.topic.starts_with("homeassistant/") { (c + 1, v) } else { (c, v + 1) });
        let bytes: usize = first.iter().map(|m| m.topic.len() + m.payload.len()).sum();
        let n = 20_000u32;
        let t = std::time::Instant::now();
        for _ in 0..n {
            assert!(changed(&l, 7, &item(80.0), &mut sent).is_empty());
        }
        let same = t.elapsed().as_secs_f64() * 1e6 / f64::from(n);
        let t = std::time::Instant::now();
        let mut msgs = 0;
        for i in 0..n {
            let mut it = item(50.0 + f64::from(i % 300) / 10.0);
            it.now.at_ms += u64::from(i) * 1000;
            msgs += changed(&l, 7, &it, &mut sent).len();
        }
        let moving = t.elapsed().as_secs_f64() * 1e6 / f64::from(n);
        let t = std::time::Instant::now();
        for i in 0..2_000u32 {
            let mut it = item(80.0);
            it.info.name = format!("Car {i}");
            std::hint::black_box(changed(&l, 7, &it, &mut sent));
        }
        let renamed = t.elapsed().as_secs_f64() * 1e6 / 2_000.0;
        println!(
            "mqtt: the first time {configs} discovery configs and {values} values ({bytes} bytes); a sample with \
             nothing new {same:.1} us and no message; one with the charge level and time new {moving:.1} us and \
             {:.1} messages; a renamed car (every config again) {renamed:.0} us",
            f64::from(u32::try_from(msgs).unwrap_or(u32::MAX)) / f64::from(n)
        );
    }

    #[test]
    fn forget_clears_everything() {
        let l = layout(true, Units::default());
        let mut cache = SentCache::new(16);
        let _ = messages_for(&l, &Item::State(7, Pending::Now(Box::new(item(50.0)))), &mut cache);
        let msgs = messages_for(&l, &Item::State(7, Pending::Forget), &mut cache);
        assert!(msgs.iter().all(|m| m.payload.is_empty() && m.retain));
        assert!(msgs.iter().any(|m| m.topic == "sentry-mate/cars/c7/soc"));
        assert!(msgs.iter().any(|m| m.topic == "homeassistant/sensor/sentry-mate_c7/soc/config"));
        assert_eq!(cache.len(), 0);
        // and a car that comes back is sent in full
        let back = messages_for(&l, &Item::State(7, Pending::Now(Box::new(item(50.0)))), &mut cache);
        assert!(back.iter().any(|m| m.topic == "sentry-mate/cars/c7/soc"));
    }

    #[test]
    fn flight_confirms_in_order() {
        let ev = |soc| Item::State(1, Pending::Now(Box::new(item(soc))));
        let mut f = Flight::default();
        f.push(3, ev(1.0));
        f.push(5, ev(2.0));
        f.push(9, ev(3.0));
        f.confirm(4);
        assert_eq!(f.items.len(), 2);
        let left = f.unconfirmed(5);
        assert_eq!(left, vec![ev(3.0)]);
        assert!(f.items.is_empty());
    }
}
