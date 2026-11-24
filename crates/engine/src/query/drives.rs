//! `/cars/{car}/drives`, a drive, its track, `/drive-stats`.

use super::system::Shown;
use super::{
    CarRef, Paging, Period, QResult, QueryError, Scoped, Where, car_of, cmp_key, make_after, parse_place, r, scoped,
    where_of,
};
use crate::Engine;
use crate::pipeline::all_rows;
use model::{Car, CarId, Drive, Sample, Source};
use serde::Serialize;
use std::collections::BinaryHeap;

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct DriveFlags {
    pub in_progress: bool,
    /// How long the drive has no data (stretches between its samples longer than [`gap_limit`] where the records say
    /// logs are missing, failed their check, paused or lost frames): the same in the list and the detail.
    pub gap_ms: u64,
    /// How many frames were lost: `0` when the drive's logs lost none; null when they did (the bridge's `# dropped`
    /// lines say frames were lost there, not how many) and for imported history.
    pub lost_frames: Option<u64>,
    /// Null: no signal read for it yet (the car's cold-locked part of the pack is not decoded).
    pub reduced_range: Option<bool>,
    /// Whether the drive's logs lost frames (`flags=gap` matches it).
    #[serde(skip)]
    pub frames_dropped: bool,
}

/// Samples further apart than this are a gap in a drive's data (and break its track).
pub const GAP_MS: u64 = 10_000;

/// How far apart two of a drive's samples may be before the stretch between them is a gap (ms): [`GAP_MS`], or, where
/// either is an API's reading alone (one every few seconds to two minutes), as long as the timeline counts an API's
/// reading ([`timeline::Params::api_max_hold_ms`]).
pub(crate) fn gap_limit(e: &Engine, api: bool) -> u64 {
    if api { GAP_MS.max(e.options().params.api_max_hold_ms) } else { GAP_MS }
}

/// Whether the second `t` of a live drive's samples is an API's alone (`marks`: [`timeline::Fused::marks`]).
fn api_second(marks: &[(u64, model::Inputs)], t: u64) -> bool {
    marks.binary_search_by_key(&t, |m| m.0).is_ok_and(|i| !marks[i].1.contains(model::Input::Smo))
}

