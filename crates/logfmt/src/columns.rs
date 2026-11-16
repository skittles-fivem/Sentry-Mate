//! The `canbridge-columns-xz/2` body (after xz): the same lines as a candump text log, kept as columns.
//!
//! Spec: SMO `docs/sentry-manager-format.md`, "Columns format". Decoding gives back exactly the candump text a
//! `/1` log of the same frames holds, in the same order. Anything malformed is refused, and nothing is allocated
//! beyond a small multiple of the body's size, so a crafted file can't exhaust memory.

use crate::Error;

/// The 20 bytes a columns body starts with.
pub const MAGIC: &[u8; 20] = b"CANBRIDGE-COLUMNS/2\n";
const MAX_KEYS: u64 = 65_536;
const MAX_KEY_BYTES: u64 = 64;
const MAX_DATA: u8 = 64;
const MAX_NOTE: u64 = 65_536;
const MAX_TIME_US: i128 = 999_999_999_999_999_999;

/// Whether `body` (an unpacked log) is a columns log.
pub fn is_columns(body: &[u8]) -> bool {
    body.starts_with(MAGIC)
}

struct Reader<'a> {
    body: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn byte(&mut self) -> Result<u8, Error> {
        let b = *self.body.get(self.pos).ok_or(Error::Malformed("cut off"))?;
        self.pos += 1;
        Ok(b)
    }

    /// Unsigned LEB128 of up to ten bytes (as the reference decoder reads it, into a wide number).
    fn varint(&mut self) -> Result<u128, Error> {
        let mut v: u128 = 0;
        let mut shift = 0u32;
        loop {
            if shift > 63 {
                return Err(Error::Malformed("varint too long"));
            }
            let c = self.byte()?;
            v |= u128::from(c & 0x7f) << shift;
            if c & 0x80 == 0 {
                return Ok(v);
            }
            shift += 7;
        }
    }

    /// Varints used for counts and indexes: at most eight bytes.
    fn small(&mut self) -> Result<u64, Error> {
        let mut v: u64 = 0;
        for i in 0..8u32 {
            let c = self.byte()?;
            v += u64::from(c & 0x7f) << (7 * i);
            if c & 0x80 == 0 {
                return Ok(v);
            }
        }
        Err(Error::Malformed("number too large"))
    }

    fn count(&mut self, max: u64) -> Result<usize, Error> {
        let n = self.small()?;
        if n > max {
            return Err(Error::Malformed("count out of range"));
        }
        usize::try_from(n).map_err(|_| Error::Malformed("count out of range"))
    }

    fn bytes(&mut self, n: usize) -> Result<&'a [u8], Error> {
        if n > self.body.len() - self.pos {
            return Err(Error::Malformed("cut off"));
        }
        let b = &self.body[self.pos..self.pos + n];
        self.pos += n;
        Ok(b)
    }

    fn left(&self) -> usize {
        self.body.len() - self.pos
    }
}

/// Decodes a whole columns body and calls `on_line` with each line (without its LF), in the original order.
pub fn decode(body: &[u8], mut on_line: impl FnMut(&[u8])) -> Result<(), Error> {
    if !is_columns(body) {
        return Err(Error::Malformed("not a columns log"));
    }
    let mut r = Reader { body, pos: MAGIC.len() };

    let key_count = r.count(MAX_KEYS)?;
    let mut keys = Vec::with_capacity(key_count);
    for _ in 0..key_count {
        let n = r.count(MAX_KEY_BYTES)?;
        keys.push(r.bytes(n)?);
    }

    let note_count = r.count(body.len() as u64)?;
    let mut notes: Vec<(u64, &[u8])> = Vec::with_capacity(note_count.min(r.left()));
    for j in 0..note_count {
        let at = r.small()?;
        if j > 0 && at < notes[j - 1].0 {
            return Err(Error::Malformed("notes out of order"));
        }
        let n = r.count(MAX_NOTE)?;
        notes.push((at, r.bytes(n)?));
    }

    // each frame takes at least 3 bytes (a time, a key, a length)
    let frames = r.count((r.left() / 3) as u64)?;
    if let Some(&(last, _)) = notes.last() {
        if last > frames as u64 {
            return Err(Error::Malformed("note after the last frame"));
        }
    }

    let mut times = Vec::with_capacity(frames);
    let mut t: i128 = 0;
    for _ in 0..frames {
        let z = r.varint()?;
        let half = i128::try_from(z >> 1).map_err(|_| Error::Malformed("time out of range"))?;
        t += if z & 1 == 1 { -(half + 1) } else { half };
        if !(0..=MAX_TIME_US).contains(&t) {
            return Err(Error::Malformed("time out of range"));
        }
        times.push(t as u64);
    }

    let mut key_of = Vec::with_capacity(frames);
    for _ in 0..frames {
        let k = r.small()?;
        if k >= key_count as u64 {
            return Err(Error::Malformed("bad key index"));
        }
        key_of.push(k as usize);
    }

    // frames grouped by key, in their original order within each key
    let mut start = vec![0usize; key_count + 1];
    for &k in &key_of {
        start[k + 1] += 1;
    }
    for k in 0..key_count {
        start[k + 1] += start[k];
    }
    let mut fill = start[..key_count].to_vec();
    let mut order = vec![0usize; frames];
    for (f, &k) in key_of.iter().enumerate() {
        order[fill[k]] = f;
        fill[k] += 1;
    }

    let mut lens = vec![0u8; frames];
    for &f in &order {
        let n = r.byte()?;
        if n > MAX_DATA {
            return Err(Error::Malformed("data too long"));
        }
        lens[f] = n;
    }
    let mut at = Vec::with_capacity(frames);
    let mut total = 0usize;
    for &n in &lens {
        at.push(total);
        total += usize::from(n);
    }
    if total > r.left() {
        return Err(Error::Malformed("data cut off"));
    }
    let mut data = vec![0u8; total];
    for k in 0..key_count {
        let group = &order[start[k]..start[k + 1]];
        let longest = group.iter().map(|&f| lens[f]).max().unwrap_or(0);
        for j in 0..longest {
            for &f in group {
                if lens[f] > j {
                    data[at[f] + usize::from(j)] = r.byte()?;
                }
            }
        }
    }
    if r.left() != 0 {
        return Err(Error::Malformed("trailing bytes"));
    }

    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut line = Vec::with_capacity(160);
    let mut note = 0usize;
    for f in 0..=frames {
        while note < notes.len() && notes[note].0 == f as u64 {
            on_line(notes[note].1);
            note += 1;
        }
        if f == frames {
            break;
        }
        line.clear();
        let t = times[f];
        line.push(b'(');
        line.extend_from_slice((t / 1_000_000).to_string().as_bytes());
        line.extend_from_slice(format!(".{:06}) ", t % 1_000_000).as_bytes());
        line.extend_from_slice(keys[key_of[f]]);
        line.push(b'#');
        for &v in &data[at[f]..at[f] + usize::from(lens[f])] {
            line.push(HEX[usize::from(v >> 4)]);
            line.push(HEX[usize::from(v & 15)]);
        }
        on_line(&line);
    }
    Ok(())
}
