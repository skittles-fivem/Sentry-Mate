"""Same seed and flags give the same answers; another seed gives other data."""

from __future__ import annotations

import unittest

from .helpers import NOW, Running

PATHS = [
    "/api/v1/cars",
    "/api/v1/cars/c1/now",
    "/api/v1/cars/c2/now",
    f"/api/v1/cars/c1/drives?from_ms={NOW - 60 * 86_400_000}&limit=100",
    f"/api/v1/cars/c2/charges?limit=100",
    f"/api/v1/cars/c3/timeline?limit=100",
    f"/api/v1/cars/c1/series?fields=soc_pct,speed_kph&from_ms={NOW - 86_400_000}",
    "/api/v1/fleet/attention",
]


def answers(**kw):
    r = Running(**kw)
    try:
        c = r.client()
        out = {}
        for p in PATHS:
            st, hd, raw = c.request("GET", p)
            assert st == 200, (p, st, raw[:200])
            out[p] = raw
        return out
    finally:
        r.close()


class Determinism(unittest.TestCase):
    def test_same_seed_same_answers(self):
        a = answers(seed=7)
        b = answers(seed=7)
        for p in PATHS:
            self.assertEqual(a[p], b[p], p)

    def test_other_seed_other_data(self):
        a = answers(seed=7)
        b = answers(seed=8)
        self.assertNotEqual(a[PATHS[3]], b[PATHS[3]])

    def test_now_ms_moves_the_live_events(self):
        r = Running(now_ms=NOW + 3 * 86_400_000 + 2 * 3_600_000)
        try:
            c = r.client()
            self.assertEqual(c.get("/api/v1/cars/c1/now")["state"], "driving")
            self.assertEqual(c.get("/api/v1/cars/c2/now")["state"], "charging")
        finally:
            r.close()
