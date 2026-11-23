//! A state kept as a sealed snapshot plus a journal of changes since it: changes are appended (never a whole file
//! rewritten per change), and the journal is folded into a new snapshot when it has grown about as large as the
//! state itself, so writing costs a small constant factor however big the state gets.
//!
//! ```text
//! <name>.snap  = "SMDB" | kind | version | generation (u64) | level (0 warm, 1 cold) | compressed state
//! <name>.log   = journal whose header names the generation it follows
//! ```
//!
//! Folding is crash-safe: the new snapshot (generation g + 1) is written whole and renamed into place, then the old
//! journal (generation g) is removed. A crash before the rename leaves the old snapshot and journal; after it, the
//! new snapshot, and the old journal is recognised by its generation and removed. A journal *newer* than its
//! snapshot means the snapshot was lost or replaced: reported as damage, never read as empty.

use crate::codec;
use crate::error::{Error, Result};
use crate::file::{Ctx, Kind, Which, check_header, header, remove_if_exists, sync_dir};
use crate::journal::{Files, Journal};

/// A state that can be kept this way.
pub(crate) trait Logged: Sized {
    type Op;
    /// What the snapshot holds (tables and the fleet: `Snapshot`; segment days: `Segment`).
    const KIND: Kind;
    fn empty() -> Self;
    fn apply(&mut self, op: Self::Op);
    fn encode_op(ctx: &Ctx, op: &Self::Op) -> Result<Vec<u8>>;
    fn decode_op(b: &[u8]) -> Result<Self::Op>;
    fn encode_state(&self) -> Result<Vec<u8>>;
    fn decode_state(b: &[u8]) -> Result<Self>;
    /// How hard snapshots are compressed when the journal is folded in on the way.
    const FOLD: Which = Which::Warm;
    /// Whether to fold now, from the journal's size and the last snapshot's.
    fn fold_due(ctx: &Ctx, journal_bytes: u64, _entries: u64, _snap_bytes: u64, snap_plain: u64) -> bool {
        journal_bytes >= snap_plain.max(ctx.opts.fold_min_bytes)
    }
}

pub(crate) const LEVEL_WARM: u8 = 0;
pub(crate) const LEVEL_COLD: u8 = 1;
/// Header, generation and level: what [`snapshot_head`] reads.
pub(crate) const SNAP_HEAD: usize = crate::file::HEADER + 9;

pub(crate) struct LoggedFile<S: Logged> {
    pub state: S,
    pub files: Files,
    generation: u64,
    journal: Option<Journal>,
    /// The snapshot's sealed plaintext size and its state's size uncompressed.
    snap_bytes: u64,
    snap_plain: u64,
    snap_level: u8,
}

/// A snapshot's generation and level, opening only its first chunk. `Ok(None)`: no snapshot.
pub(crate) fn snapshot_head(ctx: &Ctx, files: &Files, kind: Kind) -> Result<Option<(u64, u8, u64)>> {
    let label = ctx.label(&files.snap);
    let Some(head) = ctx.read_head(&files.snap, SNAP_HEAD)? else { return Ok(None) };
    let body = check_header(&head, kind, &label)?;
    let (generation, level) = parse_gen_level(body, &label)?;
    let len = std::fs::metadata(&files.snap)?.len();
    Ok(Some((generation, level, len)))
}

fn parse_gen_level(body: &[u8], label: &str) -> Result<(u64, u8)> {
    let damaged = || Error::Damaged { file: label.to_string(), why: "snapshot header cut short".into() };
    let generation =
        body.get(..8).and_then(|g| <[u8; 8]>::try_from(g).ok()).map(u64::from_be_bytes).ok_or_else(damaged)?;
    let level = *body.get(8).ok_or_else(damaged)?;
    Ok((generation, level))
}

impl<S: Logged> LoggedFile<S> {
    /// Reads the snapshot and replays the journal, kept open to commit changes to.
    pub(crate) fn load(ctx: &Ctx, files: Files) -> Result<LoggedFile<S>> {
        LoggedFile::load_as(ctx, files, true, true)
    }

    /// Reads the state only (snapshot and journal), for a reader that commits nothing: the journal is opened
    /// read-only, so it is never cut or changed.
    pub(crate) fn read_state(ctx: &Ctx, files: Files) -> Result<S> {
        Ok(LoggedFile::load_as(ctx, files, false, true)?.state)
    }

    /// [`Self::read_state`] for another process's files ([`crate::Reader`]): not even a crash's leftovers (a
    /// temporary file, a journal already folded) are cleaned up; the owner does that.
    pub(crate) fn inspect_state(ctx: &Ctx, files: Files) -> Result<S> {
        Ok(LoggedFile::load_as(ctx, files, false, false)?.state)
    }

