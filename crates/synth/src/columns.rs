//! The `canbridge-columns-xz/2` body (before xz), made from candump text lines the way the CAN Bridge makes it
//! (SMO `docs/sentry-manager-format.md`, "Columns format"). `logfmt::columns::decode` gives the same lines back.

fn varint(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

/// A line in the exact frame form, split as (time µs, key, data), else `None` (it is kept as a note).
fn frame_parts(l: &[u8]) -> Option<(u64, &[u8], Vec<u8>)> {
    let rest = l.strip_prefix(b"(")?;
    let close = rest.iter().position(|&c| c == b')')?;
    let (time, rest) = rest.split_at(close);
    let dot = time.iter().position(|&c| c == b'.')?;
    let (sec, usec) = (&time[..dot], &time[dot + 1..]);
    let digits = |s: &[u8]| !s.is_empty() && s.iter().all(u8::is_ascii_digit);
    if !digits(sec) || sec.len() > 12 || (sec.len() > 1 && sec[0] == b'0') || usec.len() != 6 || !digits(usec) {
        return None;
    }
    let rest = rest.strip_prefix(b") ")?;
    let hash = rest.iter().position(|&c| c == b'#')?;
    let (key, hexd) = (&rest[..hash], &rest[hash + 1..]);
    if key.is_empty() || key.len() > 64 || !key.iter().all(|c| (0x20..0x7f).contains(c) && *c != b'#') {
        return None;
    }
    if hexd.len() % 2 != 0 || hexd.len() > 128 || !hexd.iter().all(|c| c.is_ascii_digit() || (b'A'..=b'F').contains(c))
    {
        return None;
    }
    let data = hex::decode(hexd).ok()?;
    let t = std::str::from_utf8(sec).ok()?.parse::<u64>().ok()? * 1_000_000
        + std::str::from_utf8(usec).ok()?.parse::<u64>().ok()?;
    Some((t, key, data))
}

/// Encodes candump text lines (each without its LF) as a columns body.
pub fn encode<'a>(lines: impl IntoIterator<Item = &'a [u8]>) -> Vec<u8> {
    let mut keys: Vec<&[u8]> = Vec::new();
    let mut key_index: std::collections::HashMap<&[u8], usize> = std::collections::HashMap::new();
    let mut notes: Vec<(u64, &[u8])> = Vec::new();
    let mut times: Vec<u64> = Vec::new();
    let mut key_of: Vec<usize> = Vec::new();
    let mut data: Vec<Vec<u8>> = Vec::new();
    for l in lines {
        match frame_parts(l) {
            Some((t, key, d)) => {
                let k = *key_index.entry(key).or_insert_with(|| {
                    keys.push(key);
                    keys.len() - 1
                });
                times.push(t);
                key_of.push(k);
                data.push(d);
            }
            None => notes.push((times.len() as u64, l)),
        }
    }
    let mut out = Vec::with_capacity(16 + times.len() * 12);
    out.extend_from_slice(logfmt::columns::MAGIC);
    varint(&mut out, keys.len() as u64);
    for k in &keys {
        varint(&mut out, k.len() as u64);
        out.extend_from_slice(k);
    }
    varint(&mut out, notes.len() as u64);
    for (at, n) in &notes {
        varint(&mut out, *at);
        varint(&mut out, n.len() as u64);
        out.extend_from_slice(n);
    }
    varint(&mut out, times.len() as u64);
    let mut prev: i64 = 0;
    for &t in &times {
        let d = t as i64 - prev;
        prev = t as i64;
        varint(&mut out, ((d << 1) ^ (d >> 63)) as u64);
    }
    for &k in &key_of {
        varint(&mut out, k as u64);
    }
    // frames grouped by key, in order within each key
    let mut groups: Vec<Vec<usize>> = vec![Vec::new(); keys.len()];
    for (f, &k) in key_of.iter().enumerate() {
        groups[k].push(f);
    }
    for g in &groups {
        for &f in g {
            out.push(data[f].len() as u8);
        }
    }
    for g in &groups {
        let longest = g.iter().map(|&f| data[f].len()).max().unwrap_or(0);
        for j in 0..longest {
            for &f in g {
                if let Some(&b) = data[f].get(j) {
                    out.push(b);
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
    use super::*;

    #[test]
    fn decodes_back_to_the_same_lines() {
        let text = "# canbridge v1 app=0.2.150\n# clock wall=1772409600.000000 mono_ns=5000\n\
                    (1772409600.001000) can0 118#0102030405060708\n(1772409600.000500) can1 3F3#AABBCC\n\
                    (1772409600.002000) can0 118#01\n(01772409600.002000) can0 118#01\n(1772409600.003000) can3 04F#\n\
                    # dropped 3 chunks of frames: logger fell behind\n(1772409600.004000) can0 12345678#00FF\n\
                    # clock wall=1772409660.000000 mono_ns=60000005000 end";
        let lines: Vec<&[u8]> = text.lines().map(str::as_bytes).collect();
        let body = encode(lines.iter().copied());
        let mut back: Vec<Vec<u8>> = Vec::new();
        logfmt::columns::decode(&body, |l| back.push(l.to_vec())).unwrap();
        let back: Vec<&[u8]> = back.iter().map(|l| l.as_slice()).collect();
        assert_eq!(back, lines);
    }
}
