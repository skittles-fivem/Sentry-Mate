//! A hash grid over the unit sphere, for "what is within r of here" when r has a known upper bound.
//!
//! Points are filed by their 3D position on the unit sphere (see [`crate::geo`]) in cubes whose side is at least
//! the chord of the largest distance asked about. Anything within that distance is then in the cube of the query
//! or one of its 26 neighbours, so a query looks at 27 cubes whatever the number of points, and there is no seam at
//! the antimeridian and no crowding of cells at the poles. Only cubes holding points are stored.

use std::collections::HashMap;

type Cell = (i32, i32, i32);

pub(crate) struct Grid {
    side: f64,
    cells: HashMap<Cell, Vec<u32>>,
}

impl Grid {
    /// A grid for queries reaching at most `reach_chord` (a chord on the unit sphere, see `geo::chord_for_m`).
    pub(crate) fn new(reach_chord: f64) -> Grid {
        // At least a metre's chord, so a tiny or zero reach can't make the cube numbers overflow.
        let side = if reach_chord.is_finite() { reach_chord.max(1.6e-7) } else { 2.0 };
        Grid { side, cells: HashMap::new() }
    }

    fn cell(&self, v: &[f64; 3]) -> Cell {
        let k = |x: f64| (x / self.side).floor() as i32;
        (k(v[0]), k(v[1]), k(v[2]))
    }

    pub(crate) fn insert(&mut self, v: &[f64; 3], id: u32) {
        let c = self.cell(v);
        self.cells.entry(c).or_default().push(id);
    }

    /// Takes `id` out of the cube `v` is in (where it was filed). False when it wasn't there.
    pub(crate) fn remove(&mut self, v: &[f64; 3], id: u32) -> bool {
        let c = self.cell(v);
        let Some(ids) = self.cells.get_mut(&c) else { return false };
        let Some(i) = ids.iter().position(|&x| x == id) else { return false };
        ids.swap_remove(i);
        if ids.is_empty() {
            self.cells.remove(&c);
        }
        true
    }

    /// Calls `f` with every id filed in the 27 cubes around `v` (each once).
    pub(crate) fn near(&self, v: &[f64; 3], mut f: impl FnMut(u32)) {
        let (x, y, z) = self.cell(v);
        for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    let c = (x.saturating_add(dx), y.saturating_add(dy), z.saturating_add(dz));
                    if let Some(ids) = self.cells.get(&c) {
                        ids.iter().for_each(|&id| f(id));
                    }
                }
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn cells(&self) -> usize {
        self.cells.len()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
    use super::*;
    use crate::geo::{chord_for_m, unit};

    #[test]
    fn finds_neighbours_and_forgets_removed_ones() {
        let mut g = Grid::new(chord_for_m(1000.0));
        let a = unit(10.0, 20.0);
        let b = unit(10.005, 20.0); // ~556 m north
        let far = unit(11.0, 20.0);
        g.insert(&a, 1);
        g.insert(&b, 2);
        g.insert(&far, 3);
        let mut seen = vec![];
        g.near(&a, |id| seen.push(id));
        seen.sort();
        assert_eq!(seen, vec![1, 2]);
        assert!(g.remove(&b, 2));
        assert!(!g.remove(&b, 2));
        let mut seen = vec![];
        g.near(&a, |id| seen.push(id));
        assert_eq!(seen, vec![1]);
        assert_eq!(g.cells(), 2);
    }

    #[test]
    fn no_seam_at_the_antimeridian() {
        let mut g = Grid::new(chord_for_m(500.0));
        g.insert(&unit(-16.0, 179.999), 7);
        let mut seen = vec![];
        g.near(&unit(-16.0, -179.999), |id| seen.push(id));
        assert_eq!(seen, vec![7]);
    }

    #[test]
    fn odd_reach_values_stay_usable() {
        for reach in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            let mut g = Grid::new(reach);
            g.insert(&unit(1.0, 1.0), 1);
            let mut n = 0;
            g.near(&unit(1.0, 1.0), |_| n += 1);
            assert_eq!(n, 1);
        }
    }
}
