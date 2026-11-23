//! The `canbridge-columns-xz/2` body (after xz): the same lines as a candump text log, kept as columns.
//!
//! Spec: SMO `docs/sentry-manager-format.md`, "Columns format". Decoding gives back exactly the candump text a
//! `/1` log of the same frames holds, in the same order. Anything malformed is refused. Decoding holds the body and
//! 9 bytes per frame besides the frames' data (the times and keys are read again as the lines go out), and a body
//! holds at most [`MAX_FRAMES`] frames (the caller can set fewer), so a crafted file can't exhaust memory: the body's
//! own size is bounded by the caller (`crate::Limits`, also by how much it may unpack per packed byte).

use crate::Error;

/// The 20 bytes a columns body starts with.
pub const MAGIC: &[u8; 20] = b"CANBRIDGE-COLUMNS/2\n";
const MAX_KEYS: u64 = 65_536;
const MAX_KEY_BYTES: u64 = 64;
const MAX_DATA: u8 = 64;
const MAX_NOTE: u64 = 65_536;
const MAX_TIME_US: i128 = 999_999_999_999_999_999;
/// Frames one body may hold. The bridge closes a log at 48 MB of body, and a real frame takes 10 bytes or more
/// (a time, a key, a length and its data), so a real log holds under 5 million.
pub const MAX_FRAMES: u64 = 8_000_000;

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
        self.count_of(n, max)
    }

    fn count_of(&self, n: u64, max: u64) -> Result<usize, Error> {
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
    decode_until(body, MAX_FRAMES, |l| {
        on_line(l);
        true
    })
    .map(|_| ())
}

/// One time of the times column: the running time `t` moved by the zigzag delta read, checked.
fn next_time(r: &mut Reader<'_>, t: &mut i128) -> Result<u64, Error> {
    let z = r.varint()?;
    let half = i128::try_from(z >> 1).map_err(|_| Error::Malformed("time out of range"))?;
    *t += if z & 1 == 1 { -(half + 1) } else { half };
    if !(0..=MAX_TIME_US).contains(t) {
        return Err(Error::Malformed("time out of range"));
    }
    Ok(*t as u64)
}

fn next_key(r: &mut Reader<'_>, key_count: usize) -> Result<usize, Error> {
    let k = r.small()?;
    if k >= key_count as u64 {
        return Err(Error::Malformed("bad key index"));
    }
    Ok(k as usize)
}

/// [`decode`] with at most `max_frames` frames (more is refused before anything is allocated for them), stopping
/// early when `on_line` returns false. `Ok(false)`: stopped early.
pub fn decode_until(body: &[u8], max_frames: u64, mut on_line: impl FnMut(&[u8]) -> bool) -> Result<bool, Error> {
    let mut line = Vec::with_capacity(160);
    parts_until(body, max_frames, |p| match p {
        Part::Note(l) => on_line(l),
        Part::Frame { time_us, text, data, .. } => {
            frame_line(&mut line, time_us, text, data);
            on_line(&line)
        }
    })
}

/// One line of a columns body, before it is put back into text.
pub(crate) enum Part<'a> {
    /// A line that is not a frame, as it was written.
    Note(&'a [u8]),
    /// A frame: its time, its key (`can0 123`: the interface and the id, as the line has them; `key` is its place
    /// among the body's keys) and its data.
    Frame { time_us: u64, key: usize, text: &'a [u8], data: &'a [u8] },
}

/// The candump line of a frame: `(seconds.microseconds) key#DATA`.
pub(crate) fn frame_line(line: &mut Vec<u8>, time_us: u64, key: &[u8], data: &[u8]) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    line.clear();
    line.push(b'(');
    line.extend_from_slice((time_us / 1_000_000).to_string().as_bytes());
    line.extend_from_slice(format!(".{:06}) ", time_us % 1_000_000).as_bytes());
    line.extend_from_slice(key);
    line.push(b'#');
    for &v in data {
        line.push(HEX[usize::from(v >> 4)]);
        line.push(HEX[usize::from(v & 15)]);
    }
}

