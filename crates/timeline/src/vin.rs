//! The car's VIN from its own CAN frames, and what it says (the rules of Sentry Six Manager's `web/carModels.js`).
//!
//! A Model 3 or Y (and a 2021+ Model S or X) sends its VIN in 0x405, a Model S or X from before the 2021 refresh in
//! 0x508, each in three pages:
//!
//! | Message | Page (byte 0) | Characters |
//! |---|---|---|
//! | 0x405 | 0x10, 0x11, 0x12 | bytes 5-7 (3), then 1-7 (7), then 1-7 (7) |
//! | 0x508 | 0, 1, 2 | bytes 1-7 (7), then 1-7 (7), then 1-3 (3) |
//!
//! The 4th character is the model (`3`, `Y`, `S`, `X`), the 10th the model year, the 11th the plant. Only the first
//! 11 characters are ever kept ([`model::VinInfo::vin_prefix`]): the last six are the car's serial number.

use model::VinInfo;

/// The VIN's 10th character, by model year from 2010: the letters I, O, Q, U, Z and the digit 0 are not used.
const YEAR_CODES: &[u8] = b"ABCDEFGHJKLMNPRSTVWXY123456789";

/// The model year a VIN's 10th character stands for (2010 to 2039): `L` 2020, `M` 2021, `N` 2022, `P` 2023, `R`
/// 2024, `S` 2025, `T` 2026, `V` 2027 ...
pub fn year_of(code: char) -> Option<u16> {
    let c = code.to_ascii_uppercase();
    YEAR_CODES.iter().position(|&y| char::from(y) == c).and_then(|i| u16::try_from(i).ok()).map(|i| 2010 + i)
}

/// Seventeen characters of the VIN alphabet (digits and capitals without I, O, Q).
fn is_vin(v: &[u8]) -> bool {
    v.len() == 17 && v.iter().all(|b| matches!(b, b'A'..=b'H' | b'J'..=b'N' | b'P' | b'R'..=b'Z' | b'0'..=b'9'))
}

/// What a VIN says, without its serial number. `via` is the message it came in (0x405 or 0x508). `None` when the
/// text is no VIN.
pub fn decode_vin(vin: &str, via: u32) -> Option<VinInfo> {
    let v = vin.trim().to_ascii_uppercase();
    let b = v.as_bytes();
    if !is_vin(b) {
        return None;
    }
    let ch = |i: usize| char::from(b[i]);
    Some(VinInfo {
        vin_prefix: v.chars().take(11).collect(),
        model: matches!(ch(3), '3' | 'Y' | 'S' | 'X').then_some(ch(3)),
        year: year_of(ch(9)),
        plant: Some(ch(10)),
        via,
    })
}

/// Collects the VIN from a log's frames, one at a time, on whichever bus they come.
#[derive(Debug, Clone, Default)]
pub struct VinCollector {
    /// The pages of 0x405 and of 0x508 seen so far.
    parts: [[Option<String>; 3]; 2],
    found: Option<VinInfo>,
}

impl VinCollector {
    pub fn new() -> VinCollector {
        VinCollector::default()
    }

    /// One frame. Returns the VIN once the three pages of one message have come (and from then on).
    pub fn add(&mut self, id: u32, data: &[u8]) -> Option<&VinInfo> {
        if self.found.is_some() || data.len() < 8 {
            return self.found.as_ref();
        }
        let chars = |from: usize, to: usize| -> String { data[from..=to].iter().map(|&c| char::from(c)).collect() };
        let (which, page, text) = match id {
            0x405 => {
                if data[0] & 0xF0 != 0x10 || data[0] & 0x0F > 2 {
                    return None;
                }
                let page = usize::from(data[0] & 0x0F);
                (0, page, if page == 0 { chars(5, 7) } else { chars(1, 7) })
            }
            0x508 => {
                if data[0] > 2 {
                    return None;
                }
                let page = usize::from(data[0]);
                (1, page, if page == 2 { chars(1, 3) } else { chars(1, 7) })
            }
            _ => return None,
        };
        let parts = &mut self.parts[which];
        parts[page] = Some(text);
        if let [Some(a), Some(b), Some(c)] = parts {
            match decode_vin(&format!("{a}{b}{c}"), id) {
                Some(v) => self.found = Some(v),
                // pages of two different sends: start again
                None => *parts = [None, None, None],
            }
        }
        self.found.as_ref()
    }

    /// A CAN Bridge frame (11-bit ids only, as Sentry Six Manager reads them).
    pub fn add_frame(&mut self, frame: &logfmt::Frame) -> Option<&VinInfo> {
        if frame.extended {
            return self.found.as_ref();
        }
        self.add(frame.id, frame.payload())
    }

