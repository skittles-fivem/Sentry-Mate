//! Time series per car per UTC day: [`model::Sample`]s (1 Hz positions and car state) and [`model::ChargePoint`]s,
//! one series per kind and source, so a CAN rebuild never touches what a TeslaMate import brought:
//!
//! ```text
//! db/cars/<car>/seg/<samples|charge>.<can|tesla-api|teslamate>/<YYYY-MM-DD>.snap   the day, columns (see columns), compressed
//! db/cars/<car>/seg/<samples|charge>.<can|tesla-api|teslamate>/<YYYY-MM-DD>.log    windows written since: (from, to, points)
//! db/cars/<car>/seg/<samples|charge>.<can|tesla-api|teslamate>/<YYYY-MM>.pack      a finished month's days, as their snapshots were (see pack)
//! ```
//!
//! A write replaces every point of a window. A window that covers whole days writes those days' snapshots
//! directly; a part of a day is appended to its journal without reading the day (cheap enough to do every minute
//! for every car), and the journal is folded into the day's snapshot once it is about as big as the snapshot.
//! Folding while writing uses the warm level and the quick columns; [`crate::Db::settle`] writes finished days in
//! their smallest form (the small columns of [`columns`], compressed hardest), each read back and compared with the
//! day bit for bit before it takes the old snapshot's place.
//!
//! A day's snapshot is its own file or, once its month is over and [`crate::Db::compact_series`] has passed, an
//! entry of the month's pack (first the same bytes, then packed together with its neighbours: see [`pack`]), and a
//! snapshot file always wins over the pack's entry for its day.
//! Everything that looks at a day's snapshot looks in both places, the file first. A data folder without packs
//! reads as it always did; nothing but that pass makes them. A day written after it was packed gets a journal and
//! then a snapshot file again (the next pass takes it back into the pack); a day emptied while the pack holds it
//! keeps an empty snapshot file until the pass that drops it from the pack.

pub(crate) mod columns;
pub(crate) mod pack;
mod points;

pub use columns::{
    Col, ColumnSize, Derived, Fields, Point, Tuning, decode, decode_fields, describe, encode, encode_small,
    encode_tuned, same_points,
};

use crate::codec;
use crate::error::{Error, Result};
use crate::file::{Ctx, Kind, Which, check_header, header, remove_if_exists};
use crate::journal::{Files, Journal};
use crate::logged::{LEVEL_COLD, LEVEL_SMALL, Logged, LoggedFile, append_only, snapshot_head};
use columns::{Reader, put_varint};
use model::{ChargePoint, Sample, Stream};
use pack::{Month, Packs};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub const DAY_MS: u64 = 86_400_000;

/// 10000-01-01 00:00 UTC: points are kept up to here. A day's files are named `YYYY-MM-DD`, so a later day would
/// get a name no listing finds; a window reaching past it (a rebuild of "everything", `0..u64::MAX`) ends here.
pub const END_MS: u64 = 253_402_300_800_000;

/// A window cut to what can be kept (see [`END_MS`]).
fn clamp(from: u64, to: u64) -> (u64, u64) {
    (from.min(END_MS), to.min(END_MS))
}

/// The days of `[from, to)` (cut to [`END_MS`]) the series has, from a listing of its folder, for a long window: a
/// window of years or `0..u64::MAX` costs one listing, not a step per day. `None` for a window of a few weeks,
/// whose days are cheaper checked one by one by name.
fn window_days(ctx: &Ctx, dir: &Path, from: u64, to: u64) -> Result<Option<Vec<u64>>> {
    if from >= to {
        return Ok(Some(Vec::new()));
    }
    let (first, last) = (from / DAY_MS, (to - 1) / DAY_MS);
    if last - first > 62 {
        return Ok(Some(list_days(ctx, dir)?.into_iter().filter(|d| (first..=last).contains(d)).collect()));
    }
    Ok(None)
}

/// A day kept for a series.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DayInfo {
    /// `YYYY-MM-DD` (UTC).
    pub date: String,
    pub start_ms: u64,
    /// Bytes on disk (snapshot and journal; for a day in a month's pack, its part of the pack).
    pub bytes: u64,
}

/// Days since 1970-01-01 to a civil date (proleptic Gregorian), and back.
pub(crate) fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

pub(crate) fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let m = i64::from(m);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

pub(crate) fn day_name(day: u64) -> String {
    let (y, m, d) = civil(day as i64);
    format!("{y:04}-{m:02}-{d:02}")
}

pub(crate) fn parse_day(name: &str) -> Option<u64> {
    let b = name.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let y: i64 = name[..4].parse().ok()?;
    let m: u32 = name[5..7].parse().ok()?;
    let d: u32 = name[8..].parse().ok()?;
    let days = days_from_civil(y, m, d);
    (days >= 0 && day_name(days as u64) == name).then_some(days as u64)
}

pub(crate) fn series_dir<P: Point>(car_dir: &Path, stream: Stream) -> PathBuf {
    car_dir.join("seg").join(format!("{}.{}", P::SERIES, stream.dir_name()))
}

fn day_files(dir: &Path, day: u64) -> Files {
    Files::named(dir, &day_name(day))
}

