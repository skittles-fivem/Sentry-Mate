//! Place prices from Supercharger invoices: for each place where invoiced sessions were matched to charges, a dated
//! price marked `invoice` ([`PriceSource::Invoice`]): the median price per kWh billed (Tesla's `rateBase`) over the
//! place's recent sessions, in their currency, from the day of the earliest of them at that price.
//!
//! - **Where a session was**: Tesla's charging history gives the time and the site's name, no position, so a
//!   session is at the place of the charge it was matched to.
//! - **Worked out again from every session** each time ([`update`]): the sessions in time order, a window of the
//!   [`RECENT`] newest up to each one, and a new dated price whenever the window's median changes. So the place keeps
//!   the price of each stretch (charges before a change keep the older price), and newer invoices that move the
//!   median add a price from their day on. The same sessions give the same prices, however they arrived.
//! - **A typed price always wins** ([`crate::placing::price_on`]): an invoice price is used only on days no price an
//!   admin typed covers, and is never written over one. Charges at the place with no cost then get it (a reprice of
//!   the charges without a cost); costs kept already stay put.
//!
//! The sessions come through [`InvoiceRates`], so this does not depend on how invoices are read or kept.

use crate::placing::{DatedPrice, InvoiceBasis, PriceSource};
use crate::query::places::Price;
use crate::{Engine, Result};
use model::{CarId, Charge, PlaceId};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap};

/// The median is taken over this many of a place's newest sessions.
pub const RECENT: usize = 10;

/// One invoiced session matched to a charge, with the price per kWh it was billed at.
#[derive(Debug, Clone, PartialEq)]
pub struct InvoiceRate {
    pub car: CarId,
    /// The charge the session was matched to (its place is the session's).
    pub charge: u64,
    /// When the session started.
    pub start_ms: u64,
    /// ISO 4217.
    pub currency: String,
    /// The price per kWh billed for charging (`rateBase`).
    pub per_kwh: f64,
}

/// Where the invoiced sessions come from: every session matched to a charge that has a price per kWh.
pub trait InvoiceRates {
    fn invoice_rates(&self, e: &Engine) -> Result<Vec<InvoiceRate>>;
}

/// A fixed list (tests, and an input that has its sessions at hand).
impl InvoiceRates for [InvoiceRate] {
    fn invoice_rates(&self, _e: &Engine) -> Result<Vec<InvoiceRate>> {
        Ok(self.to_vec())
    }
}

impl InvoiceRates for Vec<InvoiceRate> {
    fn invoice_rates(&self, e: &Engine) -> Result<Vec<InvoiceRate>> {
        self.as_slice().invoice_rates(e)
    }
}

/// A price per kWh as kept (5 decimals, as a typed price).
fn round5(v: f64) -> f64 {
    (v * 100_000.0).round() / 100_000.0
}

/// The dated invoice prices of one place's sessions: `(local day, price per kWh)` in time order, all in `currency`.
/// The first window is the first [`RECENT`] sessions (all of them, when fewer); each later session moves it on by one.
/// The median of an even count is the lower middle one, so it is always a price some session was billed at.
pub fn derive(sessions: &[(String, f64)], currency: &str) -> Vec<DatedPrice> {
    let n = sessions.len();
    if n == 0 {
        return Vec::new();
    }
    let first = RECENT.min(n) - 1;
    let mut out: Vec<(String, f64, u32)> = Vec::new();
    for i in first..n {
        let window = &sessions[(i + 1).saturating_sub(RECENT)..=i];
        let mut rates: Vec<f64> = window.iter().map(|s| round5(s.1)).collect();
        rates.sort_by(f64::total_cmp);
        let median = rates[(rates.len() - 1) / 2];
        if out.last().is_some_and(|l| l.1 == median) {
            continue;
        }
        // the earliest session of the window at that price (one always is)
        let Some(from) = window.iter().find(|s| round5(s.1) == median).map(|s| s.0.clone()) else { continue };
        // a change that reaches back over the last one's day takes its place
        while out.last().is_some_and(|l| l.0 >= from) {
            out.pop();
        }
        if out.last().is_some_and(|l| l.1 == median) {
            continue;
        }
        out.push((from, median, window.len() as u32));
    }
    out.into_iter()
        .map(|(from, per_kwh, sessions)| DatedPrice {
            from: Some(from),
            source: PriceSource::Invoice,
            price: Price {
                currency: currency.to_string(),
                per_kwh: Some(per_kwh),
                per_minute: None,
                rates: Vec::new(),
                session_fee: None,
                tiers: None,
            },
            invoice: Some(InvoiceBasis { sessions }),
        })
        .collect()
}

/// What [`update`] did.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Updated {
    /// Places whose invoice prices changed.
    pub places: Vec<String>,
    /// Charges without a cost that got one from them.
    pub charges_priced: u64,
}

