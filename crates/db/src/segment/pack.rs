//! A month of a series' finished days in one sealed file, so a year of history is some tens of files a series, not
//! hundreds: every file takes whole blocks on disk and carries the vault's overhead, and many days are tiny (a
//! charge curve of 200 bytes takes 4 KiB as a file of its own).
//!
//! ```text
//! <series>/<YYYY-MM>.pack = "SMDB" | kind 11 | version | count (u8, at most 31)
//!                           | count × (day u32 | generation u64 | level u8 | length u32 | hash u64)   in day order
//!                           | the bodies, in that order
//! ```
//!
//! (Numbers big endian; `day` counts from 1970-01-01.) A body is what follows the generation and level in the day's
//! own `<day>.snap`: the compressed columns, copied as they are, never unpacked or compressed again, so taking a day
//! out of a pack gives back the very bytes of its snapshot. `hash` is the first 8 bytes of the body's SHA-256,
//! checked when a body is taken out; with the generation and length it also says what a day is in the pack whatever
//! the file around it ([`crate::plain::Stamp::packed`]).
//!
//! **Days packed together** (version 2; a pack without any is written as version 1, as ever). A day's columns
//! compressed by themselves start from nothing each day; a month's days compressed as one stream are a quarter to a
//! half smaller (measured, docs/plans/storage.md). So [`group_month`] puts runs of neighbouring days together:
//!
//! ```text
//! level 3   the first day of a run: its body is the run's stream, the columns of this day and of the days after it
//!           that have level 4, one after the other, compressed as one; length and hash are the stream's
//! level 4   another day of the run before it: no bytes of its own; length and hash are of its columns, unpacked
//!           (the first day's columns are what is left of the stream before the others')
//! ```
//!
//! A run holds about [`crate::Options::group_bytes`] of columns (2 MiB) at most, so reading one day unpacks at most
//! that much (a few milliseconds), and the run's other days are kept unpacked with it, since a page reads them next
//! ([`Opened::unpack`]). A run that loses a day (the day was written again, or emptied) is taken apart by
//! [`pack_month`]: its other days stand by themselves again until [`group_month`] next passes.
//!
//! **A day's own snapshot always wins over the pack's entry for it**: a day written again after it was packed gets a
//! journal (of the generation the pack keeps for it), which folds into a snapshot file of the next generation; the
//! next pass takes that into the pack. A day emptied while a pack holds it keeps an empty snapshot file until the
//! pass that drops it from the pack.
//!
//! Packing ([`pack_month`], under the car's lock) never leaves a day unreadable: the new pack is sealed to
//! `<pack>.tmp` and synced, read back from disk, opened, and every body compared with where it came from (the day's
//! snapshot read from disk again, or the pack before); only then is it renamed into place, and only after that are
//! the snapshots it took in removed. A crash before the rename leaves the days as they were (the temporary file is
//! removed by the next pass), one after it leaves days that are in both, the same bytes, which the next pass
//! removes. Readers hold no lock for a day without a journal: they look for the day's own snapshot first and then in
//! the pack, and the day is always in at least one of the two.
//!
//! **Days packed before there was the small form** (`level` 1: the quick columns, see [`super::columns`]) are written
//! again on the way, a few a turn: each is unpacked, its points encoded small, and the pack written anew under the
//! same rule (sealed to `<pack>.tmp`, synced, read back from disk, opened; every day written again is decoded from
//! that copy and compared with the points the old pack gives, bit for bit, a day only put together with others is
//! compared with its columns byte for byte, and every other day with its bytes; then the rename). There is nothing
//! to remove afterwards: a crash before the rename leaves the old pack, one after it the new. A day keeps its
//! generation (its points are the same), and its `level` says it is done (3 or 4; 2 where days are not packed
//! together, [`recode_month`]), so a later pass sees it in the index and does not unpack it.
//!
//! Indexes are kept in memory by what the pack file was when read (only the file's first chunk is opened for one),
//! so listing a series costs a `stat` per month; the few packs opened last are kept whole (wiped when let go), since
//! a chart of a year reads a month's days one after the other.

use super::{DayLog, Point, civil, day_files, day_name, days_from_civil};
use crate::Crash;
use crate::codec;
use crate::error::{Error, Result};
use crate::file::{
    Ctx, HEADER, Kind, Which, check_header, check_header_upto, header, private_create, remove_if_exists, sync_dir,
    with_suffix,
};
use crate::journal::Files;
use crate::logged::{Elsewhere, LEVEL_COLD, LEVEL_SMALL, Logged, LoggedFile, SNAP_HEAD, parse_gen_level};
use crate::plain::{Stamp, stamp};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use zeroize::Zeroizing;

/// Days a pack holds at most (a month).
const MAX_DAYS: usize = 31;
/// An index entry: day, generation, level, length, hash.
const ENTRY: usize = 4 + 8 + 1 + 4 + 8;
/// The plaintext an index fits in.
const INDEX_MAX: usize = HEADER + 1 + MAX_DAYS * ENTRY;

/// A calendar month (UTC): its first day and the day after its last, in days since 1970-01-01.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct Month {
    pub first: u64,
    pub end: u64,
}

impl Month {
    /// The month a day is in.
    pub(crate) fn of(day: u64) -> Month {
        let (y, m, _) = civil(day as i64);
        Month::at(y, m)
    }

    fn at(y: i64, m: u32) -> Month {
        let (ny, nm) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
        Month { first: days_from_civil(y, m, 1) as u64, end: days_from_civil(ny, nm, 1) as u64 }
    }

    /// `YYYY-MM`.
    pub(crate) fn name(&self) -> String {
        day_name(self.first)[..7].to_string()
    }

    /// The month a pack's file name (without `.pack`) names.
    pub(crate) fn parse(name: &str) -> Option<Month> {
        let b = name.as_bytes();
        if b.len() != 7 || b[4] != b'-' {
            return None;
        }
        let y: i64 = name[..4].parse().ok()?;
        let m: u32 = name[5..].parse().ok()?;
        if !(1..=12).contains(&m) || days_from_civil(y, m, 1) < 0 {
            return None;
        }
        let month = Month::at(y, m);
        (month.name() == name).then_some(month)
    }

    pub(crate) fn file(&self, dir: &Path) -> PathBuf {
        dir.join(format!("{}.pack", self.name()))
    }
}

/// A day packed together with others (see the module docs): the first of a run, and another day of one.
pub(crate) const LEVEL_GROUP: u8 = 3;
pub(crate) const LEVEL_MEMBER: u8 = 4;
/// The version of a pack that has days packed together.
const VERSION_GROUPS: u16 = 2;

