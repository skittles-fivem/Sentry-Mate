//! Where a GPS message keeps its latitude, longitude and accuracy: a field's bits (start, width, byte order, sign)
//! and its scale and offset, written as a DBC file writes a signal (`28|28@1- (1e-06,0)`). The decoder reads the
//! Screenmate's own receiver (0x04F on its internal bus) with the DBC's layout unless a car has one of these set
//! (`sentry-mate gps-layout` works one out from the stored logs). A DBC's `1E-006` and Rust's `1e-6` both read.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// One field of an 8-byte frame.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct GpsField {
    /// The start bit as a DBC file counts it: the least significant bit for Intel (`@1`), the most significant for
    /// Motorola (`@0`).
    pub start: u8,
    pub width: u8,
    pub motorola: bool,
    pub signed: bool,
    pub scale: f64,
    pub offset: f64,
}

/// The three fields of a GPS frame (no accuracy: none is read).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct GpsLayout {
    pub lat: GpsField,
    pub lon: GpsField,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accuracy: Option<GpsField>,
}

impl GpsField {
    /// Whether the field fits in 8 bytes and its numbers are usable.
    pub fn valid(&self) -> bool {
        let fits = if self.motorola {
            self.start < 64 && motorola_index(self.start) + u32::from(self.width) <= 64
        } else {
            u32::from(self.start) + u32::from(self.width) <= 64
        };
        (1..=32).contains(&self.width) && fits && self.scale.is_finite() && self.scale != 0.0 && self.offset.is_finite()
    }

    /// The field's raw value in a frame (cut-short frames are padded with zeros).
    pub fn raw(&self, payload: &[u8]) -> i64 {
        let mut b = [0u8; 8];
        let n = payload.len().min(8);
        b[..n].copy_from_slice(&payload[..n]);
        self.raw_of(u64::from_le_bytes(b), u64::from_be_bytes(b))
    }

    /// [`Self::raw`] of a frame as both its little-endian and big-endian 64-bit numbers.
    pub fn raw_of(&self, le: u64, be: u64) -> i64 {
        let w = u32::from(self.width);
        let bits = if self.motorola {
            let l = motorola_index(self.start);
            (be << l) >> (64 - w)
        } else {
            (le >> self.start) & mask(w)
        };
        if self.signed && w > 0 && bits >> (w - 1) & 1 == 1 { bits as i64 - (1i64 << w) } else { bits as i64 }
    }

    /// The field's value in a frame.
    pub fn value(&self, payload: &[u8]) -> f64 {
        self.raw(payload) as f64 * self.scale + self.offset
    }
}

fn mask(w: u32) -> u64 {
    if w >= 64 { u64::MAX } else { (1u64 << w) - 1 }
}

/// A Motorola start bit (DBC numbering: bit 7 of byte 0 is 7, its next bit 6 ... bit 0 of byte 0 is followed by bit
/// 7 of byte 1, 15) as its place counted from the most significant bit of the frame read big-endian.
pub fn motorola_index(start: u8) -> u32 {
    u32::from(start / 8) * 8 + (7 - u32::from(start % 8))
}

/// A number as short as it stays exact (`1e-6`, `0.2`, `0`).
fn num(x: f64) -> String {
    let plain = format!("{x}");
    let sci = format!("{x:e}");
    if sci.len() < plain.len() { sci } else { plain }
}

impl fmt::Display for GpsField {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let order = if self.motorola { 0 } else { 1 };
        let sign = if self.signed { '-' } else { '+' };
        write!(f, "{}|{}@{}{} ({},{})", self.start, self.width, order, sign, num(self.scale), num(self.offset))
    }
}

impl fmt::Display for GpsLayout {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "lat {}; lon {}", self.lat, self.lon)?;
        if let Some(a) = &self.accuracy {
            write!(f, "; accuracy {a}")?;
        }
        Ok(())
    }
}

impl FromStr for GpsField {
    type Err = String;

