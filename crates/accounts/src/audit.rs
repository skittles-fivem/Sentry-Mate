//! The audit log: who did what, when and from where (sign-ins, account changes, and the other parts' trust, token,
//! delete and setting changes). Append-only and sealed; never holds a password, token or code.
//!
//! ```text
//! <dir>/<yyyy-mm>.<nnnn>.log    one part: sealed records (vault::records), one JSON line per entry
//! ```
//!
//! A new part starts each month (UTC) and whenever the current one reaches [`PART_ENTRIES`] entries, so no file ever
//! becomes huge and reading a page costs at most a part or two, however long the server has run. Listing goes
//! newest first, a page at a time, with a cursor (`<part>:<entry>`) for the next page.

use crate::error::{Error, Result, corrupt};
use crate::journal::Journal;
use crate::{Clock, one_line};
use std::collections::VecDeque;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use vault::Vault;

/// Entries per part at most (about 200 bytes each: a part stays around 2 MB).
pub const PART_ENTRIES: u64 = 10_000;
/// The most entries one page returns.
pub const MAX_PAGE: usize = 1000;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Entry {
    /// When, ms since 1970 UTC.
    pub t_ms: u64,
    /// The account that did it (its name), if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    /// The client's address, if it came from a request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    /// What was done, e.g. `sign-in`, `account-created`, `token-revoked`.
    pub action: String,
    /// What it was done to and how, in words.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub details: String,
}

/// A page of entries, newest first; `next` is the cursor for the page after it (`None`: that was the oldest).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Page {
    pub entries: Vec<Entry>,
    pub next: Option<String>,
}

struct Current {
    stem: String,
    journal: Journal,
}

pub struct Audit {
    dir: PathBuf,
    vault: Arc<Vault>,
    clock: Clock,
    current: Mutex<Option<Current>>,
}

/// Year and month (UTC) of a time (Howard Hinnant's civil_from_days).
pub(crate) fn year_month(t_ms: u64) -> (i64, u32) {
    let z = (t_ms / 86_400_000) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m)
}

fn month_of(t_ms: u64) -> String {
    let (y, m) = year_month(t_ms);
    format!("{y:04}-{m:02}")
}

/// `yyyy-mm.nnnn`.
fn valid_stem(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 12
        && b[4] == b'-'
        && b[7] == b'.'
        && b.iter().enumerate().all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
}

fn label(stem: &str) -> String {
    format!("audit/{stem}")
}

impl Audit {
    /// Opens the audit log kept in `dir` (made when needed). One `Audit` per folder at a time: the server's parts share
    /// it with an `Arc`.
    pub fn open(dir: impl Into<PathBuf>, vault: Arc<Vault>, clock: Clock) -> Result<Audit> {
        let dir = dir.into();
        std::fs::create_dir_all(&dir)?;
        Ok(Audit { dir, vault, clock, current: Mutex::new(None) })
    }

    fn path(&self, stem: &str) -> PathBuf {
        self.dir.join(format!("{stem}.log"))
    }

    /// The parts there are, oldest first.
    fn parts(&self) -> Result<Vec<String>> {
        let mut out = Vec::new();
        for e in std::fs::read_dir(&self.dir)? {
            let name = e?.file_name();
            if let Some(stem) = name.to_str().and_then(|n| n.strip_suffix(".log")).filter(|s| valid_stem(s)) {
                out.push(stem.to_string());
            }
        }
        out.sort_unstable();
        Ok(out)
    }

    fn open_part(&self, stem: &str) -> Result<Journal> {
        Journal::open(&self.vault, &self.path(stem), &label(stem), |_| {})
    }

