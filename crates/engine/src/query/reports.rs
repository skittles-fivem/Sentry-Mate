//! `/statistics`, `/cars/{car}/mileage`, `/cars/{car}/totals`, `/cars/{car}/gaps` and `/compare`.
//!
//! Every figure is summed from the same drive and charge rows the lists show (overlaps with imported history settled
//! by [`Shown`]), so a month here adds up to the Drives and Charges lists for that month. Periods are the car's own
//! local days, ISO weeks, months and years ([`tz::period_of`]).
//!
//! Nothing here holds a whole history: a page of periods is worked out from the calendar first, and only the rows of
//! that page's span are read, a db page at a time ([`super::drives::each`]); `/totals` streams every row through a
//! few counters; `/gaps` reads the states newest first and stops once the page is full.

use super::drives::{Best, each, page_of};
use super::odometer::{self, Odometer};
use super::system::Shown;
use super::{
    CarRef, List, Money, Paging, Period, PlaceBrief, QResult, QueryError, Scoped, car_of, cmp_key, money, place_ref, r,
    read_after, scoped,
};
use crate::Engine;
use crate::tz::{self, Period as P};
use chrono_tz::Tz;
use db::{StateRow, UpdateRow};
use model::{Car, CarId, Charge, Drive, Source, State};
use serde::Serialize;
use std::cmp::Ordering;
use std::collections::BTreeMap;

const DAY: u64 = 86_400_000;
/// A drive longer than this is not looked for before a period's start (a stop's arrival, an event's end).
pub(crate) const LONGEST_DRIVE_MS: u64 = 2 * DAY;
/// Periods one answer may hold at most (a `/compare` series, a page's walk back through the calendar).
const MAX_SLOTS: usize = 20_000;

// ---------------------------------------------------------------- shared helpers

/// When the car's history starts: its first state, drive or charge.
pub(crate) fn first_ms(e: &Engine, car: CarId) -> QResult<Option<u64>> {
    fn oldest<T: db::Item>(e: &Engine, car: CarId) -> QResult<Option<u64>> {
        let q = db::Query::default().oldest_first().limit(1);
        Ok(e.db().list::<T>(car, &q)?.items.first().map(|t| t.start_ms()))
    }
    let all = [oldest::<StateRow>(e, car)?, oldest::<Drive>(e, car)?, oldest::<Charge>(e, car)?];
    Ok(all.into_iter().flatten().min())
}

/// Calls `f` with every row of `car` that meets `[a, b)` (a row that began before `a` and goes on into it included),
/// oldest first, a db page at a time; `f` answers whether to go on.
pub(crate) fn overlapping<T: db::Item>(
    e: &Engine,
    car: CarId,
    a: u64,
    b: u64,
    source: Option<Source>,
    newest_first: bool,
    mut f: impl FnMut(T) -> QResult<bool>,
) -> QResult<()> {
    if a >= b {
        return Ok(());
    }
    let mut q = db::Query::between(a, b).limit(db::MAX_PAGE);
    if !newest_first {
        q = q.oldest_first();
    }
    q.source = source;
    loop {
        let page = e.db().list::<T>(car, &q)?;
        for row in page.items {
            if !f(row)? {
                return Ok(());
            }
        }
        match page.next {
            Some(c) => q.after = Some(c),
            None => return Ok(()),
        }
    }
}

/// The newest shown drive that started before `t` and ended by then (`ended`), or the newest that started before it.
pub(crate) fn drive_before(e: &Engine, car: CarId, t: u64, shown: &Shown, ended: bool) -> QResult<Option<Drive>> {
    let mut out = None;
    each::<Drive>(e, car, 0, t, None, shown, true, |d| {
        if !ended || d.end_ms <= t {
            out = Some(d);
            return Ok(false);
        }
        Ok(true)
    })?;
    Ok(out)
}

/// The oldest shown drive that started at or after `t`.
pub(crate) fn drive_from(e: &Engine, car: CarId, t: u64, shown: &Shown) -> QResult<Option<Drive>> {
    let mut out = None;
    each::<Drive>(e, car, t, u64::MAX, None, shown, false, |d| {
        out = Some(d);
        Ok(false)
    })?;
    Ok(out)
}

/// The odometer at the start and at the end of `[a, b)`, both null when no data came in it: the period starts once
/// the car's data stopped (`still`, see [`still_unknown`]). A period the data stopped in ends where the data did.
pub(crate) fn odometer_over(odo: &Odometer, a: u64, b: u64, still: Option<u64>) -> (Option<f64>, Option<f64>) {
    let b = still.map_or(b, |t| b.min(t));
    if b <= a {
        return (None, None);
    }
    (odo.at(a), odo.at(b - 1))
}

/// One period of a report: its start and end (clipped to the asked span) and its label.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Slot {
    pub start: u64,
    pub end: u64,
    pub label: String,
}

/// The periods of `[lo, end)` newest first (`All`: the span as one), each clipped to it.
pub(crate) fn slots_back(lo: u64, end: u64, p: P, zone: Tz) -> impl Iterator<Item = Slot> {
    let mut cur = end;
    let mut done = lo >= end;
    std::iter::from_fn(move || {
        if done {
            return None;
        }
        if p == P::All {
            done = true;
            return Some(Slot { start: lo, end, label: "All".into() });
        }
        let (s, e, label) = tz::period_of(cur - 1, p, zone);
        let slot = Slot { start: s.max(lo), end: e.min(end).min(cur), label };
        if s <= lo || s >= cur {
            done = true;
        } else {
            cur = s;
        }
        Some(slot)
    })
}

/// The periods of `[lo, end)` oldest first.
pub(crate) fn slots_forward(lo: u64, end: u64, p: P, zone: Tz) -> Vec<Slot> {
    let mut v: Vec<Slot> = slots_back(lo, end, p, zone).take(MAX_SLOTS).collect();
    v.reverse();
    v
}

/// The slot holding `t`, of slots sorted oldest first.
pub(crate) fn slot_of(slots: &[Slot], t: u64) -> Option<usize> {
    let i = slots.partition_point(|s| s.start <= t).checked_sub(1)?;
    (t < slots[i].end).then_some(i)
}

