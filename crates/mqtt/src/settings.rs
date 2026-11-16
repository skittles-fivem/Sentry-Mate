//! What the owner sets in Settings → MQTT, and the checks that refuse an unsafe or broken setup before anything
//! connects. The rules are SMO's (`MqttSecurity.kt`): the link is encrypted unless the owner explicitly allows
//! plain MQTT, a self-signed broker is trusted only by its pinned SHA-256, and here also: a password never goes over
//! an unencrypted link (Sentry Six Manager's "passwords only over HTTPS").

use crate::error::Error;
use crate::security;
use crate::topics;
use model::CarId;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fmt;

/// MQTT publishing, off unless the owner turns it on. Saved with the program's settings, so every field has a
/// default and unknown fields from a newer version are ignored.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Nothing is published (and nothing connects) while this is off.
    pub enabled: bool,
    /// `mqtts://host[:port]` (8883 by default; `ssl://` is taken as the same). `mqtt://host[:port]` (1883) only with
    /// [`Settings::allow_unencrypted`]. No path, no user or password in it.
    pub broker: String,
    /// Lets `mqtt://` through: for a broker on the same machine or a trusted LAN only. Passwords are still refused.
    pub allow_unencrypted: bool,
    /// The broker certificate's SHA-256 (any of `AB:CD:…`, `abcd…`, or openssl's `sha256 Fingerprint=…` line): a
    /// self-signed broker is trusted by this alone. Empty: the certificate must chain to a public CA (webpki roots)
    /// and match the host name.
    pub cert_sha256: Option<String>,
    /// TLS 1.2 as well as 1.3, for old brokers (rustls offers only forward-secret AEAD suites either way).
    pub allow_tls12: bool,
    pub username: Option<String>,
    /// Sent only over TLS. Never written to the log or shown by `Debug`.
    pub password: Option<String>,
    /// The MQTT client id; one connection publishes for every car.
    pub client_id: String,
    /// Topics are `<prefix>/cars/<car id>/<field>`; the connection's own `<prefix>/status` is `online`, or `offline`
    /// (the last will) when the link drops.
    pub prefix: String,
    /// Latitude and longitude are published only when this is on (a separate opt-in from MQTT itself); while it is
    /// off, any position published earlier is cleared from the broker.
    pub publish_location: bool,
    /// Only these cars are published; `None`: every car.
    pub cars: Option<BTreeSet<CarId>>,
    /// Home Assistant MQTT discovery: one device per car, its sensors made automatically.
    pub discovery: bool,
    /// Where Home Assistant listens for discovery (its default is `homeassistant`).
    pub discovery_prefix: String,
    pub units: Units,
    pub limits: Limits,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            enabled: false,
            broker: String::new(),
            allow_unencrypted: false,
            cert_sha256: None,
            allow_tls12: false,
            username: None,
            password: None,
            client_id: "sentry-mate".into(),
            prefix: "sentry-mate".into(),
            publish_location: false,
            cars: None,
            discovery: false,
            discovery_prefix: "homeassistant".into(),
            units: Units::default(),
            limits: Limits::default(),
        }
    }
}

impl fmt::Debug for Settings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Settings")
            .field("enabled", &self.enabled)
            .field("broker", &self.broker)
            .field("allow_unencrypted", &self.allow_unencrypted)
            .field("cert_sha256", &self.cert_sha256)
            .field("allow_tls12", &self.allow_tls12)
            .field("username", &self.username)
            .field("password", &self.password.as_ref().map(|_| "(set)"))
            .field("client_id", &self.client_id)
            .field("prefix", &self.prefix)
            .field("publish_location", &self.publish_location)
            .field("cars", &self.cars.as_ref().map(|c| c.len()))
            .field("discovery", &self.discovery)
            .field("discovery_prefix", &self.discovery_prefix)
            .field("units", &self.units)
            .field("limits", &self.limits)
            .finish()
    }
}

