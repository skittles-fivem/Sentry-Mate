//! TeslaFi's CSV, a line at a time: comma separated, `"` quoting with `""` for a quote, CRLF or LF, UTF-8 (with or
//! without a BOM) or, for a line that is not UTF-8, Windows-1252. A record is one line (TeslaFi writes no line breaks
//! inside fields). Some files start a line with a stray `"`: a line with an odd number of quotes that starts with one
//! has it taken off. Lines longer than the limit are passed over (counted), never held whole.

use std::borrow::Cow;
use std::io::{self, BufRead};

/// One line read: its bytes, or that it was too long.
pub(crate) enum Line {
    Text,
    TooLong,
}

/// Reads lines into `buf` (without the line ending), holding at most `max` bytes.
pub(crate) struct Lines<R> {
    inner: R,
    max: usize,
    pub bytes: u64,
}

impl<R: BufRead> Lines<R> {
    pub fn new(inner: R, max: usize) -> Lines<R> {
        Lines { inner, max, bytes: 0 }
    }

    /// The next line, `None` at the end.
    pub fn next(&mut self, buf: &mut Vec<u8>) -> io::Result<Option<Line>> {
        buf.clear();
        let mut over = false;
        let mut any = false;
        loop {
            let avail = match self.inner.fill_buf() {
                Ok(b) => b,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            };
            if avail.is_empty() {
                if !any {
                    return Ok(None);
                }
                break;
            }
            any = true;
            let (take, done) = match memchr::memchr(b'\n', avail) {
                Some(i) => (i + 1, true),
                None => (avail.len(), false),
            };
            let part = &avail[..if done { take - 1 } else { take }];
            if !over {
                if buf.len() + part.len() > self.max {
                    over = true;
                    buf.clear();
                } else {
                    buf.extend_from_slice(part);
                }
            }
            self.inner.consume(take);
            self.bytes += take as u64;
            if done {
                break;
            }
        }
        if over {
            return Ok(Some(Line::TooLong));
        }
        if buf.last() == Some(&b'\r') {
            buf.pop();
        }
        Ok(Some(Line::Text))
    }
}

/// Windows-1252's characters for 0x80..0x9F (0 where it has none).
const CP1252_HIGH: [u16; 32] = [
    0x20AC, 0, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160, 0x2039, 0x0152, 0, 0x017D, 0, 0,
    0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013, 0x2014, 0x02DC, 0x2122, 0x0161, 0x203A, 0x0153, 0, 0x017E, 0x0178,
];

/// A line as text: UTF-8 as it is, else read as Windows-1252 (`true` when it was).
pub(crate) fn text(b: &[u8]) -> (Cow<'_, str>, bool) {
    let b = b.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(b);
    match std::str::from_utf8(b) {
        Ok(s) => (Cow::Borrowed(s), false),
        Err(_) => {
            let s: String = b
                .iter()
                .map(|&c| match c {
                    0x80..=0x9F => char::from_u32(u32::from(CP1252_HIGH[usize::from(c - 0x80)])).unwrap_or('\u{FFFD}'),
                    _ => char::from(c),
                })
                .collect();
            (Cow::Owned(s), true)
        }
    }
}

/// The fields of one line, as ranges into it (a field holding `""` is unescaped when read).
#[derive(Debug, Default)]
pub(crate) struct Fields {
    spans: Vec<(usize, usize, bool)>,
}

impl Fields {
    /// Splits `line` (a stray leading quote taken off first).
    pub fn split(&mut self, line: &str) -> usize {
        self.spans.clear();
        let off = usize::from(line.starts_with('"') && line.bytes().filter(|&c| c == b'"').count() % 2 == 1);
        let b = line.as_bytes();
        let mut i = off;
        loop {
            if b.get(i) == Some(&b'"') {
                // quoted: up to the closing quote not doubled
                let start = i + 1;
                let mut j = start;
                let mut escaped = false;
                let end;
                loop {
                    match b.get(j) {
                        None => {
                            end = j;
                            break;
                        }
                        Some(b'"') if b.get(j + 1) == Some(&b'"') => {
                            escaped = true;
                            j += 2;
                        }
                        Some(b'"') => {
                            end = j;
                            j += 1;
                            break;
                        }
                        Some(_) => j += 1,
                    }
                }
                self.spans.push((start, end, escaped));
                // anything after the closing quote up to the comma is passed over
                match memchr::memchr(b',', &b[j.min(b.len())..]) {
                    Some(k) => i = j + k + 1,
                    None => break,
                }
            } else {
                match memchr::memchr(b',', &b[i.min(b.len())..]) {
                    Some(k) => {
                        self.spans.push((i, i + k, false));
                        i += k + 1;
                    }
                    None => {
                        self.spans.push((i, b.len(), false));
                        break;
                    }
                }
            }
        }
        if off == 1
            && let Some(first) = self.spans.first_mut()
        {
            first.0 = first.0.max(1);
        }
        self.spans.len()
    }

    pub fn len(&self) -> usize {
        self.spans.len()
    }

    /// Field `i` of `line` (the line split last), trimmed.
    pub fn get<'a>(&self, line: &'a str, i: usize) -> Option<Cow<'a, str>> {
        let &(a, b, esc) = self.spans.get(i)?;
        let s = line.get(a..b)?.trim();
        Some(if esc { Cow::Owned(s.replace("\"\"", "\"")) } else { Cow::Borrowed(s) })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn fields(line: &str) -> Vec<String> {
        let mut f = Fields::default();
        f.split(line);
        (0..f.len()).map(|i| f.get(line, i).unwrap().into_owned()).collect()
    }

    #[test]
    fn splits_quoted_and_plain_fields() {
        assert_eq!(fields("a,b,,c"), ["a", "b", "", "c"]);
        assert_eq!(fields(r#"1,"x, y","say ""hi""",z"#), ["1", "x, y", r#"say "hi""#, "z"]);
        assert_eq!(fields(r#""data_id,Date,state"#), ["data_id", "Date", "state"]);
        assert_eq!(fields(r#""a","b""#), ["a", "b"]);
        assert_eq!(fields(""), [""]);
        assert_eq!(fields("a,"), ["a", ""]);
    }

    #[test]
    fn reads_lines_bounded_and_windows_1252() {
        let data = b"one\r\ntwo-is-long\nthree".to_vec();
        let mut l = Lines::new(&data[..], 8);
        let mut buf = Vec::new();
        assert!(matches!(l.next(&mut buf).unwrap(), Some(Line::Text)));
        assert_eq!(buf, b"one");
        assert!(matches!(l.next(&mut buf).unwrap(), Some(Line::TooLong)));
        assert!(matches!(l.next(&mut buf).unwrap(), Some(Line::Text)));
        assert_eq!(buf, b"three");
        assert!(l.next(&mut buf).unwrap().is_none());
        assert_eq!(text(b"caf\xe9 \x80").0, "café €");
        assert_eq!(text(b"\xEF\xBB\xBFdata_id").0, "data_id");
    }
}