/// A day in a pack.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Entry {
    pub day: u64,
    pub generation: u64,
    pub level: u8,
    /// Bytes of its body (the first day of a run packed together: of the run's stream; another day of a run: of
    /// its columns, unpacked).
    pub len: u64,
    pub hash: u64,
    /// About what the day takes in the pack (a day of a run: its share of the stream).
    pub bytes: u64,
    /// Where its body (or its run's stream) starts in the pack's plaintext.
    offset: usize,
    /// A day of a run packed together: where the run's first day is among the entries, and how many days it has.
    group: Option<(usize, usize)>,
}

impl Entry {
    /// What the day is in the pack, whatever the file around it.
    pub(crate) fn stamp(&self) -> Stamp {
        Stamp::packed(self.generation, self.len, self.hash)
    }

    /// About what the day would take as a snapshot of its own.
    pub(crate) fn snapshot_len(&self) -> u64 {
        self.bytes + SNAP_HEAD as u64
    }
}

/// A pack's entries, in day order.
#[derive(Debug, Default)]
pub(crate) struct Index {
    pub entries: Vec<Entry>,
}

impl Index {
    pub(crate) fn get(&self, day: u64) -> Option<&Entry> {
        self.entries.binary_search_by_key(&day, |e| e.day).ok().map(|i| &self.entries[i])
    }
}

/// The first 8 bytes of a body's SHA-256.
pub(crate) fn content_hash(body: &[u8]) -> u64 {
    let d = Sha256::digest(body);
    let mut h = [0u8; 8];
    h.copy_from_slice(&d[..8]);
    u64::from_be_bytes(h)
}

fn damaged(label: &str, why: impl Into<String>) -> Error {
    Error::Damaged { file: label.to_string(), why: why.into() }
}

/// Reads an index from a pack's plaintext (or its start, when `whole` is false). With the whole pack the bodies'
/// lengths must add up to the file's.
fn parse_index(plain: &[u8], month: Month, label: &str, whole: bool) -> Result<Index> {
    let (version, b) = check_header_upto(plain, Kind::SegmentPack, label, VERSION_GROUPS)?;
    let count = usize::from(*b.first().ok_or_else(|| damaged(label, "pack index cut short"))?);
    if count > MAX_DAYS {
        return Err(damaged(label, "more days than a month has"));
    }
    let table = b.get(1..1 + count * ENTRY).ok_or_else(|| damaged(label, "pack index cut short"))?;
    let mut offset = HEADER + 1 + count * ENTRY;
    let mut entries: Vec<Entry> = Vec::with_capacity(count);
    for (i, e) in table.as_chunks::<ENTRY>().0.iter().enumerate() {
        let u32_at = |i: usize| u64::from(u32::from_be_bytes([e[i], e[i + 1], e[i + 2], e[i + 3]]));
        let u64_at = |i: usize| {
            let mut v = [0u8; 8];
            v.copy_from_slice(&e[i..i + 8]);
            u64::from_be_bytes(v)
        };
        let len = u32_at(13);
        let mut entry = Entry {
            day: u32_at(0),
            generation: u64_at(4),
            level: e[12],
            len,
            hash: u64_at(17),
            bytes: len,
            offset,
            group: None,
        };
        if !(month.first..month.end).contains(&entry.day) || entries.last().is_some_and(|l| l.day >= entry.day) {
            return Err(damaged(label, "its days are not this month's, in order"));
        }
        if entry.level >= LEVEL_GROUP && version < VERSION_GROUPS {
            return Err(damaged(label, "days packed together in a pack that has none"));
        }
        match entry.level {
            LEVEL_GROUP => {
                entry.group = Some((i, 1));
                offset += len as usize;
            }
            LEVEL_MEMBER => {
                let first = entries
                    .last()
                    .and_then(|l| l.group)
                    .map(|g| g.0)
                    .ok_or_else(|| damaged(label, "a day packed together with none before it"))?;
                entry.group = Some((first, 1));
                entry.offset = entries[first].offset;
            }
            _ => offset += len as usize,
        }
        entries.push(entry);
    }
    // each run's days know how many they are, and share its bytes
    let mut i = 0;
    while i < entries.len() {
        let Some((first, _)) = entries[i].group else {
            i += 1;
            continue;
        };
        let days = entries[first..].iter().take_while(|e| e.group.map(|g| g.0) == Some(first)).count();
        let share = entries[first].len / days as u64;
        for e in &mut entries[first..first + days] {
            e.group = Some((first, days));
            e.bytes = share;
        }
        i = first + days;
    }
    if whole && offset != plain.len() {
        return Err(damaged(label, "its days' lengths don't add up to its own"));
    }
    Ok(Index { entries })
}

/// A day to put in a pack.
struct Part<'a> {
    day: u64,
    generation: u64,
    level: u8,
    /// Its body (the first day of a run packed together: the run's stream); none for another day of a run.
    body: &'a [u8],
    /// Another day of a run: the length and hash of its columns, unpacked.
    member: Option<(u64, u64)>,
}

impl<'a> Part<'a> {
    /// A day with a body of its own.
    fn alone(day: u64, generation: u64, level: u8, body: &'a [u8]) -> Part<'a> {
        Part { day, generation, level, body, member: None }
    }

    /// Another day of the run before it.
    fn member(day: u64, generation: u64, len: u64, hash: u64) -> Part<'a> {
        Part { day, generation, level: LEVEL_MEMBER, body: &[], member: Some((len, hash)) }
    }

