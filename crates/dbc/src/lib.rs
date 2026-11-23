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
pub mod firmware;

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
    if sig.length <= 53 {
        // every bit fits a float exactly: the sum below is the same whole number, taken out with integers
        return raw_bits_exact(bytes, sig);
    }
    raw_bits_summed(bytes, sig)
}

/// [`raw_bits`] bit by bit in floating point (signals of over 53 bits, and what the quick way is tested against).
fn raw_bits_summed(bytes: &[u8], sig: &Signal) -> Option<f64> {
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

/// [`raw_bits`] of a signal of at most 53 bits.
fn raw_bits_exact(bytes: &[u8], sig: &Signal) -> Option<f64> {
    let len = sig.length;
    if len == 0 {
        return Some(0.0);
    }
    let mut raw = 0u64;
    if sig.little {
        // bits start..start+len, the lowest first: at most 8 bytes, the first's low bits shifted out
        let first = (sig.start >> 3) as usize;
        let last = ((sig.start + len - 1) >> 3) as usize;
        let span = bytes.get(first..=last)?;
        let mut wide = 0u128;
        for (i, b) in span.iter().enumerate() {
            wide |= u128::from(*b) << (8 * i);
        }
        raw = ((wide >> (sig.start & 7)) as u64) & ((1u64 << len) - 1);
    } else {
        let mut bit = sig.start;
        for _ in 0..len {
            let byte = *bytes.get((bit >> 3) as usize)?;
            raw = (raw << 1) | u64::from((byte >> (bit & 7)) & 1);
            bit = if bit & 7 == 0 { bit + 15 } else { bit - 1 };
        }
    }
    let value = if sig.signed && raw >= 1u64 << (len - 1) { raw as i64 - (1i64 << len) } else { raw as i64 };
    Some(value as f64)
}

/// A signal's value in a frame: raw × factor + offset.
pub fn signal_value(bytes: &[u8], sig: &Signal) -> Option<f64> {
    raw_bits(bytes, sig).map(|raw| raw * sig.factor + sig.offset)
}

/// Every signal a frame carries (multiplexed ones by the frame's multiplexer value).
pub fn decode_frame<'m>(msg: &'m Message, bytes: &[u8]) -> Vec<(&'m Signal, f64)> {
    let mux_value = msg.mux_value(bytes);
    let mut out = Vec::with_capacity(msg.signals.len());
    for sig in &msg.signals {
        if let Some(v) = sig.value_in(bytes, mux_value) {
            out.push((sig, v));
        }
    }
    out
}

impl Message {
    /// The value of the frame's multiplexer signal (which page of the message it is), when the message has one.
    pub fn mux_value(&self, bytes: &[u8]) -> Option<f64> {
        self.mux.and_then(|i| raw_bits(bytes, &self.signals[i]))
    }
}

impl Signal {
    /// The signal's value in a frame of its message, as [`decode_frame`] gives it: `None` when the frame is another
    /// page (`mux_value` is the message's [`Message::mux_value`]), too short, or the value is not finite.
    pub fn value_in(&self, bytes: &[u8], mux_value: Option<f64>) -> Option<f64> {
        if let Mux::Value(v) = self.mux
            && mux_value != Some(v as f64)
        {
            return None;
        }
        signal_value(bytes, self).filter(|v| v.is_finite())
    }
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
    /// What each interface seen so far is (a log has a few), worked out at its first frame.
    roles: Vec<(u8, Role)>,
}

/// What an interface (`can0`, `can1`, ...) is to the decoder.
enum Role {
    /// One of the car's buses, by its DBC name.
    Car(String),
    /// A Screenmate internal bus.
    Internal,
    /// Not in the bus map: its frames are left.
    Other,
}

impl Decoder {
    pub fn new(profile: Profile, sidecar: &serde_json::Value) -> Decoder {
        Decoder {
            dbc: profile.dbc(),
            buses: bus_map(sidecar, profile),
            internal: internal_buses(sidecar),
            car_sent: None,
            roles: Vec::new(),
        }
    }

