//! `/charges` (one car or several), a charge, its curve, `/charging-stats`, its places and the DC curve.
//!
//! Every list here reads the db a page at a time ([`each`]) and keeps only what it answers: the page asked for (the
//! best `limit` rows after the cursor, whatever the sort), running totals, and fixed-size tallies. Nothing loads a
//! period's rows to sort them. What the imports fill into a live charge is worked out once and kept, in memory and
//! across restarts, while what it was made from stays the same ([`FillDone`]).

use super::now::kind_word;
use super::system::Shown;
use super::{
    Address, CarRef, CursorBody, Money, Paging, Period, QResult, QueryError, Scoped, car_of, money, paginate,
    place_ref, r, scoped, sig_hash,
};
use crate::records::{self, CostEntry};
use crate::{Engine, costs};
use base64::Engine as _;
use model::{Car, CarId, Charge, ChargeKind, ChargePoint, Drive, Sample, Source};
use serde::Serialize;
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap, VecDeque};
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ChargePlace {
    pub id: String,
    pub name: String,
    pub kind: &'static str,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Position {
    pub lat: f64,
    pub lon: f64,
    pub accuracy_m: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Cost {
    pub amount: f64,
    pub currency: String,
    /// `tariff`, `manual`, `free_supercharging`, `teslamate`.
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PricePerKwh {
    pub value: f64,
    /// `grid` (over the grid energy) or `pack` (over the energy into the pack).
    pub basis: &'static str,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ChargeFlags {
    pub in_progress: bool,
    pub gap_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ChargeView {
    pub id: String,
    pub car: CarRef,
    pub time_zone: String,
    pub start_ms: u64,
    pub end_ms: Option<u64>,
    pub charging_ms: u64,
    /// How long the cable was in: null until the plug-in span is decoded (the charging time is no stand-in for it).
    pub plugged_ms: Option<u64>,
    /// `ac`, `dc` or null.
    pub kind: Option<&'static str>,
    /// The fast charger's type, brand, class and billed energy: null while nothing about the charger is decoded.
    pub fast_charger: Option<serde_json::Value>,
    pub place: Option<ChargePlace>,
    pub address: Address,
    pub position: Option<Position>,
    pub odometer_km: Option<f64>,
    pub energy_added_kwh: Option<f64>,
    pub energy_grid_kwh: Option<f64>,
    pub dc_inlet_kwh: Option<f64>,
    /// Grid energy less energy added, and their ratio: null unless both are known over the same span.
    pub losses_kwh: Option<f64>,
    pub efficiency_pct: Option<f64>,
    /// Why `efficiency_pct` and `losses_kwh` are null although both energies are known (they were measured over
    /// different parts of the charge), as a sentence to show with them; else null.
    pub efficiency_note: Option<String>,
    pub cost: Option<Cost>,
    pub price_per_kwh: Option<PricePerKwh>,
    pub start_soc_pct: Option<f64>,
    pub end_soc_pct: Option<f64>,
    pub start_range_km: Option<f64>,
    pub end_range_km: Option<f64>,
    pub range_added_km: Option<f64>,
    pub range_per_hour_km: Option<f64>,
    pub power_max_kw: Option<f64>,
    pub power_avg_kw: Option<f64>,
    pub outside_avg_c: Option<f64>,
    pub battery_avg_c: Option<f64>,
    #[serde(flatten)]
    pub from: super::Sources,
    /// The values an import filled where the live row had none (field → the import): empty for a row's own values.
    pub value_sources: super::gapfill::ValueSources,
    pub has_gaps: bool,
    pub flags: ChargeFlags,
}

pub(crate) struct Ctx {
    pub places: std::sync::Arc<crate::placing::Places>,
    pub live: Option<u64>,
}

/// The charge still going: the newest CAN charge, when the car's newest snapshot says it is charging, that charge
/// ends within 5 minutes of it, and the snapshot is fresh (the data has not stopped: no upload for longer than
/// [`super::now::STALE_MS`] means the charge stopped where its data did, as `/cars` and `/gaps` say), and the
/// engine has not settled it (the car's data goes on past it).
pub(crate) fn ctx(e: &Engine, c: &Car) -> QResult<Ctx> {
    let now = crate::now_ms();
    let snap = e
        .db()
        .entry(c.id)?
        .and_then(|x| x.summary.now)
        .filter(|n| n.sample.charging == Some(true) && now.saturating_sub(n.at_ms) <= super::now::STALE_MS);
    let live = match snap {
        Some(n) => e
            .db()
            .list::<Charge>(c.id, &db::Query { source: Some(Source::Live), ..db::Query::default().limit(1) })?
            .items
            .into_iter()
            .next()
            .filter(|ch| ch.end_ms + 300_000 >= n.at_ms)
            .map(|ch| costs::ended(e, c.id, &ch).map(|done| (!done).then_some(ch.id)))
            .transpose()?
            .flatten(),
        None => None,
    };
    Ok(Ctx { places: e.places(), live })
}

/// The cost a charge shows: kept, else from the place's price now (a charge still going), else the imported one.
pub(crate) fn cost_of(e: &Engine, c: &Car, ch: &Charge) -> QResult<Option<CostEntry>> {
    cost_live(e, c, ch, false)
}

/// [`cost_of`], for a charge known to be still going or not: one still going follows its place's price (only a typed
/// cost holds), whatever cost was kept for it before.
fn cost_live(e: &Engine, c: &Car, ch: &Charge, live: bool) -> QResult<Option<CostEntry>> {
    let kept = records::cost(e.db(), c.id, ch.id, ch.start_ms)?;
    let typed = kept.as_ref().is_some_and(|k| k.source == "manual");
    // a Supercharger invoice matched to it wins over every price; only a typed cost holds over it
    if !typed && let Some(inv) = e.invoice_cost(c.id, ch)? {
        return Ok(Some(inv));
    }
    // free Supercharging (the car's settings) wins over a place price, kept or not; only a typed cost holds
    if ch.source == Source::Live
        && !typed
        && let Some(free) = costs::free_now(e, ch, &c.time_zone)
    {
        return Ok(Some(free));
    }
    if let Some(k) = kept.filter(|k| !live || k.source == "manual") {
        return Ok(Some(k));
    }
    if ch.source == Source::Live {
        // an ended charge that had no cost when a place change was saved keeps none until a reprice
        if !live && costs::kept_none(e, c.id, ch.id)? {
            return Ok(None);
        }
        return Ok(costs::price_now(e, ch, &[], &c.time_zone));
    }
    Ok(None)
}

pub(crate) fn view(e: &Engine, c: &Car, ch: &Charge, x: &Ctx) -> QResult<ChargeView> {
    let kept = cost_live(e, c, ch, x.live == Some(ch.id))?;
    Ok(view_with(c, ch, x, kept.as_ref()))
}

/// [`view`] with the values the chosen import fills where the live row has none ([`super::gapfill`]), named in
/// `value_sources` and `sources`. The cost stays the row's own (worked out from the charge as stored).
pub(crate) fn view_filled(
    e: &Engine,
    c: &Car,
    ch: &Charge,
    x: &Ctx,
    fill: &super::gapfill::Gaps,
) -> QResult<ChargeView> {
    let kept = cost_live(e, c, ch, x.live == Some(ch.id))?;
    filled_with(e, c, ch, x, kept.as_ref(), fill)
}

/// A live charge as the imports filled it, and which values they gave ([`super::gapfill::Gaps::charge`]).
type FilledCharge = Arc<(Charge, super::gapfill::Filled)>;

/// Live charges as the imports filled them (by a key of the charge as stored), kept in memory while the car's imported
/// data and what the filling rests on ([`super::gapfill::Gaps::key`]) stay the same: filling reads a day or two of
/// import samples per charge, and every list and report goes through every charge.
type FillKept = std::sync::Mutex<std::collections::HashMap<u64, FilledCharge>>;

/// Charges kept filled in memory per car at most; past it they are all made again.
const FILL_KEPT: usize = 20_000;

/// Where filled charges are kept across restarts: `car/{car}/charge/{id}` → [`FillDone`].
const FILL_NS: &str = "charge-fill";

/// How charges are filled, in [`FillDone::stamp`]: a change to the filling rules ([`timeline::gapfill`]) moves it
/// on, and every kept fill is made again.
const FILL_VERSION: u32 = 1;

/// The API words a filled charge value can have ([`super::gapfill::Filled`]): a kept fill naming another is made
/// again.
const FILL_WORDS: &[&str] = &[
    "position",
    "odometer_km",
    "start_soc_pct",
    "end_soc_pct",
    "start_range_km",
    "end_range_km",
    "outside_avg_c",
    "battery_avg_c",
];

/// A charge's fill kept across restarts, with a stamp of everything it was made from: the charge as stored, the
/// rules, which import shows where and whether filling is on, and the size of each import day it read.
#[derive(serde::Serialize, serde::Deserialize)]
struct FillDone {
    stamp: String,
    charge: Charge,
    filled: Vec<(String, model::ImportSite)>,
}

/// A charge with the values the chosen import fills, and which they are: kept in memory, else across restarts while
/// what it was made from stays the same ([`FillDone`]), else filled (and kept).
fn filled_charge(e: &Engine, car: CarId, ch: &Charge, fill: &super::gapfill::Gaps) -> QResult<FilledCharge> {
    // (only live charges are ever filled)
    if ch.source != Source::Live || !fill.is_on() {
        return fill_one(ch, fill);
    }
    let kept = fill_kept(e, car, fill)?;
    let key = crate::memo::key(ch);
    if let Some(v) = lock_kept(&kept).get(&key) {
        return Ok(v.clone());
    }
    let v = match fill_done(e, car, ch, fill)? {
        (Some(v), _) => v,
        (None, stamp) => make_fill(e, car, ch, fill, stamp)?,
    };
    keep_fill(&kept, key, v.clone());
    Ok(v)
}

fn fill_kept(e: &Engine, car: CarId, fill: &super::gapfill::Gaps) -> QResult<Arc<FillKept>> {
    e.memo(car, "charges-filled", crate::memo::On::Imported, fill.key(), || Ok::<_, QueryError>(FillKept::default()))
}

fn lock_kept(kept: &FillKept) -> std::sync::MutexGuard<'_, std::collections::HashMap<u64, FilledCharge>> {
    kept.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn keep_fill(kept: &FillKept, key: u64, v: FilledCharge) {
    let mut m = lock_kept(kept);
    if m.len() >= FILL_KEPT {
        m.clear();
    }
    m.insert(key, v);
}

/// A charge's fill kept across restarts, if what it was made from is the same; and the stamp of that.
fn fill_done(
    e: &Engine,
    car: CarId,
    ch: &Charge,
    fill: &super::gapfill::Gaps,
) -> QResult<(Option<FilledCharge>, String)> {
    let stamp = fill_stamp(e, car, ch, fill)?;
    // (one that cannot be read is made again)
    let done = records::kept::<FillDone>(e.db(), FILL_NS, &fill_db_key(car, ch)).ok().flatten();
    let done = done.filter(|d| d.stamp == stamp).and_then(|d| {
        let words: Option<Vec<_>> = d
            .filled
            .iter()
            .map(|(w, site)| FILL_WORDS.iter().find(|k| **k == w.as_str()).map(|k| (*k, *site)))
            .collect();
        Some(Arc::new((d.charge, super::gapfill::Filled(words?))))
    });
    Ok((done, stamp))
}

fn fill_db_key(car: CarId, ch: &Charge) -> String {
    format!("car/{car}/charge/{}", ch.id)
}

/// Fills a charge and keeps the fill across restarts under `stamp`.
fn make_fill(e: &Engine, car: CarId, ch: &Charge, fill: &super::gapfill::Gaps, stamp: String) -> QResult<FilledCharge> {
    let v = fill_one(ch, fill)?;
    let filled = v.1.0.iter().map(|(w, site)| ((*w).to_string(), *site)).collect();
    // (a fill that cannot be kept, as on a full disk, is made again next time)
    let _ = records::put(e.db(), FILL_NS, &fill_db_key(car, ch), &FillDone { stamp, charge: v.0.clone(), filled });
    Ok(v)
}

/// Makes ready the fills of the live charges of `car` starting in `[a, b)` that are not kept in memory: those kept
/// across restarts are taken, the rest are filled on as many threads as the machine has (each with its own days of
/// import samples, a stretch of neighbouring charges each), so that the pass over the rows that follows finds them
/// all kept.
pub(crate) fn prefill(e: &Engine, car: CarId, a: u64, b: u64, fill: &super::gapfill::Gaps) -> QResult<()> {
    if !fill.is_on() || a >= b {
        return Ok(());
    }
    let kept = fill_kept(e, car, fill)?;
    let mut todo: Vec<(u64, Charge, String)> = Vec::new();
    let mut q = db::Query::between(a, b).limit(db::MAX_PAGE).oldest_first();
    q.source = Some(Source::Live);
    loop {
        let page = e.db().list::<Charge>(car, &q)?;
        for ch in page.items {
            if ch.source != Source::Live || ch.start_ms < a || ch.start_ms >= b {
                continue;
            }
            let key = crate::memo::key(&ch);
            if lock_kept(&kept).contains_key(&key) {
                continue;
            }
            match fill_done(e, car, &ch, fill)? {
                (Some(v), _) => keep_fill(&kept, key, v),
                (None, stamp) => todo.push((key, ch, stamp)),
            }
        }
        match page.next {
            Some(c) => q.after = Some(c),
            None => break,
        }
    }
    super::parked::spread(&todo, |part| {
        let fill = super::gapfill::Gaps::new(e, car, None)?;
        for (key, ch, stamp) in part {
            keep_fill(&kept, *key, make_fill(e, car, ch, &fill, stamp.clone())?);
        }
        Ok(Vec::<()>::new())
    })?;
    Ok(())
}

fn fill_one(ch: &Charge, fill: &super::gapfill::Gaps) -> QResult<FilledCharge> {
    let mut filled = ch.clone();
    let got = fill.charge(&mut filled)?;
    Ok(Arc::new((filled, got)))
}

/// The stamp of a charge's fill ([`FillDone`]): the import days it may read are those around the charge, by the
/// fuse margin ([`super::gapfill::Gaps::charge`]).
fn fill_stamp(e: &Engine, car: CarId, ch: &Charge, fill: &super::gapfill::Gaps) -> QResult<String> {
    use sha2::{Digest, Sha256};
    const DAY: u64 = 86_400_000;
    let sizes = e.memo(car, "import-day-sizes", crate::memo::On::Imported, 0, || {
        let mut out = Vec::new();
        for site in model::ImportSite::ALL {
            let d: BTreeMap<u64, u64> = e
                .db()
                .days::<model::Sample>(car, model::Stream::of_site(site))?
                .into_iter()
                .map(|d| (d.start_ms, d.bytes))
                .collect();
            out.push((site, d));
        }
        Ok::<_, QueryError>(out)
    })?;
    let p = &e.inner.opts.fuse;
    let m = p.margin_ms();
    let (a, b) = (ch.start_ms.saturating_sub(m), ch.end_ms.saturating_add(m + 1));
    let read: Vec<(model::ImportSite, Vec<(u64, u64)>)> =
        sizes.iter().map(|(site, d)| (*site, d.range(a / DAY * DAY..b).map(|(s, n)| (*s, *n)).collect())).collect();
    let mut h = Sha256::new();
    h.update(FILL_VERSION.to_be_bytes());
    h.update(fill.key().to_be_bytes());
    h.update(serde_json::to_vec(&(ch, p, &read)).unwrap_or_default());
    Ok(h.finalize()[..16].iter().map(|b| format!("{b:02x}")).collect())
}

fn filled_with(
    e: &Engine,
    c: &Car,
    ch: &Charge,
    x: &Ctx,
    kept: Option<&CostEntry>,
    fill: &super::gapfill::Gaps,
) -> QResult<ChargeView> {
    let f = filled_charge(e, c.id, ch, fill)?;
    let (filled, got) = (&f.0, &f.1);
    let mut v = view_with(c, filled, x, kept);
    if !got.is_empty() {
        v.value_sources = got.value_sources();
        v.from = got.sources(&v.from);
    }
    Ok(v)
}

/// A charge's energy added and grid energy `(added, grid)` where the two may be compared (its efficiency and losses,
/// and the totals of charging efficiency over many charges): every page that shows an efficiency asks this, so a
/// charge is in all of them or in none. Only an AC charge (a DC charger's grid side is out of sight, and an unknown
/// kind may be DC), both energies known and above zero, and no more into the pack than came from the grid: more is two
/// energies that were not measured over the same span by the same input (or one in the wrong unit), never a real
/// charge, so it is left out rather than counted as an efficiency over 100 %. Nor a charge whose two energies cover
/// different spans of it ([`Charge::energy_spans`]: its `efficiency_note` says why).
pub(crate) fn compared_energies(ch: &Charge) -> Option<(f64, f64)> {
    if ch.kind != Some(ChargeKind::Ac) || !ch.energies_comparable() {
        return None;
    }
    let (added, grid) = (ch.energy_added_kwh?, ch.energy_used_kwh?);
    (added.is_finite() && grid.is_finite() && added > 0.0 && grid > 0.0 && added <= grid).then_some((added, grid))
}

fn view_with(c: &Car, ch: &Charge, x: &Ctx, kept: Option<&CostEntry>) -> ChargeView {
    let live = x.live == Some(ch.id);
    let ms = ch.end_ms.saturating_sub(ch.start_ms);
    let place = ch.position.as_ref().and_then(|p| x.places.place_at(p.lat, p.lon));
    // an imported charge's TeslaMate cost is kept (as an entry) only in the currency the import was started with:
    // without one, its amount alone could not be told apart from another currency's, so it shows no cost
    let cost = kept.map(|k| Cost { amount: k.amount, currency: k.currency.clone(), source: k.source.clone() });
    let grid = ch.energy_used_kwh;
    let added = ch.energy_added_kwh;
    let price_per_kwh = cost.as_ref().and_then(|cst| {
        // a typed cost is over the energy it was entered for; else the grid's where measured, else the pack's
        let typed_basis = kept.filter(|k| k.source == "manual").and_then(|k| k.energy_basis.as_deref());
        let (basis, kwh) = match grid.filter(|_| typed_basis != Some("pack")) {
            Some(g) => ("grid", g),
            None => ("pack", added?),
        };
        let value = match kept {
            Some(k) if k.mode.as_deref() == Some("per_kwh") => k.entered?,
            _ if kwh > 0.0 => (cst.amount / kwh * 10_000.0).round() / 10_000.0,
            _ => return None,
        };
        Some(PricePerKwh { value, basis })
    });
    let hours = ms as f64 / 3_600_000.0;
    // compared only when measured over the same span
    let range_added = ch.end_range_km.zip(ch.start_range_km).map(|(b, a)| b - a);
    ChargeView {
        id: ch.id.to_string(),
        car: CarRef(c.id),
        time_zone: c.time_zone.clone(),
        start_ms: ch.start_ms,
        end_ms: (!live).then_some(ch.end_ms),
        charging_ms: ms,
        plugged_ms: None,
        kind: ch.kind.map(kind_word),
        fast_charger: None,
        place: place.map(|p| ChargePlace { id: place_ref(p.id), name: p.name.clone(), kind: x.places.kind_word(p) }),
        address: Address { short: ch.address.clone(), ..Address::default() },
        position: ch.position.as_ref().map(|p| Position {
            lat: r(Some(p.lat), 6).unwrap_or(p.lat),
            lon: r(Some(p.lon), 6).unwrap_or(p.lon),
            accuracy_m: None,
        }),
        odometer_km: r(ch.position.as_ref().and_then(|p| p.odometer_km), 3),
        energy_added_kwh: r(added, 3),
        energy_grid_kwh: r(grid, 3),
        dc_inlet_kwh: None,
        losses_kwh: r(compared_energies(ch).map(|(a, g)| g - a), 3),
        efficiency_pct: r(compared_energies(ch).map(|(a, g)| a / g * 100.0), 1),
        efficiency_note: ch.energy_spans.filter(|_| grid.is_some() && added.is_some()).map(efficiency_note),
        cost,
        price_per_kwh,
        start_soc_pct: r(ch.start_soc, 1),
        end_soc_pct: r(ch.end_soc, 1),
        start_range_km: r(ch.start_range_km, 1),
        end_range_km: r(ch.end_range_km, 1),
        range_added_km: r(range_added, 1),
        range_per_hour_km: r(range_added.filter(|_| hours > 0.01).map(|ra| ra / hours), 1),
        power_max_kw: r(ch.power_max_kw, 1),
        power_avg_kw: r(added.filter(|_| hours > 0.001).map(|a| a / hours), 1),
        outside_avg_c: r(ch.outside_temp_avg_c, 1),
        battery_avg_c: r(ch.battery_temp_avg_c, 1),
        from: super::Sources::of(ch.source, ch.inputs),
        value_sources: Default::default(),
        has_gaps: false,
        flags: ChargeFlags { in_progress: live, gap_ms: 0 },
    }
}

/// Why a charge's energies are not compared ([`ChargeView::efficiency_note`]).
fn efficiency_note(s: model::EnergySpans) -> String {
    let min = |ms: u64| {
        let s = ms.div_ceil(1000);
        if s < 60 { format!("{s} s") } else { format!("{} min", (s + 30) / 60) }
    };
    format!(
        "Not measured over the same time: the energy added covers {} of this charge and the grid energy {} \
         ({} both), so no efficiency or losses are shown.",
        min(s.added_ms),
        min(s.grid_ms),
        min(s.both_ms)
    )
}

/// Calls `f` with each shown row of `car` whose start is in `[a, b)`, newest or oldest first, reading the db a page
/// at a time (nothing is held but that page); `f` answers whether to go on.
#[allow(clippy::too_many_arguments)]
fn each<T: db::Item>(
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

/// The charges of a car in a period (by start, oldest first), as shown.
pub(crate) fn rows(
    e: &Engine,
    car: CarId,
    period: &Period,
    source: Option<super::SourceFilter>,
) -> QResult<Vec<Charge>> {
    let (a, b) = period.bounds()?;
    let shown = Shown::new(e, car, source)?;
    let mut out = Vec::new();
    each::<Charge>(e, car, a, b, source, &shown, false, |c| {
        out.push(c);
        Ok(true)
    })?;
    Ok(out)
}

/// A stretch without data: `unknown` (no log, a log that failed its check, a pause in recording), `dropped` (lost
/// frames) or `chain` (a log missing from the bridge's chain).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Gap {
    pub start_ms: u64,
    pub end_ms: u64,
    pub kind: &'static str,
}

/// How long a car's chain gaps are kept: reading a key's chain looks at the link of every log it sent, too slow for
/// every page of a list. A log that comes late shows within this time.
const CHAIN_MEMO: std::time::Duration = std::time::Duration::from_secs(30);

/// The chain gaps found lately: (the db, the car, when it was made, its keys) → when read, the gaps.
type ChainKey = (usize, CarId, u64, Vec<String>);
type ChainMemoRow = (ChainKey, std::time::Instant, std::sync::Arc<Vec<(u64, u64)>>);
static CHAINS: std::sync::Mutex<Vec<ChainMemoRow>> = std::sync::Mutex::new(Vec::new());

/// The spans of a car's missing chain links ([`super::timeline::chain_gaps`]), read at most every [`CHAIN_MEMO`].
fn chain_gaps(e: &Engine, c: &Car) -> QResult<std::sync::Arc<Vec<(u64, u64)>>> {
    let key: ChainKey = (std::ptr::from_ref(e.db()) as usize, c.id, c.created_ms, c.vehicle_keys.clone());
    let fresh = |t: &std::time::Instant| t.elapsed() < CHAIN_MEMO;
    if let Ok(m) = CHAINS.lock()
        && let Some((_, _, gaps)) = m.iter().find(|(k, t, _)| *k == key && fresh(t))
    {
        return Ok(gaps.clone());
    }
    // reading the chain waits on the engine's runtime: from inside a runtime (a caller that did not move off the
    // async threads), on a thread of its own
    let read = || super::timeline::chain_gaps(e, c.id);
    let gaps = if tokio::runtime::Handle::try_current().is_ok() {
        std::thread::scope(|s| s.spawn(read).join())
            .map_err(|_| QueryError::Engine(crate::Error::Damaged("reading a key's chain failed".into())))??
    } else {
        read()?
    };
    let gaps = std::sync::Arc::new(gaps);
    if let Ok(mut m) = CHAINS.lock() {
        m.retain(|(k, t, _)| *k != key && fresh(t));
        m.truncate(255);
        m.push((key, std::time::Instant::now(), gaps.clone()));
    }
    Ok(gaps)
}

/// The stretches without data in `[a, b)`, by start: logs left out (failed their check, or never stored), pauses in
/// recording and lost frames of decoded logs (from the records), and gaps in each key's chain.
fn holes(e: &Engine, c: &Car, a: u64, b: u64) -> QResult<Vec<Gap>> {
    use crate::records::Outcome;
    if a >= b {
        return Ok(Vec::new());
    }
    let db = e.db();
    let meta = records::meta(db, c.id)?;
    let mut out = Vec::new();
    for (_, en) in records::entries_between(db, c.id, &meta, a, b)? {
        match &en.outcome {
            Outcome::Hole { from_ms, to_ms, .. } => {
                out.push(Gap { start_ms: *from_ms, end_ms: *to_ms, kind: "unknown" });
            }
            Outcome::Decoded { facts, .. } => {
                for n in &facts.not_recorded {
                    let from = n.paused_ms.map_or(n.at_ms.saturating_sub(1), |p| n.at_ms.saturating_sub(p));
                    out.push(Gap { start_ms: from, end_ms: n.at_ms, kind: "unknown" });
                }
                if facts.dropped > 0 {
                    let (x, y) = facts.span();
                    out.push(Gap { start_ms: x, end_ms: y, kind: "dropped" });
                }
            }
        }
    }
    for &(x, y) in chain_gaps(e, c)?.iter() {
        if y > a && x < b {
            out.push(Gap { start_ms: x, end_ms: y, kind: "chain" });
        }
    }
    out.retain(|g| g.end_ms > g.start_ms && g.end_ms > a && g.start_ms < b);
    out.sort_by_key(|g| (g.start_ms, g.end_ms));
    Ok(out)
}

/// The holes inside `[a, b)`, cut at its edges.
fn holes_in(holes: &[Gap], a: u64, b: u64) -> Vec<Gap> {
    holes
        .iter()
        .filter(|g| g.start_ms < b && g.end_ms > a)
        .map(|g| Gap { start_ms: g.start_ms.max(a), end_ms: g.end_ms.min(b), kind: g.kind })
        .collect()
}

/// How long the gaps cover (overlaps counted once).
fn covered_ms(gaps: &[Gap]) -> u64 {
    let mut spans: Vec<(u64, u64)> = gaps.iter().map(|g| (g.start_ms, g.end_ms)).collect();
    spans.sort_unstable();
    let (mut total, mut until) = (0u64, 0u64);
    for (a, b) in spans {
        let a = a.max(until);
        if b > a {
            total += b - a;
            until = b;
        }
    }
    total
}

/// `/charges` filters.
#[derive(Debug, Clone, Default)]
pub struct ChargesQuery {
    pub period: Period,
    pub places: Option<String>,
    /// `home`, `work`, `charger`, `service`, `other`, `none` (at no saved place).
    pub place_kind: Option<String>,
    /// `ac`, `dc`, `unknown`.
    pub kind: Option<String>,
    pub q: Option<String>,
    pub min_cost: Option<f64>,
    pub include_unpriced: Option<bool>,
    pub min_duration_min: Option<f64>,
    pub source: Option<String>,
    pub flags: Option<String>,
    /// `start` (default), `energy`, `duration`, `cost`, `power_max`, `end_soc`.
    pub sort: Option<String>,
    pub dir: Option<String>,
}

const SORTS: &[&str] = &["start", "energy", "duration", "cost", "power_max", "end_soc"];
const PLACE_KINDS: &[&str] = &["home", "work", "charger", "service", "other", "none"];

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Totals {
    pub count: u64,
    pub energy_added_kwh: f64,
    pub energy_grid_kwh: f64,
    pub energy_grid_known: u64,
    pub dc_inlet_kwh: Option<f64>,
    pub cost_by_currency: Vec<Money>,
    pub unpriced: u64,
    pub avg_duration_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ChargesPage {
    /// One car's list (`/cars/{car}/charges`): the car and its time zone.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub car: Option<CarRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_zone: Option<String>,
    /// Several cars' (`/charges`): the cars.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cars: Option<Vec<CarRef>>,
    pub items: Vec<ChargeView>,
    pub next: Option<String>,
    pub totals: Totals,
}

/// The checked filters of a query.
struct Filter<'a> {
    q: &'a ChargesQuery,
    source: Option<super::SourceFilter>,
    places: Option<Vec<u32>>,
    needle: Option<String>,
    flags: Vec<&'a str>,
    /// Whether every row is marked with the gaps inside it while the rows stream by (`flags=gap` needs it to choose
    /// rows; without it a list marks only its page's rows, [`mark_gaps`], and the stats none).
    gaps: bool,
}

impl<'a> Filter<'a> {
    fn new(e: &Engine, q: &'a ChargesQuery) -> QResult<Filter<'a>> {
        let source = super::parse_source(q.source.as_deref())?;
        if let Some(k) = q.kind.as_deref().filter(|k| !k.is_empty())
            && k.split(',').any(|w| !["ac", "dc", "unknown"].contains(&w))
        {
            return Err(QueryError::bad("kind", "ac, dc or unknown"));
        }
        if let Some(k) = q.place_kind.as_deref().filter(|k| !k.is_empty())
            && !PLACE_KINDS.contains(&k)
        {
            return Err(QueryError::bad("place_kind", "home, work, charger, service, other or none"));
        }
        let flags: Vec<&str> =
            q.flags.as_deref().map_or_else(Vec::new, |f| f.split(',').filter(|s| !s.is_empty()).collect());
        if flags.iter().any(|f| !["gap", "in_progress"].contains(f)) {
            return Err(QueryError::bad("flags", "gap or in_progress"));
        }
        let places = super::drives::place_list(q.places.as_deref(), &e.places())?;
        let needle = q.q.as_deref().map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty());
        let gaps = flags.contains(&"gap");
        Ok(Filter { q, source, places, needle, flags, gaps })
    }

    /// Whether a row can match before its view is made (its place and kind).
    fn row_may(&self, ch: &Charge, x: &Ctx) -> bool {
        if let Some(k) = self.q.kind.as_deref().filter(|k| !k.is_empty()) {
            let have = ch.kind.map_or("unknown", kind_word);
            if !k.split(',').any(|w| w == have) {
                return false;
            }
        }
        if let Some(ps) = &self.places {
            let here = ch.position.as_ref().and_then(|p| x.places.place_at(p.lat, p.lon)).map(|p| p.id);
            if !here.is_some_and(|id| ps.contains(&id)) {
                return false;
            }
        }
        true
    }

    fn matches(&self, v: &ChargeView) -> bool {
        let q = self.q;
        if let Some(k) = q.place_kind.as_deref().filter(|k| !k.is_empty()) {
            let have = v.place.as_ref().map_or("none", |p| p.kind);
            if have != k {
                return false;
            }
        }
        if let Some(n) = &self.needle {
            let text = [
                v.place.as_ref().map(|p| p.name.as_str()),
                v.address.short.as_deref(),
                v.address.locality.as_deref(),
                v.kind,
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase();
            if !text.contains(n.as_str()) {
                return false;
            }
        }
        if let Some(m) = q.min_cost {
            match &v.cost {
                Some(c) if c.amount < m => return false,
                None if !q.include_unpriced.unwrap_or(false) => return false,
                _ => {}
            }
        }
        if q.min_duration_min.is_some_and(|m| (v.charging_ms as f64) < m * 60_000.0) {
            return false;
        }
        if !self.flags.is_empty()
            && !self.flags.iter().any(|f| if *f == "gap" { v.has_gaps } else { v.flags.in_progress })
        {
            return false;
        }
        true
    }
}

/// Calls `f` with the view of each charge of `cars` that matches the filters (newest or oldest first per car). With
/// [`Filter::gaps`] each is marked with its gaps, read a week of rows at a time ([`HoleWindow`]).
fn each_view(
    e: &Engine,
    cars: &[CarId],
    flt: &Filter,
    newest_first: bool,
    mut f: impl FnMut(&Car, &Charge, ChargeView) -> QResult<bool>,
) -> QResult<()> {
    let (a, b) = flt.q.period.bounds()?;
    for &car in cars {
        let c = car_of(e, car)?;
        let x = ctx(e, &c)?;
        let shown = Shown::new(e, car, flt.source)?;
        let fill = super::gapfill::Gaps::new(e, car, flt.source)?;
        prefill(e, car, a, b, &fill)?;
        let mut win = HoleWindow::default();
        let mut go = true;
        each::<Charge>(e, car, a, b, flt.source, &shown, newest_first, |ch| {
            if !flt.row_may(&ch, &x) {
                return Ok(true);
            }
            let mut v = view_filled(e, &c, &ch, &x, &fill)?;
            if flt.gaps {
                mark(&mut v, &win.of(e, &c, &ch)?);
            }
            if !flt.matches(&v) {
                return Ok(true);
            }
            go = f(&c, &ch, v)?;
            Ok(go)
        })?;
        if !go {
            break;
        }
    }
    Ok(())
}

/// A charge's span for its gaps (at least 1 ms).
fn span_of(ch: &Charge) -> (u64, u64) {
    (ch.start_ms, ch.end_ms.max(ch.start_ms + 1))
}

/// Marks a view with the gaps inside its charge.
fn mark(v: &mut ChargeView, gaps: &[Gap]) {
    v.has_gaps = !gaps.is_empty();
    v.flags.gap_ms = covered_ms(gaps);
}

/// The length of the stretch [`HoleWindow`] reads at once.
const HOLE_WINDOW_MS: u64 = 7 * 86_400_000;

/// The holes of one stretch of a car (a week, widened to the charge that needed it), read again only when a charge
/// falls outside it: rows stream by in order, so each week's records are read about once.
#[derive(Default)]
struct HoleWindow {
    at: Option<(u64, u64)>,
    holes: Vec<Gap>,
}

impl HoleWindow {
    fn of(&mut self, e: &Engine, c: &Car, ch: &Charge) -> QResult<Vec<Gap>> {
        let (a, b) = span_of(ch);
        if !self.at.is_some_and(|(x, y)| x <= a && b <= y) {
            let lo = a - a % HOLE_WINDOW_MS;
            let hi = b.max(lo.saturating_add(HOLE_WINDOW_MS));
            self.holes = holes(e, c, lo, hi)?;
            self.at = Some((lo, hi));
        }
        Ok(holes_in(&self.holes, a, b))
    }
}

/// Marks a page's rows with their gaps: each car's holes read over its rows' spans, merged where they are within a
/// day of each other, so a page costs about the records of the days it shows.
fn mark_gaps(e: &Engine, rows: &mut [ChargeView]) -> QResult<()> {
    let mut by_car: BTreeMap<CarId, Vec<usize>> = BTreeMap::new();
    for (i, v) in rows.iter().enumerate() {
        by_car.entry(v.car.0).or_default().push(i);
    }
    for (car, mut idx) in by_car {
        let c = car_of(e, car)?;
        let span = |v: &ChargeView| (v.start_ms, v.end_ms.unwrap_or(v.start_ms + v.charging_ms).max(v.start_ms + 1));
        idx.sort_by_key(|&i| span(&rows[i]));
        let mut k = 0;
        while k < idx.len() {
            let (lo, mut hi) = span(&rows[idx[k]]);
            let mut j = k + 1;
            while j < idx.len() && span(&rows[idx[j]]).0 <= hi.saturating_add(86_400_000) {
                hi = hi.max(span(&rows[idx[j]]).1);
                j += 1;
            }
            let found = holes(e, &c, lo, hi)?;
            for &i in &idx[k..j] {
                let (a, b) = span(&rows[i]);
                mark(&mut rows[i], &holes_in(&found, a, b));
            }
            k = j;
        }
    }
    Ok(())
}

/// Running totals of a list.
#[derive(Default)]
struct Acc {
    count: u64,
    added: f64,
    grid: f64,
    grid_known: u64,
    by_cur: BTreeMap<String, f64>,
    unpriced: u64,
    ms: u64,
}

impl Acc {
    fn add(&mut self, v: &ChargeView) {
        self.count += 1;
        self.added += v.energy_added_kwh.unwrap_or(0.0);
        if let Some(g) = v.energy_grid_kwh {
            self.grid += g;
            self.grid_known += 1;
        }
        match &v.cost {
            Some(c) => *self.by_cur.entry(c.currency.clone()).or_default() += c.amount,
            None => self.unpriced += 1,
        }
        self.ms += v.charging_ms;
    }

    fn totals(self) -> Totals {
        Totals {
            count: self.count,
            energy_added_kwh: r(Some(self.added), 3).unwrap_or(0.0),
            energy_grid_kwh: r(Some(self.grid), 3).unwrap_or(0.0),
            energy_grid_known: self.grid_known,
            dc_inlet_kwh: None,
            cost_by_currency: money(self.by_cur),
            unpriced: self.unpriced,
            avg_duration_ms: (self.count > 0).then(|| self.ms / self.count),
        }
    }
}

/// Where a row stands in a list: its sort key, then its car and id.
type Rank = (f64, CarId, u64);

fn cmp_rank(a: &Rank, b: &Rank, desc: bool) -> Ordering {
    let o = a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2));
    if desc { o.reverse() } else { o }
}

/// A row in the bounded heap of a page: the greatest is the one that comes last in the list.
struct Ranked<T> {
    rank: Rank,
    desc: bool,
    row: T,
}

impl<T> PartialEq for Ranked<T> {
    fn eq(&self, o: &Self) -> bool {
        self.cmp(o) == Ordering::Equal
    }
}
impl<T> Eq for Ranked<T> {}
impl<T> PartialOrd for Ranked<T> {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}
impl<T> Ord for Ranked<T> {
    fn cmp(&self, o: &Self) -> Ordering {
        cmp_rank(&self.rank, &o.rank, self.desc)
    }
}

/// The best `limit + 1` rows after a cursor, kept while rows stream by.
struct Best<T> {
    heap: BinaryHeap<Ranked<T>>,
    keep: usize,
    desc: bool,
    after: Option<Rank>,
}

impl<T> Best<T> {
    fn new(limit: usize, desc: bool, after: Option<Rank>) -> Best<T> {
        Best { heap: BinaryHeap::new(), keep: limit + 1, desc, after }
    }

    fn offer(&mut self, rank: Rank, row: T) {
        if self.after.as_ref().is_some_and(|a| cmp_rank(&rank, a, self.desc) != Ordering::Greater) {
            return;
        }
        if self.heap.len() == self.keep
            && self.heap.peek().is_some_and(|w| cmp_rank(&rank, &w.rank, self.desc) != Ordering::Less)
        {
            return;
        }
        self.heap.push(Ranked { rank, desc: self.desc, row });
        if self.heap.len() > self.keep {
            self.heap.pop();
        }
    }

    /// The page in order, and the rank of its last row when more follow.
    fn page(self) -> (Vec<T>, Option<Rank>) {
        let keep = self.keep;
        let mut all = self.heap.into_sorted_vec();
        let more = all.len() == keep;
        all.truncate(keep - 1);
        let last = if more { all.last().map(|x| x.rank) } else { None };
        (all.into_iter().map(|x| x.row).collect(), last)
    }
}

fn read_cursor(paging: &Paging, sig: &str) -> QResult<Option<Rank>> {
    let Some(c) = &paging.after else { return Ok(None) };
    let bad = || QueryError::bad("after", "not a cursor of this list: start again without it");
    let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(c).map_err(|_| bad())?;
    let body: CursorBody = serde_json::from_slice(&raw).map_err(|_| bad())?;
    if body.h != sig_hash(sig) {
        return Err(bad());
    }
    let (car, id) = body.id.split_once(':').ok_or_else(bad)?;
    Ok(Some((f64::from_bits(body.k), car.parse().map_err(|_| bad())?, id.parse().map_err(|_| bad())?)))
}

fn make_cursor(rank: &Rank, sig: &str) -> QResult<String> {
    let body = CursorBody { k: rank.0.to_bits(), id: format!("{}:{}", rank.1, rank.2), h: sig_hash(sig) };
    let raw = serde_json::to_vec(&body).map_err(|e| QueryError::Engine(crate::Error::Damaged(e.to_string())))?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw))
}

