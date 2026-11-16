//! One line of a CAN Bridge log: a candump `-L` frame or a `#` comment.

/// A CAN frame as the bridge recorded it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// Microseconds since 1970 (the helper's receive time, wall clock anchored once per helper run).
    pub time_us: u64,
    /// Interface number: `can0` is 0.
    pub bus: u8,
    pub id: u32,
    /// Written with 8 hex digits (29-bit id) rather than 3.
    pub extended: bool,
    pub len: u8,
    pub data: [u8; 64],
}

impl Frame {
    pub fn payload(&self) -> &[u8] {
        &self.data[..usize::from(self.len)]
    }
}

/// What a `# clock` line marks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockKind {
    /// File start or the once-a-minute pairing.
    Regular,
    /// The bridge saw the wall clock move against the monotonic clock (false ones in build 147 and earlier).
    Jump,
    /// File end.
    End,
}

/// The helper's own clock line: lets a wrong wall clock at the anchor be corrected afterwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HelperClock {
    pub anchor_wall_us: i64,
    pub anchor_mono_ns: i64,
    pub now_wall_us: i64,
    pub now_mono_ns: i64,
}

impl HelperClock {
    /// The monotonic time (ns) of a frame of this helper run.
    pub fn mono_ns_of(&self, frame_us: u64) -> i128 {
        i128::from(self.anchor_mono_ns)
            + (i128::from(frame_us) - i128::from(self.anchor_wall_us)) * 1000
    }

    /// A frame's time by this line's later, possibly corrected, wall clock (spec: `true_us = W + (mono_ns - N) / 1000`).
    pub fn true_us(&self, frame_us: u64) -> i128 {
        i128::from(self.now_wall_us)
            + (self.mono_ns_of(frame_us) - i128::from(self.now_mono_ns)) / 1000
    }

