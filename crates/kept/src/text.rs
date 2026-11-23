//! A block of text turned into streams that compress well, and back, byte for byte.
//!
//! The text is cut into lines. Runs of lines with the same number of fields (the rows of a `COPY` block, separated
//! by tabs; the rows of a CSV file, separated by commas outside quotes) are turned on their side: one stream per
//! column. Everything else (SQL statements, comments, headers, a last line without its newline) is kept as it is.
//!
//! A column is stored in the first way that gives back every field's exact text:
//!
//! - **numbers**: whole numbers and decimals as they are canonically written (`-12`, `0.50`, `12000.853759999999`),
//!   and timestamps (`2024-03-01 08:01:00.123456`, with any fixed text after them). Each is a whole number of the
//!   column's smallest step (a timestamp: microseconds), stored as the difference to the one before or as itself,
//!   whichever codes shorter, with the number of fraction digits it was written with. A field that is not written
//!   that way (`\N`, `007`, `-0`, `1e5`, `+1`, `1.`, `.5`, a space) is an exception: its text is kept;
//! - **kilometres worked out from miles**: a column of floats with many digits (`19754.844159739394`) in which
//!   nearly every value is a number of miles with six decimals times 1.609344, written as the shortest text that
//!   reads back the same float: the millionths of a mile are stored, as numbers are. Each value is worked out
//!   again and compared when it is stored; one that doesn't come back the same is an exception;
//! - **a dictionary**: a column of at most 256 different texts is their list and a byte per row;
//! - **text**: the fields as they are, a newline after each.
//!
//! Nothing is rounded, sorted or reordered: the order of the rows is part of the file. The encoder decodes every
//! block it made and compares it with the text before it is used ([`crate::Codec`]).

use std::collections::HashMap;

/// A stream that doesn't read as one this crate wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bad(pub &'static str);

/// Rows fewer than this stay as text (a column's bookkeeping would cost more than it saves).
const MIN_RUN: usize = 4;
const RAW: u8 = 0;
const DICT: u8 = 1;
const NUM: u8 = 2;
const STYLE_NUMBER: u8 = 0;
const STYLE_TIME: u8 = 1;
const STYLE_KM: u8 = 2;
/// Kilometres in a mile; a column of kilometres worked out from miles with six decimals is stored as the miles.
const KM_PER_MILE: f64 = 1.609_344;
const MODE_PLAIN: u8 = 0;
const MODE_DELTA: u8 = 1;
/// The most fraction digits and digits in all of a number stored as one.
const MAX_DIGITS: usize = 18;
/// The longest text after a timestamp (a zone such as `+00` or `+01:00`).
const MAX_SUFFIX: usize = 16;
const POW10: [i128; 19] = {
    let mut p = [1i128; 19];
    let mut i = 1;
    while i < 19 {
        p[i] = p[i - 1] * 10;
        i += 1;
    }
    p
};

fn put_varint(out: &mut Vec<u8>, mut v: u128) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn zigzag(v: i128) -> u128 {
    ((v << 1) ^ (v >> 127)) as u128
}

fn unzigzag(v: u128) -> i128 {
    ((v >> 1) as i128) ^ -((v & 1) as i128)
}

fn put_bytes(out: &mut Vec<u8>, b: &[u8]) {
    put_varint(out, b.len() as u128);
    out.extend_from_slice(b);
}

struct Cursor<'a> {
    b: &'a [u8],
    at: usize,
}

impl<'a> Cursor<'a> {
    fn byte(&mut self) -> Result<u8, Bad> {
        let v = *self.b.get(self.at).ok_or(Bad("cut short"))?;
        self.at += 1;
        Ok(v)
    }

    fn varint(&mut self) -> Result<u128, Bad> {
        let (mut v, mut shift) = (0u128, 0u32);
        loop {
            let b = self.byte()?;
            if shift > 126 {
                return Err(Bad("a number too long"));
            }
            v |= u128::from(b & 0x7f) << shift;
            if b & 0x80 == 0 {
                return Ok(v);
            }
            shift += 7;
        }
    }

    fn len(&mut self) -> Result<usize, Bad> {
        let v = self.varint()?;
        usize::try_from(v).ok().filter(|n| *n <= self.b.len()).ok_or(Bad("a length past the end"))
    }

