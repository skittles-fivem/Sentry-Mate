//! Suggested places: where cars stop most that isn't saved yet (Settings → Places).
//!
//! The idea of Sentry Six Manager's `suggestPlaces`: stops are grouped within 150 m of a group's middle (the middle
//! follows the group as stops join), stops inside a saved place are left out, and groups with at least two visits
//! are offered, busiest first, then the most recently visited. Here the groups live in a grid on the sphere, so each
//! stop looks only at the groups near it, and stops come in one at a time ([`Suggester::add`]): a fleet's or a
//! decade's stops can be streamed from the per-car files without ever being a list in memory. Memory grows with
//! the number of groups, not with the number of stops.
//!
//! Feed one spot per stop (where a drive ended, or each parked span), not every position while parked: each spot
//! counts as a visit.

use crate::geo::{self, chord_for_m, chord2, from_unit, unit};
use crate::geofence::PlaceIndex;
use crate::grid::Grid;
use model::CarId;
use serde::{Deserialize, Serialize};

/// Where a car stopped, and when.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Spot {
    pub lat: f64,
    pub lon: f64,
    pub t_ms: u64,
    pub car: CarId,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SuggestOptions {
    /// Stops this close to a group's middle join it, m.
    pub cluster_m: f64,
    /// Groups with fewer visits are not offered.
    pub min_visits: usize,
    /// At most this many suggestions.
    pub max: usize,
}

impl Default for SuggestOptions {
    fn default() -> Self {
        SuggestOptions { cluster_m: 150.0, min_visits: 2, max: 8 }
    }
}

/// A spot worth saving as a place.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Suggestion {
    /// The group's middle, rounded to 6 decimals (~0.1 m).
    pub lat: f64,
    pub lon: f64,
    pub visits: usize,
    pub first_ms: u64,
    pub last_ms: u64,
    /// The cars that stopped there, in id order.
    pub cars: Vec<CarId>,
}

struct Group {
    /// Sum of the members' unit vectors; its direction is the middle.
    sum: [f64; 3],
    mid: [f64; 3],
    visits: usize,
    first_ms: u64,
    last_ms: u64,
    cars: Vec<CarId>,
}

/// Builds suggestions from stops fed one at a time.
pub struct Suggester<'a> {
    places: &'a PlaceIndex,
    opts: SuggestOptions,
    reach2: f64,
    groups: Vec<Group>,
    grid: Grid,
    at_saved: usize,
    invalid: usize,
}

impl<'a> Suggester<'a> {
    pub fn new(places: &'a PlaceIndex, opts: SuggestOptions) -> Suggester<'a> {
        let cluster_m = if opts.cluster_m.is_finite() { opts.cluster_m.clamp(1.0, 5000.0) } else { 150.0 };
        let reach = chord_for_m(cluster_m);
        Suggester {
            places,
            opts: SuggestOptions { cluster_m, ..opts },
            reach2: reach * reach,
            groups: Vec::new(),
            grid: Grid::new(reach * 1.000_001),
            at_saved: 0,
            invalid: 0,
        }
    }

    /// One stop. Newest first gives the same groups as Sentry Six Manager; any order gives sensible ones.
    pub fn add(&mut self, s: &Spot) {
        if !geo::is_valid(s.lat, s.lon) {
            self.invalid += 1;
            return;
        }
        if self.places.place_at(s.lat, s.lon).is_some() {
            self.at_saved += 1;
            return;
        }
        let v = unit(s.lat, s.lon);
        let mut best: Option<(f64, u32)> = None;
        self.grid.near(&v, |i| {
            let d = chord2(&self.groups[i as usize].mid, &v);
            if d <= self.reach2 && best.is_none_or(|(bd, bi)| d < bd || (d == bd && i < bi)) {
                best = Some((d, i));
            }
        });
        let Some((_, i)) = best else {
            let id = self.groups.len() as u32;
            self.grid.insert(&v, id);
            self.groups.push(Group { sum: v, mid: v, visits: 1, first_ms: s.t_ms, last_ms: s.t_ms, cars: vec![s.car] });
            return;
        };
        let g = &mut self.groups[i as usize];
        self.grid.remove(&g.mid, i);
        g.sum = [g.sum[0] + v[0], g.sum[1] + v[1], g.sum[2] + v[2]];
        let n = (g.sum[0] * g.sum[0] + g.sum[1] * g.sum[1] + g.sum[2] * g.sum[2]).sqrt();
        if n > 1e-12 {
            g.mid = [g.sum[0] / n, g.sum[1] / n, g.sum[2] / n];
        }
        g.visits += 1;
        g.first_ms = g.first_ms.min(s.t_ms);
        g.last_ms = g.last_ms.max(s.t_ms);
        if let Err(at) = g.cars.binary_search(&s.car) {
            g.cars.insert(at, s.car);
        }
        let mid = g.mid;
        self.grid.insert(&mid, i);
    }

    /// Stops left out because they were at a saved place.
    pub fn at_saved_places(&self) -> usize {
        self.at_saved
    }

    /// Stops left out because their position wasn't a real one.
    pub fn invalid(&self) -> usize {
        self.invalid
    }

    /// The busiest groups not at a saved place, busiest first (then the most recently visited).
    pub fn finish(self) -> Vec<Suggestion> {
        let mut out: Vec<(u32, Suggestion)> = Vec::new();
        for (i, g) in self.groups.into_iter().enumerate() {
            if g.visits < self.opts.min_visits.max(1) {
                continue;
            }
            let Some(mid) = from_unit(g.mid) else { continue };
            // A middle that drifted into a saved place is that place already.
            if self.places.place_at(mid.lat, mid.lon).is_some() {
                continue;
            }
            let round = |x: f64| (x * 1e6).round() / 1e6;
            out.push((
                i as u32,
                Suggestion {
                    lat: round(mid.lat),
                    lon: round(geo::wrap_lon(mid.lon)),
                    visits: g.visits,
                    first_ms: g.first_ms,
                    last_ms: g.last_ms,
                    cars: g.cars,
                },
            ));
        }
        out.sort_by(|a, b| b.1.visits.cmp(&a.1.visits).then(b.1.last_ms.cmp(&a.1.last_ms)).then(a.0.cmp(&b.0)));
        out.into_iter().take(self.opts.max).map(|(_, s)| s).collect()
    }
}

/// Suggestions from a list of stops, taken newest first as Sentry Six Manager does. For many stops, stream them
/// into a [`Suggester`] instead.
pub fn suggest(spots: impl IntoIterator<Item = Spot>, places: &PlaceIndex, opts: SuggestOptions) -> Vec<Suggestion> {
    let mut spots: Vec<Spot> = spots.into_iter().collect();
    spots.sort_by_key(|s| std::cmp::Reverse(s.t_ms));
    let mut s = Suggester::new(places, opts);
    spots.iter().for_each(|p| s.add(p));
    s.finish()
}
