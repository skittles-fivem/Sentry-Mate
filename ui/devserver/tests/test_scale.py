"""A large fleet: --cars 200000 pages through /api/v1/cars well under 200 ms per page."""

from __future__ import annotations

import time

from .helpers import ServerCase


class BigFleet(ServerCase):
    server_args = {"cars": 200_000}

    def timed(self, path):
        t = time.perf_counter()
        d = self.c.get(path)
        return d, time.perf_counter() - t

    def test_paging_speed(self):
        d, _ = self.timed("/api/v1/cars?limit=100")
        self.assertEqual(d["total"], 200_000)
        seen = {x["id"] for x in d["items"]}
        worst = 0.0
        for _ in range(10):
            d, dt = self.timed(f"/api/v1/cars?limit=100&after={d['next']}")
            worst = max(worst, dt)
            ids = {x["id"] for x in d["items"]}
            self.assertFalse(ids & seen)
            seen |= ids
        self.assertEqual(len(seen), 1100)
        self.assertLess(worst, 0.2, f"slowest page {worst * 1000:.0f} ms")

    def test_sorted_and_filtered_pages(self):
        for q in ("sort=name", "sort=soc", "sort=name&dir=desc", "state=charging", "search=willow"):
            self.timed(f"/api/v1/cars?limit=100&{q}")  # warm the cached order
            d, dt = self.timed(f"/api/v1/cars?limit=100&{q}")
            self.assertEqual(len(d["items"]), 100, q)
            self.assertLess(dt, 0.2, f"{q}: {dt * 1000:.0f} ms")
            d2, dt2 = self.timed(f"/api/v1/cars?limit=100&{q}&after={d['next']}")
            self.assertLess(dt2, 0.2, f"{q} page 2: {dt2 * 1000:.0f} ms")
            self.assertFalse({x["id"] for x in d["items"]} & {x["id"] for x in d2["items"]})
        names = [x["name"].lower() for x in self.c.get("/api/v1/cars?limit=200&sort=name")["items"]]
        self.assertEqual(names, sorted(names))
        socs = [x["now"]["soc_pct"] for x in self.c.get("/api/v1/cars?limit=200&sort=soc")["items"]]
        self.assertEqual(socs, sorted(socs, reverse=True))

    def test_synthetic_car_is_light_but_valid(self):
        car = self.c.get("/api/v1/cars/c150000")
        self.assertEqual(car["id"], "c150000")
        self.assertEqual(self.c.get("/api/v1/cars/c150000/drives")["items"], [])
        self.assertIn("state", self.c.get("/api/v1/cars/c150000/now"))
        self.c.error("GET", "/api/v1/cars/c200001/now", 404, "no_such_car")
        s = self.c.get("/api/v1/fleet/summary")
        self.assertEqual((s["cars"], s["sold"]), (199_999, 1))  # the sold car is counted apart
        self.assertEqual(sum(s["by_state"].values()), 199_999)
        a = self.c.get("/api/v1/fleet/attention?limit=20")
        self.assertEqual(len(a["items"]), 20)
