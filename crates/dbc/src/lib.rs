//! Decoding Tesla CAN frames with DBC files, by the same rules as Sentry Six Manager (`server/canDbc.js`,
//! `server/canIndex.js`), so a log decodes to the same values in both.
//!
//! - A DBC set per kind of car ([`Profile`]): Model 3/Y (and the 2021+ S/X) or the legacy Model S/X. Files are read
//!   in order as one text; a later `BO_` replaces the message of its id whole.
//! - A frame is decoded only on a bus the sidecar's bus map (or the set's defaults) names, and only with a message of
//!   that bus: the same id means different things on different buses.
//! - The Screenmate's internal bus (`can3`, "screenmate-internal") is decoded for 0x04F only (its own GPS receiver:
//!   latitude, longitude, accuracy), and only while no car bus has sent 0x04F for 2.5 s.
//! - A signal name used by two messages gets the message id after it (`UI_cpuTemperature_353`): its series name.

pub mod encode;

pub use encode::{EncodeError, encode_message, encode_signal};

use regex::Regex;
use std::collections::{BTreeMap, HashMap};
use std::sync::OnceLock;

/// Which of a multiplexed message's signals a signal is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mux {
    /// Always there.
    None,
    /// The multiplexer: says which of the others the frame carries.
    Multiplexer,
    /// There when the multiplexer has this value.
    Value(u64),
}

#[derive(Debug, Clone)]
pub struct Signal {
    pub name: String,
    /// Unique across the set: the name, or `name_ID` when another message has the same name.
    pub series: String,
    pub message: String,
    pub message_id: u32,
    pub bus: String,
    pub start: u32,
    pub length: u32,
    pub little: bool,
    pub signed: bool,
    pub factor: f64,
    pub offset: f64,
    pub min: f64,
    pub max: f64,
    pub unit: String,
    pub mux: Mux,
    /// Value names (`VAL_`), by raw value.
    pub values: Option<BTreeMap<i64, String>>,
    pub comment: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Message {
    pub id: u32,
    pub name: String,
    pub size: u32,
    pub bus: String,
    pub signals: Vec<Signal>,
    /// Index of the multiplexer in `signals`.
    pub mux: Option<usize>,
}

#[derive(Debug, Clone, Default)]
pub struct Dbc {
    pub messages: HashMap<u32, Message>,
}

fn re(cell: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).unwrap_or_else(|_| Regex::new("$^").unwrap_or_else(|_| unreachable!())))
}

/// JavaScript's `Number(text)` for the number forms DBC files use (NaN when it isn't one).
fn number(s: &str) -> f64 {
    s.parse::<f64>().unwrap_or(f64::NAN)
}

/// Extended ids carry bit 31 in DBC files.
fn plain_id(raw: u64) -> u32 {
    (if raw >= 0x8000_0000 { raw - 0x8000_0000 } else { raw }) as u32
}

