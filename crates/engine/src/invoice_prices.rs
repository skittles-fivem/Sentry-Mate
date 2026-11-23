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
//! The sessions come through [`InvoiceRates`], so this does not depend on how invoices are read or kept; the
//! engine's own store is [`Kept`].
//!
//! - **When**: [`run`] works them out again in the background ([`Engine::invoice_prices_soon`], one run at a time,
//!   runs asked for while one is queued folded into it): when the server starts, when an input keeps new or changed
//!   sessions ([`Engine::invoice_upsert`]), and when a car's charges are rebuilt over the time of one of its sessions
//!   (the matches may move with them).
//! - **Audited**: each place whose invoice prices changed leaves an entry ([`Audited`]) the web server takes into the
//!   audit log ([`Engine::invoice_price_audits`]), kept until then, so a restart in between loses none.

use crate::invoices::Session;
use crate::placing::{DatedPrice, InvoiceBasis, PriceSource};
use crate::query::places::Price;
use crate::{Engine, Result, lock, now_ms, records};
use model::{CarId, Charge, PlaceId};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::atomic::Ordering;

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

/// The invoices this server keeps ([`crate::invoices`]): every session matched to a charge, at the price per kWh
/// its charging was billed at.
pub struct Kept;

impl InvoiceRates for Kept {
    fn invoice_rates(&self, e: &Engine) -> Result<Vec<InvoiceRate>> {
        let all = e.invoice_sessions()?;
        let cars: BTreeSet<CarId> = all.values().filter_map(|s| s.car).collect();
        let mut out = Vec::new();
        for car in cars {
            let matches = e.invoice_matches(car)?;
            for (&charge, keys) in matches.iter() {
                for s in keys.iter().filter_map(|k| all.get(k)) {
                    let (Some(per_kwh), Some(currency)) = (rate_of(s), s.currency.clone()) else { continue };
                    out.push(InvoiceRate { car, charge, start_ms: s.start_ms, currency, per_kwh });
                }
            }
        }
        out.sort_by_key(|r| (r.car, r.start_ms, r.charge));
        Ok(out)
    }
}

/// The price per kWh a session's charging was billed at: its charging fee by the kWh (the one that billed the most
/// kWh, when there are more), none when it was billed by the minute or gives no rate.
pub fn rate_of(s: &Session) -> Option<f64> {
    s.fees
        .iter()
        .filter(|f| f.kind == "charging" && f.unit.as_deref() == Some("kwh"))
        .filter_map(|f| Some((f.rate.filter(|r| r.is_finite() && *r > 0.0)?, f.usage.unwrap_or(0.0))))
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(rate, _)| rate)
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
    /// Each place whose invoice prices changed: what it has now.
    pub changes: Vec<Audited>,
}

/// A place whose invoice prices changed, as the audit log tells it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Audited {
    pub at_ms: u64,
    /// `p<id>`.
    pub place: String,
    pub name: String,
    /// Its invoice prices now, oldest first (none: it has none any more).
    pub prices: Vec<DatedPrice>,
    /// Its charges without a cost that got one.
    pub charges_priced: u64,
}

