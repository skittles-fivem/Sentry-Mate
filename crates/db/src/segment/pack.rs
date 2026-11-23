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
//! Indexes are kept in memory by what the pack file was when read (only the file's first chunk is opened for one),
//! so listing a series costs a `stat` per month; the few packs opened last are kept whole (wiped when let go), since
//! a chart of a year reads a month's days one after the other.

use super::{DayLog, Point, civil, day_files, day_name, days_from_civil};
use crate::Crash;
use crate::codec;
use crate::error::{Error, Result};
use crate::file::{
    Ctx, HEADER, Kind, Which, check_header, header, private_create, remove_if_exists, sync_dir, with_suffix,
};
use crate::journal::Files;
use crate::logged::{Elsewhere, LEVEL_COLD, Logged, LoggedFile, SNAP_HEAD, parse_gen_level};
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

/// A day in a pack.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Entry {
    pub day: u64,
    pub generation: u64,
    pub level: u8,
    /// Bytes of its body.
    pub len: u64,
    pub hash: u64,
    /// Where its body starts in the pack's plaintext.
    offset: usize,
}

impl Entry {
    /// What the day is in the pack, whatever the file around it.
    pub(crate) fn stamp(&self) -> Stamp {
        Stamp::packed(self.generation, self.len, self.hash)
    }

    /// About what the day would take as a snapshot of its own.
    pub(crate) fn snapshot_len(&self) -> u64 {
        self.len + SNAP_HEAD as u64
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
    let b = check_header(plain, Kind::SegmentPack, label)?;
    let count = usize::from(*b.first().ok_or_else(|| damaged(label, "pack index cut short"))?);
    if count > MAX_DAYS {
        return Err(damaged(label, "more days than a month has"));
    }
    let table = b.get(1..1 + count * ENTRY).ok_or_else(|| damaged(label, "pack index cut short"))?;
    let mut offset = HEADER + 1 + count * ENTRY;
    let mut entries: Vec<Entry> = Vec::with_capacity(count);
    for e in table.as_chunks::<ENTRY>().0 {
        let u32_at = |i: usize| u64::from(u32::from_be_bytes([e[i], e[i + 1], e[i + 2], e[i + 3]]));
        let u64_at = |i: usize| {
            let mut v = [0u8; 8];
            v.copy_from_slice(&e[i..i + 8]);
            u64::from_be_bytes(v)
        };
        let entry =
            Entry { day: u32_at(0), generation: u64_at(4), level: e[12], len: u32_at(13), hash: u64_at(17), offset };
        if !(month.first..month.end).contains(&entry.day) || entries.last().is_some_and(|l| l.day >= entry.day) {
            return Err(damaged(label, "its days are not this month's, in order"));
        }
        offset += entry.len as usize;
        entries.push(entry);
    }
    if whole && offset != plain.len() {
        return Err(damaged(label, "its days' lengths don't add up to its own"));
    }
    Ok(Index { entries })
}

/// A pack's plaintext for these days (day, generation, level, body), given in day order.
fn build(parts: &[(u64, u64, u8, &[u8])]) -> Result<Zeroizing<Vec<u8>>> {
    if parts.len() > MAX_DAYS {
        return Err(Error::invalid("a pack holds one month"));
    }
    let bodies: usize = parts.iter().map(|p| p.3.len()).sum();
    let mut b = Zeroizing::new(Vec::with_capacity(INDEX_MAX + bodies));
    b.extend_from_slice(&header(Kind::SegmentPack));
    b.push(parts.len() as u8);
    for (day, generation, level, body) in parts {
        let (Ok(day), Ok(len)) = (u32::try_from(*day), u32::try_from(body.len())) else {
            return Err(Error::invalid("a day too large to pack"));
        };
        b.extend_from_slice(&day.to_be_bytes());
        b.extend_from_slice(&generation.to_be_bytes());
        b.push(*level);
        b.extend_from_slice(&len.to_be_bytes());
        b.extend_from_slice(&content_hash(body).to_be_bytes());
    }
    for (_, _, _, body) in parts {
        b.extend_from_slice(body);
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

    /// A day's body as it is in the pack (not checked against its hash).
    fn raw(&self, e: &Entry) -> &[u8] {
        self.plain.get(e.offset..e.offset + e.len as usize).unwrap_or(&[])
    }

    /// A day's body, checked against its hash.
    pub(crate) fn body(&self, e: &Entry) -> Result<&[u8]> {
        let b = self.raw(e);
        if b.len() as u64 != e.len || content_hash(b) != e.hash {
            return Err(damaged(&self.label, format!("the day {} in it is not what its index says", day_name(e.day))));
        }
        Ok(b)
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
    let body = opened.body(&e)?.to_vec();
    Ok(Some(Elsewhere { label: opened.label.clone(), generation: e.generation, level: e.level, body }))
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
    /// The pack before.
    Pack(Entry),
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
        if loose.level != LEVEL_COLD {
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
                e.generation == loose.generation && e.level == loose.level && o.raw(&e) == loose.body()
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
            days.extend(o.index.entries.iter().filter(|e| !replaced(e.day)).map(|e| (e.day, From::Pack(*e))));
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
            From::Pack(_) => None,
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
            (From::Pack(_), None) => Err(Error::invalid("a day from a pack that isn't there")),
        }
    };
    let head_of = |from: &From| match from {
        From::Loose(_, loose) => (loose.generation, loose.level),
        From::Pack(e) => (e.generation, e.level),
    };
    {
        let bodies = days.iter().map(|(_, from)| body_of(from)).collect::<Result<Vec<_>>>()?;
        let parts: Vec<(u64, u64, u8, &[u8])> = days
            .iter()
            .zip(&bodies)
            .map(|((day, from), body)| (*day, head_of(from).0, head_of(from).1, body.as_slice()))
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
            let packed = new.body(e).map_err(|_| wrong(*day))?;
            let ok = e.day == *day
                && match from {
                    From::Loose(files, _) => {
                        // the day's snapshot, read from disk again
                        let again = Loose::read(ctx, files)?.ok_or_else(|| wrong(*day))?;
                        (e.generation, e.level) == (again.generation, again.level) && packed == again.body()
                    }
                    From::Pack(was) => {
                        (e.generation, e.level) == (was.generation, was.level)
                            && old.is_some_and(|o| o.body(was).is_ok_and(|b| b == packed))
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
        let parts: Vec<(u64, u64, u8, &[u8])> =
            vec![(m.first, 3, 1, &a), (m.first + 4, 1, 1, &b), (m.end - 1, 9, 0, &c)];
        let plain = build(&parts).unwrap();
        let o = Opened::parse(plain.clone(), m, "p").unwrap();
        assert_eq!(o.index.entries.len(), 3);
        for (e, p) in o.index.entries.iter().zip(&parts) {
            assert_eq!((e.day, e.generation, e.level), (p.0, p.1, p.2));
            assert_eq!(o.body(e).unwrap(), p.3);
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
}