/// The files a day of a series is read from (its snapshot, its journal, its month's pack): what a reader of another
/// process's files looks at before and after it reads the day, to see whether the day changed meanwhile
/// ([`crate::Reader::day_points`]).
pub(crate) fn day_paths<P: Point>(car_dir: &Path, stream: Stream, day: u64) -> [PathBuf; 3] {
    let dir = series_dir::<P>(car_dir, stream);
    let f = day_files(&dir, day);
    [f.snap, f.log, Month::of(day).file(&dir)]
}

/// A day's points in memory.
pub(crate) struct DayLog<P> {
    pub points: Vec<P>,
}

/// Every point in `[from, to)` replaced by `points` (all in that window, in time order).
pub(crate) struct Replace<P> {
    pub from: u64,
    pub to: u64,
    pub points: Vec<P>,
}

fn check_sorted<P: Point>(points: &[P]) -> Result<()> {
    match points.windows(2).all(|w| w[0].t_ms() < w[1].t_ms()) {
        true => Ok(()),
        false => Err(Error::Corrupt("points out of order".into())),
    }
}

impl<P: Point> Logged for DayLog<P> {
    type Op = Replace<P>;
    const KIND: Kind = Kind::Segment;

    fn empty() -> Self {
        DayLog { points: Vec::new() }
    }

    fn apply(&mut self, op: Replace<P>) {
        let a = self.points.partition_point(|p| p.t_ms() < op.from);
        let b = self.points.partition_point(|p| p.t_ms() < op.to);
        self.points.splice(a..b, op.points);
    }

    fn encode_op(ctx: &Ctx, op: &Replace<P>) -> Result<Vec<u8>> {
        let mut b = Vec::new();
        put_varint(&mut b, op.from);
        put_varint(&mut b, op.to);
        b.extend_from_slice(&codec::compress(&encode(&op.points), ctx.level(Which::Hot))?);
        Ok(b)
    }

    fn decode_op(b: &[u8]) -> Result<Replace<P>> {
        let mut r = Reader::new(b);
        let (from, to) = (r.varint()?, r.varint()?);
        let points: Vec<P> = decode(&codec::decompress(r.rest())?)?;
        check_sorted(&points)?;
        if from > to || points.first().is_some_and(|p| p.t_ms() < from) || points.last().is_some_and(|p| p.t_ms() >= to)
        {
            return Err(Error::Corrupt("points outside their window".into()));
        }
        Ok(Replace { from, to, points })
    }

    fn encode_state(&self) -> Result<Vec<u8>> {
        Ok(encode(&self.points))
    }

    fn decode_state(b: &[u8]) -> Result<Self> {
        let points = decode(b)?;
        check_sorted(&points)?;
        Ok(DayLog { points })
    }

    fn encode_cold(&self, ctx: &Ctx) -> Result<(Vec<u8>, u8, codec::Level)> {
        match ctx.opts.small_columns {
            true => Ok((columns::encode_small(&self.points), LEVEL_SMALL, ctx.opts.small)),
            false => Ok((encode(&self.points), LEVEL_COLD, ctx.level(Which::Cold))),
        }
    }

    fn settled_level(ctx: &Ctx) -> u8 {
        if ctx.opts.small_columns { LEVEL_SMALL } else { LEVEL_COLD }
    }

    fn same_as(&self, other: &Self) -> Result<bool> {
        Ok(columns::same_points(&self.points, &other.points))
    }

    fn fold_due(ctx: &Ctx, journal_bytes: u64, _entries: u64, snap_bytes: u64, _snap_plain: u64) -> bool {
        // the journal is compressed like the snapshot, so compare packed sizes
        journal_bytes >= snap_bytes.max(ctx.opts.fold_min_bytes)
    }
}

/// A day's snapshot, wherever it is: its generation, level and about its bytes. The day's own file first, else its
/// entry in the month's pack.
fn snap_head(ctx: &Ctx, dir: &Path, day: u64, files: &Files) -> Result<Option<(u64, u8, u64)>> {
    if let Some(h) = snapshot_head(ctx, files, Kind::Segment)? {
        return Ok(Some(h));
    }
    Ok(pack::entry(ctx, dir, day)?.map(|e| (e.generation, e.level, e.snapshot_len())))
}

/// A day's snapshot and journal, kept open to commit to (under the car's lock).
fn load_day<P: Point>(ctx: &Ctx, dir: &Path, day: u64) -> Result<LoggedFile<DayLog<P>>> {
    LoggedFile::load_or(ctx, day_files(dir, day), &|| pack::elsewhere(ctx, dir, day))
}

/// Leaves a day with no points: its files removed, unless the month's pack holds the day; then an empty snapshot of
/// the next generation stays (or the pack's entry would be the day again) until the pack drops the day.
fn empty_day<P: Point>(ctx: &Ctx, dir: &Path, day: u64, mut lf: LoggedFile<DayLog<P>>) -> Result<()> {
    if pack::entry(ctx, dir, day)?.is_some() {
        lf.state = DayLog::empty();
        return lf.fold(ctx, Which::Hot);
    }
    lf.remove_files(ctx)
}

