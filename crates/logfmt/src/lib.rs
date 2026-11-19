//! Reading the Screenmate CAN Bridge's logs (SMO `docs/sentry-manager-format.md`).
//!
//! A log is one xz stream holding either candump text (`canbridge-candump-xz/1`) or the same lines as columns
//! (`canbridge-columns-xz/2`); both give the same lines, in the same order. Pure Rust, and bounded: a candump text
//! log is read line by line as it unpacks and stopped at [`Limits::max_unpacked`]; a columns body is decoded whole
//! but only up to [`Limits::max_columns_body`] and [`Limits::max_frames`]; a line longer than [`Limits::max_line`] is
//! skipped and counted. Neither unpacks to more than [`Limits::max_ratio`] times the packed size (at least
//! [`RATIO_FLOOR`]): real logs unpack to under 10 times theirs, so a small file crafted to unpack to gigabytes of
//! text, or to a columns body of millions of empty frames, costs no more than a real log of its size.

pub mod columns;
pub mod line;

pub use line::{ClockKind, Frame, HelperClock, Line};

use std::io::{self, Write};

pub const FORMAT_CANDUMP: &str = "canbridge-candump-xz/1";
pub const FORMAT_COLUMNS: &str = "canbridge-columns-xz/2";
/// The formats this reader takes, as a server's `ping` lists them.
pub const FORMATS: [&str; 2] = [FORMAT_CANDUMP, FORMAT_COLUMNS];
/// A log may always unpack to this much, whatever its packed size ([`Limits::max_ratio`]).
pub const RATIO_FLOOR: u64 = 1 << 20;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("not a valid xz stream: {0}")]
    Xz(String),
    #[error("malformed columns log: {0}")]
    Malformed(&'static str),
    #[error("columns log unpacks to more than {0} bytes")]
    ColumnsTooLarge(u64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Candump,
    Columns,
}

impl Format {
    pub fn name(self) -> &'static str {
        match self {
            Format::Candump => FORMAT_CANDUMP,
            Format::Columns => FORMAT_COLUMNS,
        }
    }
}

/// Bounds for reading one log (the same as Sentry Manager's: `docs/can-bridge-api.md`, "Reading a log").
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// A candump text log is stopped here (what came before is kept).
    pub max_unpacked: u64,
    /// A columns body larger than this is refused (the bridge closes one at 48 MB).
    pub max_columns_body: u64,
    /// Longer lines are skipped.
    pub max_line: usize,
    /// Unpacked bytes per packed byte at most (but never less than [`RATIO_FLOOR`] in all): candump text is cut
    /// there (as at [`Limits::max_unpacked`]), a columns body over it refused.
    pub max_ratio: u64,
    /// A columns body holding more frames than this is refused.
    pub max_frames: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_unpacked: 4 << 30,
            max_columns_body: 64 << 20,
            max_line: 512,
            max_ratio: 200,
            max_frames: columns::MAX_FRAMES,
        }
    }
}

impl Limits {
    /// These limits for a log of `packed` bytes: the unpack limits brought down to [`Limits::max_ratio`] of it.
    fn for_packed(self, packed: usize) -> Limits {
        let by_ratio = (packed as u64).saturating_mul(self.max_ratio).max(RATIO_FLOOR);
        Limits {
            max_unpacked: self.max_unpacked.min(by_ratio),
            max_columns_body: self.max_columns_body.min(by_ratio),
            ..self
        }
    }
}

/// How a read went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Summary {
    pub format: Format,
    pub lines: u64,
    pub skipped_long_lines: u64,
    /// A candump text log was cut at [`Limits::max_unpacked`] (or its ratio), or the caller stopped the read
    /// ([`read_log_while`]).
    pub truncated: bool,
}

enum Mode {
    Detect(Vec<u8>),
    Text,
    Columns(Vec<u8>),
}

/// Receives the unpacked bytes: finds the format from the first bytes, splits text into lines as they come.
struct Sink<'f, F: FnMut(&[u8]) -> bool> {
    mode: Mode,
    limits: Limits,
    partial: Vec<u8>,
    skipping: bool,
    total: u64,
    truncated: bool,
    lines: u64,
    skipped: u64,
    on_line: &'f mut F,
}