    /// A day as a pack has it, with its body there (none for another day of a run).
    fn kept(e: &Entry, body: &'a [u8]) -> Part<'a> {
        match e.level {
            LEVEL_MEMBER => Part::member(e.day, e.generation, e.len, e.hash),
            _ => Part::alone(e.day, e.generation, e.level, body),
        }
    }
}

/// A pack's plaintext for these days, given in day order.
fn build(parts: &[Part<'_>]) -> Result<Zeroizing<Vec<u8>>> {
    if parts.len() > MAX_DAYS {
        return Err(Error::invalid("a pack holds one month"));
    }
    let bodies: usize = parts.iter().map(|p| p.body.len()).sum();
    let mut b = Zeroizing::new(Vec::with_capacity(INDEX_MAX + bodies));
    b.extend_from_slice(&header(Kind::SegmentPack));
    if parts.iter().any(|p| p.level >= LEVEL_GROUP) {
        b[5..7].copy_from_slice(&VERSION_GROUPS.to_be_bytes());
    }
    b.push(parts.len() as u8);
    for p in parts {
        if (p.level == LEVEL_MEMBER) != p.member.is_some() || (p.member.is_some() && !p.body.is_empty()) {
            return Err(Error::invalid("a day of a run packed with a body of its own"));
        }
        let (len, hash) = p.member.unwrap_or_else(|| (p.body.len() as u64, content_hash(p.body)));
        let (Ok(day), Ok(len)) = (u32::try_from(p.day), u32::try_from(len)) else {
            return Err(Error::invalid("a day too large to pack"));
        };
        b.extend_from_slice(&day.to_be_bytes());
        b.extend_from_slice(&p.generation.to_be_bytes());
        b.push(p.level);
        b.extend_from_slice(&len.to_be_bytes());
        b.extend_from_slice(&hash.to_be_bytes());
    }
    for p in parts {
        b.extend_from_slice(p.body);
    }
    Ok(b)
}

/// A pack opened whole.
pub(crate) struct Opened {
    pub index: Index,
    label: String,
    plain: Zeroizing<Vec<u8>>,
}

impl Opened {
    fn parse(plain: Zeroizing<Vec<u8>>, month: Month, label: &str) -> Result<Opened> {
        let index = parse_index(&plain, month, label, true)?;
        Ok(Opened { index, label: label.to_string(), plain })
    }

    /// A day's body as it is in the pack (not checked against its hash); nothing for a day that has none of its own.
    fn raw(&self, e: &Entry) -> &[u8] {
        match e.level {
            LEVEL_MEMBER => &[],
            _ => self.plain.get(e.offset..e.offset + e.len as usize).unwrap_or(&[]),
        }
    }

    /// A day's body (the first day of a run packed together: the run's stream), checked against its hash.
    pub(crate) fn body(&self, e: &Entry) -> Result<&[u8]> {
        let b = self.raw(e);
        if e.level == LEVEL_MEMBER || b.len() as u64 != e.len || content_hash(b) != e.hash {
            return Err(damaged(&self.label, format!("the day {} in it is not what its index says", day_name(e.day))));
        }
        Ok(b)
    }

    /// The days packed together with a day (itself among them, in day order); a day by itself: only that day.
    fn run(&self, e: &Entry) -> Vec<Entry> {
        match e.group {
            Some((first, days)) => {
                self.index.entries.get(first..first + days).map(<[Entry]>::to_vec).unwrap_or_default()
            }
            None => vec![*e],
        }
    }

    /// A day's columns unpacked, and those of the days packed together with it, in day order.
    pub(crate) fn unpack(&self, e: &Entry) -> Result<Vec<(Entry, Vec<u8>)>> {
        let run = self.run(e);
        let Some(first) = run.first() else {
            return Err(damaged(&self.label, format!("the day {} in it is in no run", day_name(e.day))));
        };
        let mut all = codec::decompress(self.body(first)?).map_err(|err| err.in_file(&self.label))?;
        if run.len() == 1 {
            return Ok(vec![(*first, all)]);
        }
        // the first day's columns are what the others leave
        let others: u64 = run[1..].iter().map(|d| d.len).sum();
        let Some(first_len) = (all.len() as u64).checked_sub(others) else {
            let why = format!("the days packed with {} are not as long as its index says", day_name(first.day));
            return Err(damaged(&self.label, why));
        };
        let mut out = Vec::with_capacity(run.len());
        for d in run.iter().rev() {
            let len = if d.day == first.day { first_len } else { d.len } as usize;
            let plain = all.split_off(all.len() - len);
            out.push((*d, plain));
        }
        out.reverse();
        Ok(out)
    }

    /// One day's columns, unpacked.
    fn unpack_day(&self, e: &Entry) -> Result<Vec<u8>> {
        let found = self.unpack(e)?.into_iter().find(|(d, _)| d.day == e.day).map(|(_, plain)| plain);
        found.ok_or_else(|| damaged(&self.label, format!("the day {} in it is in no run", day_name(e.day))))
    }
}

/// Indexes kept at most (about 1 KiB each).
const MAX_INDEXES: usize = 4096;
/// Packs kept opened at most, and their plaintext bytes in all.
const MAX_OPENED: usize = 4;
const MAX_OPENED_BYTES: usize = 32 << 20;

#[derive(Default)]
struct Kept {
    tick: u64,
    indexes: HashMap<PathBuf, (Stamp, u64, Arc<Index>)>,
    opened: Vec<(PathBuf, Stamp, u64, Arc<Opened>)>,
}

/// Pack indexes, and the packs opened last, by what the file was when read.
#[derive(Default)]
pub(crate) struct PackCache {
    kept: Mutex<Kept>,
}

impl PackCache {
    fn lock(&self) -> std::sync::MutexGuard<'_, Kept> {
        self.kept.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn index(&self, file: &Path, st: Stamp) -> Option<Arc<Index>> {
        let mut k = self.lock();
        k.tick += 1;
        let tick = k.tick;
        let e = k.indexes.get_mut(file).filter(|e| e.0 == st)?;
        e.1 = tick;
        Some(e.2.clone())
    }

    fn put_index(&self, file: &Path, st: Stamp, index: Arc<Index>) {
        let mut k = self.lock();
        k.tick += 1;
        let tick = k.tick;
        if k.indexes.len() >= MAX_INDEXES && !k.indexes.contains_key(file) {
            // the least recently used quarter goes
            let mut used: Vec<u64> = k.indexes.values().map(|e| e.1).collect();
            used.sort_unstable();
            let cut = used[used.len() / 4];
            k.indexes.retain(|_, e| e.1 > cut);
        }
        k.indexes.insert(file.to_path_buf(), (st, tick, index));
    }

    fn opened(&self, file: &Path, st: Stamp) -> Option<Arc<Opened>> {
        let mut k = self.lock();
        k.tick += 1;
        let tick = k.tick;
        let e = k.opened.iter_mut().find(|e| e.0 == file && e.1 == st)?;
        e.2 = tick;
        Some(e.3.clone())
    }

    fn put_opened(&self, file: &Path, st: Stamp, opened: Arc<Opened>) {
        if opened.plain.len() > MAX_OPENED_BYTES / 2 {
            return;
        }
        let mut k = self.lock();
        k.tick += 1;
        let tick = k.tick;
        k.opened.retain(|e| e.0 != file);
        k.opened.push((file.to_path_buf(), st, tick, opened));
        while k.opened.len() > MAX_OPENED || k.opened.iter().map(|e| e.3.plain.len()).sum::<usize>() > MAX_OPENED_BYTES
        {
            let Some(oldest) = k.opened.iter().enumerate().min_by_key(|(_, e)| e.2).map(|(i, _)| i) else { break };
            k.opened.swap_remove(oldest);
        }
    }

    /// Forgets a pack (it was written again or removed).
    fn forget(&self, file: &Path) {
        let mut k = self.lock();
        k.indexes.remove(file);
        k.opened.retain(|e| e.0 != file);
    }
}

/// A month's index: from memory while the pack file is the one it was read from, else from the file's first chunk.
/// `Ok(None)`: the month has no pack.
pub(crate) fn index(ctx: &Ctx, dir: &Path, month: Month) -> Result<Option<Arc<Index>>> {
    let file = month.file(dir);
    let Some(st) = stamp(&file)? else { return Ok(None) };
    if let Some(i) = ctx.packs.index(&file, st) {
        return Ok(Some(i));
    }
    let label = ctx.label(&file);
    let Some(head) = ctx.read_head(&file, INDEX_MAX)? else { return Ok(None) };
    let index = Arc::new(parse_index(&head, month, &label, false)?);
    // kept only when the file read is the one looked at (a pack written meanwhile is read again next time)
    if stamp(&file)? == Some(st) {
        ctx.packs.put_index(&file, st, index.clone());
    }
    Ok(Some(index))
}

/// A day's entry in its month's pack, if it has one.
pub(crate) fn entry(ctx: &Ctx, dir: &Path, day: u64) -> Result<Option<Entry>> {
    Ok(index(ctx, dir, Month::of(day))?.and_then(|i| i.get(day).copied()))
}

/// The entries of the months asked for last, for a walk over a series' days in order: one look at each pack.
pub(crate) struct Packs<'a> {
    ctx: &'a Ctx,
    dir: &'a Path,
    last: Option<(Month, Option<Arc<Index>>)>,
}

impl<'a> Packs<'a> {
    pub(crate) fn new(ctx: &'a Ctx, dir: &'a Path) -> Packs<'a> {
        Packs { ctx, dir, last: None }
    }

