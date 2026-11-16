//! Positions and distances on the Earth.
//!
//! Distances are haversine on a sphere of 6,371 km (the same figure Sentry Six Manager uses, so a place's radius
//! means the same in both). For indexes, a position is also turned into a point on the unit sphere in 3D: the
//! straight-line (chord) distance between two such points grows with the distance along the surface, so "nearest"
//! and "within r" can be answered with plain 3D geometry, with no special case at the antimeridian or the poles.

use serde::{Deserialize, Serialize};

/// Mean Earth radius, m.
pub const EARTH_RADIUS_M: f64 = 6_371_000.0;

/// A position in degrees (WGS 84, as GPS gives it).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LatLon {
    pub lat: f64,
    pub lon: f64,
}

impl LatLon {
    /// A position, if it is one: latitude -90..=90 and longitude -180..=180, both finite.
    pub fn new(lat: f64, lon: f64) -> Option<LatLon> {
        is_valid(lat, lon).then_some(LatLon { lat, lon })
    }

    pub fn distance_m(&self, other: &LatLon) -> f64 {
        distance_m(self.lat, self.lon, other.lat, other.lon)
    }
}

/// True for a real position: finite, latitude -90..=90, longitude -180..=180.
pub fn is_valid(lat: f64, lon: f64) -> bool {
    lat.is_finite() && lon.is_finite() && (-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon)
}

/// Metres between two positions along the surface (haversine).
pub fn distance_m(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
    let dp = (lat2 - lat1).to_radians();
    let dl = (lon2 - lon1).to_radians();
    let s = (dp / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dl / 2.0).sin().powi(2);
    2.0 * EARTH_RADIUS_M * s.sqrt().min(1.0).asin()
}

/// The point on the unit sphere for a position.
pub(crate) fn unit(lat: f64, lon: f64) -> [f64; 3] {
    let (p, l) = (lat.to_radians(), lon.to_radians());
    [p.cos() * l.cos(), p.cos() * l.sin(), p.sin()]
}

/// The position of a point on (or near) the unit sphere; the point is scaled onto it first. `None` for the centre.
pub(crate) fn from_unit(v: [f64; 3]) -> Option<LatLon> {
    let n = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if !n.is_finite() || n < 1e-12 {
        return None;
    }
    let (x, y, z) = (v[0] / n, v[1] / n, v[2] / n);
    let lat = z.clamp(-1.0, 1.0).asin().to_degrees();
    let lon = if x.abs() < 1e-15 && y.abs() < 1e-15 { 0.0 } else { y.atan2(x).to_degrees() };
    Some(LatLon { lat, lon })
}

/// The chord (straight line through the Earth, on the unit sphere) for a distance along the surface.
pub(crate) fn chord_for_m(m: f64) -> f64 {
    let angle = (m / EARTH_RADIUS_M).min(std::f64::consts::PI);
    2.0 * (angle / 2.0).sin()
}

/// Metres along the surface for a squared chord on the unit sphere.
pub(crate) fn m_for_chord2(c2: f64) -> f64 {
    let c = c2.max(0.0).sqrt().min(2.0);
    2.0 * (c / 2.0).asin() * EARTH_RADIUS_M
}

pub(crate) fn chord2(a: &[f64; 3], b: &[f64; 3]) -> f64 {
    let (dx, dy, dz) = (a[0] - b[0], a[1] - b[1], a[2] - b[2]);
    dx * dx + dy * dy + dz * dz
}

/// A longitude brought into -180..180 (180 itself becomes -180: the same meridian).
pub fn wrap_lon(lon: f64) -> f64 {
    if !lon.is_finite() {
        return lon;
    }
    let w = (lon + 180.0).rem_euclid(360.0) - 180.0;
    if w == -0.0 { 0.0 } else { w }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
    use super::*;

    #[test]
    fn haversine_matches_known_distances() {
        // One degree of latitude is about 111.19 km on a 6,371 km sphere.
        let d = distance_m(10.0, 20.0, 11.0, 20.0);
        assert!((d - 111_194.9).abs() < 1.0, "{d}");
        // Across the antimeridian: 0.002 degrees of longitude at the equator, about 222 m, not 40,000 km.
        let d = distance_m(0.0, 179.999, 0.0, -179.999);
        assert!((d - 222.4).abs() < 0.5, "{d}");
        assert_eq!(distance_m(45.0, 7.0, 45.0, 7.0), 0.0);
        // Antipodes.
        let d = distance_m(0.0, 0.0, 0.0, 180.0);
        assert!((d - std::f64::consts::PI * EARTH_RADIUS_M).abs() < 1.0);
    }

    #[test]
    fn chord_and_metres_agree_with_haversine() {
        for (a, b) in [((1.0, 2.0), (1.001, 2.002)), ((60.0, -179.99), (60.01, 179.99)), ((-89.9, 0.0), (-89.9, 90.0))]
        {
            let h = distance_m(a.0, a.1, b.0, b.1);
            let c = m_for_chord2(chord2(&unit(a.0, a.1), &unit(b.0, b.1)));
            assert!((h - c).abs() < 1e-3 * h.max(1.0), "{h} vs {c}");
            assert!((chord_for_m(h).powi(2) - chord2(&unit(a.0, a.1), &unit(b.0, b.1))).abs() < 1e-12);
        }
    }

    #[test]
    fn unit_round_trips() {
        for (lat, lon) in [(0.0, 0.0), (45.5, -122.6), (-33.9, 151.2), (10.0, 179.9999), (89.0, -45.0)] {
            let p = from_unit(unit(lat, lon)).unwrap();
            assert!((p.lat - lat).abs() < 1e-9 && (p.lon - lon).abs() < 1e-9, "{p:?}");
        }
        assert!(from_unit([0.0, 0.0, 0.0]).is_none());
    }

    #[test]
    fn validity_and_wrapping() {
        assert!(LatLon::new(90.0, 180.0).is_some());
        assert!(LatLon::new(90.1, 0.0).is_none());
        assert!(LatLon::new(0.0, -180.1).is_none());
        assert!(LatLon::new(f64::NAN, 0.0).is_none());
        assert!(LatLon::new(0.0, f64::INFINITY).is_none());
        assert_eq!(wrap_lon(180.0), -180.0);
        assert_eq!(wrap_lon(190.0), -170.0);
        assert_eq!(wrap_lon(-190.0), 170.0);
        assert_eq!(wrap_lon(-0.0), 0.0);
        assert_eq!(wrap_lon(12.5), 12.5);
    }
}