/// The seconds a live drive has data in `[a, b)`, each with whether only an API read it: SMO's and those of the
/// APIs that gave the drive's seconds, by time.
fn data_seconds(e: &Engine, d: &Drive, a: u64, b: u64) -> QResult<Vec<(u64, bool)>> {
    let read = |stream: model::Stream| -> QResult<Vec<u64>> {
        Ok(e.db().read_fields::<Sample>(d.car, stream, a, b, &["speed_kmh"])?.t_ms)
    };
    let mut t: Vec<(u64, bool)> = read(model::Stream::Smo)?.into_iter().map(|x| (x, false)).collect();
    for input in d.inputs.without_smo().iter() {
        t.extend(read(model::Stream::from(input))?.into_iter().map(|x| (x, true)));
    }
    // (a second SMO has is SMO's)
    t.sort_unstable();
    t.dedup_by_key(|x| x.0);
    Ok(t)
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DriveView {
    pub id: String,
    pub car: CarRef,
    pub time_zone: String,
    pub start_ms: u64,
    pub end_ms: Option<u64>,
    pub duration_ms: u64,
    pub start: Where,
    pub end: Where,
    pub start_odometer_km: Option<f64>,
    pub end_odometer_km: Option<f64>,
    pub distance_km: Option<f64>,
    pub speed_avg_kph: Option<f64>,
    pub speed_max_kph: Option<f64>,
    pub power_max_kw: Option<f64>,
    pub power_min_kw: Option<f64>,
    pub energy_kwh: Option<f64>,
    /// `measured` (CAN: the pack's power), `estimated` (imported: from range).
    pub energy_basis: &'static str,
    pub regen_kwh: Option<f64>,
    pub wh_per_km: Option<f64>,
    pub efficiency_pct: Option<f64>,
    pub start_soc_pct: Option<f64>,
    pub end_soc_pct: Option<f64>,
    pub start_range_km: Option<f64>,
    pub end_range_km: Option<f64>,
    pub outside_avg_c: Option<f64>,
    pub cabin_avg_c: Option<f64>,
    pub climb_m: Option<f64>,
    pub descent_m: Option<f64>,
    pub hvac_kwh: Option<f64>,
    #[serde(flatten)]
    pub from: super::Sources,
    /// The values an import filled where the live row had none (field → the import): empty for a row's own values.
    pub value_sources: super::gapfill::ValueSources,
    pub flags: DriveFlags,
    pub purpose: Option<serde_json::Value>,
}

/// What a drive's row needs besides the drive.
pub(crate) struct Ctx {
    pub places: std::sync::Arc<crate::placing::Places>,
    /// The id of the drive going on now, if any (the car's newest values say it is driving, and are fresh).
    pub live: Option<u64>,
    /// The drive the car's newest values say is going on, but whose data stopped: no values for over
    /// [`super::now::STALE_MS`] (the bridge lost power or its connection mid-drive).
    pub stopped: Option<u64>,
    pub rated_wh_per_km: Option<f64>,
    pub now_ms: u64,
}

pub(crate) fn ctx(e: &Engine, c: &Car) -> QResult<Ctx> {
    let now_ms = crate::now_ms();
    let snap = e.db().entry(c.id)?.and_then(|x| x.summary.now).filter(|n| n.driving);
    let newest = snap.as_ref().and_then(|n| {
        e.db()
            .list::<Drive>(c.id, &db::Query { source: Some(Source::Live), ..db::Query::default().limit(1) })
            .ok()
            .and_then(|p| p.items.into_iter().next())
            .filter(|d| d.end_ms + 120_000 >= n.at_ms)
            .map(|d| d.id)
    });
    let fresh = snap.as_ref().is_some_and(|n| now_ms.saturating_sub(n.at_ms) <= super::now::STALE_MS);
    let (live, stopped) = if fresh { (newest, None) } else { (None, newest) };
    Ok(Ctx { places: e.places(), live, stopped, rated_wh_per_km: c.efficiency_wh_km, now_ms })
}

pub(crate) fn view(c: &Car, d: &Drive, x: &Ctx) -> DriveView {
    let live = x.live == Some(d.id);
    let stopped = x.stopped == Some(d.id) || d.end.is_none();
    // a drive going on lasts up to now
    let duration = if live { x.now_ms.max(d.end_ms) } else { d.end_ms }.saturating_sub(d.start_ms);
    let whkm = d.efficiency_wh_km();
    let (sp, ep) = (d.start.as_ref(), d.end.as_ref());
    let tm = d.source.is_imported();
    DriveView {
        id: d.id.to_string(),
        car: CarRef(c.id),
        time_zone: c.time_zone.clone(),
        start_ms: d.start_ms,
        end_ms: (!live).then_some(d.end_ms),
        duration_ms: duration,
        start: where_of(&x.places, sp.map(|p| p.lat), sp.map(|p| p.lon), d.start_address.as_deref(), None),
        end: where_of(
            &x.places,
            ep.map(|p| p.lat),
            ep.map(|p| p.lon),
            d.end_address.as_deref(),
            (live || stopped).then_some(if live { "Driving now" } else { "Data stopped here" }),
        ),
        start_odometer_km: r(d.start_odometer_km, 3),
        end_odometer_km: r(d.end_odometer_km, 3),
        distance_km: r(d.distance_km, 3),
        speed_avg_kph: r(d.distance_km.filter(|_| duration > 0).map(|km| km / (duration as f64 / 3_600_000.0)), 1),
        speed_max_kph: r(d.speed_max_kmh, 1),
        power_max_kw: r(d.power_max_kw, 1),
        power_min_kw: r(d.power_min_kw, 1),
        energy_kwh: r(d.energy_used_kwh, 3),
        // the APIs alone (no SMO): their power, read every few seconds to a minute, summed
        energy_basis: if tm || (!d.inputs.is_empty() && !d.inputs.contains(model::Input::Smo)) {
            "estimated"
        } else {
            "measured"
        },
        regen_kwh: None,
        wh_per_km: r(whkm, 1),
        efficiency_pct: r(x.rated_wh_per_km.zip(whkm).filter(|(_, w)| *w > 0.0).map(|(rated, w)| rated / w * 100.0), 1),
        start_soc_pct: r(d.start_soc, 1),
        end_soc_pct: r(d.end_soc, 1),
        start_range_km: r(d.start_range_km, 1),
        end_range_km: r(d.end_range_km, 1),
        outside_avg_c: r(d.outside_temp_avg_c, 1),
        cabin_avg_c: r(d.inside_temp_avg_c, 1),
        climb_m: r(d.ascent_m, 0),
        descent_m: r(d.descent_m, 0),
        hvac_kwh: None,
        from: super::Sources::of(d.source, d.inputs),
        value_sources: Default::default(),
        // gaps and lost frames are marked by the list and the detail ([`Cover`])
        flags: DriveFlags { in_progress: live, ..DriveFlags::default() },
        purpose: None,
    }
}

/// [`view`] of a drive with the values the chosen import fills where the live row has none ([`super::gapfill`]),
/// named in `value_sources` and `sources`; the filled drive too (for figures read from it).
pub(crate) fn view_filled(c: &Car, d: &Drive, x: &Ctx, gaps: &super::gapfill::Gaps) -> QResult<(DriveView, Drive)> {
    let mut d = d.clone();
    let filled = gaps.drive(&mut d)?;
    let mut v = view(c, &d, x);
    if !filled.is_empty() {
        v.value_sources = filled.value_sources();
        v.from = filled.sources(&v.from);
    }
    Ok((v, d))
}

/// `/cars/{car}/drives` filters.
#[derive(Debug, Clone, Default)]
pub struct DrivesQuery {
    pub period: Period,
    /// `places=p1,p2` and which end they are matched at: `start`, `end` or `any` (default).
    pub places: Option<String>,
    pub place_side: Option<String>,
    /// Text in a start or end label.
    pub q: Option<String>,
    pub min_distance_km: Option<f64>,
    pub min_avg_kph: Option<f64>,
    pub source: Option<String>,
    /// `gap`, `in_progress`, `reduced_range`: rows with any of them.
    pub flags: Option<String>,
    /// `plain` (default) or `hill_corrected`.
    pub efficiency: Option<String>,
    /// `start` (default), `distance`, `duration`, `energy`, `wh_per_km`, `speed_max`.
    pub sort: Option<String>,
    pub dir: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DrivesSummary {
    pub count: u64,
    pub distance_km: f64,
    pub duration_ms: u64,
    pub energy_kwh: f64,
    pub energy_measured_kwh: f64,
    pub energy_estimated_kwh: f64,
    pub wh_per_km: Option<f64>,
    pub without_energy: u64,
    pub efficiency: &'static str,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DrivesPage {
    pub items: Vec<DriveView>,
    pub next: Option<String>,
    pub summary: DrivesSummary,
}

pub(crate) fn place_list(s: Option<&str>, places: &crate::placing::Places) -> QResult<Option<Vec<u32>>> {
    place_list_named(s, places, "places")
}

/// A comma list of place ids, each a saved place; a bad one is `400` on `field`.
pub(crate) fn place_list_named(
    s: Option<&str>,
    places: &crate::placing::Places,
    field: &str,
) -> QResult<Option<Vec<u32>>> {
    let Some(s) = s.filter(|s| !s.is_empty()) else { return Ok(None) };
    let mut out = Vec::new();
    for p in s.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let id = parse_place(p)
            .filter(|&id| places.get(id).is_some())
            .ok_or_else(|| QueryError::bad(field, "no such place"))?;
        out.push(id);
    }
    Ok(Some(out))
}

/// What a car's records say about its data over a stretch: where decoded logs have data (merged when less than
/// [`GAP_MS`] apart), where they say it is missing (logs left out, pauses in recording) and which logs lost frames.
/// Read a day of records at a time; only the merged spans are kept.
pub(crate) struct Cover {
    data: Vec<(u64, u64)>,
    holes: Vec<(u64, u64)>,
    dropped: Vec<(u64, u64)>,
}

/// Sorts spans and joins those less than `slack` apart.
fn merge(v: &mut Vec<(u64, u64)>, slack: u64) {
    v.retain(|(a, b)| b > a);
    v.sort_unstable();
    let mut out: Vec<(u64, u64)> = Vec::with_capacity(v.len());
    for &(a, b) in v.iter() {
        match out.last_mut() {
            Some(l) if a <= l.1.saturating_add(slack) => l.1 = l.1.max(b),
            _ => out.push((a, b)),
        }
    }
    *v = out;
}

/// Whether merged spans meet `[a, b)`.
fn meets(v: &[(u64, u64)], a: u64, b: u64) -> bool {
    let i = v.partition_point(|&(_, y)| y <= a);
    v.get(i).is_some_and(|&(x, _)| x < b)
}

impl Cover {
    /// The records of `car` for `[a, b)`.
    pub(crate) fn read(e: &Engine, car: CarId, a: u64, b: u64) -> QResult<Cover> {
        use crate::records::{self, Outcome};
        let mut c = Cover { data: Vec::new(), holes: Vec::new(), dropped: Vec::new() };
        if a >= b {
            return Ok(c);
        }
        let db = e.db();
        let meta = records::meta(db, car)?;
        let (lo, hi) = (records::day_of(a).saturating_sub(1), records::day_of(b.saturating_sub(1)));
        for &day in meta.days.range(lo..=hi.max(lo)) {
            for en in records::day(db, car, day)?.logs.into_values() {
                let (x, y) = en.span();
                if y < a || x >= b {
                    continue;
                }
                match &en.outcome {
                    Outcome::Hole { from_ms, to_ms, .. } => c.holes.push((*from_ms, *to_ms)),
                    Outcome::Decoded { facts, .. } => {
                        c.data.push((x, y.max(x + 1)));
                        for n in &facts.not_recorded {
                            let from = n.paused_ms.map_or(n.at_ms.saturating_sub(1), |p| n.at_ms.saturating_sub(p));
                            c.holes.push((from, n.at_ms));
                        }
                        if facts.dropped > 0 {
                            c.dropped.push((x, y.max(x + 1)));
                        }
                    }
                }
            }
            // keep what is held small however long the stretch
            if c.data.len() + c.holes.len() + c.dropped.len() > 4096 {
                c.settle();
            }
        }
        c.settle();
        Ok(c)
    }

    fn settle(&mut self) {
        merge(&mut self.data, GAP_MS);
        merge(&mut self.holes, 0);
        merge(&mut self.dropped, 0);
    }

    /// Whether decoded logs cover `[a, b)` whole (to within [`GAP_MS`] at each edge and between logs).
    fn covers(&self, a: u64, b: u64) -> bool {
        let i = self.data.partition_point(|&(_, y)| y.saturating_add(GAP_MS) < b);
        self.data.get(i).is_some_and(|&(x, _)| x <= a.saturating_add(GAP_MS))
    }

    /// Whether the logs of `[a, b)` lost frames.
    fn lost(&self, a: u64, b: u64) -> bool {
        meets(&self.dropped, a, b)
    }

    /// Whether the records leave part of `[a, b)` without data, or say frames were lost in it.
    fn doubtful(&self, a: u64, b: u64) -> bool {
        !self.covers(a, b) || meets(&self.holes, a, b) || self.lost(a, b)
    }
}

/// Marks a drive's gaps and lost frames on its row, and answers its gaps: the stretches between its samples longer
/// than [`gap_limit`], looked for only where the records doubt its data (a drive its logs cover whole, with no pause
/// and no lost frame, has none). The list and the detail mark a drive alike: both read the seconds of SMO and of the
/// APIs the drive names. Imported history has no records: no gaps, lost frames null.
pub(crate) fn mark_gaps(e: &Engine, d: &Drive, cover: &Cover, v: &mut DriveView) -> QResult<Vec<Gap>> {
    if d.source != Source::Live {
        return Ok(Vec::new());
    }
    let (a, b) = (d.start_ms, d.end_ms.max(d.start_ms) + 1);
    let lost = cover.lost(a, b);
    v.flags.frames_dropped = lost;
    v.flags.lost_frames = (!lost).then_some(0);
    if !cover.doubtful(a, b) {
        return Ok(Vec::new());
    }
    let (smo_limit, api_limit) = (gap_limit(e, false), gap_limit(e, true));
    let gaps: Vec<Gap> = data_seconds(e, d, a, b)?
        .windows(2)
        .filter(|w| w[1].0.saturating_sub(w[0].0) > if w[0].1 || w[1].1 { api_limit } else { smo_limit })
        .map(|w| Gap {
            start_ms: w[0].0 + 1000,
            end_ms: w[1].0,
            kind: if cover.lost(w[0].0, w[1].0 + 1) { "dropped" } else { "unknown" },
        })
        .collect();
    v.flags.gap_ms = gaps.iter().map(|g| g.end_ms.saturating_sub(g.start_ms)).sum();
    Ok(gaps)
}

/// The drives of a car in a period (by start), as shown (overlaps with imported history settled).
pub(crate) fn rows(
    e: &Engine,
    car: CarId,
    period: &Period,
    source: Option<super::SourceFilter>,
) -> QResult<Vec<Drive>> {
    let (a, b) = period.bounds()?;
    let all: Vec<Drive> = all_rows(e.db(), car, a, b, source.map(super::SourceFilter::family))?;
    let shown = Shown::new(e, car, source)?;
    Ok(all
        .into_iter()
        .filter(|d| d.start_ms >= a && d.start_ms < b && shown.keep(d.source, d.inputs, d.start_ms))
        .collect())
}

/// Calls `f` with each row of `car` whose start is in `[a, b)` and that is shown (overlaps with imported history
/// settled), newest first or oldest first, reading the db a page at a time: `f` answers whether to go on. Nothing
/// is held but the page being read.
#[allow(clippy::too_many_arguments)]
pub(crate) fn each<T: db::Item>(
    e: &Engine,
    car: CarId,
    a: u64,
    b: u64,
    source: Option<super::SourceFilter>,
    shown: &Shown,
    newest_first: bool,
    mut f: impl FnMut(T) -> QResult<bool>,
) -> QResult<()> {
    if a >= b {
        return Ok(());
    }
    let mut q = db::Query::between(a, b).limit(500);
    if !newest_first {
        q = q.oldest_first();
    }
    q.source = source.map(super::SourceFilter::family);
    loop {
        let page = e.db().list::<T>(car, &q)?;
        for row in page.items {
            let (start, src) = (row.start_ms(), row.source());
            if start >= a && start < b && shown.keep(src, row.inputs(), start) && !f(row)? {
                return Ok(());
            }
        }
        match page.next {
            Some(c) => q.after = Some(c),
            None => return Ok(()),
        }
    }
}

/// The first shown row of `car` in `[a, b)`, newest or oldest.
fn first<T: db::Item>(
    e: &Engine,
    car: CarId,
    a: u64,
    b: u64,
    shown: &Shown,
    newest_first: bool,
    keep: impl Fn(&T) -> bool,
) -> QResult<Option<T>> {
    let mut out = None;
    each::<T>(e, car, a, b, None, shown, newest_first, |row| {
        if keep(&row) {
            out = Some(row);
            return Ok(false);
        }
        Ok(true)
    })?;
    Ok(out)
}

/// A row ranked by its list key, for [`Best`].
struct Ranked<T> {
    key: (f64, String),
    desc: bool,
    row: T,
}

impl<T> PartialEq for Ranked<T> {
    fn eq(&self, o: &Self) -> bool {
        self.cmp(o) == std::cmp::Ordering::Equal
    }
}
impl<T> Eq for Ranked<T> {}
impl<T> PartialOrd for Ranked<T> {
    fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(o))
    }
}
impl<T> Ord for Ranked<T> {
    fn cmp(&self, o: &Self) -> std::cmp::Ordering {
        cmp_key(&self.key, &o.key, self.desc)
    }
}

