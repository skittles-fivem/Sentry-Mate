//! Overlaps between imported and CAN history and between two imports, a car's logs and their check results, `/cars/{car}/health`,
//! `/cars/{car}/storage`, and the engine's own figures for `/system/health`.

use super::{List, Paging, Period, QResult, QueryError, Scoped, SourceFilter, car_of, paginate, scoped};
use crate::records::{self, NS, Outcome};
use crate::{Engine, now_ms};
use db::StateRow;
use model::{CarId, ImportSite, Inputs, Source, State};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// Which side of an overlap a span shows: of live and imported history `can` or `teslamate` (the imported side), of
/// two imports the site (`teslamate` or `teslafi`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Flip {
    pub start_ms: u64,
    pub end_ms: u64,
    pub shown: String,
}

impl Flip {
    /// Whether this choice was made for a span that meets `[a, b)`: a span keeps its choice when a later import
    /// moves its start or end (and with it its id), so a choice is matched by the time it covers, not its start.
    fn meets(&self, a: u64, b: u64) -> bool {
        self.start_ms < b.max(a.saturating_add(1)) && a < self.end_ms.max(self.start_ms.saturating_add(1))
    }
}

/// The latest choice made for a span that meets `[a, b)`.
fn chosen_for(chosen: &[Flip], a: u64, b: u64) -> Option<&Flip> {
    chosen.iter().rev().find(|f| f.meets(a, b))
}

fn flips(e: &Engine, car: CarId) -> QResult<Vec<Flip>> {
    Ok(records::get(e.db(), NS, &format!("car/{car}/overlaps"))?.unwrap_or_default())
}

/// The sites chosen for spans both imports cover (`car/{car}/import-overlaps`), and how often a car's overlap choices
/// (of either kind) were changed: the rows' `version` is one more.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct Choices {
    changes: u64,
    sites: Vec<Flip>,
}

fn choices(e: &Engine, car: CarId) -> QResult<Choices> {
    Ok(records::get(e.db(), NS, &format!("car/{car}/import-overlaps"))?.unwrap_or_default())
}

/// The import site shown in each span both imports cover (by start, apart): elsewhere every site's rows are shown.
#[derive(Debug, Clone, Default, Hash)]
pub(crate) struct Sites(Vec<(u64, u64, ImportSite)>);

impl Sites {
    pub(crate) fn new(e: &Engine, car: CarId) -> QResult<Sites> {
        // worked out again only when the imports or the choices change (it may read every position both imports
        // have where they overlap)
        let ch = choices(e, car)?;
        let kept = e.memo(car, "import-sites", crate::memo::On::Imported, crate::memo::key(&ch.sites), || {
            Ok::<_, QueryError>(Sites::of(&import_pairs(e, car, &ch.sites)?))
        })?;
        Ok((*kept).clone())
    }

    fn of(pairs: &[Pair]) -> Sites {
        Sites(pairs.iter().map(|p| (p.start_ms, p.end_ms, p.shown)).collect())
    }

    /// Whether a row or point of `site` at `t` is shown.
    pub(crate) fn shows(&self, site: ImportSite, t: u64) -> bool {
        let i = self.0.partition_point(|s| s.1 <= t);
        self.0.get(i).is_none_or(|s| s.0 > t || s.2 == site)
    }

    pub(crate) fn keeps(&self, source: Source, t: u64) -> bool {
        source.site().is_none_or(|site| self.shows(site, t))
    }
}

/// Whether a row is shown: with `source=all` an imported row inside CAN history is hidden (CAN wins), unless that
/// overlap was flipped to show the imported side; where both imports cover the same time only one site's rows are
/// shown (with `source=all` and `source=import`).
pub(crate) struct Shown {
    filter: Option<SourceFilter>,
    coverage: Vec<(u64, u64)>,
    flips: Vec<Flip>,
    sites: Sites,
}

impl Shown {
    /// A key of what it shows (a kept answer built with it rests on it).
    pub(crate) fn key(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        format!("{:?}", self.filter).hash(&mut h);
        self.coverage.hash(&mut h);
        self.sites.hash(&mut h);
        crate::memo::key(&self.flips).hash(&mut h);
        h.finish()
    }

    pub(crate) fn new(e: &Engine, car: CarId, filter: Option<SourceFilter>) -> QResult<Shown> {
        let sites = match filter {
            None | Some(SourceFilter::Import(None)) => Sites::new(e, car)?,
            Some(_) => Sites::default(),
        };
        let imported = e.db().entry(car)?.is_some_and(|x| {
            let t = &x.summary.tally.teslamate;
            t.drives > 0 || t.charges > 0 || x.summary.tally.states > 0 && t.drives + t.charges > 0
        });
        if filter.is_some() || !imported {
            return Ok(Shown { filter, coverage: Vec::new(), flips: Vec::new(), sites });
        }
        Ok(Shown { filter, coverage: imported_coverage(e, car)?.to_vec(), flips: flips(e, car)?, sites })
    }