/// How values are shown: the model's units (km, °C, bar) or the owner's choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Units {
    pub distance: DistanceUnit,
    pub temperature: TemperatureUnit,
    pub pressure: PressureUnit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum DistanceUnit {
    #[default]
    Km,
    /// Miles: distances, speeds (mph), efficiency (Wh/mi) and elevation (ft).
    Mi,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum TemperatureUnit {
    #[default]
    C,
    F,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum PressureUnit {
    #[default]
    Bar,
    Psi,
}

const KM_PER_MI: f64 = 1.609_344;
const M_PER_FT: f64 = 0.3048;
const PSI_PER_BAR: f64 = 14.503_773_773_022;

impl Units {
    pub fn distance(&self, km: f64) -> f64 {
        match self.distance {
            DistanceUnit::Km => km,
            DistanceUnit::Mi => km / KM_PER_MI,
        }
    }

    pub fn distance_unit(&self) -> &'static str {
        match self.distance {
            DistanceUnit::Km => "km",
            DistanceUnit::Mi => "mi",
        }
    }

    pub fn speed(&self, kmh: f64) -> f64 {
        self.distance(kmh)
    }

    pub fn speed_unit(&self) -> &'static str {
        match self.distance {
            DistanceUnit::Km => "km/h",
            DistanceUnit::Mi => "mph",
        }
    }

    /// Wh per km becomes Wh per mile (more Wh: a mile is longer).
    pub fn efficiency(&self, wh_km: f64) -> f64 {
        match self.distance {
            DistanceUnit::Km => wh_km,
            DistanceUnit::Mi => wh_km * KM_PER_MI,
        }
    }

    pub fn efficiency_unit(&self) -> &'static str {
        match self.distance {
            DistanceUnit::Km => "Wh/km",
            DistanceUnit::Mi => "Wh/mi",
        }
    }

    /// Climbs and elevation follow the distance unit: metres with km, feet with miles.
    pub fn elevation(&self, m: f64) -> f64 {
        match self.distance {
            DistanceUnit::Km => m,
            DistanceUnit::Mi => m / M_PER_FT,
        }
    }

    pub fn elevation_unit(&self) -> &'static str {
        match self.distance {
            DistanceUnit::Km => "m",
            DistanceUnit::Mi => "ft",
        }
    }

    pub fn temperature(&self, c: f64) -> f64 {
        match self.temperature {
            TemperatureUnit::C => c,
            TemperatureUnit::F => c * 9.0 / 5.0 + 32.0,
        }
    }

    pub fn temperature_unit(&self) -> &'static str {
        match self.temperature {
            TemperatureUnit::C => "°C",
            TemperatureUnit::F => "°F",
        }
    }

    pub fn pressure(&self, bar: f64) -> f64 {
        match self.pressure {
            PressureUnit::Bar => bar,
            PressureUnit::Psi => bar * PSI_PER_BAR,
        }
    }

    pub fn pressure_unit(&self) -> &'static str {
        match self.pressure {
            PressureUnit::Bar => "bar",
            PressureUnit::Psi => "psi",
        }
    }

    /// Decimals worth showing for a pressure: 2.85 bar, 41.3 psi.
    pub(crate) fn pressure_decimals(&self) -> i32 {
        match self.pressure {
            PressureUnit::Bar => 2,
            PressureUnit::Psi => 1,
        }
    }
}

/// Bounds that keep memory flat however many cars there are. The defaults suit thousands of cars on a small server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Limits {
    /// Events waiting while the broker is away; past this the oldest are dropped (and counted).
    pub max_events: usize,
    /// Cars with a state waiting (one each, the newest); past this the car waiting longest loses its state (counted;
    /// its next update brings it back). Cars waiting to be forgotten don't count and are never dropped: no later
    /// update would bring them back.
    pub max_pending_cars: usize,
    /// Cars whose last published values are remembered, so only changed values are sent again. A car that fell out
    /// just has all its values (and its Home Assistant configs) sent once more. Cars update in turn, so the number
    /// remembered through a whole round is half of this (16,384 by default); each takes about 220 bytes, and only
    /// cars that publish take any.
    pub cache_cars: usize,
    pub keep_alive_s: u16,
    pub connect_timeout_s: u16,
    /// Waits between reconnects, doubling from the first to the second; back to the first once a connection has
    /// lasted [`STABLE_AFTER`] (one that drops sooner, e.g. the broker accepts and then closes it, keeps doubling).
    pub reconnect_min_ms: u64,
    pub reconnect_max_ms: u64,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits {
            max_events: 10_000,
            max_pending_cars: 100_000,
            cache_cars: 32_768,
            keep_alive_s: 30,
            connect_timeout_s: 10,
            reconnect_min_ms: 1_000,
            reconnect_max_ms: 60_000,
        }
    }
}

/// How long a connection must last for the wait between reconnects to start again from `reconnect_min_ms`.
pub const STABLE_AFTER: std::time::Duration = std::time::Duration::from_secs(30);

/// The broker's address, from [`Settings::broker`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Broker {
    /// A DNS name or an IP address (an IPv6 one without its brackets).
    pub host: String,
    pub port: u16,
    pub tls: bool,
}

impl fmt::Display for Broker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let scheme = if self.tls { "mqtts" } else { "mqtt" };
        if self.host.contains(':') {
            write!(f, "{scheme}://[{}]:{}", self.host, self.port)
        } else {
            write!(f, "{scheme}://{}:{}", self.host, self.port)
        }
    }
}

