//! A car key's trust records on disk, laid out so an upload costs the same whether the key has ten logs or two hundred
//! thousand: nothing is rewritten whole on an upload, and what an upload needs is small.
//!
//! ```text
//! <can>/<key>/trust.head          the commit point (sealed): format, generation, rules version, pairing, the logs
//!                                 still waiting for their sidecar, the logs the owner said to use anyway
//! <can>/<key>/trust-<g>.records   every log's record as of generation g (sealed), one line each, in name order:
//!                                 ["<name>", Record]
//! <can>/<key>/trust-<g>.journal   what changed since (sealed records, `vault::records`): each record one batch,
//!                                 [Change, ...]
//! <can>/<key>/trust.json          the format before this one (one sealed book, rewritten whole on every upload):
//!                                 read once and moved into the files above
//! <can>/<key>/trust.lock          empty: held (flock) by whoever reads or changes the files above, so the server and
//!                                 an owner's command (`keys trust`) take turns
//! <can>/.waiting/<key>            an empty file for each key with logs waiting for their sidecar, so the sweep
//!                                 looks only at those keys (it names a car key, as the folder names do)
//! ```
//!
//! An upload reads the head (kept in memory for recently used keys) and adds one batch to the journal. Each journal
//! record holds whole changes, and a record that a crash cut short is dropped whole when the journal is opened again
//! (`vault::records`). An upload's batch (a log's record, with a new pin or key) always fits in one record, so it is
//! all there or not at all. A longer batch (the sweep's) can take several records, and a crash then keeps the first
//! ones: that is safe because each log's change stands on its own (a log whose verdict was lost is still waiting and
//! is judged again). Changes that only make sense together (a new pin with every log judged again by it) don't go
//! through the journal: they are folded straight into the next generation, all or nothing.
//!
//! Two processes can use a key's files: the server, and a command the owner runs next to it. Each takes the key's lock
//! file first, and the head a process keeps in memory carries a stamp of the files ([`Stamp`]): when another process
//! changed them since, the head is read again before it is used.
//!
//! When the journal is long it is folded into the next generation: `trust-<g+1>.records` is written (the old records
//! merged with the journal, streamed in name order, so a key's history is never all in memory), then the head naming
//! `g+1` (written whole or not at all: a temporary file renamed over it), then generation g's files are removed. A
//! crash at any moment leaves either the old generation or the new one in force, never a mix.

use crate::judge::{Book, Former, Pin, Record, Status};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use vault::Vault;

/// The layout above.
pub(crate) const FORMAT: u32 = 2;
/// Where the "waiting" markers are, under the CAN folder.
pub(crate) const WAITING_DIR: &str = ".waiting";
/// Says the markers are complete (keys judged by the format before were looked at once).
const WAITING_READY: &str = ".ready";
/// A journal batch is at most one sealed record.
const BATCH_LIMIT: usize = vault::stream::CHUNK;
/// The file a process holds (flock) while it reads or changes a key's trust files.
pub(crate) const LOCK_FILE: &str = "trust.lock";

fn other(e: impl std::fmt::Display) -> io::Error {
    io::Error::other(e.to_string())
}

/// A journal as it was read for a lookup ([`Ledger::journal_as_read`]).
struct KeptJournal {
    path: PathBuf,
    stamp: (u64, u64, std::time::SystemTime),
    changes: Arc<Vec<Change>>,
}

/// Journals kept in memory at most (the keys whose logs are being worked on).
const JOURNALS_KEPT: usize = 4;

static JOURNALS: std::sync::Mutex<Vec<KeptJournal>> = std::sync::Mutex::new(Vec::new());

/// What a journal's file is now: its inode, size and change time (`None`: not there, or not to be told).
fn journal_stamp(path: &Path) -> Option<(u64, u64, std::time::SystemTime)> {
    use std::os::unix::fs::MetadataExt;
    let m = std::fs::metadata(path).ok()?;
    Some((m.ino(), m.len(), m.modified().ok()?))
}

/// A CAN Bridge key as it may appear in a path: 1 to 64 letters, digits or `-` (the log name rule).
pub(crate) fn valid_key(key: &str) -> bool {
    !key.is_empty() && key.len() <= 64 && key.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
}

/// One change to a key's trust state. The journal holds these; the head is what they add up to, minus the records.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "t", rename_all = "kebab-case")]
// (most changes are a log's record: boxing it would only add an allocation to each)
#[allow(clippy::large_enum_variant)]
pub(crate) enum Change {
    Log {
        name: String,
        record: Record,
    },
    Pin {
        pin: Option<Pin>,
    },
    NewKey {
        fingerprint: Option<String>,
    },
    Rules {
        v: u32,
    },
    /// The keys trusted before the pin, each until the owner trusted another one.
    Former {
        former: Vec<Former>,
    },
}

/// What a key's files were when a process last read or wrote them: the head file's size and first bytes (a sealed
/// file starts with a random id, new on every write) and the journal's size and time. Another process that changed the
/// files changed this too.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Stamp {
    head: Option<(u64, Vec<u8>)>,
    journal: Option<(u64, Option<std::time::SystemTime>)>,
}