/// The generation a day's files are at (the newer of snapshot and journal headers).
fn current_gen(ctx: &Ctx, dir: &Path, day: u64, files: &Files) -> Result<u64> {
    let snap = snap_head(ctx, dir, day, files)?.map_or(0, |(g, _, _)| g);
    let log = Journal::read_only(ctx, &files.log, true)?.map_or(0, |r| r.generation);
    if log > snap {
        return Err(Error::Damaged {
            file: ctx.label(&files.log),
            why: format!("it follows snapshot {log} but the snapshot is {snap}"),
        });
    }
    Ok(snap)
}

/// Writes a whole day as a new cold snapshot, replacing what was there. With no points the day's files are removed
/// (an empty snapshot stays while the month's pack holds the day: see [`empty_day`]).
fn write_whole_day<P: Point>(ctx: &Ctx, dir: &Path, day: u64, points: Vec<P>) -> Result<()> {
    let files = day_files(dir, day);
    files.clean_temporary()?;
    let packed = pack::entry(ctx, dir, day)?.is_some();
    if points.is_empty() && !files.snap.exists() && !files.log.exists() && !packed {
        return Ok(());
    }
    let generation = current_gen(ctx, dir, day, &files)? + 1;
    let mut b = header(Kind::Segment);
    b.extend_from_slice(&generation.to_be_bytes());
    let which = if points.is_empty() { Which::Hot } else { Which::Cold };
    b.push(if which == Which::Cold { LEVEL_COLD } else { 0 });
    b.extend_from_slice(&codec::compress(&encode(&points), ctx.level(which))?);
    ctx.write_atomic(&files.snap, &b)?;
    ctx.crash_point(crate::Crash::BeforeJournalRemoved)?;
    remove_if_exists(&files.log)?;
    if points.is_empty() && !packed {
        remove_if_exists(&files.snap)?;
    }
    Ok(())
}

fn check_points<P: Point>(from: u64, to: u64, points: &[P]) -> Result<()> {
    if from > to {
        return Err(Error::invalid("a window ends before it starts"));
    }
    if points.windows(2).any(|w| w[0].t_ms() >= w[1].t_ms()) {
        return Err(Error::invalid("points must be in time order, one per time"));
    }
    if points.first().is_some_and(|p| p.t_ms() < from) || points.last().is_some_and(|p| p.t_ms() >= to) {
        return Err(Error::invalid("points must be inside their window"));
    }
    if points.last().is_some_and(|p| p.t_ms() >= END_MS) {
        return Err(Error::invalid("points after the year 9999 can't be kept"));
    }
    Ok(())
}

/// Replaces every point of `[from, to)` of a series with `points`. Returns the first day touched.
pub(crate) fn write<P: Point>(
    ctx: &Ctx,
    car_dir: &Path,
    source: Stream,
    from: u64,
    to: u64,
    points: &[P],
) -> Result<Option<u64>> {
    check_points(from, to, points)?;
    let (from, to) = clamp(from, to);
    if from == to {
        return Ok(None);
    }
    let dir = series_dir::<P>(car_dir, source);
    // a long window: the days there are, and the days of the new points
    let days = match window_days(ctx, &dir, from, to)? {
        Some(mut days) => {
            days.extend(points.iter().map(|p| p.t_ms() / DAY_MS));
            days.sort_unstable();
            days.dedup();
            days
        }
        None => (from / DAY_MS..=(to - 1) / DAY_MS).collect(),
    };
    let mut rest = points;
    for day in days {
        // day < END_MS / DAY_MS, so this doesn't overflow
        let (ds, de) = (day * DAY_MS, (day + 1) * DAY_MS);
        let (wf, wt) = (from.max(ds), to.min(de));
        let n = rest.partition_point(|p| p.t_ms() < wt);
        let (pts, after) = rest.split_at(n);
        rest = after;
        if wf == ds && wt == de {
            write_whole_day(ctx, &dir, day, pts.to_vec())?;
            continue;
        }
        let files = day_files(&dir, day);
        let head = snap_head(ctx, &dir, day, &files)?;
        if pts.is_empty() && head.is_none() && !files.log.exists() {
            continue;
        }
        let op = Replace { from: wf, to: wt, points: pts.to_vec() };
        if append_only::<DayLog<P>>(ctx, &files, &op, head.map(|(g, _, len)| (g, len)))? {
            let mut lf = load_day::<P>(ctx, &dir, day)?;
            if lf.state.points.is_empty() {
                empty_day(ctx, &dir, day, lf)?;
            } else {
                lf.fold(ctx, Which::Warm)?;
            }
        }
    }
    Ok(Some(from / DAY_MS))
}

/// What a series' folder holds, from one listing: the days with a snapshot file of their own, the days with a
/// journal, and the months with a pack.
#[derive(Default)]
struct Listing {
    snaps: BTreeSet<u64>,
    logs: BTreeSet<u64>,
    packs: Vec<Month>,
}