    /// Whether a row of `source` from `inputs`, starting at `t`, is shown.
    pub(crate) fn keep(&self, source: Source, inputs: Inputs, t: u64) -> bool {
        if let Some(f) = self.filter {
            return f.keeps(source, inputs) && self.sites.keeps(source, t);
        }
        // the CAN span `t` is in, and the choice made for it (matched by the time it covers, as the list does)
        let i = self.coverage.partition_point(|s| s.1 <= t);
        let span = self.coverage.get(i).filter(|s| s.0 <= t);
        let flipped = span.and_then(|s| chosen_for(&self.flips, s.0, s.1)).map(|f| f.shown.as_str());
        match source {
            Source::Live => flipped != Some("teslamate"),
            Source::Teslamate | Source::Teslafi => {
                (span.is_none() || flipped == Some("teslamate")) && self.sites.keeps(source, t)
            }
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Counts {
    pub drives: u64,
    pub charges: u64,
}

/// Each import site's drives and charges in a span.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct SiteCounts {
    pub teslamate: Counts,
    pub teslafi: Counts,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Overlap {
    /// `overlap:<start_ms>` (live and imported history) or `imports:<start_ms>` (two imports).
    pub id: String,
    /// `live` (live and imported history both exist) or `imports` (both import sites have rows).
    pub kind: &'static str,
    pub start_ms: u64,
    pub end_ms: u64,
    /// `can` or `teslamate` (the old words, kept for one release; `teslamate` is any imported side).
    pub shown: String,
    /// `live`, or the imported side: `import:teslamate`, `import:teslafi` (`import` when both are in a `live` span:
    /// which one shows there is the `imports` span's choice).
    pub shown_source: String,
    /// What the span shows when no one chose: `live`, or (an `imports` span) the import with more data.
    pub default_source: String,
    /// Why that import: `more_drives_and_charges`, `more_samples`, or `tie` (TeslaMate's then); null on a `live`
    /// span.
    pub default_reason: Option<&'static str>,
    /// The live side's counts (`can` is the same, kept for one release).
    pub live: Counts,
    /// The imported side's counts as the lists show it: each real drive and charge once (where both imports cover
    /// the time, the shown site's rows; each site's own are in `sites`). `teslamate` is the same, kept for one release.
    pub imported: Counts,
    /// Each import site's counts.
    pub sites: SiteCounts,
    pub can: Counts,
    pub teslamate: Counts,
    /// The sides, live first.
    pub sources: Vec<super::SourceRef>,
    /// The car's overlap choices' version: a PATCH may send it, and one sent stale is refused.
    pub version: u64,
}

impl Overlap {
    fn settle_words(&mut self) {
        // the imported side by its site (`import:teslamate`, `import:teslafi`), `import` when both sites are in it
        let sites: Vec<&str> = self.sources.iter().filter_map(|s| s.site).collect();
        if self.kind == "live" {
            self.shown_source = match (self.shown == "teslamate", sites.as_slice()) {
                (false, _) => "live".to_string(),
                (true, [one]) => format!("import:{one}"),
                (true, _) => "import".to_string(),
            };
        }
        self.live = self.can.clone();
        self.imported = self.teslamate.clone();
    }
}

/// `shown` as a PATCH of a `live` span may write it: `live` or `can`, `import` (`import:teslamate`, `import:teslafi`)
/// or `teslamate`. Answers the old word.
fn shown_word(w: &str) -> Option<&'static str> {
    match w {
        "live" | "can" | "smo" => Some("can"),
        "import:teslamate" | "import:teslafi" | "teslamate" | "import" => Some("teslamate"),
        _ => None,
    }
}

/// The site a PATCH of an `imports` span names (`import:teslamate`, `import:teslafi`, or the bare site word).
fn site_word(w: &str) -> Option<ImportSite> {
    match w.strip_prefix("import:").unwrap_or(w) {
        "teslamate" => Some(ImportSite::Teslamate),
        "teslafi" => Some(ImportSite::Teslafi),
        _ => None,
    }
}

/// The spans the CAN history covers (online or asleep) over `[from, to)`, joined: the state rows stream past a page at
/// a time, only the joined spans are kept.
fn can_coverage(e: &Engine, car: CarId, from: u64, to: u64) -> QResult<Vec<(u64, u64)>> {
    let mut out: Vec<(u64, u64)> = Vec::new();
    if from >= to {
        return Ok(out);
    }
    let mut q = db::Query::between(from, to).oldest_first().limit(db::MAX_PAGE);
    q.source = Some(Source::Live);
    loop {
        let page = e.db().list::<StateRow>(car, &q)?;
        for s in &page.items {
            if s.span.state == State::Unknown {
                continue;
            }
            match out.last_mut() {
                Some(last) if s.span.start_ms <= last.1.saturating_add(60_000) => last.1 = last.1.max(s.span.end_ms),
                _ => out.push((s.span.start_ms, s.span.end_ms)),
            }
        }
        match page.next {
            Some(c) => q.after = Some(c),
            None => return Ok(out),
        }
    }
}

/// From the first start of a car's imported drives and charges to the end of the last one to start (at least its
/// start, `[first, last + 1)`): imported history can overlap CAN history only there. (To the last one's end, not its
/// start: live logging that began while it was going on meets it.)
pub(crate) fn imported_span(e: &Engine, car: CarId) -> QResult<Option<(u64, u64)>> {
    fn ends<T: db::Item>(e: &Engine, car: CarId) -> QResult<Option<(u64, u64)>> {
        let mut q = db::Query::default().oldest_first().limit(1);
        q.source = Some(Source::Teslamate);
        let Some(first) = e.db().list::<T>(car, &q)?.items.first().map(db::Item::start_ms) else { return Ok(None) };
        let mut q = db::Query::default().limit(1);
        q.source = Some(Source::Teslamate);
        let last = e.db().list::<T>(car, &q)?.items.first().map_or(first, |r| r.start_ms().max(r.end_ms()));
        Ok(Some((first, last.max(first).saturating_add(1))))
    }
    let join = |x: Option<(u64, u64)>, y: Option<(u64, u64)>| match (x, y) {
        (Some(a), Some(b)) => Some((a.0.min(b.0), a.1.max(b.1))),
        (a, b) => a.or(b),
    };
    Ok(join(ends::<model::Drive>(e, car)?, ends::<model::Charge>(e, car)?))
}

/// Where a row of `source` is counted: its place in [`Source::ALL`] (live 0, TeslaMate 1, TeslaFi 2); an imported row
/// the lists show (of the site its span shows, where both imports cover it) also in [`SHOWN`].
fn slot(source: Source) -> usize {
    source.index()
}

/// The slot of an import site's rows ([`slot`]).
fn import_slot(site: ImportSite) -> usize {
    slot(Source::of_site(site))
}

/// The slots of every import site's rows.
fn import_slots() -> Vec<usize> {
    ImportSite::ALL.into_iter().map(import_slot).collect()
}

/// Counts each row of a table (of one source's family) into the span its start falls in, by its source ([`slot`]):
/// one pass, oldest first and a page at a time, over the spans' reach, with a pointer that only moves forward.
fn count_into<T: db::Item>(
    e: &Engine,
    car: CarId,
    family: Source,
    spans: &[(u64, u64)],
    sites: &Sites,
    counts: &mut [Slots],
) -> QResult<()> {
    let (Some(first), Some(last)) = (spans.first(), spans.last()) else { return Ok(()) };
    let mut q = db::Query::between(first.0, last.1).oldest_first().limit(db::MAX_PAGE);
    q.source = Some(family);
    let mut i = 0usize;
    loop {
        let page = e.db().list::<T>(car, &q)?;
        for r in &page.items {
            let t = r.start_ms();
            while spans.get(i).is_some_and(|s| s.1 <= t) {
                i += 1;
            }
            if spans.get(i).is_some_and(|s| s.0 <= t)
                && let Some(c) = counts.get_mut(i)
            {
                if let Some(n) = c.get_mut(slot(r.source())) {
                    *n += 1;
                }
                if r.source().site().is_some_and(|site| sites.shows(site, t))
                    && let Some(n) = c.get_mut(SHOWN)
                {
                    *n += 1;
                }
            }
        }
        match page.next {
            Some(c) => q.after = Some(c),
            None => return Ok(()),
        }
    }
}

/// The slot of the imported rows the lists show ([`slot`]): one per real drive or charge where both imports cover a
/// span.
const SHOWN: usize = Source::ALL.len();

/// A span's count per [`slot`], and the shown imported rows in [`SHOWN`].
type Slots = [u64; SHOWN + 1];

/// Drives and charges of a span from its counts: every import site's, one site's, the shown ones, or the live side's.
fn counts_of(d: &Slots, c: &Slots, slots: &[usize]) -> Counts {
    let sum = |v: &Slots| slots.iter().filter_map(|&k| v.get(k)).sum();
    Counts { drives: sum(d), charges: sum(c) }
}

/// A span's drives and charges, by source ([`slot`]).
type SpanCounts = (Slots, Slots);

/// The spans' counts of drives and charges, by source (the imported rows `sites` shows in [`SHOWN`]).
fn span_counts(
    e: &Engine,
    car: CarId,
    family: Source,
    spans: &[(u64, u64)],
    sites: &Sites,
) -> QResult<Vec<SpanCounts>> {
    let (mut d, mut c) = (vec![Slots::default(); spans.len()], vec![Slots::default(); spans.len()]);
    count_into::<model::Drive>(e, car, family, spans, sites, &mut d)?;
    count_into::<model::Charge>(e, car, family, spans, sites, &mut c)?;
    Ok(d.into_iter().zip(c).collect())
}

/// The `live` overlaps among `spans` (CAN coverage): the spans that hold imported drives or charges, with both sides'
/// counts. The imported side counts each real drive and charge once, as the lists show it: where both imports cover
/// the time, only the rows of the site `sites` shows there (each site's own counts are in `sites`).
fn overlaps_of(
    e: &Engine,
    car: CarId,
    spans: &[(u64, u64)],
    fl: &[Flip],
    sites: &Sites,
    version: u64,
) -> QResult<Vec<Overlap>> {
    let imported = span_counts(e, car, Source::Teslamate, spans, sites)?;
    let held: Vec<((u64, u64), SpanCounts)> = spans
        .iter()
        .copied()
        .zip(imported)
        .filter(|(_, (d, c))| counts_of(d, c, &import_slots()) != Counts::default())
        .collect();
    // the CAN side counted over the overlaps only
    let live = span_counts(e, car, Source::Live, &held.iter().map(|h| h.0).collect::<Vec<_>>(), &Sites::default())?;
    let mut items = Vec::new();
    for (((a, b), (d, c)), (ld, lc)) in held.into_iter().zip(live) {
        let sites = site_counts(&d, &c);
        let origins: Vec<model::Origin> = std::iter::once(model::Origin::Smo)
            .chain(
                [(ImportSite::Teslamate, &sites.teslamate), (ImportSite::Teslafi, &sites.teslafi)]
                    .into_iter()
                    .filter(|(_, n)| **n != Counts::default())
                    .map(|(s, _)| model::Origin::Import(s)),
            )
            .collect();
        let shown = chosen_for(fl, a, b).map_or_else(|| "can".to_string(), |f| f.shown.clone());
        let mut o = Overlap {
            id: format!("overlap:{a}"),
            kind: "live",
            start_ms: a,
            end_ms: b,
            shown,
            shown_source: String::new(),
            default_source: "live".into(),
            default_reason: None,
            live: Counts::default(),
            imported: Counts::default(),
            sites,
            can: counts_of(&ld, &lc, &[slot(Source::Live)]),
            teslamate: counts_of(&d, &c, &[SHOWN]),
            sources: super::source_refs(&origins),
            version,
        };
        o.settle_words();
        items.push(o);
    }
    Ok(items)
}

/// A span both import sites cover, and which one it shows.
#[derive(Debug, Clone)]
struct Pair {
    start_ms: u64,
    end_ms: u64,
    shown: ImportSite,
    default: ImportSite,
    reason: &'static str,
}

/// Each import site's drives and charges of a span ([`span_counts`]).
fn site_counts(d: &Slots, c: &Slots) -> SiteCounts {
    let of = |site| counts_of(d, c, &[import_slot(site)]);
    SiteCounts { teslamate: of(ImportSite::Teslamate), teslafi: of(ImportSite::Teslafi) }
}

/// Spans (in start order) joined where they meet or are less than a minute apart.
fn joined(v: Vec<(u64, u64)>) -> Vec<(u64, u64)> {
    joined_within(v, 60_000)
}

/// A hole in one import's history shorter than this is taken as covered when two imports are matched: TeslaFi does not
/// poll for a while when it lets the car fall asleep, and a drive or charge in such a hole (seen by the other import)
/// must still be in the span both imports cover, or both copies of it would be shown.
const IMPORT_HOLE_MS: u64 = 2 * 3_600_000;

/// Spans (in start order) joined where they meet or are less than `gap` apart.
fn joined_within(mut v: Vec<(u64, u64)>, gap: u64) -> Vec<(u64, u64)> {
    v.sort_unstable();
    let mut out: Vec<(u64, u64)> = Vec::new();
    for (a, b) in v {
        match out.last_mut() {
            Some(last) if a <= last.1.saturating_add(gap) => last.1 = last.1.max(b),
            _ => out.push((a, b)),
        }
    }
    out
}

/// Where two lists of spans (each in order, apart) both are.
fn intersect(x: &[(u64, u64)], y: &[(u64, u64)]) -> Vec<(u64, u64)> {
    let (mut i, mut j, mut out) = (0, 0, Vec::new());
    while let (Some(a), Some(b)) = (x.get(i), y.get(j)) {
        let (lo, hi) = (a.0.max(b.0), a.1.min(b.1));
        if lo < hi {
            out.push((lo, hi));
        }
        if a.1 < b.1 {
            i += 1;
        } else {
            j += 1;
        }
    }
    out
}

/// `[a, b)`, at least a millisecond long: a row or state that ends where it starts (one poll) still covers its moment,
/// else no span of the other import could ever meet it, and its twin there would be shown beside it.
fn at_least_a_moment(a: u64, b: u64) -> (u64, u64) {
    (a, b.max(a.saturating_add(1)))
}

/// How many of the starts (in order) are in `[a, b)`.
fn starts_in(v: &[u64], a: u64, b: u64) -> u64 {
    (v.partition_point(|&t| t < b) - v.partition_point(|&t| t < a)) as u64
}

/// The positions an import site has in `[a, b)` (read only when two imports tie on drives and charges).
fn samples_in(e: &Engine, car: CarId, site: ImportSite, a: u64, b: u64) -> QResult<u64> {
    const DAY: u64 = 86_400_000;
    let stream = model::Stream::of_site(site);
    let mut n = 0u64;
    for d in e.db().days::<model::Sample>(car, stream)? {
        let (x, y) = (d.start_ms.max(a), d.start_ms.saturating_add(DAY).min(b));
        if x < y {
            // (their times alone: no field is decoded)
            n += e.db().read_fields::<model::Sample>(car, stream, x, y, &[])?.t_ms.len() as u64;
        }
    }
    Ok(n)
}

/// What one import site has: the spans its states, drives and charges cover, and the starts of its drives, its
/// charges and every row.
#[derive(Default)]
struct SiteRows {
    cover: Vec<(u64, u64)>,
    drives: Vec<u64>,
    charges: Vec<u64>,
    starts: Vec<u64>,
}

/// The spans where both import sites have history (only when a car has both): where both sites' states, drives and
/// charges cover the same time, widened over the drives and charges that reach into it (so each starts in the span it
/// is in), and only where each site has a row starting. Each shows the site `chosen` names for its start, else the
/// one with more drives and charges, then more positions, then TeslaMate's.
fn import_pairs(e: &Engine, car: CarId, chosen: &[Flip]) -> QResult<Vec<Pair>> {
    let both = e.db().entry(car)?.is_some_and(|x| {
        ImportSite::ALL.iter().all(|s| {
            let t = x.summary.tally.of_site(*s);
            t.drives + t.charges > 0
        })
    });
    if !both {
        return Ok(Vec::new());
    }
    let mut by: [SiteRows; ImportSite::ALL.len()] = Default::default();
    // every drive and charge of either site: (start, end)
    let mut reach: Vec<(u64, u64)> = Vec::new();
    let family = Some(Source::Teslamate);
    super::reports::overlapping::<StateRow>(e, car, 0, u64::MAX, family, false, |s| {
        if let Some(site) = s.span.source.site()
            && s.span.state != State::Unknown
            && let Some(r) = by.get_mut(site.index())
        {
            r.cover.push(at_least_a_moment(s.span.start_ms, s.span.end_ms));
            r.starts.push(s.span.start_ms);
        }
        Ok(true)
    })?;
    fn gather<T: db::Item>(
        e: &Engine,
        car: CarId,
        by: &mut [SiteRows; ImportSite::ALL.len()],
        reach: &mut Vec<(u64, u64)>,
        charge: bool,
    ) -> QResult<()> {
        super::reports::overlapping::<T>(e, car, 0, u64::MAX, Some(Source::Teslamate), false, |r| {
            if let Some(s) = r.source().site().and_then(|site| by.get_mut(site.index())) {
                // a row of one poll (TeslaFi's short drives and charges) ends where it starts: it still covers
                // its moment, so the same drive from the other import meets it
                let (a, b) = at_least_a_moment(r.start_ms(), r.end_ms());
                s.cover.push((a, b));
                s.starts.push(a);
                if charge {
                    s.charges.push(a)
                } else {
                    s.drives.push(a)
                }
                reach.push((a, b));
            }
            Ok(true)
        })
    }
    gather::<model::Drive>(e, car, &mut by, &mut reach, false)?;
    gather::<model::Charge>(e, car, &mut by, &mut reach, true)?;
    for r in &mut by {
        r.cover = joined_within(std::mem::take(&mut r.cover), IMPORT_HOLE_MS);
        r.drives.sort_unstable();
        r.charges.sort_unstable();
        r.starts.sort_unstable();
    }
    let [tm, tf] = &by;
    let core = intersect(&tm.cover, &tf.cover);
    if core.is_empty() {
        return Ok(Vec::new());
    }
    // widened over the drives and charges that meet a span
    let mut wide = core.clone();
    for &(a, b) in &reach {
        let mut i = core.partition_point(|s| s.1 <= a);
        while core.get(i).is_some_and(|s| s.0 < b.max(a.saturating_add(1))) {
            if let Some(w) = wide.get_mut(i) {
                w.0 = w.0.min(a);
                w.1 = w.1.max(b);
            }
            i += 1;
        }
    }
    let mut out = Vec::new();
    for (a, b) in joined(wide) {
        if by.iter().any(|r| starts_in(&r.starts, a, b) == 0) {
            continue;
        }
        let n = |r: &SiteRows| starts_in(&r.drives, a, b) + starts_in(&r.charges, a, b);
        let (default, reason) = match n(tm).cmp(&n(tf)) {
            std::cmp::Ordering::Greater => (ImportSite::Teslamate, "more_drives_and_charges"),
            std::cmp::Ordering::Less => (ImportSite::Teslafi, "more_drives_and_charges"),
            std::cmp::Ordering::Equal => {
                let (x, y) =
                    (samples_in(e, car, ImportSite::Teslamate, a, b)?, samples_in(e, car, ImportSite::Teslafi, a, b)?);
                match x.cmp(&y) {
                    std::cmp::Ordering::Greater => (ImportSite::Teslamate, "more_samples"),
                    std::cmp::Ordering::Less => (ImportSite::Teslafi, "more_samples"),
                    // the older format, which most owners came from
                    std::cmp::Ordering::Equal => (ImportSite::Teslamate, "tie"),
                }
            }
        };
        let shown = chosen_for(chosen, a, b).and_then(|f| site_word(&f.shown)).unwrap_or(default);
        out.push(Pair { start_ms: a, end_ms: b, shown, default, reason });
    }
    Ok(out)
}

/// The `imports` overlaps: [`import_pairs`] with each site's counts, the shown site's as the imported side (each real
/// drive once, as the lists show it), and the live side's, which still wins inside its own coverage.
fn pair_overlaps(e: &Engine, car: CarId, ch: &Choices, pairs: Vec<Pair>) -> QResult<Vec<Overlap>> {
    let spans: Vec<(u64, u64)> = pairs.iter().map(|p| (p.start_ms, p.end_ms)).collect();
    let imported = span_counts(e, car, Source::Teslamate, &spans, &Sites::default())?;
    let live = span_counts(e, car, Source::Live, &spans, &Sites::default())?;
    let mut items = Vec::new();
    for ((p, (d, c)), (ld, lc)) in pairs.into_iter().zip(imported).zip(live) {
        let word = |s: ImportSite| format!("import:{}", s.word());
        let shown = [import_slot(p.shown)];
        items.push(Overlap {
            id: format!("imports:{}", p.start_ms),
            kind: "imports",
            start_ms: p.start_ms,
            end_ms: p.end_ms,
            shown: "teslamate".into(),
            shown_source: word(p.shown),
            default_source: word(p.default),
            default_reason: Some(p.reason),
            live: counts_of(&ld, &lc, &[slot(Source::Live)]),
            imported: counts_of(&d, &c, &shown),
            sites: site_counts(&d, &c),
            can: counts_of(&ld, &lc, &[slot(Source::Live)]),
            teslamate: counts_of(&d, &c, &shown),
            sources: super::source_refs(&ImportSite::ALL.map(model::Origin::Import)),
            version: ch.changes + 1,
        });
    }
    Ok(items)
}

/// The CAN coverage where imported history exists (none when nothing was imported); kept while the car's data
/// stays the same.
fn imported_coverage(e: &Engine, car: CarId) -> QResult<std::sync::Arc<Vec<(u64, u64)>>> {
    e.memo(car, "imported-coverage", crate::memo::On::All, 0, || match imported_span(e, car)? {
        Some((lo, hi)) => with_straddlers(e, car, can_coverage(e, car, lo, hi)?),
        None => Ok(Vec::new()),
    })
}

/// The CAN coverage with the imported drives and charges that were going on when a span of it began (the first live
/// day, or live logging coming back after a gap: the import has the whole drive or charge, the live side the rest
/// of it from where its logging began). Such a row starts before the span, so by its start alone it would be shown
/// beside the live row of the same drive. Where the live side has a row of the same kind starting while the
/// imported one goes on, the time the imported row covers becomes a span of its own, cut off the front of the live
/// one: the two rows are then one overlap, the live one shown unless that span is flipped. With no live row there
/// the imported one stays as it is (nothing shows twice).
fn with_straddlers(e: &Engine, car: CarId, cov: Vec<(u64, u64)>) -> QResult<Vec<(u64, u64)>> {
    let (Some(first), Some(last)) = (cov.first().copied(), cov.last().copied()) else { return Ok(cov) };
    // per span, the reach of the imported rows that go on into its start
    let mut reach: Vec<Option<(u64, u64)>> = vec![None; cov.len()];
    fn gather<T: db::Item>(
        e: &Engine,
        car: CarId,
        cov: &[(u64, u64)],
        (from, to): (u64, u64),
        reach: &mut [Option<(u64, u64)>],
    ) -> QResult<()> {
        // (only rows that meet a span's start can be going on into one)
        super::reports::overlapping::<T>(e, car, from, to, Some(Source::Teslamate), false, |r| {
            let (a, b) = (r.start_ms(), r.end_ms());
            // the first span that starts after the row does; the row itself outside every span
            let i = cov.partition_point(|s| s.0 <= a);
            let outside = i.checked_sub(1).and_then(|k| cov.get(k)).is_none_or(|s| s.1 <= a);
            let Some(&(s, _)) = cov.get(i).filter(|s| outside && s.0 < b) else { return Ok(true) };
            // the live row of the same drive or charge: one that starts while the imported one goes on
            let mut twin = false;
            super::reports::overlapping::<T>(e, car, s, b, Some(Source::Live), false, |l| {
                twin = (s..b).contains(&l.start_ms());
                Ok(!twin)
            })?;
            if twin && let Some(slot) = reach.get_mut(i) {
                *slot = Some(slot.map_or((a, b), |(x, y)| (x.min(a), y.max(b))));
            }
            Ok(true)
        })
    }
    let range = (first.0, last.0.saturating_add(1));
    gather::<model::Drive>(e, car, &cov, range, &mut reach)?;
    gather::<model::Charge>(e, car, &cov, range, &mut reach)?;
    let mut out = Vec::with_capacity(cov.len() + 1);
    for ((s, end), r) in cov.into_iter().zip(reach) {
        match r {
            None => out.push((s, end)),
            Some((a, b)) => {
                let cut = b.min(end);
                out.push((a, cut));
                if cut < end {
                    out.push((cut, end));
                }
            }
        }
    }
    Ok(out)
}

/// The live spans flipped to show the imported side (in order, apart): a series shows the import there.
pub(crate) fn flipped_spans(e: &Engine, car: CarId) -> QResult<Vec<(u64, u64)>> {
    let fl = flips(e, car)?;
    if fl.is_empty() {
        return Ok(Vec::new());
    }
    Ok(imported_coverage(e, car)?
        .iter()
        .copied()
        .filter(|s| chosen_for(&fl, s.0, s.1).is_some_and(|f| f.shown == "teslamate"))
        .collect())
}

/// Every overlap of a car, both kinds.
fn all_overlaps(e: &Engine, car: CarId) -> QResult<Vec<Overlap>> {
    let ch = choices(e, car)?;
    let pairs = import_pairs(e, car, &ch.sites)?;
    let sites = Sites::of(&pairs);
    let mut items =
        overlaps_of(e, car, imported_coverage(e, car)?.as_slice(), &flips(e, car)?, &sites, ch.changes + 1)?;
    items.extend(pair_overlaps(e, car, &ch, pairs)?);
    Ok(items)
}

/// `/cars/{car}/overlaps`: spans where imported and CAN history both exist, and spans where both import sites do.
/// Only the span imported history reaches is read; each table streams past once or twice.
pub fn overlaps(e: &Engine, car: CarId, paging: &Paging) -> QResult<Scoped<List<Overlap>>> {
    let c = car_of(e, car)?;
    let items = all_overlaps(e, car)?;
    let page = paginate(items, |o| (o.start_ms as f64, o.id.clone()), true, &format!("overlaps|{car}"), paging, false)?;
    Ok(scoped(&c, page))
}

/// Flips an overlap (`PATCH /cars/{car}/overlaps` with `{ id, shown, version? }`), found by its id wherever it is in
/// the list: a `live` span to `live` or the imported side, an `imports` span to one site. A `version` sent that is
/// not the current one changes nothing.
pub fn set_overlap(
    e: &Engine,
    car: CarId,
    id: &str,
    shown: &str,
    version: Option<u64>,
) -> QResult<super::fleet::Patched<Overlap>> {
    use super::fleet::Patched;
    car_of(e, car)?;
    let none = || QueryError::NotFound("no_such_overlap");
    let (pair, start) = match (id.strip_prefix("overlap:"), id.strip_prefix("imports:")) {
        (Some(t), _) => (false, t.parse::<u64>().ok()),
        (_, Some(t)) => (true, t.parse::<u64>().ok()),
        _ => (false, None),
    };
    let start = start.ok_or_else(none)?;
    // the word first: of an `imports` span a site, of a `live` one a side
    let word: String = if pair {
        site_word(shown)
            .ok_or_else(|| QueryError::invalid("shown", "import:teslamate or import:teslafi (the site to show)"))?
            .word()
            .to_string()
    } else {
        shown_word(shown)
            .ok_or_else(|| QueryError::invalid("shown", "live or import:teslamate (old words: can, teslamate)"))?
            .to_string()
    };
    let ch = choices(e, car)?;
    let mut o = if pair {
        let pairs = import_pairs(e, car, &ch.sites)?;
        pair_overlaps(e, car, &ch, pairs)?.into_iter().find(|o| o.start_ms == start).ok_or_else(none)?
    } else {
        let span = imported_coverage(e, car)?.iter().copied().find(|s| s.0 == start).ok_or_else(none)?;
        let sites = Sites::new(e, car)?;
        overlaps_of(e, car, &[span], &flips(e, car)?, &sites, ch.changes + 1)?.into_iter().next().ok_or_else(none)?
    };
    {
        // read, checked and written back one change at a time
        let _record = crate::lock(&e.inner.car_record);
        let mut ch = choices(e, car)?;
        if let Some(v) = version
            && v != ch.changes + 1
        {
            return Ok(Patched::Stale { version: ch.changes + 1 });
        }
        let flip = Flip { start_ms: o.start_ms, end_ms: o.end_ms, shown: word.clone() };
        // a new choice replaces every earlier one made for this span, whatever its start was then
        if pair {
            ch.sites.retain(|f| !f.meets(o.start_ms, o.end_ms));
            ch.sites.push(flip);
        } else {
            let mut fl = flips(e, car)?;
            fl.retain(|f| !f.meets(o.start_ms, o.end_ms));
            if word == "teslamate" {
                fl.push(flip);
            }
            records::put(e.db(), NS, &format!("car/{car}/overlaps"), &fl)?;
        }
        ch.changes += 1;
        records::put(e.db(), NS, &format!("car/{car}/import-overlaps"), &ch)?;
        o.version = ch.changes + 1;
    }
    if pair {
        o.shown_source = format!("import:{word}");
        // the imported side is now the chosen site's rows
        let n = if word == ImportSite::Teslafi.word() { &o.sites.teslafi } else { &o.sites.teslamate };
        (o.imported, o.teslamate) = (n.clone(), n.clone());
    } else {
        o.shown = word;
        o.settle_words();
    }
    e.emit(crate::Event::Rows {
        car,
        kinds: vec!["drives", "charges", "states", "visits"],
        from_ms: o.start_ms,
        to_ms: o.end_ms,
    });
    Ok(Patched::Done(o))
}

/// One log of a car and what became of it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LogRow {
    pub name: String,
    pub key: String,
    pub start_ms: u64,
    /// `verified`, `unsigned`, `failed`, `could_not_sign`, `waiting`.
    pub result: String,
    pub reason: Option<String>,
    pub used: bool,
    /// Used on the owner's word although it failed only for being unsigned.
    pub override_use: bool,
    /// Why it has no signature: `no_sidecar`, `no_integrity`, `signed_false`, `could_not_sign` (or `unknown`, judged
    /// before the kind was kept); `None` for a log that is signed (well or badly) or not judged yet.
    pub unsigned_kind: Option<&'static str>,
    /// Its time (from its name) is before its car key's first signed log. Shown only: the name is the uploader's.
    pub before_first_signed: bool,
    pub arrived_ms: u64,
    /// `decoded`, `left_out` or `not_yet` (not worked on yet).
    pub decoded: &'static str,
    pub samples: Option<u32>,
    pub dropped: Option<u32>,
    pub buses: Vec<String>,
    pub data_from_ms: Option<u64>,
    pub data_to_ms: Option<u64>,
    pub decoded_ms: Option<u64>,
}

/// Makes the rows of one car's logs, reading each day's record once for the logs of that day in a row.
struct RowMaker<'a> {
    e: &'a Engine,
    car: CarId,
    day: Option<(u32, std::sync::Arc<records::DayRec>)>,
}

