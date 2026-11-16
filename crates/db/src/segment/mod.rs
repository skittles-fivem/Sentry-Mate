//! Time series per car per UTC day: [`model::Sample`]s (1 Hz positions and car state) and [`model::ChargePoint`]s,
//! one series per kind and source, so a CAN rebuild never touches what a TeslaMate import brought:
//!
//! ```text
//! db/cars/<car>/seg/<samples|charge>.<can|teslamate>/<YYYY-MM-DD>.snap   the day, columns (see columns), compressed
//! db/cars/<car>/seg/<samples|charge>.<can|teslamate>/<YYYY-MM-DD>.log    windows written since: (from, to, points)
//! ```
//!
//! A write replaces every point of a window. A window that covers whole days writes those days' snapshots
//! directly; a part of a day is appended to its journal without reading the day (cheap enough to do every minute
//! for every car), and the journal is folded into the day's snapshot once it is about as big as the snapshot.
//! Folding while writing uses the warm level; [`crate::Db::settle`] packs finished days at the cold level, the smallest.

pub(crate) mod columns;
mod points;

pub use columns::{Col, Derived, Fields, Point, decode, decode_fields, encode};

use crate::codec;
use crate::error::{Error, Result};
use crate::file::{Ctx, Kind, Which, check_header, header, remove_if_exists};
use crate::journal::{Files, Journal};
use crate::logged::{LEVEL_COLD, Logged, LoggedFile, append_only, snapshot_head};
use columns::{Reader, put_varint};
use model::Source;
use std::path::{Path, PathBuf};

pub const DAY_MS: u64 = 86_400_000;

/// A day kept for a series.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DayInfo {
    /// `YYYY-MM-DD` (UTC).
    pub date: String,
    pub start_ms: u64,
    /// Bytes on disk (snapshot and journal).
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

pub(crate) fn source_name(s: Source) -> &'static str {
    match s {
        Source::Can => "can",
        Source::Teslamate => "teslamate",
    }
}

pub(crate) fn series_dir<P: Point>(car_dir: &Path, source: Source) -> PathBuf {
    car_dir.join("seg").join(format!("{}.{}", P::SERIES, source_name(source)))
}

fn day_files(dir: &Path, day: u64) -> Files {
    Files::named(dir, &day_name(day))
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

    fn fold_due(ctx: &Ctx, journal_bytes: u64, _entries: u64, snap_bytes: u64, _snap_plain: u64) -> bool {
        // the journal is compressed like the snapshot, so compare packed sizes
        journal_bytes >= snap_bytes.max(ctx.opts.fold_min_bytes)
    }
}

/// The generation a day's files are at (the newer of snapshot and journal headers).
fn current_gen(ctx: &Ctx, files: &Files) -> Result<u64> {
    let snap = snapshot_head(ctx, files, Kind::Segment)?.map_or(0, |(g, _, _)| g);
    let log = Journal::open_for_append(ctx, &files.log)?.map_or(0, |o| o.generation);
    if log > snap {
        return Err(Error::Damaged {
            file: ctx.label(&files.log),
            why: format!("it follows snapshot {log} but the snapshot is {snap}"),
        });
    }
    Ok(snap)
}

