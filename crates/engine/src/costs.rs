//! What charges cost. A charge is priced at its place's price (one price or time-of-use rates in the place's time zone,
//! else the car's; a session fee and a price per minute where set) from its curve, the energy taken from the grid
//! where known. Once a charge has ended its cost is kept ([`records::CostEntry`]) and stays put: rebuilding the
//! charge, or changing the place's price, does not change it. A price change reaches past charges only through
//! [`reprice`], and a cost typed in ([`set_manual`]) is never repriced.

use crate::query::{CarRef, QueryError};
use crate::records::{self, CostEntry, CostPart};
use crate::{Engine, Result, now_ms};
use model::{CarId, Charge, ChargePoint, Source};
use serde::Serialize;

/// The cost a charge has now (kept, else worked out from its place's price), and keeps it once `ended`.
pub fn settle(
    e: &Engine,
    car: CarId,
    ch: &Charge,
    curve: &[ChargePoint],
    car_tz: &str,
    ended: bool,
) -> Result<Option<CostEntry>> {
    let db = &e.inner.db;
    if let Some(kept) = records::cost(db, car, ch.id, ch.start_ms)? {
        return Ok(Some(kept));
    }
    let Some(c) = price_now(e, ch, curve, car_tz) else { return Ok(None) };
    if ended {
        records::put_cost(db, car, ch.id, ch.start_ms, Some(c.clone()))?;
    }
    Ok(Some(c))
}

/// Keeps the costs of a window's charges when it is rebuilt: a charge's id comes from its start, so a late log that
/// moves the start (or new rules) gives it a new id. Each kept cost of an old charge that is gone goes to the new
/// charge it overlaps most (a typed cost first, then the larger overlap), unless that one has a cost kept already;
/// a cost with no new charge to go to stays where it was (the charge may come back).
pub(crate) fn carry(e: &Engine, car: CarId, old: &[Charge], new: &[Charge]) -> Result<()> {
    let db = &e.inner.db;
    let new_ids: std::collections::HashSet<u64> = new.iter().map(|c| c.id).collect();
    let overlap = |a: &Charge, b: &Charge| {
        let (x, y) = (a.start_ms.max(b.start_ms), a.end_ms.max(a.start_ms + 1).min(b.end_ms.max(b.start_ms + 1)));
        y.saturating_sub(x)
    };
    let mut moves: Vec<(bool, u64, &Charge, &Charge, CostEntry)> = Vec::new();
    for o in old.iter().filter(|o| !new_ids.contains(&o.id) && o.source == Source::Can) {
        let Some(kept) = records::cost(db, car, o.id, o.start_ms)? else { continue };
        let best = new.iter().map(|n| (overlap(o, n), n)).filter(|(v, _)| *v > 0).max_by_key(|(v, _)| *v);
        if let Some((v, n)) = best {
            moves.push((kept.source != "manual", u64::MAX - v, o, n, kept));
        }
    }
    moves.sort_by_key(|m| (m.0, m.1));
    let mut taken = std::collections::HashSet::new();
    for (_, _, o, n, kept) in moves {
        if !taken.insert(n.id) || records::cost(db, car, n.id, n.start_ms)?.is_some() {
            continue;
        }
        records::put_cost(db, car, n.id, n.start_ms, Some(kept))?;
        records::put_cost(db, car, o.id, o.start_ms, None)?;
    }
    Ok(())
}

