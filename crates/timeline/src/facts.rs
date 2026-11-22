//! What one log says about the car being reachable, besides its signals: when its frames came and on which buses,
//! why the bridge closed it, recording pauses before it, and frames it lost (SMO `docs/sentry-manager-format.md`).
//!
//! These are what the timeline's states come from: frames mean online; a log closed `quiet` (60 s without frames)
//! means the car went to sleep at its last frame; `# not recorded` means the bridge wasn't recording (unknown, never
//! sleep); no log at all means unknown.

use serde::{Deserialize, Serialize};

/// Why the bridge closed a log (the sidecar's `closed_because`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Closed {
    /// A minute of frames: the next log follows at once.
    Rotate,
    /// 60 s without frames: the car is asleep.
    Quiet,
    /// The bridge stopped.
    Stop,
    /// Over 48 MB in memory: the next log follows at once.
    Size,
    /// Closed early before an app update or a shutdown.
    Flush,
    /// The sidecar didn't say, or said something else.
    #[default]
    Unknown,
}

impl Closed {
    pub fn parse(s: &str) -> Closed {
        match s.trim() {
            "rotate" => Closed::Rotate,
            "quiet" => Closed::Quiet,
            "stop" => Closed::Stop,
            "size" => Closed::Size,
            "flush" => Closed::Flush,
            _ => Closed::Unknown,
        }
    }
}

/// A `# not recorded: paused <s> s …` line: the bridge was not recording for that long before `at_ms`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NotRecorded {
    /// When recording started again: the log's start for the line at its top (else the frame after the line).
    pub at_ms: u64,
    /// How long it paused; `None` when the line didn't say (then everything back to the log before is unknown).
    pub paused_ms: Option<u64>,
}

/// One log, as the timeline needs it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub struct LogFacts {
    /// The sidecar's `start_wall_ms` and `end_wall_ms` (else the first and last frame).
    pub start_ms: u64,
    pub end_ms: u64,
    /// The first and last frame (ms; queued frames can come up to a few seconds before `start_ms`).
    pub first_frame_ms: Option<u64>,
    pub last_frame_ms: Option<u64>,
    pub closed: Closed,
    /// The interfaces that sent frames (`can0`, `can1`, `can3` ...), sorted.
    pub buses: Vec<String>,
    pub not_recorded: Vec<NotRecorded>,
    /// How many `# dropped` lines: places where frames were lost.
    pub dropped: u32,
}

impl LogFacts {
    pub fn new(start_ms: u64, end_ms: u64, closed: Closed) -> LogFacts {
        LogFacts { start_ms, end_ms, closed, ..LogFacts::default() }
    }

    /// Start, end and why it was closed, from the sidecar (the rest comes from the log's lines).
    pub fn from_sidecar(side: &serde_json::Value) -> LogFacts {
        let ms = |k: &str| side.get(k).and_then(|v| v.as_u64().or_else(|| v.as_f64().map(|f| f.max(0.0) as u64)));
        let closed = side.get("closed_because").and_then(|v| v.as_str()).map_or(Closed::Unknown, Closed::parse);
        LogFacts::new(ms("start_wall_ms").unwrap_or(0), ms("end_wall_ms").unwrap_or(0), closed)
    }

    /// The span of the log's frames, when it has some.
    pub fn frames(&self) -> Option<(u64, u64)> {
        self.first_frame_ms.zip(self.last_frame_ms)
    }

    /// Where the log's data begins and ends: its frames and its own start and end together.
    pub fn span(&self) -> (u64, u64) {
        let (a, b) = self.frames().map_or((self.start_ms, self.end_ms), |(f, l)| {
            (if self.start_ms == 0 { f } else { f.min(self.start_ms) }, l.max(self.end_ms))
        });
        (a, b.max(a))
    }
}

/// The most `# not recorded` lines kept from one log. The bridge writes one, at the top of a file; a crafted log with
/// millions would otherwise grow the facts (and every timeline built from them) without bound.
pub const MAX_NOT_RECORDED: usize = 64;

/// Reads a log's lines into its [`LogFacts`].
#[derive(Debug, Clone)]
pub struct FactsCollector {
    facts: LogFacts,
    /// Interfaces seen, as a bit set (can0 … can255).
    buses: [u64; 4],
    /// `# not recorded` lines waiting for the next frame's time.
    pending: Vec<usize>,
}

impl FactsCollector {
    /// Starts from what the sidecar says ([`LogFacts::from_sidecar`] or [`LogFacts::new`]).
    pub fn new(facts: LogFacts) -> FactsCollector {
        FactsCollector { facts, buses: [0; 4], pending: Vec::new() }
    }

