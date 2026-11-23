//! Places matched live: every read matches drives, charges, stops and Now against the places as they are now (the
//! nearest centre among those whose circle holds the point, the lowest id on a tie), so a renamed, moved or removed
//! place shows everywhere at once. The index is built once and kept until a place changes
//! ([`Engine::places_changed`]).
//!
//! What the contract has on a place beyond [`model::Place`] (its time zone, the `service` kind, a price per minute,
//! when it was made, its version, its dated prices) is kept beside it ([`PlaceExtra`]).
//!
//! A place holds any number of dated prices ([`DatedPrice`]): each is in force from its first day on (in the place's
//! time zone, else the car's) until the next one's, so a charge is priced at the price of its own day. A price an
//! admin typed always wins over one worked out from Supercharger invoices ([`crate::invoice_prices`]): an invoice
//! price is used only on days no typed price covers. A place saved before prices had dates has one price with no
//! first day (it covers every day).

use crate::records::{self, NS};
use crate::{Engine, Result};
use model::{Place, PlaceId};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

/// A place's fields the model doesn't carry.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PlaceExtra {
    /// IANA zone of the place (its rates follow it); `None`: the car's.
    pub time_zone: Option<String>,
    /// `service` (the model has home, work, charger, other).
    pub kind: Option<String>,
    pub per_minute: Option<f64>,
    pub created_ms: Option<u64>,
    pub version: u64,
    pub address: Option<String>,
    /// The price as the owner gave it (`/places` shape: a price per kWh may be left out, rates may carry a price
    /// per minute); the model's plan is what charges are priced at.
    #[serde(default)]
    pub price: Option<crate::query::places::Price>,
    /// The place's dated prices, oldest first (empty on a place saved before prices had dates: its `price` is then
    /// its one price, from the start).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prices: Vec<DatedPrice>,
}

/// Who set a dated price.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PriceSource {
    /// An admin typed it.
    #[default]
    Typed,
    /// Worked out from the place's Supercharger invoices ([`crate::invoice_prices`]).
    Invoice,
}

/// What an invoice price was worked out from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InvoiceBasis {
    /// The recent sessions its median was taken over.
    pub sessions: u32,
}

/// One of a place's prices, in force from `from` (a local date, `YYYY-MM-DD`; `None`: from the start) until the next
/// one of the same source starts. `price` is any price the API takes (one price, time-of-use rates, a session fee,
/// ...): the date is beside it, not in it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DatedPrice {
    #[serde(default)]
    pub from: Option<String>,
    #[serde(default)]
    pub source: PriceSource,
    pub price: crate::query::places::Price,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invoice: Option<InvoiceBasis>,
}

/// Dated prices in order: oldest first (no first day before every date), typed before invoice on the same day.
pub fn sort_prices(list: &mut [DatedPrice]) {
    list.sort_by(|a, b| a.from.cmp(&b.from).then((a.source as u8).cmp(&(b.source as u8))));
}

/// The price in force on `day` (`YYYY-MM-DD`): the newest typed price that started by then, else the newest invoice
/// price that did. `list` is oldest first ([`sort_prices`]).
pub fn price_on<'a>(list: &'a [DatedPrice], day: &str) -> Option<&'a DatedPrice> {
    let of = |src: PriceSource| {
        list.iter().rev().find(|d| d.source == src && d.from.as_deref().is_none_or(|f| f <= day))
    };
    of(PriceSource::Typed).or_else(|| of(PriceSource::Invoice))
}

/// The places, indexed for lookups.
pub struct Places {
    index: places::PlaceIndex,
    extras: HashMap<PlaceId, PlaceExtra>,
    prices: HashMap<PlaceId, Vec<DatedPrice>>,
}

impl Places {
    pub fn new(list: Vec<Place>, extras: HashMap<PlaceId, PlaceExtra>) -> Places {
        let prices = list
            .iter()
            .map(|p| (p.id, dated(p, extras.get(&p.id))))
            .filter(|(_, v)| !v.is_empty())
            .collect();
        Places { index: places::PlaceIndex::new(list), extras, prices }
    }

    /// A place's dated prices, oldest first (none: no price).
    pub fn prices(&self, id: PlaceId) -> &[DatedPrice] {
        self.prices.get(&id).map_or(&[], Vec::as_slice)
    }

    /// The price of a place in force on `day` ([`price_on`]).
    pub fn price_on(&self, id: PlaceId, day: &str) -> Option<&DatedPrice> {
        price_on(self.prices(id), day)
    }

    /// Whether an admin gave the place a price (of any day).
    pub fn typed_price(&self, id: PlaceId) -> Option<&DatedPrice> {
        self.prices(id).iter().rev().find(|d| d.source == PriceSource::Typed)
    }

    /// The place a point is at, by the nearest-centre rule.
    pub fn place_at(&self, lat: f64, lon: f64) -> Option<&Place> {
        if !lat.is_finite() || !lon.is_finite() {
            return None;
        }
        self.index.place_at(lat, lon)
    }