    pub fn dbc(&self) -> &'static Dbc {
        self.dbc
    }

    pub fn buses(&self) -> &BTreeMap<String, String> {
        &self.buses
    }

    /// The signals of one frame, as (signal, value). Empty when it isn't decoded on its bus.
    /// The message a frame carries, when its interface is one of the car's buses and the message is that bus's (or
    /// the Screenmate's own position on an internal bus, while the car sends none: `true` then, and only
    /// [`INTERNAL_SERIES`] count). Every frame of a log goes through here, in order: the car's own position frames
    /// are noted.
    pub fn message(&mut self, frame: &logfmt::Frame) -> Option<(&'static Message, bool)> {
        let msg = self.dbc.messages.get(&frame.id)?;
        let role = match self.roles.iter().find(|(bus, _)| *bus == frame.bus) {
            Some((_, role)) => role,
            None => {
                let iface = format!("can{}", frame.bus);
                let role = match self.buses.get(&iface) {
                    Some(want) => Role::Car(want.clone()),
                    None if self.internal.contains(&iface) => Role::Internal,
                    None => Role::Other,
                };
                self.roles.push((frame.bus, role));
                &self.roles[self.roles.len() - 1].1
            }
        };
        match role {
            Role::Car(want) => {
                if &msg.bus != want {
                    return None;
                }
                if frame.id == INTERNAL_MESSAGE {
                    self.car_sent = Some(frame.time_us as f64 / 1000.0);
                }
                Some((msg, false))
            }
            Role::Internal => {
                if frame.id != INTERNAL_MESSAGE {
                    return None;
                }
                let t = frame.time_us as f64 / 1000.0;
                if self.car_sent.is_some_and(|c| (t - c).abs() < INTERNAL_YIELD_MS) {
                    return None;
                }
                Some((msg, true))
            }
            Role::Other => None,
        }
    }

    pub fn decode(&mut self, frame: &logfmt::Frame) -> Vec<(&'static Signal, f64)> {
        let Some((msg, only_internal)) = self.message(frame) else { return Vec::new() };
        let mut out = decode_frame(msg, frame.payload());
        if only_internal {
            out.retain(|(s, _)| INTERNAL_SERIES.contains(&s.series.as_str()));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
    use super::*;

    /// The quick way of taking a signal's bits out gives what the bit-by-bit sum gives: every signal of both sets,
    /// on made-up payloads (all zero, all one, mixed), whole and cut short.
    #[test]
    fn the_quick_bits_are_the_summed_bits() {
        let mut x = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        let mut checked = 0u32;
        for profile in [Profile::Tesla3Y, Profile::TeslaSxLegacy] {
            for msg in profile.dbc().messages.values() {
                let size = (msg.size as usize).clamp(1, 64);
                for round in 0..40 {
                    let mut bytes = vec![0u8; size];
                    match round {
                        0 => {}
                        1 => bytes.fill(0xFF),
                        _ => bytes.iter_mut().for_each(|b| *b = next() as u8),
                    }
                    for cut in [size, size.saturating_sub(1), size / 2, 0] {
                        for sig in &msg.signals {
                            let slow = raw_bits_summed(&bytes[..cut], sig);
                            assert_eq!(raw_bits(&bytes[..cut], sig), slow, "{} in {:03X}", sig.name, msg.id);
                            if sig.length <= 53 {
                                checked += 1;
                            }
                        }
                    }
                }
            }
        }
        assert!(checked > 100_000, "{checked}");
        // made-up layouts the files may not have: every start and length, both byte orders, signed and not
        for start in 0..64u32 {
            for length in 0..=64u32 {
                for (little, signed) in [(true, false), (true, true), (false, false), (false, true)] {
                    let sig = Signal {
                        name: String::new(),
                        series: String::new(),
                        message: String::new(),
                        message_id: 0,
                        bus: String::new(),
                        start,
                        length,
                        little,
                        signed,
                        factor: 1.0,
                        offset: 0.0,
                        min: 0.0,
                        max: 0.0,
                        unit: String::new(),
                        mux: Mux::None,
                        values: None,
                        comment: None,
                    };
                    for _ in 0..8 {
                        let bytes = next().to_le_bytes();
                        for cut in [8, 5, 1] {
                            let slow = raw_bits_summed(&bytes[..cut], &sig);
                            assert_eq!(raw_bits(&bytes[..cut], &sig), slow, "{start}|{length} {little} {signed}");
                        }
                    }
                }
            }
        }
    }
}