    pub fn vin(&self) -> Option<&VinInfo> {
        self.found.as_ref()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
    use super::*;

    /// A made-up VIN (not a real car's): maker XP7, Model Y, 2024 (`R`), plant `B`.
    const FAKE: &str = "XP7YGCEK0RB000042";

    fn pages_405(vin: &str) -> [[u8; 8]; 3] {
        let v = vin.as_bytes();
        let mut p = [[0u8; 8]; 3];
        p[0][0] = 0x10;
        p[0][5..8].copy_from_slice(&v[0..3]);
        p[1][0] = 0x11;
        p[1][1..8].copy_from_slice(&v[3..10]);
        p[2][0] = 0x12;
        p[2][1..8].copy_from_slice(&v[10..17]);
        p
    }

    fn pages_508(vin: &str) -> [[u8; 8]; 3] {
        let v = vin.as_bytes();
        let mut p = [[0u8; 8]; 3];
        p[0][0] = 0;
        p[0][1..8].copy_from_slice(&v[0..7]);
        p[1][0] = 1;
        p[1][1..8].copy_from_slice(&v[7..14]);
        p[2][0] = 2;
        p[2][1..4].copy_from_slice(&v[14..17]);
        p
    }

    #[test]
    fn years_by_the_tenth_character() {
        assert_eq!(year_of('L'), Some(2020));
        assert_eq!(year_of('M'), Some(2021));
        assert_eq!(year_of('N'), Some(2022));
        assert_eq!(year_of('P'), Some(2023));
        assert_eq!(year_of('R'), Some(2024));
        assert_eq!(year_of('S'), Some(2025));
        assert_eq!(year_of('T'), Some(2026));
        assert_eq!(year_of('V'), Some(2027));
        assert_eq!(year_of('9'), Some(2039));
        assert_eq!(year_of('a'), Some(2010));
        assert_eq!(year_of('U'), None);
        assert_eq!(year_of('0'), None);
    }

    #[test]
    fn decoding_keeps_no_serial_number() {
        let v = decode_vin(FAKE, 0x405).unwrap();
        assert_eq!(v.vin_prefix, "XP7YGCEK0RB");
        assert_eq!(v.model, Some('Y'));
        assert_eq!(v.year, Some(2024));
        assert_eq!(v.plant, Some('B'));
        assert_eq!(v.via, 0x405);
        let other = decode_vin("5yj3e1ea7lf000001", 0x405).unwrap();
        assert_eq!((other.model, other.year, other.plant), (Some('3'), Some(2020), Some('F')));
        // an unknown model letter, letters a VIN never has, wrong lengths
        assert_eq!(decode_vin("5YJAE1EA7LF000001", 0x405).unwrap().model, None);
        assert!(decode_vin("5YJ3E1EA7LF00000I", 0x405).is_none());
        assert!(decode_vin("5YJ3E1EA7LF00001", 0x405).is_none());
        assert!(decode_vin("", 0x405).is_none());
    }

    #[test]
    fn collects_the_three_pages_of_0x405_in_any_order() {
        let p = pages_405(FAKE);
        let mut c = VinCollector::new();
        assert!(c.add(0x405, &p[2]).is_none());
        assert!(c.add(0x405, &p[0]).is_none());
        let v = c.add(0x405, &p[1]).cloned().unwrap();
        assert_eq!(v.vin_prefix, "XP7YGCEK0RB");
        assert_eq!(v.via, 0x405);
        // kept once found, whatever comes next
        assert_eq!(c.add(0x123, &[0; 8]), Some(&v));
    }

    #[test]
    fn collects_a_legacy_model_s_from_0x508() {
        let p = pages_508("5YJSA1E26HF000007");
        let mut c = VinCollector::new();
        for page in &p {
            c.add(0x508, page);
        }
        let v = c.vin().unwrap();
        assert_eq!((v.model, v.year, v.via), (Some('S'), Some(2017), 0x508));
    }

    #[test]
    fn ignores_short_frames_wrong_pages_and_starts_again_on_nonsense() {
        let p = pages_405(FAKE);
        let mut c = VinCollector::new();
        assert!(c.add(0x405, &p[0][..7]).is_none());
        let mut bad = p[1];
        bad[0] = 0x13;
        assert!(c.add(0x405, &bad).is_none());
        let mut junk = p[1];
        junk[3] = b'O'; // not in the VIN alphabet
        c.add(0x405, &p[0]);
        c.add(0x405, &junk);
        assert!(c.add(0x405, &p[2]).is_none(), "a garbled page gives no VIN");
        // the pages were dropped: all three are needed again
        assert!(c.add(0x405, &p[1]).is_none());
        c.add(0x405, &p[0]);
        assert!(c.add(0x405, &p[2]).is_some());
    }
}