fn listing(dir: &Path) -> Result<Listing> {
    let mut l = Listing::default();
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(l),
        Err(e) => return Err(e.into()),
    };
    for e in rd {
        let name = e?.file_name();
        let name = name.to_string_lossy();
        if let Some(d) = name.strip_suffix(".snap").and_then(parse_day) {
            l.snaps.insert(d);
        } else if let Some(d) = name.strip_suffix(".log").and_then(parse_day) {
            l.logs.insert(d);
        } else if let Some(m) = name.strip_suffix(".pack").and_then(Month::parse) {
            l.packs.push(m);
        }
    }
    l.packs.sort_unstable();
    Ok(l)
}

/// The days a series has, in order: those with files of their own and those in the months' packs. Under the car's
/// lock (a month packed meanwhile could be seen neither as files nor as a pack).
pub(crate) fn list_days(ctx: &Ctx, dir: &Path) -> Result<Vec<u64>> {
    let l = listing(dir)?;
    let mut days: BTreeSet<u64> = l.snaps.union(&l.logs).copied().collect();
    for m in l.packs {
        if let Some(i) = pack::index(ctx, dir, m)? {
            days.extend(i.entries.iter().map(|e| e.day));
        }
    }
    Ok(days.into_iter().collect())
}

/// The days of `[from, to)` a series has (files of their own, or in a month's pack). Under the car's lock.
pub(crate) fn days_in<P: Point>(ctx: &Ctx, car_dir: &Path, source: Stream, from: u64, to: u64) -> Result<Vec<u64>> {
    let dir = series_dir::<P>(car_dir, source);
    let (from, to) = clamp(from, to);
    if let Some(days) = window_days(ctx, &dir, from, to)? {
        return Ok(days);
    }
    let mut packs = Packs::new(ctx, &dir);
    let mut days = Vec::new();
    for d in from / DAY_MS..=(to - 1) / DAY_MS {
        let f = day_files(&dir, d);
        if f.snap.exists() || f.log.exists() || packs.entry(d)?.is_some() {
            days.push(d);
        }
    }
    Ok(days)
}

/// A snapshot file opened and unpacked: its generation and body.
fn unpack(b: &[u8], label: &str) -> Result<crate::plain::Plain> {
    let body = check_header(b, Kind::Segment, label)?;
    let generation = body
        .get(..8)
        .and_then(|g| <[u8; 8]>::try_from(g).ok())
        .map(u64::from_be_bytes)
        .ok_or_else(|| Error::Damaged { file: label.to_string(), why: "snapshot header cut short".into() })?;
    let plain = body.get(9..).ok_or_else(|| Error::Corrupt("cut short".into())).and_then(codec::decompress);
    Ok(Arc::new((generation, zeroize::Zeroizing::new(plain.map_err(|e| e.in_file(label))?))))
}

/// A day's snapshot opened and unpacked (its generation and body) and the file it is in, from the cache of days
/// read last while it is the same ([`crate::plain`]). The day's own file first; when there is none (or it goes
/// while it is read: the month was just packed) the month's pack, which then has the day. `None`: no snapshot.
fn snapshot_plain(ctx: &Ctx, dir: &Path, day: u64, files: &Files) -> Result<Option<(crate::plain::Plain, String)>> {
    if let Some(st) = crate::plain::stamp(&files.snap)? {
        let label = ctx.label(&files.snap);
        if let Some(p) = ctx.plain.get(&files.snap, st) {
            return Ok(Some((p, label)));
        }
        if let Some(b) = ctx.read(&files.snap)? {
            let p = unpack(&b, &label)?;
            // kept only when the file read is the one looked at (a day written meanwhile is read again next time)
            if crate::plain::stamp(&files.snap)? == Some(st) {
                ctx.plain.put(&files.snap, st, p.clone());
            }
            return Ok(Some((p, label)));
        }
    }
    let Some(e) = pack::entry(ctx, dir, day)? else { return Ok(None) };
    let key = pack::plain_key(dir, day);
    let label = ctx.label(&Month::of(day).file(dir));
    if let Some(p) = ctx.plain.get(&key, e.stamp()) {
        return Ok(Some((p, label)));
    }
    // the pack as it is now (written again since its index was read, it may have the day anew, or no more)
    let Some((e, opened)) = pack::find(ctx, dir, day)? else { return Ok(None) };
    // (a day packed together with others is unpacked with them: they are all kept, a page reads them next)
    let mut found = None;
    for (d, plain) in opened.unpack(&e)? {
        let p: crate::plain::Plain = Arc::new((d.generation, zeroize::Zeroizing::new(plain)));
        ctx.plain.put(&pack::plain_key(dir, d.day), d.stamp(), p.clone());
        if d.day == day {
            found = Some(p);
        }
    }
    Ok(found.map(|p| (p, label)))
}

