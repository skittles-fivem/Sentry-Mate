//! Topic names. Everything a car publishes is under its own `<prefix>/cars/<car id>/`, so a subscriber can follow
//! one car (`sentry-mate/cars/c7/#`) or all of them (`sentry-mate/cars/+/soc`) without the server knowing who listens.

use crate::error::Error;
use model::CarId;

/// The topic names for one setup (prefix and Home Assistant's discovery prefix).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Topics {
    prefix: String,
    discovery_prefix: String,
    /// The prefix made safe for Home Assistant ids (`[A-Za-z0-9_-]`), so two Sentry Mates with different prefixes on
    /// one broker don't share entities.
    node_base: String,
}

impl Topics {
    /// Takes prefixes already checked with [`check_prefix`].
    pub fn new(prefix: &str, discovery_prefix: &str) -> Topics {
        let node_base =
            prefix.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
        Topics { prefix: prefix.to_string(), discovery_prefix: discovery_prefix.to_string(), node_base }
    }

    /// The connection's own topic: `online` while connected, `offline` (retained, and the last will) otherwise.
    /// Every Home Assistant entity uses it for availability.
    pub fn status(&self) -> String {
        format!("{}/status", self.prefix)
    }

    /// A value of one car, e.g. `sentry-mate/cars/c7/soc`.
    pub fn field(&self, car: CarId, key: &str) -> String {
        format!("{}/cars/c{car}/{key}", self.prefix)
    }

    /// A car's drive and charge events (not retained: they are moments).
    pub fn event(&self, car: CarId) -> String {
        self.field(car, "event")
    }

    /// Home Assistant's node id for a car: also its device identifier.
    pub fn node_id(&self, car: CarId) -> String {
        format!("{}_c{car}", self.node_base)
    }

    /// `homeassistant/<component>/<node id>/<object>/config`.
    pub fn discovery(&self, component: &str, car: CarId, object: &str) -> String {
        format!("{}/{component}/{}/{object}/config", self.discovery_prefix, self.node_id(car))
    }
}

/// A prefix a broker and Home Assistant both take: levels joined by `/`, none empty, no wildcards (`+`, `#`), not a
/// broker topic (`$…`), no control characters.
pub fn check_prefix(p: &str) -> Result<(), Error> {
    let bad = |why: &str| Err(Error::BadPrefix(format!("{p:?}: {why}")));
    if p.is_empty() {
        return bad("empty");
    }
    if p.len() > 200 {
        return bad("longer than 200 bytes");
    }
    if p.contains(['+', '#']) {
        return bad("no + or # (they are wildcards)");
    }
    if p.starts_with('$') {
        return bad("topics starting with $ belong to the broker");
    }
    if p.chars().any(char::is_control) {
        return bad("control characters");
    }
    if p.split('/').any(str::is_empty) {
        return bad("no empty level (a / at either end, or //)");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        let t = Topics::new("sentry-mate", "homeassistant");
        assert_eq!(t.status(), "sentry-mate/status");
        assert_eq!(t.field(7, "soc"), "sentry-mate/cars/c7/soc");
        assert_eq!(t.event(7), "sentry-mate/cars/c7/event");
        assert_eq!(t.node_id(7), "sentry-mate_c7");
        assert_eq!(t.discovery("sensor", 7, "soc"), "homeassistant/sensor/sentry-mate_c7/soc/config");
        let custom = Topics::new("home/garage.cars", "ha");
        assert_eq!(custom.field(4_000_000_000, "range"), "home/garage.cars/cars/c4000000000/range");
        assert_eq!(custom.node_id(1), "home_garage_cars_c1");
        assert_eq!(
            custom.discovery("binary_sensor", 1, "driving"),
            "ha/binary_sensor/home_garage_cars_c1/driving/config"
        );
    }

    #[test]
    fn prefixes() {
        for ok in ["sentry-mate", "a/b/c", "Home Cars", "car_logger-2"] {
            assert!(check_prefix(ok).is_ok(), "{ok}");
        }
        for bad in ["", "/a", "a/", "a//b", "a/+", "#", "$SYS", "a\nb", &"x".repeat(201)] {
            assert!(check_prefix(bad).is_err(), "{bad:?}");
        }
    }
}
