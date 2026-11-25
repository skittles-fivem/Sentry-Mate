//! The move's log for the owner: what a move did, step by step, kept with the move's state on each server (the old
//! server's in `.move/log.sealed`, the new server's in its staged state and then in its moved-in note), the newest
//! [`MAX_LINES`] lines. The web pages show it as it grows and after a reload (`docs/ui-api.md`, "The move's log").
//!
//! Nothing secret goes in: no code, key, token, session or file path. A file is named by its kind ([`kind_of`]),
//! the other server by the address the page shows already.

use serde::{Deserialize, Serialize};

/// The lines kept (the oldest go first).
pub const MAX_LINES: usize = 500;

/// One line: when, and what happened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Line {
    pub at_ms: u64,
    pub text: String,
}

/// Adds a line at `at_ms`, dropping the oldest beyond [`MAX_LINES`].
pub fn push(log: &mut Vec<Line>, at_ms: u64, text: impl Into<String>) {
    log.push(Line { at_ms, text: text.into() });
    if log.len() > MAX_LINES {
        let over = log.len() - MAX_LINES;
        log.drain(..over);
    }
}

/// The log as plain text, one line each: `2026-11-17 14:02:05 UTC  text` (the Download button's file).
pub fn plain(log: &[Line]) -> String {
    let mut s = String::new();
    for l in log {
        s.push_str(&stamp(l.at_ms));
        s.push_str("  ");
        s.push_str(&l.text);
        s.push('\n');
    }
    s
}

/// `2026-11-17 14:02:05 UTC`.
pub fn stamp(ms: u64) -> String {
    let secs = ms / 1000;
    let (h, m, s) = ((secs / 3600) % 24, (secs / 60) % 60, secs % 60);
    let (y, mo, d) = civil(secs / 86_400);
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{m:02}:{s:02} UTC")
}

/// Days since 1970 to (year, month, day) (Howard Hinnant's `civil_from_days`).
fn civil(days: u64) -> (u64, u64, u64) {
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y.max(0) as u64, m as u64, d as u64)
}

/// What a carried file is, in words, from its path under the data folder (the log names no path).
pub fn kind_of(path: &str) -> &'static str {
    let mut parts = path.split('/');
    let top = parts.next().unwrap_or("");
    let last = path.rsplit('/').next().unwrap_or("");
    match top {
        "can" => {
            if path.contains("/uploading/") {
                "an upload still arriving"
            } else if last.ends_with(".log.xz.json") || last.ends_with(".log.json") {
                "a CAN log's sidecar"
            } else if last.contains(".log") {
                "a CAN log"
            } else if last.starts_with("trust") {
                "a trust record"
            } else {
                "a CAN log list"
            }
        }
        "db" => "a part of the history database",
        "tokens" | "tokens.json" => "an upload token",
        "tls" => "the certificate",
        "accounts" => "the accounts",
        "audit" => "the audit log",
        "tesla" => "the Tesla API input's state",
        "teslafi" => "the TeslaFi input's state",
        "teslamate-local" => "the local TeslaMate input's state",
        "maps" => "an offline map",
        "geonames" => "the offline addresses",
        "web" if path.starts_with("web/imports/") => "an import's upload",
        "web" if path.starts_with("web/packs/") => "a UI pack",
        "web" => "a web setting",
        _ => "a data file",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn at_most_the_newest_lines_are_kept() {
        let mut l = Vec::new();
        for i in 0..(MAX_LINES as u64 + 20) {
            push(&mut l, i, format!("line {i}"));
        }
        assert_eq!(l.len(), MAX_LINES);
        assert_eq!(l[0].text, "line 20");
        assert_eq!(l.last().unwrap().text, format!("line {}", MAX_LINES + 19));
    }

    #[test]
    fn plain_text_has_a_time_on_every_line() {
        let l = vec![Line { at_ms: 1_763_388_125_000, text: "connected".into() }];
        assert_eq!(plain(&l), "2025-11-17 14:02:05 UTC  connected\n");
        assert_eq!(stamp(0), "1970-01-01 00:00:00 UTC");
        assert_eq!(stamp(951_782_400_000), "2000-02-29 00:00:00 UTC");
    }

    #[test]
    fn a_file_is_named_by_its_kind_never_its_path() {
        assert_eq!(kind_of("can/abc123/abc123_20261108T143000Z_1.log.xz"), "a CAN log");
        assert_eq!(kind_of("can/abc123/abc123_20261108T143000Z_1.log.xz.json"), "a CAN log's sidecar");
        assert_eq!(kind_of("can/abc123/uploading/x.part"), "an upload still arriving");
        assert_eq!(kind_of("can/abc123/trust-3.records"), "a trust record");
        assert_eq!(kind_of("db/fleet.snap"), "a part of the history database");
        assert_eq!(kind_of("tokens/ab.json"), "an upload token");
        assert_eq!(kind_of("web/imports/7/backup"), "an import's upload");
        assert_eq!(kind_of("whatever"), "a data file");
    }
}
