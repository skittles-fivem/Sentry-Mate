"""Time series and signals."""

from __future__ import annotations

from .helpers import NOW, ServerCase

STATES = {"driving", "charging", "parked", "asleep", "unknown"}


class Series(ServerCase):
    def test_columns(self):
        d = self.c.get(f"/api/v1/cars/c1/series?fields=soc_pct,speed_kph,lat,lon&from_ms={NOW - 86_400_000}"
                       f"&max_points=500")
        n = len(d["t_ms"])
        self.assertGreater(n, 100)
        self.assertLessEqual(n, 501)
        self.assertEqual(len(d["coverage_pct"]), n)
        self.assertEqual(len(d["state"]), n)
        self.assertTrue(set(d["state"]) <= STATES)
        for k, col in d["values"].items():
            self.assertEqual(len(col), n, k)
        self.assertIn("speed_kph.max", d["values"])  # downsampling keeps the extremes
        self.assertNotIn("lat.max", d["values"])
        steps = {b - a for a, b in zip(d["t_ms"], d["t_ms"][1:])}
        self.assertEqual(steps, {d["bucket_ms"]})
        # nulls where the car slept (no samples), and the state column says so
        asleep_nulls = [i for i, s in enumerate(d["state"]) if s == "asleep" and d["values"]["soc_pct"][i] is None]
        self.assertTrue(asleep_nulls)

    def test_extremes_survive_any_bucket(self):
        drives = self.c.get(f"/api/v1/cars/c1/drives?from_ms={NOW - 10 * 86_400_000}&limit=500")["items"]
        top = max((x for x in drives if x["end_ms"]), key=lambda x: x["speed_max_kph"])
        for mp in (20, 200, 5000):
            d = self.c.get(f"/api/v1/cars/c1/series?fields=speed_kph&from_ms={NOW - 10 * 86_400_000}"
                           f"&max_points={mp}&agg=max")
            best = max(v for v in d["values"]["speed_kph"] if v is not None)
            self.assertAlmostEqual(best, top["speed_max_kph"], delta=0.2, msg=f"max_points={mp}")

    def test_aggs(self):
        d = self.c.get(f"/api/v1/cars/c2/series?fields=power_kw&from_ms={NOW - 3_600_000}&agg=min,mean,max")
        vals = d["values"]
        self.assertEqual(set(vals), {"power_kw", "power_kw.min", "power_kw.mean", "power_kw.max"})
        for lo, mid, hi in zip(vals["power_kw.min"], vals["power_kw.mean"], vals["power_kw.max"]):
            if mid is not None:
                self.assertLessEqual(lo, mid + 1e-9)
                self.assertLessEqual(mid, hi + 1e-9)
        self.c.error("GET", "/api/v1/cars/c1/series?fields=warp_factor", 400, "bad_input")
        self.c.error("GET", "/api/v1/cars/c1/series?agg=median", 400, "bad_input")

    def test_teslamate_source(self):
        d = self.c.get(f"/api/v1/cars/c3/series?fields=soc_pct&from_ms={NOW - 700 * 86_400_000}"
                       f"&to_ms={NOW - 600 * 86_400_000}&source=teslamate&max_points=100")
        self.assertTrue(any(v is not None for v in d["values"]["soc_pct"]))
        e = self.c.get(f"/api/v1/cars/c3/series?fields=soc_pct&from_ms={NOW - 700 * 86_400_000}"
                       f"&to_ms={NOW - 600 * 86_400_000}&source=can&max_points=100")
        self.assertTrue(all(v is None for v in e["values"]["soc_pct"]))

    def test_signals(self):
        d = self.c.get("/api/v1/cars/c1/signals/latest?limit=100")
        names = {x["series"] for x in d["items"]}
        self.assertIn("vehicle_speed", names)
        for x in d["items"]:
            self.assertEqual(set(x), {"series", "message_id", "bus", "value", "unit", "value_name", "comment", "at_ms", "has_series"})
            self.assertTrue(x["message_id"].startswith("0x"))
        gear = next(x for x in d["items"] if x["series"] == "gear")
        self.assertEqual(gear["value_name"], "D")
        s = self.c.get("/api/v1/cars/c1/signals/latest?search=tpms")
        self.assertEqual(len(s["items"]), 4)
        cols = self.c.get(f"/api/v1/cars/c1/signals?series=vehicle_speed,soc_display&from_ms={NOW - 600_000}")
        self.assertEqual(set(cols["values"]), {"vehicle_speed", "soc_display"})
        self.assertEqual(cols["units"]["vehicle_speed"], "km/h")
        self.assertEqual(len(cols["t_ms"]), len(cols["values"]["vehicle_speed"]))
