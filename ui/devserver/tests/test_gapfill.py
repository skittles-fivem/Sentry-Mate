"""Import gap filling: where live history has no value, the import shown there fills it, labelled as imported.

Atlas (`c3`) has no cabin temperature on CAN; its TeslaMate import does. In the days both cover, its live drives
get the import's, named in `value_sources`; a live value is never replaced, rows are not doubled, totals stay, and
the switch (the car's, else the server's) turns it off.
"""

from __future__ import annotations

from types import SimpleNamespace
from unittest import TestCase

from .helpers import ServerCase

OVERLAP = "/api/v1/cars/c3/overlaps"


class GapFill(ServerCase):
    def span(self):
        return next(x for x in self.c.get(OVERLAP)["items"] if x.get("kind", "live") == "live")

    def drives(self, extra=""):
        o = self.span()
        return self.c.get(f"/api/v1/cars/c3/drives?limit=500&from_ms={o['start_ms']}&to_ms={o['end_ms']}{extra}")

    def set_car(self, value):
        s = self.c.get("/api/v1/cars/c3/settings")
        return self.c.json("PATCH", "/api/v1/cars/c3/settings", {"version": s["version"], "import_gap_fill": value})

    def test_live_drives_get_the_missing_value_from_the_import(self):
        out = self.drives()
        live = [x for x in out["items"] if x["source"] == "can"]
        self.assertTrue(live)
        for x in live:
            self.assertIsNotNone(x["cabin_avg_c"], x["id"])
            self.assertEqual(x["value_sources"]["cabin_avg_c"][0]["label"], "Imported from TeslaMate")
            self.assertNotIn("outside_avg_c", x["value_sources"], "a live value is never replaced")
            self.assertNotIn("energy_kwh", x["value_sources"])
            self.assertEqual(x["source_label"], "Data from SMO; imported from TeslaMate")
            self.assertEqual(x["energy_basis"], "measured")
        ids = [x["id"] for x in out["items"]]
        self.assertEqual(len(ids), len(set(ids)), "no doubled rows")
        self.assertFalse(any(x["source"] == "teslamate" for x in out["items"]), "the live side is shown")
        # one source alone: nothing filled
        smo = self.drives("&source=smo")["items"]
        self.assertTrue(all(x["cabin_avg_c"] is None and x["value_sources"] == {} for x in smo))
        self.assertEqual(self.drives()["summary"]["distance_km"], out["summary"]["distance_km"])

    def test_series_fills_from_the_import_and_says_so(self):
        o = self.span()
        a, b = o["start_ms"] + 86_400_000, o["end_ms"] - 86_400_000  # (buckets are aligned: inside the span)
        q = f"/api/v1/cars/c3/series?fields=cabin_c,speed_kph&from_ms={a}&to_ms={b}&max_points=300"
        s = self.c.get(q)
        self.assertTrue(any(v is not None for v in s["values"]["cabin_c"]))
        self.assertEqual([x["kind"] for x in s["value_sources"]["cabin_c"]], ["import"])
        self.assertEqual([x["kind"] for x in s["value_sources"]["speed_kph"]], ["smo"])
        alone = self.c.get(q + "&source=smo")
        self.assertTrue(all(v is None for v in alone["values"]["cabin_c"]))

    def test_the_switch_turns_it_off_and_flipping_still_works(self):
        before = self.drives()["summary"]
        try:
            v = self.set_car(False)
            self.assertFalse(v["import_gap_fill_on"])
            live = [x for x in self.drives()["items"] if x["source"] == "can"]
            self.assertTrue(live and all(x["cabin_avg_c"] is None and x["value_sources"] == {} for x in live))
            self.assertEqual(self.drives()["summary"]["distance_km"], before["distance_km"])
            v = self.set_car(None)
            self.assertTrue(v["import_gap_fill_on"] and v["import_gap_fill"] is None)
            s = self.c.get("/api/v1/settings")
            self.assertTrue(s["import_gap_fill"]["enabled"])
            s = self.c.json("PATCH", "/api/v1/settings", {"version": s["version"], "import_gap_fill": {"enabled": False}})
            self.assertFalse(self.c.get("/api/v1/cars/c3/settings")["import_gap_fill_on"])
            self.assertTrue(self.set_car(True)["import_gap_fill_on"], "the car's own setting wins")
            self.set_car(None)
            self.c.json("PATCH", "/api/v1/settings", {"version": s["version"], "import_gap_fill": {"enabled": True}})
            # the span flipped to the imported side: the import's rows alone, nothing filled
            span = self.span()
            self.c.json("PATCH", OVERLAP, {"id": span["id"], "shown": "import:teslamate"})
            rows = self.drives()["items"]
            self.assertTrue(rows and all(x["source"] == "teslamate" and x["value_sources"] == {} for x in rows))
            self.c.json("PATCH", OVERLAP, {"id": span["id"], "shown": "live"})
        finally:
            self.set_car(None)

    def test_bad_values_are_refused(self):
        s = self.c.get("/api/v1/cars/c3/settings")
        e = self.c.json("PATCH", "/api/v1/cars/c3/settings", {"version": s["version"], "import_gap_fill": "yes"},
                        expect=400)
        self.assertIn("import_gap_fill", e["error"]["fields"])


class FillRow(TestCase):
    """`fill_row` fills what the server fills (crates/timeline/src/gapfill.rs): a live drive's start (never its end,
    whose absence means its data stopped), a charge's position, and its odometer only with the position."""

    class Seg:
        def __init__(self, kind):
            self.source, self.kind, self.id = "can", kind, 1

    def fill(self, kind, row, other):
        from mockapi import gapfill

        app = SimpleNamespace(store=SimpleNamespace(car_settings={}, server_settings={}))
        twin = SimpleNamespace(tm_copy_of=1, source="teslamate", site="teslamate")
        h = SimpleNamespace(car=SimpleNamespace(id="c1"), shadow=[twin], site_shown=lambda t: True)
        return gapfill.fill_row(app, h, self.Seg(kind), dict(row, sources=[]), lambda t: other)

    def test_a_drive_gets_its_start_but_never_its_end(self):
        p = {"lat": 37.0, "lon": -122.0, "label": None}
        row = self.fill("drive", {"start": None, "end": None, "cabin_avg_c": None},
                        {"start": p, "end": p, "cabin_avg_c": 21.0})
        self.assertEqual(row["start"], p)
        self.assertIsNone(row["end"])
        self.assertEqual(sorted(row["value_sources"]), ["cabin_avg_c", "start"])
        from mockapi import gapfill
        self.assertIsNone(gapfill.unfill(row)["start"])

    def test_a_charge_gets_its_odometer_only_with_its_position(self):
        p = {"lat": 37.0, "lon": -122.0, "accuracy_m": 6.0}
        row = self.fill("charge", {"position": None, "odometer_km": None}, {"position": p, "odometer_km": 1000.0})
        self.assertEqual((row["position"], row["odometer_km"]), (p, 1000.0))
        self.assertEqual(sorted(row["value_sources"]), ["odometer_km", "position"])
        row = self.fill("charge", {"position": p, "odometer_km": None}, {"position": p, "odometer_km": 1000.0})
        self.assertIsNone(row["odometer_km"], "the live position stays, so the odometer is not filled")
        self.assertEqual(row["value_sources"], {})