impl Audited {
    /// The sentence of its audit entry.
    pub fn summary(&self) -> String {
        let Some(last) = self.prices.last() else {
            return format!(
                "The place {} has no price from Supercharger invoices any more (no invoiced session is matched to a \
                 charge there).",
                self.name
            );
        };
        let rate = last.price.per_kwh.map_or_else(String::new, |r| format!("{r} {} a kWh", last.price.currency));
        let from = last.from.as_deref().map_or_else(String::new, |d| format!(" from {d}"));
        let n = last.invoice.as_ref().map_or(0, |b| b.sessions);
        let older = match self.prices.len() - 1 {
            0 => String::new(),
            1 => " (and 1 older price)".to_string(),
            k => format!(" (and {k} older prices)"),
        };
        let charges = match self.charges_priced {
            0 => String::new(),
            1 => "; 1 charge without a cost priced at it".to_string(),
            k => format!("; {k} charges without a cost priced at it"),
        };
        format!(
            "Priced the place {} from Supercharger invoices: {rate}{from}, the median of {n} recent sessions{older}{charges}.",
            self.name
        )
    }
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
    let mut out = Updated {
        places: changed.iter().map(|&id| crate::query::place_ref(id)).collect(),
        charges_priced: 0,
        changes: Vec::new(),
    };
    if changed.is_empty() {
        return Ok(out);
    }
    let cars = crate::query::all_cars(e)?;
    let now = e.places();
    for id in changed {
        let n = crate::query::places::reprice(e, &cars, id, None, None, true, false)?.charges;
        out.charges_priced += n;
        out.changes.push(Audited {
            at_ms: now_ms(),
            place: crate::query::place_ref(id),
            name: now.get(id).map(|p| p.name.clone()).unwrap_or_default(),
            prices: now.prices(id).iter().filter(|x| x.source == PriceSource::Invoice).cloned().collect(),
            charges_priced: n,
        });
    }
    Ok(out)
}

/// The key/value place of the entries the audit log has not taken yet.
const NS: &str = "invoice-prices";
const PENDING: &str = "audit";
/// The most entries kept for the audit log (the oldest go first).
const MAX_PENDING: usize = 500;

/// Works every place's invoice prices out again from the invoices kept ([`Kept`]) and keeps an audit entry for each
/// place that changed. One run at a time.
pub fn run(e: &Engine) -> Result<()> {
    // a change from here on asks for another run
    e.inner.invoice_prices_queued.store(false, Ordering::SeqCst);
    let _one = lock(&e.inner.invoice_prices_run);
    let up = update(e, &Kept).map_err(|x| match x {
        crate::query::QueryError::Engine(x) => x,
        crate::query::QueryError::Db(x) => crate::Error::Db(x),
        other => crate::Error::Other(other.to_string()),
    })?;
    if up.changes.is_empty() {
        return Ok(());
    }
    tracing::info!(
        "invoice prices: {} place(s) changed, {} charge(s) without a cost priced",
        up.changes.len(),
        up.charges_priced
    );
    let _w = lock(&e.inner.invoice_write);
    let mut pending: Vec<Audited> = records::get(&e.inner.db, NS, PENDING)?.unwrap_or_default();
    pending.extend(up.changes);
    let over = pending.len().saturating_sub(MAX_PENDING);
    pending.drain(..over);
    records::put(&e.inner.db, NS, PENDING, &pending)
}

impl Engine {
    /// Queues a run of the places' invoice prices ([`run`]); nothing while the engine is not running (its start
    /// queues one) or while one is queued already.
    pub fn invoice_prices_soon(&self) {
        if self.handle().is_err() || self.inner.invoice_prices_queued.swap(true, Ordering::SeqCst) {
            return;
        }
        self.send(crate::Work::InvoicePrices);
    }

    /// Charges of a car changed in `[from_ms, to_ms)`: a run when one of its invoiced sessions is in that time.
    pub(crate) fn invoice_rows_changed(&self, car: CarId, from_ms: u64, to_ms: u64) {
        let slack = crate::invoices::TIME_SLACK_MS;
        let Ok(all) = self.invoice_sessions() else { return };
        if all.values().any(|s| {
            s.car == Some(car) && s.start_ms < to_ms.saturating_add(slack) && s.end_ms.saturating_add(slack) >= from_ms
        }) {
            self.invoice_prices_soon();
        }
    }

    /// Takes the entries of invoice price changes the audit log has not had yet, oldest first (they are gone from
    /// here once taken).
    pub fn invoice_price_audits(&self) -> Result<Vec<Audited>> {
        let _w = lock(&self.inner.invoice_write);
        let pending: Vec<Audited> = records::get(&self.inner.db, NS, PENDING)?.unwrap_or_default();
        if !pending.is_empty() {
            records::put(&self.inner.db, NS, PENDING, &Vec::<Audited>::new())?;
        }
        Ok(pending)
    }
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
