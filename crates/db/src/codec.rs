//! Compression of what the db stores, before sealing (sealed bytes don't compress). Pure Rust: brotli, picked by
//! measuring (see the crate docs, "Compression"). A compressed body starts with a byte naming its codec, so other
//! codecs can be added later without a new format version.

use crate::error::{Error, Result};
use std::io::Read;

pub(crate) const NONE: u8 = 0;
pub(crate) const BROTLI: u8 = 1;

/// How hard to compress.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Level {
    /// brotli quality, 0 to 11.
    pub quality: u32,
    /// log2 of brotli's window, 10 to 24.
    pub lgwin: u32,
}

impl Level {
    /// Journal entries: a window written every minute, so fast (measured 60-210 MB/s, within 5-10% of q9).
    pub const HOT: Level = Level { quality: 4, lgwin: 22 };
    /// Snapshots folded while writing (tables, the fleet, days still being written).
    pub const WARM: Level = Level { quality: 6, lgwin: 22 };
    /// Finished days, packed by `Db::settle`: about 10% smaller than q9 at a fraction of a second per day (q11
    /// gains another 1% at 2-4 times the time). A day's columns (at most ~3 MB) fit the 4 MiB window whole.
    pub const COLD: Level = Level { quality: 10, lgwin: 22 };
    /// Finished days in their smallest form (`Db::compact_series`): the last 2% (measured on made-up days of every
    /// input, docs/plans/storage.md) at about twice the time of q10, paid once per day in the background.
    pub const SMALL: Level = Level { quality: 11, lgwin: 22 };
}

/// Nothing a db file holds unpacks past this: a guard against a body that claims to be huge.
pub(crate) const MAX_PLAIN: usize = 1 << 30;

/// `codec byte | compressed bytes`. Data too small to gain is stored as it is.
pub(crate) fn compress(data: &[u8], level: Level) -> Result<Vec<u8>> {
    if data.len() < 64 {
        let mut out = Vec::with_capacity(data.len() + 1);
        out.push(NONE);
        out.extend_from_slice(data);
        return Ok(out);
    }
    let params = brotli::enc::BrotliEncoderParams {
        quality: level.quality.min(11) as i32,
        lgwin: level.lgwin.clamp(10, 24) as i32,
        size_hint: data.len(),
        ..Default::default()
    };
    let mut out = vec![BROTLI];
    brotli::BrotliCompress(&mut &data[..], &mut out, &params)?;
    Ok(out)
}

pub(crate) fn decompress(b: &[u8]) -> Result<Vec<u8>> {
    let (&codec, body) = b.split_first().ok_or_else(|| Error::Corrupt("empty body".into()))?;
    match codec {
        NONE => Ok(body.to_vec()),
        BROTLI => {
            let mut out = Vec::with_capacity(body.len() * 4);
            brotli::Decompressor::new(body, 1 << 16)
                .take(MAX_PLAIN as u64 + 1)
                .read_to_end(&mut out)
                .map_err(|e| Error::Corrupt(format!("brotli: {e}")))?;
            if out.len() > MAX_PLAIN {
                return Err(Error::Corrupt("unpacks too large".into()));
            }
            Ok(out)
        }
        c => Err(Error::Corrupt(format!("unknown codec {c}"))),
    }
}

/// What unpacks from the start of a body that may be cut short or damaged: the bytes it gives before it stops, and
/// whether it ended as a whole body does (`false`: the output is a prefix of what was packed, how long a one is not
/// known).
pub(crate) fn decompress_prefix(b: &[u8]) -> (Vec<u8>, bool) {
    let Some((&codec, body)) = b.split_first() else { return (Vec::new(), false) };
    match codec {
        NONE => (body.to_vec(), true),
        BROTLI => {
            let mut out = Vec::with_capacity(body.len() * 4);
            let mut d = brotli::Decompressor::new(body, 1 << 16);
            let mut buf = vec![0u8; 1 << 16];
            let whole = loop {
                match d.read(&mut buf) {
                    Ok(0) => break true,
                    Ok(n) => {
                        out.extend_from_slice(&buf[..n]);
                        if out.len() > MAX_PLAIN {
                            break false;
                        }
                    }
                    Err(_) => break false,
                }
            };
            (out, whole)
        }
        _ => (Vec::new(), false),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn round_trips_and_refuses_damage() {
        for data in [vec![], b"short".to_vec(), (0..100_000u32).flat_map(|i| (i % 300).to_le_bytes()).collect()] {
            for level in [Level::HOT, Level::WARM, Level::COLD] {
                let c = compress(&data, level).unwrap();
                assert_eq!(decompress(&c).unwrap(), data);
            }
        }
        assert!(decompress(&[]).is_err());
        assert!(decompress(&[9, 1, 2]).is_err());
        let c = compress(&vec![7u8; 10_000], Level::HOT).unwrap();
        assert!(decompress(&c[..c.len() / 2]).is_err());
    }
}