impl RowMaker<'_> {
    fn row(
        &mut self,
        start: u64,
        name: String,
        key: String,
        rec: &ingest::judge::Record,
        first_signed: Option<i64>,
    ) -> QResult<LogRow> {
        let d = records::day_of(start);
        if self.day.as_ref().is_none_or(|(x, _)| *x != d) {
            self.day = Some((d, records::day_shared(self.e.db(), self.car, d)?));
        }
        let entry = self.day.as_ref().and_then(|(_, r)| r.logs.get(&name));
        let (decoded, samples, dropped, buses, from, to) = match entry.map(|x| &x.outcome) {
            Some(Outcome::Decoded { facts, samples, samples_from_ms, samples_to_ms }) => {
                ("decoded", Some(*samples), Some(facts.dropped), facts.buses.clone(), *samples_from_ms, *samples_to_ms)
            }
            Some(Outcome::Hole { from_ms, to_ms, .. }) => {
                ("left_out", None, None, Vec::new(), Some(*from_ms), Some(*to_ms))
            }
            None => ("not_yet", None, None, Vec::new(), None, None),
        };
        let result = match rec.status {
            ingest::judge::Status::Verified => "verified",
            ingest::judge::Status::Failed => "failed",
            ingest::judge::Status::CouldNotSign => "could_not_sign",
            ingest::judge::Status::Unsigned => "unsigned",
            ingest::judge::Status::Waiting => "waiting",
        };
        let before_first_signed = first_signed.is_some_and(|f| i64::try_from(start).is_ok_and(|s| s < f));
        Ok(LogRow {
            used: rec.usable(),
            override_use: rec.override_use,
            unsigned_kind: rec.unsigned_kind(),
            before_first_signed,
            arrived_ms: rec.arrived_ms,
            result: result.into(),
            reason: rec.reason.clone(),
            decoded_ms: entry.map(|x| x.done_ms),
            name,
            key,
            start_ms: start,
            decoded,
            samples,
            dropped,
            buses,
            data_from_ms: from,
            data_to_ms: to,
        })
    }
}