/// A key's lock file, held until dropped.
pub(crate) struct Locked {
    _file: Option<File>,
}

/// A key's trust state without its per-log records: all an upload needs, small, so this is what stays in memory.
#[derive(Debug, Clone)]
pub(crate) struct Head {
    pub generation: u64,
    pub v: u32,
    pub pin: Option<Pin>,
    pub new_key: Option<String>,
    pub former: Vec<Former>,
    /// Logs waiting for their sidecar, and when each arrived.
    pub waiting: BTreeMap<String, u64>,
    /// Logs the owner said to use anyway.
    pub overrides: BTreeSet<String>,
    /// Changes in the journal of this generation.
    pub journal_len: usize,
    /// The journal could not be read to its end: what could be read is folded into a new generation before anything
    /// is added (records after a damaged one would never be read).
    pub damaged: bool,
    /// This process made sure the key's waiting marker is there.
    pub marked: bool,
    /// The files as this head was read or last written (`None`: not known, read the head again).
    pub stamp: Option<Stamp>,
}

impl Head {
    fn new() -> Head {
        Head {
            generation: 0,
            v: trust::RULES_VERSION,
            pin: None,
            new_key: None,
            former: Vec::new(),
            waiting: BTreeMap::new(),
            overrides: BTreeSet::new(),
            journal_len: 0,
            damaged: false,
            marked: false,
            stamp: None,
        }
    }