/// Reads a broker address and applies the encryption rule: `mqtts://` (or `ssl://`) always, `mqtt://` (or `tcp://`)
/// only when allowed, nothing else.
pub fn parse_broker(url: &str, allow_unencrypted: bool) -> Result<Broker, Error> {
    let url = url.trim();
    let Some((scheme, rest)) = url.split_once("://") else {
        return Err(Error::NoScheme);
    };
    let (tls, default_port) = match scheme.to_ascii_lowercase().as_str() {
        "mqtts" | "ssl" => (true, 8883),
        "mqtt" | "tcp" if allow_unencrypted => (false, 1883),
        "mqtt" | "tcp" => return Err(Error::Unencrypted(scheme.to_ascii_lowercase())),
        other => return Err(Error::UnsupportedScheme(other.to_string())),
    };
    let rest = rest.strip_suffix('/').unwrap_or(rest);
    if rest.contains('@') {
        return Err(Error::CredentialsInUrl);
    }
    if rest.contains(['/', '?', '#']) {
        return Err(Error::BadAddress("no path or query after the host and port".into()));
    }
    let (host, port) = if let Some(inner) = rest.strip_prefix('[') {
        let (h, after) =
            inner.split_once(']').ok_or_else(|| Error::BadAddress("unclosed [ in an IPv6 address".into()))?;
        if h.parse::<std::net::Ipv6Addr>().is_err() {
            return Err(Error::BadAddress(format!("{h} is not an IPv6 address")));
        }
        match after {
            "" => (h, None),
            p => (h, Some(p.strip_prefix(':').ok_or_else(|| Error::BadAddress("expected :port after ]".into()))?)),
        }
    } else {
        match rest.rsplit_once(':') {
            Some((h, _)) if h.contains(':') => {
                return Err(Error::BadAddress("put an IPv6 address in brackets: mqtts://[::1]:8883".into()));
            }
            Some((h, p)) => (h, Some(p)),
            None => (rest, None),
        }
    };
    let port = match port {
        None => default_port,
        Some(p) => match p.parse::<u16>() {
            Ok(n) if n > 0 => n,
            _ => return Err(Error::BadAddress(format!("{p} is not a port"))),
        },
    };
    if host.is_empty() {
        return Err(Error::BadAddress("no host".into()));
    }
    // the name TLS will check (and the one rumqttc hands to rustls), refused here rather than on every reconnect
    if rustls::pki_types::ServerName::try_from(host).is_err() {
        return Err(Error::BadAddress(format!("{host} is not a host name or IP address")));
    }
    Ok(Broker { host: host.to_string(), port, tls })
}

/// Settings that passed every check, ready to connect with.
#[derive(Debug, Clone)]
pub(crate) struct Checked {
    pub broker: Broker,
    pub pin: Option<[u8; 32]>,
}

impl Settings {
    /// Every rule, in one place, so Settings → MQTT can show why a setup is refused before saving it.
    pub fn check(&self) -> Result<(), Error> {
        self.checked().map(|_| ())
    }