    fn load_as(ctx: &Ctx, files: Files, to_commit: bool, tidy: bool) -> Result<LoggedFile<S>> {
        if tidy {
            files.clean_temporary()?;
        }
        let snap_label = ctx.label(&files.snap);
        let (state, generation, snap_bytes, snap_plain, snap_level) = match ctx.read(&files.snap)? {
            Some(b) => {
                let body = check_header(&b, S::KIND, &snap_label)?;
                let (generation, level) = parse_gen_level(body, &snap_label)?;
                let plain = codec::decompress(&body[9..]).map_err(|e| e.in_file(&snap_label))?;
                let state = S::decode_state(&plain).map_err(|e| e.in_file(&snap_label))?;
                (state, generation, b.len() as u64, plain.len() as u64, level)
            }
            None => (S::empty(), 0, 0, 0, LEVEL_WARM),
        };
        let mut lf = LoggedFile { state, files, generation, journal: None, snap_bytes, snap_plain, snap_level };
        let log_label = ctx.label(&lf.files.log);
        let found = if to_commit {
            Journal::open(ctx, &lf.files.log)?.map(|o| (o.generation, o.entries, Some(o.journal)))
        } else {
            Journal::read_only(ctx, &lf.files.log, false)?.map(|r| (r.generation, r.entries, None))
        };
        if let Some((generation, entries, journal)) = found {
            if generation < lf.generation {
                // folded into the snapshot already; a crash came before it was removed (another process's
                // reader leaves it to the owner)
                drop(journal);
                if tidy {
                    remove_if_exists(&lf.files.log)?;
                }
            } else if generation > lf.generation {
                return Err(Error::Damaged {
                    file: log_label,
                    why: format!("it follows snapshot {} but the snapshot is {}", generation, lf.generation),
                });
            } else {
                for e in &entries {
                    lf.state.apply(S::decode_op(e).map_err(|e| e.in_file(&log_label))?);
                }
                lf.journal = journal;
            }
        }
        Ok(lf)
    }

    /// Records a change, then applies it. Fold if due.
    pub(crate) fn commit(&mut self, ctx: &Ctx, op: S::Op) -> Result<()> {
        let bytes = S::encode_op(ctx, &op)?;
        let j = match &mut self.journal {
            Some(j) => j,
            None => self.journal.insert(Journal::create(ctx, &self.files.log, self.generation)?),
        };
        j.append(&bytes)?;
        let (jb, je) = (j.bytes, j.entries);
        self.state.apply(op);
        if S::fold_due(ctx, jb, je, self.snap_bytes, self.snap_plain) {
            self.fold(ctx, S::FOLD)?;
        }
        Ok(())
    }

    /// Writes the state as a new snapshot and drops the journal.
    pub(crate) fn fold(&mut self, ctx: &Ctx, which: Which) -> Result<()> {
        let plain = self.state.encode_state()?;
        let packed = codec::compress(&plain, ctx.level(which))?;
        let level = if which == Which::Cold { LEVEL_COLD } else { LEVEL_WARM };
        let mut b = header(S::KIND);
        b.extend_from_slice(&(self.generation + 1).to_be_bytes());
        b.push(level);
        b.extend_from_slice(&packed);
        ctx.write_atomic(&self.files.snap, &b)?;
        self.generation += 1;
        self.snap_bytes = b.len() as u64;
        self.snap_plain = plain.len() as u64;
        self.snap_level = level;
        self.journal = None;
        ctx.crash_point(crate::Crash::BeforeJournalRemoved)?;
        remove_if_exists(&self.files.log)?;
        if let Some(d) = self.files.log.parent() {
            sync_dir(d);
        }
        Ok(())
    }

    /// Whether the state is all in a cold snapshot (nothing in the journal).
    pub(crate) fn settled(&self) -> bool {
        self.journal.is_none() && self.snap_level == LEVEL_COLD
    }

    /// About how much memory this holds (the state's size as stored, plus the journal).
    pub(crate) fn weight(&self) -> u64 {
        self.snap_plain + self.journal.as_ref().map_or(0, |j| j.bytes)
    }

    /// Removes both files (crash-safe: an empty snapshot of the next generation first, so a crash midway leaves an
    /// empty state, never the old one back).
    pub(crate) fn remove_files(mut self, ctx: &Ctx) -> Result<()> {
        self.state = S::empty();
        self.fold(ctx, Which::Hot)?;
        remove_if_exists(&self.files.snap)?;
        Ok(())
    }
}

/// Appends a change to a state without reading it (for segment days written often and read now and then). Returns
/// whether the journal is due to be folded.
pub(crate) fn append_only<S: Logged>(ctx: &Ctx, files: &Files, op: &S::Op) -> Result<bool> {
    files.clean_temporary()?;
    let (generation, snap_bytes) = match snapshot_head(ctx, files, S::KIND)? {
        Some((generation, _, len)) => (generation, len),
        None => (0, 0),
    };
    let mut journal = match Journal::open_for_append(ctx, &files.log)? {
        Some(o) if o.generation == generation => o.journal,
        Some(o) if o.generation < generation => {
            drop(o);
            remove_if_exists(&files.log)?;
            Journal::create(ctx, &files.log, generation)?
        }
        Some(o) => {
            return Err(Error::Damaged {
                file: ctx.label(&files.log),
                why: format!("it follows snapshot {} but the snapshot is {generation}", o.generation),
            });
        }
        None => Journal::create(ctx, &files.log, generation)?,
    };
    journal.append(&S::encode_op(ctx, op)?)?;
    Ok(S::fold_due(ctx, journal.bytes, journal.entries, snap_bytes, 0))
}
