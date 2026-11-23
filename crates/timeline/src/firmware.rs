//! The firmware fingerprints one log's frames carry ([`dbc::firmware`]): which values each field had on the vehicle
//! bus, how often, and when.
//!
//! Only `can0` (the vehicle bus) counts: the gateway copies some of these messages to the chassis bus, and a copy
//! must not count twice or bring another bus's view. A value is not trusted from one frame: the history takes a value
//! once it was seen [`CONFIRM`] times within one awake span (a corrupt frame must not look like an update). What
//! this collects is the raw tally; the confirming is done over the span's logs together, since one log can end
//! before a field was sent often enough.

use serde::{Deserialize, Serialize};

/// How many sightings within one awake span make a value confirmed: 3. The modules send these about once a second
/// (each multiplexer value every few seconds), so a minute awake sees a real value dozens of times and even a short
/// wake-up reaches three; a corrupt frame would have to come out wrong the same way three times, which does not
/// happen. More would only lose short wake-ups.
pub const CONFIRM: u32 = 3;

/// The most distinct field values kept from one log. A log has about 32 (one per field); corrupt frames add a few. A
/// crafted log with millions of made-up values would otherwise grow every record made from it.
pub const MAX_VALUES: usize = 128;

/// One field value a log's vehicle-bus frames carried.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Seen {
    /// The DBC's field name (`BMS_appCrc`).
    pub field: String,
    pub value: u64,
    /// How many frames carried it.
    pub count: u32,
    /// When it was first and last seen (ms).
    pub first_ms: u64,
    pub last_ms: u64,
}

/// What one log says about the car's firmware.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub struct LogFirmware {
    /// In the order first seen.
    pub seen: Vec<Seen>,
    /// `# firmware changed:` lines the bridge wrote (a hint only; the frames are what counts).
    pub hints: u32,
}

impl LogFirmware {
    pub fn is_empty(&self) -> bool {
        self.seen.is_empty() && self.hints == 0
    }
}

/// Reads a log's lines into its [`LogFirmware`].
#[derive(Debug, Clone, Default)]
pub struct FirmwareCollector {
    out: LogFirmware,
}

impl FirmwareCollector {
    pub fn new() -> FirmwareCollector {
        FirmwareCollector::default()
    }

    /// One frame: counted when it is a module's info frame on `can0` with a value that says something.
    pub fn frame(&mut self, f: &logfmt::Frame) {
        if f.bus != 0 || f.extended || dbc::firmware::module_of(f.id).is_none() {
            return;
        }
        let ms = f.time_us / 1000;
        for (_, field, value) in dbc::firmware::read(f.id, f.payload()) {
            let seen = &mut self.out.seen;
            if let Some(s) = seen.iter_mut().find(|s| s.value == value && s.field == field.name) {
                s.count = s.count.saturating_add(1);
                s.first_ms = s.first_ms.min(ms);
                s.last_ms = s.last_ms.max(ms);
            } else if seen.len() < MAX_VALUES {
                seen.push(Seen { field: field.name.to_string(), value, count: 1, first_ms: ms, last_ms: ms });
            }
        }
    }

    pub fn line(&mut self, line: &logfmt::Line<'_>) {
        match line {
            logfmt::Line::Frame(f) => self.frame(f),
            logfmt::Line::FirmwareChanged(_) => self.out.hints = self.out.hints.saturating_add(1),
            _ => {}
        }
    }

    pub fn finish(self) -> LogFirmware {
        self.out
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn read(text: &str) -> LogFirmware {
        let mut c = FirmwareCollector::new();
        for l in text.lines() {
            c.line(&logfmt::Line::parse(l.as_bytes()));
        }
        c.finish()
    }

    #[test]
    fn counts_can0_only_and_skips_zero_and_sna() {
        let fw = read(
            "(1767225600.100000) can0 300#0D000000484280F2\n\
             (1767225601.100000) can1 300#0D000000DDCCBBAA\n\
             (1767225602.100000) can0 300#0D000000484280F2\n\
             (1767225603.100000) can0 300#0D00000000000000\n\
             (1767225604.100000) can0 300#0D000000FFFFFFFF\n\
             (1767225605.100000) can0 300#0D000000484280F3\n\
             # firmware changed: Battery management (0x300), BMS_appCrc 920f4248 -> f2804248\n\
             # something else\n\
             (1767225606.100000) can0 300#0D000000484280F2\n",
        );
        assert_eq!(fw.hints, 1);
        assert_eq!(
            fw.seen,
            vec![
                Seen {
                    field: "BMS_appCrc".into(),
                    value: 0xF280_4248,
                    count: 3,
                    first_ms: 1_767_225_600_100,
                    last_ms: 1_767_225_606_100
                },
                Seen {
                    field: "BMS_appCrc".into(),
                    value: 0xF380_4248,
                    count: 1,
                    first_ms: 1_767_225_605_100,
                    last_ms: 1_767_225_605_100
                },
            ]
        );
    }

    #[test]
    fn a_flood_of_values_is_bounded() {
        let mut text = String::new();
        for i in 1..=1000u32 {
            text += &format!("(1767225600.100000) can0 300#0D000000{:08X}\n", i.swap_bytes());
        }
        assert_eq!(read(&text).seen.len(), MAX_VALUES);
    }
}
