//! Optional MQTT publishing, for Home Assistant and anything else that speaks MQTT. Off unless the owner turns it on;
//! nothing leaves the server before that.
//!
//! One connection publishes for every car, however many there are. Callers hand over a car's newest values
//! ([`model::Now`]) and its drive and charge events; they never wait on the network. Behind them a bounded queue keeps
//! one state per car (the newest) while the broker is away or busy, and the events, up to a bound. Per car, only the
//! values that changed since they were last sent on this connection go out again (a bounded cache of recent cars
//! remembers what was sent; a car that fell out of it is just sent in full).
//!
//! On the wire: `<prefix>/cars/c<n>/<field>` (the car as the API names it), retained, QoS 1; events on
//! `<prefix>/cars/c<n>/event`; `<prefix>/status` is `online`, with `offline` as the last will. Home Assistant discovery
//! makes one device per car ([`cleanup`] clears what an earlier layout left).
//!
//! Security, as SMO's `MqttSecurity.kt`: TLS (`mqtts://`) with rustls, the broker trusted by webpki roots and its
//! host name or by a pinned SHA-256 (self-signed brokers); plain `mqtt://` refused unless the owner allows it, and
//! even then never with a password. Positions are published only when the owner also turns that on.
//!
//! ```no_run
//! # async fn demo(now: model::Now) -> Result<(), mqtt::Error> {
//! let settings = mqtt::Settings { enabled: true, broker: "mqtts://broker.lan".into(), ..Default::default() };
//! if let Some(mqtt) = mqtt::Mqtt::start(&settings)? {
//!     mqtt.publish_now(&mqtt::CarInfo { name: "Model Y".into(), model: None }, &now, Some("Home"));
//!     mqtt.stop().await;
//! }
//! # Ok(()) }
//! ```

mod cache;
pub mod cleanup;
pub mod discovery;
mod error;
pub mod probe;
mod queue;
pub mod render;
pub mod security;
pub mod settings;
pub mod topics;
mod worker;

pub use error::Error;
pub use render::{CarInfo, Event, Field, Layout, Message};
pub use settings::{DistanceUnit, Limits, PressureUnit, Settings, TemperatureUnit, Units};

use model::{CarId, Now};
use queue::{NowItem, Pending, Queue};
use rumqttc::{LastWill, MqttOptions, QoS, TlsConfiguration, Transport};
use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use worker::Shared;

/// Largest message either way (discovery configs and events are a few hundred bytes).
const MAX_PACKET: usize = 256 * 1024;

/// How long [`Mqtt::stop`] waits to say `offline` before closing anyway.
const STOP_WAIT: Duration = Duration::from_secs(5);

/// A running publisher. Dropping it closes the connection at once (the broker then sends the last will).
pub struct Mqtt {
    shared: Arc<Shared>,
    cars: Option<BTreeSet<CarId>>,
    stop: watch::Sender<bool>,
    task: Option<JoinHandle<()>>,
    describe: String,
}

/// How publishing goes, for Settings → MQTT.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Stats {
    /// The broker and how it is trusted, e.g. `mqtts://broker.lan:8883 (TLS, pinned)`.
    pub broker: String,
    pub connected: bool,
    /// Connections made since start (more than one: the link dropped and came back).
    pub connects: u64,
    /// Messages the broker acknowledged.
    pub delivered: u64,
    pub states_waiting: usize,
    pub events_waiting: usize,
    /// States lost because more cars waited than `limits.max_pending_cars` (each car's next update brings it back).
    pub states_dropped: u64,
    /// Events lost because more waited than `limits.max_events`.
    pub events_dropped: u64,
    /// Why the last connection failed or dropped, until one works, in words.
    pub last_error: Option<String>,
    /// The same as a code: `connection_refused`, `auth_failed`, `tls_failed`, `pin_mismatch` or `timeout` (the codes
    /// of [`probe::Probe::error`]).
    pub last_error_code: Option<&'static str>,
}