/// Works out every place's invoice prices from the sessions `src` gives and keeps those that changed (a place
/// without sessions any more loses them), then prices the charges there that have no cost.
pub fn update(e: &Engine, src: &dyn InvoiceRates) -> crate::query::QResult<Updated> {
    let places = e.places();
    let mut zones: HashMap<CarId, String> = HashMap::new();
    // place -> (start, local day, currency, price), from the charge each session went to
    let mut at: BTreeMap<PlaceId, Vec<(u64, String, String, f64)>> = BTreeMap::new();
    for r in src.invoice_rates(e)? {
        let currency = r.currency.trim().to_ascii_uppercase();
        if !r.per_kwh.is_finite()
            || !(0.0..=places::price::MAX_PER_KWH).contains(&r.per_kwh)
            || currency.len() != 3
            || !currency.bytes().all(|b| b.is_ascii_uppercase())
        {
            continue;
        }
        let Some(ch) = e.db().get::<Charge>(r.car, r.charge)? else { continue };
        let Some(place) = ch.position.as_ref().and_then(|p| places.place_at(p.lat, p.lon)) else { continue };
        let tz = match places.extra(place.id).and_then(|x| x.time_zone.clone()) {
            Some(z) => z,
            None => match zones.get(&r.car) {
                Some(z) => z.clone(),
                None => {
                    let z = e.db().car(r.car)?.map_or_else(|| "UTC".to_string(), |c| e.car_zone(&c).0);
                    zones.insert(r.car, z.clone());
                    z
                }
            },
        };
        let day = crate::tz::date_str(r.start_ms, crate::tz::zone(&tz));
        at.entry(place.id).or_default().push((r.start_ms, day, currency, r.per_kwh));
    }
    let mut want: HashMap<PlaceId, Vec<DatedPrice>> = HashMap::new();
    for (id, mut v) in at {
        v.sort_by(|a, b| a.0.cmp(&b.0).then(a.3.total_cmp(&b.3)));
        // the currency of the newest session; sessions in another are left out
        let Some(currency) = v.last().map(|s| s.2.clone()) else { continue };
        let list: Vec<(String, f64)> = v.into_iter().filter(|s| s.2 == currency).map(|s| (s.1, s.3)).collect();
        want.insert(id, derive(&list, &currency));
    }
    let changed = crate::query::places::set_invoice_prices(e, &want)?;
    let mut out = Updated { places: changed.iter().map(|&id| crate::query::place_ref(id)).collect(), charges_priced: 0 };
    if changed.is_empty() {
        return Ok(out);
    }
    let cars = crate::query::all_cars(e)?;
    for id in changed {
        out.charges_priced += crate::query::places::reprice(e, &cars, id, None, None, true, false)?.charges;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn days(v: &[(&str, f64)]) -> Vec<(String, f64)> {
        v.iter().map(|(d, r)| (d.to_string(), *r)).collect()
    }

    fn got(v: &[DatedPrice]) -> Vec<(String, f64)> {
        v.iter().map(|d| (d.from.clone().unwrap(), d.price.per_kwh.unwrap())).collect()
    }

    #[test]
    fn the_median_of_the_recent_sessions_from_the_earliest_at_it() {
        // a few sessions: the lower middle price, from the first session billed at it
        let s = days(&[("2026-01-03", 0.52), ("2026-01-09", 0.48), ("2026-01-20", 0.61), ("2026-02-01", 0.48)]);
        let p = derive(&s, "EUR");
        assert_eq!(got(&p), vec![("2026-01-09".to_string(), 0.48)]);
        assert_eq!(p[0].source, PriceSource::Invoice);
        assert_eq!(p[0].price.currency, "EUR");
        assert_eq!(p[0].invoice.as_ref().unwrap().sessions, 4);
        assert!(derive(&[], "EUR").is_empty());
    }

    #[test]
    fn a_new_price_when_newer_sessions_move_the_median() {
        // ten at 0.40 through January, then eight at 0.45 from March: the window's median moves to 0.45 once six of
        // its ten are, from the first session at 0.45
        let mut s: Vec<(String, f64)> = (1..=10).map(|d| (format!("2026-01-{d:02}"), 0.40)).collect();
        s.extend((1..=8).map(|d| (format!("2026-03-{d:02}"), 0.45)));
        let p = derive(&s, "USD");
        assert_eq!(got(&p), vec![("2026-01-01".to_string(), 0.40), ("2026-03-01".to_string(), 0.45)]);
        // one odd session does not move it
        let mut t = s.clone();
        t.push(("2026-03-20".into(), 0.99));
        assert_eq!(got(&derive(&t, "USD")), got(&p));
    }
}
