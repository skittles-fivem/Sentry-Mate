//! One field of a COPY row, as PostgreSQL's text format writes it, turned into a value.
//!
//! pg_dump writes each value as text: `\N` for NULL, and backslash escapes for the bytes that would break the format
//! (`\t`, `\n`, `\r`, `\\`, ...), plus octal (`\101`) and hex (`\x41`) escapes PostgreSQL also accepts. Numbers and
//! times never hold escapes in practice, but they are unescaped first when they do, so nothing depends on that.
//!
//! TeslaMate keeps its times as `timestamp without time zone` holding UTC; they become milliseconds since 1970 UTC
//! (anything after the millisecond is dropped, as the model keeps ms). A value that isn't what its column should hold
//! (a number that doesn't parse, `NaN`, a date before 1970) reads as unreadable, never as a made-up value.

use std::borrow::Cow;

/// One field of a row: not in this backup's table at all, NULL, or its raw (still escaped) bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field<'a> {
    Absent,
    Null,
    Raw(&'a [u8]),
}

/// What reading a field as some type gave.
#[derive(Debug, Clone, PartialEq)]
pub enum Val<T> {
    /// The column isn't in this backup (an older or newer TeslaMate).
    Absent,
    Null,
    Ok(T),
    /// There, but not readable as what the column holds.
    Bad,
}

/// The bytes a raw field stands for, escapes undone.
pub fn unescape(raw: &[u8]) -> Cow<'_, [u8]> {
    if !raw.contains(&b'\\') {
        return Cow::Borrowed(raw);
    }
    let mut out = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        let c = raw[i];
        i += 1;
        if c != b'\\' {
            out.push(c);
            continue;
        }
        let Some(&e) = raw.get(i) else {
            // A lone backslash at the very end: PostgreSQL wouldn't write one; keep it as it is.
            out.push(b'\\');
            break;
        };
        i += 1;
        match e {
            b'b' => out.push(0x08),
            b'f' => out.push(0x0c),
            b'n' => out.push(b'\n'),
            b'r' => out.push(b'\r'),
            b't' => out.push(b'\t'),
            b'v' => out.push(0x0b),
            b'0'..=b'7' => {
                let mut v = u32::from(e - b'0');
                for _ in 0..2 {
                    match raw.get(i) {
                        Some(&d @ b'0'..=b'7') => {
                            v = v * 8 + u32::from(d - b'0');
                            i += 1;
                        }
                        _ => break,
                    }
                }
                out.push((v & 0xff) as u8);
            }
            b'x' => match raw.get(i).and_then(|&d| hex(d)) {
                Some(h) => {
                    i += 1;
                    let mut v = h;
                    if let Some(l) = raw.get(i).and_then(|&d| hex(d)) {
                        v = v * 16 + l;
                        i += 1;
                    }
                    out.push(v);
                }
                None => out.push(b'x'),
            },
            other => out.push(other),
        }
    }
    Cow::Owned(out)
}

fn hex(d: u8) -> Option<u8> {
    match d {
        b'0'..=b'9' => Some(d - b'0'),
        b'a'..=b'f' => Some(d - b'a' + 10),
        b'A'..=b'F' => Some(d - b'A' + 10),
        _ => None,
    }
}

/// Text, escapes undone; bytes that aren't UTF-8 become U+FFFD (the backup says `client_encoding = 'UTF8'`).
pub fn text(raw: &[u8]) -> String {
    match unescape(raw) {
        Cow::Borrowed(b) => String::from_utf8_lossy(b).into_owned(),
        Cow::Owned(v) => match String::from_utf8(v) {
            Ok(s) => s,
            Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
        },
    }
}

fn ascii(raw: &[u8]) -> Option<Cow<'_, str>> {
    match unescape(raw) {
        Cow::Borrowed(b) => std::str::from_utf8(b).ok().map(Cow::Borrowed),
        Cow::Owned(v) => String::from_utf8(v).ok().map(Cow::Owned),
    }
}

/// A finite number (`numeric`, `double precision`, `smallint`, ...).
pub fn number(raw: &[u8]) -> Option<f64> {
    let s = ascii(raw)?;
    let v: f64 = s.trim().parse().ok()?;
    v.is_finite().then_some(v)
}

/// A whole number (ids, `smallint`, `integer`, `bigint`).
pub fn int(raw: &[u8]) -> Option<i64> {
    ascii(raw)?.trim().parse().ok()
}

/// `t` or `f` (also `true` / `false`, which PostgreSQL accepts too).
pub fn boolean(raw: &[u8]) -> Option<bool> {
    match ascii(raw)?.trim() {
        "t" | "true" | "TRUE" => Some(true),
        "f" | "false" | "FALSE" => Some(false),
        _ => None,
    }
}