    pub fn line(&mut self, line: &logfmt::Line<'_>) {
        match line {
            logfmt::Line::Frame(f) => {
                let ms = f.time_us / 1000;
                let first = self.facts.first_frame_ms.is_none();
                for i in self.pending.drain(..) {
                    if let Some(n) = self.facts.not_recorded.get_mut(i) {
                        n.at_ms = if first && self.facts.start_ms > 0 { self.facts.start_ms } else { ms };
                    }
                }
                self.facts.first_frame_ms = Some(self.facts.first_frame_ms.map_or(ms, |m| m.min(ms)));
                self.facts.last_frame_ms = Some(self.facts.last_frame_ms.map_or(ms, |m| m.max(ms)));
                self.buses[usize::from(f.bus >> 6)] |= 1 << (f.bus & 63);
            }
            logfmt::Line::Dropped(_) => self.facts.dropped = self.facts.dropped.saturating_add(1),
            logfmt::Line::NotRecorded { paused_s, .. } if self.facts.not_recorded.len() < MAX_NOT_RECORDED => {
                self.pending.push(self.facts.not_recorded.len());
                self.facts.not_recorded.push(NotRecorded {
                    at_ms: self.facts.start_ms,
                    paused_ms: paused_s.map(|s| s.saturating_mul(1000)),
                });
            }
            _ => {}
        }
    }

    pub fn finish(mut self) -> LogFacts {
        let f = &mut self.facts;
        // a line after the last frame: recording started again at the log's end
        for i in self.pending.drain(..) {
            if let Some(n) = f.not_recorded.get_mut(i) {
                n.at_ms = f.last_frame_ms.unwrap_or(f.start_ms).max(n.at_ms);
            }
        }
        if f.start_ms == 0 {
            f.start_ms = f.first_frame_ms.unwrap_or(0);
        }
        if f.end_ms == 0 {
            f.end_ms = f.last_frame_ms.unwrap_or(f.start_ms);
        }
        f.end_ms = f.end_ms.max(f.start_ms);
        f.buses = (0..=255u8)
            .filter(|b| (self.buses[usize::from(b >> 6)] >> (b & 63)) & 1 == 1)
            .map(|b| format!("can{b}"))
            .collect();
        self.facts
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
    use super::*;

    fn lines(text: &str, side: serde_json::Value) -> LogFacts {
        let mut c = FactsCollector::new(LogFacts::from_sidecar(&side));
        for l in text.lines() {
            c.line(&logfmt::Line::parse(l.as_bytes()));
        }
        c.finish()
    }

    #[test]
    fn frames_buses_drops_and_pauses() {
        let f = lines(
            "# canbridge v1 app=0.2.150\n# not recorded: paused 120 s, about 3000 KB of frames (folder full)\n\
             # clock wall=1767225600.000000 mono_ns=1000\n(1767225598.500000) can0 132#0000000000000000\n\
             (1767225630.250000) can3 04F#0000000000000000\n# dropped 3 chunks of frames: logger fell behind\n\
             (1767225659.900000) can1 3F3#000000\n# clock wall=1767225660.000000 mono_ns=60000001000 end\n",
            serde_json::json!({ "start_wall_ms": 1_767_225_600_000u64, "end_wall_ms": 1_767_225_660_000u64, "closed_because": "rotate" }),
        );
        assert_eq!(f.closed, Closed::Rotate);
        assert_eq!((f.start_ms, f.end_ms), (1_767_225_600_000, 1_767_225_660_000));
        assert_eq!(f.frames(), Some((1_767_225_598_500, 1_767_225_659_900)), "queued frames come before the start");
        assert_eq!(f.buses, vec!["can0", "can1", "can3"]);
        assert_eq!(f.dropped, 1);
        assert_eq!(f.not_recorded, vec![NotRecorded { at_ms: 1_767_225_600_000, paused_ms: Some(120_000) }]);
        assert_eq!(f.span(), (1_767_225_598_500, 1_767_225_660_000));
    }

    #[test]
    fn a_flood_of_pause_lines_is_bounded() {
        let mut text = String::from("(1767225600.100000) can0 132#00\n");
        for _ in 0..10_000 {
            text += "# not recorded: paused 5 s, about 1 KB of frames (folder full)\n";
        }
        let f = lines(&text, serde_json::json!({ "start_wall_ms": 1_767_225_600_000u64 }));
        assert_eq!(f.not_recorded.len(), MAX_NOT_RECORDED);
    }

    #[test]
    fn a_sidecar_without_times_uses_the_frames() {
        let f = lines(
            "(1767225600.100000) can0 132#00\n(1767225610.100000) can0 132#00\n",
            serde_json::json!({ "closed_because": "quiet" }),
        );
        assert_eq!(f.closed, Closed::Quiet);
        assert_eq!((f.start_ms, f.end_ms), (1_767_225_600_100, 1_767_225_610_100));
        let unknown =
            lines("", serde_json::json!({ "closed_because": "exploded", "start_wall_ms": 5, "end_wall_ms": 1 }));
        assert_eq!(unknown.closed, Closed::Unknown);
        assert_eq!((unknown.start_ms, unknown.end_ms, unknown.frames()), (5, 5, None));
    }
}