    /// `28|28@1- (1e-06,0)` (spaces anywhere).
    fn from_str(s: &str) -> Result<GpsField, String> {
        let s: String = s.chars().filter(|c| !c.is_whitespace()).collect();
        let bad = || format!("not a field like 28|28@1- (1e-06,0): {s}");
        let (bits, rest) = s.split_once('@').ok_or_else(bad)?;
        let (start, width) = bits.split_once('|').ok_or_else(bad)?;
        let mut r = rest.chars();
        let motorola = match r.next() {
            Some('0') => true,
            Some('1') => false,
            _ => return Err(bad()),
        };
        let signed = match r.next() {
            Some('-') => true,
            Some('+') => false,
            _ => return Err(bad()),
        };
        let nums = r.as_str().strip_prefix('(').and_then(|x| x.strip_suffix(')')).ok_or_else(bad)?;
        let (scale, offset) = nums.split_once(',').ok_or_else(bad)?;
        let field = GpsField {
            start: start.parse().map_err(|_| bad())?,
            width: width.parse().map_err(|_| bad())?,
            motorola,
            signed,
            scale: scale.parse().map_err(|_| bad())?,
            offset: offset.parse().map_err(|_| bad())?,
        };
        if !field.valid() {
            return Err(format!("the field does not fit in 8 bytes, or its scale is 0: {s}"));
        }
        Ok(field)
    }
}

impl FromStr for GpsLayout {
    type Err = String;

    /// `lat 0|28@1- (1e-06,0); lon 28|28@1- (1e-06,0)`, with `; accuracy ...` if wanted (what `gps-layout`
    /// prints).
    fn from_str(s: &str) -> Result<GpsLayout, String> {
        let (mut lat, mut lon, mut accuracy) = (None, None, None);
        for part in s.split(';').map(str::trim).filter(|p| !p.is_empty()) {
            let (name, field) = part.split_once(char::is_whitespace).ok_or_else(|| format!("no field: {part}"))?;
            let f: GpsField = field.parse()?;
            match name {
                "lat" => lat = Some(f),
                "lon" => lon = Some(f),
                "accuracy" => accuracy = Some(f),
                _ => return Err(format!("lat, lon or accuracy, not {name}")),
            }
        }
        match (lat, lon) {
            (Some(lat), Some(lon)) => Ok(GpsLayout { lat, lon, accuracy }),
            _ => Err("both lat and lon are needed".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn written_and_read_back_as_a_dbc_writes_a_signal() {
        let s = "lat 0|28@1- (1e-06,0); lon 28|28@1- (1e-06,0); accuracy 57|7@1+ (0.2,0)";
        let l: GpsLayout = s.parse().unwrap();
        let written = "lat 0|28@1- (1e-6,0); lon 28|28@1- (1e-6,0); accuracy 57|7@1+ (0.2,0)";
        assert_eq!(l.to_string(), written);
        assert_eq!(written.parse::<GpsLayout>().unwrap(), l);
        assert_eq!(l.lon, GpsField { start: 28, width: 28, motorola: false, signed: true, scale: 1e-6, offset: 0.0 });
        assert!("lat 0|28@1- (1e-06,0)".parse::<GpsLayout>().is_err(), "no lon");
        assert!("lat 60|28@1- (1e-06,0); lon 0|8@1+ (1,0)".parse::<GpsLayout>().is_err(), "past the 8 bytes");
        assert!("lat 0|28@1- (0,0); lon 0|8@1+ (1,0)".parse::<GpsLayout>().is_err(), "scale 0");
    }

    #[test]
    fn intel_and_motorola_bits() {
        // a made-up value, -12.5 degrees, 28 bits signed, Intel at bit 28
        let f = GpsField { start: 28, width: 28, motorola: false, signed: true, scale: 1e-6, offset: 0.0 };
        let raw = (-12_500_000i64).rem_euclid(1 << 28) as u64;
        let frame = (raw << 28).to_le_bytes();
        assert!((f.value(&frame) + 12.5).abs() < 1e-9);
        // Motorola: start 7 (the top bit of byte 0), 16 bits: bytes 0 and 1 big-endian
        let m = GpsField { start: 7, width: 16, motorola: true, signed: false, scale: 1.0, offset: 0.0 };
        assert_eq!(m.raw(&[0x12, 0x34, 0, 0, 0, 0, 0, 0]), 0x1234);
        // start 3 (bit 3 of byte 0), 8 bits: the low nibble of byte 0 then the high nibble of byte 1
        let m = GpsField { start: 3, width: 8, ..m };
        assert_eq!(m.raw(&[0xA5, 0xC3, 0, 0, 0, 0, 0, 0]), 0x5C);
        assert!(m.valid() && !GpsField { start: 56, width: 16, ..m }.valid());
    }
}
