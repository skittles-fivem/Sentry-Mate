//! Finding the tables' rows in a plain pg_dump backup without running any of it.
//!
//! A plain backup is SQL, but every table's rows are in a block pg_dump writes the same way:
//!
//! ```text
//! COPY public.positions (id, date, latitude, ...) FROM stdin;
//! 1    2024-03-01 08:01:00    12.301000    ...
//! \.
//! ```
//!
//! one row per line, fields split by tabs (shown as spaces above), `\N` for NULL, escapes for tabs, newlines and
//! backslashes inside values. Only these blocks are read; every other line (settings, tables, functions, indexes) is
//! passed over. The reader is fed bytes as they unpack and holds at most one line ([`CopyReader::new`]'s `max_line`;
//! a longer row is counted and dropped). A block the handler doesn't want is passed over without being split or
//! copied at all: only the first bytes of each of its lines are looked at, to find the `\.` that ends it. That is how
//! Tesla's tokens are never read.

use crate::value::Field;
use memchr::{memchr, memchr_iter};

/// A `COPY` line: which table the block is, and its columns in the order the rows give them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyHeader {
    /// `None` when the name isn't schema-qualified (pg_dump before 10.3 didn't qualify them).
    pub schema: Option<String>,
    pub table: String,
    pub columns: Vec<String>,
}

impl CopyHeader {
    /// `schema.table`, for reports.
    pub fn full_name(&self) -> String {
        match &self.schema {
            Some(s) => format!("{s}.{}", self.table),
            None => self.table.clone(),
        }
    }
}

/// What to do with a block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Take {
    /// Hand its rows over.
    Rows,
    /// Pass over it unread.
    Skip,
}

/// One row: the line and where its fields end.
pub struct RawRow<'a> {
    line: &'a [u8],
    ends: &'a [usize],
}

impl<'a> RawRow<'a> {
    /// How many fields the row has.
    pub fn fields(&self) -> usize {
        self.ends.len()
    }

    /// Field `i` (0-based), NULL as [`Field::Null`]; past the end as [`Field::Absent`].
    pub fn field(&self, i: usize) -> Field<'a> {
        let Some(&end) = self.ends.get(i) else { return Field::Absent };
        let start = if i == 0 { 0 } else { self.ends[i - 1] + 1 };
        let raw = &self.line[start..end];
        if raw == b"\\N" { Field::Null } else { Field::Raw(raw) }
    }
}

/// Receives what the reader finds.
pub trait Handler {
    type Error;
    /// A block starts: take its rows, or pass over it.
    fn begin(&mut self, header: &CopyHeader) -> Result<Take, Self::Error>;
    fn row(&mut self, row: &RawRow<'_>) -> Result<(), Self::Error>;
    /// A row of the current block was longer than the line limit: dropped.
    fn too_long(&mut self);
    /// The block ended (also after a passed-over one).
    fn end(&mut self) -> Result<(), Self::Error>;
    /// A `COPY` line that couldn't be read: its block is passed over.
    fn unreadable(&mut self) {}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Sql,
    Rows,
    Skip,
}

pub struct CopyReader {
    mode: Mode,
    max_line: usize,
    partial: Vec<u8>,
    /// The current line passed `max_line`: the rest of it is dropped.
    overlong: bool,
    /// In a passed-over block: the first bytes of the current line, and how long it is so far.
    skip_head: [u8; 3],
    skip_len: usize,
    ends: Vec<usize>,
    /// The line pg_dump writes last was seen.
    complete: bool,
}

const COMPLETE: &[u8] = b"-- PostgreSQL database dump complete";

impl CopyReader {
    pub fn new(max_line: usize) -> Self {
        CopyReader {
            mode: Mode::Sql,
            max_line,
            partial: Vec::new(),
            overlong: false,
            skip_head: [0; 3],
            skip_len: 0,
            ends: Vec::new(),
            complete: false,
        }
    }

    /// The backup ended with pg_dump's closing line, outside any block (a backup cut short doesn't).
    pub fn saw_end(&self) -> bool {
        self.complete && self.mode == Mode::Sql
    }

