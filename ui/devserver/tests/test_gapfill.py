"""Import gap filling: where live history has no value, the import shown there fills it, labelled as imported.

Atlas (`c3`) has no cabin temperature on CAN; its TeslaMate import does. In the days both cover, its live drives
get the import's, named in `value_sources`; a live value is never replaced, rows are not doubled, totals stay, and
the switch (the car's, else the server's) turns it off.
"""

from __future__ import annotations

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
