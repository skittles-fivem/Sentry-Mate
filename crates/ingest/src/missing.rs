//! The logs the server should have but no longer does, so it can ask the CAN Bridge app to send them again.
//!
//! A log stays known after its file is gone: its trust record (with the SHA-256 and length it was stored with, and
//! whether it had a sidecar) and, for a signed log, the next log's chain link (`prev_name`, `prev_sha256`). The scan
//! ([`Judge::scan_missing`]) compares those with what is on disk, per car key:
//!
//! - a log with a record whose stored file is gone, or whose sidecar is gone while the record says it had one;
//! - a chain gap: a verified log names a previous log the server has no record or file of, while older logs of the
//!   same signing key are here (a chain received from its middle is not a gap: its first log's previous one was never
//!   sent here).
//!
//! What it finds is kept sealed per car key, with when it was found:
//!
//! ```text
//! <can>/<vehicleKey>/missing.json    the logs and sidecars missing on the server, and when it last looked
//!                                    (sealed; no file: never looked, none known)
//! ```
//!
//! `GET /api/can/logs/ping` lists them as `wanted` (at most [`WANTED_MAX`], oldest first) for the car its token is
//! tied to. A name leaves the list once it arrives again and is judged with the SHA-256 it was stored with (an upload
//! of a wanted name with other bytes is refused, `409 wanted_mismatch`, unless that SHA-256 was never known), when the
//! scan finds its file back, or when nothing names it any more. The app says which it no longer holds
//! (`not_held` of `POST /api/can/logs/status`): those stay listed for the owner (lost for good), and are no longer
//! asked for. See `docs/can-bridge-wanted.md`.
//!
//! A log "is here" when its stored form exists ([`crate::store::Store::exists`]: the finished file under its name);
//! [`Judge::verify_stored`] hashes it against its record when asked (not during the scan: the scan reads no log).
//! Every change is made with the car key held (its lock and lock file, as the trust records are), so the server, the
//! scan and an owner's command never write over each other's changes.

use crate::judge::{Judge, Record, Status, blocking};
use crate::ledger::{self, valid_key};
use crate::store::now_ms;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io;
use std::ops::ControlFlow;
use std::path::PathBuf;
use trust::LogName;

/// Names a ping's `wanted` holds at most.
pub const WANTED_MAX: usize = 500;
/// The file of a car key's missing logs.
pub const FILE: &str = "missing.json";
/// How often the server looks for missing logs (and once at start-up).
pub const SCAN_EVERY_MS: u64 = 24 * 3600 * 1000;

/// How a missing log is known.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Known {
    /// Its trust record: it was stored here.
    Record,
    /// Only from the next log's chain link: it never arrived here, or its record is gone too.
    Chain,
}

/// One log (or sidecar) missing on the server.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MissingLog {
    /// When the scan first found it missing.
    pub found_ms: u64,
    pub known: Known,
    /// The SHA-256 and length it was stored with (a chain gap: the SHA-256 the next log names), when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub length: Option<u64>,
    /// Its log's verdict when it was found missing (`None`: known from the chain only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict: Option<Status>,
    /// When a ping first asked the car for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asked_ms: Option<u64>,
    /// When the app last asked about it (`POST /api/can/logs/status`): it still holds it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub held_ms: Option<u64>,
    /// When the app said it no longer holds it either: lost for good, no longer asked for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gone_ms: Option<u64>,
}

/// A car key's missing logs.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Missing {
    pub v: u32,
    /// When the scan last looked at this key.
    #[serde(default)]
    pub scanned_ms: Option<u64>,
    /// By name (a log's, or a sidecar's `<log>.json`).
    #[serde(default)]
    pub logs: BTreeMap<String, MissingLog>,
}

/// What a scan of one key found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Scan {
    /// Names found missing by this scan that were not on the list before, oldest first.
    pub new: Vec<String>,
    /// On the list now and still asked for.
    pub wanted: usize,
    /// On the list now, and the car no longer has them either.
    pub gone: usize,
}