    pub(crate) fn checked(&self) -> Result<Checked, Error> {
        let broker = parse_broker(&self.broker, self.allow_unencrypted)?;
        let pin = match self.cert_sha256.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            None => None,
            Some(_) if !broker.tls => return Err(Error::PinWithoutTls),
            Some(p) => Some(security::normalize_fingerprint(p).ok_or(Error::BadPin)?),
        };
        let user = self.username.as_deref().filter(|u| !u.is_empty());
        let password = self.password.as_deref().filter(|p| !p.is_empty());
        if password.is_some() && user.is_none() {
            return Err(Error::PasswordWithoutUser);
        }
        if password.is_some() && !broker.tls {
            return Err(Error::PasswordUnencrypted);
        }
        if user.is_some_and(|u| u.len() > 1024 || u.chars().any(char::is_control)) {
            return Err(Error::BadUsername);
        }
        // an MQTT string's length is 16 bits: rumqttc would write a longer one's length cut short, a broken CONNECT
        if password.is_some_and(|p| p.len() > usize::from(u16::MAX)) {
            return Err(Error::BadPassword);
        }
        let id = &self.client_id;
        if id.is_empty() || id.len() > 128 || id.chars().any(|c| c.is_control() || c.is_whitespace()) {
            return Err(Error::BadClientId);
        }
        topics::check_prefix(&self.prefix)?;
        // checked even with discovery off: forgetting a car also removes its Home Assistant device
        topics::check_prefix(&self.discovery_prefix)?;
        let l = &self.limits;
        let bad = |what: &'static str| Err(Error::BadLimit(what));
        if l.max_events == 0 {
            return bad("max_events");
        }
        if l.max_pending_cars == 0 {
            return bad("max_pending_cars");
        }
        if l.cache_cars < 2 {
            return bad("cache_cars");
        }
        if l.keep_alive_s < 5 {
            return bad("keep_alive_s");
        }
        if l.connect_timeout_s == 0 {
            return bad("connect_timeout_s");
        }
        if l.reconnect_min_ms == 0 || l.reconnect_max_ms < l.reconnect_min_ms {
            return bad("reconnect_min_ms / reconnect_max_ms");
        }
        Ok(Checked { broker, pin })
    }

    /// Whether this car is published.
    pub fn includes(&self, car: CarId) -> bool {
        self.cars.as_ref().is_none_or(|set| set.contains(&car))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn on(broker: &str) -> Settings {
        Settings { enabled: true, broker: broker.into(), ..Settings::default() }
    }

    #[test]
    fn broker_addresses() {
        let b = parse_broker("mqtts://broker.example", false).unwrap();
        assert_eq!(b, Broker { host: "broker.example".into(), port: 8883, tls: true });
        assert_eq!(parse_broker(" ssl://10.0.0.2:18883/ ", false).unwrap().port, 18883);
        let v6 = parse_broker("MQTTS://[::1]:8884", false).unwrap();
        assert_eq!((v6.host.as_str(), v6.port), ("::1", 8884));
        assert_eq!(v6.to_string(), "mqtts://[::1]:8884");
        assert_eq!(parse_broker("mqtts://[fd00::5]", false).unwrap().port, 8883);
        assert_eq!(
            parse_broker("mqtt://localhost", true).unwrap(),
            Broker { host: "localhost".into(), port: 1883, tls: false }
        );
        assert_eq!(parse_broker("tcp://127.0.0.1:1884", true).unwrap().port, 1884);
    }

    #[test]
    fn plain_mqtt_refused_unless_allowed() {
        assert_eq!(parse_broker("mqtt://broker.lan", false), Err(Error::Unencrypted("mqtt".into())));
        assert_eq!(parse_broker("tcp://broker.lan:1883", false), Err(Error::Unencrypted("tcp".into())));
        assert!(Error::Unencrypted("mqtt".into()).to_string().contains("Allow unencrypted MQTT"));
        assert!(parse_broker("mqtt://broker.lan", true).is_ok());
        // plain is never assumed: no scheme, or one we don't speak, is refused
        assert_eq!(parse_broker("broker.lan:8883", true), Err(Error::NoScheme));
        assert_eq!(parse_broker("ws://broker.lan", true), Err(Error::UnsupportedScheme("ws".into())));
        assert_eq!(parse_broker("wss://broker.lan", true), Err(Error::UnsupportedScheme("wss".into())));
        assert_eq!(parse_broker("https://broker.lan", true), Err(Error::UnsupportedScheme("https".into())));
    }

    #[test]
    fn bad_addresses() {
        assert_eq!(parse_broker("mqtts://user:pw@broker.lan", false), Err(Error::CredentialsInUrl));
        for bad in [
            "mqtts://",
            "mqtts://:8883",
            "mqtts://broker.lan:0",
            "mqtts://broker.lan:99999",
            "mqtts://broker.lan:x",
            "mqtts://broker.lan/topic",
            "mqtts://broker.lan?x=1",
            "mqtts://::1:8883",
            "mqtts://[::1",
            "mqtts://[nope]:1",
            "mqtts://[::1]8883",
            "mqtts://bad host",
        ] {
            assert!(matches!(parse_broker(bad, true), Err(Error::BadAddress(_))), "{bad}");
        }
    }

    #[test]
    fn passwords_only_over_tls() {
        let mut s = on("mqtt://127.0.0.1");
        s.allow_unencrypted = true;
        s.username = Some("ha".into());
        assert!(s.check().is_ok(), "a user name alone is fine on a trusted LAN");
        s.password = Some("secret".into());
        assert_eq!(s.check(), Err(Error::PasswordUnencrypted));
        s.broker = "mqtts://127.0.0.1".into();
        assert!(s.check().is_ok());
        s.username = None;
        assert_eq!(s.check(), Err(Error::PasswordWithoutUser));
        s.username = Some(String::new());
        assert_eq!(s.check(), Err(Error::PasswordWithoutUser));
        assert!(!format!("{s:?}").contains("secret"), "Debug never shows the password");
        s.username = Some("ha".into());
        s.password = Some("p".repeat(65_535));
        assert!(s.check().is_ok(), "as long as MQTT allows");
        s.password = Some("p".repeat(65_536));
        assert_eq!(s.check(), Err(Error::BadPassword));
    }

    #[test]
    fn pins() {
        let hex = "ab".repeat(32);
        let mut s = on("mqtts://broker.lan");
        for form in [hex.clone(), hex.to_uppercase(), format!("sha256 Fingerprint={}", vec!["AB"; 32].join(":"))] {
            s.cert_sha256 = Some(form);
            assert_eq!(s.checked().unwrap().pin, Some([0xab; 32]));
        }
        s.cert_sha256 = Some("  ".into());
        assert_eq!(s.checked().unwrap().pin, None, "an empty pin means public CAs");
        s.cert_sha256 = Some("abcd".into());
        assert_eq!(s.check(), Err(Error::BadPin));
        s.cert_sha256 = Some("zz".repeat(32));
        assert_eq!(s.check(), Err(Error::BadPin));
        s.cert_sha256 = Some(hex);
        s.broker = "mqtt://broker.lan".into();
        s.allow_unencrypted = true;
        assert_eq!(s.check(), Err(Error::PinWithoutTls), "a pin on a plain link would only look safe");
    }

    #[test]
    fn names_and_limits() {
        let mut s = on("mqtts://broker.lan");
        assert!(s.check().is_ok());
        s.client_id = "has space".into();
        assert_eq!(s.check(), Err(Error::BadClientId));
        s.client_id = String::new();
        assert_eq!(s.check(), Err(Error::BadClientId));
        s.client_id = "sm-1".into();
        s.prefix = "cars/#".into();
        assert!(matches!(s.check(), Err(Error::BadPrefix(_))));
        s.prefix = "home/sentry".into();
        s.discovery_prefix = "+".into();
        assert!(matches!(s.check(), Err(Error::BadPrefix(_))));
        s.discovery_prefix = "homeassistant".into();
        s.limits.keep_alive_s = 1;
        assert_eq!(s.check(), Err(Error::BadLimit("keep_alive_s")));
        s.limits = Limits { reconnect_max_ms: 10, reconnect_min_ms: 20, ..Limits::default() };
        assert!(matches!(s.check(), Err(Error::BadLimit(_))));
    }

    #[test]
    fn car_filter() {
        let mut s = Settings::default();
        assert!(s.includes(1) && s.includes(u32::MAX), "no list: every car");
        s.cars = Some([2, 5].into_iter().collect());
        assert!(s.includes(2) && s.includes(5) && !s.includes(3));
        s.cars = Some(BTreeSet::new());
        assert!(!s.includes(2), "an empty list publishes no car");
    }

    #[test]
    fn units() {
        let mi = Units { distance: DistanceUnit::Mi, temperature: TemperatureUnit::F, pressure: PressureUnit::Psi };
        assert!((mi.distance(160.9344) - 100.0).abs() < 1e-9);
        assert!((mi.speed(100.0) - 62.137_119).abs() < 1e-6);
        assert!((mi.temperature(-40.0) + 40.0).abs() < 1e-9);
        assert!((mi.temperature(20.0) - 68.0).abs() < 1e-9);
        assert!((mi.pressure(2.9) - 42.06).abs() < 0.01);
        assert!((mi.efficiency(150.0) - 241.4).abs() < 0.01);
        assert!((mi.elevation(100.0) - 328.08).abs() < 0.01);
        assert_eq!(
            (mi.distance_unit(), mi.speed_unit(), mi.temperature_unit(), mi.pressure_unit()),
            ("mi", "mph", "°F", "psi")
        );
        let km = Units::default();
        assert_eq!((km.distance(12.5), km.temperature(21.5), km.pressure(2.9)), (12.5, 21.5, 2.9));
        assert_eq!(
            (km.distance_unit(), km.speed_unit(), km.temperature_unit(), km.pressure_unit()),
            ("km", "km/h", "°C", "bar")
        );
    }

    #[test]
    fn settings_round_trip_and_defaults() {
        let s: Settings =
            serde_json::from_str(r#"{"enabled":true,"broker":"mqtts://b","units":{"distance":"mi"},"future":1}"#)
                .unwrap();
        assert!(s.enabled && !s.publish_location && !s.allow_unencrypted && !s.discovery);
        assert_eq!((s.units.distance, s.units.temperature), (DistanceUnit::Mi, TemperatureUnit::C));
        assert_eq!(s.prefix, "sentry-mate");
        let back: Settings = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back, s);
        assert!(!Settings::default().enabled, "off unless the owner turns it on");
    }
}