/// The span `[lo, end)` a car's report covers: from the asked start (or the history's) to the asked end (or now).
fn span_of(e: &Engine, car: CarId, period: &Period) -> QResult<Option<(u64, u64)>> {
    let (a, b) = period.bounds()?;
    let Some(first) = first_ms(e, car)? else { return Ok(None) };
    let (lo, end) = (a.max(first), b.min(crate::now_ms()));
    Ok((lo < end).then_some((lo, end)))
}

fn median(mut v: Vec<f64>) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(f64::total_cmp);
    let n = v.len();
    Some(if n % 2 == 1 { v[n / 2] } else { (v[n / 2 - 1] + v[n / 2]) / 2.0 })
}

/// When the car's data stopped, if its newest state is unknown (no data since: the time up to now is unknown).
pub(crate) fn still_unknown(e: &Engine, car: CarId) -> QResult<Option<u64>> {
    let q = db::Query { source: Some(Source::Live), ..db::Query::default().limit(1) };
    let newest = e.db().list::<StateRow>(car, &q)?;
    Ok(newest.items.first().filter(|s| s.span.state == State::Unknown).map(|s| s.span.start_ms))
}

// ---------------------------------------------------------------- /statistics

/// What a statistics row says about its period's completeness.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Completeness {
    pub recorded_pct: Option<f64>,
    pub unknown_ms: u64,
    pub dropped_stretches: u64,
    pub open_drive: bool,
    pub open_charge: bool,
    pub teslamate_rows: u64,
    /// Odometer readings of the period left out of the car's series ([`odometer`]).
    pub odometer_dropped: u64,
}

