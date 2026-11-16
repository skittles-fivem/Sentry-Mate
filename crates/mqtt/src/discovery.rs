//! Home Assistant MQTT discovery: one device per car, with its sensors, binary sensors, a device tracker (only with
//! location publishing on) and an event entity for drives and charges. Configs are retained, so Home Assistant finds
//! them again after a restart without the server listening for it; each car's are sent once per connection (again
//! only if its name changes or it was out of the recent-car cache).

use crate::render::{Event, Field, Layout, MAX_NAME, Message, STATES, clip};
use model::CarId;
use serde_json::{Map, Value, json};

/// The unit a sensor shows, in the owner's choice where there is one.
#[derive(Debug, Clone, Copy)]
enum Unit {
    None,
    Percent,
    Distance,
    Speed,
    Power,
    Energy,
    Temperature,
    Pressure,
    Volt,
}

struct Entity {
    field: Field,
    component: &'static str,
    name: &'static str,
    device_class: Option<&'static str>,
    state_class: Option<&'static str>,
    unit: Unit,
    icon: Option<&'static str>,
    /// The values an `enum` sensor can take.
    options: &'static [&'static str],
    diagnostic: bool,
}

const fn sensor(field: Field, name: &'static str, device_class: &'static str, unit: Unit) -> Entity {
    Entity {
        field,
        component: "sensor",
        name,
        device_class: Some(device_class),
        state_class: Some("measurement"),
        unit,
        icon: None,
        options: &[],
        diagnostic: false,
    }
}

const fn text(field: Field, name: &'static str, icon: &'static str, options: &'static [&'static str]) -> Entity {
    Entity {
        field,
        component: "sensor",
        name,
        device_class: if options.is_empty() { None } else { Some("enum") },
        state_class: None,
        unit: Unit::None,
        icon: Some(icon),
        options,
        diagnostic: false,
    }
}

const fn binary(field: Field, name: &'static str, device_class: &'static str) -> Entity {
    Entity {
        field,
        component: "binary_sensor",
        name,
        device_class: Some(device_class),
        state_class: None,
        unit: Unit::None,
        icon: None,
        options: &[],
        diagnostic: false,
    }
}

/// Every value topic but the position ones, which the device tracker covers.
const ENTITIES: &[Entity] = &[
    text(Field::State, "State", "mdi:car", &STATES),
    binary(Field::Driving, "Driving", "moving"),
    binary(Field::Charging, "Charging", "battery_charging"),
    text(Field::Place, "Place", "mdi:map-marker", &[]),
    sensor(Field::Soc, "Battery", "battery", Unit::Percent),
    sensor(Field::Range, "Range", "distance", Unit::Distance),
    sensor(Field::Speed, "Speed", "speed", Unit::Speed),
    sensor(Field::Power, "Power", "power", Unit::Power),
    sensor(Field::ChargePower, "Charge power", "power", Unit::Power),
    text(Field::ChargeKind, "Charger", "mdi:ev-plug-type2", &["ac", "dc"]),
    sensor(Field::EnergyRemaining, "Energy remaining", "energy_storage", Unit::Energy),
    sensor(Field::BatteryTemp, "Battery temperature", "temperature", Unit::Temperature),
    sensor(Field::OutsideTemp, "Outside temperature", "temperature", Unit::Temperature),
    sensor(Field::InsideTemp, "Inside temperature", "temperature", Unit::Temperature),
    sensor(Field::TireFrontLeft, "Tire pressure front left", "pressure", Unit::Pressure),
    sensor(Field::TireFrontRight, "Tire pressure front right", "pressure", Unit::Pressure),
    sensor(Field::TireRearLeft, "Tire pressure rear left", "pressure", Unit::Pressure),
    sensor(Field::TireRearRight, "Tire pressure rear right", "pressure", Unit::Pressure),
    Entity { state_class: Some("total_increasing"), ..sensor(Field::Odometer, "Odometer", "distance", Unit::Distance) },
    text(Field::Gear, "Gear", "mdi:car-shift-pattern", &["P", "R", "N", "D"]),
    Entity { diagnostic: true, ..sensor(Field::LvVoltage, "12 V battery", "voltage", Unit::Volt) },
    Entity {
        device_class: Some("timestamp"),
        state_class: None,
        diagnostic: true,
        ..sensor(Field::Updated, "Last data", "timestamp", Unit::None)
    },
];

