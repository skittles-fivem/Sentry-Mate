//! `/cars/{car}/parked`: stays parked off a charger (vampire drain), newest first, clipped to the period.
//!
//! A spell runs from the end of a drive or charge to the start of the next one, wherever that lies (after the
//! period too). Only a spell that no drive or charge follows at all is still going (`end_ms` null, it ends now), and
//! none starts after a drive or charge still going. Its energy is the pack's own lifetime discharge counter across
//! it where the car sends one (`counter`), else the pack's energy remaining (`bms_energy`), else the rated range lost
//! at the car's consumption (`range`, imported history). The values at a spell's ends come from the drive or charge
//! either side, or at a clipped edge from the sample nearest to it; only the days that hold those samples are read
//! (each once), and the page's spells alone are read whole (for their temperatures and awake power). The summary
//! counts the known time of the spells whose energy is known: unknown time (no data) is not time the drain is
//! known for.

use super::system::Shown;
use super::{Paging, Period, PlaceBrief, QResult, Scoped, car_of, paginate, place_ref, r, scoped, source_word};
use crate::Engine;
use crate::pipeline::all_rows;
use db::StateRow;
use model::{Car, CarId, Charge, Drive, Sample, Source};
use serde::Serialize;

const DAY: u64 = 86_400_000;
/// How many days with samples the nearest reading to an edge is looked for in, each way.
const NEAR_DAYS: usize = 3;
/// Days of sample columns a [`Sampler`] keeps.
const KEEP_DAYS: usize = 4;
/// How far outside a row the sample nearest to its edge may be (the drive or charge beside it).
pub(crate) const EDGE_MS: u64 = DAY;
/// A battery colder than this at its coldest makes the spell `cold` (the pack locks part of its energy).
pub const COLD_C: f64 = 2.0;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Spell {
    pub id: String,
    pub car: super::CarRef,
    pub start_ms: u64,
    /// Null while the spell goes on.
    pub end_ms: Option<u64>,
    pub duration_ms: u64,
    pub clipped: bool,
    pub asleep_ms: u64,
    pub awake_ms: u64,
    pub awake_vehicle_bus_only_ms: u64,
    pub unknown_ms: u64,
    pub soc_start_pct: Option<f64>,
    pub soc_end_pct: Option<f64>,
    pub range_start_km: Option<f64>,
    pub range_end_km: Option<f64>,
    pub range_lost_km: Option<f64>,
    pub range_lost_per_hour_km: Option<f64>,
    pub energy_kwh: Option<f64>,
    /// `counter`, `bms_energy` or `range`.
    pub energy_method: &'static str,
    pub avg_power_w: Option<f64>,
    pub awake_avg_power_w: Option<f64>,
    pub battery_min_c: Option<f64>,
    pub outside_avg_c: Option<f64>,
    pub cold: Option<bool>,
    pub place: Option<PlaceBrief>,
    pub address: Option<String>,
    pub awake_reasons: Vec<&'static str>,
    pub source: &'static str,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Summary {
    pub spells: usize,
    pub energy_kwh: Option<f64>,
    pub hours: Option<f64>,
    pub kwh_per_day: Option<f64>,
    pub cost: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Parked {
    pub items: Vec<Spell>,
    pub summary: Summary,
    pub next: Option<String>,
}

/// A drive or charge: what ends one spell and starts the next.
#[derive(Debug, Clone)]
pub(crate) struct Busy {
    pub start: u64,
    pub end: u64,
    pub soc0: Option<f64>,
    pub soc1: Option<f64>,
    pub range0: Option<f64>,
    pub range1: Option<f64>,
    pub at: Option<model::Position>,
    pub address: Option<String>,
    pub source: Source,
    /// Still going (the car's current drive or charge).
    pub live: bool,
}

fn of_drive(d: &Drive, live: Option<u64>) -> Busy {
    Busy {
        start: d.start_ms,
        end: d.end_ms,
        soc0: d.start_soc,
        soc1: d.end_soc,
        range0: d.start_range_km,
        range1: d.end_range_km,
        at: d.end.clone(),
        address: d.end_address.clone(),
        source: d.source,
        live: live == Some(d.id),
    }
}

fn of_charge(ch: &Charge, live: Option<u64>) -> Busy {
    Busy {
        start: ch.start_ms,
        end: ch.end_ms,
        soc0: ch.start_soc,
        soc1: ch.end_soc,
        range0: ch.start_range_km,
        range1: ch.end_range_km,
        at: ch.position.clone(),
        address: ch.address.clone(),
        source: ch.source,
        live: live == Some(ch.id),
    }
}

/// The first shown row of `car` in `[a, b)`, newest or oldest.
fn first_of<T: db::Item>(e: &Engine, car: CarId, a: u64, b: u64, shown: &Shown, newest: bool) -> QResult<Option<T>> {
    let mut out = None;
    super::drives::each::<T>(e, car, a, b, None, shown, newest, |row| {
        out = Some(row);
        Ok(false)
    })?;
    Ok(out)
}

/// The start of the newest drive or charge that started before `a` (its spell may reach into the period).
pub(crate) fn busy_start_before(e: &Engine, car: CarId, shown: &Shown, a: u64) -> QResult<Option<u64>> {
    let d = first_of::<Drive>(e, car, 0, a, shown, true)?.map(|d| d.start_ms);
    let c = first_of::<Charge>(e, car, 0, a, shown, true)?.map(|c| c.start_ms);
    Ok(d.max(c))
}

/// The first drive or charge that starts at or after `b` (it ends the spell the period's last one starts).
pub(crate) fn next_busy(e: &Engine, car: CarId, shown: &Shown, b: u64) -> QResult<Option<Result<Drive, Charge>>> {
    if b == u64::MAX {
        return Ok(None);
    }
    let d = first_of::<Drive>(e, car, b, u64::MAX, shown, false)?;
    let c = first_of::<Charge>(e, car, b, u64::MAX, shown, false)?;
    Ok(match (d, c) {
        (Some(d), Some(c)) if c.start_ms < d.start_ms => Some(Err(c)),
        (Some(d), _) => Some(Ok(d)),
        (None, Some(c)) => Some(Err(c)),
        (None, None) => None,
    })
}

/// The drives and charges a period's spells lie between, oldest first: those starting in it, the last one before
/// it, and the first one after it (`next`).
fn busy_around(e: &Engine, c: &Car, a: u64, b: u64) -> QResult<(Vec<Busy>, Option<Busy>)> {
    let car = c.id;
    let shown = Shown::new(e, car, None)?;
    let live_drive = super::drives::ctx(e, c)?.live;
    let live_charge = super::charges::ctx(e, c)?.live;
    let from = busy_start_before(e, car, &shown, a)?.unwrap_or(a).min(a);
    let per = Period::new(Some(from), Some(b));
    let mut busy: Vec<Busy> = super::drives::rows(e, car, &per, None)?
        .iter()
        .map(|d| of_drive(d, live_drive))
        .chain(super::charges::rows(e, car, &per, None)?.iter().map(|ch| of_charge(ch, live_charge)))
        .collect();
    busy.sort_by_key(|x| x.start);
    let next = next_busy(e, car, &shown, b)?.map(|n| match n {
        Ok(d) => of_drive(&d, live_drive),
        Err(ch) => of_charge(&ch, live_charge),
    });
    Ok((busy, next))
}

/// Some fields of the car's samples in `[a, b)`, a day at a time and only the days that have samples; `f` gets each
/// sample's time and values.
fn each(
    e: &Engine,
    car: CarId,
    source: Source,
    a: u64,
    b: u64,
    names: &[&str],
    mut f: impl FnMut(u64, &[Option<f64>]),
) -> QResult<()> {
    let mut row = vec![None; names.len()];
    for d in e.db().days::<Sample>(car, source)? {
        let (x, y) = (d.start_ms.max(a), d.start_ms.saturating_add(DAY).min(b));
        if y <= x {
            continue;
        }
        let cols = e.db().read_fields::<Sample>(car, source, x, y, names)?;
        for (i, &t) in cols.t_ms.iter().enumerate() {
            for (k, (_, col)) in cols.values.iter().enumerate() {
                row[k] = col.get(i).copied().flatten().filter(|v| v.is_finite());
            }
            f(t, &row);
        }
    }
    Ok(())
}

/// One value per field asked for (null: none).
pub(crate) type Row = Vec<Option<f64>>;

/// A day of some sample fields as columns.
struct Cols {
    t: Vec<u64>,
    v: Vec<Vec<Option<f64>>>,
}

/// The values of some sample fields nearest to given times: each day of columns is read once (the last few are
/// kept), and only days that have samples are looked at.
pub(crate) struct Sampler<'a> {
    e: &'a Engine,
    car: CarId,
    source: Source,
    names: &'static [&'static str],
    days: Vec<u64>,
    cache: Vec<(u64, Cols)>,
}