impl Dbc {
    /// Reads DBC text: messages, signals, value names and comments.
    pub fn parse(text: &str) -> Dbc {
        static BO: OnceLock<Regex> = OnceLock::new();
        static SG: OnceLock<Regex> = OnceLock::new();
        static VAL: OnceLock<Regex> = OnceLock::new();
        static VAL_PAIR: OnceLock<Regex> = OnceLock::new();
        static CM: OnceLock<Regex> = OnceLock::new();
        let bo = re(&BO, r"^BO_\s+(\d+)\s+([A-Za-z0-9_]+)\s*:\s*(\d+)\s+([A-Za-z0-9_]+)");
        let sg = re(
            &SG,
            r#"^\s+SG_\s+([A-Za-z0-9_]+)\s*(M|m\d+)?\s*:\s*(\d+)\|(\d+)@([01])([+-])\s*\(\s*([-+0-9.eE]+)\s*,\s*([-+0-9.eE]+)\s*\)\s*\[\s*([-+0-9.eE]+)\s*\|\s*([-+0-9.eE]+)\s*\]\s*"([^"]*)""#,
        );
        let mut messages: HashMap<u32, Message> = HashMap::new();
        let mut current: Option<u32> = None;
        let lines: Vec<&str> = text.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l)).collect();
        for line in &lines {
            if let Some(m) = bo.captures(line) {
                let id = plain_id(m[1].parse().unwrap_or(0));
                messages.insert(
                    id,
                    Message {
                        id,
                        name: m[2].to_string(),
                        size: m[3].parse().unwrap_or(0),
                        bus: m[4].to_string(),
                        signals: Vec::new(),
                        mux: None,
                    },
                );
                current = Some(id);
                continue;
            }
            if let (Some(m), Some(cur)) = (sg.captures(line), current.and_then(|c| messages.get_mut(&c))) {
                let mux = match m.get(2).map(|x| x.as_str()) {
                    Some("M") => Mux::Multiplexer,
                    Some(v) => v[1..].parse().map_or(Mux::None, Mux::Value),
                    None => Mux::None,
                };
                let sig = Signal {
                    name: m[1].to_string(),
                    series: String::new(),
                    message: cur.name.clone(),
                    message_id: cur.id,
                    bus: cur.bus.clone(),
                    start: m[3].parse().unwrap_or(0),
                    length: m[4].parse().unwrap_or(0),
                    little: &m[5] == "1",
                    signed: &m[6] == "-",
                    factor: number(&m[7]),
                    offset: number(&m[8]),
                    min: number(&m[9]),
                    max: number(&m[10]),
                    unit: m[11].to_string(),
                    mux,
                    values: None,
                    comment: None,
                };
                if mux == Mux::Multiplexer {
                    cur.mux = Some(cur.signals.len());
                }
                cur.signals.push(sig);
                continue;
            }
            if !line.starts_with(char::is_whitespace) {
                current = None;
            }
        }
        // value names and comments may span lines: read them from the whole text
        let all = lines.join("\n");
        let val = re(&VAL, r#"(?m)^VAL_\s+(\d+)\s+([A-Za-z0-9_]+)\s+((?:-?\d+\s+"[^"]*"\s*)+);"#);
        let pair = re(&VAL_PAIR, r#"(-?\d+)\s+"([^"]*)""#);
        for v in val.captures_iter(&all) {
            let id = plain_id(v[1].parse().unwrap_or(0));
            let Some(sig) = messages.get_mut(&id).and_then(|m| m.signals.iter_mut().find(|s| s.name == v[2])) else {
                continue;
            };
            let mut names = BTreeMap::new();
            for p in pair.captures_iter(&v[3]) {
                if let Ok(k) = p[1].parse::<i64>() {
                    names.insert(k, p[2].to_string());
                }
            }
            sig.values = Some(names);
        }
        let cm = re(&CM, r#"(?m)^CM_\s+SG_\s+(\d+)\s+([A-Za-z0-9_]+)\s+"((?:[^"\\]|\\.)*)"\s*;"#);
        for c in cm.captures_iter(&all) {
            let id = plain_id(c[1].parse().unwrap_or(0));
            if let Some(sig) = messages.get_mut(&id).and_then(|m| m.signals.iter_mut().find(|s| s.name == c[2])) {
                let text: String = c[3].split_whitespace().collect::<Vec<_>>().join(" ");
                sig.comment = Some(text.chars().take(200).collect());
            }
        }
        // series names: the name where it is unique, else name_ID
        let mut count: HashMap<String, usize> = HashMap::new();
        for m in messages.values() {
            for s in &m.signals {
                *count.entry(s.name.clone()).or_default() += 1;
            }
        }
        for m in messages.values_mut() {
            let hex = format!("{:03X}", m.id);
            for s in &mut m.signals {
                s.series = if count.get(&s.name).copied().unwrap_or(0) > 1 {
                    format!("{}_{hex}", s.name)
                } else {
                    s.name.clone()
                };
            }
        }
        Dbc { messages }
    }
}

/// A signal's raw (unscaled) bits, summed as JavaScript does (in floating point, in the same order), so values of
/// over 53 bits come out the same as Sentry Six Manager's. `None` when the frame is too short.
pub fn raw_bits(bytes: &[u8], sig: &Signal) -> Option<f64> {
    let mut value = 0f64;
    if sig.little {
        // Intel: bits counted from the least significant bit of byte 0 upward
        for i in 0..sig.length {
            let bit = sig.start + i;
            let byte = *bytes.get((bit >> 3) as usize)?;
            if (byte >> (bit & 7)) & 1 == 1 {
                value += 2f64.powi(i as i32);
            }
        }
    } else {
        // Motorola: start is the most significant bit; walk toward the least
        let mut bit = sig.start;
        for i in (0..sig.length).rev() {
            let byte = *bytes.get((bit >> 3) as usize)?;
            if (byte >> (bit & 7)) & 1 == 1 {
                value += 2f64.powi(i as i32);
            }
            bit = if bit & 7 == 0 { bit + 15 } else { bit - 1 };
        }
    }
    if sig.signed && sig.length > 0 && value >= 2f64.powi(sig.length as i32 - 1) {
        value -= 2f64.powi(sig.length as i32);
    }
    Some(value)
}

/// A signal's value in a frame: raw × factor + offset.
pub fn signal_value(bytes: &[u8], sig: &Signal) -> Option<f64> {
    raw_bits(bytes, sig).map(|raw| raw * sig.factor + sig.offset)
}

/// Every signal a frame carries (multiplexed ones by the frame's multiplexer value).
pub fn decode_frame<'m>(msg: &'m Message, bytes: &[u8]) -> Vec<(&'m Signal, f64)> {
    let mux_value = msg.mux.and_then(|i| raw_bits(bytes, &msg.signals[i]));
    let mut out = Vec::with_capacity(msg.signals.len());
    for sig in &msg.signals {
        if let Mux::Value(v) = sig.mux
            && mux_value != Some(v as f64)
        {
            continue;
        }
        if let Some(v) = signal_value(bytes, sig).filter(|v| v.is_finite()) {
            out.push((sig, v));
        }
    }
    out
}

