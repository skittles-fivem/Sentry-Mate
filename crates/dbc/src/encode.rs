//! The decoder's exact inverse: a signal's value into a frame's bits, as [`crate::signal_value`] reads them back.
//!
//! Made for tests and the made-up logs of the `synth` crate: a value is rounded to the signal's step (its factor),
//! refused when its raw number does not fit the signal's bits, and written Intel or Motorola as the DBC says. What
//! comes back from [`encode_signal`] is the value the decoder reads from the bytes, bit for bit.

use crate::{Message, Mux, Signal, signal_value};

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum EncodeError {
    #[error("{0} = {1} does not fit its {2} bits")]
    OutOfRange(String, f64, u32),
    #[error("{0} has no step (factor 0) or is over 64 bits")]
    NotEncodable(String),
    #[error("{0} reaches past the frame's {1} bytes")]
    TooShort(String, usize),
    #[error("{0} is not a signal of {1}")]
    UnknownSignal(String, String),
    #[error("{0} needs multiplexer value {1}, another signal set it to {2}")]
    MuxConflict(String, u64, u64),
    #[error("{0} is not a finite number")]
    NotFinite(String),
}

/// The raw (unscaled) number a value is written as: `round((value - offset) / factor)`, checked against the bits.
pub fn raw_of(sig: &Signal, value: f64) -> Result<i128, EncodeError> {
    if !value.is_finite() {
        return Err(EncodeError::NotFinite(sig.series.clone()));
    }
    if sig.factor == 0.0 || !sig.factor.is_finite() || sig.length == 0 || sig.length > 64 {
        return Err(EncodeError::NotEncodable(sig.series.clone()));
    }
    let raw = ((value - sig.offset) / sig.factor).round();
    let (lo, hi) = if sig.signed {
        (-(2f64.powi(sig.length as i32 - 1)), 2f64.powi(sig.length as i32 - 1) - 1.0)
    } else {
        (0.0, 2f64.powi(sig.length as i32) - 1.0)
    };
    if !(lo..=hi).contains(&raw) {
        return Err(EncodeError::OutOfRange(sig.series.clone(), value, sig.length));
    }
    Ok(raw as i128)
}

/// The bit positions of a signal in a frame, least significant bit of the value first.
fn positions(sig: &Signal) -> Vec<u32> {
    if sig.little {
        (0..sig.length).map(|i| sig.start + i).collect()
    } else {
        // Motorola: start is the most significant bit; walk toward the least, as the decoder does
        let mut out = Vec::with_capacity(sig.length as usize);
        let mut bit = sig.start;
        for _ in 0..sig.length {
            out.push(bit);
            bit = if bit & 7 == 0 { bit + 15 } else { bit.wrapping_sub(1) };
        }
        out.reverse();
        out
    }
}

/// Writes a raw number into the signal's bits (two's complement when signed); the other bits are left as they are.
pub fn write_raw(bytes: &mut [u8], sig: &Signal, raw: i128) -> Result<(), EncodeError> {
    let pos = positions(sig);
    let len = bytes.len();
    if pos.iter().any(|&b| (b >> 3) as usize >= len) {
        return Err(EncodeError::TooShort(sig.series.clone(), len));
    }
    let bits = raw as u128;
    for (i, &b) in pos.iter().enumerate() {
        let byte = &mut bytes[(b >> 3) as usize];
        let mask = 1u8 << (b & 7);
        if (bits >> i) & 1 == 1 {
            *byte |= mask;
        } else {
            *byte &= !mask;
        }
    }
    Ok(())
}

/// Writes `value` into the signal's bits and gives back what the decoder reads from them (the value rounded to the
/// signal's step).
pub fn encode_signal(bytes: &mut [u8], sig: &Signal, value: f64) -> Result<f64, EncodeError> {
    let raw = raw_of(sig, value)?;
    write_raw(bytes, sig, raw)?;
    signal_value(bytes, sig).ok_or_else(|| EncodeError::TooShort(sig.series.clone(), bytes.len()))
}

/// The signal of a message by its series name or its plain name.
pub fn signal_of<'m>(msg: &'m Message, name: &str) -> Option<&'m Signal> {
    msg.signals.iter().find(|s| s.series == name).or_else(|| msg.signals.iter().find(|s| s.name == name))
}