/// A day with a journal, read whole (snapshot and journal) and kept in the same cache as an unpacked snapshot would
/// be, while its journal is the same file (a write to the day appends to it, a fold replaces or removes it): days
/// written by the minute (a live car's) are read again without opening every entry of their journal each time.
/// `None`: the journal went meanwhile (the day is its snapshot alone now).
pub(crate) fn journaled_plain<P: Point>(
    ctx: &Ctx,
    car_dir: &Path,
    source: Stream,
    day: u64,
) -> Result<Option<crate::plain::Plain>> {
    let dir = series_dir::<P>(car_dir, source);
    let files = &day_files(&dir, day);
    let Some(st) = crate::plain::stamp(&files.log)? else { return Ok(None) };
    if let Some(p) = ctx.plain.get(&files.log, st) {
        return Ok(Some(p));
    }
    // (its snapshot may be in the month's pack)
    let elsewhere = || pack::elsewhere(ctx, &dir, day);
    let state = LoggedFile::<DayLog<P>>::read_state_or(ctx, files.clone(), &elsewhere)?;
    let p = Arc::new((0, zeroize::Zeroizing::new(state.encode_state()?)));
    // kept only when the journal read is the one looked at (a day written meanwhile is read again next time)
    if crate::plain::stamp(&files.log)? == Some(st) {
        ctx.plain.put(&files.log, st, p.clone());
    }
    Ok(Some(p))
}

/// A day with a journal read whole for another process's files ([`crate::Reader`]): no cache, no cleaning up.
/// `None`: the day has no journal.
pub(crate) fn inspected_plain<P: Point>(
    ctx: &Ctx,
    car_dir: &Path,
    source: Stream,
    day: u64,
) -> Result<Option<crate::plain::Plain>> {
    let dir = series_dir::<P>(car_dir, source);
    let files = day_files(&dir, day);
    if !files.log.exists() {
        return Ok(None);
    }
    // (its snapshot may be in the month's pack)
    let elsewhere = || pack::elsewhere(ctx, &dir, day);
    let state = LoggedFile::<DayLog<P>>::inspect_state_or(ctx, files, &elsewhere)?;
    Ok(Some(Arc::new((0, zeroize::Zeroizing::new(state.encode_state()?)))))
}

/// One day of a series unpacked: its body (the snapshot's, or the snapshot's and the journal's), through the cache,
/// and the file it came from. Decoded afterwards, outside the car's lock ([`points_of`], [`fields_of`]), so reads of
/// one car decode side by side.
pub(crate) struct DayPlain {
    label: String,
    plain: crate::plain::Plain,
}

/// Unpacks a day that has a journal: under the car's lock (a fold meanwhile would pair a snapshot with a journal of
/// another generation); see [`journaled_plain`]. A day of a snapshot alone is unpacked without it: the snapshot is
/// only ever replaced whole, by a rename.
pub(crate) type Journaled<'a> = &'a mut dyn FnMut(u64) -> Result<Option<crate::plain::Plain>>;

fn day_plain(ctx: &Ctx, dir: &Path, day: u64, journaled: Journaled<'_>) -> Result<Option<DayPlain>> {
    let files = &day_files(dir, day);
    if files.log.exists()
        && let Some(plain) = journaled(day)?
    {
        return Ok(Some(DayPlain { label: ctx.label(&files.log), plain }));
    }
    if let Some((plain, label)) = snapshot_plain(ctx, dir, day, files)? {
        return Ok(Some(DayPlain { label, plain }));
    }
    // (no snapshot: a journal begun meanwhile)
    Ok(journaled(day)?.map(|plain| DayPlain { label: ctx.label(&files.log), plain }))
}

/// The days `days` of a series (those of the window that it has: [`days_in`]), unpacked (those with a journal
/// through `journaled`).
pub(crate) fn plains<P: Point>(
    ctx: &Ctx,
    car_dir: &Path,
    source: Stream,
    days: &[u64],
    journaled: Journaled<'_>,
) -> Result<Vec<DayPlain>> {
    let dir = series_dir::<P>(car_dir, source);
    let mut out = Vec::new();
    for &day in days {
        out.extend(day_plain(ctx, &dir, day, journaled)?);
    }
    Ok(out)
}

/// Days read last on this thread, decoded, while [`keeping_days`] runs: each with the unpacked body it came from
/// (the same body while its file stays the same).
struct KeptDays {
    max: usize,
    days: Vec<(crate::plain::Plain, Arc<dyn std::any::Any + Send + Sync>)>,
}

thread_local! {
    static KEPT_DAYS: std::cell::RefCell<Option<KeptDays>> = const { std::cell::RefCell::new(None) };
}

/// Runs `f` with the last `n` days of points [`crate::Db::read_points`] reads on this thread kept decoded (while
/// their files stay the same), for work that reads short stretches of the same days again and again (a track per
/// drive of a month). Only `f`'s reads on this thread; nothing is kept after it.
pub fn keeping_days<R>(n: usize, f: impl FnOnce() -> R) -> R {
    struct Restore(Option<KeptDays>);
    impl Drop for Restore {
        fn drop(&mut self) {
            let old = self.0.take();
            KEPT_DAYS.with(|k| *k.borrow_mut() = old);
        }
    }
    let _restore = Restore(KEPT_DAYS.with(|k| k.borrow_mut().replace(KeptDays { max: n.max(1), days: Vec::new() })));
    f()
}

fn decode_day<P: Point>(d: &DayPlain) -> Result<Vec<P>> {
    Ok(DayLog::<P>::decode_state(&d.plain.1).map_err(|e| e.in_file(&d.label))?.points)
}