/// The first `k` rows of a list in its order, after a cursor, kept while the rows stream past (never more than `k`
/// at once, however many the period holds).
pub(crate) struct Best<T> {
    k: usize,
    desc: bool,
    after: Option<(f64, String)>,
    heap: BinaryHeap<Ranked<T>>,
}

impl<T> Best<T> {
    pub(crate) fn new(k: usize, desc: bool, after: Option<(f64, String)>) -> Best<T> {
        Best { k, desc, after, heap: BinaryHeap::with_capacity(k + 1) }
    }

    pub(crate) fn offer(&mut self, key: (f64, String), row: T) {
        if self.after.as_ref().is_some_and(|at| cmp_key(&key, at, self.desc) != std::cmp::Ordering::Greater) {
            return;
        }
        if self.heap.len() >= self.k
            && self.heap.peek().is_some_and(|top| cmp_key(&key, &top.key, self.desc) != std::cmp::Ordering::Less)
        {
            return;
        }
        self.heap.push(Ranked { key, desc: self.desc, row });
        if self.heap.len() > self.k {
            self.heap.pop();
        }
    }

    /// The rows in the list's order, with their keys.
    pub(crate) fn sorted(self) -> Vec<((f64, String), T)> {
        self.heap.into_sorted_vec().into_iter().map(|r| (r.key, r.row)).collect()
    }
}

/// A page of the list from a [`Best`] of `limit + 1`: the rows and the cursor after the last (when more follow).
pub(crate) fn page_of<T>(best: Best<T>, limit: usize, sig: &str) -> QResult<(Vec<T>, Option<String>)> {
    let mut rows = best.sorted();
    let more = rows.len() > limit;
    rows.truncate(limit);
    let next = match rows.last() {
        Some((key, _)) if more => Some(make_after(key, sig)?),
        _ => None,
    };
    Ok((rows.into_iter().map(|(_, r)| r).collect(), next))
}

/// What `/cars/{car}/drives` filters with, checked.
struct Filter {
    source: Option<super::SourceFilter>,
    side: &'static str,
    hill: bool,
    flags: Vec<String>,
    places: Option<Vec<u32>>,
    needle: Option<String>,
    min_distance_km: Option<f64>,
    min_avg_kph: Option<f64>,
}