    /// The input ended inside a block.
    pub fn inside_block(&self) -> bool {
        self.mode != Mode::Sql
    }

    /// The input is over: a last line without a newline is read too.
    pub fn finish<H: Handler>(&mut self, h: &mut H) -> Result<(), H::Error> {
        if self.mode == Mode::Skip {
            if self.skip_len > 0 {
                self.feed(b"\n", h)?;
            }
        } else if !self.partial.is_empty() || self.overlong {
            self.feed(b"\n", h)?;
        }
        Ok(())
    }

    pub fn feed<H: Handler>(&mut self, mut data: &[u8], h: &mut H) -> Result<(), H::Error> {
        while !data.is_empty() {
            let (piece, rest, whole) = match memchr(b'\n', data) {
                Some(i) => (&data[..i], &data[i + 1..], true),
                None => (data, &data[data.len()..], false),
            };
            data = rest;
            if self.mode == Mode::Skip {
                self.skip_piece(piece);
                if whole {
                    let head = &self.skip_head[..self.skip_len.min(3)];
                    let end = (self.skip_len == 2 && head == b"\\.") || (self.skip_len == 3 && head == b"\\.\r");
                    self.skip_len = 0;
                    self.skip_head = [0; 3];
                    if end {
                        self.mode = Mode::Sql;
                        h.end()?;
                    }
                }
                continue;
            }
            if self.overlong {
                if whole {
                    self.overlong = false;
                    if self.mode == Mode::Rows {
                        h.too_long();
                    }
                }
                continue;
            }
            if self.partial.len() + piece.len() > self.max_line {
                self.partial.clear();
                if whole {
                    if self.mode == Mode::Rows {
                        h.too_long();
                    }
                } else {
                    self.overlong = true;
                }
                continue;
            }
            if !whole {
                self.partial.extend_from_slice(piece);
                continue;
            }
            if self.partial.is_empty() {
                self.line(piece, h)?;
            } else {
                let mut l = std::mem::take(&mut self.partial);
                l.extend_from_slice(piece);
                let r = self.line(&l, h);
                l.clear();
                self.partial = l;
                r?;
            }
        }
        Ok(())
    }

    fn skip_piece(&mut self, piece: &[u8]) {
        for (i, &b) in piece.iter().take(3usize.saturating_sub(self.skip_len)).enumerate() {
            self.skip_head[self.skip_len + i] = b;
        }
        self.skip_len = self.skip_len.saturating_add(piece.len());
    }

