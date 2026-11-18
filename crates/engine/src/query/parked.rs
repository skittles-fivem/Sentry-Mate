//! `/cars/{car}/parked`: stays parked off a charger (vampire drain), newest first, clipped to the period.
//!
//! A spell runs from the end of a drive or charge to the start of the next one (the spell still going ends now). Its
//! energy is the pack's own lifetime discharge counter across it where the car sends one (`counter`), else the
//! pack's energy remaining (`bms_energy`), else the rated range lost at the car's consumption (`range`, imported
//! history). Only the samples near a spell's ends are read for it, and the page's spells alone are read whole (for
//! their temperatures and awake power).

use super::{Paging, Period, PlaceBrief, QResult, Scoped, car_of, paginate, place_ref, r, scoped, source_word};
use crate::Engine;
use crate::pipeline::all_rows;
use db::StateRow;
use model::{CarId, Sample, Source};
use serde::Serialize;

const DAY: u64 = 86_400_000;
/// How far into a spell its first reading, and before its end its last, may be.
const END_READ_MS: u64 = 30 * 60_000;
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
struct Busy {
    start: u64,
    end: u64,
    soc0: Option<f64>,
    soc1: Option<f64>,
    range0: Option<f64>,
    range1: Option<f64>,
    at: Option<model::Position>,
    address: Option<String>,
    source: Source,
}

/// Some fields of the car's samples in `[a, b)`, a day at a time; `f` gets each sample's time and values.
fn each(
    e: &Engine,
    car: CarId,
    source: Source,
    a: u64,
    b: u64,
    names: &[&str],
    mut f: impl FnMut(u64, &[Option<f64>]),
) -> QResult<()> {
    let mut x = a;
    let mut row = vec![None; names.len()];
    while x < b {
        let y = (x / DAY + 1).saturating_mul(DAY).min(b);
        let cols = e.db().read_fields::<Sample>(car, source, x, y, names)?;
        for (i, &t) in cols.t_ms.iter().enumerate() {
            for (k, (_, col)) in cols.values.iter().enumerate() {
                row[k] = col.get(i).copied().flatten().filter(|v| v.is_finite());
            }
            f(t, &row);
        }
        x = y;
    }
    Ok(())
}

/// One value per field asked for (null: none).
type Row = Vec<Option<f64>>;

/// The values of `names` nearest to `at` (each field its own), from the samples within [`END_READ_MS`] of it: at a
/// spell's edge, the drive or charge beside it still has the reading.
fn nearest(e: &Engine, car: CarId, source: Source, at: u64, names: &[&str]) -> QResult<Row> {
    let mut best: Vec<Option<(u64, f64)>> = vec![None; names.len()];
    each(e, car, source, at.saturating_sub(END_READ_MS), at.saturating_add(END_READ_MS), names, |t, row| {
        let d = t.abs_diff(at);
        for (k, v) in row.iter().enumerate() {
            if let Some(v) = v
                && best[k].is_none_or(|(bd, _)| d < bd)
            {
                best[k] = Some((d, *v));
            }
        }
    })?;
    Ok(best.into_iter().map(|b| b.map(|(_, v)| v)).collect())
}

/// The values of `names` at both ends of `[a, b)`.
fn ends(e: &Engine, car: CarId, source: Source, a: u64, b: u64, names: &[&str]) -> QResult<(Row, Row)> {
    Ok((nearest(e, car, source, a, names)?, nearest(e, car, source, b, names)?))
}

const END_FIELDS: [&str; 2] = ["total_discharge_kwh", "energy_remaining_kwh"];

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
    let wide = Period::new(Some(a.saturating_sub(30 * DAY)), Some(b));
    let mut busy: Vec<Busy> = super::drives::rows(e, car, &wide, None)?
        .into_iter()
        .map(|d| Busy {
            start: d.start_ms,
            end: d.end_ms,
            soc0: d.start_soc,
            soc1: d.end_soc,
            range0: d.start_range_km,
            range1: d.end_range_km,
            at: d.end.clone(),
            address: d.end_address.clone(),
            source: d.source,
        })
        .chain(super::charges::rows(e, car, &wide, None)?.into_iter().map(|ch| Busy {
            start: ch.start_ms,
            end: ch.end_ms,
            soc0: ch.start_soc,
            soc1: ch.end_soc,
            range0: ch.start_range_km,
            range1: ch.end_range_km,
            at: ch.position.clone(),
            address: ch.address.clone(),
            source: ch.source,
        }))
        .collect();
    busy.sort_by_key(|x| x.start);
    // the spell still going: from the last drive or charge to now
    let tail = busy.last().map(|l| l.end).filter(|&end| end < now);
    let states: Vec<model::StateSpan> =
        all_rows::<StateRow>(e.db(), car, a, b, Some(Source::Can))?.into_iter().map(|s| s.span).collect();
    let now_soc = e.db().entry(car)?.and_then(|x| x.summary.now).map(|n| n.sample);
    let places = e.places();
    let rated = c.efficiency_wh_km;
    let mut items = Vec::new();
    for i in 0..busy.len() {
        let p = &busy[i];
        let n = busy.get(i + 1);
        let (s0, s1) = match n {
            Some(n) => (p.end, n.start),
            None => match tail {
                Some(t) => (t, now),
                None => continue,
            },
        };
        if s1 <= s0 || s1 <= a || s0 >= b {
            continue;
        }
        let (x, y) = (s0.max(a), s1.min(b));
        let dur = y - x;
        if min_hours.is_some_and(|h| (dur as f64) < h * 3_600_000.0) {
            continue;
        }
        let going = n.is_none();
        let sp = super::timeline::split(&states, x, y);
        let source = p.source;
        // the ends: the drive or charge either side, or for a clipped spell the samples at the period's edges
        let (soc0, range0) = (p.soc1, p.range1);
        let (soc1, range1) = match n {
            Some(n) => (n.soc0, n.range0),
            None => (now_soc.as_ref().and_then(|s| s.soc), now_soc.as_ref().and_then(|s| s.range_km)),
        };
        let (mut energy, mut method) = (None, "range");
        if source == Source::Can {
            let (first, last) = ends(e, car, Source::Can, x, y, &END_FIELDS)?;
            let counter = first[0].zip(last[0]).map(|(d0, d1)| d1 - d0).filter(|k| *k >= 0.0);
            let bms = first[1].zip(last[1]).map(|(e0, e1)| e0 - e1);
            if let Some(k) = counter {
                (energy, method) = (Some(k), "counter");
            } else if let Some(k) = bms {
                (energy, method) = (Some(k), "bms_energy");
            }
        }
        let lost = range0.zip(range1).map(|(r0, r1)| r0 - r1);
        if energy.is_none() {
            energy = lost.zip(rated).map(|(km, wh)| km * wh / 1000.0);
        }
        let hours = dur as f64 / 3_600_000.0;
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
            awake_reasons: Vec::new(),
            source: source_word(source),
        });
    }
    let total_kwh: f64 = items.iter().filter_map(|s| s.energy_kwh).sum();
    let ms: u64 = items.iter().map(|s| s.duration_ms).sum();
    let hours = ms as f64 / 3_600_000.0;
    let summary = Summary {
        spells: items.len(),
        energy_kwh: r(Some(total_kwh), 3),
        hours: r(Some(hours), 1),
        kwh_per_day: r((ms > 3_600_000).then(|| total_kwh / hours * 24.0), 3),
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
        let end = s.end_ms.unwrap_or(b);
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