/// The cost at the place's price as it is now (nothing kept, nothing written).
pub fn price_now(e: &Engine, ch: &Charge, curve: &[ChargePoint], car_tz: &str) -> Option<CostEntry> {
    let places = e.places();
    let pos = ch.position.as_ref()?;
    let place = places.place_at(pos.lat, pos.lon)?;
    let plan = place.price.as_ref()?;
    let extra = places.extra(place.id);
    let tz = extra.and_then(|x| x.time_zone.as_deref()).unwrap_or(car_tz);
    let tariff = places::Tariff::new(plan, tz).or_else(|_| places::Tariff::new(plan, "UTC")).ok()?;
    let per_minute = extra.and_then(|x| x.per_minute).unwrap_or(0.0);
    let minutes = ch.end_ms.saturating_sub(ch.start_ms) as f64 / 60_000.0;
    let (total, parts, _fee, gap_ms) = if curve.len() >= 2 {
        let opts = places::CostOptions { grid_kwh: ch.energy_used_kwh, ..places::CostOptions::default() };
        let c = tariff.charge_cost(curve, &opts);
        let parts = c
            .parts
            .iter()
            .map(|p| CostPart { rate: p.rate.clone(), per_kwh: p.per_kwh, kwh: p.kwh, cost: p.cost })
            .collect();
        (c.total, parts, c.session_fee, c.gap_ms)
    } else {
        let kwh = ch.energy_used_kwh.or(ch.energy_added_kwh)?;
        let fee = plan.session_fee.unwrap_or(0.0);
        let cost = tariff.cost_of(kwh, ch.start_ms);
        let part = CostPart {
            rate: tariff.rate_name_at(ch.start_ms).map(str::to_string),
            per_kwh: tariff.price_at(ch.start_ms),
            kwh,
            cost,
        };
        (cost + fee, vec![part], fee, 0)
    };
    let amount = total + per_minute * minutes;
    if !amount.is_finite() {
        return None;
    }
    Some(CostEntry {
        amount: round2(amount),
        currency: tariff.currency().to_string(),
        source: "tariff".into(),
        mode: None,
        entered: None,
        energy_basis: Some(if ch.energy_used_kwh.is_some() { "grid" } else { "pack" }.into()),
        place: Some(place.id),
        parts,
        session_fee: plan.session_fee,
        gap_ms,
        at_ms: now_ms(),
    })
}

pub(crate) fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

/// How a typed cost is given.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Total,
    PerKwh,
    PerMinute,
}

impl Mode {
    pub fn parse(s: &str) -> Option<Mode> {
        match s {
            "total" => Some(Mode::Total),
            "per_kwh" => Some(Mode::PerKwh),
            "per_minute" => Some(Mode::PerMinute),
            _ => None,
        }
    }

    fn word(self) -> &'static str {
        match self {
            Mode::Total => "total",
            Mode::PerKwh => "per_kwh",
            Mode::PerMinute => "per_minute",
        }
    }
}

/// A cost typed on a charge (`PUT .../cost`): kept across rebuilds and never repriced. `grid_basis`: the energy a
/// per-kWh amount is over is the grid's (else the pack's).
pub fn set_manual(
    e: &Engine,
    car: CarId,
    charge: u64,
    mode: Mode,
    amount: f64,
    currency: &str,
    grid_basis: bool,
) -> Result<CostEntry, QueryError> {
    let db = &e.inner.db;
    let ch: Charge = db.get(car, charge)?.ok_or(QueryError::NotFound("no_such_charge"))?;
    if !amount.is_finite() || amount < 0.0 {
        return Err(QueryError::invalid("amount", "a cost is a number of 0 or more"));
    }
    let basis_kwh = if grid_basis { ch.energy_used_kwh.or(ch.energy_added_kwh) } else { ch.energy_added_kwh };
    let minutes = ch.end_ms.saturating_sub(ch.start_ms) as f64 / 60_000.0;
    let total = match mode {
        Mode::Total => amount,
        Mode::PerKwh => amount * basis_kwh.unwrap_or(0.0),
        Mode::PerMinute => amount * minutes,
    };
    let c = CostEntry {
        amount: round2(total),
        currency: currency.to_string(),
        source: "manual".into(),
        mode: Some(mode.word().into()),
        entered: Some(amount),
        energy_basis: Some(if grid_basis { "grid" } else { "pack" }.into()),
        place: None,
        parts: Vec::new(),
        session_fee: None,
        gap_ms: 0,
        at_ms: now_ms(),
    };
    records::put_cost(db, car, charge, ch.start_ms, Some(c.clone()))?;
    e.emit(crate::Event::Rows {
        car,
        kinds: vec!["charges"],
        from_ms: ch.start_ms,
        to_ms: ch.end_ms.max(ch.start_ms + 1),
    });
    Ok(c)
}

/// Takes a typed cost off: the charge goes back to its place's price (as it is now).
pub fn clear_manual(e: &Engine, car: CarId, charge: u64) -> Result<Option<CostEntry>, QueryError> {
    let db = &e.inner.db;
    let ch: Charge = db.get(car, charge)?.ok_or(QueryError::NotFound("no_such_charge"))?;
    records::put_cost(db, car, charge, ch.start_ms, None)?;
    let tz = db.car(car)?.map_or_else(|| "UTC".to_string(), |c| c.time_zone);
    let curve = curve_of(e, car, &ch)?;
    let c = settle(e, car, &ch, &curve, &tz, true)?;
    e.emit(crate::Event::Rows {
        car,
        kinds: vec!["charges"],
        from_ms: ch.start_ms,
        to_ms: ch.end_ms.max(ch.start_ms + 1),
    });
    Ok(c)
}