    /// Adds an entry (synced before it returns). Text is cut to one line of a sensible length.
    pub fn record(&self, account: Option<&str>, address: Option<IpAddr>, action: &str, details: &str) -> Result<()> {
        let t_ms = (self.clock)();
        let entry = Entry {
            t_ms,
            account: account.map(|a| one_line(a, 80)),
            address: address.map(|a| crate::net::canonical(a).to_string()),
            action: one_line(action, 64),
            details: one_line(details, 2000),
        };
        let month = month_of(t_ms);
        let mut cur = self.current.lock().unwrap_or_else(|e| e.into_inner());
        let stale = match &*cur {
            Some(c) => !c.stem.starts_with(&month) || c.journal.lines() >= PART_ENTRIES,
            None => true,
        };
        if stale {
            // carry on in this month's newest part while it has room, else start the next one
            let newest = self.parts()?.into_iter().rev().find(|s| s.starts_with(&month));
            let mut next = format!("{month}.0001");
            if let Some(stem) = newest {
                let j = self.open_part(&stem)?;
                if j.lines() < PART_ENTRIES {
                    *cur = Some(Current { stem, journal: j });
                    next.clear();
                } else {
                    let n: u32 = stem[8..].parse().map_err(corrupt)?;
                    next = format!("{month}.{:04}", n + 1);
                }
            }
            if !next.is_empty() {
                let j = self.open_part(&next)?;
                *cur = Some(Current { stem: next, journal: j });
            }
        }
        match cur.as_mut() {
            Some(c) => c.journal.append(std::slice::from_ref(&entry)),
            None => Err(corrupt("no audit part")),
        }
    }

    /// A page of entries, newest first: `limit` of them (at most [`MAX_PAGE`]) from where `cursor` (a page's
    /// `next`) says, or from the newest.
    pub fn list(&self, limit: usize, cursor: Option<&str>) -> Result<Page> {
        let limit = limit.clamp(1, MAX_PAGE);
        let (start_stem, mut end) = match cursor {
            Some(c) => {
                let (s, i) = c.split_once(':').ok_or(Error::BadInput("That is not an audit cursor."))?;
                let i: u64 = i.parse().map_err(|_| Error::BadInput("That is not an audit cursor."))?;
                if !valid_stem(s) {
                    return Err(Error::BadInput("That is not an audit cursor."));
                }
                (Some(s.to_string()), Some(i))
            }
            None => (None, None),
        };
        let parts = self.parts()?;
        let mut out = Vec::with_capacity(limit);
        for stem in parts.iter().rev().filter(|s| start_stem.as_ref().is_none_or(|start| *s <= start)) {
            // entries before `end` in this part (all of them for older parts)
            let before = if start_stem.as_deref() == Some(stem.as_str()) { end.take() } else { None };
            if before == Some(0) {
                continue;
            }
            // keep only what this page needs from this part, as it is read: the newest `limit - out.len()` entries
            // before `before`
            let need = limit - out.len();
            let mut tail: VecDeque<(u64, Entry)> = VecDeque::with_capacity(need);
            let mut i = 0u64;
            self.read_part(stem, |e| {
                if before.is_none_or(|b| i < b) {
                    if tail.len() == need {
                        tail.pop_front();
                    }
                    tail.push_back((i, e));
                }
                i += 1;
            })?;
            let first_taken = tail.front().map(|(i, _)| *i);
            out.extend(tail.into_iter().rev().map(|(_, e)| e));
            if out.len() == limit {
                let at = first_taken.unwrap_or(0);
                let older = at > 0 || parts.iter().any(|s| s < stem);
                return Ok(Page { entries: out, next: older.then(|| format!("{stem}:{at}")) });
            }
        }
        Ok(Page { entries: out, next: None })
    }

    /// Hands each entry of a part to `each`, oldest first.
    fn read_part(&self, stem: &str, mut each: impl FnMut(Entry)) -> Result<()> {
        let mut keep = |line: &[u8]| {
            if let Ok(e) = serde_json::from_slice::<Entry>(line) {
                each(e);
            }
        };
        // reads happen under the writer's lock: opening a part trims a record cut short by a crash, which must never
        // meet an append in progress
        let mut cur = self.current.lock().unwrap_or_else(|e| e.into_inner());
        match cur.as_mut().filter(|c| c.stem == stem) {
            Some(c) => c.journal.read(&mut keep),
            None => Journal::open(&self.vault, &self.path(stem), &label(stem), &mut keep).map(drop),
        }
    }

    /// Where the log is kept.
    pub fn dir(&self) -> &Path {
        &self.dir
    }
}