/// The counts the owner's pages show of a car key's missing logs.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct Counts {
    /// Still asked for.
    pub wanted: u64,
    /// The car no longer has them either.
    pub gone: u64,
    /// The start (from its name) of the oldest still asked for, and of the oldest gone.
    pub oldest_ms: Option<u64>,
    pub gone_oldest_ms: Option<u64>,
    /// When the first of those still asked for was found missing, and first asked for.
    pub found_ms: Option<u64>,
    pub asked_ms: Option<u64>,
    /// When the scan last looked.
    pub scanned_ms: Option<u64>,
}

impl Missing {
    /// The names still asked for, oldest first (by their start, then their sequence; a log before its sidecar).
    pub fn wanted(&self) -> Vec<String> {
        let mut v: Vec<&String> = self.logs.iter().filter(|(_, m)| m.gone_ms.is_none()).map(|(n, _)| n).collect();
        v.sort_by_key(|n| order(n));
        v.into_iter().take(WANTED_MAX).cloned().collect()
    }

    pub fn counts(&self) -> Counts {
        let mut c = Counts { scanned_ms: self.scanned_ms, ..Counts::default() };
        for (name, m) in &self.logs {
            let start = LogName::parse(name).and_then(|n| u64::try_from(n.started_ms).ok());
            let min = |a: Option<u64>, b: Option<u64>| match (a, b) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
            match m.gone_ms {
                Some(_) => {
                    c.gone += 1;
                    c.gone_oldest_ms = min(c.gone_oldest_ms, start);
                }
                None => {
                    c.wanted += 1;
                    c.oldest_ms = min(c.oldest_ms, start);
                    c.found_ms = min(c.found_ms, Some(m.found_ms));
                    c.asked_ms = min(c.asked_ms, m.asked_ms);
                }
            }
        }
        c
    }
}

/// A name's place in "oldest first".
fn order(name: &str) -> (i64, u32, bool, String) {
    match LogName::parse(name) {
        Some(n) => (n.started_ms, n.seq, n.sidecar, name.to_string()),
        None => (i64::MAX, u32::MAX, true, name.to_string()),
    }
}

fn other(e: impl std::fmt::Display) -> io::Error {
    io::Error::other(e.to_string())
}

fn bad_key() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, "not a CAN Bridge key")
}

/// One key's missing-logs file.
struct File {
    path: PathBuf,
    label: String,
    vault: std::sync::Arc<vault::Vault>,
}

impl File {
    fn read(&self) -> io::Result<Missing> {
        match self.vault.read_file(&self.path, &self.label).map_err(other)? {
            Some(b) => serde_json::from_slice(&b).map_err(other),
            None => Ok(Missing::default()),
        }
    }

    /// Writes it (sealed, whole or not at all); an empty list that was never scanned leaves no file.
    fn write(&self, m: &Missing) -> io::Result<()> {
        if m.logs.is_empty() && m.scanned_ms.is_none() {
            return ledger::remove_if_there(&self.path);
        }
        let mut m = m.clone();
        m.v = 1;
        let bytes = serde_json::to_vec(&m).map_err(other)?;
        ledger::write_sealed(&self.vault, &self.path, &self.label, |w| w.write_all(&bytes))
    }
}

impl Judge {
    fn missing_file(&self, key: &str) -> File {
        File {
            path: self.dir.join(key).join(FILE),
            label: format!("can/{key}/{FILE}"),
            vault: self.store.vault().clone(),
        }
    }

    /// Whether a car key may have missing logs at all (its file is there): a stat, for the upload path.
    fn may_miss(&self, key: &str) -> bool {
        valid_key(key) && self.dir.join(key).join(FILE).exists()
    }

    /// A car key's missing logs as last written (none: an empty list).
    pub async fn missing(&self, key: &str) -> io::Result<Missing> {
        if !valid_key(key) {
            return Err(bad_key());
        }
        if !self.may_miss(key) {
            return Ok(Missing::default());
        }
        let f = self.missing_file(key);
        blocking(move || f.read()).await
    }

    /// Changes a key's missing logs with the key held: `change` says whether it changed anything (only then is the
    /// file written).
    async fn change_missing(
        &self,
        key: &str,
        change: impl FnOnce(&mut Missing) -> bool + Send + 'static,
    ) -> io::Result<Missing> {
        let _held = self.hold(key).await?;
        let f = self.missing_file(key);
        blocking(move || {
            let mut m = f.read()?;
            if change(&mut m) {
                f.write(&m)?;
            }
            Ok(m)
        })
        .await
    }