    pub(crate) fn apply(&mut self, c: &Change) {
        match c {
            Change::Log { name, record } => {
                if record.status == Status::Waiting {
                    self.waiting.insert(name.clone(), record.arrived_ms);
                } else {
                    self.waiting.remove(name);
                }
                if record.override_use {
                    self.overrides.insert(name.clone());
                } else {
                    self.overrides.remove(name);
                }
            }
            Change::Pin { pin } => self.pin = pin.clone(),
            Change::NewKey { fingerprint } => self.new_key = fingerprint.clone(),
            Change::Rules { v } => self.v = *v,
            Change::Former { former } => self.former = former.clone(),
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct HeadFile {
    format: u32,
    generation: u64,
    v: u32,
    pin: Option<Pin>,
    new_key: Option<String>,
    #[serde(default)]
    former: Vec<Former>,
    #[serde(default)]
    waiting: BTreeMap<String, u64>,
    #[serde(default)]
    overrides: BTreeSet<String>,
}

/// One key's trust files.
pub(crate) struct Ledger {
    can: PathBuf,
    key: String,
    vault: Arc<Vault>,
}

/// A sealed file written whole or not at all: a temporary file of its own (two processes never share one), synced,
/// renamed over the file, and the folder synced.
pub(crate) fn write_sealed(
    vault: &Vault,
    path: &Path,
    label: &str,
    fill: impl FnOnce(&mut dyn Write) -> io::Result<()>,
) -> io::Result<()> {
    let dir = path.parent().ok_or_else(|| other("no folder"))?;
    std::fs::create_dir_all(dir)?;
    let mut rnd = [0u8; 8];
    getrandom::fill(&mut rnd).map_err(other)?;
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".{}.tmp", hex::encode(rnd)));
    let tmp = PathBuf::from(tmp);
    let result = (|| {
        let f = {
            let mut o = std::fs::OpenOptions::new();
            o.write(true).create_new(true);
            #[cfg(unix)]
            std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
            o.open(&tmp)?
        };
        let mut w = vault.seal_writer(label, io::BufWriter::with_capacity(1 << 16, f)).map_err(other)?;
        fill(&mut w)?;
        let f = w.finish()?.into_inner().map_err(|e| e.into_error())?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result?;
    if let Ok(d) = File::open(dir) {
        let _ = d.sync_all();
    }
    Ok(())
}

pub(crate) fn remove_if_there(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// The name a records line starts with (`["<name>",`), without parsing the record. Log names need no JSON escapes.
fn line_name(line: &[u8]) -> Option<&str> {
    let rest = line.strip_prefix(b"[\"")?;
    let end = rest.iter().position(|&c| c == b'"' || c == b'\\')?;
    if rest[end] != b'"' {
        return None;
    }
    std::str::from_utf8(&rest[..end]).ok()
}

fn name_of(line: &[u8]) -> io::Result<String> {
    match line_name(line) {
        Some(n) => Ok(n.to_string()),
        None => serde_json::from_slice::<(String, serde::de::IgnoredAny)>(line).map(|(n, _)| n).map_err(other),
    }
}

fn line_of(name: &str, record: &Record) -> io::Result<Vec<u8>> {
    serde_json::to_vec(&(name, record)).map_err(other)
}

/// Makes sure the sweep will look at `key` (it has logs waiting).
pub(crate) fn mark_waiting(can: &Path, key: &str) -> io::Result<()> {
    let dir = can.join(WAITING_DIR);
    std::fs::create_dir_all(&dir)?;
    std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(dir.join(key)).map(|_| ())
}

pub(crate) fn unmark_waiting(can: &Path, key: &str) -> io::Result<()> {
    remove_if_there(&can.join(WAITING_DIR).join(key))
}

/// The keys with logs waiting. The first time, keys still in the format before are marked too (they had no markers).
pub(crate) fn waiting_keys(can: &Path) -> io::Result<Vec<String>> {
    let dir = can.join(WAITING_DIR);
    if !dir.join(WAITING_READY).exists() {
        std::fs::create_dir_all(&dir)?;
        for e in std::fs::read_dir(can)?.flatten() {
            let name = e.file_name();
            let Some(key) = name.to_str().filter(|k| valid_key(k)) else { continue };
            if e.path().join("trust.json").exists() {
                mark_waiting(can, key)?;
            }
        }
        File::create(dir.join(WAITING_READY))?;
    }
    let mut keys = Vec::new();
    for e in std::fs::read_dir(&dir)?.flatten() {
        if let Some(k) = e.file_name().to_str().filter(|k| valid_key(k)) {
            keys.push(k.to_string());
        }
    }
    keys.sort_unstable();
    Ok(keys)
}

impl Ledger {
    pub(crate) fn new(can: &Path, key: &str, vault: Arc<Vault>) -> Ledger {
        Ledger { can: can.to_path_buf(), key: key.to_string(), vault }
    }

    fn file(&self, name: &str) -> (PathBuf, String) {
        (self.can.join(&self.key).join(name), format!("can/{}/{name}", self.key))
    }

    fn head_file(&self) -> (PathBuf, String) {
        self.file("trust.head")
    }

    fn records_file(&self, generation: u64) -> (PathBuf, String) {
        self.file(&format!("trust-{generation}.records"))
    }

    fn journal_file(&self, generation: u64) -> (PathBuf, String) {
        self.file(&format!("trust-{generation}.journal"))
    }

    fn legacy_file(&self) -> (PathBuf, String) {
        self.file("trust.json")
    }

    /// Takes the key's lock file, waiting while another process holds it. Made when it isn't there. None is needed
    /// for a key with no folder (no logs: nothing to change), or for one that can't be written (a read-only copy of the
    /// data) without a lock file.
    pub(crate) fn lock(&self) -> io::Result<Locked> {
        let dir = self.can.join(&self.key);
        let path = dir.join(LOCK_FILE);
        let mut o = std::fs::OpenOptions::new();
        o.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
        let f = match o.open(&path) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound && !dir.exists() => return Ok(Locked { _file: None }),
            Err(e) if matches!(e.kind(), io::ErrorKind::PermissionDenied | io::ErrorKind::ReadOnlyFilesystem) => {
                match File::open(&path) {
                    Ok(f) => f,
                    Err(n) if n.kind() == io::ErrorKind::NotFound => return Ok(Locked { _file: None }),
                    Err(_) => return Err(e),
                }
            }
            Err(e) => return Err(e),
        };
        loop {
            match rustix::fs::flock(&f, rustix::fs::FlockOperation::LockExclusive) {
                Ok(()) => return Ok(Locked { _file: Some(f) }),
                Err(rustix::io::Errno::INTR) => continue,
                Err(e) => return Err(e.into()),
            }
        }
    }

    /// The files as they are now (the head of `generation` was read or written last).
    pub(crate) fn stamp(&self, generation: u64) -> io::Result<Stamp> {
        let head = match File::open(self.head_file().0) {
            Ok(f) => {
                let len = f.metadata()?.len();
                let mut first = Vec::with_capacity(vault::stream::HEADER);
                f.take(vault::stream::HEADER as u64).read_to_end(&mut first)?;
                Some((len, first))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => return Err(e),
        };
        let journal = match std::fs::metadata(self.journal_file(generation).0) {
            Ok(m) => Some((m.len(), m.modified().ok())),
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => return Err(e),
        };
        Ok(Stamp { head, journal })
    }

    /// The key's head: from its head file (or the format before, moved over now), plus its journal.
    pub(crate) fn load_head(&self) -> io::Result<Head> {
        let (path, label) = self.head_file();
        let mut head = match self.vault.read_file(&path, &label).map_err(other)? {
            Some(bytes) => {
                let f: HeadFile = serde_json::from_slice(&bytes).map_err(other)?;
                if f.format != FORMAT {
                    // (not the key in the message: errors end up in the server's log, kept in the clear)
                    return Err(other(format!("trust records in an unknown format {}", f.format)));
                }
                // left by a crash just after a commit: the format before, and the generation before
                remove_if_there(&self.legacy_file().0)?;
                if f.generation > 0 {
                    self.remove_journal(f.generation - 1)?;
                    remove_if_there(&self.records_file(f.generation - 1).0)?;
                }
                Head {
                    generation: f.generation,
                    v: f.v,
                    pin: f.pin,
                    new_key: f.new_key,
                    former: f.former,
                    waiting: f.waiting,
                    overrides: f.overrides,
                    ..Head::new()
                }
            }
            None => match self.migrate_legacy()? {
                Some(h) => h,
                None => Head::new(),
            },
        };
        let (changes, damaged) = self.read_journal(head.generation)?;
        for c in &changes {
            head.apply(c);
        }
        head.journal_len = changes.len();
        head.damaged = damaged;
        Ok(head)
    }

    /// A `trust.json` of the format before (the whole book, sealed): written as generation 1, then removed.
    fn migrate_legacy(&self) -> io::Result<Option<Head>> {
        let (path, label) = self.legacy_file();
        let Some(bytes) = self.vault.read_file(&path, &label).map_err(other)? else {
            return Ok(None);
        };
        let book: Book = serde_json::from_slice(&bytes).map_err(other)?;
        let mut head = Head {
            generation: 1,
            v: book.v,
            pin: book.pin.clone(),
            new_key: book.new_key.clone(),
            former: book.former.clone(),
            ..Head::new()
        };
        for (name, r) in &book.logs {
            head.apply(&Change::Log { name: name.clone(), record: r.clone() });
        }
        let (rp, rl) = self.records_file(1);
        write_sealed(&self.vault, &rp, &rl, |w| {
            for (name, r) in &book.logs {
                w.write_all(&line_of(name, r)?)?;
                w.write_all(b"\n")?;
            }
            Ok(())
        })?;
        if !head.waiting.is_empty() {
            mark_waiting(&self.can, &self.key)?;
            head.marked = true;
        }
        self.write_head(&head)?;
        remove_if_there(&path)?;
        Ok(Some(head))
    }

    fn write_head(&self, head: &Head) -> io::Result<()> {
        let f = HeadFile {
            format: FORMAT,
            generation: head.generation,
            v: head.v,
            pin: head.pin.clone(),
            new_key: head.new_key.clone(),
            former: head.former.clone(),
            waiting: head.waiting.clone(),
            overrides: head.overrides.clone(),
        };
        let bytes = serde_json::to_vec(&f).map_err(other)?;
        let (path, label) = self.head_file();
        write_sealed(&self.vault, &path, &label, |w| w.write_all(&bytes))
    }

    /// The journal's changes, in order. `true` with them: it could not be read to its end (what came before is kept).
    fn read_journal(&self, generation: u64) -> io::Result<(Vec<Change>, bool)> {
        let (path, label) = self.journal_file(generation);
        if !path.exists() {
            return Ok((Vec::new(), false));
        }
        // read only: never cut or changed here, so a reader (the `chain` command, say) can't harm an append under way
        let mut journal = match self.vault.records_read(&path, &label) {
            Ok(j) => j,
            // folded away since it was looked for: as when it isn't there
            Err(e) if e.is_not_found() => return Ok((Vec::new(), false)),
            Err(vault::Error::NotSealed) => return Ok((Vec::new(), true)),
            Err(e) => return Err(other(e)),
        };
        let (mut out, mut damaged) = (Vec::new(), false);
        let read = journal.read_all(|batch| {
            match serde_json::from_slice::<Vec<Change>>(batch) {
                Ok(v) => out.extend(v),
                Err(_) => damaged = true,
            }
            Ok(())
        });
        if let Err(e) = read {
            match vault::unwrap_io(e) {
                vault::Error::Damaged | vault::Error::NotSealed => damaged = true,
                e => return Err(other(e)),
            }
        }
        Ok((out, damaged))
    }

    /// [`Ledger::read_journal`]'s changes for a lookup, from memory while the journal's file is the one they were
    /// read from (the same file, size and time: a journal only ever grows, or is replaced by the next generation's).
    /// The background decoding looks every log's record up as it works on it, and reading a long journal for each
    /// took longer than reading the log. The journals of the keys looked at last are kept ([`JOURNALS_KEPT`]; a
    /// journal has at most a few thousand changes before it is folded).
    fn journal_as_read(&self, generation: u64) -> io::Result<Arc<Vec<Change>>> {
        let (path, _) = self.journal_file(generation);
        let Some(stamp) = journal_stamp(&path) else { return Ok(Arc::new(self.read_journal(generation)?.0)) };
        {
            let mut kept = JOURNALS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(i) = kept.iter().position(|k| k.path == path && k.stamp == stamp) {
                // the most recently used last
                let hit = kept.remove(i);
                let changes = hit.changes.clone();
                kept.push(hit);
                return Ok(changes);
            }
        }
        let (changes, damaged) = self.read_journal(generation)?;
        let changes = Arc::new(changes);
        // kept only when it was read whole and the file is still the one looked at
        if !damaged && journal_stamp(&path) == Some(stamp) {
            let mut kept = JOURNALS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            kept.retain(|k| k.path != path);
            if kept.len() >= JOURNALS_KEPT {
                kept.remove(0);
            }
            kept.push(KeptJournal { path, stamp, changes: changes.clone() });
        }
        Ok(changes)
    }

    /// Adds `changes` (already applied to `head`) to the journal, in batches of one sealed record each. A change too
    /// big for one record, or a damaged journal, goes through [`Ledger::compact`] instead.
    #[cfg(test)]
    pub(crate) fn append(&self, head: &mut Head, changes: &[Change]) -> io::Result<()> {
        let groups: Vec<&[Change]> = changes.iter().map(std::slice::from_ref).collect();
        self.append_groups(head, &groups)
    }

    /// [`Ledger::append`] of several groups of changes (already applied to `head`, in this order), each group whole
    /// in one sealed record (a log's record with the pin or key it sets: all there after a crash, or none of it), as
    /// many groups to a record as fit, all written with one sync.
    pub(crate) fn append_groups(&self, head: &mut Head, groups: &[&[Change]]) -> io::Result<()> {
        let total: usize = groups.iter().map(|g| g.len()).sum();
        if total == 0 {
            return Ok(());
        }
        let all = || groups.iter().flat_map(|g| g.iter().cloned()).collect::<Vec<Change>>();
        if head.damaged {
            return self.compact(head, all());
        }
        let mut batches: Vec<Vec<u8>> = Vec::new();
        let mut cur: Vec<u8> = Vec::new();
        for g in groups.iter().filter(|g| !g.is_empty()) {
            let mut gb: Vec<u8> = Vec::new();
            for c in g.iter() {
                if !gb.is_empty() {
                    gb.push(b',');
                }
                gb.extend_from_slice(&serde_json::to_vec(c).map_err(other)?);
            }
            if gb.len() + 2 > BATCH_LIMIT {
                return self.compact(head, all());
            }
            if !cur.is_empty() && cur.len() + gb.len() + 2 > BATCH_LIMIT {
                cur.push(b']');
                batches.push(std::mem::take(&mut cur));
            }
            cur.push(if cur.is_empty() { b'[' } else { b',' });
            cur.extend_from_slice(&gb);
        }
        if !cur.is_empty() {
            cur.push(b']');
            batches.push(cur);
        }
        let (path, label) = self.journal_file(head.generation);
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        let mut journal = self.vault.records(&path, &label).map_err(other)?;
        journal.append_each(&batches)?;
        head.journal_len += total;
        Ok(())
    }

    /// Folds the journal, and `extra` (changes already applied to `head` but not in the journal), into the next
    /// generation, all at once: until the new head is in place the old generation is in force. Memory: the journal's
    /// changes and `extra`; the records file is streamed.
    pub(crate) fn compact(&self, head: &mut Head, extra: Vec<Change>) -> io::Result<()> {
        let (mut changes, _) = self.read_journal(head.generation)?;
        changes.extend(extra);
        let mut newer: BTreeMap<String, Record> = BTreeMap::new();
        for c in changes {
            if let Change::Log { name, record } = c {
                newer.insert(name, record);
            }
        }
        let next = head.generation + 1;
        let (rp, rl) = self.records_file(next);
        write_sealed(&self.vault, &rp, &rl, |w| {
            let mut pending = newer.iter().peekable();
            self.read_lines(head.generation, |line| {
                let name = name_of(line)?;
                while let Some((n, r)) = pending.next_if(|(n, _)| n.as_str() < name.as_str()) {
                    w.write_all(&line_of(n, r)?)?;
                    w.write_all(b"\n")?;
                }
                if let Some((n, r)) = pending.next_if(|(n, _)| **n == name) {
                    w.write_all(&line_of(n, r)?)?;
                } else {
                    w.write_all(line)?;
                }
                w.write_all(b"\n")?;
                Ok(ControlFlow::Continue(()))
            })?;
            for (n, r) in pending {
                w.write_all(&line_of(n, r)?)?;
                w.write_all(b"\n")?;
            }
            Ok(())
        })?;
        let old = head.generation;
        let mut next_head = head.clone();
        next_head.generation = next;
        self.write_head(&next_head)?; // the commit
        head.generation = next;
        head.journal_len = 0;
        head.damaged = false;
        self.remove_journal(old)?;
        remove_if_there(&self.records_file(old).0)?;
        Ok(())
    }

    /// Removes a generation's journal, with the copy a crash may have left while the vault wrote it again in its
    /// newer format (`vault::records::converting_path`).
    fn remove_journal(&self, generation: u64) -> io::Result<()> {
        let path = self.journal_file(generation).0;
        remove_if_there(&path)?;
        remove_if_there(&vault::records::converting_path(&path))
    }

    /// Calls `each` with every line of the records file of `generation` (none for generation 0, which has no file).
    fn read_lines(
        &self,
        generation: u64,
        mut each: impl FnMut(&[u8]) -> io::Result<ControlFlow<()>>,
    ) -> io::Result<()> {
        if generation == 0 {
            return Ok(());
        }
        let (path, label) = self.records_file(generation);
        let f = File::open(&path)?;
        let len = f.metadata()?.len();
        let reader = self.vault.open_reader(&label, BufReader::with_capacity(1 << 16, f), len).map_err(other)?;
        let mut reader = BufReader::with_capacity(1 << 16, reader);
        let mut line = Vec::new();
        loop {
            line.clear();
            let n = reader.read_until(b'\n', &mut line).map_err(|e| other(vault::unwrap_io(e)))?;
            if n == 0 {
                return Ok(());
            }
            let l = line.strip_suffix(b"\n").unwrap_or(&line);
            if l.is_empty() {
                continue;
            }
            if each(l)?.is_break() {
                return Ok(());
            }
        }
    }

    /// Every log's record, in name order (the journal's newer ones in place of the records file's).
    pub(crate) fn for_each(&self, head: &Head, mut f: impl FnMut(&str, &Record) -> ControlFlow<()>) -> io::Result<()> {
        let (changes, _) = self.read_journal(head.generation)?;
        let mut newer: BTreeMap<String, Record> = BTreeMap::new();
        for c in changes {
            if let Change::Log { name, record } = c {
                newer.insert(name, record);
            }
        }
        let mut pending = newer.iter().peekable();
        let mut stopped = false;
        self.read_lines(head.generation, |line| {
            let (name, record): (String, Record) = serde_json::from_slice(line).map_err(other)?;
            while let Some((n, r)) = pending.next_if(|(n, _)| n.as_str() < name.as_str()) {
                if f(n, r).is_break() {
                    stopped = true;
                    return Ok(ControlFlow::Break(()));
                }
            }
            let flow = match pending.next_if(|(n, _)| **n == name) {
                Some((n, r)) => f(n, r),
                None => f(&name, &record),
            };
            stopped = flow.is_break();
            Ok(flow)
        })?;
        if !stopped {
            for (n, r) in pending {
                if f(n, r).is_break() {
                    break;
                }
            }
        }
        Ok(())
    }

    /// One log's record: the journal's last word on it, else the records file (read up to its place in name order).
    pub(crate) fn lookup(&self, head: &Head, name: &str) -> io::Result<Option<Record>> {
        let changes = self.journal_as_read(head.generation)?;
        let newest = changes.iter().rev().find_map(|c| match c {
            Change::Log { name: n, record } if n == name => Some(record.clone()),
            _ => None,
        });
        if newest.is_some() {
            return Ok(newest);
        }
        let mut found = None;
        self.read_lines(head.generation, |line| {
            let n = name_of(line)?;
            Ok(match n.as_str().cmp(name) {
                std::cmp::Ordering::Less => ControlFlow::Continue(()),
                std::cmp::Ordering::Equal => {
                    found = Some(serde_json::from_slice::<(String, Record)>(line).map_err(other)?.1);
                    ControlFlow::Break(())
                }
                std::cmp::Ordering::Greater => ControlFlow::Break(()),
            })
        })?;
        Ok(found)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    fn vault() -> (tempfile::TempDir, Arc<Vault>, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        std::fs::create_dir_all(&data).unwrap();
        let (v, _) = Vault::create(&data, &dir.path().join("keys"), vault::Kdf::FAST_FOR_TESTS).unwrap();
        (dir, Arc::new(v), data.join("can"))
    }

    fn rec(status: Status, arrived_ms: u64) -> Record {
        Record {
            v: trust::RULES_VERSION,
            status,
            reason: None,
            link: None,
            override_use: false,
            judged_ms: arrived_ms,
            arrived_ms,
            sha256: None,
            length: None,
            sidecar_sha256: None,
            unsigned: None,
        }
    }

    fn name(i: u32) -> String {
        format!("k1_20261108T{:02}{:02}00Z_{}.log.xz", i / 60 % 24, i % 60, i)
    }

    fn all(l: &Ledger, h: &Head) -> Vec<(String, Status)> {
        let mut v = Vec::new();
        l.for_each(h, |n, r| {
            v.push((n.to_string(), r.status.clone()));
            ControlFlow::Continue(())
        })
        .unwrap();
        v
    }

    #[test]
    fn line_names_are_read_without_parsing() {
        assert_eq!(line_name(br#"["k1_20261108T000000Z_1.log.xz",{"v":1}]"#), Some("k1_20261108T000000Z_1.log.xz"));
        assert_eq!(line_name(br#"["a\"b",1]"#), None);
        assert_eq!(name_of(br#"["a\"b",1]"#).unwrap(), "a\"b");
    }

    #[test]
    fn changes_survive_compaction_and_stay_in_name_order() {
        let (_d, v, can) = vault();
        let l = Ledger::new(&can, "k1", v);
        let mut h = l.load_head().unwrap();
        for round in 0..3u32 {
            let mut changes = Vec::new();
            for i in (0..50u32).rev() {
                let st = if (i + round) % 3 == 0 { Status::Waiting } else { Status::Unsigned };
                changes.push(Change::Log { name: name(i + round * 25), record: rec(st, u64::from(i)) });
            }
            for c in &changes {
                h.apply(c);
            }
            l.append(&mut h, &changes).unwrap();
            l.compact(&mut h, Vec::new()).unwrap();
        }
        assert_eq!(h.generation, 3);
        let got = all(&l, &h);
        assert_eq!(got.len(), 100);
        assert!(got.windows(2).all(|w| w[0].0 < w[1].0), "in name order, no duplicates");
        // what was written last wins
        let last = got.iter().find(|(n, _)| *n == name(50 + 49)).unwrap();
        assert_eq!(last.1, if (49 + 2) % 3 == 0 { Status::Waiting } else { Status::Unsigned });
        let reread = l.load_head().unwrap();
        assert_eq!(reread.waiting, h.waiting);
        assert_eq!(reread.generation, 3);
        let dir = can.join("k1");
        let mut files: Vec<String> =
            std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        files.sort();
        assert_eq!(files, ["trust-3.records", "trust.head"], "the generations before are gone");
        assert_eq!(l.lookup(&h, &name(10)).unwrap().unwrap().arrived_ms, 10);
        assert!(l.lookup(&h, "k1_20991108T000000Z_1.log.xz").unwrap().is_none());
    }

    /// A lookup's journal comes from memory only while its file is the one that was read: every change added
    /// after a lookup is seen by the next one, also another process's (another `Ledger` on the same files), a
    /// journal cut short, and a new generation.
    #[test]
    fn a_lookup_sees_every_change_made_since_the_last_one() {
        let (_d, v, can) = vault();
        let l = Ledger::new(&can, "k1", v.clone());
        let mut h = l.load_head().unwrap();
        let put = |l: &Ledger, h: &mut Head, i: u32, st: Status, at: u64| {
            let c = vec![Change::Log { name: name(i), record: rec(st, at) }];
            h.apply(&c[0]);
            l.append(h, &c).unwrap();
        };
        assert!(l.lookup(&h, &name(1)).unwrap().is_none());
        put(&l, &mut h, 1, Status::Waiting, 1);
        assert_eq!(l.lookup(&h, &name(1)).unwrap().unwrap().status, Status::Waiting);
        // asked again with nothing changed: the same answer
        assert_eq!(l.lookup(&h, &name(1)).unwrap().unwrap().arrived_ms, 1);
        for at in 2..40u64 {
            put(&l, &mut h, 1, Status::Unsigned, at);
            assert_eq!(l.lookup(&h, &name(1)).unwrap().unwrap().arrived_ms, at);
            assert!(l.lookup(&h, &name(2)).unwrap().is_none());
        }
        // another process's change
        let other = Ledger::new(&can, "k1", v.clone());
        let mut oh = other.load_head().unwrap();
        put(&other, &mut oh, 2, Status::Verified, 7);
        assert_eq!(l.lookup(&oh, &name(2)).unwrap().unwrap().status, Status::Verified);
        // a journal cut short (a crash mid-record): what is left is what a lookup gives
        let journal = can.join("k1/trust-0.journal");
        let whole = std::fs::metadata(&journal).unwrap().len();
        put(&l, &mut oh, 2, Status::Failed, 8);
        assert_eq!(l.lookup(&oh, &name(2)).unwrap().unwrap().status, Status::Failed);
        std::fs::OpenOptions::new().write(true).open(&journal).unwrap().set_len(whole).unwrap();
        assert_eq!(l.lookup(&oh, &name(2)).unwrap().unwrap().status, Status::Verified);
        // the next generation: its journal is another file
        let mut h = l.load_head().unwrap();
        l.compact(&mut h, Vec::new()).unwrap();
        assert_eq!(l.lookup(&h, &name(1)).unwrap().unwrap().arrived_ms, 39);
        put(&l, &mut h, 1, Status::Verified, 99);
        assert_eq!(l.lookup(&h, &name(1)).unwrap().unwrap().arrived_ms, 99);
        assert_eq!(l.lookup(&h, &name(2)).unwrap().unwrap().status, Status::Verified);
    }

    #[test]
    fn a_batch_cut_by_a_crash_is_dropped_and_the_rest_is_kept() {
        let (_d, v, can) = vault();
        let l = Ledger::new(&can, "k1", v);
        let mut h = l.load_head().unwrap();
        let first = vec![
            Change::Log { name: name(1), record: rec(Status::Waiting, 1) },
            Change::Pin { pin: Some(Pin { fingerprint: "aa".into(), pinned_ms: 1, how: "owner".into() }) },
        ];
        for c in &first {
            h.apply(c);
        }
        l.append(&mut h, &first).unwrap();
        let journal = can.join("k1/trust-0.journal");
        let whole = std::fs::metadata(&journal).unwrap().len();
        let second = vec![Change::Log { name: name(1), record: rec(Status::Verified, 1) }];
        l.append(&mut h, &second).unwrap();
        // the crash: the second batch's record is only partly on disk
        let f = std::fs::OpenOptions::new().write(true).open(&journal).unwrap();
        let cut = std::fs::metadata(&journal).unwrap().len() - 5;
        f.set_len(cut).unwrap();
        drop(f);
        let back = l.load_head().unwrap();
        assert_eq!(back.waiting.keys().collect::<Vec<_>>(), [&name(1)], "the first batch is all there");
        assert_eq!(back.pin.as_ref().map(|p| p.fingerprint.as_str()), Some("aa"));
        assert_eq!(back.journal_len, 2);
        assert!(!back.damaged);
        assert_eq!(std::fs::metadata(&journal).unwrap().len(), cut, "reading leaves the file as it is");
        // and the journal goes on from there: the next append cuts the torn record off first
        let kept = std::fs::read(&journal).unwrap()[..whole as usize].to_vec();
        let mut back = back;
        let third = vec![Change::Log { name: name(2), record: rec(Status::Unsigned, 2) }];
        back.apply(&third[0]);
        l.append(&mut back, &third).unwrap();
        let now = std::fs::read(&journal).unwrap();
        assert!(now.starts_with(&kept) && now.len() as u64 > whole, "the new record follows the whole ones");
        let again = l.load_head().unwrap();
        assert_eq!(again.journal_len, 3);
        assert_eq!(all(&l, &again), [(name(1), Status::Waiting), (name(2), Status::Unsigned)]);
    }

    #[test]
    fn a_crash_during_compaction_leaves_the_old_generation_in_force() {
        let (_d, v, can) = vault();
        let l = Ledger::new(&can, "k1", v.clone());
        let mut h = l.load_head().unwrap();
        let c = vec![Change::Log { name: name(1), record: rec(Status::Unsigned, 1) }];
        h.apply(&c[0]);
        l.append(&mut h, &c).unwrap();
        l.compact(&mut h, Vec::new()).unwrap(); // generation 1
        let c = vec![Change::Log { name: name(2), record: rec(Status::Waiting, 2) }];
        h.apply(&c[0]);
        l.append(&mut h, &c).unwrap();
        // the crash: generation 2's records are written but its head is not (the commit never happened)
        let (rp, rl) = l.records_file(2);
        write_sealed(&v, &rp, &rl, |w| w.write_all(b"[\"garbage\",{}]\n")).unwrap();
        // and a temporary file of a head that was never renamed
        std::fs::write(can.join("k1/trust.head.0011223344556677.tmp"), b"half").unwrap();
        let back = l.load_head().unwrap();
        assert_eq!(back.generation, 1);
        assert_eq!(all(&l, &back), [(name(1), Status::Unsigned), (name(2), Status::Waiting)]);
        // compacting again replaces the half-made generation
        let mut back = back;
        l.compact(&mut back, Vec::new()).unwrap();
        assert_eq!(back.generation, 2);
        assert_eq!(all(&l, &back), [(name(1), Status::Unsigned), (name(2), Status::Waiting)]);
    }

    #[test]
    fn a_damaged_journal_keeps_what_came_before_and_is_folded_away() {
        let (_d, v, can) = vault();
        let l = Ledger::new(&can, "k1", v);
        let mut h = l.load_head().unwrap();
        for i in 0..3 {
            let c = vec![Change::Log { name: name(i), record: rec(Status::Unsigned, u64::from(i)) }];
            h.apply(&c[0]);
            l.append(&mut h, &c).unwrap();
        }
        // flip a byte inside the last record (not a cut: a change)
        let journal = can.join("k1/trust-0.journal");
        let mut bytes = std::fs::read(&journal).unwrap();
        let n = bytes.len();
        bytes[n - 3] ^= 1;
        std::fs::write(&journal, &bytes).unwrap();
        // and a copy a crash left while the vault wrote the journal again in its newer format
        let converting = vault::records::converting_path(&journal);
        std::fs::write(&converting, b"half a copy").unwrap();
        let mut back = l.load_head().unwrap();
        assert!(back.damaged);
        assert_eq!(back.journal_len, 2);
        let c = vec![Change::Log { name: name(9), record: rec(Status::Unsigned, 9) }];
        back.apply(&c[0]);
        l.append(&mut back, &c).unwrap();
        assert_eq!((back.generation, back.damaged), (1, false), "folded into a new generation before adding");
        assert_eq!(all(&l, &back).len(), 3);
        assert!(!journal.exists() && !converting.exists(), "the folded journal goes, with its copy");
        // a crash just after a commit left the generation before, and its copy: both go when the head is read
        let (old, old_copy) = (can.join("k1/trust-0.journal"), converting);
        std::fs::write(&old, b"left").unwrap();
        std::fs::write(&old_copy, b"left").unwrap();
        l.load_head().unwrap();
        assert!(!old.exists() && !old_copy.exists());
    }

    #[test]
    fn a_change_too_big_for_one_record_goes_through_compaction() {
        let (_d, v, can) = vault();
        let l = Ledger::new(&can, "k1", v);
        let mut h = l.load_head().unwrap();
        let big = Pin { fingerprint: "f".repeat(BATCH_LIMIT), pinned_ms: 1, how: "owner".into() };
        let mut changes = vec![Change::Pin { pin: Some(big) }];
        changes.extend((0..500).map(|i| Change::Log { name: name(i), record: rec(Status::Unsigned, 1) }));
        for c in &changes {
            h.apply(c);
        }
        l.append(&mut h, &changes).unwrap();
        assert_eq!(h.generation, 1);
        let back = l.load_head().unwrap();
        assert_eq!(back.pin.as_ref().unwrap().fingerprint.len(), BATCH_LIMIT);
        assert_eq!(all(&l, &h).len(), 500);
        // many small ones are split over several records
        let mut h = back;
        let many: Vec<Change> =
            (500..1500).map(|i| Change::Log { name: name(i), record: rec(Status::Unsigned, 1) }).collect();
        l.append(&mut h, &many).unwrap();
        assert_eq!(l.load_head().unwrap().journal_len, 1000);
        assert_eq!(all(&l, &h).len(), 1500);
    }
}