impl Filter {
    fn new(q: &DrivesQuery, places: &crate::placing::Places) -> QResult<Filter> {
        let side = match q.place_side.as_deref() {
            None | Some("") | Some("any") => "any",
            Some("start") => "start",
            Some("end") => "end",
            Some(_) => return Err(QueryError::bad("place_side", "start, end or any")),
        };
        let hill = match q.efficiency.as_deref() {
            None | Some("") | Some("plain") => false,
            Some("hill_corrected") => true,
            Some(_) => return Err(QueryError::bad("efficiency", "plain or hill_corrected")),
        };
        let flags: Vec<String> = q.flags.as_deref().map_or_else(Vec::new, |f| {
            f.split(',').map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect()
        });
        if let Some(bad) = flags.iter().find(|f| !["gap", "in_progress", "reduced_range"].contains(&f.as_str())) {
            return Err(QueryError::bad("flags", &format!("{bad}: gap, in_progress or reduced_range")));
        }
        Ok(Filter {
            source: super::parse_source(q.source.as_deref())?,
            side,
            hill,
            flags,
            places: place_list(q.places.as_deref(), places)?,
            needle: q.q.as_deref().map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty()),
            min_distance_km: q.min_distance_km,
            min_avg_kph: q.min_avg_kph,
        })
    }

    /// Whether the row passes (after its efficiency basis is set).
    fn keeps(&self, v: &DriveView) -> bool {
        if let Some(ps) = &self.places {
            let at = |w: &Where| w.place.as_ref().and_then(|p| parse_place(&p.id)).is_some_and(|id| ps.contains(&id));
            // a drive still going has no destination yet
            let end = !v.flags.in_progress && at(&v.end);
            let ok = match self.side {
                "start" => at(&v.start),
                "end" => end,
                _ => at(&v.start) || end,
            };
            if !ok {
                return false;
            }
        }
        if let Some(n) = &self.needle {
            let p = |k: &str| v.purpose.as_ref().and_then(|p| p.get(k)).and_then(serde_json::Value::as_str);
            let hit = texts(&v.start)
                .into_iter()
                .chain(texts(&v.end))
                .chain([p("value"), p("relation"), p("note")])
                .flatten()
                .any(|t| t.to_lowercase().contains(n));
            if !hit {
                return false;
            }
        }
        if self.min_distance_km.is_some_and(|m| v.distance_km.is_none_or(|d| d < m)) {
            return false;
        }
        if self.min_avg_kph.is_some_and(|m| v.speed_avg_kph.is_none_or(|s| s < m)) {
            return false;
        }
        if !self.flags.is_empty() {
            let f = &v.flags;
            let any = self.flags.iter().any(|w| match w.as_str() {
                "gap" => f.gap_ms > 0 || f.frames_dropped || f.lost_frames.is_some_and(|n| n > 0),
                "in_progress" => f.in_progress,
                _ => f.reduced_range == Some(true),
            });
            if !any {
                return false;
            }
        }
        true
    }
}

/// The texts of a drive's end a search looks in.
fn texts(w: &Where) -> [Option<&str>; 2] {
    [w.label.as_deref(), w.address.short.as_deref()]
}

/// The car's mass (kg) the hill-corrected basis needs. None is known yet: no signal the server decodes, no setting
/// and no profile carries it, so the basis answers null rather than a guessed mass.
fn mass_kg(_c: &Car) -> Option<f64> {
    None
}

/// Sets a row's `wh_per_km` and `efficiency_pct` on the hill-corrected basis: the potential energy of the net climb
/// (ascent less descent: what a descent gives back by regen) taken off, with the car's own mass; null when the mass,
/// the climb or the energy is not known, or the result is not above 0.
fn hill_corrected(v: &mut DriveView, d: &Drive, rated: Option<f64>, mass_kg: Option<f64>) {
    let w = (|| {
        let net_m = d.ascent_m? - d.descent_m?;
        let km = d.distance_km.filter(|km| *km > 0.1)?;
        let kwh = d.energy_used_kwh? - mass_kg? * 9.81 * net_m / 3.6e6;
        Some(kwh * 1000.0 / km)
    })();
    v.wh_per_km = r(w.filter(|w| *w > 0.0), 1);
    v.efficiency_pct = r(rated.zip(v.wh_per_km).map(|(rated, w)| rated / w * 100.0), 1);
}

/// Whether a drive has a consumption the pages show: not one still under way, under 1 km, or with a gap over 60 s in
/// its data (the pages show "—" for those).
fn has_figure(v: &DriveView) -> bool {
    v.wh_per_km.is_some()
        && !v.flags.in_progress
        && v.distance_km.is_some_and(|km| km >= 1.0)
        && v.flags.gap_ms <= 60_000
}

/// The drives list's sort key. By consumption, a drive without a figure goes last in either direction (the lowest key
/// going down, the highest going up).
fn sort_key(sort: &str, v: &DriveView, desc: bool) -> (f64, String) {
    let n = |x: Option<f64>| x.unwrap_or(f64::NEG_INFINITY);
    let figure =
        || v.wh_per_km.filter(|_| has_figure(v)).unwrap_or(if desc { f64::NEG_INFINITY } else { f64::INFINITY });
    let k = match sort {
        "distance" => n(v.distance_km),
        "duration" => v.duration_ms as f64,
        "energy" => n(v.energy_kwh),
        "wh_per_km" => figure(),
        "speed_max" => n(v.speed_max_kph),
        _ => v.start_ms as f64,
    };
    (k, v.id.clone())
}

/// One pass over the period's drives (a page of the db at a time) for `/cars/{car}/drives`: each row that passes the
/// filters, with its sort key, goes to `take`; the summary is summed on the way.
fn pass(
    e: &Engine,
    c: &Car,
    q: &DrivesQuery,
    desc: bool,
    mut take: impl FnMut((f64, String), DriveView),
) -> QResult<DrivesSummary> {
    let car = c.id;
    let x = ctx(e, c)?;
    let f = Filter::new(q, &x.places)?;
    let sort = sort_of(q)?;
    let (a, b) = q.period.bounds()?;
    let shown = Shown::new(e, car, f.source)?;
    let gaps = super::gapfill::Gaps::new(e, car, f.source)?;
    let mut purposes = super::logbook::Purposes::new(e, c)?;
    let mut sum = Summer::new(f.hill);
    let mass = mass_kg(c);
    // the records of the stretch the rows can cover (a drive that starts in the period may end after it)
    let cover = if f.source.is_some_and(|s| s.family() == Source::Teslamate) {
        Cover { data: Vec::new(), holes: Vec::new(), dropped: Vec::new() }
    } else {
        Cover::read(e, car, a.saturating_sub(GAP_MS), b.saturating_add(2 * 86_400_000))?
    };
    each::<Drive>(e, car, a, b, f.source, &shown, desc, |d| {
        let (mut v, filled) = view_filled(c, &d, &x, &gaps)?;
        mark_gaps(e, &d, &cover, &mut v)?;
        v.purpose = purposes.of(e, &filled, &v)?;
        // (the consumption over the climb from the row's own climb: an imported one is never counted as measured)
        if f.hill {
            hill_corrected(&mut v, &d, x.rated_wh_per_km, mass);
        }
        if f.keeps(&v) {
            sum.add(&v);
            take(sort_key(sort, &v, desc), v);
        }
        Ok(true)
    })?;
    Ok(sum.done())
}

fn sort_of(q: &DrivesQuery) -> QResult<&str> {
    let sort = q.sort.as_deref().filter(|s| !s.is_empty()).unwrap_or("start");
    if !["start", "distance", "duration", "energy", "wh_per_km", "speed_max"].contains(&sort) {
        return Err(QueryError::bad("sort", "start, distance, duration, energy, wh_per_km or speed_max"));
    }
    Ok(sort)
}