    /// Every place whose circle holds the point, nearest centre first, with the distance (m).
    pub fn places_at(&self, lat: f64, lon: f64) -> Vec<(&Place, f64)> {
        let mut v: Vec<(&Place, f64)> = self
            .index
            .places()
            .iter()
            .map(|p| (p, places::distance_m(lat, lon, p.lat, p.lon)))
            .filter(|(p, d)| *d <= p.radius_m)
            .collect();
        v.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.0.id.cmp(&b.0.id)));
        v
    }

    pub fn get(&self, id: PlaceId) -> Option<&Place> {
        self.index.get(id)
    }

    pub fn all(&self) -> &[Place] {
        self.index.places()
    }

    pub fn extra(&self, id: PlaceId) -> Option<&PlaceExtra> {
        self.extras.get(&id)
    }

    /// The contract's kind word.
    pub fn kind_word(&self, p: &Place) -> &'static str {
        if self.extra(p.id).and_then(|e| e.kind.as_deref()) == Some("service") {
            return "service";
        }
        match p.kind {
            model::PlaceKind::Home => "home",
            model::PlaceKind::Work => "work",
            model::PlaceKind::Charger => "charger",
            model::PlaceKind::Other => "other",
        }
    }
}

/// A place's dated prices as kept, oldest first; a place saved before prices had dates has its one price, from the
/// start.
pub fn dated(p: &Place, extra: Option<&PlaceExtra>) -> Vec<DatedPrice> {
    if let Some(x) = extra.filter(|x| !x.prices.is_empty()) {
        let mut v = x.prices.clone();
        sort_prices(&mut v);
        return v;
    }
    crate::query::places::price_of(p, extra)
        .map(|price| vec![DatedPrice { from: None, source: PriceSource::Typed, price, invoice: None }])
        .unwrap_or_default()
}

pub fn extra(db: &db::Db, id: PlaceId) -> Result<Option<PlaceExtra>> {
    records::get(db, NS, &format!("place/{id}"))
}

pub fn set_extra(db: &db::Db, id: PlaceId, e: Option<&PlaceExtra>) -> Result<()> {
    let key = format!("place/{id}");
    match e {
        Some(e) => records::put(db, NS, &key, e),
        None => {
            db.kv().delete(NS, &key)?;
            Ok(())
        }
    }
}

impl Engine {
    /// The places as they are now (built on first use after a change).
    pub fn places(&self) -> Arc<Places> {
        if let Ok(g) = self.inner.places.read()
            && let Some(p) = g.as_ref()
        {
            return p.clone();
        }
        let list = self.inner.db.places().unwrap_or_default();
        let extras =
            list.iter().filter_map(|p| extra(&self.inner.db, p.id).ok().flatten().map(|e| (p.id, e))).collect();
        let built = Arc::new(Places::new(list, extras));
        if let Ok(mut g) = self.inner.places.write() {
            *g = Some(built.clone());
        }
        built
    }

    /// A place was added, changed or removed: the next read matches against the places as they are now. Charges keep
    /// the cost they had (see [`crate::costs`]).
    pub fn places_changed(&self) {
        if let Ok(mut g) = self.inner.places.write() {
            *g = None;
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::query::places::Price;

    fn at(from: Option<&str>, source: PriceSource, per_kwh: f64) -> DatedPrice {
        let price =
            Price {
            currency: "EUR".into(),
            per_kwh: Some(per_kwh),
            per_minute: None,
            rates: vec![],
            session_fee: None,
            tiers: None,
        };
        DatedPrice { from: from.map(str::to_string), source, price, invoice: None }
    }

    #[test]
    fn each_day_has_the_price_that_started_by_then_and_a_typed_one_wins() {
        use PriceSource::{Invoice, Typed};
        let mut v = vec![
            at(Some("2026-05-01"), Typed, 0.30),
            at(Some("2026-01-10"), Invoice, 0.48),
            at(Some("2026-03-01"), Typed, 0.25),
            at(Some("2026-06-01"), Invoice, 0.52),
        ];
        sort_prices(&mut v);
        let on = |d: &str| price_on(&v, d).map(|x| x.price.per_kwh.unwrap());
        assert_eq!(on("2026-01-09"), None, "before every price");
        assert_eq!(on("2026-01-10"), Some(0.48), "only the invoice price covers it");
        assert_eq!(on("2026-02-28"), Some(0.48));
        assert_eq!(on("2026-03-01"), Some(0.25), "a typed price from its day on");
        assert_eq!(on("2026-05-01"), Some(0.30));
        assert_eq!(on("2026-07-01"), Some(0.30), "a newer invoice price does not replace a typed one");
        // one from the start covers every day
        v.push(at(None, Typed, 0.1));
        sort_prices(&mut v);
        assert_eq!(v[0].from, None);
        assert_eq!(price_on(&v, "2000-01-01").map(|x| x.price.per_kwh), Some(Some(0.1)));
    }
}