impl<'a> Sampler<'a> {
    pub(crate) fn new(e: &'a Engine, car: CarId, source: Source, names: &'static [&'static str]) -> QResult<Self> {
        let mut days: Vec<u64> = e.db().days::<Sample>(car, source)?.into_iter().map(|d| d.start_ms).collect();
        days.sort_unstable();
        Ok(Sampler { e, car, source, names, days, cache: Vec::new() })
    }

    fn cols(&mut self, day: u64) -> QResult<usize> {
        if let Some(i) = self.cache.iter().position(|(d, _)| *d == day) {
            return Ok(i);
        }
        let f = self.e.db().read_fields::<Sample>(self.car, self.source, day, day.saturating_add(DAY), self.names)?;
        let cols = Cols {
            v: f.values
                .into_iter()
                .map(|(_, col)| col.into_iter().map(|x| x.filter(|v| v.is_finite())).collect())
                .collect(),
            t: f.t_ms,
        };
        if self.cache.len() >= KEEP_DAYS {
            self.cache.remove(0);
        }
        self.cache.push((day, cols));
        Ok(self.cache.len() - 1)
    }

    /// Each field's value at the sample nearest to `t` (before or after it) within `[lo, hi]`, looked for in the
    /// [`NEAR_DAYS`] days with samples either way.
    pub(crate) fn at(&mut self, t: u64, lo: u64, hi: u64) -> QResult<Row> {
        let k = self.names.len();
        let mut before: Vec<Option<(u64, f64)>> = vec![None; k];
        let mut after: Vec<Option<(u64, f64)>> = vec![None; k];
        // the days that may hold a sample at or before t (newest first), and at or after it (oldest first)
        let n = self.days.partition_point(|&d| d <= t);
        let back: Vec<u64> = self.days[..n].iter().rev().take(NEAR_DAYS).copied().collect();
        let i = self.days.partition_point(|&d| d.saturating_add(DAY) <= t);
        let on: Vec<u64> = self.days[i..].iter().take(NEAR_DAYS).copied().collect();
        for d in back {
            if d.saturating_add(DAY) <= lo || before.iter().all(Option::is_some) {
                break;
            }
            let ci = self.cols(d)?;
            let c = &self.cache[ci].1;
            for idx in (0..c.t.len()).rev() {
                let ts = c.t[idx];
                if ts > t {
                    continue;
                }
                if ts < lo {
                    break;
                }
                for (f, slot) in before.iter_mut().enumerate() {
                    if slot.is_none()
                        && let Some(v) = c.v.get(f).and_then(|col| col.get(idx)).copied().flatten()
                    {
                        *slot = Some((ts, v));
                    }
                }
            }
        }
        for d in on {
            if d > hi || after.iter().all(Option::is_some) {
                break;
            }
            let ci = self.cols(d)?;
            let c = &self.cache[ci].1;
            for idx in 0..c.t.len() {
                let ts = c.t[idx];
                if ts < t {
                    continue;
                }
                if ts > hi {
                    break;
                }
                for (f, slot) in after.iter_mut().enumerate() {
                    if slot.is_none()
                        && let Some(v) = c.v.get(f).and_then(|col| col.get(idx)).copied().flatten()
                    {
                        *slot = Some((ts, v));
                    }
                }
            }
        }
        Ok(before
            .into_iter()
            .zip(after)
            .map(|(x, y)| match (x, y) {
                (Some((tx, vx)), Some((ty, vy))) => Some(if t - tx <= ty - t { vx } else { vy }),
                (Some((_, v)), None) | (None, Some((_, v))) => Some(v),
                (None, None) => None,
            })
            .collect())
    }
}

/// The fields a spell's energy and ends are read from: the pack's discharge counter, its energy remaining, the
/// battery level and the rated range.
pub(crate) const END_FIELDS: [&str; 4] = ["total_discharge_kwh", "energy_remaining_kwh", "soc", "range_km"];

/// A spell's energy over `[x, y)` (kWh lost, positive) and how it was measured: the discharge counter, else the
/// pack's energy remaining, at the samples nearest to the ends (`first` and `last`, as [`END_FIELDS`]); else the range
/// lost at the car's rated consumption.
pub(crate) fn energy_of(
    first: Option<&Row>,
    last: Option<&Row>,
    range0: Option<f64>,
    range1: Option<f64>,
    rated_wh_km: Option<f64>,
) -> (Option<f64>, &'static str) {
    if let (Some(f), Some(l)) = (first, last) {
        let at = |row: &Row, i: usize| row.get(i).copied().flatten();
        if let Some(k) = at(f, 0).zip(at(l, 0)).map(|(d0, d1)| d1 - d0).filter(|k| *k >= 0.0) {
            return (Some(k), "counter");
        }
        if let Some(k) = at(f, 1).zip(at(l, 1)).map(|(e0, e1)| e0 - e1) {
            return (Some(k), "bms_energy");
        }
    }
    let lost = range0.zip(range1).map(|(r0, r1)| r0 - r1);
    (lost.zip(rated_wh_km).map(|(km, wh)| km * wh / 1000.0), "range")
}

/// `/cars/{car}/parked`: `min_hours` leaves out shorter spells (as clipped).
pub fn parked(
    e: &Engine,
    car: CarId,
    period: &Period,
    min_hours: Option<f64>,
    paging: &Paging,
) -> QResult<Scoped<Parked>> {
    let c = car_of(e, car)?;
    let (a, b) = period.bounds()?;
    let now = crate::now_ms();
    let b = b.min(now);
    let (busy, next) = busy_around(e, &c, a, b)?;
    let states: Vec<model::StateSpan> = if a < b {
        all_rows::<StateRow>(e.db(), car, a, b, Some(Source::Can))?.into_iter().map(|s| s.span).collect()
    } else {
        Vec::new()
    };
    let now_soc = e.db().entry(car)?.and_then(|x| x.summary.now).map(|n| n.sample);
    let places = e.places();
    let rated = c.efficiency_wh_km;
    let mut sampler = Sampler::new(e, car, Source::Can, &END_FIELDS)?;
    let mut items = Vec::new();
    // what known time the summary counts: that of the spells whose energy is known
    let (mut known_ms, mut known_kwh) = (0u64, 0.0);
    for i in 0..busy.len() {
        let p = &busy[i];
        let n = busy.get(i + 1).or(next.as_ref());
        let (s0, s1) = match n {
            Some(n) => (p.end, n.start),
            // no spell starts after a drive or charge still going
            None if p.live => continue,
            None => (p.end, now),
        };
        if s1 <= s0 || s1 <= a || s0 >= b {
            continue;
        }
        let (x, y) = (s0.max(a), s1.min(b));
        let dur = y - x;
        if min_hours.is_some_and(|h| (dur as f64) < h * 3_600_000.0) {
            continue;
        }
        // going: nothing follows it, and the period does not end before it does
        let going = n.is_none() && y == s1;
        let sp = super::timeline::split(&states, x, y);
        let source = p.source;
        // the ends: the drive or charge either side, the car's values now for the spell still going, else (at a
        // clipped edge) the sample nearest to it
        let can = source == Source::Can;
        let hi = n.map_or(now, |n| n.end);
        let first = if can { Some(sampler.at(x, p.start, y)?) } else { None };
        let last = if can { Some(sampler.at(y, x, hi)?) } else { None };
        let edge = |row: &Option<Row>, k: usize| row.as_ref().and_then(|v| v.get(k).copied().flatten());
        let (soc0, range0) = if x == s0 { (p.soc1, p.range1) } else { (edge(&first, 2), edge(&first, 3)) };
        let (soc1, range1) = match n {
            Some(n) if y == s1 => (n.soc0, n.range0),
            None if going => (now_soc.as_ref().and_then(|s| s.soc), now_soc.as_ref().and_then(|s| s.range_km)),
            _ => (edge(&last, 2), edge(&last, 3)),
        };
        let (energy, method) = energy_of(first.as_ref(), last.as_ref(), range0, range1, rated);
        let lost = range0.zip(range1).map(|(r0, r1)| r0 - r1);
        let hours = dur as f64 / 3_600_000.0;
        if let Some(k) = energy {
            known_ms += dur - sp.unknown.min(dur);
            known_kwh += k;
        }
        let place = p.at.as_ref().and_then(|pos| places.place_at(pos.lat, pos.lon));
        items.push(Spell {
            id: format!("parked:{s0}"),
            car: super::CarRef(car),
            start_ms: x,
            end_ms: (!going).then_some(y),
            duration_ms: dur,
            clipped: x > s0 || y < s1,
            asleep_ms: sp.asleep,
            awake_ms: sp.awake,
            awake_vehicle_bus_only_ms: sp.awake_vehicle_bus_only,
            unknown_ms: sp.unknown,
            soc_start_pct: r(soc0, 1),
            soc_end_pct: r(soc1, 1),
            range_start_km: r(range0, 1),
            range_end_km: r(range1, 1),
            range_lost_km: r(lost, 1),
            range_lost_per_hour_km: r(lost.filter(|_| dur > 60_000).map(|l| l / hours), 2),
            energy_kwh: r(energy, 3),
            energy_method: method,
            avg_power_w: r(energy.filter(|_| dur > 60_000).map(|k| k * 1000.0 / hours), 0),
            awake_avg_power_w: None,
            battery_min_c: None,
            outside_avg_c: None,
            cold: None,
            place: place.map(|pl| PlaceBrief { id: place_ref(pl.id), name: pl.name.clone() }),
            address: p.address.clone(),
            // the only reason the decoded signals give yet is charging, and a spell holds no charge
            awake_reasons: Vec::new(),
            source: source_word(source),
        });
    }
    let hours = known_ms as f64 / 3_600_000.0;
    let summary = Summary {
        spells: items.len(),
        energy_kwh: r(Some(known_kwh), 3),
        hours: r(Some(hours), 1),
        kwh_per_day: r((known_ms > 3_600_000).then(|| known_kwh / hours * 24.0), 3),
        cost: None,
    };
    let sig = format!("parked|{car}|{}|{min_hours:?}", period.sig());
    let page = paginate(items, |s| (s.start_ms as f64, s.id.clone()), true, &sig, paging, false)?;
    let mut out = page.items;
    // the page's spells read whole: battery and outside temperatures, the pack's power while awake
    for s in &mut out {
        if s.source != "can" {
            continue;
        }
        let end = s.start_ms.saturating_add(s.duration_ms);
        let (mut bmin, mut out_sum, mut out_n, mut kw_sum, mut kw_n) = (None::<f64>, 0.0, 0u64, 0.0, 0u64);
        each(
            e,
            car,
            Source::Can,
            s.start_ms,
            end,
            &["battery_temp_c", "outside_temp_c", "battery_power_kw"],
            |_, v| {
                if let Some(t) = v[0] {
                    bmin = Some(bmin.map_or(t, |m: f64| m.min(t)));
                }
                if let Some(t) = v[1] {
                    out_sum += t;
                    out_n += 1;
                }
                if let Some(k) = v[2] {
                    kw_sum += k;
                    kw_n += 1;
                }
            },
        )?;
        s.battery_min_c = r(bmin, 1);
        s.outside_avg_c = r((out_n > 0).then(|| out_sum / out_n as f64), 1);
        s.cold = bmin.map(|t| t < COLD_C);
        // samples come only while the car is awake: their mean power is the awake power
        s.awake_avg_power_w = r((kw_n > 0).then(|| kw_sum / kw_n as f64 * 1000.0), 0);
    }
    Ok(scoped(&c, Parked { items: out, summary, next: page.next }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn energy_from_the_counter_then_the_pack_then_the_range() {
        let row = |d: Option<f64>, e: Option<f64>| -> Row { vec![d, e, None, None] };
        let (k, m) =
            energy_of(Some(&row(Some(10.0), Some(50.0))), Some(&row(Some(10.5), Some(49.0))), None, None, None);
        assert_eq!((k, m), (Some(0.5), "counter"));
        let (k, m) = energy_of(Some(&row(None, Some(50.0))), Some(&row(None, Some(49.0))), None, None, None);
        assert_eq!((k, m), (Some(1.0), "bms_energy"));
        let (k, m) = energy_of(None, None, Some(300.0), Some(290.0), Some(150.0));
        assert_eq!((k, m), (Some(1.5), "range"));
        assert_eq!(energy_of(None, None, Some(300.0), None, Some(150.0)), (None, "range"));
    }
}