fn unit_text(layout: &Layout, unit: Unit) -> Option<&'static str> {
    let u = &layout.units;
    match unit {
        Unit::None => None,
        Unit::Percent => Some("%"),
        Unit::Distance => Some(u.distance_unit()),
        Unit::Speed => Some(u.speed_unit()),
        Unit::Power => Some("kW"),
        Unit::Energy => Some("kWh"),
        Unit::Temperature => Some(u.temperature_unit()),
        Unit::Pressure => Some(u.pressure_unit()),
        Unit::Volt => Some("V"),
    }
}

/// What every entity of a car shares: its device, availability (the connection's status topic) and origin.
fn base(layout: &Layout, car: CarId, info: &crate::CarInfo, name: &str, object: &str) -> Map<String, Value> {
    let t = &layout.topics;
    let node = t.node_id(car);
    let mut device = Map::new();
    device.insert("identifiers".into(), json!([node]));
    let car_name =
        if info.name.trim().is_empty() { format!("Car {car}") } else { clip(&info.name, MAX_NAME).to_string() };
    device.insert("name".into(), car_name.into());
    device.insert("manufacturer".into(), "Tesla".into());
    if let Some(m) = &info.model {
        device.insert("model".into(), clip(m, MAX_NAME).into());
    }
    let mut o = Map::new();
    o.insert("name".into(), name.into());
    o.insert("unique_id".into(), format!("{node}_{object}").into());
    o.insert("availability_topic".into(), t.status().into());
    o.insert("payload_available".into(), "online".into());
    o.insert("payload_not_available".into(), "offline".into());
    o.insert("qos".into(), 1.into());
    o.insert("device".into(), Value::Object(device));
    o.insert("origin".into(), json!({"name": "Sentry Mate", "sw_version": env!("CARGO_PKG_VERSION")}));
    o
}

/// The config messages of a car's entities (retained). With location publishing off the device tracker's config is
/// sent empty, which removes a tracker made while it was on.
pub fn configs(layout: &Layout, car: CarId, info: &crate::CarInfo) -> Vec<Message> {
    let t = &layout.topics;
    let mut out = Vec::with_capacity(ENTITIES.len() + 2);
    for e in ENTITIES {
        let key = e.field.key();
        let mut o = base(layout, car, info, e.name, key);
        o.insert("state_topic".into(), t.field(car, key).into());
        if let Some(c) = e.device_class {
            o.insert("device_class".into(), c.into());
        }
        if let Some(c) = e.state_class {
            o.insert("state_class".into(), c.into());
        }
        if let Some(u) = unit_text(layout, e.unit) {
            o.insert("unit_of_measurement".into(), u.into());
        }
        if let Some(i) = e.icon {
            o.insert("icon".into(), i.into());
        }
        if !e.options.is_empty() {
            o.insert("options".into(), json!(e.options));
        }
        if e.component == "binary_sensor" {
            o.insert("payload_on".into(), "true".into());
            o.insert("payload_off".into(), "false".into());
        }
        if e.diagnostic {
            o.insert("entity_category".into(), "diagnostic".into());
        }
        out.push(Message {
            topic: t.discovery(e.component, car, key),
            payload: Value::Object(o).to_string(),
            retain: true,
        });
    }
    let tracker = t.discovery("device_tracker", car, Field::Location.key());
    let payload = if layout.location {
        let mut o = base(layout, car, info, "Location", Field::Location.key());
        o.insert("json_attributes_topic".into(), t.field(car, Field::Location.key()).into());
        o.insert("source_type".into(), "gps".into());
        Value::Object(o).to_string()
    } else {
        String::new()
    };
    out.push(Message { topic: tracker, payload, retain: true });
    let mut o = base(layout, car, info, "Events", "event");
    o.insert("state_topic".into(), t.event(car).into());
    o.insert("event_types".into(), json!(Event::TYPES));
    o.insert("icon".into(), "mdi:car-clock".into());
    out.push(Message {
        topic: t.discovery("event", car, "event"),
        payload: Value::Object(o).to_string(),
        retain: true,
    });
    out
}

