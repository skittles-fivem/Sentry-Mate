"""Route geometry for made-up drives: a polyline with elevation, and positions along it.

Routes come from three sources: the San Francisco street routes already used by the Logbook pack
(data/sf_routes.json), the two road-trip waypoint lists in places.py, and for anything else a
deterministic street-grid path between the two points.
"""

from __future__ import annotations

import bisect
import functools
import json
import math
import os

from ..util import bearing_deg, haversine_m, rng
from . import places as P

_HERE = os.path.dirname(os.path.abspath(__file__))
with open(os.path.join(_HERE, "..", "data", "sf_routes.json"), encoding="utf-8") as f:
    _SF = json.load(f)["routes"]

# place id -> the key the SF route file uses
SF_KEYS = {"p1": "home", "p2": "work", "p4": "tartine", "p5": "costco", "p6": "ferry", "p7": "crissy",
           "p8": "oceanbeach", "p9": "ggpark", "p10": "traderjoes", "p11": "stanford", "p12": "muirwoods",
           "p13": "halfmoonbay", "p14": "berkeley"}


def terrain_m(lat: float, lon: float) -> float:
    """A smooth made-up elevation: hills around the Bay, nearly flat around Chicago."""
    if lon > -100:  # Midwest and Ontario
        return 180 + 6 * math.sin(lat * 40) * math.cos(lon * 37)
    hill = math.sin(lat * 157.0) * math.cos(lon * 131.0)
    ridge = math.sin(lat * 23.0 + lon * 17.0)
    return max(2.0, 45 + 70 * hill * hill + 35 * ridge)


class Route:
    __slots__ = ("key", "pts", "cum", "length", "climb_m", "descent_m")

    def __init__(self, key, pts):
        # drop repeated points so every segment has a length
        clean = [pts[0]]
        for p in pts[1:]:
            if haversine_m(clean[-1][0], clean[-1][1], p[0], p[1]) > 0.5:
                clean.append(p)
        if len(clean) == 1:
            clean.append((clean[0][0] + 0.0001, clean[0][1], clean[0][2]))
        self.key = key
        self.pts = clean
        cum = [0.0]
        for a, b in zip(clean, clean[1:]):
            cum.append(cum[-1] + haversine_m(a[0], a[1], b[0], b[1]))
        self.cum = cum
        self.length = cum[-1]
        climb = descent = 0.0
        prev = None
        steps = max(2, int(self.length / 50))
        for i in range(steps + 1):
            e = self.at(self.length * i / steps)[2]
            if prev is not None:
                d = e - prev
                if d > 0:
                    climb += d
                else:
                    descent -= d
            prev = e
        self.climb_m = climb
        self.descent_m = descent

    def latlon(self, s: float):
        """(lat, lon) at s metres along the route: at() without elevation and heading (for long tracks)."""
        cum = self.cum
        if s <= 0:
            k, f = 1, 0.0
        elif s >= self.length:
            k, f = len(cum) - 1, 1.0
        else:
            k = min(max(bisect.bisect_right(cum, s), 1), len(cum) - 1)
            seg = cum[k] - cum[k - 1]
            f = (s - cum[k - 1]) / seg if seg > 0 else 0.0
        a, b = self.pts[k - 1], self.pts[k]
        return a[0] + (b[0] - a[0]) * f, a[1] + (b[1] - a[1]) * f

    def at(self, s: float):
        """(lat, lon, elevation m, heading deg) at s metres along the route."""
        cum = self.cum
        if s <= 0:
            k = 1
            f = 0.0
        elif s >= self.length:
            k = len(cum) - 1
            f = 1.0
        else:
            k = bisect.bisect_right(cum, s)
            k = min(max(k, 1), len(cum) - 1)
            seg = cum[k] - cum[k - 1]
            f = (s - cum[k - 1]) / seg if seg > 0 else 0.0
        a, b = self.pts[k - 1], self.pts[k]
        lat = a[0] + (b[0] - a[0]) * f
        lon = a[1] + (b[1] - a[1]) * f
        ele = a[2] + (b[2] - a[2]) * f
        return lat, lon, ele, bearing_deg(a[0], a[1], b[0], b[1])


