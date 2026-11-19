//! Saved places (geofences): which one a position is at.
//!
//! A position is at a place when it is within that place's radius; when several overlap, the nearest centre wins
//! (then the one listed first), as in Sentry Six Manager's `places.js`. Places are shared by all cars and there may
//! be thousands (every charger a fleet uses), so [`PlaceIndex`] files them in a grid on the sphere: a lookup looks at
//! the few places near the position, never at the whole list. A place limited to some cars is a filter the caller
//! passes ([`PlaceIndex::place_at_where`]), so the index stays one for everybody.

use crate::geo::{self, chord_for_m, unit};
use crate::grid::Grid;
use crate::price::{PriceError, validate_plan};
use model::{DEFAULT_RADIUS_M, MAX_RADIUS_M, MIN_RADIUS_M, Place, PlaceId};
use std::collections::HashMap;

/// A place's radius brought into 10 m to 5 km; a radius that isn't a number is the default 200 m.
pub fn clamp_radius(r: f64) -> f64 {
    if r.is_finite() { r.clamp(MIN_RADIUS_M, MAX_RADIUS_M) } else { DEFAULT_RADIUS_M }
}

/// A radius typed in Settings, in whole metres, or `None` when it is not 10 m to 5 km (so the form can say so
/// instead of quietly saving another radius).
pub fn clean_radius(r: f64) -> Option<f64> {
    let r = r.round();
    (r.is_finite() && (MIN_RADIUS_M..=MAX_RADIUS_M).contains(&r)).then_some(r)
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum PlaceError {
    #[error("Give the place a name of at most 60 characters, without < or >.")]
    Name,
    #[error("The position is not a latitude (-90 to 90) and longitude (-180 to 180).")]
    Position,
    #[error("The radius is 10 m to 5 km.")]
    Radius,
    #[error(transparent)]
    Price(#[from] PriceError),
}

/// Checks and tidies a place the owner saves: a plain name, a real position, a radius of 10 m to 5 km in whole
/// metres, and a valid price plan.
pub fn clean_place(p: &Place) -> Result<Place, PlaceError> {
    let name = p.name.trim();
    if name.is_empty() || name.chars().count() > 60 || name.chars().any(|c| c.is_control() || c == '<' || c == '>') {
        return Err(PlaceError::Name);
    }
    if !geo::is_valid(p.lat, p.lon) {
        return Err(PlaceError::Position);
    }
    let radius_m = clean_radius(p.radius_m).ok_or(PlaceError::Radius)?;
    let price = p.price.as_ref().map(validate_plan).transpose()?;
    Ok(Place { name: name.to_string(), radius_m, price, ..p.clone() })
}

/// The saved places, ready for lookups. Built once from the list (cheap: one pass) and rebuilt when a place
/// changes; lookups need no lock, so every car's worker can use one shared index at once.
pub struct PlaceIndex {
    places: Vec<Place>,
    grid: Grid,
    by_id: HashMap<PlaceId, u32>,
    skipped: Vec<PlaceId>,
}

impl PlaceIndex {
    /// Indexes the places. Radii are clamped to 10 m to 5 km; a place without a real position, or with an id
    /// already seen, is left out (see [`PlaceIndex::skipped`]).
    pub fn new(places: impl IntoIterator<Item = Place>) -> PlaceIndex {
        let mut ix = PlaceIndex {
            places: Vec::new(),
            // Cubes as wide as the largest radius: a place containing the position is in the 27 around it.
            grid: Grid::new(chord_for_m(MAX_RADIUS_M) * 1.000_001),
            by_id: HashMap::new(),
            skipped: Vec::new(),
        };
        for mut p in places {
            if !geo::is_valid(p.lat, p.lon) || ix.by_id.contains_key(&p.id) || ix.places.len() >= u32::MAX as usize {
                ix.skipped.push(p.id);
                continue;
            }
            p.radius_m = clamp_radius(p.radius_m);
            let i = ix.places.len() as u32;
            ix.grid.insert(&unit(p.lat, p.lon), i);
            ix.by_id.insert(p.id, i);
            ix.places.push(p);
        }
        ix
    }

    pub fn len(&self) -> usize {
        self.places.len()
    }

    pub fn is_empty(&self) -> bool {
        self.places.is_empty()
    }

    /// The places indexed, in the order given (radii clamped).
    pub fn places(&self) -> &[Place] {
        &self.places
    }

    pub fn get(&self, id: PlaceId) -> Option<&Place> {
        self.by_id.get(&id).map(|&i| &self.places[i as usize])
    }

    /// Ids of places left out: no real position, or the id given twice (the first one is kept).
    pub fn skipped(&self) -> &[PlaceId] {
        &self.skipped
    }

    /// The place the position is at: the nearest whose radius contains it.
    pub fn place_at(&self, lat: f64, lon: f64) -> Option<&Place> {
        self.place_at_where(lat, lon, |_| true)
    }

    /// As [`PlaceIndex::place_at`], among the places `keep` accepts (e.g. the places a car may use).
    pub fn place_at_where(&self, lat: f64, lon: f64, keep: impl Fn(&Place) -> bool) -> Option<&Place> {
        let mut best: Option<(f64, u32)> = None;
        self.each_containing(lat, lon, |i, m| {
            if keep(&self.places[i as usize]) && best.is_none_or(|(bm, bi)| m < bm || (m == bm && i < bi)) {
                best = Some((m, i));
            }
        });
        best.map(|(_, i)| &self.places[i as usize])
    }

    /// Every place whose radius contains the position, with its distance in m, nearest first.
    pub fn places_at(&self, lat: f64, lon: f64) -> Vec<(&Place, f64)> {
        let mut found = Vec::new();
        self.each_containing(lat, lon, |i, m| found.push((i, m)));
        found.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)));
        found.into_iter().map(|(i, m)| (&self.places[i as usize], m)).collect()
    }

    /// How many of the spots are at each place (every place listed, 0 included), e.g. visits per place.
    pub fn count_at(&self, spots: impl IntoIterator<Item = (f64, f64)>) -> HashMap<PlaceId, usize> {
        let mut out: HashMap<PlaceId, usize> = self.places.iter().map(|p| (p.id, 0)).collect();
        for (lat, lon) in spots {
            if let Some(p) = self.place_at(lat, lon) {
                *out.entry(p.id).or_default() += 1;
            }
        }
        out
    }

    fn each_containing(&self, lat: f64, lon: f64, mut f: impl FnMut(u32, f64)) {
        if !geo::is_valid(lat, lon) {
            return;
        }
        let q = unit(lat, lon);
        self.grid.near(&q, |i| {
            let p = &self.places[i as usize];
            let m = geo::distance_m(lat, lon, p.lat, p.lon);
            if m <= p.radius_m {
                f(i, m);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
    use super::*;
    use model::PlaceKind;

    fn place(id: PlaceId, lat: f64, lon: f64, r: f64) -> Place {
        Place { id, name: format!("P{id}"), kind: PlaceKind::Other, lat, lon, radius_m: r, price: None }
    }

    #[test]
    fn radius_rules() {
        assert_eq!(clamp_radius(5.0), MIN_RADIUS_M);
        assert_eq!(clamp_radius(9000.0), MAX_RADIUS_M);
        assert_eq!(clamp_radius(f64::NAN), DEFAULT_RADIUS_M);
        assert_eq!(clamp_radius(f64::INFINITY), DEFAULT_RADIUS_M);
        assert_eq!(clamp_radius(321.0), 321.0);
        assert_eq!(clean_radius(9.4), None);
        assert_eq!(clean_radius(9.6), Some(10.0));
        assert_eq!(clean_radius(5000.4), Some(5000.0));
        assert_eq!(clean_radius(5000.6), None);
        assert_eq!(clean_radius(f64::NAN), None);
    }

    #[test]
    fn clean_place_checks_everything() {
        let ok = clean_place(&Place { name: "  Home ".into(), radius_m: 150.4, ..place(1, 1.0, 2.0, 0.0) }).unwrap();
        assert_eq!(ok.name, "Home");
        assert_eq!(ok.radius_m, 150.0);
        assert_eq!(clean_place(&Place { name: " ".into(), ..place(1, 1.0, 2.0, 100.0) }), Err(PlaceError::Name));
        assert_eq!(clean_place(&Place { name: "<b>".into(), ..place(1, 1.0, 2.0, 100.0) }), Err(PlaceError::Name));
        assert_eq!(clean_place(&Place { name: "x".repeat(61), ..place(1, 1.0, 2.0, 100.0) }), Err(PlaceError::Name));
        assert_eq!(clean_place(&place(1, 91.0, 2.0, 100.0)), Err(PlaceError::Position));
        assert_eq!(clean_place(&place(1, 1.0, 2.0, 5.0)), Err(PlaceError::Radius));
    }

    #[test]
    fn skips_bad_and_duplicate_places() {
        let ix =
            PlaceIndex::new([place(1, 1.0, 1.0, 100.0), place(2, f64::NAN, 1.0, 100.0), place(1, 5.0, 5.0, 100.0)]);
        assert_eq!(ix.len(), 1);
        assert_eq!(ix.skipped(), &[2, 1]);
        assert_eq!(ix.get(1).unwrap().lat, 1.0);
    }
}