    /// The names to put in a ping's `wanted` for a car key, oldest first, at most [`WANTED_MAX`]. The first time a
    /// name is asked for is written down.
    pub async fn wanted(&self, key: &str) -> io::Result<Vec<String>> {
        let m = self.missing(key).await?;
        let wanted = m.wanted();
        if wanted.iter().all(|n| m.logs.get(n).is_some_and(|l| l.asked_ms.is_some())) {
            return Ok(wanted);
        }
        let now = now_ms();
        let m = self
            .change_missing(key, move |m| {
                let mut changed = false;
                for n in m.wanted() {
                    if let Some(l) = m.logs.get_mut(&n).filter(|l| l.asked_ms.is_none()) {
                        l.asked_ms = Some(now);
                        changed = true;
                    }
                }
                changed
            })
            .await?;
        Ok(m.wanted())
    }

    /// What a status question told of a car key's missing logs: `asked` names the app holds (it asked about them),
    /// `not_held` names it no longer has. Nothing is read or written for a key with none missing.
    pub async fn told_status(&self, key: &str, asked: &[String], not_held: &[String]) -> io::Result<()> {
        if !self.may_miss(key) || (asked.is_empty() && not_held.is_empty()) {
            return Ok(());
        }
        let m = self.missing(key).await?;
        let held: Vec<String> = asked.iter().filter(|n| m.logs.contains_key(*n)).cloned().collect();
        let gone: Vec<String> =
            not_held.iter().filter(|n| m.logs.get(*n).is_some_and(|l| l.gone_ms.is_none())).cloned().collect();
        if held.is_empty() && gone.is_empty() {
            return Ok(());
        }
        let now = now_ms();
        self.change_missing(key, move |m| {
            let mut changed = false;
            for n in &held {
                if let Some(l) = m.logs.get_mut(n) {
                    // it still has it: asked for again even if it once said otherwise
                    l.held_ms = Some(now);
                    l.gone_ms = None;
                    changed = true;
                }
            }
            for n in &gone {
                if let Some(l) = m.logs.get_mut(n).filter(|l| l.gone_ms.is_none()) {
                    l.gone_ms = Some(now);
                    changed = true;
                }
            }
            changed
        })
        .await?;
        Ok(())
    }

    /// The SHA-256 an upload of `name` must have, when it is a wanted name whose original SHA-256 is known: an
    /// upload with another one is refused (`409 wanted_mismatch`). `None`: not wanted, or its SHA-256 never known.
    pub async fn wanted_sha256(&self, name: &str) -> Option<String> {
        let n = LogName::parse(name)?;
        let m = self.missing(n.vehicle_key).await.ok()?;
        m.logs.get(name).and_then(|l| l.sha256.clone())
    }

    /// Hashes a stored log (or sidecar) against what its record says it was stored with: `Some(true)` it matches,
    /// `Some(false)` it differs, `None` not here or nothing recorded to compare with.
    pub async fn verify_stored(&self, name: &str) -> io::Result<Option<bool>> {
        let Some(n) = LogName::parse(name) else { return Ok(None) };
        if !self.store.exists(name) {
            return Ok(None);
        }
        let log = name.strip_suffix(".json").unwrap_or(name);
        let Some(rec) = self.record(n.vehicle_key, log).await else { return Ok(None) };
        let want = if n.sidecar { rec.sidecar_sha256.clone() } else { rec.original().0.map(str::to_string) };
        let Some(want) = want else { return Ok(None) };
        let (sha, _) = self.store.hash(name).await.map_err(|e| other(e.to_string()))?;
        Ok(Some(sha.eq_ignore_ascii_case(&want)))
    }

