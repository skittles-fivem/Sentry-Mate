"""Drive detail additions: speed band width, ideal range, the charges around a drive, the estimate's basis,
export file names and LO/HI set points."""

from __future__ import annotations

import re

from .helpers import NOW, ServerCase

MONTH = 30 * 86_400_000


class DriveDetail(ServerCase):
    def drives(self, car="c1"):
        return self.c.get(f"/api/v1/cars/{car}/drives?from_ms={NOW - MONTH}&limit=500")["items"]

    def test_band_width(self):
        x = self.drives()[3]
        d = self.c.get(f"/api/v1/cars/c1/drives/{x['id']}?speed_band_kph=16.09344")
        self.assertEqual(d["speed_band_kph"], 16.09344)
        for b in d["speed_bands"]:
            self.assertAlmostEqual(b["to_kph"] - b["from_kph"], 16.09344, places=2)
            self.assertAlmostEqual(b["from_kph"] / 16.09344, round(b["from_kph"] / 16.09344), places=3)
        self.assertAlmostEqual(sum(b["seconds"] for b in d["speed_bands"]) * 1000, d["duration_ms"], delta=2000)
        self.assertEqual(self.c.get(f"/api/v1/cars/c1/drives/{x['id']}")["speed_band_kph"], 10)
        self.c.error("GET", f"/api/v1/cars/c1/drives/{x['id']}?speed_band_kph=0", 400, "bad_input")

    def test_ideal_charges_estimate(self):
        x = self.drives()[3]
        d = self.c.get(f"/api/v1/cars/c1/drives/{x['id']}")
        self.assertGreater(d["start_ideal_range_km"], d["start_range_km"])
        self.assertGreaterEqual(d["start_ideal_range_km"], d["end_ideal_range_km"])
        self.assertGreater(d["est_wh_per_km"], 50)
        self.assertEqual(d["est_basis"], "wh_per_km_30d")
        charges = self.c.get(f"/api/v1/cars/c1/charges?limit=500")["items"]
        before = [c for c in charges if c["start_ms"] < d["start_ms"]]
        self.assertEqual(d["prev_charge"], max(before, key=lambda c: c["start_ms"])["id"] if before else None)
        if d["next_charge"]:
            nxt = next(c for c in charges if c["id"] == d["next_charge"])
            self.assertGreaterEqual(nxt["start_ms"], d["start_ms"])

    def test_export_names(self):
        x = self.drives()[3]
        for fmt in ("gpx", "geojson"):
            st, hd, _ = self.c.request("GET", f"/api/v1/cars/c1/drives/{x['id']}/track?format={fmt}")
            self.assertEqual(st, 200)
            self.assertRegex(hd["content-disposition"], rf'filename="[A-Za-z0-9._-]+-drive-\d{{4}}-\d\d-\d\d-\d{{4}}\.{fmt}"')

    def test_lo_hi_setpoints(self):
        seen = set()
        for x in self.drives():
            s = self.c.get(f"/api/v1/cars/c1/series?fields=setpoint_left_c,climate&from_ms={x['start_ms']}"
                           f"&to_ms={x['start_ms'] + 400_000}&max_points=400")
            seen |= {v for v in s["values"]["setpoint_left_c"] if isinstance(v, str)}
            seen |= {v for v in s["values"]["climate"] if v == "precondition"}
            if {"precondition"} <= seen and seen & {"LO", "HI"}:
                break
        self.assertIn("precondition", seen)
        self.assertTrue(seen & {"LO", "HI"})