    pub(crate) fn entry(&mut self, day: u64) -> Result<Option<Entry>> {
        let month = Month::of(day);
        let index = match &self.last {
            Some((m, i)) if *m == month => i.clone(),
            Some(_) | None => {
                let i = index(self.ctx, self.dir, month)?;
                self.last = Some((month, i.clone()));
                i
            }
        };
        Ok(index.and_then(|i| i.get(day).copied()))
    }
}

/// A month's pack opened whole, from the packs opened last while the file is the same. `Ok(None)`: no pack.
fn open(ctx: &Ctx, dir: &Path, month: Month) -> Result<Option<Arc<Opened>>> {
    let file = month.file(dir);
    let Some(st) = stamp(&file)? else { return Ok(None) };
    if let Some(o) = ctx.packs.opened(&file, st) {
        return Ok(Some(o));
    }
    let Some(plain) = ctx.read(&file)? else { return Ok(None) };
    let opened = Arc::new(Opened::parse(plain, month, &ctx.label(&file))?);
    if stamp(&file)? == Some(st) {
        ctx.packs.put_opened(&file, st, opened.clone());
    }
    Ok(Some(opened))
}

/// A day's entry and its pack, opened. `Ok(None)`: the day is in no pack.
pub(crate) fn find(ctx: &Ctx, dir: &Path, day: u64) -> Result<Option<(Entry, Arc<Opened>)>> {
    let Some(opened) = open(ctx, dir, Month::of(day))? else { return Ok(None) };
    Ok(opened.index.get(day).copied().map(|e| (e, opened)))
}

/// The key a packed day's unpacked body is kept under in [`crate::plain`].
pub(crate) fn plain_key(dir: &Path, day: u64) -> PathBuf {
    with_suffix(&Month::of(day).file(dir), &format!("#{}", day_name(day)))
}

/// A packed day as a snapshot kept elsewhere, for [`LoggedFile::load_or`].
pub(crate) fn elsewhere(ctx: &Ctx, dir: &Path, day: u64) -> Result<Option<Elsewhere>> {
    let Some((e, opened)) = find(ctx, dir, day)? else { return Ok(None) };
    let plain = opened.unpack_day(&e)?;
    Ok(Some(Elsewhere { label: opened.label.clone(), generation: e.generation, level: e.level, plain, bytes: e.bytes }))
}

/// A day's own snapshot file, read whole.
struct Loose {
    generation: u64,
    level: u8,
    plain: Zeroizing<Vec<u8>>,
}

impl Loose {
    fn read(ctx: &Ctx, files: &Files) -> Result<Option<Loose>> {
        let label = ctx.label(&files.snap);
        let Some(plain) = ctx.read(&files.snap)? else { return Ok(None) };
        let (generation, level) = parse_gen_level(check_header(&plain, Kind::Segment, &label)?, &label)?;
        Ok(Some(Loose { generation, level, plain }))
    }

    fn body(&self) -> &[u8] {
        self.plain.get(SNAP_HEAD..).unwrap_or(&[])
    }
}

/// Where a day of the new pack comes from.
enum From {
    /// Its own snapshot file, taken in.
    Loose(Files, Loose),
    /// The pack before: a day by itself, or the first day of a run packed together that stays whole (its stream).
    Pack(Entry),
    /// The pack before: another day of a run that stays whole.
    Member(Entry),
    /// The pack before: a day of a run that is taken apart, by itself again (its columns, and them compressed).
    Apart { generation: u64, plain: Vec<u8>, body: Vec<u8> },
}