/// A car's unsigned logs as the owner's choice sees them (`/cars/{car}/logs/unsigned`): its keys' counts added up,
/// and the newest [`HEALTH_LIST_MAX`] logs that failed only for being unsigned or are used on the owner's word.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UnsignedLogs {
    /// Logs that failed only for being unsigned and are not used: what an admin can choose to use.
    pub can_use: u64,
    /// Of those, the ones whose time (from their name) is before their key's first signed log.
    pub can_use_before_first_signed: u64,
    /// Of those, per kind (`no_sidecar`, `no_integrity`, `signed_false`, `could_not_sign`, `unknown`).
    pub kinds: std::collections::BTreeMap<&'static str, u64>,
    /// Logs used on the owner's word.
    pub in_use: u64,
    /// Logs that failed for anything else (a bad signature, another key's, a changed file...): never used.
    pub never: u64,
    /// Each key's first signed log (its time from its name), `null` before one.
    pub keys: Vec<KeyFirstSigned>,
    pub items: Vec<LogRow>,
    pub items_total: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct KeyFirstSigned {
    pub key: String,
    pub first_signed_ms: Option<i64>,
}

pub fn unsigned_logs(e: &Engine, car: CarId) -> QResult<Scoped<UnsignedLogs>> {
    let c = car_of(e, car)?;
    let rt = e.handle()?;
    let mut out = UnsignedLogs {
        can_use: 0,
        can_use_before_first_signed: 0,
        kinds: Default::default(),
        in_use: 0,
        never: 0,
        keys: Vec::new(),
        items: Vec::new(),
        items_total: 0,
    };
    let mut found = Vec::new();
    for key in &c.vehicle_keys {
        let s = rt
            .block_on(e.judge().unsigned_summary(key, HEALTH_LIST_MAX))
            .map_err(|x| QueryError::Engine(crate::Error::Io(x)))?;
        out.can_use += s.can_use;
        out.can_use_before_first_signed += s.can_use_before_first_signed;
        for (k, n) in &s.kinds {
            *out.kinds.entry(k).or_default() += n;
        }
        out.in_use += s.in_use;
        out.never += s.never;
        out.items_total += s.logs_total;
        out.keys.push(KeyFirstSigned { key: key.clone(), first_signed_ms: s.first_signed_ms });
        for (name, rec) in s.logs {
            if let Some(t) = records::name_start_ms(&name) {
                found.push((t, name, key.clone(), rec, s.first_signed_ms));
            }
        }
    }
    found.sort_by(|x, y| super::cmp_key(&(x.0 as f64, x.1.clone()), &(y.0 as f64, y.1.clone()), true));
    found.truncate(HEALTH_LIST_MAX);
    let mut rows = RowMaker { e, car, day: None };
    for (start, name, key, rec, first) in found {
        out.items.push(rows.row(start, name, key, &rec, first)?);
    }
    Ok(scoped(&c, out))
}