def _with_terrain(lonlat):
    return [(lat, lon, terrain_m(lat, lon)) for lon, lat in lonlat]


def _grid(a, b, key):
    """A street-grid path between two points: a few right-angle turns with a little jitter."""
    r = rng("grid", key)
    lat1, lon1 = a
    lat2, lon2 = b
    pts = [(lat1, lon1)]
    d = haversine_m(lat1, lon1, lat2, lon2)
    if d < 4000:
        mid = r.uniform(0.3, 0.7)
        if r.random() < 0.5:
            pts.append((lat1, lon1 + (lon2 - lon1) * mid))
            pts.append((lat2, lon1 + (lon2 - lon1) * mid))
        else:
            pts.append((lat1 + (lat2 - lat1) * mid, lon1))
            pts.append((lat1 + (lat2 - lat1) * mid, lon2))
    else:
        n = min(12, max(3, int(d / 2500)))
        for i in range(1, n):
            f = i / n
            j = 0.004 * (r.random() - 0.5)
            lat = lat1 + (lat2 - lat1) * f + j
            lon = lon1 + (lon2 - lon1) * f - j
            # a staircase so it reads as streets
            pts.append((pts[-1][0], lon))
            pts.append((lat, lon))
    pts.append((lat2, lon2))
    return [(la, lo, terrain_m(la, lo)) for la, lo in pts]


def _road(waypoints, a, b):
    """The part of a road-trip waypoint list between two points, in travel order."""
    def nearest(pt):
        return min(range(len(waypoints)), key=lambda i: haversine_m(pt[0], pt[1], waypoints[i][0], waypoints[i][1]))
    i, j = nearest(a), nearest(b)
    if i <= j:
        seg = waypoints[i:j + 1]
    else:
        seg = list(reversed(waypoints[j:i + 1]))
    pts = [(a[0], a[1], seg[0][2])] + [tuple(w) for w in seg] + [(b[0], b[1], seg[-1][2])]
    return pts


def _road_for(a, b):
    for wp in (P.ROAD_SF_LA, P.ROAD_CHI_TOR):
        lat_lo = min(w[0] for w in wp) - 0.3
        lat_hi = max(w[0] for w in wp) + 0.3
        lon_lo = min(w[1] for w in wp) - 0.3
        lon_hi = max(w[1] for w in wp) + 0.3
        if all(lat_lo <= p[0] <= lat_hi and lon_lo <= p[1] <= lon_hi for p in (a, b)):
            return wp
    return None


@functools.lru_cache(maxsize=4096)
def between(from_key: str, to_key: str, road: bool = False) -> Route:
    """The route from one place (or "lat,lon") to another. `road`: follow a road-trip corridor."""
    a = _coords(from_key)
    b = _coords(to_key)
    key = f"{from_key}>{to_key}"
    if road:
        wp = _road_for(a, b)
        if wp is not None:
            return Route(key + ":road", _road(wp, a, b))
    fa, fb = SF_KEYS.get(from_key), SF_KEYS.get(to_key)
    if fa and fb:
        k = f"{fa}>{fb}"
        if k in _SF:
            return Route(key, _with_terrain(_SF[k]))
        rk = f"{fb}>{fa}"
        if rk in _SF:
            return Route(key, _with_terrain(list(reversed(_SF[rk]))))
    return Route(key, _grid(a, b, key))


def _coords(k: str):
    if k in P.BY_ID:
        p = P.BY_ID[k]
        return (p["lat"], p["lon"])
    lat, lon = k.split(",")
    return (float(lat), float(lon))


def point_key(lat, lon) -> str:
    return f"{lat:.5f},{lon:.5f}"