/// Packs a month of a series (see the module docs): the days' own cold snapshots without a journal go into the
/// month's pack, over what the pack had for them; days emptied since are dropped from it. Returns how many snapshot
/// files it took in or dropped. Under the car's lock. A pack that doesn't read is reported, and nothing is removed.
pub(crate) fn pack_month<P: Point>(ctx: &Ctx, dir: &Path, month: Month) -> Result<usize> {
    let file = month.file(dir);
    let label = ctx.label(&file);
    let tmp = with_suffix(&file, ".tmp");
    remove_if_exists(&tmp)?;
    // the pack as it is on disk now
    let old = match ctx.read(&file)? {
        Some(plain) => Some(Opened::parse(plain, month, &label)?),
        None => None,
    };
    let mut taken: Vec<(u64, Files, Loose)> = Vec::new();
    let mut emptied: Vec<(u64, Files)> = Vec::new();
    for day in month.first..month.end {
        let files = day_files(dir, day);
        if files.log.exists() {
            // written since: not finished
            continue;
        }
        let Some(mut loose) = Loose::read(ctx, &files)? else { continue };
        if loose.level < LEVEL_COLD {
            // an emptied day, or one folded while it was written and never settled (a crash before that was noted)
            let snap_label = ctx.label(&files.snap);
            let plain = codec::decompress(loose.body()).map_err(|e| e.in_file(&snap_label))?;
            if DayLog::<P>::decode_state(&plain).map_err(|e| e.in_file(&snap_label))?.points.is_empty() {
                emptied.push((day, files));
                continue;
            }
            LoggedFile::<DayLog<P>>::load(ctx, files.clone())?.fold(ctx, Which::Cold)?;
            loose = Loose::read(ctx, &files)?.ok_or_else(|| damaged(&snap_label, "is gone while it was packed"))?;
        }
        taken.push((day, files, loose));
    }
    if taken.is_empty() && emptied.is_empty() {
        return Ok(0);
    }
    let old_entry = |day: u64| old.as_ref().and_then(|o| o.index.get(day).copied());
    // whether the pack already is what it would become (a crash after it was written, before the files were removed)
    let mut same = emptied.iter().all(|(day, _)| old_entry(*day).is_none());
    for (day, _, loose) in &taken {
        same &= match (old_entry(*day), &old) {
            (Some(e), Some(o)) => {
                e.group.is_none()
                    && e.generation == loose.generation
                    && e.level == loose.level
                    && o.raw(&e) == loose.body()
            }
            (Some(_), None) | (None, Some(_)) | (None, None) => false,
        };
    }
    let n = taken.len() + emptied.len();
    let emptied_days: Vec<u64> = emptied.iter().map(|e| e.0).collect();
    let mut loose_files: Vec<PathBuf> = emptied.into_iter().map(|(_, f)| f.snap).collect();
    if !same {
        let mut days: Vec<(u64, From)> = Vec::new();
        if let Some(o) = &old {
            let replaced = |day: u64| taken.iter().any(|t| t.0 == day) || emptied_days.contains(&day);
            // a run packed together stays whole unless a day comes or goes inside it; then each of its days
            // stands by itself again
            let changed = |run: &[Entry]| match (run.first(), run.last()) {
                (Some(a), Some(b)) => {
                    let inside = |day: u64| (a.day..=b.day).contains(&day);
                    taken.iter().any(|t| inside(t.0)) || emptied_days.iter().any(|d| inside(*d))
                }
                _ => true,
            };
            let mut apart: HashMap<u64, Vec<u8>> = HashMap::new();
            for e in o.index.entries.iter().filter(|e| !replaced(e.day)) {
                let from = match e.group {
                    None => From::Pack(*e),
                    Some(_) if !changed(&o.run(e)) => match e.level {
                        LEVEL_MEMBER => From::Member(*e),
                        _ => From::Pack(*e),
                    },
                    Some(_) => {
                        if !apart.contains_key(&e.day) {
                            apart.extend(o.unpack(e)?.into_iter().map(|(d, plain)| (d.day, plain)));
                        }
                        let plain = apart.remove(&e.day).unwrap_or_default();
                        let body = codec::compress(&plain, ctx.opts.small)?;
                        From::Apart { generation: e.generation, plain, body }
                    }
                };
                days.push((e.day, from));
            }
        }
        days.extend(taken.into_iter().map(|(day, files, loose)| (day, From::Loose(files, loose))));
        days.sort_unstable_by_key(|d| d.0);
        if days.is_empty() {
            // every day it had is emptied: the empty snapshots stand for them until they are removed below
            remove_if_exists(&file)?;
            sync_dir(dir);
        } else {
            write_checked(ctx, &file, &tmp, &label, month, &days, old.as_ref())?;
        }
        ctx.packs.forget(&file);
        loose_files.extend(days.into_iter().filter_map(|(_, from)| match from {
            From::Loose(files, _) => Some(files.snap),
            From::Pack(_) | From::Member(_) | From::Apart { .. } => None,
        }));
    } else {
        loose_files.extend(taken.into_iter().map(|(_, files, _)| files.snap));
    }
    ctx.crash_point(Crash::BeforeLooseRemoved)?;
    for f in &loose_files {
        remove_if_exists(f)?;
    }
    sync_dir(dir);
    Ok(n)
}

/// Writes a pack of `days` in place of `file`: sealed to `tmp` and synced, read back and compared with where every
/// day came from, then renamed. Anything that doesn't match removes `tmp` and leaves the rest as it is.
fn write_checked(
    ctx: &Ctx,
    file: &Path,
    tmp: &Path,
    label: &str,
    month: Month,
    days: &[(u64, From)],
    old: Option<&Opened>,
) -> Result<()> {
    let body_of = |from: &'_ From| -> Result<Vec<u8>> {
        match (from, old) {
            (From::Loose(_, loose), _) => Ok(loose.body().to_vec()),
            (From::Pack(e), Some(o)) => Ok(o.body(e)?.to_vec()),
            (From::Member(_), Some(_)) => Ok(Vec::new()),
            (From::Apart { body, .. }, _) => Ok(body.clone()),
            (From::Pack(_) | From::Member(_), None) => Err(Error::invalid("a day from a pack that isn't there")),
        }
    };
    {
        let bodies = days.iter().map(|(_, from)| body_of(from)).collect::<Result<Vec<_>>>()?;
        let parts: Vec<Part<'_>> = days
            .iter()
            .zip(&bodies)
            .map(|((day, from), body)| match from {
                From::Loose(_, loose) => Part::alone(*day, loose.generation, loose.level, body),
                From::Pack(e) | From::Member(e) => Part::kept(e, body),
                From::Apart { generation, .. } => Part::alone(*day, *generation, LEVEL_SMALL, body),
            })
            .collect();
        let sealed = ctx.vault.seal(label, &build(&parts)?)?;
        let mut f = private_create(tmp)?;
        f.write_all(&sealed)?;
        f.sync_all()?;
    }
    let check = || -> Result<()> {
        // what is on disk now, opened as the pack will be
        let sealed = std::fs::read(tmp)?;
        let plain = ctx.vault.open_sealed(label, &sealed).map_err(|e| Error::from(e).in_file(label))?;
        let new = Opened::parse(plain, month, label)?;
        let wrong = |day: u64| {
            damaged(label, format!("its copy of {} is not the day's snapshot; nothing was removed", day_name(day)))
        };
        if new.index.entries.len() != days.len() {
            return Err(damaged(label, "its days are not the ones packed; nothing was removed"));
        }
        for (e, (day, from)) in new.index.entries.iter().zip(days) {
            let ok = e.day == *day
                && match from {
                    From::Loose(files, _) => {
                        // the day's snapshot, read from disk again
                        let again = Loose::read(ctx, files)?.ok_or_else(|| wrong(*day))?;
                        (e.generation, e.level) == (again.generation, again.level)
                            && new.body(e).is_ok_and(|packed| packed == again.body())
                    }
                    From::Pack(was) => {
                        (e.generation, e.level) == (was.generation, was.level)
                            && old
                                .is_some_and(|o| o.body(was).is_ok_and(|b| new.body(e).is_ok_and(|packed| b == packed)))
                    }
                    From::Member(was) => {
                        (e.generation, e.level, e.len, e.hash) == (was.generation, was.level, was.len, was.hash)
                    }
                    From::Apart { generation, plain, .. } => {
                        (e.generation, e.level) == (*generation, LEVEL_SMALL)
                            && new.body(e).is_ok_and(|b| codec::decompress(b).is_ok_and(|p| p == *plain))
                    }
                };
            if !ok {
                return Err(wrong(*day));
            }
        }
        Ok(())
    };
    if let Err(e) = check() {
        remove_if_exists(tmp)?;
        return Err(e);
    }
    ctx.crash_point(Crash::BeforeRename)?;
    std::fs::rename(tmp, file)?;
    sync_dir(file.parent().unwrap_or(file));
    Ok(())
}