    fn bytes(&mut self) -> Result<&'a [u8], Bad> {
        let n = self.len()?;
        let s = self.b.get(self.at..self.at + n).ok_or(Bad("cut short"))?;
        self.at += n;
        Ok(s)
    }
}

/// The separator of a line's fields and how many it has (`None`: one field, kept as text).
fn shape(line: &[u8]) -> Option<(u8, u32)> {
    let tabs = line.iter().filter(|b| **b == b'\t').count();
    if tabs > 0 {
        return Some((b'\t', tabs as u32 + 1));
    }
    let (mut commas, mut quoted) = (0u32, false);
    for &b in line {
        match b {
            b'"' => quoted = !quoted,
            b',' if !quoted => commas += 1,
            _ => {}
        }
    }
    (commas > 0).then_some((b',', commas + 1))
}

/// The fields of a line, cut where [`shape`] counted.
fn fields<'a>(line: &'a [u8], sep: u8, out: &mut Vec<&'a [u8]>) {
    let (mut from, mut quoted) = (0, false);
    for (i, &b) in line.iter().enumerate() {
        if b == b'"' && sep == b',' {
            quoted = !quoted;
        } else if b == sep && !quoted {
            out.push(&line[from..i]);
            from = i + 1;
        }
    }
    out.push(&line[from..]);
}

/// A number as canonically written: its digits as a whole number and how many of them follow the point.
fn number(t: &[u8]) -> Option<(i64, u8)> {
    let (neg, digits) = match t.split_first() {
        Some((b'-', rest)) => (true, rest),
        _ => (false, t),
    };
    let (whole, frac) = match digits.iter().position(|b| *b == b'.') {
        Some(p) => (&digits[..p], &digits[p + 1..]),
        None => (digits, &digits[..0]),
    };
    if whole.is_empty()
        || (whole.len() > 1 && whole[0] == b'0')
        || whole.len() + frac.len() > MAX_DIGITS
        || (frac.is_empty() && whole.len() != digits.len())
        || !whole.iter().chain(frac).all(u8::is_ascii_digit)
    {
        return None;
    }
    let mut v = 0i64;
    for b in whole.iter().chain(frac) {
        v = v * 10 + i64::from(b - b'0');
    }
    if neg && v == 0 {
        return None;
    }
    Some((if neg { -v } else { v }, frac.len() as u8))
}

fn write_number(out: &mut Vec<u8>, v: i64, nd: u8) {
    if v < 0 {
        out.push(b'-');
    }
    let digits = v.unsigned_abs().to_string();
    let nd = usize::from(nd);
    let pad = (nd + 1).saturating_sub(digits.len());
    let all = pad + digits.len();
    for i in 0..all {
        if i == all - nd && nd > 0 {
            out.push(b'.');
        }
        out.push(if i < pad { b'0' } else { digits.as_bytes()[i - pad] });
    }
}

/// Kilometres as text from millionths of a mile.
fn write_km(out: &mut Vec<u8>, micro_miles: i64) {
    out.extend_from_slice(format!("{}", micro_miles as f64 / 1e6 * KM_PER_MILE).as_bytes());
}

/// The millionths of a mile whose kilometres are written exactly as `t`.
fn km(t: &[u8], scratch: &mut Vec<u8>) -> Option<i64> {
    let x: f64 = std::str::from_utf8(t).ok()?.parse().ok()?;
    let near = x / KM_PER_MILE * 1e6;
    if !near.is_finite() || near.abs() > 9e15 {
        return None;
    }
    let near = near.round() as i64;
    [near, near - 1, near + 1].into_iter().find(|m| {
        scratch.clear();
        write_km(scratch, *m);
        scratch.as_slice() == t
    })
}

fn two(t: &[u8]) -> Option<i64> {
    match t {
        [a, b] if a.is_ascii_digit() && b.is_ascii_digit() => Some(i64::from(a - b'0') * 10 + i64::from(b - b'0')),
        _ => None,
    }
}