/// One frame of a message (its DBC size, other bits 0) carrying these values, by series or plain name. A multiplexed
/// signal sets the multiplexer to its page (two pages in one frame are refused). Gives back each value as the
/// decoder reads it, in the order given.
pub fn encode_message(msg: &Message, values: &[(&str, f64)]) -> Result<(Vec<u8>, Vec<f64>), EncodeError> {
    let mut bytes = vec![0u8; msg.size as usize];
    let mut page: Option<u64> = None;
    let mut sigs = Vec::with_capacity(values.len());
    for &(name, _) in values {
        let sig = signal_of(msg, name).ok_or_else(|| EncodeError::UnknownSignal(name.to_string(), msg.name.clone()))?;
        if let Mux::Value(v) = sig.mux {
            match page {
                Some(p) if p != v => return Err(EncodeError::MuxConflict(sig.series.clone(), v, p)),
                _ => page = Some(v),
            }
        }
        sigs.push(sig);
    }
    if let (Some(p), Some(m)) = (page, msg.mux) {
        write_raw(&mut bytes, &msg.signals[m], i128::from(p))?;
    }
    let mut out = Vec::with_capacity(values.len());
    for (sig, &(_, v)) in sigs.iter().zip(values) {
        if sig.mux == Mux::Multiplexer
            && let Some(p) = page
            && raw_of(sig, v)? != i128::from(p)
        {
            return Err(EncodeError::MuxConflict(sig.series.clone(), p, v as u64));
        }
        out.push(encode_signal(&mut bytes, sig, v)?);
    }
    Ok((bytes, out))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
    use super::*;
    use crate::{Profile, decode_frame, raw_bits};

    /// A small deterministic generator (no dependency for a test).
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        }
    }

    fn mask(sig: &Signal, size: usize) -> Vec<u8> {
        let mut m = vec![0u8; size];
        for b in positions(sig) {
            m[(b >> 3) as usize] |= 1 << (b & 7);
        }
        m
    }

    #[test]
    fn every_signal_of_every_set_comes_back_bit_for_bit() {
        let mut rng = Rng(7);
        for profile in [Profile::Tesla3Y, Profile::TeslaSxLegacy] {
            let mut checked = 0;
            for msg in profile.dbc().messages.values() {
                let size = msg.size as usize;
                for sig in &msg.signals {
                    if sig.length == 0
                        || sig.length > 52
                        || sig.factor == 0.0
                        || positions(sig).iter().any(|&b| (b >> 3) as usize >= size)
                    {
                        continue;
                    }
                    for _ in 0..8 {
                        // random bytes: decode, encode the value into a clean frame, the signal's bits are the same
                        let bytes: Vec<u8> = (0..size).map(|_| rng.next() as u8).collect();
                        let raw = raw_bits(&bytes, sig).unwrap();
                        let value = raw * sig.factor + sig.offset;
                        let mut out = vec![0u8; size];
                        let back = encode_signal(&mut out, sig, value).unwrap_or_else(|e| panic!("{e} ({raw})"));
                        let m = mask(sig, size);
                        for i in 0..size {
                            assert_eq!(out[i] & m[i], bytes[i] & m[i], "{} byte {i}", sig.series);
                            assert_eq!(out[i] & !m[i], 0, "{} wrote outside its bits", sig.series);
                        }
                        assert_eq!(back.to_bits(), value.to_bits(), "{}", sig.series);
                        assert_eq!(raw_bits(&out, sig), Some(raw));
                    }
                    checked += 1;
                }
            }
            assert!(checked > 500, "{profile:?}: {checked}");
        }
    }

    #[test]
    fn values_round_to_the_step_and_out_of_range_is_refused() {
        let dbc = Profile::Tesla3Y.dbc();
        let speed = dbc.messages[&0x257].signals.iter().find(|s| s.name == "DI_vehicleSpeed").unwrap();
        let mut b = vec![0u8; 8];
        // 0.08 km/h steps, offset -40
        let v = encode_signal(&mut b, speed, 88.03).unwrap();
        assert!((v - 88.0).abs() < 1e-9, "{v}");
        assert_eq!(crate::signal_value(&b, speed), Some(v));
        assert!(matches!(encode_signal(&mut b, speed, 400.0), Err(EncodeError::OutOfRange(..))));
        assert!(matches!(encode_signal(&mut b, speed, -41.0), Err(EncodeError::OutOfRange(..))));
        assert!(matches!(encode_signal(&mut b, speed, f64::NAN), Err(EncodeError::NotFinite(_))));
        // signed: the pack current
        let amps = dbc.messages[&0x132].signals.iter().find(|s| s.name == "SmoothBattCurrent132").unwrap();
        assert_eq!(encode_signal(&mut b, amps, -123.4).unwrap(), crate::signal_value(&b, amps).unwrap());
        assert!((crate::signal_value(&b, amps).unwrap() + 123.4).abs() < 1e-9);
    }

    #[test]
    fn a_multiplexed_page_sets_its_multiplexer() {
        let msg = &Profile::Tesla3Y.dbc().messages[&0x219];
        let (bytes, back) =
            encode_message(msg, &[("VCSEC_TPMSPressure0", 2.9), ("VCSEC_TPMSPressure3", 3.025)]).unwrap();
        let got: Vec<(String, f64)> = decode_frame(msg, &bytes).into_iter().map(|(s, v)| (s.name.clone(), v)).collect();
        assert!(got.contains(&("VCSEC_TPMSDataIndex".to_string(), 5.0)), "{got:?}");
        assert!(got.contains(&("VCSEC_TPMSPressure0".to_string(), back[0])));
        assert!(got.contains(&("VCSEC_TPMSPressure3".to_string(), back[1])));
        assert!((back[0] - 2.9).abs() < 1e-9);
        // two pages in one frame
        let err = encode_message(msg, &[("VCSEC_TPMSPressure0", 2.9), ("VCSEC_TPMSRecommendedPressure0", 2.9)]);
        assert!(matches!(err, Err(EncodeError::MuxConflict(..))));
        assert!(matches!(encode_message(msg, &[("nothing", 1.0)]), Err(EncodeError::UnknownSignal(..))));
    }

    #[test]
    fn motorola_signals_come_back_too() {
        let mut n = 0;
        for msg in Profile::TeslaSxLegacy.dbc().messages.values() {
            for sig in msg.signals.iter().filter(|s| !s.little && s.length > 1 && s.length <= 32) {
                let size = msg.size as usize;
                if positions(sig).iter().any(|&b| (b >> 3) as usize >= size) {
                    continue;
                }
                let raw = if sig.signed { -1.0 } else { 2f64.powi(sig.length as i32) - 2.0 };
                let value = raw * sig.factor + sig.offset;
                let mut out = vec![0u8; size];
                let back = encode_signal(&mut out, sig, value).unwrap();
                assert_eq!(back.to_bits(), value.to_bits(), "{}", sig.series);
                n += 1;
            }
        }
        assert!(n > 10, "{n}");
    }
}