fn sort_key(sort: &str, v: &ChargeView) -> f64 {
    let n = |x: Option<f64>| x.unwrap_or(f64::NEG_INFINITY);
    match sort {
        "energy" => n(v.energy_added_kwh),
        "duration" => v.charging_ms as f64,
        "cost" => n(v.cost.as_ref().map(|c| c.amount)),
        "power_max" => n(v.power_max_kw),
        "end_soc" => n(v.end_soc_pct),
        _ => v.start_ms as f64,
    }
}

/// `/charges` for the cars given: one page (at most `limit` rows, any sort, read in one pass over the rows while only
/// the page is kept) and the totals of every matching charge.
pub fn charges(e: &Engine, cars: &[CarId], q: &ChargesQuery, paging: &Paging) -> QResult<ChargesPage> {
    let flt = Filter::new(e, q)?;
    let sort = q.sort.as_deref().filter(|s| !s.is_empty()).unwrap_or("start");
    if !SORTS.contains(&sort) {
        return Err(QueryError::bad("sort", "start, energy, duration, cost, power_max or end_soc"));
    }
    let desc = super::parse_dir(q.dir.as_deref(), true)?;
    let limit = paging.limit()?;
    let sig = format!("charges|{cars:?}|{}|{q:?}", q.period.sig());
    let after = read_cursor(paging, &sig)?;
    let mut acc = Acc::default();
    let mut best = Best::new(limit, desc, after);
    each_view(e, cars, &flt, desc, |c, ch, v| {
        acc.add(&v);
        best.offer((sort_key(sort, &v), c.id, ch.id), v);
        Ok(true)
    })?;
    let (mut items, last) = best.page();
    if !flt.gaps {
        mark_gaps(e, &mut items)?;
    }
    let next = last.map(|l| make_cursor(&l, &sig)).transpose()?;
    Ok(ChargesPage {
        car: None,
        time_zone: None,
        cars: Some(cars.iter().map(|&c| CarRef(c)).collect()),
        items,
        next,
        totals: acc.totals(),
    })
}