/// A day's points through the days kept on this thread (`None`: none are kept).
fn kept_day_points<P: Point>(d: &DayPlain) -> Result<Option<Arc<Vec<P>>>> {
    let found = KEPT_DAYS.with(|k| {
        k.borrow().as_ref().map(|k| k.days.iter().find(|x| Arc::ptr_eq(&x.0, &d.plain)).map(|x| x.1.clone()))
    });
    let Some(found) = found else { return Ok(None) };
    if let Some(v) = found.and_then(|v| v.downcast::<Vec<P>>().ok()) {
        return Ok(Some(v));
    }
    let v = Arc::new(decode_day::<P>(d)?);
    KEPT_DAYS.with(|k| {
        if let Some(k) = k.borrow_mut().as_mut() {
            if k.days.len() >= k.max {
                k.days.remove(0);
            }
            k.days.push((d.plain.clone(), v.clone()));
        }
    });
    Ok(Some(v))
}

/// Every point of `[from, to)` of the days, in time order.
pub(crate) fn points_of<P: Point>(days: Vec<DayPlain>, from: u64, to: u64) -> Result<Vec<P>> {
    let mut out = Vec::new();
    for d in days {
        if let Some(pts) = kept_day_points::<P>(&d)? {
            let a = pts.partition_point(|p| p.t_ms() < from);
            let b = pts.partition_point(|p| p.t_ms() < to);
            out.extend_from_slice(&pts[a..b.max(a)]);
            continue;
        }
        let pts = decode_day::<P>(&d)?;
        let a = pts.partition_point(|p| p.t_ms() < from);
        let b = pts.partition_point(|p| p.t_ms() < to);
        out.extend(pts.into_iter().skip(a).take(b - a));
    }
    Ok(out)
}

/// Some fields of `[from, to)` of the days, as columns (only those fields decoded).
pub(crate) fn fields_of<P: Point>(days: Vec<DayPlain>, from: u64, to: u64, names: &[&str]) -> Result<Fields> {
    columns::field_columns::<P>(names)?;
    let mut out = Fields::new(names);
    for d in days {
        let fields = decode_fields::<P>(&d.plain.1, names).map_err(|e| e.in_file(&d.label))?;
        out.extend_range(fields, from, to);
    }
    Ok(out)
}

/// The days of a series, with their size (a day in a month's pack: its part of the pack).
pub(crate) fn days<P: Point>(ctx: &Ctx, car_dir: &Path, source: Stream) -> Result<Vec<DayInfo>> {
    let dir = series_dir::<P>(car_dir, source);
    let mut packs = Packs::new(ctx, &dir);
    let mut out = Vec::new();
    for day in list_days(ctx, &dir)? {
        let f = day_files(&dir, day);
        let size = |p: &Path| std::fs::metadata(p).map_or(0, |m| m.len());
        let snap = match std::fs::metadata(&f.snap) {
            Ok(m) => m.len(),
            Err(_) => packs.entry(day)?.map_or(0, |e| e.bytes),
        };
        out.push(DayInfo { date: day_name(day), start_ms: day * DAY_MS, bytes: snap + size(&f.log) });
    }
    Ok(out)
}

/// Per day of a series (its UTC start), a key of what its files are now (each one's inode, size and change time): it
/// changes whenever the day is written, so what was worked out from a day can be kept with it. A day in a month's
/// pack: what the day is there (its generation, length and content hash), so its key stays when other days are packed
/// around it.
pub(crate) fn day_stamps<P: Point>(ctx: &Ctx, car_dir: &Path, source: Stream) -> Result<Vec<(u64, u64)>> {
    use std::hash::{Hash, Hasher};
    let dir = series_dir::<P>(car_dir, source);
    let mut packs = Packs::new(ctx, &dir);
    let mut out = Vec::new();
    for day in list_days(ctx, &dir)? {
        let f = day_files(&dir, day);
        let mut h = std::collections::hash_map::DefaultHasher::new();
        let snap = match crate::plain::stamp(&f.snap)? {
            Some(st) => Some(st),
            None => packs.entry(day)?.map(|e| e.stamp()),
        };
        snap.hash(&mut h);
        crate::plain::stamp(&f.log)?.hash(&mut h);
        out.push((day * DAY_MS, h.finish()));
    }
    Ok(out)
}

/// One key of what the files of a series' days in `[from, to)` are now ([`day_stamps`] of those days as one): it
/// changes whenever one of them is written, made or removed.
pub(crate) fn days_stamp<P: Point>(ctx: &Ctx, car_dir: &Path, source: Stream, from: u64, to: u64) -> Result<u64> {
    use std::hash::{Hash, Hasher};
    let dir = series_dir::<P>(car_dir, source);
    let mut packs = Packs::new(ctx, &dir);
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for day in days_in::<P>(ctx, car_dir, source, from, to)? {
        let f = day_files(&dir, day);
        // a day in a month's pack: what it is there, as in [`day_stamps`]
        let snap = match crate::plain::stamp(&f.snap)? {
            Some(st) => Some(st),
            None => packs.entry(day)?.map(|e| e.stamp()),
        };
        (day, snap, crate::plain::stamp(&f.log)?).hash(&mut h);
    }
    Ok(h.finish())
}