/// `/cars/{car}/drives`: one pass over the period's drives that sums the summary and keeps only the page asked for.
pub fn drives(e: &Engine, car: CarId, q: &DrivesQuery, paging: &Paging) -> QResult<Scoped<DrivesPage>> {
    let c = car_of(e, car)?;
    sort_of(q)?;
    let desc = super::parse_dir(q.dir.as_deref(), true)?;
    let limit = paging.limit()?;
    let sig = format!("drives|{car}|{}|{q:?}", q.period.sig());
    let mut best = Best::new(limit + 1, desc, super::read_after(paging, &sig)?);
    let summary = pass(e, &c, q, desc, |key, v| best.offer(key, v))?;
    let (items, next) = page_of(best, limit, &sig)?;
    Ok(scoped(&c, DrivesPage { items, next, summary }))
}

/// Every row of `/cars/{car}/drives` that passes the filters, in the list's order, each given to `f` (a CSV carries
/// them all): one pass over the period's drives, not one per page. Only the matching rows are held, to be sorted.
pub fn each_drive(e: &Engine, car: CarId, q: &DrivesQuery, mut f: impl FnMut(DriveView) -> QResult<()>) -> QResult<()> {
    let c = car_of(e, car)?;
    sort_of(q)?;
    let desc = super::parse_dir(q.dir.as_deref(), true)?;
    let mut rows: Vec<((f64, String), DriveView)> = Vec::new();
    pass(e, &c, q, desc, |key, v| rows.push((key, v)))?;
    rows.sort_by(|p, o| cmp_key(&p.0, &o.0, desc));
    for (_, v) in rows {
        f(v)?;
    }
    Ok(())
}

/// The drives summary, summed row by row.
struct Summer {
    s: DrivesSummary,
    /// The distance and energy of the rows with a consumption on the basis asked (rows without one are left out).
    km_with_wh: f64,
    basis_kwh: f64,
}

impl Summer {
    fn new(hill: bool) -> Summer {
        Summer {
            s: DrivesSummary {
                count: 0,
                distance_km: 0.0,
                duration_ms: 0,
                energy_kwh: 0.0,
                energy_measured_kwh: 0.0,
                energy_estimated_kwh: 0.0,
                wh_per_km: None,
                without_energy: 0,
                efficiency: if hill { "hill_corrected" } else { "plain" },
            },
            km_with_wh: 0.0,
            basis_kwh: 0.0,
        }
    }

    fn add(&mut self, v: &DriveView) {
        let s = &mut self.s;
        s.count += 1;
        let km = v.distance_km.unwrap_or(0.0);
        s.distance_km += km;
        s.duration_ms += v.duration_ms;
        // the basis of wh_per_km: the row's own consumption (hill corrected or not)
        if let Some(w) = v.wh_per_km.filter(|_| km > 0.1) {
            self.km_with_wh += km;
            self.basis_kwh += w * km / 1000.0;
        }
        match v.energy_kwh {
            Some(kwh) => {
                s.energy_kwh += kwh;
                if v.energy_basis == "measured" {
                    s.energy_measured_kwh += kwh;
                } else {
                    s.energy_estimated_kwh += kwh;
                }
            }
            None => s.without_energy += 1,
        }
    }