/// Writes a whole day as a new cold snapshot (or removes it when there are no points), replacing what was there.
fn write_whole_day<P: Point>(ctx: &Ctx, files: &Files, points: Vec<P>) -> Result<()> {
    files.clean_temporary()?;
    if points.is_empty() && !files.snap.exists() && !files.log.exists() {
        return Ok(());
    }
    let generation = current_gen(ctx, files)? + 1;
    let mut b = header(Kind::Segment);
    b.extend_from_slice(&generation.to_be_bytes());
    let which = if points.is_empty() { Which::Hot } else { Which::Cold };
    b.push(if which == Which::Cold { LEVEL_COLD } else { 0 });
    b.extend_from_slice(&codec::compress(&encode(&points), ctx.level(which))?);
    ctx.write_atomic(&files.snap, &b)?;
    ctx.crash_point(crate::Crash::BeforeJournalRemoved)?;
    remove_if_exists(&files.log)?;
    if points.is_empty() {
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
    Ok(())
}

/// Replaces every point of `[from, to)` of a series with `points`. Returns the first day touched.
pub(crate) fn write<P: Point>(
    ctx: &Ctx,
    car_dir: &Path,
    source: Source,
    from: u64,
    to: u64,
    points: &[P],
) -> Result<Option<u64>> {
    check_points(from, to, points)?;
    if from == to {
        return Ok(None);
    }
    let dir = series_dir::<P>(car_dir, source);
    let (first, last) = (from / DAY_MS, (to - 1) / DAY_MS);
    let existing = if last - first > 62 { Some(list_days(&dir)?) } else { None };
    let mut rest = points;
    for day in first..=last {
        let (ds, de) = (day * DAY_MS, (day + 1) * DAY_MS);
        let (wf, wt) = (from.max(ds), to.min(de));
        let n = rest.partition_point(|p| p.t_ms() < wt);
        let (pts, after) = rest.split_at(n);
        rest = after;
        if pts.is_empty() && existing.as_ref().is_some_and(|e| e.binary_search(&day).is_err()) {
            continue;
        }
        let files = day_files(&dir, day);
        if wf == ds && wt == de {
            write_whole_day(ctx, &files, pts.to_vec())?;
            continue;
        }
        if pts.is_empty() && !files.snap.exists() && !files.log.exists() {
            continue;
        }
        let op = Replace { from: wf, to: wt, points: pts.to_vec() };
        if append_only::<DayLog<P>>(ctx, &files, &op)? {
            let mut lf = LoggedFile::<DayLog<P>>::load(ctx, files)?;
            if lf.state.points.is_empty() {
                lf.remove_files(ctx)?;
            } else {
                lf.fold(ctx, Which::Warm)?;
            }
        }
    }
    Ok(Some(first))
}

/// The days a series has, in order.
pub(crate) fn list_days(dir: &Path) -> Result<Vec<u64>> {
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut days = Vec::new();
    for e in rd {
        let name = e?.file_name();
        let name = name.to_string_lossy();
        if let Some(d) = name.strip_suffix(".snap").or_else(|| name.strip_suffix(".log")).and_then(parse_day) {
            days.push(d);
        }
    }
    days.sort_unstable();
    days.dedup();
    Ok(days)
}

/// The days of `[from, to)` that have files.
fn days_in(dir: &Path, from: u64, to: u64) -> Result<Vec<u64>> {
    if from >= to {
        return Ok(Vec::new());
    }
    let (first, last) = (from / DAY_MS, (to - 1) / DAY_MS);
    if last - first > 62 {
        return Ok(list_days(dir)?.into_iter().filter(|d| (first..=last).contains(d)).collect());
    }
    Ok((first..=last)
        .filter(|&d| {
            let f = day_files(dir, d);
            f.snap.exists() || f.log.exists()
        })
        .collect())
}

/// Every point of `[from, to)`, in time order.
pub(crate) fn read<P: Point>(ctx: &Ctx, car_dir: &Path, source: Source, from: u64, to: u64) -> Result<Vec<P>> {
    let dir = series_dir::<P>(car_dir, source);
    let mut out = Vec::new();
    for day in days_in(&dir, from, to)? {
        let lf = LoggedFile::<DayLog<P>>::load(ctx, day_files(&dir, day))?;
        let pts = lf.state.points;
        let a = pts.partition_point(|p| p.t_ms() < from);
        let b = pts.partition_point(|p| p.t_ms() < to);
        out.extend(pts.into_iter().skip(a).take(b - a));
    }
    Ok(out)
}

/// Some fields of `[from, to)`, as columns. A day with nothing in its journal is read straight from its snapshot,
/// decoding only those fields.
pub(crate) fn read_fields<P: Point>(
    ctx: &Ctx,
    car_dir: &Path,
    source: Source,
    from: u64,
    to: u64,
    names: &[&str],
) -> Result<Fields> {
    let cols = columns::field_columns::<P>(names)?;
    let dir = series_dir::<P>(car_dir, source);
    let mut out = Fields::new(names);
    for day in days_in(&dir, from, to)? {
        let files = day_files(&dir, day);
        let fields = if files.log.exists() {
            let lf = LoggedFile::<DayLog<P>>::load(ctx, files)?;
            let mut f = Fields::new(names);
            f.t_ms = lf.state.points.iter().map(Point::t_ms).collect();
            for ((_, v), c) in f.values.iter_mut().zip(&cols) {
                *v = lf.state.points.iter().map(|p| c.value(p)).collect();
            }
            f
        } else {
            let label = ctx.label(&files.snap);
            let Some(b) = ctx.read(&files.snap)? else { continue };
            let body = check_header(&b, Kind::Segment, &label)?;
            let plain = body.get(9..).ok_or_else(|| Error::Corrupt("cut short".into())).and_then(codec::decompress);
            decode_fields::<P>(&plain.map_err(|e| e.in_file(&label))?, names).map_err(|e| e.in_file(&label))?
        };
        out.extend_range(fields, from, to);
    }
    Ok(out)
}

/// The days of a series, with their size.
pub(crate) fn days<P: Point>(car_dir: &Path, source: Source) -> Result<Vec<DayInfo>> {
    let dir = series_dir::<P>(car_dir, source);
    let mut out = Vec::new();
    for day in list_days(&dir)? {
        let f = day_files(&dir, day);
        let size = |p: &Path| std::fs::metadata(p).map_or(0, |m| m.len());
        out.push(DayInfo { date: day_name(day), start_ms: day * DAY_MS, bytes: size(&f.snap) + size(&f.log) });
    }
    Ok(out)
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

/// Packs the days of a series in `[from, before)` at the cold level (nothing left in their journals). Returns how
/// many it packed.
pub(crate) fn settle<P: Point>(ctx: &Ctx, car_dir: &Path, source: Source, from: u64, before: u64) -> Result<usize> {
    let dir = series_dir::<P>(car_dir, source);
    let mut n = 0;
    for day in list_days(&dir)? {
        if day * DAY_MS < from || (day + 1) * DAY_MS > before {
            continue;
        }
        let files = day_files(&dir, day);
        let cold = !files.log.exists() && snapshot_head(ctx, &files, Kind::Segment)?.is_some_and(|h| h.1 == LEVEL_COLD);
        if cold {
            continue;
        }
        let mut lf = LoggedFile::<DayLog<P>>::load(ctx, files)?;
        if lf.settled() {
            continue;
        }
        if lf.state.points.is_empty() {
            lf.remove_files(ctx)?;
        } else {
            lf.fold(ctx, Which::Cold)?;
        }
        n += 1;
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