/// `YYYY-MM-DD HH:MM:SS[.ffffff][+HH[:MM]]` (also with `T`), as ms since 1970 UTC. Without an offset the time is
/// taken as UTC, which is how TeslaMate keeps it.
pub fn timestamp_ms(raw: &[u8]) -> Option<u64> {
    let s = ascii(raw)?;
    let b = s.trim().as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || !(b[10] == b' ' || b[10] == b'T') || b[13] != b':' {
        return None;
    }
    if b[16] != b':' {
        return None;
    }
    let year = digits(&b[0..4])?;
    let month = digits(&b[5..7])?;
    let day = digits(&b[8..10])?;
    let hour = digits(&b[11..13])?;
    let minute = digits(&b[14..16])?;
    let second = digits(&b[17..19])?;
    if !(1..=12).contains(&month) || day < 1 || day > days_in_month(year, month) || hour > 23 || minute > 59 {
        return None;
    }
    // PostgreSQL allows a leap second (60) on input but never stores one; take it as the next second.
    if second > 60 {
        return None;
    }
    let mut rest = &b[19..];
    let mut ms = 0i64;
    if let Some(r) = rest.strip_prefix(b".") {
        let n = r.iter().take_while(|c| c.is_ascii_digit()).count();
        if n == 0 {
            return None;
        }
        let frac = &r[..n];
        for (i, d) in frac.iter().take(3).enumerate() {
            ms += i64::from(d - b'0') * [100, 10, 1][i];
        }
        rest = &r[n..];
    }
    let mut offset_s = 0i64;
    if !rest.is_empty() {
        let (sign, r) = match rest[0] {
            b'+' => (1, &rest[1..]),
            b'-' => (-1, &rest[1..]),
            b'Z' if rest.len() == 1 => (1, &rest[..0]),
            _ => return None,
        };
        if !r.is_empty() {
            let (h, m) = match r.len() {
                2 => (digits(r)?, 0),
                4 => (digits(&r[..2])?, digits(&r[2..])?),
                5 if r[2] == b':' => (digits(&r[..2])?, digits(&r[3..])?),
                _ => return None,
            };
            offset_s = sign * (h * 3600 + m * 60);
        }
    }
    let days = days_from_civil(year, month, day);
    let secs = days * 86_400 + hour * 3600 + minute * 60 + second - offset_s;
    let total = secs.checked_mul(1000)?.checked_add(ms)?;
    u64::try_from(total).ok()
}

fn digits(b: &[u8]) -> Option<i64> {
    let mut v = 0i64;
    for &c in b {
        if !c.is_ascii_digit() {
            return None;
        }
        v = v * 10 + i64::from(c - b'0');
    }
    Some(v)
}

fn is_leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

fn days_in_month(y: i64, m: i64) -> i64 {
    match m {
        2 if is_leap(y) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Days from 1970-01-01 to a date of the proleptic Gregorian calendar (the well-known civil-from-days arithmetic,
/// counting in 400-year eras).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn escapes() {
        assert_eq!(&*unescape(b"plain"), b"plain");
        assert_eq!(&*unescape(br"a\tb\nc\rd\\e"), b"a\tb\nc\rd\\e");
        assert_eq!(&*unescape(br"\b\f\v"), b"\x08\x0c\x0b");
        assert_eq!(&*unescape(br"\101\60\0x"), b"A0\0x");
        assert_eq!(&*unescape(br"\777"), b"\xff");
        assert_eq!(&*unescape(br"\x41\x4a2\xg"), b"AJ2xg");
        assert_eq!(&*unescape(br"\x4"), b"\x04");
        assert_eq!(&*unescape(br"\.\N\q"), b".Nq");
        assert_eq!(&*unescape(br"end\"), b"end\\");
        assert_eq!(text(br"Caf\303\251 \x41"), "Café A");
        assert_eq!(text("Café\\tok".as_bytes()), "Café\tok");
        assert_eq!(text(br"bad \377 byte"), "bad \u{fffd} byte");
    }

    #[test]
    fn numbers() {
        assert_eq!(number(b"12.300000"), Some(12.3));
        assert_eq!(number(b"-45.6"), Some(-45.6));
        assert_eq!(number(b"30001"), Some(30001.0));
        assert_eq!(number(b"1e3"), Some(1000.0));
        assert_eq!(number(b"NaN"), None);
        assert_eq!(number(b"Infinity"), None);
        assert_eq!(number(b"twelve"), None);
        assert_eq!(number(b""), None);
        assert_eq!(int(b"-12"), Some(-12));
        assert_eq!(int(b"9223372036854775807"), Some(i64::MAX));
        assert_eq!(int(b"1.5"), None);
        assert_eq!(boolean(b"t"), Some(true));
        assert_eq!(boolean(b"f"), Some(false));
        assert_eq!(boolean(b"yes"), None);
    }

    #[test]
    fn timestamps() {
        assert_eq!(timestamp_ms(b"1970-01-01 00:00:00"), Some(0));
        assert_eq!(timestamp_ms(b"2024-03-01 08:00:00"), Some(1_709_280_000_000));
        assert_eq!(timestamp_ms(b"2024-03-01 08:00:00.123456"), Some(1_709_280_000_123));
        assert_eq!(timestamp_ms(b"2024-03-01 08:00:00.5"), Some(1_709_280_000_500));
        assert_eq!(timestamp_ms(b"2024-03-01T08:00:00Z"), Some(1_709_280_000_000));
        assert_eq!(timestamp_ms(b"2024-03-01 10:00:00+02"), Some(1_709_280_000_000));
        assert_eq!(timestamp_ms(b"2024-03-01 03:30:00-04:30"), Some(1_709_280_000_000));
        assert_eq!(timestamp_ms(b"2024-02-29 00:00:00"), Some(1_709_164_800_000));
        assert_eq!(timestamp_ms(b"2000-02-29 12:00:00"), Some(951_825_600_000));
        assert_eq!(timestamp_ms(b"2023-02-29 00:00:00"), None);
        assert_eq!(timestamp_ms(b"1900-02-29 00:00:00"), None);
        assert_eq!(timestamp_ms(b"2024-13-01 00:00:00"), None);
        assert_eq!(timestamp_ms(b"2024-03-01 24:00:00"), None);
        assert_eq!(timestamp_ms(b"1969-12-31 23:59:59"), None);
        assert_eq!(timestamp_ms(b"2024-03-01"), None);
        assert_eq!(timestamp_ms(b"2024-03-01 08:00:00."), None);
        assert_eq!(timestamp_ms(b"2024-03-01 08:00:00 junk"), None);
        assert_eq!(timestamp_ms(b"infinity"), None);
    }
}