fn days_in_month(y: i64, m: i64) -> i64 {
    match m {
        4 | 6 | 9 | 11 => 30,
        2 if y % 4 == 0 && (y % 100 != 0 || y % 400 == 0) => 29,
        2 => 28,
        _ => 31,
    }
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// A timestamp `YYYY-MM-DD?HH:MM:SS[.f...]suffix`: its microseconds, the fraction digits written, the character
/// between date and time, and the text after it.
fn time(t: &[u8]) -> Option<(i64, u8, u8, &[u8])> {
    if t.len() < 19 || t[4] != b'-' || t[7] != b'-' || t[13] != b':' || t[16] != b':' || !matches!(t[10], b' ' | b'T') {
        return None;
    }
    let y = two(&t[0..2])? * 100 + two(&t[2..4])?;
    let (m, d, h, mi, s) = (two(&t[5..7])?, two(&t[8..10])?, two(&t[11..13])?, two(&t[14..16])?, two(&t[17..19])?);
    if !(1..=12).contains(&m) || d < 1 || d > days_in_month(y, m) || h > 23 || mi > 59 || s > 59 {
        return None;
    }
    let mut us = (days_from_civil(y, m, d) * 86_400 + h * 3600 + mi * 60 + s) * 1_000_000;
    let mut rest = &t[19..];
    let mut nd = 0u8;
    if let Some((b'.', after)) = rest.split_first() {
        let n = after.iter().take_while(|b| b.is_ascii_digit()).count();
        if n == 0 || n > 6 {
            return None;
        }
        let mut f = 0i64;
        for b in &after[..n] {
            f = f * 10 + i64::from(b - b'0');
        }
        us += f * POW10[6 - n] as i64;
        nd = n as u8;
        rest = &after[n..];
    }
    if rest.len() > MAX_SUFFIX || rest.first().is_some_and(|b| b.is_ascii_digit() || *b == b'.') {
        return None;
    }
    Some((us, nd, t[10], rest))
}

fn write_time(out: &mut Vec<u8>, us: i64, nd: u8, sep: u8, suffix: &[u8]) {
    let secs = us.div_euclid(1_000_000);
    let frac = us.rem_euclid(1_000_000);
    let (y, m, d) = civil_from_days(secs.div_euclid(86_400));
    let rem = secs.rem_euclid(86_400);
    out.extend_from_slice(format!("{y:04}-{m:02}-{d:02}").as_bytes());
    out.push(sep);
    out.extend_from_slice(format!("{:02}:{:02}:{:02}", rem / 3600, rem % 3600 / 60, rem % 60).as_bytes());
    if nd > 0 {
        let n = usize::from(nd.min(6));
        out.extend_from_slice(format!(".{:0n$}", frac / POW10[6 - n] as i64).as_bytes());
    }
    out.extend_from_slice(suffix);
}

/// About how many bits the bytes take once coded by how often each occurs.
fn cost(b: &[u8]) -> f64 {
    let mut n = [0u32; 256];
    for &x in b {
        n[usize::from(x)] += 1;
    }
    let len = b.len() as f64;
    n.iter().filter(|c| **c > 0).map(|&c| -f64::from(c) * (f64::from(c) / len).log2()).sum()
}

/// The typed values of a column: each in the column's smallest step, with the fraction digits it was written with.
struct Typed {
    style: u8,
    sep: u8,
    suffix: Vec<u8>,
    k: u8,
    /// `None`: an exception.
    vals: Vec<Option<(i64, u8)>>,
}

fn typed(col: &[&[u8]]) -> Option<Typed> {
    let numbers = col.iter().filter(|t| number(t).is_some()).count();
    let first_time = col.iter().find_map(|t| time(t));
    let (style, sep, suffix) = match first_time {
        Some((_, _, sep, suffix)) if numbers == 0 => (STYLE_TIME, sep, suffix.to_vec()),
        _ if numbers > 0 => (STYLE_NUMBER, 0, Vec::new()),
        _ => return None,
    };
    let mut vals: Vec<Option<(i64, u8)>> = col
        .iter()
        .map(|t| match style {
            STYLE_TIME => time(t).filter(|x| x.2 == sep && x.3 == suffix.as_slice()).map(|x| (x.0, x.1)),
            _ => number(t),
        })
        .collect();
    let mut k = if style == STYLE_TIME { 6 } else { vals.iter().flatten().map(|v| v.1).max().unwrap_or(0) };
    // floats with many digits: kilometres from miles, when nearly all of them are
    let mut style = style;
    if style == STYLE_NUMBER && k > 6 {
        let mut scratch = Vec::new();
        let sample = col.iter().take(64).filter(|t| km(t, &mut scratch).is_some()).count();
        if sample * 10 >= col.len().min(64) * 9 {
            let miles: Vec<Option<(i64, u8)>> = col.iter().map(|t| km(t, &mut scratch).map(|m| (m, 0))).collect();
            if miles.iter().flatten().count() * 10 >= numbers * 9 {
                (vals, style, k) = (miles, STYLE_KM, 0);
            }
        }
    }
    // each in the column's smallest step; one that doesn't fit is an exception
    for v in &mut vals {
        if let Some((x, nd)) = *v {
            *v = match style {
                STYLE_TIME | STYLE_KM => Some((x, nd)),
                _ => i64::try_from(i128::from(x) * POW10[usize::from(k - nd)]).ok().map(|s| (s, nd)),
            };
        }
    }
    // worth it when the exceptions are few, or few different ones (`\N` in a column of numbers)
    let exceptions: Vec<&[u8]> = col.iter().zip(&vals).filter(|x| x.1.is_none()).map(|x| *x.0).collect();
    if exceptions.len() == col.len() {
        return None;
    }
    if exceptions.len() * 10 > col.len() {
        let mut seen: Vec<&[u8]> = Vec::new();
        for e in &exceptions {
            if !seen.contains(e) {
                if seen.len() == 8 {
                    return None;
                }
                seen.push(e);
            }
        }
    }
    Some(Typed { style, sep, suffix, k, vals })
}

fn encode_typed(col: &[&[u8]], t: &Typed, out: &mut Vec<u8>) {
    out.extend_from_slice(&[NUM, t.style, t.sep, t.k]);
    put_bytes(out, &t.suffix);
    // which rows are typed: runs, typed first
    let mut runs: Vec<u128> = Vec::new();
    let (mut now, mut n) = (true, 0u128);
    for v in &t.vals {
        if v.is_some() != now {
            runs.push(n);
            (now, n) = (!now, 0);
        }
        n += 1;
    }
    runs.push(n);
    put_varint(out, runs.len() as u128);
    for r in runs {
        put_varint(out, r);
    }
    let vals: Vec<(i64, u8)> = t.vals.iter().flatten().copied().collect();
    // the fraction digits: one for all, or one each
    match vals.split_first() {
        Some((first, rest)) if rest.iter().all(|v| v.1 == first.1) => out.extend_from_slice(&[0, first.1]),
        _ => {
            out.push(1);
            out.extend(vals.iter().map(|v| v.1));
        }
    }
    let k = usize::from(t.k);
    let (mut plain, mut delta) = (Vec::with_capacity(vals.len() * 2), Vec::with_capacity(vals.len() * 2));
    let mut prev = (0i64, t.k);
    for &(v, nd) in &vals {
        put_varint(&mut plain, zigzag(i128::from(v) / POW10[k - usize::from(nd)]));
        // both are whole numbers of the coarser step of the two
        let step = POW10[k - usize::from(nd.max(prev.1))];
        put_varint(&mut delta, zigzag((i128::from(v) - i128::from(prev.0)) / step));
        prev = (v, nd);
    }
    if cost(&delta) < cost(&plain) {
        out.push(MODE_DELTA);
        put_bytes(out, &delta);
    } else {
        out.push(MODE_PLAIN);
        put_bytes(out, &plain);
    }
    let mut exceptions = Vec::new();
    for (text, _) in col.iter().zip(&t.vals).filter(|x| x.1.is_none()) {
        exceptions.extend_from_slice(text);
        exceptions.push(b'\n');
    }
    put_bytes(out, &exceptions);
}

fn encode_column(col: &[&[u8]], out: &mut Vec<u8>) {
    if let Some(t) = typed(col) {
        return encode_typed(col, &t, out);
    }
    let mut dict: HashMap<&[u8], u8> = HashMap::new();
    let mut order: Vec<&[u8]> = Vec::new();
    let mut index = Vec::with_capacity(col.len());
    for t in col {
        let next = order.len();
        match dict.get(t) {
            Some(i) => index.push(*i),
            None if next < 256 => {
                dict.insert(t, next as u8);
                order.push(t);
                index.push(next as u8);
            }
            None => break,
        }
    }
    if index.len() == col.len() && order.len() * 2 <= col.len() {
        out.push(DICT);
        put_varint(out, order.len() as u128);
        for t in order {
            put_bytes(out, t);
        }
        out.extend_from_slice(&index);
        return;
    }
    out.push(RAW);
    let mut text = Vec::with_capacity(col.iter().map(|t| t.len() + 1).sum());
    for t in col {
        text.extend_from_slice(t);
        text.push(b'\n');
    }
    put_bytes(out, &text);
}

/// The text as streams (not yet compressed).
pub fn encode(text: &[u8]) -> Vec<u8> {
    // each line: where it starts and ends (without its newline) and its shape when it could be a row
    struct Line {
        from: usize,
        to: usize,
        cr: bool,
        shape: Option<(u8, u32)>,
    }
    let mut lines: Vec<Line> = Vec::new();
    let mut from = 0;
    for piece in text.split_inclusive(|b| *b == b'\n') {
        let end = from + piece.len();
        if piece.last() == Some(&b'\n') {
            let mut to = end - 1;
            let cr = to > from && text[to - 1] == b'\r';
            if cr {
                to -= 1;
            }
            lines.push(Line { from, to, cr, shape: shape(&text[from..to]) });
        }
        from = end;
    }
    let mut out = Vec::with_capacity(text.len() / 2 + 64);
    let mut verbatim_from = 0;
    let mut cells: Vec<&[u8]> = Vec::new();
    let mut col: Vec<&[u8]> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let key = (lines[i].shape, lines[i].cr);
        let mut j = i + 1;
        while j < lines.len() && (lines[j].shape, lines[j].cr) == key {
            j += 1;
        }
        let (Some((sep, n)), true) = (key.0, j - i >= MIN_RUN) else {
            i = j;
            continue;
        };
        let (rows, n) = (j - i, n as usize);
        put_bytes(&mut out, &text[verbatim_from..lines[i].from]);
        put_varint(&mut out, rows as u128);
        out.extend_from_slice(&[sep, u8::from(key.1)]);
        put_varint(&mut out, n as u128);
        cells.clear();
        for l in &lines[i..j] {
            fields(&text[l.from..l.to], sep, &mut cells);
        }
        for c in 0..n {
            col.clear();
            col.extend((0..rows).map(|r| cells[r * n + c]));
            encode_column(&col, &mut out);
        }
        verbatim_from = lines[j - 1].to + usize::from(key.1) + 1;
        i = j;
    }
    put_bytes(&mut out, &text[verbatim_from..]);
    put_varint(&mut out, 0);
    out
}