/// `/cars/{car}/charges`: [`charges`] for one car, with the car and its time zone.
pub fn car_charges(e: &Engine, car: CarId, q: &ChargesQuery, paging: &Paging) -> QResult<ChargesPage> {
    let c = car_of(e, car)?;
    let mut page = charges(e, &[car], q, paging)?;
    page.car = Some(CarRef(car));
    page.time_zone = Some(c.time_zone);
    page.cars = None;
    Ok(page)
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BreakdownPart {
    pub rate: Option<String>,
    pub per_kwh: Option<f64>,
    pub per_minute: Option<f64>,
    pub kwh: Option<f64>,
    pub minutes: Option<f64>,
    pub cost: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Breakdown {
    pub parts: Vec<BreakdownPart>,
    pub session_fee: Option<f64>,
    pub total: Option<f64>,
    pub gap_ms: u64,
    /// `tariff` (each rate of the place's price), `total`, `per_kwh` or `per_minute` (typed in), or the cost's own
    /// source (`teslamate`, `free_supercharging`, `tesla_invoice`, `teslafi`); null without a cost.
    pub mode: Option<String>,
}

/// A cost typed on the charge, as it was entered.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ManualCost {
    pub mode: Option<String>,
    pub amount: Option<f64>,
    pub currency: String,
    pub energy_basis: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ChargeDetail {
    #[serde(flatten)]
    pub charge: ChargeView,
    pub prev: Option<String>,
    pub next: Option<String>,
    pub cost_breakdown: Breakdown,
    pub manual_cost: Option<ManualCost>,
    pub gaps: Vec<Gap>,
    /// The Supercharger sessions matched to it ([`crate::invoices`]), oldest first; empty without one.
    pub invoices: Vec<InvoiceRef>,
}

/// A Supercharger session matched to a charge, as the charge's page shows it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct InvoiceRef {
    /// The session's key (`tesla-...`), for its PDF (`.../invoices/{invoice}/pdf`).
    pub id: String,
    /// `tesla` or `teslafi`.
    pub source: String,
    pub site: Option<String>,
    pub start_ms: u64,
    pub end_ms: u64,
    pub total: Option<f64>,
    pub currency: Option<String>,
    pub kwh_billed: Option<f64>,
    pub minutes_billed: Option<f64>,
    pub idle_fee: Option<f64>,
    pub fees: Vec<crate::invoices::Fee>,
    /// The invoice's own file name.
    pub invoice_name: Option<String>,
    /// The PDF is kept here (else not given by the source, or not downloaded yet).
    pub pdf: bool,
    /// The charge's cost is this invoice's (false while a typed cost wins over it).
    pub applied: bool,
}

/// `/cars/{car}/charges/{charge}`. The charges before and after it are read from the db from it outwards.
pub fn charge(e: &Engine, car: CarId, id: u64) -> QResult<ChargeDetail> {
    let c = car_of(e, car)?;
    let ch: Charge = e.db().get(car, id)?.ok_or(QueryError::NotFound("no_such_charge"))?;
    let x = ctx(e, &c)?;
    let shown = Shown::new(e, car, None)?;
    let at = (ch.start_ms, ch.id);
    let mut prev = None;
    each::<Charge>(e, car, 0, ch.start_ms.saturating_add(1), None, &shown, true, |o| {
        if (o.start_ms, o.id) < at {
            prev = Some(o.id.to_string());
            return Ok(false);
        }
        Ok(true)
    })?;
    let mut next = None;
    each::<Charge>(e, car, ch.start_ms, u64::MAX, None, &shown, false, |o| {
        if (o.start_ms, o.id) > at {
            next = Some(o.id.to_string());
            return Ok(false);
        }
        Ok(true)
    })?;
    // the same gaps the lists mark (the records' and the chain's), so a charge reads alike in both
    let end = ch.end_ms.max(ch.start_ms + 1);
    let gaps = holes_in(&holes(e, &c, ch.start_ms, end)?, ch.start_ms, end);
    let kept = cost_live(e, &c, &ch, x.live == Some(ch.id))?;
    let minutes = ch.end_ms.saturating_sub(ch.start_ms) as f64 / 60_000.0;
    let cost_breakdown = match &kept {
        Some(k) if k.source == "manual" => Breakdown {
            parts: vec![BreakdownPart {
                rate: Some("Entered".into()),
                per_kwh: if k.mode.as_deref() == Some("per_kwh") { k.entered } else { None },
                per_minute: if k.mode.as_deref() == Some("per_minute") { k.entered } else { None },
                kwh: r(
                    if k.energy_basis.as_deref() == Some("grid") {
                        ch.energy_used_kwh.or(ch.energy_added_kwh)
                    } else {
                        ch.energy_added_kwh
                    },
                    3,
                ),
                minutes: (k.mode.as_deref() == Some("per_minute")).then(|| (minutes * 10.0).round() / 10.0),
                cost: k.amount,
            }],
            session_fee: None,
            total: Some(k.amount),
            gap_ms: 0,
            mode: k.mode.clone(),
        },
        Some(k) => Breakdown {
            parts: k
                .parts
                .iter()
                .map(|p| BreakdownPart {
                    rate: p.rate.clone().or_else(|| Some("Standard".into())),
                    per_kwh: Some(p.per_kwh),
                    per_minute: None,
                    kwh: r(Some(p.kwh), 3),
                    minutes: None,
                    cost: (p.cost * 100.0).round() / 100.0,
                })
                .collect(),
            session_fee: k.session_fee,
            total: Some(k.amount),
            gap_ms: k.gap_ms,
            mode: Some(k.source.clone()),
        },
        None => Breakdown { parts: Vec::new(), session_fee: None, total: None, gap_ms: 0, mode: None },
    };
    let sessions = e.invoice_sessions_of(car, ch.id)?;
    let applied = kept.as_ref().is_some_and(|k| k.source == "tesla_invoice" || k.source == "teslafi");
    let cost_breakdown = match &kept {
        // an invoice's fees: energy at its rate, minutes (idle, or billed by time) at theirs
        Some(k) if applied => Breakdown {
            parts: sessions
                .iter()
                .flat_map(|s| s.fees.iter())
                .map(|f| BreakdownPart {
                    rate: Some(
                        match f.kind.as_str() {
                            "charging" => "Charging",
                            "idle" => "Idle fee",
                            _ => "Other fee",
                        }
                        .into(),
                    ),
                    per_kwh: f.rate.filter(|_| f.unit.as_deref() == Some("kwh")),
                    per_minute: f.rate.filter(|_| f.unit.as_deref() == Some("min")),
                    kwh: f.usage.filter(|_| f.unit.as_deref() == Some("kwh")),
                    minutes: f.usage.filter(|_| f.unit.as_deref() == Some("min")),
                    cost: (f.amount * 100.0).round() / 100.0,
                })
                .collect(),
            session_fee: None,
            total: Some(k.amount),
            gap_ms: 0,
            mode: Some(k.source.clone()),
        },
        _ => cost_breakdown,
    };
    let invoices = sessions
        .iter()
        .map(|s| InvoiceRef {
            id: s.key.clone(),
            source: s.source.clone(),
            site: s.site.clone(),
            start_ms: s.start_ms,
            end_ms: s.end_ms,
            total: s.total,
            currency: s.currency.clone(),
            kwh_billed: s.kwh_billed,
            minutes_billed: s.minutes_billed,
            idle_fee: s.idle_fee(),
            fees: s.fees.clone(),
            invoice_name: s.invoice_name.clone(),
            pdf: s.pdf,
            applied,
        })
        .collect();
    let manual_cost = kept.as_ref().filter(|k| k.source == "manual").map(|k| ManualCost {
        mode: k.mode.clone(),
        amount: k.entered,
        currency: k.currency.clone(),
        energy_basis: k.energy_basis.clone(),
    });
    let fill = super::gapfill::Gaps::new(e, car, None)?;
    let mut v = filled_with(e, &c, &ch, &x, kept.as_ref(), &fill)?;
    v.has_gaps = !gaps.is_empty();
    v.flags.gap_ms = covered_ms(&gaps);
    Ok(ChargeDetail { charge: v, prev, next, cost_breakdown, manual_cost, gaps, invoices })
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Curve {
    pub charge: String,
    /// `ac`, `dc` or null.
    pub kind: Option<&'static str>,
    pub t_ms: Vec<u64>,
    pub values: BTreeMap<String, Column>,
    pub step_ms: u64,
}

/// One field of a curve, a value (or null) per `t_ms`: numbers, or words (`limit_source`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum Column {
    Numbers(Vec<Option<f64>>),
    Words(Vec<Option<&'static str>>),
}

impl Column {
    /// The column's length.
    pub fn len(&self) -> usize {
        match self {
            Column::Numbers(v) => v.len(),
            Column::Words(v) => v.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The values of a column of numbers (none for words).
    pub fn numbers(&self) -> &[Option<f64>] {
        match self {
            Column::Numbers(v) => v,
            Column::Words(_) => &[],
        }
    }
}

/// Every field the curve takes. Those this server does not record yet come as a column of nulls.
pub const CURVE_FIELDS: &[&str] = &[
    "power_kw",
    "volts_v",
    "amps_a",
    "soc_pct",
    "usable_soc_pct",
    "battery_c",
    "range_km",
    "outside_c",
    "phases",
    "pilot_a",
    "ac_line_kw",
    "heater_kw",
    "dc_volts_v",
    "dc_amps_a",
    "ac_volts_v",
    "ac_amps_a",
    "limit_source",
];

/// The curve fields whose values are words, not numbers.
const WORD_FIELDS: &[&str] = &["limit_source"];

/// The fields asked for when none are named.
const DEFAULT_CURVE_FIELDS: &[&str] = &["power_kw", "soc_pct", "volts_v", "amps_a", "battery_c"];

/// The car's own samples' columns a curve field comes from (the pack side and the outside temperature).
fn sample_field(f: &str) -> Option<&'static str> {
    match f {
        "volts_v" => Some("battery_voltage_v"),
        "amps_a" => Some("battery_current_a"),
        "outside_c" => Some("outside_temp_c"),
        _ => None,
    }
}

/// `/cars/{car}/charges/{charge}/curve`: 1 Hz up to `max_points` (default 4000, 2 to 20000), coarser beyond (`step_ms`
/// says). `power_kw` is the charging power (positive); `volts_v` and `amps_a` the pack side; `dc_*` the DC
/// charger's output and `ac_*` the AC line at the inlet (each null on the other kind).
pub fn curve(e: &Engine, car: CarId, id: u64, fields: &[&str], max_points: Option<usize>) -> QResult<Scoped<Curve>> {
    let c = car_of(e, car)?;
    let ch: Charge = e.db().get(car, id)?.ok_or(QueryError::NotFound("no_such_charge"))?;
    let max = max_points.unwrap_or(4000);
    if !(2..=20_000).contains(&max) {
        return Err(QueryError::bad("max_points", "2 to 20000"));
    }
    let fields: Vec<&str> = if fields.is_empty() { DEFAULT_CURVE_FIELDS.to_vec() } else { fields.to_vec() };
    if let Some(bad) = fields.iter().find(|f| !CURVE_FIELDS.contains(f)) {
        return Err(QueryError::bad("fields", &format!("{bad} is not a curve field")));
    }
    let pts = costs::curve_of(e, car, &ch)?;
    // seconds a point stands for, from the curve's span: at most `max` buckets from its first point
    let t0 = pts.first().map_or(0, |p| p.t_ms);
    let span_s = pts.last().map_or(0, |p| p.t_ms.saturating_sub(t0)) / 1000 + 1;
    let step = span_s.div_ceil(max as u64).max(1) * 1000;
    let mut t_ms = Vec::new();
    let mut picked: Vec<&ChargePoint> = Vec::new();
    let mut last_bucket = None;
    for p in &pts {
        let b = p.t_ms.saturating_sub(t0) / step;
        if last_bucket != Some(b) {
            last_bucket = Some(b);
            t_ms.push(p.t_ms);
            picked.push(p);
        }
    }
    // the samples' columns, each read at the curve's times (the newest sample within 30 s at or before)
    let wanted: Vec<&str> =
        fields.iter().filter_map(|f| sample_field(f)).collect::<BTreeSet<_>>().into_iter().collect();
    let mut from_samples: BTreeMap<&str, Vec<Option<f64>>> = BTreeMap::new();
    if !wanted.is_empty() && !t_ms.is_empty() {
        let cols =
            e.db().read_fields::<Sample>(car, ch.source, ch.start_ms, ch.end_ms.max(ch.start_ms) + 1, &wanted)?;
        for (name, col) in &cols.values {
            let mut out = Vec::with_capacity(t_ms.len());
            let mut i = 0usize;
            let mut have: Option<(u64, f64)> = None;
            for &t in &t_ms {
                while i < cols.t_ms.len() && cols.t_ms[i] <= t {
                    if let Some(v) = col.get(i).copied().flatten() {
                        have = Some((cols.t_ms[i], v));
                    }
                    i += 1;
                }
                out.push(have.filter(|(at, _)| t - at <= 30_000).map(|(_, v)| v));
            }
            if let Some(n) = wanted.iter().find(|w| **w == name.as_str()) {
                from_samples.insert(n, out);
            }
        }
    }
    let (ac, dc) = (ch.kind == Some(ChargeKind::Ac), ch.kind == Some(ChargeKind::Dc));
    let mut values = BTreeMap::new();
    for f in &fields {
        if WORD_FIELDS.contains(f) {
            // what sets the current (`station`, `cable`, `car`, `setting`): not decoded into the charge points yet,
            // so null at every point, as for a car that does not say
            values.insert((*f).to_string(), Column::Words(vec![None; picked.len()]));
            continue;
        }
        let col: Vec<Option<f64>> = if let Some(s) = sample_field(f) {
            let src = from_samples.get(s);
            (0..picked.len())
                .map(|i| {
                    let v = src.and_then(|c| c.get(i).copied().flatten());
                    match *f {
                        "volts_v" => r(v, 1),
                        "amps_a" => r(v.map(f64::abs), 1),
                        _ => r(v, 1),
                    }
                })
                .collect()
        } else {
            picked
                .iter()
                .map(|p| match *f {
                    // charging power, positive
                    "power_kw" => r(p.power_kw.map(f64::abs), 2),
                    "soc_pct" => r(p.soc, 1),
                    "battery_c" => r(p.battery_temp_c, 1),
                    "range_km" => r(p.range_km, 1),
                    "dc_volts_v" if dc => r(p.voltage_v, 1),
                    "dc_amps_a" if dc => r(p.current_a.map(f64::abs), 1),
                    "ac_volts_v" if ac => r(p.voltage_v, 1),
                    "ac_amps_a" if ac => r(p.current_a.map(f64::abs), 1),
                    _ => None,
                })
                .collect()
        };
        values.insert((*f).to_string(), Column::Numbers(col));
    }
    Ok(scoped(&c, Curve { charge: ch.id.to_string(), kind: ch.kind.map(kind_word), t_ms, values, step_ms: step }))
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct KindStat {
    pub kwh: f64,
    pub ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PriceOf {
    pub currency: String,
    pub value: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Prices {
    pub all: Vec<PriceOf>,
    pub ac: Vec<PriceOf>,
    pub dc: Vec<PriceOf>,
    pub basis: &'static str,
    /// Charges with a cost.
    pub priced: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CostPer100 {
    pub value: Vec<Money>,
    pub method: &'static str,
    pub distance_km: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Heatmap {
    pub start: Vec<Vec<u64>>,
    pub end: Vec<Vec<u64>>,
    pub rows: &'static str,
    pub columns: &'static str,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Band {
    pub band: String,
    pub count: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Run {
    pub car: CarRef,
    pub charges: u64,
    pub start_ms: u64,
    pub min_start_soc_pct: Option<f64>,
    pub max_end_soc_pct: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ChargingStats {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub car: Option<CarRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_zone: Option<String>,
    pub cars: Vec<CarRef>,
    pub count: u64,
    pub energy_added_kwh: f64,
    pub by_kind: BTreeMap<&'static str, KindStat>,
    pub cost_by_currency: Vec<Money>,
    pub price_per_kwh: Prices,
    /// Sessions the charger names as a Supercharger, priced: empty until the charger's name is decoded.
    pub supercharger_cost: Vec<Money>,
    pub cost_per_100km: CostPer100,
    pub start_bands: Vec<Band>,
    pub end_bands: Vec<Band>,
    pub heatmap: Heatmap,
    pub runs: Vec<Run>,
    pub share_above_80_pct: Option<f64>,
    pub share_below_20_pct: Option<f64>,
}

/// Runs kept at most (the newest).
pub const MAX_RUNS: usize = 1000;

/// Cost and the energy it is over, per currency.
#[derive(Default)]
struct PriceAcc(BTreeMap<String, (f64, f64)>);

impl PriceAcc {
    fn add(&mut self, v: &ChargeView) {
        let (Some(c), Some(p)) = (&v.cost, &v.price_per_kwh) else { return };
        let kwh = if p.basis == "grid" { v.energy_grid_kwh } else { v.energy_added_kwh };
        let e = self.0.entry(c.currency.clone()).or_default();
        e.0 += c.amount;
        e.1 += kwh.unwrap_or(0.0);
    }

    fn list(self) -> Vec<PriceOf> {
        self.0
            .into_iter()
            .map(|(currency, (cost, kwh))| PriceOf { currency, value: r((kwh > 0.0).then(|| cost / kwh), 4) })
            .collect()
    }
}

fn bands(b: [u64; 10]) -> Vec<Band> {
    b.iter().enumerate().map(|(i, n)| Band { band: format!("{}-{}", i * 10, i * 10 + 10), count: *n }).collect()
}

/// `/charging-stats` for the cars given (`/cars/{car}/charging-stats` is one car): one pass over the charges (each
/// car oldest first), only tallies kept.
pub fn charging_stats(e: &Engine, cars: &[CarId], q: &ChargesQuery) -> QResult<ChargingStats> {
    let mut flt = Filter::new(e, q)?;
    flt.gaps = false;
    let (a, b) = q.period.bounds()?;
    let mut acc = Acc::default();
    let mut by_kind: BTreeMap<&'static str, KindStat> =
        ["ac", "dc", "unknown"].into_iter().map(|k| (k, KindStat { kwh: 0.0, ms: 0 })).collect();
    let (mut p_all, mut p_ac, mut p_dc) = (PriceAcc::default(), PriceAcc::default(), PriceAcc::default());
    let mut priced = 0u64;
    let mut start_bands = [0u64; 10];
    let mut end_bands = [0u64; 10];
    let mut heat_start = vec![vec![0u64; 24]; 7];
    let mut heat_end = vec![vec![0u64; 24]; 7];
    let (mut above, mut below, mut with_end, mut with_start) = (0u64, 0u64, 0u64, 0u64);
    let mut runs: VecDeque<Run> = VecDeque::new();
    // the run being followed (its car, the start of its last charge) and whether it counts yet
    let mut cur: Option<(Run, u64)> = None;
    each_view(e, cars, &flt, false, |c, _, v| {
        acc.add(&v);
        let k = by_kind.entry(v.kind.unwrap_or("unknown")).or_insert(KindStat { kwh: 0.0, ms: 0 });
        k.kwh += v.energy_added_kwh.unwrap_or(0.0);
        k.ms += v.charging_ms;
        if v.cost.is_some() {
            priced += 1;
        }
        p_all.add(&v);
        match v.kind {
            Some("ac") => p_ac.add(&v),
            Some("dc") => p_dc.add(&v),
            _ => {}
        }
        let tz = crate::tz::zone(&c.time_zone);
        let (h, w) = crate::tz::hour_weekday(v.start_ms, tz);
        heat_start[w as usize][h as usize] += 1;
        if let Some(end) = v.end_ms {
            let (h, w) = crate::tz::hour_weekday(end, tz);
            heat_end[w as usize][h as usize] += 1;
        }
        if let Some(s) = v.start_soc_pct {
            start_bands[((s / 10.0).floor().max(0.0) as usize).min(9)] += 1;
            with_start += 1;
            below += u64::from(s < 20.0);
        }
        if let Some(s) = v.end_soc_pct {
            end_bands[((s / 10.0).floor().max(0.0) as usize).min(9)] += 1;
            with_end += 1;
            above += u64::from(s > 80.0);
        }
        // runs: two or more charges of a car, each starting within a day of the one before
        let joins = cur
            .as_ref()
            .is_some_and(|(run, last)| run.car == CarRef(c.id) && v.start_ms.saturating_sub(*last) <= 86_400_000);
        if joins {
            if let Some((run, last)) = cur.as_mut() {
                run.charges += 1;
                run.min_start_soc_pct = min_opt(run.min_start_soc_pct, v.start_soc_pct);
                run.max_end_soc_pct = max_opt(run.max_end_soc_pct, v.end_soc_pct);
                *last = v.start_ms;
            }
        } else {
            close_run(&mut cur, &mut runs);
            cur = Some((
                Run {
                    car: CarRef(c.id),
                    charges: 1,
                    start_ms: v.start_ms,
                    min_start_soc_pct: v.start_soc_pct,
                    max_end_soc_pct: v.end_soc_pct,
                },
                v.start_ms,
            ));
        }
        Ok(true)
    })?;
    close_run(&mut cur, &mut runs);
    for k in by_kind.values_mut() {
        k.kwh = r(Some(k.kwh), 3).unwrap_or(0.0);
    }
    // the distance driven in the period, for the cost per 100 km
    let mut km = 0.0;
    for &car in cars {
        let shown = Shown::new(e, car, flt.source)?;
        each::<Drive>(e, car, a, b, flt.source, &shown, false, |d| {
            km += d.distance_km.unwrap_or(0.0);
            Ok(true)
        })?;
    }
    let totals = acc.totals();
    let cost_per_100km = CostPer100 {
        value: if km > 1.0 {
            totals
                .cost_by_currency
                .iter()
                .map(|m| Money {
                    currency: m.currency.clone(),
                    amount: (m.amount / km * 100.0 * 100.0).round() / 100.0,
                })
                .collect()
        } else {
            Vec::new()
        },
        method: "cost of the charges in the period over the distance driven in it",
        distance_km: r(Some(km), 1),
    };
    Ok(ChargingStats {
        car: None,
        time_zone: None,
        cars: cars.iter().map(|&c| CarRef(c)).collect(),
        count: totals.count,
        energy_added_kwh: totals.energy_added_kwh,
        by_kind,
        cost_by_currency: totals.cost_by_currency,
        price_per_kwh: Prices {
            all: p_all.list(),
            ac: p_ac.list(),
            dc: p_dc.list(),
            basis: "grid energy where measured (AC), else the energy into the pack",
            priced,
        },
        supercharger_cost: Vec::new(),
        cost_per_100km,
        start_bands: bands(start_bands),
        end_bands: bands(end_bands),
        heatmap: Heatmap {
            start: heat_start,
            end: heat_end,
            rows: "weekday, Monday first",
            columns: "hour, local time",
        },
        runs: runs.into_iter().collect(),
        share_above_80_pct: r((with_end > 0).then(|| above as f64 / with_end as f64 * 100.0), 1),
        share_below_20_pct: r((with_start > 0).then(|| below as f64 / with_start as f64 * 100.0), 1),
    })
}

/// `/cars/{car}/charging-stats`: [`charging_stats`] for one car, with the car and its time zone.
pub fn car_charging_stats(e: &Engine, car: CarId, q: &ChargesQuery) -> QResult<ChargingStats> {
    let c = car_of(e, car)?;
    let mut s = charging_stats(e, &[car], q)?;
    s.car = Some(CarRef(car));
    s.time_zone = Some(c.time_zone);
    Ok(s)
}

/// Ends the run being followed: kept when it has two charges or more (the newest [`MAX_RUNS`]).
fn close_run(cur: &mut Option<(Run, u64)>, runs: &mut VecDeque<Run>) {
    if let Some((run, _)) = cur.take()
        && run.charges >= 2
    {
        runs.push_back(run);
        if runs.len() > MAX_RUNS {
            runs.pop_front();
        }
    }
}

fn min_opt(a: Option<f64>, b: Option<f64>) -> Option<f64> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.min(y)),
        (x, None) => x,
        (None, y) => y,
    }
}

fn max_opt(a: Option<f64>, b: Option<f64>) -> Option<f64> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.max(y)),
        (x, None) => x,
        (None, y) => y,
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PlaceStat {
    /// The saved place's id, or null for a spot that is not one.
    pub place: Option<String>,
    pub label: Option<String>,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    pub sessions: u64,
    pub energy_added_kwh: f64,
    pub cost_by_currency: Vec<Money>,
    /// Over the grid energy where known, else the energy into the pack; null with more than one currency.
    pub price_per_kwh: Option<f64>,
    pub cars: Vec<CarRef>,
}

/// A page of `/charging-stats/places`, with the cars it covers.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PlacesPage {
    pub cars: Vec<CarRef>,
    pub items: Vec<PlaceStat>,
    pub next: Option<String>,
}

/// One spot of `/charging-stats/places` while it is summed.
struct Spot {
    stat: PlaceStat,
    cost: BTreeMap<String, f64>,
    basis_kwh: f64,
    cars: BTreeSet<CarId>,
}

/// `/charging-stats/places`: where the cars charged, by saved place, else by position (about 100 m), never by
/// address text. Charges with neither are left out. `sort=energy|cost|sessions`, most first.
pub fn charging_places(
    e: &Engine,
    cars: &[CarId],
    q: &ChargesQuery,
    sort: Option<&str>,
    paging: &Paging,
) -> QResult<PlacesPage> {
    let sort = sort.filter(|s| !s.is_empty()).unwrap_or("energy");
    if !["energy", "cost", "sessions"].contains(&sort) {
        return Err(QueryError::bad("sort", "energy, cost or sessions"));
    }
    let mut flt = Filter::new(e, q)?;
    flt.gaps = false;
    let places = e.places();
    let mut by: BTreeMap<String, Spot> = BTreeMap::new();
    each_view(e, cars, &flt, false, |c, _, v| {
        let (key, place, label, lat, lon) = match (&v.place, &v.position) {
            (Some(p), _) => {
                let centre = super::parse_place(&p.id).and_then(|id| places.get(id)).map(|pl| (pl.lat, pl.lon));
                let (lat, lon) = match (centre, &v.position) {
                    (Some((a, b)), _) => (Some(a), Some(b)),
                    (None, Some(pos)) => (Some(pos.lat), Some(pos.lon)),
                    (None, None) => (None, None),
                };
                (format!("#place {}", p.id), Some(p.id.clone()), Some(p.name.clone()), lat, lon)
            }
            (None, Some(pos)) => {
                let (la, lo) = ((pos.lat * 1000.0).round() / 1000.0, (pos.lon * 1000.0).round() / 1000.0);
                let label = v.address.short.clone().unwrap_or_else(|| format!("{la:.3}, {lo:.3}"));
                (format!("#at {la:.3},{lo:.3}"), None, Some(label), Some(pos.lat), Some(pos.lon))
            }
            (None, None) => return Ok(true),
        };
        let s = by.entry(key).or_insert_with(|| Spot {
            stat: PlaceStat {
                place,
                label,
                lat: r(lat, 6),
                lon: r(lon, 6),
                sessions: 0,
                energy_added_kwh: 0.0,
                cost_by_currency: Vec::new(),
                price_per_kwh: None,
                cars: Vec::new(),
            },
            cost: BTreeMap::new(),
            basis_kwh: 0.0,
            cars: BTreeSet::new(),
        });
        s.stat.sessions += 1;
        s.stat.energy_added_kwh += v.energy_added_kwh.unwrap_or(0.0);
        s.cars.insert(c.id);
        if let Some(cost) = &v.cost {
            *s.cost.entry(cost.currency.clone()).or_default() += cost.amount;
            s.basis_kwh += v.energy_grid_kwh.or(v.energy_added_kwh).unwrap_or(0.0);
        }
        Ok(true)
    })?;
    let items: Vec<(String, PlaceStat)> = by
        .into_iter()
        .map(|(key, s)| {
            let mut st = s.stat;
            let total: f64 = s.cost.values().sum();
            st.price_per_kwh = r((s.cost.len() == 1 && s.basis_kwh > 0.0).then(|| total / s.basis_kwh), 4);
            st.energy_added_kwh = r(Some(st.energy_added_kwh), 3).unwrap_or(0.0);
            st.cost_by_currency = money(s.cost);
            st.cars = s.cars.into_iter().map(CarRef).collect();
            (key, st)
        })
        .collect();
    let sig = format!("charging-places|{cars:?}|{q:?}|{sort}");
    let page = paginate(
        items,
        |(key, s)| {
            let k = match sort {
                "cost" => s.cost_by_currency.iter().map(|m| m.amount).sum(),
                "sessions" => s.sessions as f64,
                _ => s.energy_added_kwh,
            };
            // the key last: two spots may share a name and a figure, and a page must not skip one
            (k, format!("#{}\u{1f}{key}", s.label.clone().unwrap_or_default()))
        },
        true,
        &sig,
        paging,
        false,
    )?;
    Ok(PlacesPage {
        cars: cars.iter().map(|&c| CarRef(c)).collect(),
        items: page.items.into_iter().map(|(_, s)| s).collect(),
        next: page.next,
    })
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DcPoint {
    /// The whole percent.
    pub soc_pct: i64,
    pub power_kw: f64,
    pub battery_c: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DcSession {
    pub charge: String,
    pub label: String,
    pub start_ms: u64,
    pub points: Vec<DcPoint>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MedianPoint {
    pub soc_pct: i64,
    pub power_kw: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DcCurve {
    pub median: Vec<MedianPoint>,
    pub sessions: Vec<DcSession>,
}

/// DC sessions kept at most (the newest in the period).
pub const MAX_DC_SESSIONS: usize = 200;

/// `/cars/{car}/charging-stats/dc-curve`: each DC charge's power by battery percent (one point per whole percent:
/// the mean power over it, the battery temperature at its end), and the median of those per percent. The newest
/// [`MAX_DC_SESSIONS`] sessions of the period, so at most about 100 points each.
pub fn dc_curve(e: &Engine, car: CarId, period: &Period, places: Option<&str>) -> QResult<Scoped<DcCurve>> {
    let c = car_of(e, car)?;
    let (a, b) = period.bounds()?;
    let x = ctx(e, &c)?;
    let want = super::drives::place_list(places, &x.places)?;
    let shown = Shown::new(e, car, None)?;
    let mut sessions = Vec::new();
    each::<Charge>(e, car, a, b, None, &shown, true, |ch| {
        if ch.kind != Some(ChargeKind::Dc) {
            return Ok(true);
        }
        let here = ch.position.as_ref().and_then(|p| x.places.place_at(p.lat, p.lon));
        if let Some(ps) = &want
            && !here.is_some_and(|p| ps.contains(&p.id))
        {
            return Ok(true);
        }
        let mut points: Vec<DcPoint> = Vec::new();
        let mut cur: Option<(i64, f64, u32, Option<f64>)> = None;
        for p in costs::curve_of(e, car, &ch)? {
            let (Some(soc), Some(kw)) = (p.soc, p.power_kw) else { continue };
            let pct = soc.floor() as i64;
            match &mut cur {
                Some((at, sum, n, temp)) if *at == pct => {
                    *sum += kw.abs();
                    *n += 1;
                    *temp = p.battery_temp_c.or(*temp);
                }
                _ => {
                    if let Some((at, sum, n, temp)) = cur.take() {
                        points.push(DcPoint {
                            soc_pct: at,
                            power_kw: r(Some(sum / f64::from(n)), 1).unwrap_or(0.0),
                            battery_c: r(temp, 1),
                        });
                    }
                    cur = Some((pct, kw.abs(), 1, p.battery_temp_c));
                }
            }
        }
        if let Some((at, sum, n, temp)) = cur {
            points.push(DcPoint {
                soc_pct: at,
                power_kw: r(Some(sum / f64::from(n)), 1).unwrap_or(0.0),
                battery_c: r(temp, 1),
            });
        }
        let label =
            here.map(|p| p.name.clone()).or_else(|| ch.address.clone()).unwrap_or_else(|| "Fast charger".to_string());
        sessions.push(DcSession { charge: ch.id.to_string(), label, start_ms: ch.start_ms, points });
        Ok(sessions.len() < MAX_DC_SESSIONS)
    })?;
    sessions.reverse();
    let mut by_pct: BTreeMap<i64, Vec<f64>> = BTreeMap::new();
    for s in &sessions {
        for p in &s.points {
            by_pct.entry(p.soc_pct).or_default().push(p.power_kw);
        }
    }
    let median = by_pct
        .into_iter()
        .map(|(pct, mut v)| {
            v.sort_by(f64::total_cmp);
            let m = if v.len() % 2 == 1 {
                v.get(v.len() / 2).copied()
            } else {
                v.get(v.len() / 2 - 1).zip(v.get(v.len() / 2)).map(|(a, b)| (a + b) / 2.0)
            };
            MedianPoint { soc_pct: pct, power_kw: r(m, 1) }
        })
        .collect();
    Ok(scoped(&c, DcCurve { median, sessions }))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn the_best_rows_after_a_cursor_come_in_order_whatever_order_they_arrive_in() {
        let ranks: Vec<Rank> = (0..23u64).map(|i| (((i * 7) % 5) as f64, (i % 3) as u32, i)).collect();
        for desc in [true, false] {
            let mut want = ranks.clone();
            want.sort_by(|a, b| cmp_rank(a, b, desc));
            let mut got = Vec::new();
            let mut after = None;
            loop {
                let mut best = Best::new(4, desc, after);
                for r in ranks.iter().rev() {
                    best.offer(*r, *r);
                }
                let (rows, last) = best.page();
                assert!(rows.len() <= 4);
                got.extend(rows);
                match last {
                    Some(l) => after = Some(l),
                    None => break,
                }
            }
            assert_eq!(got, want, "desc={desc}");
        }
    }

    #[test]
    fn cursors_go_round_and_belong_to_their_list() {
        let rank: Rank = (f64::NEG_INFINITY, 3, u64::MAX - 1);
        let c = make_cursor(&rank, "a").unwrap();
        let back = read_cursor(&Paging { limit: None, after: Some(c.clone()) }, "a").unwrap().unwrap();
        assert_eq!((back.0.to_bits(), back.1, back.2), (rank.0.to_bits(), rank.1, rank.2));
        let other = read_cursor(&Paging { limit: None, after: Some(c) }, "b");
        assert_eq!(other.unwrap_err().code(), "bad_input");
    }

    #[test]
    fn gaps_are_cut_at_the_charge_and_counted_once() {
        let holes = vec![
            Gap { start_ms: 0, end_ms: 50, kind: "unknown" },
            Gap { start_ms: 40, end_ms: 70, kind: "dropped" },
            Gap { start_ms: 200, end_ms: 300, kind: "chain" },
        ];
        let inside = holes_in(&holes, 30, 100);
        assert_eq!(inside.len(), 2);
        assert_eq!(inside[0].start_ms, 30);
        assert_eq!(covered_ms(&inside), 40);
        assert!(holes_in(&holes, 100, 200).is_empty());
    }

    #[test]
    fn energies_are_compared_only_where_both_sides_can_be() {
        let ch = |kind, added: Option<f64>, used: Option<f64>| Charge {
            id: 1,
            car: 1,
            start_ms: 0,
            end_ms: 3_600_000,
            position: None,
            place: None,
            address: None,
            kind,
            energy_added_kwh: added,
            energy_used_kwh: used,
            start_soc: None,
            end_soc: None,
            start_range_km: None,
            end_range_km: None,
            power_max_kw: None,
            outside_temp_avg_c: None,
            battery_temp_avg_c: None,
            cost: None,
            source: Source::Live,
            inputs: model::Inputs::SMO,
            rules_version: 0,
        };
        let ac = Some(ChargeKind::Ac);
        assert_eq!(compared_energies(&ch(ac, Some(18.0), Some(20.0))), Some((18.0, 20.0)));
        assert_eq!(compared_energies(&ch(ac, Some(20.0), Some(20.0))), Some((20.0, 20.0)));
        // more into the pack than from the grid: not one span or one unit
        assert_eq!(compared_energies(&ch(ac, Some(23.1), Some(20.0))), None);
        assert_eq!(compared_energies(&ch(ac, Some(18.0), Some(0.0))), None);
        // a DC charger's grid side is out of sight, an unknown kind may be DC
        assert_eq!(compared_energies(&ch(Some(ChargeKind::Dc), Some(30.0), Some(31.0))), None);
        assert_eq!(compared_energies(&ch(None, Some(18.0), Some(20.0))), None);
        assert_eq!(compared_energies(&ch(ac, None, Some(20.0))), None);
        assert_eq!(compared_energies(&ch(ac, Some(18.0), None)), None);
    }
}