/// How many days of a month's pack are not yet as small as they go (from its index alone): not packed together
/// with their neighbours or, where days are not packed together, not in the small form.
pub(crate) fn stale(ctx: &Ctx, dir: &Path, month: Month) -> Result<usize> {
    if !ctx.opts.small_columns {
        return Ok(0);
    }
    below(ctx, dir, month, if ctx.opts.group_bytes > 0 { LEVEL_GROUP } else { LEVEL_SMALL })
}

/// How many days of a month's pack are below a level (from its index alone).
pub(crate) fn below(ctx: &Ctx, dir: &Path, month: Month, level: u8) -> Result<usize> {
    Ok(index(ctx, dir, month)?.map_or(0, |i| i.entries.iter().filter(|e| e.level < level).count()))
}

/// Unpacked bytes of days written again per call of [`recode_month`] or [`group_month`], about (they hold the
/// car's lock): a second or two of work. A call always finishes the run of days it began, so it may go one run
/// ([`crate::Options::group_bytes`]) past this.
const RECODE_PLAIN_MAX: usize = 1 << 20;

/// A pack written again in place of `file`, by the rule of the module docs: sealed to `tmp` and synced, read back
/// from the disk and opened, `check` asked whether it is what the old pack was; then the rename. Anything that
/// doesn't match removes `tmp` and leaves the pack as it is.
fn replace_checked(
    ctx: &Ctx,
    file: &Path,
    label: &str,
    month: Month,
    parts: &[Part<'_>],
    check: &dyn Fn(&Opened) -> Result<()>,
) -> Result<()> {
    let tmp = with_suffix(file, ".tmp");
    {
        let sealed = ctx.vault.seal(label, &build(parts)?)?;
        let mut f = private_create(&tmp)?;
        f.write_all(&sealed)?;
        f.sync_all()?;
    }
    let read_back = || -> Result<()> {
        // what is on disk now, opened as the pack will be
        let sealed = std::fs::read(&tmp)?;
        let plain = ctx.vault.open_sealed(label, &sealed).map_err(|e| Error::from(e).in_file(label))?;
        check(&Opened::parse(plain, month, label)?)
    };
    if let Err(e) = read_back() {
        remove_if_exists(&tmp)?;
        return Err(e);
    }
    ctx.crash_point(Crash::BeforeRename)?;
    std::fs::rename(&tmp, file)?;
    sync_dir(file.parent().unwrap_or(file));
    ctx.packs.forget(file);
    Ok(())
}

/// Whether a day a pack was written again around is in the new pack as it was in the old: its place in the index
/// and its bytes.
fn kept_as_it_was(new: &Opened, e: &Entry, old: &Opened, was: &Entry) -> bool {
    (e.day, e.generation, e.level, e.len, e.hash) == (was.day, was.generation, was.level, was.len, was.hash)
        && (e.level == LEVEL_MEMBER || old.body(was).is_ok_and(|b| new.body(e).is_ok_and(|n| n == b)))
}

/// Writes the days of a month's pack that are not yet in the small form again, each by itself (see the module
/// docs), oldest first, at most `max_days` of them and about [`RECODE_PLAIN_MAX`] bytes. `for_now`: the days are
/// packed together afterwards ([`group_month`]), so they are compressed lightly here. Returns how many it wrote
/// again and whether more are left. Under the car's lock. A pack or a day in it that doesn't read is reported with
/// the pack's name, and the pack stays as it is.
pub(crate) fn recode_month<P: Point>(
    ctx: &Ctx,
    dir: &Path,
    month: Month,
    max_days: usize,
    for_now: bool,
) -> Result<(usize, bool)> {
    let file = month.file(dir);
    let label = ctx.label(&file);
    remove_if_exists(&with_suffix(&file, ".tmp"))?;
    let Some(plain) = ctx.read(&file)? else { return Ok((0, false)) };
    let old = Opened::parse(plain, month, &label)?;
    let stale = old.index.entries.iter().filter(|e| e.level < LEVEL_SMALL).count();
    // the days written again: what they were, their points, their new bodies
    let mut again: Vec<(Entry, Vec<P>, Vec<u8>)> = Vec::new();
    let mut unpacked = 0;
    for e in old.index.entries.iter().filter(|e| e.level < LEVEL_SMALL) {
        if again.len() >= max_days.max(1) || unpacked >= RECODE_PLAIN_MAX {
            break;
        }
        let body = old.body(e)?;
        let plain = codec::decompress(body).map_err(|err| err.in_file(&label))?;
        let points = DayLog::<P>::decode_state(&plain).map_err(|err| err.in_file(&label))?.points;
        let how = if for_now { ctx.opts.hot } else { ctx.opts.small };
        let small = codec::compress(&super::columns::encode_small(&points), how)?;
        unpacked += plain.len();
        // a day that would not get smaller keeps its bytes, and is noted as done all the same
        again.push((*e, points, if for_now || small.len() < body.len() { small } else { body.to_vec() }));
    }
    if again.is_empty() {
        return Ok((0, false));
    }
    let mut parts: Vec<Part<'_>> = Vec::with_capacity(old.index.entries.len());
    for e in &old.index.entries {
        parts.push(match again.iter().find(|a| a.0.day == e.day) {
            Some((_, _, body)) => Part::alone(e.day, e.generation, LEVEL_SMALL, body),
            None if e.level == LEVEL_MEMBER => Part::kept(e, &[]),
            None => Part::kept(e, old.body(e)?),
        });
    }
    replace_checked(ctx, &file, &label, month, &parts, &|new: &Opened| {
        let wrong = |day: u64| {
            damaged(&label, format!("{} written again does not read back as it was; the pack was kept", day_name(day)))
        };
        if new.index.entries.len() != old.index.entries.len() {
            return Err(damaged(&label, "written again, its days are not the ones it had; the pack was kept"));
        }
        for (e, was) in new.index.entries.iter().zip(&old.index.entries) {
            let ok = match again.iter().find(|a| a.0.day == was.day) {
                Some((_, points, _)) => {
                    let read =
                        new.body(e).and_then(codec::decompress).and_then(|plain| DayLog::<P>::decode_state(&plain));
                    (e.day, e.generation, e.level) == (was.day, was.generation, LEVEL_SMALL)
                        && read.is_ok_and(|read| super::columns::same_points(&read.points, points))
                }
                None => kept_as_it_was(new, e, &old, was),
            };
            if !ok {
                return Err(wrong(was.day));
            }
        }
        Ok(())
    })?;
    Ok((again.len(), again.len() < stale))
}

/// A day going into a run packed together: where it is among the old pack's entries, its columns (small), and
/// its points if the columns were written anew for it (a day that was not yet in the small form).
struct Grouped<P> {
    at: usize,
    plain: Vec<u8>,
    points: Option<Vec<P>>,
}

/// Packs the days of a month's pack that stand by themselves together with their neighbours (see the module docs),
/// oldest first: runs of days next to each other in the pack, each of about [`crate::Options::group_bytes`] of
/// columns at most; a day not yet in the small form is encoded small on the way. About [`RECODE_PLAIN_MAX`] bytes a
/// call. Returns how many days it packed together and whether more are left. Under the car's lock. A pack or a
/// day in it that doesn't read is reported with the pack's name, and the pack stays as it is.
pub(crate) fn group_month<P: Point>(ctx: &Ctx, dir: &Path, month: Month) -> Result<(usize, bool)> {
    let file = month.file(dir);
    let label = ctx.label(&file);
    remove_if_exists(&with_suffix(&file, ".tmp"))?;
    let Some(plain) = ctx.read(&file)? else { return Ok((0, false)) };
    let old = Opened::parse(plain, month, &label)?;
    let entries = &old.index.entries;
    let stale = entries.iter().filter(|e| e.level < LEVEL_GROUP).count();
    let mut runs: Vec<Vec<Grouped<P>>> = Vec::new();
    let mut open: Vec<Grouped<P>> = Vec::new();
    let (mut unpacked, mut open_bytes) = (0usize, 0usize);
    for (at, e) in entries.iter().enumerate() {
        if e.level >= LEVEL_GROUP {
            // a run ends where days already packed together stand
            if !open.is_empty() {
                runs.push(std::mem::take(&mut open));
            }
            if unpacked >= RECODE_PLAIN_MAX {
                break;
            }
            continue;
        }
        let was = codec::decompress(old.body(e)?).map_err(|err| err.in_file(&label))?;
        let (plain, points) = match e.level < LEVEL_SMALL {
            true => {
                let points = DayLog::<P>::decode_state(&was).map_err(|err| err.in_file(&label))?.points;
                (super::columns::encode_small(&points), Some(points))
            }
            false => (was, None),
        };
        if !open.is_empty() && open_bytes + plain.len() > ctx.opts.group_bytes {
            runs.push(std::mem::take(&mut open));
            if unpacked >= RECODE_PLAIN_MAX {
                // (this day another time)
                break;
            }
        }
        if open.is_empty() {
            open_bytes = 0;
        }
        unpacked += plain.len();
        open_bytes += plain.len();
        open.push(Grouped { at, plain, points });
    }
    if !open.is_empty() {
        runs.push(open);
    }
    if runs.is_empty() {
        return Ok((0, false));
    }
    let streams = runs
        .iter()
        .map(|run| {
            let all: Vec<u8> = run.iter().flat_map(|d| d.plain.iter().copied()).collect();
            codec::compress(&all, ctx.opts.small)
        })
        .collect::<Result<Vec<_>>>()?;
    // where each entry of the old pack goes: (run, place in it)
    let place =
        |at: usize| runs.iter().enumerate().find_map(|(r, run)| run.iter().position(|d| d.at == at).map(|i| (r, i)));
    let mut parts: Vec<Part<'_>> = Vec::with_capacity(entries.len());
    for (at, e) in entries.iter().enumerate() {
        parts.push(match place(at) {
            Some((r, 0)) => Part::alone(e.day, e.generation, LEVEL_GROUP, &streams[r]),
            Some((r, i)) => {
                let plain = &runs[r][i].plain;
                Part::member(e.day, e.generation, plain.len() as u64, content_hash(plain))
            }
            None if e.level == LEVEL_MEMBER => Part::kept(e, &[]),
            None => Part::kept(e, old.body(e)?),
        });
    }
    replace_checked(ctx, &file, &label, month, &parts, &|new: &Opened| {
        let wrong = |day: u64| {
            let why =
                format!("{} packed with its neighbours does not read back as it was; the pack was kept", day_name(day));
            damaged(&label, why)
        };
        if new.index.entries.len() != entries.len() {
            return Err(damaged(&label, "written again, its days are not the ones it had; the pack was kept"));
        }
        for (at, (e, was)) in new.index.entries.iter().zip(entries).enumerate() {
            let ok = match place(at) {
                // a run is looked at whole at its first day: every day's columns as they went in, and the points
                // of the days encoded anew, bit for bit
                Some((r, 0)) => {
                    let read = new.unpack(e).map_err(|_| wrong(was.day))?;
                    read.len() == runs[r].len()
                        && read.iter().zip(&runs[r]).all(|((d, plain), day)| {
                            let (old_day, points) = (&entries[day.at], &day.points);
                            (d.day, d.generation) == (old_day.day, old_day.generation)
                                && *plain == day.plain
                                && points.as_ref().is_none_or(|points| {
                                    DayLog::<P>::decode_state(plain)
                                        .is_ok_and(|read| super::columns::same_points(&read.points, points))
                                })
                        })
                }
                Some(_) => (e.day, e.generation, e.level) == (was.day, was.generation, LEVEL_MEMBER),
                None => kept_as_it_was(new, e, &old, was),
            };
            if !ok {
                return Err(wrong(was.day));
            }
        }
        Ok(())
    })?;
    let done: usize = runs.iter().map(Vec::len).sum();
    Ok((done, done < stale))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn months_and_their_names() {
        let feb = Month::parse("2024-02").unwrap();
        assert_eq!(feb.end - feb.first, 29);
        assert_eq!(day_name(feb.first), "2024-02-01");
        assert_eq!(day_name(feb.end), "2024-03-01");
        assert_eq!(Month::of(feb.end - 1), feb);
        assert_eq!(feb.name(), "2024-02");
        let dec = Month::parse("2026-12").unwrap();
        assert_eq!(day_name(dec.end), "2027-01-01");
        for bad in ["2024-13", "2024-00", "2024-2", "24-02", "2024_02", "../../x", "1969-12"] {
            assert_eq!(Month::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn a_pack_round_trips_and_a_wrong_one_is_told() {
        let m = Month::parse("2026-11").unwrap();
        let (a, b, c) = (vec![1u8; 300], Vec::new(), vec![7u8; 5]);
        let parts =
            vec![Part::alone(m.first, 3, 1, &a), Part::alone(m.first + 4, 1, 1, &b), Part::alone(m.end - 1, 9, 0, &c)];
        let plain = build(&parts).unwrap();
        assert_eq!(plain[5..7], [0, 1], "a pack without days packed together is of the first version");
        let o = Opened::parse(plain.clone(), m, "p").unwrap();
        assert_eq!(o.index.entries.len(), 3);
        for (e, p) in o.index.entries.iter().zip(&parts) {
            assert_eq!((e.day, e.generation, e.level), (p.day, p.generation, p.level));
            assert_eq!(o.body(e).unwrap(), p.body);
        }
        assert!(o.index.get(m.first + 1).is_none());
        // the start alone gives the same index
        let head = parse_index(&plain[..INDEX_MAX.min(plain.len())], m, "p", false).unwrap();
        assert_eq!(head.entries, o.index.entries);
        // cut, or of another month, or with a body changed
        assert!(Opened::parse(Zeroizing::new(plain[..plain.len() - 1].to_vec()), m, "p").is_err());
        assert!(Opened::parse(plain.clone(), Month::parse("2026-10").unwrap(), "p").is_err());
        let mut changed = plain.clone();
        let last = changed.len() - 1;
        changed[last] ^= 1;
        let o = Opened::parse(changed, m, "p").unwrap();
        assert!(o.body(&o.index.entries[2]).is_err());
        assert!(o.body(&o.index.entries[0]).is_ok());
    }

    #[test]
    fn days_packed_together_come_apart_as_they_went_in() {
        let m = Month::parse("2026-11").unwrap();
        let days: Vec<Vec<u8>> = (0..5u8).map(|d| vec![d; 100 + usize::from(d) * 37]).collect();
        let stream = codec::compress(&days[1..4].concat(), codec::Level::HOT).unwrap();
        let (alone, last) = (
            codec::compress(&days[0], codec::Level::HOT).unwrap(),
            codec::compress(&days[4], codec::Level::HOT).unwrap(),
        );
        let member = |i: usize, generation| {
            Part::member(m.first + i as u64, generation, days[i].len() as u64, content_hash(&days[i]))
        };
        let parts = vec![
            Part::alone(m.first, 7, 2, &alone),
            Part::alone(m.first + 1, 1, LEVEL_GROUP, &stream),
            member(2, 2),
            member(3, 3),
            // a run of one day
            Part::alone(m.first + 4, 4, LEVEL_GROUP, &last),
        ];
        let plain = build(&parts).unwrap();
        assert_eq!(plain[5..7], [0, 2]);
        let o = Opened::parse(plain.clone(), m, "p").unwrap();
        for (i, e) in o.index.entries.iter().enumerate() {
            assert_eq!(e.generation, [7, 1, 2, 3, 4][i]);
            assert_eq!(o.unpack_day(e).unwrap(), days[i], "day {i}");
            let together: Vec<u64> = o.unpack(e).unwrap().iter().map(|(d, _)| d.day - m.first).collect();
            assert_eq!(together, [vec![0], vec![1, 2, 3], vec![1, 2, 3], vec![1, 2, 3], vec![4]][i]);
        }
        assert_eq!(o.index.entries[2].bytes, stream.len() as u64 / 3);
        // the start alone gives the same index
        let head = parse_index(&plain[..INDEX_MAX.min(plain.len())], m, "p", false).unwrap();
        assert_eq!(head.entries, o.index.entries);
        // a changed stream is told for every day of its run, and for no other
        let mut changed = plain.clone();
        let at = o.index.entries[1].offset + 3;
        changed[at] ^= 1;
        let bad = Opened::parse(changed, m, "p").unwrap();
        for (i, e) in bad.index.entries.iter().enumerate() {
            assert_eq!(bad.unpack_day(e).is_err(), (1..4).contains(&i), "day {i}");
        }
        // days packed together need the pack's second version; one of them needs a first day before it
        let mut first_version = plain.to_vec();
        first_version[5..7].copy_from_slice(&[0, 1]);
        assert!(Opened::parse(Zeroizing::new(first_version), m, "p").is_err());
        let mut later = plain.to_vec();
        later[5..7].copy_from_slice(&[0, 3]);
        assert!(matches!(Opened::parse(Zeroizing::new(later), m, "p"), Err(Error::Version { .. })));
        assert!(build(&[member(2, 2)]).is_ok_and(|p| Opened::parse(p, m, "p").is_err()));
        // lengths that ask for more than the stream holds
        let long = vec![Part::alone(m.first + 1, 1, LEVEL_GROUP, &stream), Part::member(m.first + 2, 2, 10_000, 0)];
        let o = Opened::parse(build(&long).unwrap(), m, "p").unwrap();
        assert!(o.unpack_day(&o.index.entries[1]).is_err());
    }
}
