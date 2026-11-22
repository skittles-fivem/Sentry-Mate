//! Places matched live: every read matches drives, charges, stops and Now against the places as they are now (the
//! nearest centre among those whose circle holds the point, the lowest id on a tie), so a renamed, moved or removed
//! place shows everywhere at once. The index is built once and kept until a place changes
//! ([`Engine::places_changed`]).
//!
//! What the contract has on a place beyond [`model::Place`] (its time zone, the `service` kind, a price per minute,
//! when it was made, its version) is kept beside it ([`PlaceExtra`]).

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
}

/// The places, indexed for lookups.
pub struct Places {
    index: places::PlaceIndex,
    extras: HashMap<PlaceId, PlaceExtra>,
}

impl Places {
    pub fn new(list: Vec<Place>, extras: HashMap<PlaceId, PlaceExtra>) -> Places {
        Places { index: places::PlaceIndex::new(list), extras }
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