/// A car's logs (every CAN key of it), newest first, with their check results and what was decoded. Each key's trust
/// records are read in order up to the cursor (or the period's end) keeping only a page and one more, and only the
/// page's day records are read: a page holds `limit + 1` rows whatever the history's length.
pub fn logs(e: &Engine, car: CarId, period: &Period, paging: &Paging) -> QResult<Scoped<List<LogRow>>> {
    let c = car_of(e, car)?;
    let rt = e.handle()?;
    let (a, b) = period.bounds()?;
    let limit = paging.limit()?;
    let sig = format!("logs|{car}|{}", period.sig());
    let after = super::read_after(paging, &sig)?;
    let mut found: Vec<(u64, String, String, ingest::judge::Record)> = Vec::new();
    for key in &c.vehicle_keys {
        let (cur, cur_start) = (after.clone(), after.as_ref().map(|x| x.0));
        let keep = move |name: &str, _: &ingest::judge::Record| {
            let Some(s) = records::name_start_ms(name) else { return false };
            s >= a
                && s < b
                && cur.as_ref().is_none_or(|at| {
                    super::cmp_key(&(s as f64, name.to_string()), at, true) == std::cmp::Ordering::Greater
                })
        };
        let stop = move |name: &str| {
            records::name_start_ms(name).is_some_and(|s| s >= b || cur_start.is_some_and(|at| s as f64 > at))
        };
        let got = rt
            .block_on(e.judge().newest(key, limit + 1, keep, stop))
            .map_err(|x| QueryError::Engine(crate::Error::Io(x)))?;
        for (name, rec) in got {
            if let Some(s) = records::name_start_ms(&name) {
                found.push((s, name, key.clone(), rec));
            }
        }
    }
    found.sort_by(|x, y| super::cmp_key(&(x.0 as f64, x.1.clone()), &(y.0 as f64, y.1.clone()), true));
    let more = found.len() > limit;
    found.truncate(limit);
    let next = match found.last() {
        Some(last) if more => Some(super::make_after(&(last.0 as f64, last.1.clone()), &sig)?),
        _ => None,
    };
    let mut firsts = std::collections::HashMap::new();
    for key in &c.vehicle_keys {
        let f = rt.block_on(e.judge().first_signed_ms(key)).map_err(|x| QueryError::Engine(crate::Error::Io(x)))?;
        firsts.insert(key.clone(), f);
    }
    let mut rows = RowMaker { e, car, day: None };
    let mut items = Vec::with_capacity(found.len());
    for (start, name, key, rec) in found {
        let first = firsts.get(&key).copied().flatten();
        items.push(rows.row(start, name, key, &rec, first)?);
    }
    Ok(scoped(&c, List { items, next, total: None }))
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LastTrust {
    pub result: String,
    pub reason: Option<String>,
    pub log_start_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct KeyHealth {
    pub key: String,
    /// The bridge's last contact: its last request the upload server accepted (a ping, a status question or an
    /// upload), else (none known) when its newest log arrived.
    pub last_upload_ms: Option<u64>,
    /// When its newest log arrived whole (null when not known).
    pub newest_log_ms: Option<u64>,
    pub preferred: bool,
    /// The name an admin gave the key, and what pages call it (that name, else an automatic label).
    pub device: Option<String>,
    pub device_label: String,
    pub logs_waiting: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Span {
    pub start_ms: u64,
    pub end_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UnknownSpan {
    pub start_ms: u64,
    pub end_ms: Option<u64>,
    pub reason: &'static str,
}

/// The check result of the car's newest judged log (logs still waiting for their sidecar are not judged yet).
fn last_judged(e: &Engine, car: CarId, meta: &records::CarMeta) -> QResult<Option<LastTrust>> {
    let db = e.db();
    for &d in meta.days.iter().rev() {
        let rec = records::day_shared(db, car, d)?;
        let newest = rec
            .logs
            .iter()
            .filter(|(_, en)| en.status != "waiting")
            .filter_map(|(n, en)| records::name_start_ms(n).map(|t| (t, n, en)))
            .max_by(|x, y| (x.0, x.1).cmp(&(y.0, y.1)));
        if let Some((t, _, en)) = newest {
            return Ok(Some(LastTrust { result: en.status.clone(), reason: en.reason.clone(), log_start_ms: Some(t) }));
        }
    }
    Ok(None)
}

/// Each list of `/cars/{car}/health` holds at most this many rows (the newest); `*_total` counts them all.
pub const HEALTH_LIST_MAX: usize = 500;

/// Keeps the newest `HEALTH_LIST_MAX` of a list fed oldest first (roughly: sorted at the end), counting them all.
struct Newest<T> {
    rows: std::collections::VecDeque<T>,
    total: u64,
}

impl<T> Newest<T> {
    fn new() -> Newest<T> {
        Newest { rows: std::collections::VecDeque::new(), total: 0 }
    }

    fn push(&mut self, t: T) {
        self.total += 1;
        if self.rows.len() == HEALTH_LIST_MAX {
            self.rows.pop_front();
        }
        self.rows.push_back(t);
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Health {
    pub healthy: bool,
    pub reasons: Vec<&'static str>,
    /// The last contact of the car's CAN Bridge, the newest of its keys' (see [`KeyHealth::last_upload_ms`]).
    pub last_upload_ms: Option<u64>,
    /// When the car's newest log arrived whole, the newest of its keys'.
    pub newest_log_ms: Option<u64>,
    pub last_log_trust: Option<LastTrust>,
    pub keys: Vec<KeyHealth>,
    /// At most [`HEALTH_LIST_MAX`] (the newest) in each list; the `*_total` counts are of the whole period.
    pub chain_gaps: Vec<serde_json::Value>,
    pub chain_gaps_total: u64,
    pub dropped_frames: Vec<Span>,
    pub dropped_frames_total: u64,
    pub unknown_spans: Vec<UnknownSpan>,
    pub unknown_spans_total: u64,
    pub unknown_ms: u64,
    pub logs_waiting: u64,
    /// The logs the server should have but no longer does (see `ingest::missing`), over all the car's keys.
    pub missing_logs: MissingLogs,
    /// Every log of the car's keys as the CAN Bridge app is told it now (`check` and `check_reason` of the upload
    /// API's status call), counted, with the runs of failed logs: what `sentry-mate log-checks` prints.
    pub log_checks: ingest::judge::LogChecks,
    pub rebuild_pending: bool,
    /// The newest stretch where one input's GPS fixes disagreed with another's (`gps_inputs_disagree`): which
    /// inputs, and whose fixes were left out of positions there.
    pub gps_disagreement: Option<crate::GpsDisagreement>,
    /// The web server fills this from the upload settings.
    pub uploads_enabled: Option<bool>,
}

/// The "Missing on the server" figures of a car: logs (and sidecars) its keys' records or chains say were here and
/// are no longer, from the last scan.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct MissingLogs {
    /// Still asked of the car (a ping's `wanted`).
    pub count: u64,
    /// The start (from its name) of the oldest of those.
    pub oldest_ms: Option<u64>,
    /// When the first of those was found missing.
    pub found_ms: Option<u64>,
    /// Whether the car has been asked to send them again, and when it first was.
    pub asked: bool,
    pub asked_ms: Option<u64>,
    /// The car no longer has these either: lost for good (their time shows as unknown).
    pub gone: u64,
    pub gone_oldest_ms: Option<u64>,
    /// When the server last looked (null before its first look).
    pub scanned_ms: Option<u64>,
}

fn earliest(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

impl MissingLogs {
    fn add(&mut self, c: &ingest::missing::Counts) {
        self.count += c.wanted;
        self.gone += c.gone;
        self.oldest_ms = earliest(self.oldest_ms, c.oldest_ms);
        self.gone_oldest_ms = earliest(self.gone_oldest_ms, c.gone_oldest_ms);
        self.found_ms = earliest(self.found_ms, c.found_ms);
        self.asked_ms = earliest(self.asked_ms, c.asked_ms);
        self.asked = self.asked_ms.is_some();
        self.scanned_ms = self.scanned_ms.max(c.scanned_ms);
    }
}

/// The missing-logs figures of a car, over its keys.
pub fn missing_logs(e: &Engine, car: CarId) -> QResult<MissingLogs> {
    let c = car_of(e, car)?;
    let rt = e.handle()?;
    let mut m = MissingLogs::default();
    for key in &c.vehicle_keys {
        if let Ok(book) = rt.block_on(e.judge().missing(key)) {
            m.add(&book.counts());
        }
    }
    Ok(m)
}

/// Every log of `keys` as the CAN Bridge app is told it now, counted over the keys (one pass over each key's records).
pub fn log_checks(e: &Engine, keys: &[String]) -> QResult<ingest::judge::LogChecks> {
    let rt = e.handle()?;
    let mut all = ingest::judge::LogChecks::default();
    for key in keys {
        if !e.judge().known(key) {
            continue;
        }
        let uploading = rt.block_on(e.store().uploading(key));
        let c =
            rt.block_on(e.judge().log_checks(key, uploading)).map_err(|x| QueryError::Engine(crate::Error::Io(x)))?;
        all.merge(c);
    }
    Ok(all)
}

/// Whether nobody expects an upload from a car that never sent one: it has no CAN Bridge key and no upload token
/// (so nothing can upload for it), and its history comes from elsewhere (an import, or an API this server reads).
/// Such a car is not one whose uploads stopped: `upload_silence` is not said of it. A car with a key or a token that
/// never uploaded, and a car with nothing at all, are still silent.
pub(crate) fn no_upload_expected(e: &Engine, c: &model::Car, meta: &records::CarMeta) -> QResult<bool> {
    if meta.newest_ms.is_some() || meta.last_arrived_ms.is_some() || !c.vehicle_keys.is_empty() {
        return Ok(false);
    }
    let fed = !(meta.api_days.is_empty() && meta.teslafi_days.is_empty() && meta.tmlocal_days.is_empty())
        || imported_span(e, c.id)?.is_some();
    Ok(fed && !super::keys::has_live_token(e, c.id)?)
}

/// What the Data health verdict rests on: the car's keys and their uploads, the newest judged log, and the reasons
/// it is not healthy (none: healthy).
struct Verdict {
    keys: Vec<KeyHealth>,
    waiting: u64,
    last_upload: Option<u64>,
    newest_log: Option<u64>,
    last_trust: Option<LastTrust>,
    rebuild_pending: bool,
    missing: MissingLogs,
    reasons: Vec<&'static str>,
}

fn verdict(e: &Engine, c: &model::Car, meta: &records::CarMeta, now: u64) -> QResult<Verdict> {
    let car = c.id;
    let rt = e.handle()?;
    let mut keys = Vec::new();
    let mut waiting = 0u64;
    let mut last_upload: Option<u64> = None;
    // the engine's own figure lags while it works through a backlog, but never says a log came later than it did
    let mut newest_log: Option<u64> = meta.last_arrived_ms;
    let mut missing = MissingLogs::default();
    let mut names = e.key_names()?;
    let contacts = e.contacts();
    for key in &c.vehicle_keys {
        if let Ok(book) = rt.block_on(e.judge().missing(key)) {
            missing.add(&book.counts());
        }
        let name = names.remove(key);
        let s = rt.block_on(e.judge().summary(key));
        let w = s.as_ref().map_or(0, |s| s.waiting as u64);
        waiting += w;
        // the bridge's last request, and when its newest log arrived (not the time in the log's name nor when the
        // server worked on it): the judge's records (all of them read) only when the upload server knows neither
        let contact = contacts.and_then(|x| x.key(key)).map(|x| x.at_ms);
        let log = match (contact, contacts.and_then(|x| x.newest_log(key))) {
            (_, Some(t)) => Some(t),
            (Some(_), None) => None,
            (None, None) => {
                rt.block_on(e.judge().last_arrived(key)).map_err(|x| QueryError::Engine(crate::Error::Io(x)))?
            }
        };
        let lu = contact.max(log);
        last_upload = last_upload.max(lu);
        newest_log = newest_log.max(log);
        keys.push(KeyHealth {
            key: key.clone(),
            last_upload_ms: lu,
            newest_log_ms: log,
            preferred: c.preferred_key.as_deref() == Some(key),
            device_label: super::keys::device_label(key, name.as_deref()),
            device: name,
            logs_waiting: w,
        });
    }
    let last_trust = last_judged(e, car, meta)?;
    let mut reasons = Vec::new();
    let last_seen = meta.newest_ms.or(newest_log);
    let silent = match last_seen {
        Some(t) => now.saturating_sub(t) > 24 * super::now::STALE_MS,
        None => !no_upload_expected(e, c, meta)?,
    };
    if silent {
        reasons.push("upload_silence");
    }
    if last_trust.as_ref().is_some_and(|t| t.result == "failed") {
        reasons.push("trust_failed");
    }
    if waiting > 0 {
        reasons.push("logs_waiting_trust");
    }
    let rebuild_pending = e.rebuild_pending(car);
    if rebuild_pending {
        reasons.push("rebuild_pending");
    }
    // logs the server lost and has asked the car for (those the car no longer has either are only shown)
    if missing.count > 0 {
        reasons.push("logs_missing");
    }
    // an input's GPS out of step with the others' in the last 30 days of the car's history
    let recent = meta.newest_ms.unwrap_or(now).saturating_sub(30 * 86_400_000);
    if e.gps_disagreement(car).is_some_and(|g| g.to_ms >= recent) {
        reasons.push("gps_inputs_disagree");
    }
    Ok(Verdict { keys, waiting, last_upload, newest_log, last_trust, rebuild_pending, missing, reasons })
}

/// Whether the car's data is healthy by the Data health rule (`healthy` of `/cars/{car}/health`): uploads within a
/// day, the newest judged log trusted, no logs waiting for trust and no rebuild pending.
pub fn healthy(e: &Engine, car: CarId) -> QResult<bool> {
    let c = car_of(e, car)?;
    let meta = records::meta(e.db(), car)?;
    Ok(verdict(e, &c, &meta, now_ms())?.reasons.is_empty())
}

/// `/cars/{car}/health` over a period (default the last 30 days). The day records stream past one day at a time, the
/// state spans a page at a time, and each key's chain is read once.
pub fn health(e: &Engine, car: CarId, period: &Period) -> QResult<Scoped<Health>> {
    use super::timeline::Why;
    let c = car_of(e, car)?;
    let now = now_ms();
    let db = e.db();
    let meta = records::meta(db, car)?;
    // the last 30 days of the car's data (a car that stopped uploading still shows its last month)
    let end = meta.newest_ms.map_or(now, |n| n.saturating_add(1).min(now));
    let (a, b) = period.window(30 * 86_400_000, end)?;
    let Verdict { keys, waiting, last_upload, newest_log, last_trust, rebuild_pending, missing, reasons } =
        verdict(e, &c, &meta, now)?;
    let mut why: Vec<Why> = Vec::new();
    let mut chain_gaps = Newest::new();
    for (x, y) in super::timeline::chain_gaps(e, car)? {
        if y > a && x < b {
            chain_gaps.push(serde_json::json!({ "from_ms": x, "to_ms": y }));
            why.push(Why { from_ms: x, to_ms: y, reason: "no_log", cause: "chain_gap" });
        }
    }
    // the day records of the period, one at a time (a log's data spills a day either side of its start's day)
    let mut dropped = Newest::new();
    let (lo, hi) = (records::day_of(a).saturating_sub(1), records::day_of(b.saturating_sub(1).max(a)));
    for &d in meta.days.range(lo..=hi) {
        let mut day: Vec<(u64, String, records::LogEntry)> = records::day(db, car, d)?
            .logs
            .into_iter()
            .filter_map(|(n, en)| {
                let (x, y) = en.span();
                (y >= a && x < b).then_some((x, n, en))
            })
            .collect();
        day.sort_by(|p, q| (p.0, &p.1).cmp(&(q.0, &q.1)));
        for (_, _, en) in day {
            match &en.outcome {
                Outcome::Hole { from_ms, to_ms, reason } => {
                    let (reason, cause) = super::timeline::hole_why(reason);
                    why.push(Why { from_ms: *from_ms, to_ms: *to_ms, reason, cause });
                }
                Outcome::Decoded { facts, .. } => {
                    for n in &facts.not_recorded {
                        let from = n.paused_ms.map_or(n.at_ms.saturating_sub(1), |p| n.at_ms.saturating_sub(p));
                        why.push(Why { from_ms: from, to_ms: n.at_ms, reason: "not_recorded", cause: "not_recorded" });
                    }
                    if facts.dropped > 0 {
                        let (x, y) = facts.span();
                        dropped.push(Span { start_ms: x, end_ms: Some(y) });
                        why.push(Why { from_ms: x, to_ms: y, reason: "dropped_frames", cause: "unknown" });
                    }
                }
            }
            // (a log that failed earlier shows as unknown time; only the newest judged log raises trust_failed)
        }
    }
    let mut unknown = Newest::new();
    let mut unknown_ms = 0u64;
    if a < b {
        super::timeline::each_span(e, car, a, Some(Source::Live), |s| {
            if s.start_ms >= b {
                return Ok(false);
            }
            let (x, y) = (s.start_ms.max(a), s.end_ms.min(b));
            if s.state == State::Unknown && y > x {
                let (reason, _) = super::timeline::classify(&why, x, y);
                unknown_ms += y - x;
                unknown.push(UnknownSpan { start_ms: x, end_ms: Some(y), reason });
            }
            Ok(true)
        })?;
    }
    let mut dropped_rows: Vec<Span> = dropped.rows.into_iter().collect();
    dropped_rows.sort_by_key(|s| s.start_ms);
    Ok(scoped(
        &c,
        Health {
            healthy: reasons.is_empty(),
            reasons,
            last_upload_ms: last_upload,
            newest_log_ms: newest_log,
            last_log_trust: last_trust,
            keys,
            chain_gaps_total: chain_gaps.total,
            chain_gaps: chain_gaps.rows.into_iter().collect(),
            dropped_frames_total: dropped.total,
            dropped_frames: dropped_rows,
            unknown_spans_total: unknown.total,
            unknown_spans: unknown.rows.into_iter().collect(),
            unknown_ms,
            logs_waiting: waiting,
            missing_logs: missing,
            log_checks: log_checks(e, &c.vehicle_keys)?,
            rebuild_pending,
            gps_disagreement: e.gps_disagreement(car),
            uploads_enabled: None,
        },
    ))
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Kind {
    pub kind: &'static str,
    pub label: &'static str,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StorageDay {
    pub series: &'static str,
    /// The old word (`can`, `tesla_api`, `teslamate`), kept for one release.
    pub source: &'static str,
    pub sources: Vec<super::SourceRef>,
    pub date: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Storage {
    pub kinds: Vec<Kind>,
    pub total_bytes: u64,
    pub items: Vec<StorageDay>,
    pub next: Option<String>,
}

/// Bytes of the files right in `dir` (not its folders); 0 when it can't be read.
fn files_bytes(dir: &Path) -> u64 {
    let Ok(rd) = std::fs::read_dir(dir) else { return 0 };
    rd.flatten().filter_map(|f| f.metadata().ok()).filter(|m| m.is_file()).map(|m| m.len()).sum()
}

/// Bytes of everything under `dir` (folders followed to `depth` levels; links never followed).
pub fn tree_bytes(dir: &Path, depth: u32) -> u64 {
    let Ok(rd) = std::fs::read_dir(dir) else { return 0 };
    let mut n = 0u64;
    for f in rd.flatten() {
        let Ok(t) = f.file_type() else { continue };
        if t.is_file() {
            n += f.metadata().map_or(0, |m| m.len());
        } else if t.is_dir() && depth > 0 {
            n += tree_bytes(&f.path(), depth - 1);
        }
    }
    n
}

/// The data folder (the log store's parent: `<data>/can`).
fn data_dir(e: &Engine) -> Option<&Path> {
    e.store().dir().parent()
}

/// A car's stored CAN logs (sealed, with their sidecars) whose start falls in `[a, b)`, per day of their start in
/// `tz`: `(date, bytes)`, and the total of every log (the period apart). One look at each file's size in the car's key
/// folders; logs still arriving are not counted. A finished day packed into one file (see `ingest::store`) counts
/// whole for the UTC day it is of, when that day meets the period.
fn log_days(e: &Engine, c: &model::Car, tz: chrono_tz::Tz, a: u64, b: u64) -> (BTreeMap<String, u64>, u64) {
    let mut days: BTreeMap<String, u64> = BTreeMap::new();
    let mut total = 0u64;
    for key in &c.vehicle_keys {
        let Ok(rd) = std::fs::read_dir(e.store().dir().join(key)) else { continue };
        for f in rd.flatten() {
            let Ok(m) = f.metadata() else { continue };
            if !m.is_file() {
                continue;
            }
            total += m.len();
            let name = f.file_name();
            let name = name.to_string_lossy();
            let log = name.strip_suffix(".json").unwrap_or(&name);
            if let Some(t) = records::name_start_ms(log).filter(|t| *t >= a && *t < b) {
                *days.entry(crate::tz::date_str(t, tz)).or_default() += m.len();
            }
        }
        // finished days packed into one file each: the pack's bytes count for its (UTC) day
        for (day, bytes) in e.store().pack_sizes(key) {
            total += bytes;
            let start = chrono::NaiveDate::parse_from_str(&day, "%Y-%m-%d")
                .ok()
                .and_then(|d| d.and_hms_opt(0, 0, 0))
                .and_then(|d| u64::try_from(d.and_utc().timestamp_millis()).ok());
            if start.is_some_and(|t| t < b && t.saturating_add(86_400_000) > a) {
                *days.entry(day).or_default() += bytes;
            }
        }
    }
    (days, total)
}

/// The kinds a car's data is kept as: `logs` (the CAN logs as uploaded, sealed), `series` (1 Hz samples), `curves`
/// (charge curves) and `rows` (drives, charges, states, updates and the rest of the car's tables).
pub const STORAGE_KINDS: [&str; 4] = ["logs", "series", "curves", "rows"];

/// `/cars/{car}/storage`: the bytes per kind, and per day and kind (newest day first, only days with data; `kinds`
/// filters the days, the period too: a log by its start, a stored day when it meets the period). Imported days carry
/// `source: "teslamate"`. Logs are grouped by the car's own (local) day; series and curves are stored per UTC day,
/// so their rows carry that UTC date. The per-kind bytes are of the whole history.
pub fn storage(
    e: &Engine,
    car: CarId,
    kinds: Option<&str>,
    period: &Period,
    paging: &Paging,
) -> QResult<Scoped<Storage>> {
    let c = car_of(e, car)?;
    let (a, b) = period.bounds()?;
    let tz = crate::tz::zone(&c.time_zone);
    let meets = |d: &db::DayInfo| d.start_ms < b && d.start_ms.saturating_add(86_400_000) > a;
    let db = e.db();
    let want: Vec<&str> = match kinds {
        Some(k) => {
            let v: Vec<&str> = k.split(',').map(str::trim).filter(|s| !s.is_empty()).collect();
            if let Some(bad) = v.iter().find(|k| !STORAGE_KINDS.contains(k)) {
                return Err(QueryError::bad("kinds", &format!("{bad} is not one of logs, series, curves, rows")));
            }
            v
        }
        None => STORAGE_KINDS.to_vec(),
    };
    let mut items = Vec::new();
    let (mut series_b, mut curves_b) = (0u64, 0u64);
    for stream in model::Stream::ALL {
        let word = match stream {
            model::Stream::Smo => "can",
            model::Stream::TeslaApi => "tesla_api",
            model::Stream::TeslafiApi => "teslafi_api",
            model::Stream::TeslamateLocal => "teslamate_local",
            model::Stream::Teslamate => "teslamate",
            model::Stream::Teslafi => "teslafi",
        };
        let sources = || super::source_refs(&[stream.origin()]);
        for d in db.days::<model::Sample>(car, stream)? {
            series_b += d.bytes;
            if want.contains(&"series") && d.bytes > 0 && meets(&d) {
                items.push(StorageDay {
                    series: "series",
                    source: word,
                    sources: sources(),
                    date: d.date,
                    bytes: d.bytes,
                });
            }
        }
        for d in db.days::<model::ChargePoint>(car, stream)? {
            curves_b += d.bytes;
            if want.contains(&"curves") && d.bytes > 0 && meets(&d) {
                items.push(StorageDay {
                    series: "curves",
                    source: word,
                    sources: sources(),
                    date: d.date,
                    bytes: d.bytes,
                });
            }
        }
    }
    let (logs, logs_b) = log_days(e, &c, tz, a, b);
    if want.contains(&"logs") {
        for (date, bytes) in logs {
            items.push(StorageDay {
                series: "logs",
                source: "can",
                sources: super::source_refs(&[model::Origin::Smo]),
                date,
                bytes,
            });
        }
    }
    let rows_b = data_dir(e).map_or(0, |d| files_bytes(&d.join("db").join("cars").join(car.to_string())));
    let order = |s: &str| STORAGE_KINDS.iter().position(|k| *k == s).unwrap_or(9);
    let sig = format!("storage|{car}|{want:?}|{}", period.sig());
    let page = paginate(
        items,
        |d| (0.0, format!("{}|{}|{}", d.date, 9 - order(d.series), d.source)),
        true,
        &sig,
        paging,
        false,
    )?;
    let kinds = vec![
        Kind { kind: "logs", label: "CAN logs (sealed)", bytes: logs_b },
        Kind { kind: "series", label: "Time series", bytes: series_b },
        Kind { kind: "curves", label: "Charge curves", bytes: curves_b },
        Kind { kind: "rows", label: "Drives, charges, states", bytes: rows_b },
    ];
    let total = kinds.iter().map(|k| k.bytes).sum();
    Ok(scoped(&c, Storage { kinds, total_bytes: total, items: page.items, next: page.next }))
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Growth {
    pub date: String,
    pub bytes: u64,
}

/// `/system/storage`, but for the free space (the web server adds it).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ServerStorage {
    pub kinds: Vec<Kind>,
    pub total_bytes: u64,
    /// Bytes added on each of the last 30 full UTC days (CAN data of every car: logs, series, curves).
    pub growth: Vec<Growth>,
    pub growth_per_day_bytes: Option<u64>,
}

/// The server's stored data: every car's, imported history apart, the server's own files (accounts, settings,
/// places, the fleet list) and the offline map tiles. One pass over each car's days; the per-day sums are kept only for the last 31 days.
pub fn server_storage(e: &Engine, now_ms: u64) -> QResult<ServerStorage> {
    let db = e.db();
    let utc = crate::tz::zone("UTC");
    let today = crate::tz::date_str(now_ms, utc);
    let first = crate::tz::date_str(now_ms.saturating_sub(31 * 86_400_000), utc);
    let mut growth: BTreeMap<String, u64> = BTreeMap::new();
    let mut add = |date: &str, b: u64| {
        if date >= first.as_str() && date < today.as_str() {
            *growth.entry(date.to_string()).or_default() += b;
        }
    };
    let (mut logs, mut series, mut rows, mut imported) = (0u64, 0u64, 0u64, 0u64);
    let data = data_dir(e);
    for car in super::all_cars(e)? {
        let Some(c) = db.car(car)? else { continue };
        let (days, total) = log_days(e, &c, utc, 0, u64::MAX);
        logs += total;
        for (d, b) in &days {
            add(d, *b);
        }
        for d in db
            .days::<model::Sample>(car, Source::Live)?
            .into_iter()
            .chain(db.days::<model::ChargePoint>(car, Source::Live)?)
        {
            series += d.bytes;
            add(&d.date, d.bytes);
        }
        for stream in model::Stream::IMPORTED {
            for d in
                db.days::<model::Sample>(car, stream)?.into_iter().chain(db.days::<model::ChargePoint>(car, stream)?)
            {
                imported += d.bytes;
            }
        }
        rows += data.map_or(0, |d| files_bytes(&d.join("db").join("cars").join(car.to_string())));
    }
    // the uploaded TeslaMate backups an import keeps (sealed, up to 64 GB each): their own kind, not the server's
    let backups = data.map_or(0, |d| tree_bytes(&d.join("web").join("imports"), 3));
    // the server's own: the fleet list, places, settings, the key/value store, accounts, the audit log, the web side
    let own = data.map_or(0, |d| {
        files_bytes(&d.join("db"))
            + tree_bytes(&d.join("db").join("kv"), 4)
            + ["accounts", "audit", "web"].iter().map(|x| tree_bytes(&d.join(x), 4)).sum::<u64>()
    });
    let own = own.saturating_sub(backups);
    let kinds = vec![
        Kind { kind: "logs", label: "CAN logs (sealed)", bytes: logs },
        Kind { kind: "series", label: "Time series and charge curves", bytes: series },
        Kind { kind: "rows", label: "Drives, charges, states", bytes: rows },
        Kind { kind: "imported", label: "Imported from TeslaMate", bytes: imported },
        Kind { kind: "server", label: "Accounts, settings and places", bytes: own },
        Kind { kind: "import_uploads", label: "TeslaMate backups uploaded to import", bytes: backups },
        Kind { kind: "tiles", label: "Offline map tiles", bytes: data.map_or(0, |d| tree_bytes(&d.join("maps"), 3)) },
    ];
    let total = kinds.iter().map(|k| k.bytes).sum();
    let growth: Vec<Growth> = growth.into_iter().map(|(date, bytes)| Growth { date, bytes }).collect();
    let growth: Vec<Growth> = growth.into_iter().rev().take(30).collect::<Vec<_>>().into_iter().rev().collect();
    // a mean over the days of the window, those without new data too (from the first day listed: a younger server
    // has fewer), not over the days that had some: a car driven now and then would look full far too soon
    let span = |from: &str| {
        let d = |x: &str| chrono::NaiveDate::parse_from_str(x, "%Y-%m-%d").ok();
        match (d(from), d(&today)) {
            (Some(a), Some(b)) => (b - a).num_days().clamp(1, 30) as u64,
            _ => growth.len() as u64,
        }
    };
    let per_day = growth.first().map(|f| growth.iter().map(|g| g.bytes).sum::<u64>() / span(&f.date).max(1));
    Ok(ServerStorage { kinds, total_bytes: total, growth, growth_per_day_bytes: per_day })
}

/// Engine figures for `/system/health`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SystemHealth {
    pub engine: crate::EngineStats,
    pub db: serde_json::Value,
    pub versions: serde_json::Value,
    pub timings_ms: serde_json::Value,
}

pub fn system_health(e: &Engine) -> SystemHealth {
    let s = e.stats();
    let d = e.db().stats();
    let per = |total: u64, n: u64| if n == 0 { None } else { Some(total as f64 / n as f64) };
    SystemHealth {
        timings_ms: serde_json::json!({
            "decode_per_log": per(s.decode_ms_total, s.logs_decoded + s.logs_left_out),
            "rebuild_per_window": per(s.rebuild_ms_total, s.windows_rebuilt),
        }),
        versions: serde_json::json!({
            "db_format": db::FORMAT_VERSION,
            "timeline_rules": timeline::RULES_VERSION,
            "decode": records::DECODE_VERSION,
            "import_mapping": import::MAPPING_VERSION,
        }),
        db: serde_json::json!({
            "cars": d.cars, "fleet_bytes": d.fleet_bytes, "open_cars": d.open_cars, "open_bytes": d.open_bytes,
            "cars_opened": d.cars_opened, "tables_read": d.tables_read,
        }),
        engine: s,
    }
}