    /// How far the current wall clock is from the anchored one at this line (µs; positive: the anchor was behind).
    pub fn drift_us(&self) -> i128 {
        let anchored = i128::from(self.anchor_wall_us)
            + (i128::from(self.now_mono_ns) - i128::from(self.anchor_mono_ns)) / 1000;
        i128::from(self.now_wall_us) - anchored
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Line<'a> {
    Frame(Frame),
    /// `# canbridge v1 …`
    Header(&'a str),
    Clock {
        wall_us: i64,
        mono_ns: i64,
        kind: ClockKind,
    },
    HelperClock(HelperClock),
    /// `# dropped …`: frames were lost here.
    Dropped(&'a str),
    /// `# not recorded: paused <s> s, …`: recording was paused before this point (a real gap).
    NotRecorded {
        paused_s: Option<u64>,
        text: &'a str,
    },
    /// `# rolled over …` (two servers: old logs removed on the bridge).
    RolledOver(&'a str),
    /// Any other comment: ignored, kept for display.
    Comment(&'a str),
    /// Not a frame in the exact form and not a comment.
    Other(&'a [u8]),
}

fn hex_val(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'A'..=b'F' => Some(c - b'A' + 10),
        b'a'..=b'f' => Some(c - b'a' + 10),
        _ => None,
    }
}

fn digits(s: &[u8]) -> Option<u64> {
    if s.is_empty() || s.len() > 19 || !s.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(s).ok()?.parse().ok()
}

/// `(<sec>.<usec>) can<N> <ID>#<HEX>`
fn parse_frame(l: &[u8]) -> Option<Frame> {
    let rest = l.strip_prefix(b"(")?;
    let close = rest.iter().position(|&c| c == b')')?;
    let (time, rest) = rest.split_at(close);
    let dot = time.iter().position(|&c| c == b'.')?;
    let (sec, usec) = (&time[..dot], &time[dot + 1..]);
    if usec.len() != 6 {
        return None;
    }
    let time_us = digits(sec)?
        .checked_mul(1_000_000)?
        .checked_add(digits(usec)?)?;
    let rest = rest.strip_prefix(b") ")?;
    let sp = rest.iter().position(|&c| c == b' ')?;
    let (iface, rest) = rest.split_at(sp);
    let bus = u8::try_from(digits(iface.strip_prefix(b"can")?)?).ok()?;
    let rest = &rest[1..];
    let hash = rest.iter().position(|&c| c == b'#')?;
    let (id_hex, data_hex) = (&rest[..hash], &rest[hash + 1..]);
    let extended = match id_hex.len() {
        3 => false,
        8 => true,
        _ => return None,
    };
    let mut id = 0u32;
    for &c in id_hex {
        id = (id << 4) | u32::from(hex_val(c)?);
    }
    if data_hex.len() % 2 != 0 || data_hex.len() > 128 {
        return None;
    }
    let mut data = [0u8; 64];
    for (i, pair) in data_hex.chunks(2).enumerate() {
        data[i] = (hex_val(pair[0])? << 4) | hex_val(pair[1])?;
    }
    Some(Frame {
        time_us,
        bus,
        id,
        extended,
        len: (data_hex.len() / 2) as u8,
        data,
    })
}

/// `key=value` fields of a comment, as numbers.
fn field(text: &str, key: &str) -> Option<i64> {
    text.split_whitespace()
        .find_map(|w| w.strip_prefix(key)?.strip_prefix('=')?.parse().ok())
}

/// `wall=1794099498.083000` as microseconds.
fn wall_us(text: &str) -> Option<i64> {
    let w = text
        .split_whitespace()
        .find_map(|w| w.strip_prefix("wall="))?;
    let (s, us) = w.split_once('.')?;
    if us.len() != 6 {
        return None;
    }
    s.parse::<i64>()
        .ok()?
        .checked_mul(1_000_000)?
        .checked_add(us.parse().ok()?)
}

impl<'a> Line<'a> {
    pub fn parse(l: &'a [u8]) -> Line<'a> {
        if l.first() == Some(&b'(') {
            return parse_frame(l).map_or(Line::Other(l), Line::Frame);
        }
        let Some(text) = l
            .strip_prefix(b"# ")
            .and_then(|t| std::str::from_utf8(t).ok())
        else {
            return if l.first() == Some(&b'#') {
                std::str::from_utf8(&l[1..]).map_or(Line::Other(l), |t| Line::Comment(t.trim()))
            } else {
                Line::Other(l)
            };
        };
        if text.starts_with("canbridge ") {
            Line::Header(text)
        } else if let Some(rest) = text.strip_prefix("clock ") {
            match (wall_us(rest), field(rest, "mono_ns")) {
                (Some(wall_us), Some(mono_ns)) => {
                    let kind = match rest.split_whitespace().last() {
                        Some("jump") => ClockKind::Jump,
                        Some("end") => ClockKind::End,
                        _ => ClockKind::Regular,
                    };
                    Line::Clock {
                        wall_us,
                        mono_ns,
                        kind,
                    }
                }
                _ => Line::Comment(text),
            }
        } else if let Some(rest) = text.strip_prefix("helper-clock ") {
            match (
                field(rest, "anchor_wall_us"),
                field(rest, "anchor_mono_ns"),
                field(rest, "now_wall_us"),
                field(rest, "now_mono_ns"),
            ) {
                (
                    Some(anchor_wall_us),
                    Some(anchor_mono_ns),
                    Some(now_wall_us),
                    Some(now_mono_ns),
                ) => Line::HelperClock(HelperClock {
                    anchor_wall_us,
                    anchor_mono_ns,
                    now_wall_us,
                    now_mono_ns,
                }),
                _ => Line::Comment(text),
            }
        } else if text.starts_with("dropped") {
            Line::Dropped(text)
        } else if let Some(rest) = text.strip_prefix("not recorded: paused ") {
            Line::NotRecorded {
                paused_s: rest.split_whitespace().next().and_then(|n| n.parse().ok()),
                text,
            }
        } else if text.starts_with("rolled over") {
            Line::RolledOver(text)
        } else {
            Line::Comment(text)
        }
    }
}