/// The DBC sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Profile {
    /// Model 3 and Y of every year, and the 2021+ Model S and X.
    Tesla3Y,
    /// Model S and X before the 2021 refresh.
    TeslaSxLegacy,
}

impl Profile {
    pub fn name(self) -> &'static str {
        match self {
            Profile::Tesla3Y => "tesla-3y",
            Profile::TeslaSxLegacy => "tesla-sx-legacy",
        }
    }

    pub fn from_name(s: &str) -> Profile {
        if s == "tesla-sx-legacy" { Profile::TeslaSxLegacy } else { Profile::Tesla3Y }
    }

    /// Which DBC bus each interface is when the sidecar says nothing.
    pub fn default_buses(self) -> BTreeMap<String, String> {
        let pairs: &[(&str, &str)] = match self {
            Profile::Tesla3Y => &[("can0", "VehicleBus"), ("can1", "ChassisBus")],
            Profile::TeslaSxLegacy => &[("can0", "PowertrainBus"), ("can1", "ChassisBus")],
        };
        pairs.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
    }

    /// The set, parsed once.
    pub fn dbc(self) -> &'static Dbc {
        static TESLA_3Y: OnceLock<Dbc> = OnceLock::new();
        static SX_LEGACY: OnceLock<Dbc> = OnceLock::new();
        match self {
            Profile::Tesla3Y => TESLA_3Y.get_or_init(|| {
                Dbc::parse(
                    &[files::MODEL3CAN, files::MODEL3Y_OPENDBC, files::OVERRIDE].map(|f| format!("{f}\n")).concat(),
                )
            }),
            Profile::TeslaSxLegacy => SX_LEGACY.get_or_init(|| {
                let text = [
                    with_bus(files::OPENDBC_TESLA_CAN, "ChassisBus"),
                    with_bus(files::OPENDBC_TESLA_POWERTRAIN, "PowertrainBus"),
                    files::SX_LEGACY.to_string(),
                ]
                .map(|f| format!("{f}\n"))
                .concat();
                Dbc::parse(&text)
            }),
        }
    }
}