/// One column's fields, one after the other, and where each ends.
#[derive(Default)]
struct Column {
    text: Vec<u8>,
    ends: Vec<usize>,
}

fn decode_column(c: &mut Cursor<'_>, rows: usize, out: &mut Column) -> Result<(), Bad> {
    out.text.clear();
    out.ends.clear();
    match c.byte()? {
        RAW => {
            let text = c.bytes()?;
            for t in text.split_inclusive(|b| *b == b'\n') {
                let t = t.strip_suffix(b"\n").ok_or(Bad("a field without its end"))?;
                out.text.extend_from_slice(t);
                out.ends.push(out.text.len());
            }
        }
        DICT => {
            let n = c.len()?;
            let mut dict = Vec::with_capacity(n.min(256));
            for _ in 0..n {
                dict.push(c.bytes()?);
            }
            for _ in 0..rows {
                let t = dict.get(usize::from(c.byte()?)).ok_or(Bad("not in the dictionary"))?;
                out.text.extend_from_slice(t);
                out.ends.push(out.text.len());
            }
        }
        NUM => {
            let (style, sep, k) = (c.byte()?, c.byte()?, usize::from(c.byte()?));
            let suffix = c.bytes()?;
            if k > MAX_DIGITS || style > STYLE_KM {
                return Err(Bad("not a number column"));
            }
            let mut runs = Vec::new();
            for _ in 0..c.len()? {
                runs.push(c.len()?);
            }
            let typed: usize = runs.iter().step_by(2).sum();
            let digits_each = c.byte()? == 1;
            let digits = if digits_each {
                let d = c.b.get(c.at..c.at + typed).ok_or(Bad("cut short"))?;
                c.at += typed;
                d
            } else {
                let one = c.b.get(c.at..c.at + 1).ok_or(Bad("cut short"))?;
                c.at += 1;
                one
            };
            let mode = c.byte()?;
            let mut vals = Cursor { b: c.bytes()?, at: 0 };
            let mut exceptions = c.bytes()?.split_inclusive(|b| *b == b'\n');
            let (mut prev, mut seen) = ((0i128, k), 0usize);
            for (r, &n) in runs.iter().enumerate() {
                for _ in 0..n {
                    if r % 2 == 1 {
                        let t =
                            exceptions.next().and_then(|t| t.strip_suffix(b"\n")).ok_or(Bad("an exception short"))?;
                        out.text.extend_from_slice(t);
                    } else {
                        let nd = usize::from(digits[if digits_each { seen } else { 0 }]);
                        if nd > k {
                            return Err(Bad("more digits than the column has"));
                        }
                        seen += 1;
                        let raw = unzigzag(vals.varint()?);
                        let v = match mode {
                            MODE_PLAIN => raw.checked_mul(POW10[k - nd]),
                            MODE_DELTA => {
                                raw.checked_mul(POW10[k - nd.max(prev.1)]).and_then(|d| d.checked_add(prev.0))
                            }
                            _ => None,
                        };
                        let v = v.filter(|v| i64::try_from(*v).is_ok()).ok_or(Bad("a number out of range"))?;
                        prev = (v, nd);
                        match style {
                            STYLE_TIME => write_time(&mut out.text, v as i64, nd as u8, sep, suffix),
                            STYLE_KM => write_km(&mut out.text, v as i64),
                            _ => write_number(&mut out.text, (v / POW10[k - nd]) as i64, nd as u8),
                        }
                    }
                    out.ends.push(out.text.len());
                }
            }
        }
        _ => return Err(Bad("an unknown column")),
    }
    if out.ends.len() != rows {
        return Err(Bad("a column with the wrong number of rows"));
    }
    Ok(())
}