    /// The car keys with records here, in name order.
    pub async fn keys(&self) -> io::Result<Vec<String>> {
        let dir = self.dir.clone();
        blocking(move || {
            let mut v = Vec::new();
            for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
                let Some(k) = e.file_name().to_str().filter(|k| valid_key(k)).map(str::to_string) else { continue };
                // any of its trust files (a key's first records are in its journal, before a head is written)
                let has_trust =
                    std::fs::read_dir(e.path()).into_iter().flatten().flatten().any(|f| {
                        f.file_name().to_str().is_some_and(|n| n.starts_with("trust") && n != ledger::LOCK_FILE)
                    });
                if has_trust {
                    v.push(k);
                }
            }
            v.sort_unstable();
            Ok(v)
        })
        .await
    }

    /// Looks for a car key's missing logs (see the module): one pass over its records, a stat per log and sidecar,
    /// no log read. `uploading`: its names still arriving (not a gap). The list written is what is missing now; a name
    /// that was on it keeps when it was found and asked for.
    pub async fn scan_missing(&self, key: &str, uploading: HashSet<String>) -> io::Result<Scan> {
        if !valid_key(key) {
            return Err(bad_key());
        }
        let held = self.hold(key).await?;
        let head = self.take_head(key).await?;
        let (ctx, h, store, f) = (self.ctx(), head.clone(), self.store.clone(), self.missing_file(key));
        let key_s = key.to_string();
        let now = now_ms();
        // a chain gap is one once the log that names it has been here as long as a log waits for its sidecar (the
        // one before may still be coming)
        let gap_after = self.wait_ms;
        let r = blocking(move || {
            let mut known: HashSet<Box<str>> = HashSet::new();
            let mut lowest: HashMap<String, u64> = HashMap::new();
            // (prev name, prev SHA-256, signing key, index, arrived) of each verified log that names one
            let mut links: Vec<(String, String, String, u64, u64)> = Vec::new();
            let mut found: BTreeMap<String, MissingLog> = BTreeMap::new();
            let entry = |r: &Record, sha: Option<String>, length: Option<u64>| MissingLog {
                found_ms: now,
                known: Known::Record,
                sha256: sha,
                length,
                verdict: Some(r.status.clone()),
                asked_ms: None,
                held_ms: None,
                gone_ms: None,
            };
            // (the key's records in memory when they are kept there)
            ctx.view(&key_s, &h, |v| {
                v.for_each(|name, r| {
                    known.insert(name.into());
                    if !store.exists(name) {
                        let (sha, len) = r.original();
                        found.insert(name.to_string(), entry(r, sha.map(str::to_string), len));
                    }
                    let side = format!("{name}.json");
                    if r.had_sidecar() && !store.exists(&side) {
                        found.insert(side, entry(r, r.sidecar_sha256.clone(), None));
                    }
                    if let Some(l) = r.link.as_ref() {
                        let e = lowest.entry(l.key_fingerprint.clone()).or_insert(l.index);
                        *e = (*e).min(l.index);
                        if r.status == Status::Verified && l.prev_name != "-" {
                            links.push((
                                l.prev_name.clone(),
                                l.prev_sha256.clone(),
                                l.key_fingerprint.clone(),
                                l.index,
                                r.arrived_ms,
                            ));
                        }
                    }
                    ControlFlow::Continue(())
                })
            })?;
            for (prev, prev_sha, fp, index, arrived_ms) in links {
                let ours = LogName::parse(&prev).is_some_and(|n| n.vehicle_key == key_s && !n.sidecar);
                let older_here = lowest.get(&fp).is_some_and(|&i| i < index);
                if !ours
                    || !older_here
                    || known.contains(prev.as_str())
                    || found.contains_key(&prev)
                    || uploading.contains(&prev)
                    || now.saturating_sub(arrived_ms) < gap_after
                    || store.exists(&prev)
                {
                    continue;
                }
                found.insert(
                    prev,
                    MissingLog {
                        found_ms: now,
                        known: Known::Chain,
                        sha256: Some(prev_sha.to_ascii_lowercase()),
                        length: None,
                        verdict: None,
                        asked_ms: None,
                        held_ms: None,
                        gone_ms: None,
                    },
                );
            }
            let before = f.read()?;
            let mut new: Vec<String> = Vec::new();
            for (name, m) in found.iter_mut() {
                match before.logs.get(name) {
                    Some(b) => {
                        m.found_ms = b.found_ms;
                        m.asked_ms = b.asked_ms;
                        m.held_ms = b.held_ms;
                        m.gone_ms = b.gone_ms;
                        // what it was stored with stays what it must match (a record judged again keeps it too)
                        if b.sha256.is_some() {
                            m.sha256 = b.sha256.clone();
                            m.length = b.length.or(m.length);
                        }
                        m.verdict = b.verdict.clone().or(m.verdict.take());
                    }
                    None => new.push(name.clone()),
                }
            }
            new.sort_by_key(|n| order(n));
            let after = Missing { v: 1, scanned_ms: Some(now), logs: found };
            let counts = after.counts();
            // (written when nothing is missing too: when the server last looked is shown)
            f.write(&after)?;
            Ok(Scan { new, wanted: counts.wanted as usize, gone: counts.gone as usize })
        })
        .await;
        self.put_head(key, head);
        drop(held);
        r
    }

    /// A log or sidecar finished arriving (the key held, its record just written when both are here): off the
    /// missing list once it is what was stored (its SHA-256, when known) and judged. A log that came back from a
    /// chain gap may name another one before it the server never had: that one is asked for next.
    pub(crate) async fn missing_arrived(&self, key: &str, name: &str, rec: Option<&Record>) -> io::Result<()> {
        if !self.may_miss(key) {
            return Ok(());
        }
        let f = self.missing_file(key);
        let (name, rec, store, key_s) = (name.to_string(), rec.cloned(), self.store.clone(), key.to_string());
        let log = name.strip_suffix(".json").unwrap_or(&name).to_string();
        // the log before a log back from the list, when nothing of it is here
        let prev = rec
            .as_ref()
            .and_then(|r| r.link.as_ref())
            .filter(|l| l.prev_name != "-")
            .filter(|l| LogName::parse(&l.prev_name).is_some_and(|n| n.vehicle_key == key_s && !n.sidecar))
            .map(|l| (l.prev_name.clone(), l.prev_sha256.to_ascii_lowercase()));
        let prev_known = match &prev {
            Some((p, _)) => {
                let (ledger, head) = (self.ledger(key), self.take_head(key).await?);
                let p2 = p.clone();
                let r = blocking(move || Ok((ledger.lookup(&head, &p2)?.is_some(), head))).await?;
                self.put_head(key, r.1);
                r.0
            }
            None => true,
        };
        blocking(move || {
            let mut m = f.read()?;
            let mut changed = false;
            let judged = rec.as_ref().filter(|r| r.status != Status::Waiting);
            // the names this arrival may settle: the file itself, and its log or sidecar (now judged together)
            let side = format!("{log}.json");
            for n in [log.as_str(), side.as_str()] {
                let Some(l) = m.logs.get(n) else { continue };
                if !store.exists(n) {
                    continue;
                }
                let back = if n == log {
                    judged.is_some_and(|r| match (&l.sha256, r.sha256.as_deref()) {
                        (Some(want), Some(got)) => want.eq_ignore_ascii_case(got),
                        (Some(_), None) => false,
                        (None, _) => true,
                    })
                } else {
                    match (&l.sha256, judged.and_then(|r| r.sidecar_sha256.as_deref())) {
                        (Some(want), Some(got)) => want.eq_ignore_ascii_case(got),
                        (Some(_), None) => false,
                        (None, _) => judged.is_some() || n == name,
                    }
                };
                if back {
                    let was = m.logs.remove(n);
                    changed = true;
                    if let (Some(was), Some((p, sha))) = (was, &prev)
                        && n == log
                        && was.known == Known::Chain
                        && !prev_known
                        && !m.logs.contains_key(p)
                        && !store.exists(p)
                    {
                        let now = now_ms();
                        m.logs.insert(
                            p.clone(),
                            MissingLog {
                                found_ms: now,
                                known: Known::Chain,
                                sha256: Some(sha.clone()),
                                length: None,
                                verdict: None,
                                asked_ms: None,
                                held_ms: None,
                                gone_ms: None,
                            },
                        );
                    }
                }
            }
            if changed {
                f.write(&m)?;
            }
            Ok(())
        })
        .await
    }
}