/// A price in one currency.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PriceIn {
    pub currency: String,
    pub value: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StatRow {
    pub car: CarRef,
    pub time_zone: String,
    pub period_start_ms: u64,
    pub period_end_ms: u64,
    pub label: String,
    pub time_driven_ms: u64,
    pub drives: u64,
    pub distance_odometer_km: Option<f64>,
    pub distance_logged_km: f64,
    pub outside_avg_c: Option<f64>,
    pub driving_efficiency_pct: Option<f64>,
    pub energy_used_kwh: f64,
    /// Lost while parked (the parked spells that began in the period); null when no spell knows its energy.
    pub energy_standing_kwh: Option<f64>,
    pub energy_added_kwh: f64,
    /// Null when the period has charges but none knows its grid energy.
    pub energy_grid_kwh: Option<f64>,
    pub charges: u64,
    pub energy_per_charge_kwh: Option<f64>,
    pub cost_by_currency: Vec<Money>,
    pub price_per_kwh: Vec<PriceIn>,
    pub cost_per_100km: Vec<Money>,
    /// The charges with a cost (typed, a place's price, free Supercharging, an invoice, an import's).
    pub charges_priced: u64,
    /// The charges without one given an estimate: their place's price as it is now, else the car's
    /// `default_price_per_kwh`. Estimates are never added into `cost_by_currency`.
    pub charges_estimated: u64,
    pub cost_estimated_by_currency: Vec<Money>,
    /// The cost and the estimates together, per currency.
    pub cost_total_by_currency: Vec<Money>,
    /// That total over the logged distance (empty under 1 km) and over the period's days (at least one).
    pub cost_total_per_100km: Vec<Money>,
    pub cost_total_per_day: Vec<Money>,
    pub net_wh_per_km: Option<f64>,
    pub gross_wh_per_km: Option<f64>,
    pub overhead_pct: Option<f64>,
    pub completeness: Completeness,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Statistics {
    /// `day`, `week`, `month`, `year` or `all`.
    pub period: &'static str,
    /// The cars asked for (the several-car answer only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cars: Option<Vec<CarRef>>,
    pub items: Vec<StatRow>,
    pub next: Option<String>,
}

#[derive(Default)]
struct Acc {
    driven_ms: u64,
    drives: u64,
    km: f64,
    kwh: f64,
    temp_w: f64,
    temp_ms: u64,
    charges: u64,
    added: f64,
    grid: f64,
    grid_known: bool,
    cost: BTreeMap<String, f64>,
    basis: BTreeMap<String, f64>,
    priced: u64,
    estimated: u64,
    est: BTreeMap<String, f64>,
    stand: Option<f64>,
    unknown_ms: u64,
    dropped: u64,
    open_drive: bool,
    open_charge: bool,
    tm: u64,
}

fn period_kind(s: &str) -> QResult<(P, &'static str)> {
    Ok(match s {
        "day" => (P::Day, "day"),
        "week" => (P::Week, "week"),
        "month" => (P::Month, "month"),
        "year" => (P::Year, "year"),
        "all" => (P::All, "all"),
        _ => return Err(QueryError::bad("period", "day, week, month, year or all")),
    })
}

/// The statistics rows of one car's `slots` (sorted oldest first), from the rows of their span only.
fn stat_rows(e: &Engine, c: &Car, slots: &[Slot]) -> QResult<Vec<StatRow>> {
    let (Some(first), Some(last)) = (slots.first(), slots.last()) else { return Ok(Vec::new()) };
    let (wa, wb) = (first.start, last.end);
    let car = c.id;
    let shown = Shown::new(e, car, None)?;
    let mut acc: Vec<Acc> = slots.iter().map(|_| Acc::default()).collect();
    let dx = super::drives::ctx(e, c)?;
    // a live drive's mean temperature where only the chosen import has one ([`super::gapfill`])
    let gaps = super::gapfill::Gaps::new(e, car, None)?;
    each::<Drive>(e, car, wa, wb, None, &shown, false, |mut d| {
        if let Some(i) = slot_of(slots, d.start_ms) {
            if d.outside_temp_avg_c.is_none() {
                gaps.drive(&mut d)?;
            }
            let g = &mut acc[i];
            let ms = d.end_ms.saturating_sub(d.start_ms);
            g.drives += 1;
            g.driven_ms += ms;
            g.km += d.distance_km.unwrap_or(0.0);
            g.kwh += d.energy_used_kwh.unwrap_or(0.0);
            if let Some(t) = d.outside_temp_avg_c {
                g.temp_w += t * ms.max(1) as f64;
                g.temp_ms += ms.max(1);
            }
            g.open_drive |= dx.live == Some(d.id) || d.end.is_none();
            g.tm += u64::from(d.source.is_imported());
        }
        Ok(true)
    })?;
    let cx = super::charges::ctx(e, c)?;
    let places = e.places();
    let default_price = super::fleet::settings_of(e, car)?.default_price_per_kwh;
    super::charges::prefill(e, car, wa, wb, &gaps)?;
    each::<Charge>(e, car, wa, wb, None, &shown, false, |ch| {
        if let Some(i) = slot_of(slots, ch.start_ms) {
            let v = super::charges::view_filled(e, c, &ch, &cx, &gaps)?;
            let g = &mut acc[i];
            g.charges += 1;
            g.added += v.energy_added_kwh.unwrap_or(0.0);
            if let Some(x) = v.energy_grid_kwh {
                g.grid += x;
                g.grid_known = true;
            }
            if let Some(cost) = &v.cost {
                *g.cost.entry(cost.currency.clone()).or_default() += cost.amount;
                if let Some(p) = &v.price_per_kwh {
                    let basis =
                        if p.basis == "grid" { v.energy_grid_kwh } else { v.dc_inlet_kwh.or(v.energy_added_kwh) };
                    *g.basis.entry(cost.currency.clone()).or_default() += basis.unwrap_or(0.0);
                }
                g.priced += 1;
            } else if let Some((cur, amount)) = estimate(e, &places, &ch, &v, &c.time_zone, default_price.as_ref()) {
                g.estimated += 1;
                *g.est.entry(cur).or_default() += amount;
            }
            g.open_charge |= v.flags.in_progress;
            g.tm += u64::from(ch.source.is_imported());
        }
        Ok(true)
    })?;
    // unknown time, cut at the periods' edges (no data since the newest state: unknown up to now)
    let still = still_unknown(e, car)?;
    let now = crate::now_ms();
    overlapping::<StateRow>(e, car, wa, wb, None, false, |s| {
        if s.span.state == State::Unknown && still != Some(s.span.start_ms) {
            let (mut x, y) = (s.span.start_ms.max(wa), s.span.end_ms.min(wb));
            while x < y {
                let Some(i) = slot_of(slots, x) else { break };
                let to = y.min(slots[i].end);
                acc[i].unknown_ms += to - x;
                x = to;
            }
        }
        Ok(true)
    })?;
    if let Some(t) = still {
        let (mut x, y) = (t.max(wa), wb.min(now));
        while x < y {
            let Some(i) = slot_of(slots, x) else { break };
            let to = y.min(slots[i].end);
            acc[i].unknown_ms += to - x;
            x = to;
        }
    }
    // logs that lost frames, one day's records at a time (a log is kept under the day it starts; a day either side
    // covers what spills over)
    let meta = crate::records::meta(e.db(), car)?;
    let (d0, d1) = (crate::records::day_of(wa).saturating_sub(1), crate::records::day_of(wb.saturating_sub(1).max(wa)));
    for &d in meta.days.range(d0..=d1) {
        for en in crate::records::day_shared(e.db(), car, d)?.logs.values() {
            if let crate::records::Outcome::Decoded { facts, .. } = &en.outcome
                && facts.dropped > 0
            {
                let (x, y) = en.span();
                if y >= wa
                    && x < wb
                    && let Some(i) = slot_of(slots, facts.span().0.max(wa))
                {
                    acc[i].dropped += 1;
                }
            }
        }
    }
    // energy lost while parked, by the spell's start
    // (only their starts and energies: none is read whole)
    let mut spells = super::parked::spells(e, c, wa, wb, None)?.0;
    // summed newest first, as the parked list gives them
    spells.sort_by(|x, y| cmp_key(&(x.start_ms as f64, x.id.clone()), &(y.start_ms as f64, y.id.clone()), true));
    for s in &spells {
        if let (Some(i), Some(kwh)) = (slot_of(slots, s.start_ms), s.energy_kwh) {
            acc[i].stand = Some(acc[i].stand.unwrap_or(0.0) + kwh);
        }
    }
    let odo = odometer::of(e, car, None)?;
    let mut rows = Vec::with_capacity(slots.len());
    for (s, g) in slots.iter().zip(acc) {
        let (o0, o1) = odometer_over(&odo, s.start, s.end, still);
        let span = s.end.saturating_sub(s.start).max(1);
        let net = (g.km > 0.1).then(|| g.kwh / g.km * 1000.0);
        let gross = g.stand.filter(|_| g.km > 0.1).map(|st| (g.kwh + st) / g.km * 1000.0);
        let price: Vec<PriceIn> = g
            .cost
            .iter()
            .filter_map(|(cur, amt)| {
                let b = g.basis.get(cur).copied().filter(|b| *b > 0.0)?;
                Some(PriceIn { currency: cur.clone(), value: r(Some(amt / b), 4)? })
            })
            .collect();
        let per100: Vec<Money> = if g.km > 1.0 {
            g.cost
                .iter()
                .map(|(cur, amt)| Money { currency: cur.clone(), amount: (amt / g.km * 100.0 * 100.0).round() / 100.0 })
                .collect()
        } else {
            Vec::new()
        };
        let mut total = g.cost.clone();
        for (cur, amt) in &g.est {
            *total.entry(cur.clone()).or_default() += amt;
        }
        let days = (span as f64 / DAY as f64).max(1.0);
        let per = |by: f64| -> Vec<Money> {
            total.iter().map(|(cur, amt)| Money { currency: cur.clone(), amount: cents(amt / by) }).collect()
        };
        let total_per100 = if g.km > 1.0 { per(g.km / 100.0) } else { Vec::new() };
        let total_per_day = per(days);
        rows.push(StatRow {
            car: CarRef(car),
            time_zone: c.time_zone.clone(),
            period_start_ms: s.start,
            period_end_ms: s.end,
            label: s.label.clone(),
            time_driven_ms: g.driven_ms,
            drives: g.drives,
            distance_odometer_km: r(o0.zip(o1).map(|(a, b)| (b - a).max(0.0)), 3),
            distance_logged_km: r(Some(g.km), 3).unwrap_or(0.0),
            outside_avg_c: r((g.temp_ms > 0).then(|| g.temp_w / g.temp_ms as f64), 1),
            driving_efficiency_pct: r(c.efficiency_wh_km.zip(net).map(|(rated, n)| rated / n * 100.0), 1),
            energy_used_kwh: r(Some(g.kwh), 3).unwrap_or(0.0),
            energy_standing_kwh: r(g.stand, 3),
            energy_added_kwh: r(Some(g.added), 3).unwrap_or(0.0),
            energy_grid_kwh: if g.charges == 0 || g.grid_known { r(Some(g.grid), 3) } else { None },
            charges: g.charges,
            energy_per_charge_kwh: r((g.charges > 0).then(|| g.added / g.charges as f64), 3),
            cost_by_currency: money(g.cost),
            price_per_kwh: price,
            cost_per_100km: per100,
            charges_priced: g.priced,
            charges_estimated: g.estimated,
            cost_estimated_by_currency: money(g.est),
            cost_total_by_currency: money(total),
            cost_total_per_100km: total_per100,
            cost_total_per_day: total_per_day,
            net_wh_per_km: r(net, 1),
            gross_wh_per_km: r(gross, 1),
            overhead_pct: r(net.zip(gross).filter(|(n, _)| *n > 0.0).map(|(n, gr)| (gr - n) / n * 100.0), 1),
            completeness: Completeness {
                recorded_pct: r(Some(100.0 * span.saturating_sub(g.unknown_ms) as f64 / span as f64), 2),
                unknown_ms: g.unknown_ms,
                dropped_stretches: g.dropped,
                open_drive: g.open_drive,
                open_charge: g.open_charge,
                teslamate_rows: g.tm,
                odometer_dropped: odo.dropped_in(s.start, s.end),
            },
        });
    }
    Ok(rows)
}

fn cents(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

/// What a charge without a cost is estimated at: its place's price as it is now (a price set after the charge
/// ended), else the car's default price per kWh on the energy from the grid, else the energy added.
fn estimate(
    e: &Engine,
    places: &crate::placing::Places,
    ch: &Charge,
    v: &super::charges::ChargeView,
    car_tz: &str,
    default: Option<&super::fleet::PricePerKwh>,
) -> Option<(String, f64)> {
    if let Some(c) = crate::costs::price_in(e, places, ch, &[], car_tz) {
        return Some((c.currency, c.amount));
    }
    let p = default?;
    let kwh = v.energy_grid_kwh.or(v.energy_added_kwh).filter(|k| *k > 0.0)?;
    Some((p.currency.clone(), cents(kwh * p.value)))
}

/// `/statistics` for the cars given (the several-car answer names them): one row per car and period (day, ISO
/// week, month, year in the car's zone, or `all`), newest first, clipped to the period and to the car's history;
/// drives and charges count by their start. Only the page's periods are read.
pub fn statistics(
    e: &Engine,
    cars: &[CarId],
    period_kind_s: &str,
    period: &Period,
    paging: &Paging,
) -> QResult<Statistics> {
    let (p, word) = period_kind(period_kind_s)?;
    let limit = paging.limit()?;
    let sig = format!("statistics|{cars:?}|{word}|{}", period.sig());
    let after = read_after(paging, &sig)?;
    let past = |k: &(f64, String)| after.as_ref().is_none_or(|a| cmp_key(k, a, true) == Ordering::Greater);
    // the page's periods, from the calendar alone
    let mut best: Best<(CarId, Slot)> = Best::new(limit + 1, true, after.clone());
    let mut found = Vec::new();
    for &car in cars {
        let c = car_of(e, car)?;
        let Some((lo, end)) = span_of(e, car, period)? else { continue };
        let mut n = 0;
        for s in slots_back(lo, end, p, tz::zone(&c.time_zone)).take(MAX_SLOTS) {
            let key = (s.start as f64, car.to_string());
            if !past(&key) {
                continue;
            }
            best.offer(key, (car, s));
            n += 1;
            if n > limit {
                break;
            }
        }
        found.push(c);
    }
    let (page, next) = page_of(best, limit, &sig)?;
    let mut items = Vec::with_capacity(page.len());
    for c in &found {
        let mut slots: Vec<Slot> = page.iter().filter(|(car, _)| *car == c.id).map(|(_, s)| s.clone()).collect();
        slots.reverse();
        items.extend(stat_rows(e, c, &slots)?);
    }
    items.sort_by(|a, b| {
        cmp_key(
            &(a.period_start_ms as f64, a.car.0.to_string()),
            &(b.period_start_ms as f64, b.car.0.to_string()),
            true,
        )
    });
    Ok(Statistics { period: word, cars: Some(cars.iter().map(|&c| CarRef(c)).collect()), items, next })
}

/// `/cars/{car}/statistics`: [`statistics`] of one car, with its zone.
pub fn car_statistics(
    e: &Engine,
    car: CarId,
    period_kind_s: &str,
    period: &Period,
    paging: &Paging,
) -> QResult<Scoped<Statistics>> {
    let c = car_of(e, car)?;
    let mut s = statistics(e, &[car], period_kind_s, period, paging)?;
    s.cars = None;
    Ok(scoped(&c, s))
}

// ---------------------------------------------------------------- /mileage

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MileageRow {
    pub period_start_ms: u64,
    pub period_end_ms: u64,
    pub label: String,
    /// By the odometer.
    pub distance_km: Option<f64>,
    pub odometer_start_km: Option<f64>,
    pub odometer_end_km: Option<f64>,
    /// What the drives logged.
    pub logged_km: f64,
    /// What the car drove in gaps.
    pub not_logged_km: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Projection {
    pub km_per_year: Option<f64>,
    pub basis_days: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Mileage {
    pub group: &'static str,
    pub items: Vec<MileageRow>,
    pub next: Option<String>,
    pub projection: Projection,
}

/// `/cars/{car}/mileage` (`group=day|week|month|year`, default month): oldest first, by odometer and by what the
/// drives logged; the odometer figures are null for a period that starts once the car's data stopped. The
/// projection is over the recorded part of the asked span (up to where the data stopped; at least a week of it).
pub fn mileage(
    e: &Engine,
    car: CarId,
    period: &Period,
    group: Option<&str>,
    paging: &Paging,
) -> QResult<Scoped<Mileage>> {
    let c = car_of(e, car)?;
    let (p, word) = match group.unwrap_or("month") {
        "all" => return Err(QueryError::bad("group", "day, week, month or year")),
        g => period_kind(g).map_err(|_| QueryError::bad("group", "day, week, month or year"))?,
    };
    let limit = paging.limit()?;
    let sig = format!("mileage|{car}|{word}|{}", period.sig());
    let after = read_after(paging, &sig)?;
    let shown = Shown::new(e, car, None)?;
    let Some((lo, end)) = span_of(e, car, period)? else {
        return Ok(scoped(
            &c,
            Mileage {
                group: word,
                items: Vec::new(),
                next: None,
                projection: Projection { km_per_year: None, basis_days: 0.0 },
            },
        ));
    };
    let all = slots_forward(lo, end, p, tz::zone(&c.time_zone));
    let start = after.as_ref().map_or(0, |k| all.partition_point(|s| s.start as f64 <= k.0));
    let page: Vec<Slot> = all.iter().skip(start).take(limit + 1).cloned().collect();
    let more = page.len() > limit;
    let page: Vec<Slot> = page.into_iter().take(limit).collect();
    let mut logged = vec![0.0f64; page.len()];
    if let (Some(f), Some(l)) = (page.first(), page.last()) {
        each::<Drive>(e, car, f.start, l.end, None, &shown, false, |d| {
            if let Some(i) = slot_of(&page, d.start_ms) {
                logged[i] += d.distance_km.unwrap_or(0.0);
            }
            Ok(true)
        })?;
    }
    let still = still_unknown(e, car)?;
    let odo = odometer::of(e, car, None)?;
    let mut items = Vec::with_capacity(page.len());
    for (s, km) in page.iter().zip(logged) {
        let (o0, o1) = odometer_over(&odo, s.start, s.end, still);
        let dist = r(o0.zip(o1).map(|(a, b)| (b - a).max(0.0)), 3);
        items.push(MileageRow {
            period_start_ms: s.start,
            period_end_ms: s.end,
            label: s.label.clone(),
            distance_km: dist,
            odometer_start_km: r(o0, 1),
            odometer_end_km: r(o1, 1),
            logged_km: r(Some(km), 3).unwrap_or(0.0),
            not_logged_km: r(dist.map(|d| (d - km).max(0.0)), 3),
        });
    }
    let next = match items.last() {
        Some(last) if more => Some(super::make_after(&(last.period_start_ms as f64, String::new()), &sig)?),
        _ => None,
    };
    // over the recorded span only: from the start up to where the data stopped, if it has
    let basis_end = still.map_or(end, |t| end.min(t)).max(lo);
    let days = (basis_end - lo) as f64 / DAY as f64;
    let (o0, o1) = odometer_over(&odo, lo, basis_end, None);
    let km_per_year = o0.zip(o1).filter(|_| days >= 7.0).map(|(a, b)| (b - a).max(0.0) / days * 365.25);
    Ok(scoped(
        &c,
        Mileage {
            group: word,
            items,
            next,
            projection: Projection { km_per_year: r(km_per_year, 0), basis_days: r(Some(days), 1).unwrap_or(0.0) },
        },
    ))
}

// ---------------------------------------------------------------- /totals

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct KindTotal {
    pub charges: u64,
    pub kwh: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Totals {
    /// The filter: `smo`, `tesla_api`, `import`, `import:teslamate` or `all` (the old words `can` and `teslamate` are
    /// answered as `smo` and `import:teslamate`).
    pub source: String,
    /// What the filter reads (every input with `all`).
    pub sources: Vec<super::SourceRef>,
    pub drives: u64,
    pub distance_km: f64,
    pub odometer_first_km: Option<f64>,
    pub odometer_last_km: Option<f64>,
    pub odometer_last_ms: Option<u64>,
    /// How far the odometer moved since logging began (`odometer_last_km - odometer_first_km`, never below 0). Both
    /// ends are the valid readings only: the longest run of the drives' readings that never goes down, so a reading
    /// of another car or in another unit (higher or lower than the readings around it) is left out.
    pub driven_since_km: Option<f64>,
    pub not_logged_km: Option<f64>,
    pub charges: u64,
    pub energy_added_kwh: f64,
    pub energy_grid_kwh: f64,
    pub grid_coverage_pct: Option<f64>,
    pub charging_efficiency_pct: Option<f64>,
    pub ac: KindTotal,
    pub dc: KindTotal,
    /// Charges whose kind (AC or DC) no input told: `ac`, `dc` and these add up to `charges`.
    pub unknown_kind: KindTotal,
    pub updates: u64,
}

/// `/cars/{car}/totals` (`source=can|teslamate|all`): the whole history's counts and totals, as the lists show the
/// rows (an imported row inside CAN history counts once). Every row streams through a few counters.
pub fn totals(e: &Engine, car: CarId, source: Option<&str>) -> QResult<Scoped<Totals>> {
    let c = car_of(e, car)?;
    let src = super::parse_source(source)?;
    let (word, sources) = super::filter_words(src);
    let shown = Shown::new(e, car, src)?;
    let dx = super::drives::ctx(e, &c)?;
    let (mut drives, mut km) = (0u64, 0.0f64);
    let mut newest: Option<Drive> = None;
    each::<Drive>(e, car, 0, u64::MAX, src, &shown, false, |d| {
        drives += 1;
        km += d.distance_km.unwrap_or(0.0);
        newest = Some(d);
        Ok(true)
    })?;
    // the first and last readings of the car's clean odometer series ([`odometer`]), not the first drive's
    let odo = odometer::of(e, car, src)?;
    let first = odo.first();
    let last = odo.last().map(|(o, t)| match &newest {
        // the drive going on now: read at this moment
        Some(d) if dx.live == Some(d.id) && t >= d.start_ms => (o, crate::now_ms()),
        _ => (o, t),
    });
    let (mut charges, mut added, mut grid, mut with_grid) = (0u64, 0.0, 0.0, 0u64);
    // the efficiency: over the charges whose two energies may be compared (as each charge's own)
    let (mut added_compared, mut grid_compared) = (0.0, 0.0);
    let (mut ac, mut dc) = (KindTotal { charges: 0, kwh: 0.0 }, KindTotal { charges: 0, kwh: 0.0 });
    let mut unknown_kind = KindTotal { charges: 0, kwh: 0.0 };
    each::<Charge>(e, car, 0, u64::MAX, src, &shown, false, |ch| {
        charges += 1;
        let kwh = ch.energy_added_kwh.unwrap_or(0.0);
        added += kwh;
        if let Some(g) = ch.energy_used_kwh {
            grid += g;
            with_grid += 1;
        }
        if let Some((a, g)) = super::charges::compared_energies(&ch) {
            added_compared += a;
            grid_compared += g;
        }
        let k = match ch.kind {
            Some(model::ChargeKind::Ac) => &mut ac,
            Some(model::ChargeKind::Dc) => &mut dc,
            None => &mut unknown_kind,
        };
        k.charges += 1;
        k.kwh += kwh;
        Ok(true)
    })?;
    // (no overlaps to settle: CAN logs give no updates, so every imported one counts)
    let mut updates = 0u64;
    overlapping::<UpdateRow>(e, car, 0, u64::MAX, src.map(super::SourceFilter::family), false, |_| {
        updates += 1;
        Ok(true)
    })?;
    for k in [&mut ac, &mut dc, &mut unknown_kind] {
        k.kwh = r(Some(k.kwh), 3).unwrap_or(0.0);
    }
    Ok(scoped(
        &c,
        Totals {
            source: word,
            sources,
            drives,
            distance_km: r(Some(km), 3).unwrap_or(0.0),
            odometer_first_km: r(first, 3),
            odometer_last_km: r(last.map(|l| l.0), 3),
            odometer_last_ms: last.map(|l| l.1),
            driven_since_km: r(first.zip(last).map(|(f, l)| (l.0 - f).max(0.0)), 3),
            not_logged_km: r(first.zip(last).map(|(f, l)| (l.0 - f - km).max(0.0)), 3),
            charges,
            energy_added_kwh: r(Some(added), 3).unwrap_or(0.0),
            energy_grid_kwh: r(Some(grid), 3).unwrap_or(0.0),
            grid_coverage_pct: r((charges > 0).then(|| with_grid as f64 / charges as f64 * 100.0), 1),
            charging_efficiency_pct: r((grid_compared > 0.0).then(|| added_compared / grid_compared * 100.0), 1),
            ac,
            dc,
            unknown_kind,
            updates,
        },
    ))
}

// ---------------------------------------------------------------- /gaps

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GapRow {
    pub id: String,
    pub start_ms: u64,
    pub end_ms: Option<u64>,
    pub duration_ms: u64,
    /// How far the odometer moved across the gap (from the drive before to the drive after); null when unknown.
    pub odometer_jump_km: Option<f64>,
    pub from_place: Option<PlaceBrief>,
    pub to_place: Option<PlaceBrief>,
    /// `no_log`, `not_recorded`, `chain_gap`, `unknown`.
    pub cause: &'static str,
    /// The timeline's word: `no_log`, `not_recorded`, `failed_check`, `dropped_frames`.
    pub gap_reason: &'static str,
    #[serde(flatten)]
    pub from: super::Sources,
    /// `{ charge }` for a log missing inside a charge.
    pub inside: Option<serde_json::Value>,
}

/// The reasons the records keep for `[a, b)` (as [`super::timeline::why_unknown`], with the chain gaps worked out
/// once per answer).
fn reasons(e: &Engine, car: CarId, a: u64, b: u64, chain: &[(u64, u64)]) -> QResult<Vec<super::timeline::Why>> {
    use super::timeline::Why;
    use crate::records::{self, Outcome};
    let meta = records::meta(e.db(), car)?;
    let mut out = Vec::new();
    for (_, en) in records::entries_between(e.db(), car, &meta, a, b)? {
        match &en.outcome {
            Outcome::Hole { from_ms, to_ms, reason } => {
                let (reason, cause) =
                    if reason == "not_stored" { ("no_log", "no_log") } else { ("failed_check", "unknown") };
                out.push(Why { from_ms: *from_ms, to_ms: *to_ms, reason, cause });
            }
            Outcome::Decoded { facts, .. } => {
                for n in &facts.not_recorded {
                    let from = n.paused_ms.map_or(n.at_ms.saturating_sub(1), |p| n.at_ms.saturating_sub(p));
                    out.push(Why { from_ms: from, to_ms: n.at_ms, reason: "not_recorded", cause: "not_recorded" });
                }
                if facts.dropped > 0 {
                    let (x, y) = facts.span();
                    out.push(Why { from_ms: x, to_ms: y, reason: "dropped_frames", cause: "unknown" });
                }
            }
        }
    }
    for &(x, y) in chain {
        if y > a && x < b {
            out.push(Why { from_ms: x, to_ms: y, reason: "no_log", cause: "chain_gap" });
        }
    }
    Ok(out)
}

/// Where a run of unknown spans that reaches `y` ends, following the touching unknown spans after it.
fn run_end_after(e: &Engine, car: CarId, y: u64) -> QResult<u64> {
    let mut y = y;
    overlapping::<StateRow>(e, car, y, u64::MAX, Some(Source::Live), false, |s| {
        if s.span.start_ms > y + 1_000 {
            return Ok(false);
        }
        if s.span.state == State::Unknown {
            y = y.max(s.span.end_ms);
            return Ok(true);
        }
        Ok(s.span.end_ms <= y)
    })?;
    Ok(y)
}

/// `/cars/{car}/gaps`: unknown spans of ten minutes or more that start in the period, newest first (the one going on
/// now, when the car's newest state is unknown, has `end_ms` null and its length up to now), with why (from
/// the logs' records and each key's chain), how far the odometer moved across, and the places either side. The
/// states are read newest first, and reading stops once the page is full.
pub fn gaps(e: &Engine, car: CarId, period: &Period, paging: &Paging) -> QResult<Scoped<List<GapRow>>> {
    let c = car_of(e, car)?;
    let (a, b) = period.bounds()?;
    let limit = paging.limit()?;
    let sig = format!("gaps|{car}|{}", period.sig());
    let after = read_after(paging, &sig)?;
    // a gap is a run of touching unknown spans; read newest first below the cursor's start (or the period's end)
    let top = after.as_ref().map_or(b, |k| (k.0 as u64).min(b));
    let mut runs: Vec<(u64, u64)> = Vec::new();
    let mut cur: Option<(u64, u64)> = None;
    // the car's newest state, when it is unknown: no data since (a gap still going, once it is long enough)
    let still = still_unknown(e, car)?;
    let now = crate::now_ms();
    // the newest run may go on past the top: it is followed there (and up to now when it holds the still-unknown
    // newest state) before its length is judged, so a short one never takes a place on the page
    let mut first_seen = false;
    let mut going: Option<u64> = None;
    let mut keep = |run: (u64, u64), runs: &mut Vec<(u64, u64)>| -> QResult<()> {
        let newest = !first_seen;
        first_seen = true;
        if run.0 < a || run.0 >= top {
            return Ok(());
        }
        let mut run = run;
        if newest {
            if run.1 + 1_000 >= top {
                run.1 = run_end_after(e, car, run.1)?;
            }
            if still.is_some_and(|t| run.0 <= t && t <= run.1) {
                going = Some(run.0);
                run.1 = run.1.max(now);
            }
        }
        if run.1 - run.0 >= super::timeline::GAP_MS {
            runs.push(run);
        }
        Ok(())
    };
    overlapping::<StateRow>(e, car, a, top, Some(Source::Live), true, |s| {
        let sp = &s.span;
        if sp.state != State::Unknown {
            if let Some(run) = cur.take() {
                keep(run, &mut runs)?;
            }
            return Ok(runs.len() <= limit);
        }
        cur = match cur {
            Some((x, y)) if sp.end_ms + 1_000 >= x => Some((sp.start_ms.min(x), y.max(sp.end_ms))),
            Some(run) => {
                keep(run, &mut runs)?;
                Some((sp.start_ms, sp.end_ms))
            }
            None => Some((sp.start_ms, sp.end_ms)),
        };
        Ok(runs.len() <= limit)
    })?;
    if runs.len() <= limit
        && let Some(run) = cur.take()
    {
        keep(run, &mut runs)?;
    }
    let more = runs.len() > limit;
    runs.truncate(limit);
    let chain = if runs.is_empty() { Vec::new() } else { super::timeline::chain_gaps(e, car)? };
    let shown = Shown::new(e, car, Some(super::SourceFilter::Live))?;
    let places = e.places();
    let brief = |p: Option<&model::Position>| {
        p.and_then(|p| places.place_at(p.lat, p.lon))
            .map(|pl| PlaceBrief { id: place_ref(pl.id), name: pl.name.clone() })
    };
    let mut items = Vec::with_capacity(runs.len());
    for (x, y) in runs {
        let near = 2_000;
        let why = reasons(e, car, x.saturating_sub(near), y.saturating_add(near), &chain)?;
        let (gap_reason, cause) = super::timeline::classify(&why, x, y);
        let inside = if cause == "chain_gap" {
            let mut found = None;
            overlapping::<Charge>(e, car, x, y, Some(Source::Live), false, |ch| {
                if ch.start_ms <= x && y <= ch.end_ms {
                    found = Some(serde_json::json!({ "charge": ch.id.to_string() }));
                    return Ok(false);
                }
                Ok(true)
            })?;
            found
        } else {
            None
        };
        let before = drive_before(e, car, x, &shown, true)?;
        let after_d = drive_from(e, car, y, &shown)?;
        let jump = before
            .as_ref()
            .and_then(|d| d.end_odometer_km)
            .zip(after_d.as_ref().and_then(|d| d.start_odometer_km))
            .map(|(p, q)| (q - p).max(0.0));
        items.push(GapRow {
            id: format!("gap:{x}"),
            start_ms: x,
            end_ms: if going == Some(x) { None } else { Some(y) },
            duration_ms: y - x,
            odometer_jump_km: r(jump, 3),
            from_place: brief(before.as_ref().and_then(|d| d.end.as_ref())),
            to_place: brief(after_d.as_ref().and_then(|d| d.start.as_ref())),
            cause,
            gap_reason,
            from: super::Sources::family(model::Source::Live),
            inside,
        });
    }
    let next = match items.last() {
        Some(last) if more => Some(super::make_after(&(last.start_ms as f64, last.id.clone()), &sig)?),
        _ => None,
    };
    Ok(scoped(&c, List { items, next, total: None }))
}

// ---------------------------------------------------------------- /compare

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Series {
    pub car: CarRef,
    pub name: String,
    pub time_zone: String,
    pub t_ms: Vec<u64>,
    pub values: Vec<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Compare {
    pub metric: &'static str,
    pub unit: &'static str,
    pub cars: Vec<CarRef>,
    pub series: Vec<Series>,
}

/// The rated range at 100 % a charge's end shows (from its end range and charge level), for ends at 20 % or more.
pub(crate) fn full_range_of(ch: &Charge) -> Option<f64> {
    let (km, soc) = (ch.end_range_km?, ch.end_soc?);
    (soc >= 20.0 && km > 0.0).then(|| km / soc * 100.0)
}

/// `/compare?metric=capacity|full_range|wh_per_km|odometer`: one series per car, one point per local day with data,
/// or per ISO week when the asked span is longer than 120 days. `capacity` is the usable kWh the battery page reads
/// from charges (the median of a period's readings), `full_range` the rated range at 100 % at the end of a period's
/// charges (their median), `wh_per_km` the period's drives of at least `min_distance_km`, `odometer` the odometer at
/// the end of each period the car drove in.
pub fn compare(
    e: &Engine,
    cars: &[CarId],
    metric: Option<&str>,
    period: &Period,
    min_distance_km: Option<f64>,
) -> QResult<Compare> {
    let (metric, unit) = match metric.unwrap_or("capacity") {
        "capacity" => ("capacity", "kWh"),
        "full_range" => ("full_range", "km"),
        "wh_per_km" => ("wh_per_km", "Wh/km"),
        "odometer" => ("odometer", "km"),
        _ => return Err(QueryError::bad("metric", "capacity, full_range, wh_per_km or odometer")),
    };
    period.bounds()?;
    let min_d = min_distance_km.unwrap_or(0.0);
    let mut series = Vec::with_capacity(cars.len());
    for &car in cars {
        let c = car_of(e, car)?;
        let mut s = Series {
            car: CarRef(car),
            name: c.name.clone(),
            time_zone: c.time_zone.clone(),
            t_ms: Vec::new(),
            values: Vec::new(),
        };
        if let Some((lo, end)) = span_of(e, car, period)? {
            let p = if end - lo > 120 * DAY { P::Week } else { P::Day };
            let slots = slots_forward(lo, end, p, tz::zone(&c.time_zone));
            let shown = Shown::new(e, car, None)?;
            let push = |s: &mut Series, i: usize, v: Option<f64>, d: i32| {
                if let Some(v) = r(v, d) {
                    s.t_ms.push(slots[i].start);
                    s.values.push(v);
                }
            };
            match metric {
                "wh_per_km" | "odometer" => {
                    let mut acc: Vec<(f64, f64, Option<f64>, bool)> = vec![(0.0, 0.0, None, false); slots.len()];
                    let odo = if metric == "odometer" { Some(odometer::of(e, car, None)?) } else { None };
                    each::<Drive>(e, car, lo, end, None, &shown, false, |d| {
                        if let Some(i) = slot_of(&slots, d.start_ms) {
                            let g = &mut acc[i];
                            g.3 = true;
                            // the newest clean reading of the day (week), not a drive's own
                            if let Some(o) = odo.as_ref().and_then(|o| o.last_in(slots[i].start, slots[i].end)) {
                                g.2 = Some(o);
                            }
                            let km = d.distance_km.unwrap_or(0.0);
                            if km >= min_d {
                                g.0 += d.energy_used_kwh.unwrap_or(0.0);
                                g.1 += km;
                            }
                        }
                        Ok(true)
                    })?;
                    for (i, g) in acc.into_iter().enumerate() {
                        if metric == "odometer" {
                            push(&mut s, i, g.2.filter(|_| g.3), 1);
                        } else {
                            push(&mut s, i, (g.1 > 1.0).then(|| g.0 / g.1 * 1000.0), 1);
                        }
                    }
                }
                "full_range" => {
                    let mut acc: Vec<Vec<f64>> = vec![Vec::new(); slots.len()];
                    each::<Charge>(e, car, lo, end, None, &shown, false, |ch| {
                        if let (Some(i), Some(v)) = (slot_of(&slots, ch.end_ms.min(end - 1)), full_range_of(&ch)) {
                            acc[i].push(v);
                        }
                        Ok(true)
                    })?;
                    for (i, v) in acc.into_iter().enumerate() {
                        push(&mut s, i, median(v), 1);
                    }
                }
                _ => {
                    let mut acc: Vec<Vec<f64>> = vec![Vec::new(); slots.len()];
                    let mut pg = Paging::first(super::MAX_LIMIT);
                    for _ in 0..200 {
                        let cap = super::battery::capacity(e, car, &Period::new(Some(lo), Some(end)), None, None, &pg)?;
                        for rd in &cap.body.items {
                            if let (Some(i), Some(v)) = (slot_of(&slots, rd.t_ms), rd.usable_kwh) {
                                acc[i].push(v);
                            }
                        }
                        match cap.body.next {
                            Some(n) => pg.after = Some(n),
                            None => break,
                        }
                    }
                    for (i, v) in acc.into_iter().enumerate() {
                        push(&mut s, i, median(v), 2);
                    }
                }
            }
        }
        series.push(s);
    }
    Ok(Compare { metric, unit, cars: cars.iter().map(|&c| CarRef(c)).collect(), series })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn slots_cover_the_span_newest_first_and_clipped() {
        let zone = tz::zone("Europe/Berlin");
        // 2026-03-30 10:00 UTC to 2026-05-02 10:00 UTC
        let (lo, end) = (1_774_864_800_000, 1_777_716_000_000);
        let back: Vec<Slot> = slots_back(lo, end, P::Month, zone).collect();
        assert_eq!(back.iter().map(|s| s.label.as_str()).collect::<Vec<_>>(), ["2026-05", "2026-04", "2026-03"]);
        assert_eq!((back[0].end, back[2].start), (end, lo), "clipped to the span");
        assert!(back.windows(2).all(|w| w[1].end == w[0].start), "touching");
        let fwd = slots_forward(lo, end, P::Day, zone);
        assert_eq!(fwd.len(), 34);
        assert_eq!(slot_of(&fwd, lo), Some(0));
        assert_eq!(slot_of(&fwd, end - 1), Some(33));
        assert_eq!(slot_of(&fwd, end), None);
        let all: Vec<Slot> = slots_back(lo, end, P::All, zone).collect();
        assert_eq!(all, vec![Slot { start: lo, end, label: "All".into() }]);
        assert!(slots_back(end, end, P::Day, zone).next().is_none());
        assert_eq!(median(vec![3.0, 1.0, 2.0, 10.0]), Some(2.5));
    }
}