/// Empty retained configs for every entity of a car: Home Assistant removes the device.
pub fn removals(layout: &Layout, car: CarId) -> Vec<Message> {
    let t = &layout.topics;
    ENTITIES
        .iter()
        .map(|e| t.discovery(e.component, car, e.field.key()))
        .chain([t.discovery("device_tracker", car, Field::Location.key()), t.discovery("event", car, "event")])
        .map(|topic| Message { topic, payload: String::new(), retain: true })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CarInfo;
    use crate::settings::{DistanceUnit, PressureUnit, TemperatureUnit, Units};
    use crate::topics::Topics;
    use std::collections::HashSet;

    fn layout(location: bool, units: Units) -> Layout {
        Layout { topics: Topics::new("sentry-mate", "homeassistant"), units, location, discovery: true }
    }

    fn info() -> CarInfo {
        CarInfo { name: "Model Y".into(), model: Some("Model Y 2022".into()) }
    }

    fn find<'a>(msgs: &'a [Message], topic: &str) -> &'a Message {
        msgs.iter().find(|m| m.topic == topic).unwrap_or_else(|| panic!("no {topic}"))
    }

    fn json_of(m: &Message) -> Value {
        serde_json::from_str(&m.payload).unwrap()
    }

    #[test]
    fn battery_sensor() {
        let msgs = configs(&layout(false, Units::default()), 7, &info());
        assert!(msgs.iter().all(|m| m.retain && m.topic.starts_with("homeassistant/")));
        let soc = json_of(find(&msgs, "homeassistant/sensor/sentry-mate_car7/soc/config"));
        assert_eq!(
            soc,
            json!({
                "name": "Battery",
                "unique_id": "sentry-mate_car7_soc",
                "state_topic": "sentry-mate/cars/7/soc",
                "availability_topic": "sentry-mate/status",
                "payload_available": "online",
                "payload_not_available": "offline",
                "qos": 1,
                "device_class": "battery",
                "state_class": "measurement",
                "unit_of_measurement": "%",
                "device": {
                    "identifiers": ["sentry-mate_car7"],
                    "name": "Model Y",
                    "manufacturer": "Tesla",
                    "model": "Model Y 2022"
                },
                "origin": {"name": "Sentry Mate", "sw_version": env!("CARGO_PKG_VERSION")}
            })
        );
    }

    #[test]
    fn kinds_of_entities() {
        let msgs = configs(&layout(false, Units::default()), 7, &info());
        let driving = json_of(find(&msgs, "homeassistant/binary_sensor/sentry-mate_car7/driving/config"));
        assert_eq!((driving["payload_on"].as_str(), driving["payload_off"].as_str()), (Some("true"), Some("false")));
        assert_eq!(driving["device_class"], "moving");
        let state = json_of(find(&msgs, "homeassistant/sensor/sentry-mate_car7/state/config"));
        assert_eq!(state["device_class"], "enum");
        assert_eq!(state["options"], json!(["driving", "charging", "online", "asleep", "unknown"]));
        let odo = json_of(find(&msgs, "homeassistant/sensor/sentry-mate_car7/odometer/config"));
        assert_eq!(
            (odo["state_class"].as_str(), odo["unit_of_measurement"].as_str()),
            (Some("total_increasing"), Some("km"))
        );
        let updated = json_of(find(&msgs, "homeassistant/sensor/sentry-mate_car7/updated/config"));
        assert_eq!(updated["device_class"], "timestamp");
        assert!(updated.get("state_class").is_none() && updated.get("unit_of_measurement").is_none());
        assert_eq!(updated["entity_category"], "diagnostic");
        let place = json_of(find(&msgs, "homeassistant/sensor/sentry-mate_car7/place/config"));
        assert!(place.get("device_class").is_none() && place.get("options").is_none());
        let event = json_of(find(&msgs, "homeassistant/event/sentry-mate_car7/event/config"));
        assert_eq!(event["state_topic"], "sentry-mate/cars/7/event");
        assert_eq!(event["event_types"], json!(["drive_started", "drive_ended", "charge_started", "charge_ended"]));
    }

    #[test]
    fn tracker_only_with_location() {
        let off = configs(&layout(false, Units::default()), 7, &info());
        let tracker = "homeassistant/device_tracker/sentry-mate_car7/location/config";
        assert_eq!(find(&off, tracker).payload, "", "location off: any old tracker is removed");
        let on = configs(&layout(true, Units::default()), 7, &info());
        let t = json_of(find(&on, tracker));
        assert_eq!(t["json_attributes_topic"], "sentry-mate/cars/7/location");
        assert_eq!(t["source_type"], "gps");
        assert!(off.iter().chain(&on).all(|m| !m.topic.contains("/latitude/") && !m.topic.contains("/longitude/")));
    }

    #[test]
    fn units_follow_settings() {
        let us = Units { distance: DistanceUnit::Mi, temperature: TemperatureUnit::F, pressure: PressureUnit::Psi };
        let msgs = configs(&layout(false, us), 7, &info());
        let unit = |key: &str| {
            msgs.iter()
                .find(|m| m.topic.ends_with(&format!("/{key}/config")))
                .map(|m| json_of(m)["unit_of_measurement"].as_str().map(String::from))
                .unwrap()
        };
        assert_eq!(unit("range").as_deref(), Some("mi"));
        assert_eq!(unit("speed").as_deref(), Some("mph"));
        assert_eq!(unit("outside_temp").as_deref(), Some("°F"));
        assert_eq!(unit("tire_pressure_rr").as_deref(), Some("psi"));
        assert_eq!(unit("power").as_deref(), Some("kW"));
        assert_eq!(unit("soc").as_deref(), Some("%"));
    }

    #[test]
    fn ids_unique_across_cars_and_fields() {
        let l = layout(true, Units::default());
        let mut ids = HashSet::new();
        let mut topics = HashSet::new();
        for car in [1, 2, 10, 4_000_000_000] {
            for m in configs(&l, car, &info()) {
                assert!(topics.insert(m.topic.clone()), "{}", m.topic);
                let v = json_of(&m);
                assert!(ids.insert(v["unique_id"].as_str().unwrap().to_string()));
                assert_eq!(v["device"]["identifiers"][0], format!("sentry-mate_car{car}"));
            }
        }
        // every value topic but the positions has an entity
        let covered: HashSet<_> = ENTITIES.iter().map(|e| e.field).collect();
        assert!(Field::ALL.iter().all(|f| f.is_location() || covered.contains(f)));
    }

    #[test]
    fn unnamed_car_and_removal() {
        let msgs = configs(&layout(false, Units::default()), 3, &CarInfo::default());
        let v = json_of(find(&msgs, "homeassistant/sensor/sentry-mate_car3/range/config"));
        assert_eq!(v["device"]["name"], "Car 3");
        assert!(v["device"].get("model").is_none());
        let gone = removals(&layout(false, Units::default()), 3);
        let made: HashSet<_> =
            configs(&layout(true, Units::default()), 3, &CarInfo::default()).into_iter().map(|m| m.topic).collect();
        assert_eq!(gone.len(), made.len());
        assert!(gone.iter().all(|m| m.payload.is_empty() && m.retain && made.contains(&m.topic)));
    }
}