/// A file whose `BO_` lines name the sending ECU (opendbc's): every message gets `bus` instead.
fn with_bus(text: &str, bus: &str) -> String {
    static BO_LINE: OnceLock<Regex> = OnceLock::new();
    let r = re(&BO_LINE, r"(?m)^(BO_\s+\d+\s+[A-Za-z0-9_]+\s*:\s*\d+\s+)[A-Za-z0-9_]+");
    r.replace_all(text, |c: &regex::Captures<'_>| format!("{}{bus}", &c[1])).into_owned()
}

mod files {
    pub const MODEL3CAN: &str = include_str!("../files/Model3CAN.dbc");
    pub const MODEL3Y_OPENDBC: &str = include_str!("../files/model3y-opendbc.dbc");
    pub const OVERRIDE: &str = include_str!("../files/override.dbc");
    pub const OPENDBC_TESLA_CAN: &str = include_str!("../files/opendbc-tesla_can.dbc");
    pub const OPENDBC_TESLA_POWERTRAIN: &str = include_str!("../files/opendbc-tesla_powertrain.dbc");
    pub const SX_LEGACY: &str = include_str!("../files/sx-legacy.dbc");
}

/// The sidecar's bus map as (interface, what it says in lower case).
fn side_entries(side: &serde_json::Value) -> Vec<(String, String)> {
    let map = ["bus", "busMap", "buses"].iter().find_map(|k| side.get(*k).filter(|v| v.is_object()));
    let Some(map) = map.and_then(|m| m.as_object()) else { return Vec::new() };
    map.iter()
        .map(|(iface, v)| {
            let s = match v {
                serde_json::Value::Object(o) => {
                    o.get("dbcBus").or_else(|| o.get("name")).and_then(|x| x.as_str()).unwrap_or("").to_string()
                }
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            (iface.clone(), s.to_lowercase())
        })
        .collect()
}

fn is_internal(s: &str) -> bool {
    s.contains("internal") || s.contains("screenmate")
}

fn plain_iface(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && b.len() <= 16
        && b[0].is_ascii_alphabetic()
        && b.iter().all(|c| c.is_ascii_alphanumeric() || *c == b'_')
}

/// Which DBC bus each interface is: the sidecar's map, else the set's defaults. An interface the map calls internal
/// is no DBC bus.
pub fn bus_map(side: &serde_json::Value, profile: Profile) -> BTreeMap<String, String> {
    let mut map = profile.default_buses();
    for (iface, s) in side_entries(side) {
        if is_internal(&s) {
            map.remove(&iface);
        } else if s.contains("vehicle") {
            map.insert(iface, "VehicleBus".into());
        } else if s.contains("chassis") {
            map.insert(iface, "ChassisBus".into());
        } else if s.contains("powertrain") {
            map.insert(iface, "PowertrainBus".into());
        } else if s.contains("party") {
            map.insert(iface, "PartyBus".into());
        }
    }
    map
}

/// The interfaces the sidecar calls internal.
pub fn internal_buses(side: &serde_json::Value) -> Vec<String> {
    let mut v: Vec<String> =
        side_entries(side).into_iter().filter(|(i, s)| is_internal(s) && plain_iface(i)).map(|(i, _)| i).collect();
    v.sort();
    v
}

/// The one message decoded on an internal bus, and the series kept from it.
pub const INTERNAL_MESSAGE: u32 = 0x04F;
pub const INTERNAL_SERIES: [&str; 3] = ["GPSLatitude04F", "GPSLongitude04F", "GPSAccuracy04F"];
/// A car bus's 0x04F this recent wins over the internal one (ms).
pub const INTERNAL_YIELD_MS: f64 = 2500.0;

/// Decodes one log's frames (one decoder per log, as Sentry Six Manager does).
pub struct Decoder {
    dbc: &'static Dbc,
    buses: BTreeMap<String, String>,
    internal: Vec<String>,
    /// When a car bus last sent [`INTERNAL_MESSAGE`] (ms).
    car_sent: Option<f64>,
}

impl Decoder {
    pub fn new(profile: Profile, sidecar: &serde_json::Value) -> Decoder {
        Decoder {
            dbc: profile.dbc(),
            buses: bus_map(sidecar, profile),
            internal: internal_buses(sidecar),
            car_sent: None,
        }
    }

    pub fn dbc(&self) -> &'static Dbc {
        self.dbc
    }

    pub fn buses(&self) -> &BTreeMap<String, String> {
        &self.buses
    }

    /// The signals of one frame, as (signal, value). Empty when it isn't decoded on its bus.
    pub fn decode(&mut self, frame: &logfmt::Frame) -> Vec<(&'static Signal, f64)> {
        let iface = format!("can{}", frame.bus);
        let Some(msg) = self.dbc.messages.get(&frame.id) else { return Vec::new() };
        let t = frame.time_us as f64 / 1000.0;
        let only_internal = match self.buses.get(&iface) {
            Some(want) => {
                if &msg.bus != want {
                    return Vec::new();
                }
                if frame.id == INTERNAL_MESSAGE {
                    self.car_sent = Some(t);
                }
                false
            }
            None => {
                if frame.id != INTERNAL_MESSAGE || !self.internal.contains(&iface) {
                    return Vec::new();
                }
                if self.car_sent.is_some_and(|c| (t - c).abs() < INTERNAL_YIELD_MS) {
                    return Vec::new();
                }
                true
            }
        };
        let mut out = decode_frame(msg, frame.payload());
        if only_internal {
            out.retain(|(s, _)| INTERNAL_SERIES.contains(&s.series.as_str()));
        }
        out
    }
}