/// How long [`frame_line`]'s line is.
pub(crate) fn frame_line_len(time_us: u64, key: &[u8], data: &[u8]) -> usize {
    let mut secs = time_us / 1_000_000;
    let mut digits = 1;
    while secs >= 10 {
        secs /= 10;
        digits += 1;
    }
    1 + digits + 9 + key.len() + 1 + 2 * data.len()
}

/// [`decode_until`] before the frames are put back into text: every line in order, a frame as its parts.
pub(crate) fn parts_until(body: &[u8], max_frames: u64, mut on: impl FnMut(Part<'_>) -> bool) -> Result<bool, Error> {
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
    let frames = r.small()?;
    if frames > max_frames {
        return Err(Error::Malformed("too many frames"));
    }
    let frames = r.count_of(frames, (r.left() / 3) as u64)?;
    if let Some(&(last, _)) = notes.last()
        && last > frames as u64
    {
        return Err(Error::Malformed("note after the last frame"));
    }

    // the times are checked now and read again as the lines go out
    let times_at = r.pos;
    let mut t: i128 = 0;
    for _ in 0..frames {
        next_time(&mut r, &mut t)?;
    }

    // the keys too: counted per key now, read again for the order and for the lines
    let keys_at = r.pos;
    let mut start = vec![0usize; key_count + 1];
    for _ in 0..frames {
        start[next_key(&mut r, key_count)? + 1] += 1;
    }
    for k in 0..key_count {
        start[k + 1] += start[k];
    }

    // frames grouped by key, in their original order within each key
    let mut fill = start[..key_count].to_vec();
    let mut order = vec![0u32; frames];
    let mut kr = Reader { body, pos: keys_at };
    for f in 0..frames {
        let k = next_key(&mut kr, key_count)?;
        order[fill[k]] = u32::try_from(f).map_err(|_| Error::Malformed("too many frames"))?;
        fill[k] += 1;
    }
    drop(fill);

    let mut lens = vec![0u8; frames];
    for &f in &order {
        let n = r.byte()?;
        if n > MAX_DATA {
            return Err(Error::Malformed("data too long"));
        }
        lens[f as usize] = n;
    }
    let mut at = Vec::with_capacity(frames);
    let mut total = 0usize;
    for &n in &lens {
        at.push(u32::try_from(total).map_err(|_| Error::Malformed("data too long"))?);
        total += usize::from(n);
    }
    if total > r.left() {
        return Err(Error::Malformed("data cut off"));
    }
    let mut data = vec![0u8; total];
    for k in 0..key_count {
        let group = &order[start[k]..start[k + 1]];
        let longest = group.iter().map(|&f| lens[f as usize]).max().unwrap_or(0);
        for j in 0..longest {
            for &f in group {
                if lens[f as usize] > j {
                    data[at[f as usize] as usize + usize::from(j)] = r.byte()?;
                }
            }
        }
    }
    if r.left() != 0 {
        return Err(Error::Malformed("trailing bytes"));
    }
    drop(order);

    let mut note = 0usize;
    let (mut tr, mut kr) = (Reader { body, pos: times_at }, Reader { body, pos: keys_at });
    let mut t: i128 = 0;
    for f in 0..=frames {
        while note < notes.len() && notes[note].0 == f as u64 {
            if !on(Part::Note(notes[note].1)) {
                return Ok(false);
            }
            note += 1;
        }
        if f == frames {
            break;
        }
        let time_us = next_time(&mut tr, &mut t)?;
        let k = next_key(&mut kr, key_count)?;
        let a = at[f] as usize;
        if !on(Part::Frame { time_us, key: k, text: keys[k], data: &data[a..a + usize::from(lens[f])] }) {
            return Ok(false);
        }
    }
    Ok(true)
}
