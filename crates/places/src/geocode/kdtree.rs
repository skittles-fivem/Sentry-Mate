//! A static KD-tree over points on the unit sphere, for "the nearest town" with no upper bound on how far it is
//! (a grid would have to search outward for a long time over the sea).
//!
//! Points are the 3D unit vectors of [`crate::geo::unit`]: the nearest by straight-line distance is the nearest
//! along the surface, so the antimeridian and the poles need no special case. The tree is one array in the order
//! of a balanced tree (the median of each range is its node), built once in O(n log n), searched in about
//! O(log n): a few microseconds for the 200,000 towns of GeoNames' cities500.

use crate::geo::chord2;

pub(crate) struct KdTree {
    pts: Vec<[f64; 3]>,
    ids: Vec<u32>,
}

impl KdTree {
    pub(crate) fn build(mut items: Vec<([f64; 3], u32)>) -> KdTree {
        build_range(&mut items, 0);
        let (pts, ids) = items.into_iter().unzip();
        KdTree { pts, ids }
    }

    pub(crate) fn len(&self) -> usize {
        self.pts.len()
    }

    /// The id of the nearest point and its squared chord distance.
    pub(crate) fn nearest(&self, q: &[f64; 3]) -> Option<(u32, f64)> {
        if self.pts.is_empty() {
            return None;
        }
        let mut best = (usize::MAX, f64::INFINITY);
        self.search(q, 0, self.pts.len(), 0, &mut best);
        self.ids.get(best.0).map(|&id| (id, best.1))
    }

    fn search(&self, q: &[f64; 3], lo: usize, hi: usize, depth: usize, best: &mut (usize, f64)) {
        if lo >= hi {
            return;
        }
        let mid = lo + (hi - lo) / 2;
        let p = &self.pts[mid];
        let d = chord2(q, p);
        if d < best.1 || (d == best.1 && self.ids[mid] < self.ids.get(best.0).copied().unwrap_or(u32::MAX)) {
            *best = (mid, d);
        }
        let axis = depth % 3;
        let diff = q[axis] - p[axis];
        let (near, far) = if diff < 0.0 { ((lo, mid), (mid + 1, hi)) } else { ((mid + 1, hi), (lo, mid)) };
        self.search(q, near.0, near.1, depth + 1, best);
        if diff * diff <= best.1 {
            self.search(q, far.0, far.1, depth + 1, best);
        }
    }
}

fn build_range(items: &mut [([f64; 3], u32)], depth: usize) {
    if items.len() <= 1 {
        return;
    }
    let axis = depth % 3;
    let mid = items.len() / 2;
    items.select_nth_unstable_by(mid, |a, b| a.0[axis].total_cmp(&b.0[axis]));
    let (left, rest) = items.split_at_mut(mid);
    build_range(left, depth + 1);
    build_range(&mut rest[1..], depth + 1);
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
    use super::*;
    use crate::geo::unit;

    /// A small, repeatable pseudo-random sequence (no dependency needed for tests).
    fn rng(seed: &mut u64) -> f64 {
        *seed ^= *seed << 13;
        *seed ^= *seed >> 7;
        *seed ^= *seed << 17;
        (*seed >> 11) as f64 / (1u64 << 53) as f64
    }

    #[test]
    fn same_answer_as_looking_at_every_point() {
        let mut s = 0x9E37_79B9_7F4A_7C15u64;
        let pts: Vec<([f64; 3], u32)> =
            (0..5000).map(|i| (unit(rng(&mut s) * 180.0 - 90.0, rng(&mut s) * 360.0 - 180.0), i)).collect();
        let tree = KdTree::build(pts.clone());
        assert_eq!(tree.len(), 5000);
        for _ in 0..500 {
            let q = unit(rng(&mut s) * 180.0 - 90.0, rng(&mut s) * 360.0 - 180.0);
            let brute = pts
                .iter()
                .map(|(p, id)| (chord2(&q, p), *id))
                .min_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)))
                .unwrap();
            let (id, d) = tree.nearest(&q).unwrap();
            assert_eq!((id, d), (brute.1, brute.0));
        }
    }

    #[test]
    fn empty_and_single() {
        assert!(KdTree::build(vec![]).nearest(&unit(0.0, 0.0)).is_none());
        let t = KdTree::build(vec![(unit(1.0, 1.0), 42)]);
        assert_eq!(t.nearest(&unit(-50.0, 100.0)).unwrap().0, 42);
    }
}