/// The text the streams were made from. More than `max` bytes of it is an error.
pub fn decode(b: &[u8], max: usize) -> Result<Vec<u8>, Bad> {
    let mut c = Cursor { b, at: 0 };
    let mut out = Vec::with_capacity(max.min(b.len() * 4));
    let mut cols: Vec<Column> = Vec::new();
    loop {
        out.extend_from_slice(c.bytes()?);
        let rows = c.len()?;
        if rows == 0 {
            break;
        }
        let (sep, cr) = (c.byte()?, c.byte()? == 1);
        let n = c.len()?;
        if n == 0 || rows.saturating_mul(n) > max {
            return Err(Bad("more rows than fit"));
        }
        cols.resize_with(cols.len().max(n), Column::default);
        for col in cols.iter_mut().take(n) {
            decode_column(&mut c, rows, col)?;
        }
        for r in 0..rows {
            for (k, col) in cols.iter().take(n).enumerate() {
                let from = if r == 0 { 0 } else { col.ends[r - 1] };
                out.extend_from_slice(&col.text[from..col.ends[r]]);
                out.push(if k + 1 < n {
                    sep
                } else if cr {
                    b'\r'
                } else {
                    b'\n'
                });
            }
            if cr {
                out.push(b'\n');
            }
            if out.len() > max {
                return Err(Bad("more text than the block holds"));
            }
        }
    }
    if c.at != b.len() || out.len() > max {
        return Err(Bad("bytes left over"));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn back(text: &[u8]) -> Vec<u8> {
        let e = encode(text);
        let d = decode(&e, text.len()).unwrap();
        assert_eq!(d, text, "{:?}", String::from_utf8_lossy(text));
        e
    }

    #[test]
    fn numbers_are_stored_as_numbers_only_when_their_text_comes_back() {
        for (t, want) in [
            ("0", Some((0, 0))),
            ("-12", Some((-12, 0))),
            ("0.50", Some((50, 2))),
            ("-0.5", Some((-5, 1))),
            ("12000.853759999999", Some((12_000_853_759_999_999, 12))),
            ("999999999999999999", Some((999_999_999_999_999_999, 0))),
            ("1000000000000000000", None),
            ("-0", None),
            ("-0.00", None),
            ("007", None),
            ("00", None),
            ("1e5", None),
            ("+1", None),
            ("1.", None),
            (".5", None),
            ("-", None),
            ("", None),
            (" 1", None),
            ("1 ", None),
            ("1.2.3", None),
            ("\\N", None),
            ("NaN", None),
        ] {
            assert_eq!(number(t.as_bytes()), want, "{t:?}");
            if let Some((v, nd)) = want {
                let mut out = Vec::new();
                write_number(&mut out, v, nd);
                assert_eq!(out, t.as_bytes());
            }
        }
    }

    #[test]
    fn timestamps_come_back_with_their_fraction_and_what_follows() {
        for t in [
            "2024-03-01 08:01:00",
            "2024-03-01 08:01:00.123456",
            "2024-03-01 08:01:00.1",
            "2024-03-01 08:01:00.120",
            "2024-02-29T23:59:59.000001",
            "1969-12-31 23:59:59.5",
            "0001-01-01 00:00:00",
            "9999-12-31 23:59:59.999999+01:00",
            "2024-03-01 08:01:00+00",
            "2024-03-01T08:01:00Z",
        ] {
            let (us, nd, sep, suffix) = time(t.as_bytes()).unwrap();
            let mut out = Vec::new();
            write_time(&mut out, us, nd, sep, suffix);
            assert_eq!(String::from_utf8(out).unwrap(), t);
        }
        for t in [
            "2023-02-29 00:00:00",
            "2024-13-01 00:00:00",
            "2024-03-01 24:00:00",
            "2024-03-01 08:01:60",
            "2024-03-01 08:01:00.",
            "2024-03-01 08:01:00.1234567",
            "2024-03-01 08:01:00 and a long text after it",
            "2024-03-01_08:01:00",
            "2024-3-01 08:01:00",
        ] {
            assert!(time(t.as_bytes()).is_none(), "{t}");
        }
    }

    #[test]
    fn rows_of_a_copy_block_and_of_a_csv_file_come_back_byte_for_byte() {
        let copy = "-- a comment\nCOPY public.t (id, date, v, w, s) FROM stdin;\n\
            1\t2024-03-01 08:01:00.123456\t12.301000\t\\N\tonline\n\
            3\t2024-03-01 08:02:00.5\t12.302000\t-0\tonline\n\
            5\t2024-03-01 08:03:00\t-12.303000\t007\tasleep\n\
            7\t2024-03-01 08:04:00.12\t0.000001\t1e5\tonline\n\
            9\t\\N\t12.3\t+1\t\n\
            11\t2024-03-01 08:06:00\t12\t1.\t a space \n\
            \\.\n\nlast line without its end";
        let e = back(copy.as_bytes());
        assert!(e.len() < copy.len(), "{} of {}", e.len(), copy.len());
        let csv = "id,Date,name,v\r\n1,2024-03-01 08:01:00,\"Whale, the\",0.5\r\n2,2024-03-01 08:02:00,\"Whale, the\",.5\r\n\
            3,2024-03-01 08:03:00,\"Whale, the\",1.50\r\n4,2024-03-01 08:04:00,\"a \"\"quoted\"\" one\",1.5\r\n\
            5,2024-03-01 08:05:00,\"two\nlines\",1\r\n6,,,\r\n";
        back(csv.as_bytes());
        for t in ["", "\n", "x", "\r\n", "a\tb", "a\tb\n", "\t\n\t\n\t\n\t\n\t\n", ",\n,\n,\n,\n", "\0\0\n\0\t\0\n"] {
            back(t.as_bytes());
        }
        back(&[0xff, 0xfe, b'\t', 0x80, b'\n', 0xff, b'\t', b'\n', 0xc3, b'\t', 0x28, b'\n', b'\t', b'\n', b'\t']);
    }

    #[test]
    fn what_is_not_a_stream_is_an_error_not_a_panic() {
        let text = b"1\t2\n3\t4\n5\t6\n7\t8\n9\t10\n";
        let e = encode(text);
        for cut in 0..e.len() {
            assert!(decode(&e[..cut], text.len()).is_err(), "cut at {cut}");
        }
        for i in 0..e.len() {
            let mut b = e.clone();
            b[i] ^= 0x55;
            // changed bytes may still be a stream, of another text; they must not panic
            let _ = decode(&b, text.len());
        }
        assert!(decode(&e, text.len() - 1).is_err(), "more text than allowed");
    }
}