impl Mqtt {
    /// Checks the settings and starts publishing in the background (on the current Tokio runtime). `Ok(None)` while
    /// MQTT is off; an error says what to change. Stop the previous publisher before starting one with new settings:
    /// two with the same client id would take the connection from each other.
    pub fn start(settings: &Settings) -> Result<Option<Mqtt>, Error> {
        if !settings.enabled {
            return Ok(None);
        }
        let checked = settings.checked()?;
        let runtime = tokio::runtime::Handle::try_current().map_err(|_| Error::NoRuntime)?;
        let layout = Layout::new(settings)?;
        let broker = &checked.broker;
        let mut options = MqttOptions::new(settings.client_id.clone(), broker.host.clone(), broker.port);
        let l = &settings.limits;
        options
            .set_keep_alive(Duration::from_secs(u64::from(l.keep_alive_s)))
            .set_clean_session(true)
            .set_inflight(worker::INFLIGHT)
            .set_max_packet_size(MAX_PACKET, MAX_PACKET)
            .set_last_will(LastWill::new(layout.topics.status(), "offline", QoS::AtLeastOnce, true));
        if let Some(user) = settings.username.as_deref().filter(|u| !u.is_empty()) {
            options.set_credentials(user, settings.password.clone().unwrap_or_default());
        }
        if broker.tls {
            let tls = security::client_config(checked.pin, settings.allow_tls12)?;
            options.set_transport(Transport::tls_with_config(TlsConfiguration::Rustls(tls)));
        }
        let describe = format!(
            "{broker} ({})",
            match (broker.tls, checked.pin.is_some()) {
                (false, _) => "UNENCRYPTED",
                (true, true) => "TLS, pinned",
                (true, false) => "TLS",
            }
        );
        let shared = Arc::new(Shared::new(Queue::new(l.max_pending_cars, l.max_events)));
        let cfg = worker::Config {
            layout,
            options,
            connect_timeout_s: u64::from(l.connect_timeout_s),
            cache_cars: l.cache_cars,
            reconnect_min: Duration::from_millis(l.reconnect_min_ms),
            reconnect_max: Duration::from_millis(l.reconnect_max_ms),
            describe: describe.clone(),
        };
        let (stop, stop_rx) = watch::channel(false);
        let task = runtime.spawn(worker::run(shared.clone(), cfg, stop_rx));
        Ok(Some(Mqtt { shared, cars: settings.cars.clone(), stop, task: Some(task), describe }))
    }

    fn includes(&self, car: CarId) -> bool {
        self.cars.as_ref().is_none_or(|set| set.contains(&car))
    }

    /// Queues a car's newest values (`place`: the saved place it is at, by name). Returns at once; if the car's
    /// previous values haven't gone out yet, these replace them. Cars outside the chosen set are ignored. The values
    /// replace what the broker holds even if they were recorded earlier than ones sent before, so pass the car's
    /// newest (not those of a late log being worked through).
    pub fn publish_now(&self, car: &CarInfo, now: &Now, place: Option<&str>) {
        if !self.includes(now.car) {
            return;
        }
        let item = Box::new(NowItem { info: car.clone(), now: now.clone(), place: place.map(str::to_string) });
        self.shared.queue().put_state(now.car, Pending::Now(item));
        self.shared.wake.notify_one();
    }

    /// Queues a drive or charge event. Returns at once.
    pub fn publish_event(&self, event: Event) {
        if !self.includes(event.car()) {
            return;
        }
        self.shared.queue().put_event(event);
        self.shared.wake.notify_one();
    }

    /// Clears a car from the broker (its retained values and its Home Assistant device), e.g. when it is deleted.
    /// Works for a car outside the chosen set too, to clean up after the set changed.
    pub fn forget_car(&self, car: CarId) {
        self.shared.queue().put_state(car, Pending::Forget);
        self.shared.wake.notify_one();
    }

    pub fn stats(&self) -> Stats {
        let (states_waiting, events_waiting, states_dropped, events_dropped) = {
            let q = self.shared.queue();
            (q.states_len(), q.events_len(), q.states_dropped, q.events_dropped)
        };
        let last = self.shared.last_error().clone();
        Stats {
            broker: self.describe.clone(),
            connected: self.shared.connected.load(Ordering::SeqCst),
            connects: self.shared.connects.load(Ordering::SeqCst),
            delivered: self.shared.delivered.load(Ordering::SeqCst),
            states_waiting,
            events_waiting,
            states_dropped,
            events_dropped,
            last_error: last.as_ref().map(|(_, text)| text.clone()),
            last_error_code: last.map(|(code, _)| code),
        }
    }

    /// Says `offline` and disconnects cleanly (waiting at most a few seconds), then stops.
    pub async fn stop(mut self) {
        let _ = self.stop.send(true);
        if let Some(task) = self.task.take() {
            let abort = task.abort_handle();
            if tokio::time::timeout(STOP_WAIT, task).await.is_err() {
                abort.abort();
            }
        }
    }
}

impl Drop for Mqtt {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shareable_between_car_tasks() {
        fn send_sync<T: Send + Sync>() {}
        send_sync::<Mqtt>();
    }

    #[test]
    fn needs_a_runtime_and_stays_off_by_default() {
        assert!(Mqtt::start(&Settings::default()).unwrap().is_none());
        let s = Settings { enabled: true, broker: "mqtts://broker.lan".into(), ..Settings::default() };
        assert_eq!(Mqtt::start(&s).err(), Some(Error::NoRuntime));
        let bad = Settings { broker: "mqtt://broker.lan".into(), ..s };
        assert_eq!(Mqtt::start(&bad).err(), Some(Error::Unencrypted("mqtt".into())), "settings are checked first");
    }

    #[test]
    fn preview_without_a_broker() {
        let layout = Layout::new(&Settings::default()).unwrap();
        let msgs = render::now_messages(&layout, &Now { car: 3, ..Now::default() }, None);
        assert_eq!(msgs[0].topic, "sentry-mate/cars/c3/state");
    }

    #[test]
    fn a_waiting_state_is_small() {
        // what each waiting car costs while the broker is away (`limits.max_pending_cars` of them at most)
        let size = std::mem::size_of::<NowItem>();
        eprintln!("a waiting state: {size} bytes plus the car's name and place");
        assert!(size < 1024, "{size}");
    }
}