/// Bytes on disk of every series of a car.
pub(crate) fn bytes(car_dir: &Path) -> Result<u64> {
    let mut total = 0;
    let rd = match std::fs::read_dir(car_dir.join("seg")) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e.into()),
    };
    for series in rd {
        for f in std::fs::read_dir(series?.path())? {
            total += f?.metadata()?.len();
        }
    }
    Ok(total)
}

/// Settles a day (nothing left in its journal, its snapshot at the level finished days are kept at: the small form,
/// see [`columns::encode_small`]). Returns `None` when there was nothing to do, else about how many bytes of columns
/// the day was.
fn settle_day<P: Point>(ctx: &Ctx, dir: &Path, day: u64) -> Result<Option<u64>> {
    let files = day_files(dir, day);
    if !files.log.exists() {
        match snapshot_head(ctx, &files, Kind::Segment)? {
            // folded while it was written, emptied, or written cold before there was the small form
            Some((_, level, _)) if level < DayLog::<P>::settled_level(ctx) => {}
            // settled already, or in the month's pack
            Some(_) | None => return Ok(None),
        }
    }
    let mut lf = load_day::<P>(ctx, dir, day)?;
    if lf.settled(ctx) {
        return Ok(None);
    }
    let bytes = lf.weight();
    if !lf.state.points.is_empty() {
        lf.fold(ctx, Which::Cold)?;
        return Ok(Some(bytes));
    }
    if !lf.has_journal() && files.snap.exists() && pack::entry(ctx, dir, day)?.is_some() {
        // emptied, and its empty snapshot is there until the pack drops the day
        return Ok(None);
    }
    empty_day(ctx, dir, day, lf)?;
    Ok(Some(bytes))
}

/// Bytes of columns folded per call of [`compact`] that also packs, about: a second or so of work under the car's
/// lock (a day's columns are written several ways and compressed hard).
const FOLD_PLAIN_MAX: u64 = 1 << 20;

/// The two kinds of series, for the passes over every series of a car.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Series {
    Samples,
    Charge,
}

/// A series of a car: its kind, its stream (as its place in [`Stream::ALL`]).
type SeriesOf = (Series, usize);

fn every_series() -> impl Iterator<Item = (SeriesOf, Stream)> {
    Stream::ALL.into_iter().enumerate().flat_map(|(i, s)| [((Series::Samples, i), s), ((Series::Charge, i), s)])
}

fn dir_of(car_dir: &Path, series: Series, stream: Stream) -> PathBuf {
    match series {
        Series::Samples => series_dir::<Sample>(car_dir, stream),
        Series::Charge => series_dir::<ChargePoint>(car_dir, stream),
    }
}

/// What a pass has to look at in one series, from its listing alone. To fold: the days that ended before `before`
/// and have a journal (however old: a late upload, an import into old days), or a snapshot file not known to be
/// cold (at or after `known_cold_before`). To pack: per month that ended before `before`, the days with a snapshot
/// file and no journal.
struct Todo {
    fold: Vec<u64>,
    pack: Vec<(Month, usize)>,
}

fn todo(dir: &Path, known_cold_before: u64, before: u64) -> Result<Todo> {
    let l = listing(dir)?;
    let ended = |day: u64| (day + 1) * DAY_MS <= before;
    let mut fold: BTreeSet<u64> = l.logs.iter().copied().filter(|d| ended(*d)).collect();
    fold.extend(l.snaps.iter().copied().filter(|d| ended(*d) && d * DAY_MS >= known_cold_before));
    let mut pack: Vec<(Month, usize)> = Vec::new();
    for day in l.snaps.iter().copied().filter(|d| !l.logs.contains(d)) {
        let m = Month::of(day);
        if m.end * DAY_MS > before {
            continue;
        }
        match pack.last_mut() {
            Some((last, n)) if *last == m => *n += 1,
            Some(_) | None => pack.push((m, 1)),
        }
    }
    Ok(Todo { fold: fold.into_iter().collect(), pack })
}

/// What [`compact`] did.
pub(crate) struct Compacted {
    pub folded: usize,
    pub packed: usize,
    /// Days of the months' packs written again in the small form.
    pub recoded: usize,
    /// It stopped at `max_days` with work left.
    pub more: bool,
    /// Every day before this is now known to have nothing in its journal and a cold snapshot (or to be in a pack).
    pub known_cold_before: u64,
}