    fn done(mut self) -> DrivesSummary {
        let s = &mut self.s;
        s.wh_per_km = r((self.km_with_wh > 0.1).then(|| self.basis_kwh * 1000.0 / self.km_with_wh), 1);
        s.distance_km = r(Some(s.distance_km), 3).unwrap_or(0.0);
        s.energy_kwh = r(Some(s.energy_kwh), 3).unwrap_or(0.0);
        s.energy_measured_kwh = r(Some(s.energy_measured_kwh), 3).unwrap_or(0.0);
        s.energy_estimated_kwh = r(Some(s.energy_estimated_kwh), 3).unwrap_or(0.0);
        self.s
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SpeedBand {
    pub from_kph: f64,
    pub to_kph: f64,
    pub seconds: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Gap {
    pub start_ms: u64,
    pub end_ms: u64,
    pub kind: &'static str,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DriveDetail {
    #[serde(flatten)]
    pub drive: DriveView,
    pub prev: Option<String>,
    pub next: Option<String>,
    pub prev_charge: Option<String>,
    pub next_charge: Option<String>,
    pub speed_band_kph: f64,
    pub speed_bands: Vec<SpeedBand>,
    pub gaps: Vec<Gap>,
    pub rated_wh_per_km: Option<f64>,
    pub start_ideal_range_km: Option<f64>,
    pub end_ideal_range_km: Option<f64>,
    pub est_wh_per_km: Option<f64>,
    pub est_basis: &'static str,
}

/// The samples of a drive (its own source's), in time order, with the seconds that are not SMO's alone
/// ([`timeline::Fused::marks`]; none for imported history).
fn samples_marked(e: &Engine, car: CarId, d: &Drive) -> QResult<crate::tesla_in::LiveFused> {
    let to = d.end_ms.max(d.start_ms) + 1;
    if d.source == Source::Live {
        // SMO's and the APIs', made one as the drive was built from
        return Ok(e.live_fused(car, d.start_ms, to)?);
    }
    let samples = e.db().read_points::<Sample>(car, d.source, d.start_ms, to)?;
    Ok(crate::tesla_in::LiveFused { samples, marks: Vec::new() })
}

/// `/cars/{car}/drives/{drive}`; `speed_band_kph` is 1 to 100 (default 10).
pub fn drive(e: &Engine, car: CarId, id: u64, speed_band_kph: Option<f64>) -> QResult<DriveDetail> {
    let c = car_of(e, car)?;
    let band = speed_band_kph.unwrap_or(10.0);
    if !(1.0..=100.0).contains(&band) {
        return Err(QueryError::bad("speed_band_kph", "1 to 100"));
    }
    let d: Drive = e.db().get(car, id)?.ok_or(QueryError::NotFound("no_such_drive"))?;
    let x = ctx(e, &c)?;
    let shown = Shown::new(e, car, None)?;
    // the neighbours, read from the db a page at a time from the drive outwards
    let prev = first::<Drive>(e, car, 0, d.start_ms, &shown, true, |o| o.id != d.id)?.map(|o| o.id.to_string());
    let next =
        first::<Drive>(e, car, d.start_ms.saturating_add(1), u64::MAX, &shown, false, |o| o.start_ms > d.start_ms)?
            .map(|o| o.id.to_string());
    let prev_charge =
        first::<model::Charge>(e, car, 0, d.start_ms, &shown, true, |_| true)?.map(|ch| ch.id.to_string());
    let live = x.live == Some(d.id);
    let next_charge = if live {
        None
    } else {
        first::<model::Charge>(e, car, d.start_ms, u64::MAX, &shown, false, |ch| ch.start_ms >= d.start_ms)?
            .map(|ch| ch.id.to_string())
    };
    let crate::tesla_in::LiveFused { samples: s, marks } = samples_marked(e, car, &d)?;
    let mut bands: std::collections::BTreeMap<u64, u64> = Default::default();
    // the energy back into the pack: the negative battery power over the drive (each sample's power held until the
    // next, for the gap limit at most: an API's readings are far apart)
    let (mut regen, mut with_power) = (0.0, false);
    for w in s.windows(2) {
        let dt = w[1].t_ms.saturating_sub(w[0].t_ms);
        let limit = gap_limit(e, api_second(&marks, w[0].t_ms) || api_second(&marks, w[1].t_ms));
        if let Some(p) = w[0].battery_power_kw.filter(|p| p.is_finite()) {
            with_power = true;
            if p < 0.0 {
                regen += -p * dt.min(limit) as f64 / 3_600_000.0;
            }
        }
        if dt > limit {
            continue;
        }
        if let Some(v) = w[0].speed_kmh.filter(|v| *v > 0.5) {
            *bands.entry((v / band).floor() as u64).or_default() += dt;
        }
    }
    let speed_bands = bands
        .into_iter()
        .map(|(k, ms)| SpeedBand {
            from_kph: r(Some(k as f64 * band), 3).unwrap_or(0.0),
            to_kph: r(Some((k + 1) as f64 * band), 3).unwrap_or(0.0),
            seconds: ms / 1000,
        })
        .collect();
    // the consumption of the 30 days of drives before this one
    let (mut kwh, mut km) = (0.0, 0.0);
    each::<Drive>(e, car, d.start_ms.saturating_sub(30 * 86_400_000), d.start_ms, None, &shown, false, |o| {
        if let (Some(a), Some(b)) = (o.energy_used_kwh, o.distance_km) {
            kwh += a;
            km += b;
        }
        Ok(true)
    })?;
    let gaps = super::gapfill::Gaps::new(e, car, None)?;
    let (mut view, filled) = view_filled(&c, &d, &x, &gaps)?;
    let cover = Cover::read(e, car, d.start_ms.saturating_sub(GAP_MS), d.end_ms.saturating_add(GAP_MS + 1))?;
    let gaps = mark_gaps(e, &d, &cover, &mut view)?;
    view.purpose = super::logbook::Purposes::new(e, &c)?.of(e, &filled, &view)?;
    if d.source == Source::Live && with_power {
        view.regen_kwh = r(Some(regen), 3);
    }
    Ok(DriveDetail {
        drive: view,
        prev,
        next,
        prev_charge,
        next_charge,
        speed_band_kph: band,
        speed_bands,
        gaps,
        rated_wh_per_km: c.efficiency_wh_km,
        start_ideal_range_km: r(s.iter().find_map(|x| x.ideal_range_km), 1),
        end_ideal_range_km: r(s.iter().rev().find_map(|x| x.ideal_range_km), 1),
        est_wh_per_km: r((km > 1.0).then(|| kwh * 1000.0 / km), 1),
        est_basis: "wh_per_km_30d",
    })
}

/// One point of a drive's track.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TrackPoint {
    pub t_ms: u64,
    pub lat: f64,
    pub lon: f64,
    pub elevation_m: Option<f64>,
    pub accuracy_m: Option<f64>,
    pub speed_kph: Option<f64>,
    pub power_kw: Option<f64>,
    pub soc_pct: Option<f64>,
    /// The values of this point an import gave (where the live drive had none; all of them for a point of the
    /// import's own, in a stretch the live data does not cover): left out when none.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub imported: Vec<&'static str>,
}

/// The track fields and the sample fields they are read from.
const TRACK_FIELDS: &[(&str, &str)] = &[
    ("lat", "lat"),
    ("lon", "lon"),
    ("elevation_m", "elevation_m"),
    ("accuracy_m", "gps_accuracy_m"),
    ("speed_kph", "speed_kmh"),
    ("power_kw", "battery_power_kw"),
    ("soc_pct", "soc"),
];

/// A drive's track: one point a second with a fix, cut into stretches where data is missing for longer than
/// [`gap_limit`] (10 s, or an API's pace where only an API read the car).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Track {
    pub car: CarRef,
    pub drive: String,
    pub name: String,
    pub start_ms: u64,
    pub time_zone: String,
    pub stretches: Vec<Vec<TrackPoint>>,
    /// The track fields an import gave values to (field → the import): empty when all are the drive's own.
    pub value_sources: super::gapfill::ValueSources,
}

pub fn track(e: &Engine, car: CarId, id: u64) -> QResult<Track> {
    track_with(e, car, id, None)
}

/// [`track`], with the car's import gap filling at hand (one for many tracks keeps the import's days read).
pub(crate) fn track_with(e: &Engine, car: CarId, id: u64, gaps: Option<&super::gapfill::Gaps<'_>>) -> QResult<Track> {
    let c = car_of(e, car)?;
    let d: Drive = e.db().get(car, id)?.ok_or(QueryError::NotFound("no_such_drive"))?;
    let mut stretches: Vec<Vec<TrackPoint>> = Vec::new();
    let mut last_t: Option<u64> = None;
    let crate::tesla_in::LiveFused { samples: own, marks } = samples_marked(e, car, &d)?;
    // a live drive's track with what the chosen import fills: values it lacks, and the import's points where the
    // drive's own data has a gap
    let mut given = if d.source == Source::Live {
        let own_gaps;
        let gaps = match gaps {
            Some(g) => g,
            None => {
                own_gaps = super::gapfill::Gaps::new(e, car, None)?;
                &own_gaps
            }
        };
        let to = d.end_ms.max(d.start_ms) + 1;
        gaps.samples(own, d.start_ms, to, GAP_MS, super::gapfill::Own::WhenOn)?
    } else {
        super::gapfill::Given { samples: own, masks: Vec::new() }
    };
    // the map draws no fix the car can't have been at: a placeholder, or a jump faster than a car drives (an
    // import's own samples were not fused, so they are checked here too)
    timeline::drop_bad_fixes(&mut given.samples, timeline::MAX_FIX_KMH);
    let mut value_sources = super::gapfill::ValueSources::new();
    for s in &given.samples {
        let (Some(lat), Some(lon)) = (s.lat, s.lon) else { continue };
        let mut imported = Vec::new();
        for (_, site, mask) in given.at(s.t_ms) {
            for (name, field) in TRACK_FIELDS {
                let bit = timeline::gapfill::slot_bit(field);
                let has = match *name {
                    "elevation_m" => s.elevation_m.is_some(),
                    "accuracy_m" => s.gps_accuracy_m.is_some(),
                    "speed_kph" => s.speed_kmh.is_some(),
                    "power_kw" => s.battery_power_kw.is_some(),
                    "soc_pct" => s.soc.is_some(),
                    _ => true,
                };
                if has && (*mask == u64::MAX || mask & bit != 0) && !imported.contains(name) {
                    imported.push(*name);
                    let r = super::SourceRef::from(model::Origin::Import(*site));
                    let v = value_sources.entry((*name).to_string()).or_default();
                    if !v.contains(&r) {
                        v.push(r);
                    }
                }
            }
        }
        let p = TrackPoint {
            t_ms: s.t_ms,
            lat,
            lon,
            elevation_m: s.elevation_m,
            accuracy_m: s.gps_accuracy_m,
            speed_kph: s.speed_kmh,
            power_kw: s.battery_power_kw,
            soc_pct: s.soc,
            imported,
        };
        let apart =
            |t: u64| s.t_ms.saturating_sub(t) > gap_limit(e, api_second(&marks, t) || api_second(&marks, s.t_ms));
        if last_t.is_none_or(apart) || stretches.is_empty() {
            stretches.push(Vec::new());
        }
        if let Some(st) = stretches.last_mut() {
            st.push(p);
        }
        last_t = Some(s.t_ms);
    }
    Ok(Track {
        car: CarRef(car),
        drive: d.id.to_string(),
        name: c.name.clone(),
        start_ms: d.start_ms,
        time_zone: c.time_zone.clone(),
        stretches,
        value_sources,
    })
}

/// Douglas-Peucker thinning to `tolerance_m`, keeping the ends. A point's distance is to the segment between a
/// stretch's ends, not the endless line through them: a point beyond an end (the turn of an out-and-back) is far.
pub fn simplify(points: &[TrackPoint], tolerance_m: f64) -> Vec<TrackPoint> {
    if points.len() < 3 || tolerance_m <= 0.0 {
        return points.to_vec();
    }
    let mut keep = vec![false; points.len()];
    keep[0] = true;
    keep[points.len() - 1] = true;
    let mut stack = vec![(0usize, points.len() - 1)];
    // metres per degree near the track (a flat projection is fine at this scale)
    let lat0 = points[0].lat.to_radians();
    let (ky, kx) = (111_320.0, 111_320.0 * lat0.cos());
    while let Some((a, b)) = stack.pop() {
        if b <= a + 1 {
            continue;
        }
        let (ax, ay) = (points[a].lon * kx, points[a].lat * ky);
        let (bx, by) = (points[b].lon * kx, points[b].lat * ky);
        let (dx, dy) = (bx - ax, by - ay);
        let len2 = dx * dx + dy * dy;
        let mut best = (0.0, a);
        for (i, p) in points.iter().enumerate().take(b).skip(a + 1) {
            let (px, py) = (p.lon * kx - ax, p.lat * ky - ay);
            // the nearest point of the segment, as a share of the way from a to b
            let k = if len2 > 0.0 { ((px * dx + py * dy) / len2).clamp(0.0, 1.0) } else { 0.0 };
            let (ex, ey) = (px - k * dx, py - k * dy);
            let dist = (ex * ex + ey * ey).sqrt();
            if dist > best.0 {
                best = (dist, i);
            }
        }
        if best.0 > tolerance_m {
            keep[best.1] = true;
            stack.push((a, best.1));
            stack.push((best.1, b));
        }
    }
    points.iter().zip(keep).filter(|(_, k)| *k).map(|(p, _)| p.clone()).collect()
}

/// The track as GeoJSON: one LineString per stretch, coordinates `[lon, lat, elevation_m]`, with parallel `t_ms` and
/// `accuracy_m` (and `speed_kph`, `power_kw`, `soc_pct` when asked in `extensions`).
pub fn geojson(t: &Track, simplify_m: Option<f64>, extensions: &[&str]) -> serde_json::Value {
    let features: Vec<serde_json::Value> = t
        .stretches
        .iter()
        .map(|st| {
            let pts = simplify_m.map_or_else(|| st.clone(), |m| simplify(st, m));
            let coords: Vec<serde_json::Value> = pts
                .iter()
                .map(|p| match p.elevation_m {
                    Some(el) => serde_json::json!([p.lon, p.lat, el]),
                    None => serde_json::json!([p.lon, p.lat]),
                })
                .collect();
            let mut props = serde_json::Map::new();
            props.insert("t_ms".into(), pts.iter().map(|p| p.t_ms).collect::<Vec<_>>().into());
            props.insert("accuracy_m".into(), serde_json::json!(pts.iter().map(|p| p.accuracy_m).collect::<Vec<_>>()));
            for ext in extensions {
                let col: Vec<Option<f64>> = match *ext {
                    "speed" => pts.iter().map(|p| p.speed_kph).collect(),
                    "power" => pts.iter().map(|p| p.power_kw).collect(),
                    "soc" => pts.iter().map(|p| p.soc_pct).collect(),
                    _ => continue,
                };
                let name = match *ext {
                    "speed" => "speed_kph",
                    "power" => "power_kw",
                    _ => "soc_pct",
                };
                props.insert(name.into(), serde_json::json!(col));
            }
            // per point, the values an import gave it (only on a stretch with any)
            if pts.iter().any(|p| !p.imported.is_empty()) {
                props.insert("imported".into(), serde_json::json!(pts.iter().map(|p| &p.imported).collect::<Vec<_>>()));
            }
            serde_json::json!({
                "type": "Feature",
                "geometry": { "type": "LineString", "coordinates": coords },
                "properties": props,
            })
        })
        .collect();
    serde_json::json!({ "type": "FeatureCollection", "features": features, "value_sources": t.value_sources })
}

fn iso(ms: u64) -> String {
    let t = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(i64::try_from(ms).unwrap_or(0)).unwrap_or_default();
    t.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

fn xml(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// The track as GPX 1.1: one `trkseg` per stretch, `ele` and `time` on every point.
pub fn gpx(t: &Track, simplify_m: Option<f64>, extensions: &[&str]) -> String {
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<gpx version=\"1.1\" creator=\"Sentry Mate\" \
         xmlns=\"http://www.topografix.com/GPX/1/1\" xmlns:sm=\"https://sentry-mate.invalid/gpx/1\">\n",
    );
    out.push_str(&format!("<trk><name>{} drive {}</name>\n", xml(&t.name), iso(t.start_ms)));
    for st in &t.stretches {
        out.push_str("<trkseg>\n");
        let pts = simplify_m.map_or_else(|| st.clone(), |m| simplify(st, m));
        for p in pts {
            out.push_str(&format!("<trkpt lat=\"{:.6}\" lon=\"{:.6}\">", p.lat, p.lon));
            if let Some(el) = p.elevation_m {
                out.push_str(&format!("<ele>{el:.1}</ele>"));
            }
            out.push_str(&format!("<time>{}</time>", iso(p.t_ms)));
            let mut ext = String::new();
            for x in extensions {
                let (tag, v) = match *x {
                    "speed" => ("speed_kph", p.speed_kph),
                    "power" => ("power_kw", p.power_kw),
                    "soc" => ("soc_pct", p.soc_pct),
                    "accuracy" => ("accuracy_m", p.accuracy_m),
                    _ => continue,
                };
                if let Some(v) = v {
                    ext.push_str(&format!("<sm:{tag}>{v}</sm:{tag}>"));
                }
            }
            if !ext.is_empty() {
                out.push_str(&format!("<extensions>{ext}</extensions>"));
            }
            out.push_str("</trkpt>\n");
        }
        out.push_str("</trkseg>\n");
    }
    out.push_str("</trk>\n</gpx>\n");
    out
}

/// `<car>-drive-<YYYY-MM-DD-HHMM>.<ext>` in the car's zone (anything but letters, digits, `.`, `_`, `-` → `-`).
pub fn track_file_name(t: &Track, ext: &str) -> String {
    let tz = crate::tz::zone(&t.time_zone);
    let local = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(i64::try_from(t.start_ms).unwrap_or(0))
        .unwrap_or_default()
        .with_timezone(&tz);
    let name: String =
        t.name.chars().map(|ch| if ch.is_ascii_alphanumeric() || ".-_".contains(ch) { ch } else { '-' }).collect();
    format!("{name}-drive-{}.{ext}", local.format("%Y-%m-%d-%H%M"))
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DayCount {
    pub date: String,
    pub count: u64,
    pub distance_km: f64,
    pub energy_kwh: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MaxSpeed {
    pub kph: f64,
    pub drive: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MaxSpeeds {
    pub period: Option<MaxSpeed>,
    pub last_30d: Option<MaxSpeed>,
    pub last_7d: Option<MaxSpeed>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StatBand {
    pub from_kph: f64,
    pub to_kph: f64,
    pub seconds: u64,
    pub share_pct: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Projection {
    pub per_month_km: Option<f64>,
    pub per_year_km: Option<f64>,
    pub basis_days: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Destination {
    pub place: Option<String>,
    pub label: Option<String>,
    pub count: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DriveStats {
    pub from_ms: u64,
    pub to_ms: u64,
    pub count: u64,
    pub distance_km: f64,
    pub energy_kwh: f64,
    pub median_distance_km: Option<f64>,
    pub per_day: Vec<DayCount>,
    pub days: u64,
    pub max_speed: MaxSpeeds,
    pub speed_bands: Vec<StatBand>,
    pub projection: Projection,
    pub top_destinations: Vec<Destination>,
}

/// Local days `per_day` holds at most (about 55 years).
const MAX_DAYS: usize = 20_000;
/// The width of `/drive-stats`' speed bands.
const STAT_BAND_KPH: f64 = 10.0;

fn faster(best: &mut Option<MaxSpeed>, d: &Drive) {
    if let Some(s) = d.speed_max_kmh.filter(|s| s.is_finite())
        && best.as_ref().is_none_or(|b| s > b.kph)
    {
        *best = Some(MaxSpeed { kph: r(Some(s), 1).unwrap_or(s), drive: d.id.to_string() });
    }
}

/// `/cars/{car}/drive-stats`: the period's drives (leaving out those that start or end at `exclude_places`), read a
/// page of the db at a time; only sums, the distances (for the median) and a count per local day are kept.
pub fn drive_stats(
    e: &Engine,
    car: CarId,
    period: &Period,
    exclude_places: Option<&str>,
    now_ms: u64,
) -> QResult<Scoped<DriveStats>> {
    let c = car_of(e, car)?;
    let x = ctx(e, &c)?;
    let tz = crate::tz::zone(&c.time_zone);
    let exclude = place_list_named(exclude_places, &x.places, "exclude_places")?.unwrap_or_default();
    let (a, b) = period.bounds()?;
    let end = b.min(now_ms).max(a);
    let shown = Shown::new(e, car, None)?;
    let first_drive = first::<Drive>(e, car, 0, u64::MAX, &shown, false, |_| true)?.map(|d| d.start_ms);
    let first_data = [first_drive, crate::records::meta(e.db(), car)?.first_ms].into_iter().flatten().min();
    let mut per: std::collections::BTreeMap<String, DayCount> = Default::default();
    if let Some(f) = first_data.map(|f| f.max(a)).filter(|f| *f < end) {
        let mut day = crate::tz::date(f, tz);
        let last = crate::tz::date(end - 1, tz);
        while day <= last && per.len() < MAX_DAYS {
            let k = day.format("%Y-%m-%d").to_string();
            per.insert(k.clone(), DayCount { date: k, count: 0, distance_km: 0.0, energy_kwh: 0.0 });
            let Some(n) = day.succ_opt() else { break };
            day = n;
        }
    }
    let (mut count, mut km, mut kwh) = (0u64, 0.0, 0.0);
    let mut dist: Vec<f64> = Vec::new();
    let mut best: Option<MaxSpeed> = None;
    let mut dest: std::collections::BTreeMap<String, Destination> = Default::default();
    let mut bands: std::collections::BTreeMap<u64, u64> = Default::default();
    each::<Drive>(e, car, a, b, None, &shown, false, |d| {
        let v = view(&c, &d, &x);
        let at = |w: &Where| w.place.as_ref().and_then(|p| parse_place(&p.id)).is_some_and(|id| exclude.contains(&id));
        if at(&v.start) || at(&v.end) {
            return Ok(true);
        }
        count += 1;
        km += v.distance_km.unwrap_or(0.0);
        kwh += v.energy_kwh.unwrap_or(0.0);
        dist.extend(v.distance_km);
        faster(&mut best, &d);
        let day = crate::tz::date_str(d.start_ms, tz);
        let p = per.entry(day.clone()).or_insert(DayCount { date: day, count: 0, distance_km: 0.0, energy_kwh: 0.0 });
        p.count += 1;
        p.distance_km += v.distance_km.unwrap_or(0.0);
        p.energy_kwh += v.energy_kwh.unwrap_or(0.0);
        // a drive still going, or whose data stopped, has no destination yet
        if !v.flags.in_progress && d.end.is_some() && x.stopped != Some(d.id) {
            let key = v.end.place.as_ref().map(|p| p.id.clone()).or_else(|| v.end.label.clone());
            if let Some(key) = key {
                let place = v.end.place.as_ref().map(|p| p.id.clone());
                let n = dest.entry(key).or_insert(Destination { place, label: v.end.label.clone(), count: 0 });
                n.count += 1;
            }
        }
        // the time spent in each band, from the drive's speed column alone
        let f =
            e.db().read_fields::<Sample>(car, d.source, d.start_ms, d.end_ms.max(d.start_ms) + 1, &["speed_kmh"])?;
        if let Some((_, col)) = f.values.first() {
            for i in 1..f.t_ms.len() {
                let dt = f.t_ms[i].saturating_sub(f.t_ms[i - 1]);
                if dt > 10_000 {
                    continue;
                }
                if let Some(s) = col[i - 1].filter(|s| *s > 0.5) {
                    *bands.entry((s / STAT_BAND_KPH).floor() as u64).or_default() += dt;
                }
            }
        }
        Ok(true)
    })?;
    dist.sort_by(f64::total_cmp);
    let median = match dist.len() {
        0 => None,
        n if n % 2 == 1 => Some(dist[n / 2]),
        n => Some((dist[n / 2 - 1] + dist[n / 2]) / 2.0),
    };
    // the fastest of the 30 and 7 days to the period's end, whatever the period's start and the places left out
    let (mut last_30d, mut last_7d) = (None, None);
    let week = end.saturating_sub(7 * 86_400_000);
    each::<Drive>(e, car, end.saturating_sub(30 * 86_400_000), end, None, &shown, false, |d| {
        faster(&mut last_30d, &d);
        if d.start_ms >= week {
            faster(&mut last_7d, &d);
        }
        Ok(true)
    })?;
    let total_ms: u64 = bands.values().sum();
    let speed_bands = bands
        .into_iter()
        .map(|(k, ms)| StatBand {
            from_kph: k as f64 * STAT_BAND_KPH,
            to_kph: (k + 1) as f64 * STAT_BAND_KPH,
            seconds: ms / 1000,
            share_pct: r(Some(ms as f64 * 100.0 / total_ms.max(1) as f64), 2).unwrap_or(0.0),
        })
        .collect();
    let per_day: Vec<DayCount> = per
        .into_values()
        .map(|mut p| {
            p.distance_km = r(Some(p.distance_km), 3).unwrap_or(0.0);
            p.energy_kwh = r(Some(p.energy_kwh), 3).unwrap_or(0.0);
            p
        })
        .collect();
    let days = per_day.len() as u64;
    let mut top: Vec<Destination> = dest.into_values().collect();
    top.sort_by(|p, q| q.count.cmp(&p.count).then_with(|| p.label.cmp(&q.label)));
    top.truncate(10);
    Ok(scoped(
        &c,
        DriveStats {
            from_ms: a,
            to_ms: end,
            count,
            distance_km: r(Some(km), 3).unwrap_or(0.0),
            energy_kwh: r(Some(kwh), 3).unwrap_or(0.0),
            median_distance_km: r(median, 3),
            per_day,
            days,
            max_speed: MaxSpeeds { period: best, last_30d, last_7d },
            speed_bands,
            projection: Projection {
                per_month_km: r((days > 0).then(|| km / days as f64 * 30.436_875), 1),
                per_year_km: r((days > 0).then(|| km / days as f64 * 365.25), 0),
                basis_days: days,
            },
            top_destinations: top,
        },
    ))
}