pub(crate) fn curve_of(e: &Engine, car: CarId, ch: &Charge) -> Result<Vec<ChargePoint>> {
    Ok(e.inner.db.read_points::<ChargePoint>(car, ch.source, ch.start_ms, ch.end_ms.max(ch.start_ms) + 1)?)
}

/// What [`reprice`] did (or would do, `dry_run`).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Repriced {
    pub place: String,
    pub from_ms: Option<u64>,
    pub to_ms: Option<u64>,
    pub only_without_cost: bool,
    pub dry_run: bool,
    pub charges: u64,
    pub repriced: u64,
    pub unchanged: u64,
    pub examples: Vec<String>,
    pub example_rows: Vec<serde_json::Value>,
    pub cost_before: Vec<crate::query::Money>,
    pub cost_after: Vec<crate::query::Money>,
}

/// Applies the place's price as it is now to past charges there (`POST /places/{place}/reprice`), for the cars given.
/// Typed costs are never changed; `only_without_cost` changes only charges without one.
pub fn reprice(
    e: &Engine,
    place: model::PlaceId,
    cars: &[CarId],
    from_ms: Option<u64>,
    to_ms: Option<u64>,
    only_without_cost: bool,
    dry_run: bool,
) -> Result<Repriced, QueryError> {
    let db = &e.inner.db;
    let places = e.places();
    if places.get(place).is_none() {
        return Err(QueryError::NotFound("no_such_place"));
    }
    let mut out = Repriced {
        place: crate::query::place_ref(place),
        from_ms,
        to_ms,
        only_without_cost,
        dry_run,
        ..Repriced::default()
    };
    let mut before: std::collections::BTreeMap<String, f64> = Default::default();
    let mut after: std::collections::BTreeMap<String, f64> = Default::default();
    for &car in cars {
        let tz = db.car(car)?.map_or_else(|| "UTC".to_string(), |c| c.time_zone);
        let rows: Vec<Charge> =
            crate::pipeline::all_rows(db, car, from_ms.unwrap_or(0), to_ms.unwrap_or(u64::MAX), None)?;
        for ch in rows {
            let here = ch.position.as_ref().and_then(|p| places.place_at(p.lat, p.lon)).map(|p| p.id);
            if here != Some(place) || ch.source != Source::Can {
                continue;
            }
            out.charges += 1;
            let kept = records::cost(db, car, ch.id, ch.start_ms)?;
            if kept.as_ref().is_some_and(|k| k.source == "manual") || only_without_cost && kept.is_some() {
                out.unchanged += 1;
                continue;
            }
            let curve = curve_of(e, car, &ch)?;
            let new = price_now(e, &ch, &curve, &tz);
            if new.as_ref().map(|c| (c.amount, &c.currency)) == kept.as_ref().map(|c| (c.amount, &c.currency)) {
                out.unchanged += 1;
                continue;
            }
            if let Some(k) = &kept {
                *before.entry(k.currency.clone()).or_default() += k.amount;
            }
            if let Some(n) = &new {
                *after.entry(n.currency.clone()).or_default() += n.amount;
            }
            out.repriced += 1;
            if out.examples.len() < 8 {
                out.examples.push(ch.id.to_string());
                out.example_rows.push(serde_json::json!({
                    "kind": "charge", "id": ch.id.to_string(), "car": CarRef(car), "start_ms": ch.start_ms,
                    "change": "repriced",
                }));
            }
            if !dry_run {
                records::put_cost(db, car, ch.id, ch.start_ms, new)?;
            }
        }
        if !dry_run && out.repriced > 0 {
            e.emit(crate::Event::Rows {
                car,
                kinds: vec!["charges"],
                from_ms: from_ms.unwrap_or(0),
                to_ms: to_ms.unwrap_or(u64::MAX),
            });
        }
    }
    let money = |m: std::collections::BTreeMap<String, f64>| {
        m.into_iter().map(|(currency, amount)| crate::query::Money { currency, amount: round2(amount) }).collect()
    };
    out.cost_before = money(before);
    out.cost_after = money(after);
    Ok(out)
}