impl<F: FnMut(&[u8]) -> bool> Sink<'_, F> {
    /// Text, up to the unpack limit: past it the rest is dropped and unpacking stops.
    fn text_limited(&mut self, buf: &[u8]) -> io::Result<()> {
        if self.truncated {
            return Err(io::Error::other(Stop));
        }
        let room = self.limits.max_unpacked.saturating_sub(self.total);
        if buf.len() as u64 > room {
            self.truncated = true;
            self.text(&buf[..room as usize]);
            self.total += room;
            return Err(io::Error::other(Stop));
        }
        self.total += buf.len() as u64;
        self.text(buf);
        if self.truncated {
            return Err(io::Error::other(Stop));
        }
        Ok(())
    }

    fn text(&mut self, mut buf: &[u8]) {
        while let Some(nl) = buf.iter().position(|&c| c == b'\n') {
            if self.truncated {
                return;
            }
            let piece = &buf[..nl];
            buf = &buf[nl + 1..];
            if self.skipping || self.partial.len() + piece.len() > self.limits.max_line {
                self.skipped += 1;
            } else if self.partial.is_empty() {
                self.line(piece);
            } else {
                let mut l = std::mem::take(&mut self.partial);
                l.extend_from_slice(piece);
                self.line(&l);
                l.clear();
                self.partial = l;
            }
            self.partial.clear();
            self.skipping = false;
        }
        if self.skipping {
            return;
        }
        if self.partial.len() + buf.len() > self.limits.max_line {
            self.partial.clear();
            self.skipping = true;
        } else {
            self.partial.extend_from_slice(buf);
        }
    }

    fn line(&mut self, l: &[u8]) {
        let l = l.strip_suffix(b"\r").unwrap_or(l);
        if l.is_empty() || self.truncated {
            return;
        }
        self.lines += 1;
        if !(self.on_line)(l) {
            // the caller has what it needs: unpacking stops
            self.truncated = true;
        }
    }

    /// The first bytes decide: the columns magic, or text.
    fn decide(&mut self, head: Vec<u8>) -> io::Result<()> {
        if columns::is_columns(&head) {
            if head.len() as u64 > self.limits.max_columns_body {
                return Err(io::Error::other(Error::ColumnsTooLarge(self.limits.max_columns_body)));
            }
            self.mode = Mode::Columns(head);
            Ok(())
        } else {
            self.mode = Mode::Text;
            self.text_limited(&head)
        }
    }
}

impl<F: FnMut(&[u8]) -> bool> Write for Sink<'_, F> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match &mut self.mode {
            Mode::Detect(head) => {
                head.extend_from_slice(buf);
                if head.len() >= columns::MAGIC.len() || !columns::MAGIC.starts_with(head) {
                    let head = std::mem::take(head);
                    self.decide(head)?;
                }
            }
            Mode::Text => self.text_limited(buf)?,
            Mode::Columns(body) => {
                if (body.len() + buf.len()) as u64 > self.limits.max_columns_body {
                    return Err(io::Error::other(Error::ColumnsTooLarge(self.limits.max_columns_body)));
                }
                body.extend_from_slice(buf);
            }
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Debug)]
struct Stop;
impl std::fmt::Display for Stop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("stopped at the unpack limit")
    }
}
impl std::error::Error for Stop {}

/// Reads one `.log.xz` (either format) and calls `on_line` with every line, without its LF, in order.
pub fn read_log(xz: &[u8], limits: Limits, mut on_line: impl FnMut(&[u8])) -> Result<Summary, Error> {
    read_log_while(xz, limits, |l| {
        on_line(l);
        true
    })
}

/// [`read_log`], stopping (unpacking no further) once `on_line` returns false; the summary is then `truncated`.
pub fn read_log_while(xz: &[u8], limits: Limits, mut on_line: impl FnMut(&[u8]) -> bool) -> Result<Summary, Error> {
    let limits = limits.for_packed(xz.len());
    let mut sink = Sink {
        mode: Mode::Detect(Vec::with_capacity(32)),
        limits,
        partial: Vec::new(),
        skipping: false,
        total: 0,
        truncated: false,
        lines: 0,
        skipped: 0,
        on_line: &mut on_line,
    };
    let mut input = io::BufReader::new(xz);
    if let Err(e) = lzma_rs::xz_decompress(&mut input, &mut sink) {
        let stopped = matches!(&e, lzma_rs::error::Error::IoError(io) if io.get_ref().is_some_and(|i| i.is::<Stop>()));
        if !stopped {
            if let lzma_rs::error::Error::IoError(io) = &e
                && io.get_ref().is_some_and(|i| i.is::<Error>())
            {
                return Err(Error::ColumnsTooLarge(limits.max_columns_body));
            }
            return Err(Error::Xz(e.to_string()));
        }
    }
    // a body shorter than the magic
    if let Mode::Detect(head) = &mut sink.mode {
        let head = std::mem::take(head);
        let _ = sink.decide(head);
    }
    let format = match std::mem::replace(&mut sink.mode, Mode::Text) {
        Mode::Columns(body) => {
            let max = limits.max_line;
            let (mut lines, mut skipped) = (0u64, 0u64);
            let whole = columns::decode_until(&body, limits.max_frames, |l| {
                if l.len() > max {
                    skipped += 1;
                } else if !l.is_empty() {
                    lines += 1;
                    return on_line(l);
                }
                true
            })?;
            return Ok(Summary { format: Format::Columns, lines, skipped_long_lines: skipped, truncated: !whole });
        }
        _ => Format::Candump,
    };
    if !sink.skipping && !sink.partial.is_empty() && !sink.truncated {
        let rest = std::mem::take(&mut sink.partial);
        sink.line(&rest);
    }
    Ok(Summary { format, lines: sink.lines, skipped_long_lines: sink.skipped, truncated: sink.truncated })
}

/// Unpacks a columns or candump log to its candump text (LF after each line), as SMO's reference decoder does.
pub fn to_candump(xz: &[u8], limits: Limits) -> Result<(Vec<u8>, Summary), Error> {
    let mut out = Vec::new();
    let s = read_log(xz, limits, |l| {
        out.extend_from_slice(l);
        out.push(b'\n');
    })?;
    Ok((out, s))
}
