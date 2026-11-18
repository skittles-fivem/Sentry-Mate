//! Positions on the globe: distances and points along a route.

/// A position, degrees.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LatLon {
    pub lat: f64,
    pub lon: f64,
}

impl LatLon {
    pub const fn new(lat: f64, lon: f64) -> LatLon {
        LatLon { lat, lon }
    }

    /// Great-circle distance, km.
    pub fn km_to(self, o: LatLon) -> f64 {
        const R: f64 = 6371.0088;
        let (p1, p2) = (self.lat.to_radians(), o.lat.to_radians());
        let dp = p2 - p1;
        let dl = (o.lon - self.lon).to_radians();
        let (sp, sl) = (libm::sin(dp / 2.0), libm::sin(dl / 2.0));
        let a = sp * sp + libm::cos(p1) * libm::cos(p2) * sl * sl;
        2.0 * R * libm::asin(a.sqrt())
    }

    /// The point a fraction `f` of the way to `o` (straight in degrees: fine over a few hundred km).
    pub fn lerp(self, o: LatLon, f: f64) -> LatLon {
        LatLon { lat: self.lat + (o.lat - self.lat) * f, lon: self.lon + (o.lon - self.lon) * f }
    }

    /// Moved by `north_km` and `east_km`.
    pub fn offset(self, north_km: f64, east_km: f64) -> LatLon {
        let lat = self.lat + north_km / 111.195;
        let lon = self.lon + east_km / (111.195 * libm::cos(self.lat.to_radians()).max(0.01));
        LatLon { lat, lon }
    }
}

/// A route: a polyline with a speed limit per leg (km/h) and whether the car stops at the end of each leg.
#[derive(Debug, Clone)]
pub struct Route {
    pub points: Vec<LatLon>,
    /// Per leg (points.len() - 1).
    pub limit_kmh: Vec<f64>,
    pub stop_after: Vec<bool>,
    /// Cumulative km at each point.
    cum: Vec<f64>,
}

impl Route {
    pub fn new(points: Vec<LatLon>, limit_kmh: Vec<f64>, stop_after: Vec<bool>) -> Route {
        let mut cum = Vec::with_capacity(points.len());
        let mut d = 0.0;
        for (i, p) in points.iter().enumerate() {
            if i > 0 {
                d += points[i - 1].km_to(*p);
            }
            cum.push(d);
        }
        Route { points, limit_kmh, stop_after, cum }
    }

    pub fn km(&self) -> f64 {
        self.cum.last().copied().unwrap_or(0.0)
    }

    /// The leg a distance along the route falls in.
    pub fn leg_at(&self, km: f64) -> usize {
        let n = self.points.len().saturating_sub(1);
        (1..self.cum.len()).find(|&i| self.cum[i] > km).map_or(n.saturating_sub(1), |i| i - 1)
    }

    /// Where the leg ends, km along the route.
    pub fn leg_end_km(&self, leg: usize) -> f64 {
        self.cum.get(leg + 1).copied().unwrap_or_else(|| self.km())
    }

    /// The point `km` along the route.
    pub fn at(&self, km: f64) -> LatLon {
        let leg = self.leg_at(km);
        let (a, b) = (self.cum[leg], self.cum.get(leg + 1).copied().unwrap_or(self.cum[leg]));
        let f = if b > a { ((km - a) / (b - a)).clamp(0.0, 1.0) } else { 1.0 };
        self.points[leg].lerp(self.points.get(leg + 1).copied().unwrap_or(self.points[leg]), f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distances_and_points_along() {
        let a = LatLon::new(0.3, 0.2);
        let b = a.offset(3.0, 4.0);
        assert!((a.km_to(b) - 5.0).abs() < 0.01, "{}", a.km_to(b));
        let r = Route::new(vec![a, a.offset(1.0, 0.0), b], vec![50.0, 50.0], vec![false, true]);
        let mid = a.offset(1.0, 0.0);
        assert!((r.km() - (a.km_to(mid) + mid.km_to(b))).abs() < 1e-9);
        assert_eq!(r.leg_at(0.5), 0);
        assert_eq!(r.leg_at(1.5), 1);
        assert_eq!(r.leg_at(100.0), 1);
        assert!(r.at(r.km()).km_to(b) < 1e-6);
    }
}