/// One pass over every series of a car, at most about `max_days` of work (and, when `packing`, about a second or two
/// of it: [`FOLD_PLAIN_MAX`], [`pack::group_month`]), oldest first: settles the days that ended
/// before `before` (see [`todo`]), then, when `packing` and with all of them settled, packs the months that ended
/// before it ([`pack::pack_month`]; whole months, so the last one may go past `max_days`), and last writes the
/// packs' days again that are not yet as small as they go: together with their neighbours ([`pack::group_month`]),
/// or each in the small form ([`pack::recode_month`], when days are not packed together). Under the car's lock.
pub(crate) fn compact(
    ctx: &Ctx,
    car_dir: &Path,
    known_cold_before: u64,
    before: u64,
    max_days: usize,
    packing: bool,
) -> Result<Compacted> {
    let mut out = Compacted { folded: 0, packed: 0, recoded: 0, more: false, known_cold_before };
    // a step always gets somewhere
    let max_days = max_days.max(1);
    let mut fold: Vec<(u64, SeriesOf, Stream)> = Vec::new();
    for (series, stream) in every_series() {
        let t = todo(&dir_of(car_dir, series.0, stream), known_cold_before, before)?;
        fold.extend(t.fold.into_iter().map(|d| (d, series, stream)));
    }
    fold.sort_unstable_by_key(|f| (f.0, f.1));
    // looking at a snapshot's head is little work next to folding a day, but not none
    let mut folded_bytes = 0;
    for (looked, (day, series, stream)) in fold.into_iter().enumerate() {
        let long = packing && folded_bytes >= FOLD_PLAIN_MAX;
        if out.folded >= max_days || looked >= max_days.saturating_mul(64) || long {
            out.more = true;
            out.known_cold_before = known_cold_before.max(day * DAY_MS);
            return Ok(out);
        }
        let dir = dir_of(car_dir, series.0, stream);
        let did = match series.0 {
            Series::Samples => settle_day::<Sample>(ctx, &dir, day)?,
            Series::Charge => settle_day::<ChargePoint>(ctx, &dir, day)?,
        };
        out.folded += usize::from(did.is_some());
        folded_bytes += did.unwrap_or(0);
    }
    out.known_cold_before = known_cold_before.max(before);
    if !packing {
        return Ok(out);
    }
    for (series, stream) in every_series() {
        let dir = dir_of(car_dir, series.0, stream);
        for (month, _) in todo(&dir, out.known_cold_before, before)?.pack {
            if out.folded + out.packed >= max_days {
                out.more = true;
                return Ok(out);
            }
            out.packed += match series.0 {
                Series::Samples => pack::pack_month::<Sample>(ctx, &dir, month)?,
                Series::Charge => pack::pack_month::<ChargePoint>(ctx, &dir, month)?,
            };
        }
    }
    if !ctx.opts.small_columns {
        return Ok(out);
    }
    let grouping = ctx.opts.group_bytes > 0;
    for (series, stream) in every_series() {
        let dir = dir_of(car_dir, series.0, stream);
        for month in listing(&dir)?.packs {
            if pack::stale(ctx, &dir, month)? == 0 {
                continue;
            }
            let done = out.folded + out.packed + out.recoded;
            if done >= max_days {
                out.more = true;
                return Ok(out);
            }
            // days not yet in the small form are first written again a few at a time, each by itself (the slow
            // part: every column tried several ways); packing a month's days together is then one pass over them
            let alone = !grouping || pack::below(ctx, &dir, month, LEVEL_SMALL)? > 0;
            let (recoded, mut more) = match (series.0, alone) {
                (Series::Samples, true) => pack::recode_month::<Sample>(ctx, &dir, month, max_days - done, grouping)?,
                (Series::Charge, true) => {
                    pack::recode_month::<ChargePoint>(ctx, &dir, month, max_days - done, grouping)?
                }
                (Series::Samples, false) => pack::group_month::<Sample>(ctx, &dir, month)?,
                (Series::Charge, false) => pack::group_month::<ChargePoint>(ctx, &dir, month)?,
            };
            more |= alone && grouping;
            out.recoded += recoded;
            if more {
                out.more = true;
                return Ok(out);
            }
        }
    }
    Ok(out)
}

/// How many days [`compact`] would still fold, pack or write again in the small form, from listings and the packs'
/// indexes alone (a snapshot file not yet known to be settled counts, though a look at it may find nothing to do).
pub(crate) fn pending(ctx: &Ctx, car_dir: &Path, known_cold_before: u64, before: u64) -> Result<usize> {
    let mut n = 0;
    for (series, stream) in every_series() {
        let dir = dir_of(car_dir, series.0, stream);
        let t = todo(&dir, known_cold_before, before)?;
        let packable: usize = t.pack.iter().map(|p| p.1).sum();
        // a day of a month to pack that is also to fold (its snapshot file not known cold) counts once
        let l = listing(&dir)?;
        let both = t
            .fold
            .iter()
            .filter(|d| !l.logs.contains(d) && l.snaps.contains(d) && Month::of(**d).end * DAY_MS <= before)
            .count();
        n += t.fold.len() + packable - both;
        if ctx.opts.small_columns {
            for month in l.packs {
                n += pack::stale(ctx, &dir, month)?;
            }
        }
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn dates_round_trip() {
        assert_eq!(day_name(0), "1970-01-01");
        assert_eq!(day_name(1_794_787_200_000 / DAY_MS), "2026-11-16");
        for d in (0..80_000).step_by(37) {
            assert_eq!(parse_day(&day_name(d)), Some(d));
        }
        assert_eq!(parse_day("2026-02-30"), None);
        assert_eq!(parse_day("2026-1-01"), None);
        assert_eq!(parse_day("../../etc"), None);
    }
}