    fn line<H: Handler>(&mut self, l: &[u8], h: &mut H) -> Result<(), H::Error> {
        let l = l.strip_suffix(b"\r").unwrap_or(l);
        match self.mode {
            Mode::Rows => {
                if l == b"\\." {
                    self.mode = Mode::Sql;
                    return h.end();
                }
                self.ends.clear();
                self.ends.extend(memchr_iter(b'\t', l));
                self.ends.push(l.len());
                h.row(&RawRow { line: l, ends: &self.ends })
            }
            Mode::Sql => {
                if l.starts_with(b"COPY ") {
                    match parse_header(l) {
                        Some(header) => {
                            self.mode = match h.begin(&header)? {
                                Take::Rows => Mode::Rows,
                                Take::Skip => Mode::Skip,
                            };
                        }
                        None => {
                            h.unreadable();
                            self.mode = Mode::Skip;
                        }
                    }
                    self.complete = false;
                } else if l == COMPLETE {
                    self.complete = true;
                }
                Ok(())
            }
            // Lines of a passed-over block never get here.
            Mode::Skip => Ok(()),
        }
    }
}

/// `COPY <schema>.<table> (<col>, ...) FROM stdin;` (names quoted or not). `None` for anything else.
pub fn parse_header(line: &[u8]) -> Option<CopyHeader> {
    let s = std::str::from_utf8(line).ok()?;
    let rest = s.strip_prefix("COPY ")?;
    let tail = " FROM stdin;";
    let cut = rest.len().checked_sub(tail.len())?;
    if !rest.get(cut..)?.eq_ignore_ascii_case(tail) {
        return None;
    }
    let rest = rest.get(..cut)?.trim_end();
    let (first, mut rest) = ident(rest)?;
    let (schema, table) = if let Some(r) = rest.strip_prefix('.') {
        let (t, r) = ident(r)?;
        rest = r;
        (Some(first), t)
    } else {
        (None, first)
    };
    let rest = rest.trim_start();
    let mut columns = Vec::new();
    if !rest.is_empty() {
        let mut r = rest.strip_prefix('(')?.trim_start();
        loop {
            let (c, after) = ident(r)?;
            columns.push(c);
            let after = after.trim_start();
            if let Some(a) = after.strip_prefix(',') {
                r = a.trim_start();
            } else {
                if after != ")" {
                    return None;
                }
                break;
            }
        }
    }
    Some(CopyHeader { schema, table, columns })
}

/// One SQL identifier: `"quoted ""like"" this"` or a plain one (which PostgreSQL folds to lower case).
fn ident(s: &str) -> Option<(String, &str)> {
    if let Some(q) = s.strip_prefix('"') {
        let mut out = String::new();
        let mut it = q.char_indices();
        while let Some((i, c)) = it.next() {
            if c == '"' {
                if q[i + 1..].starts_with('"') {
                    out.push('"');
                    it.next();
                } else {
                    return Some((out, &q[i + 1..]));
                }
            } else {
                out.push(c);
            }
        }
        return None;
    }
    let n = s.find(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$')).unwrap_or(s.len());
    if n == 0 {
        return None;
    }
    Some((s[..n].to_lowercase(), &s[n..]))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[derive(Default)]
    struct Seen {
        blocks: Vec<String>,
        rows: Vec<Vec<Option<Vec<u8>>>>,
        too_long: usize,
        ends: usize,
        unreadable: usize,
        skip: Vec<&'static str>,
    }

    impl Handler for Seen {
        type Error = ();
        fn begin(&mut self, h: &CopyHeader) -> Result<Take, ()> {
            self.blocks.push(format!("{} {}", h.full_name(), h.columns.join(",")));
            Ok(if self.skip.contains(&h.table.as_str()) { Take::Skip } else { Take::Rows })
        }
        fn row(&mut self, r: &RawRow<'_>) -> Result<(), ()> {
            self.rows.push(
                (0..r.fields())
                    .map(|i| match r.field(i) {
                        Field::Raw(b) => Some(b.to_vec()),
                        _ => None,
                    })
                    .collect(),
            );
            Ok(())
        }
        fn too_long(&mut self) {
            self.too_long += 1;
        }
        fn end(&mut self) -> Result<(), ()> {
            self.ends += 1;
            Ok(())
        }
        fn unreadable(&mut self) {
            self.unreadable += 1;
        }
    }

    const DUMP: &str = "--\n-- PostgreSQL database dump\n--\nSET client_encoding = 'UTF8';\n\
        CREATE TABLE public.a (id integer);\n\
        COPY private.tokens (id, refresh) FROM stdin;\n1\t\\\\x0102\n\\.x\n\\\n\\.\n\
        COPY public.a (id, \"row\", name) FROM stdin;\n1\t\\N\tone\\ttwo\n2\tx\t\\N\n\\.\n\
        -- PostgreSQL database dump complete\n";

    fn run(dump: &[u8], chunk: usize, skip: &[&'static str]) -> (Seen, CopyReader) {
        let mut seen = Seen { skip: skip.to_vec(), ..Seen::default() };
        let mut r = CopyReader::new(64);
        for c in dump.chunks(chunk) {
            r.feed(c, &mut seen).unwrap();
        }
        r.finish(&mut seen).unwrap();
        (seen, r)
    }

    #[test]
    fn reads_blocks_and_rows_at_any_chunk_size() {
        for chunk in [1, 2, 3, 5, 7, 64, 4096] {
            let (seen, r) = run(DUMP.as_bytes(), chunk, &["tokens"]);
            assert_eq!(seen.blocks, ["private.tokens id,refresh", "public.a id,row,name"], "chunk {chunk}");
            assert_eq!(
                seen.rows,
                vec![
                    vec![Some(b"1".to_vec()), None, Some(b"one\\ttwo".to_vec())],
                    vec![Some(b"2".to_vec()), Some(b"x".to_vec()), None],
                ]
            );
            assert_eq!(seen.ends, 2);
            assert!(r.saw_end());
        }
    }

    #[test]
    fn a_passed_over_block_ends_only_at_its_own_marker() {
        // Lines that merely start like the marker (`\.x`, `\`) don't end it, at any split.
        for chunk in [1, 2, 3, 4096] {
            let (seen, _) = run(DUMP.as_bytes(), chunk, &["tokens", "a"]);
            assert!(seen.rows.is_empty());
            assert_eq!(seen.ends, 2);
        }
    }

    #[test]
    fn crlf_lines() {
        let dump = DUMP.replace('\n', "\r\n");
        for chunk in [1, 3, 4096] {
            let (seen, r) = run(dump.as_bytes(), chunk, &["tokens"]);
            assert_eq!(seen.rows.len(), 2);
            assert_eq!(seen.rows[1][2], None);
            assert_eq!(seen.ends, 2);
            assert!(r.saw_end());
            let (seen, _) = run(dump.as_bytes(), chunk, &["tokens", "a"]);
            assert_eq!(seen.ends, 2);
        }
    }

    #[test]
    fn long_rows_are_dropped_and_counted() {
        let long = "y".repeat(100);
        let dump = format!("COPY a (id) FROM stdin;\n1\n{long}\n2\n\\.\nSET x = '{long}';\n");
        for chunk in [1, 7, 4096] {
            let (seen, _) = run(dump.as_bytes(), chunk, &[]);
            assert_eq!(seen.rows, vec![vec![Some(b"1".to_vec())], vec![Some(b"2".to_vec())]], "chunk {chunk}");
            assert_eq!(seen.too_long, 1);
            assert_eq!(seen.ends, 1);
        }
    }

    #[test]
    fn cut_short() {
        let (seen, r) = run(b"COPY a (id) FROM stdin;\n1\n2", 4096, &[]);
        assert_eq!(seen.rows.len(), 2);
        assert!(r.inside_block());
        assert!(!r.saw_end());
        let (_, r) = run(b"COPY a (id) FROM stdin;\n1\n\\.\n", 4096, &[]);
        assert!(!r.inside_block());
        assert!(!r.saw_end());
    }

    #[test]
    fn headers() {
        let h = parse_header(b"COPY public.positions (id, date, \"row\", \"Odd \"\"name\"\"\") FROM stdin;").unwrap();
        assert_eq!(h.schema.as_deref(), Some("public"));
        assert_eq!(h.table, "positions");
        assert_eq!(h.columns, ["id", "date", "row", "Odd \"name\""]);
        let h = parse_header(b"COPY Cars (ID,Name) FROM STDIN;").unwrap();
        assert_eq!((h.schema, h.table.as_str()), (None, "cars"));
        assert_eq!(h.columns, ["id", "name"]);
        let h = parse_header(b"COPY \"private\".\"tokens\" FROM stdin;").unwrap();
        assert_eq!(h.full_name(), "private.tokens");
        assert!(h.columns.is_empty());
        for bad in [
            &b"COPY public.a (id FROM stdin;"[..],
            b"COPY public.a (id) TO stdout;",
            b"COPY (SELECT 1) FROM stdin;",
            b"COPY public. (id) FROM stdin;",
            b"COPY \"unterminated (id) FROM stdin;",
        ] {
            assert_eq!(parse_header(bad), None, "{}", String::from_utf8_lossy(bad));
        }
        let (seen, _) = run(b"COPY public.a (id FROM stdin;\n1\n\\.\nCOPY b (x) FROM stdin;\n2\n\\.\n", 4096, &[]);
        assert_eq!(seen.unreadable, 1);
        assert_eq!(seen.rows, vec![vec![Some(b"2".to_vec())]]);
    }
}
