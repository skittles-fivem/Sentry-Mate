"""Where rows and values came from: `sources`, `source_label`, the old `source` word, and `source=` filters."""

from __future__ import annotations

import csv
import io

from .helpers import DAY, NOW, ServerCase

KINDS = {"smo", "tesla_api", "import"}
OLD = {"can", "teslamate", "tesla_api"}


class Sources(ServerCase):
    def check_row(self, x, where):
        self.assertIn("sources", x, where)
        self.assertTrue(x["sources"], where)
        for s in x["sources"]:
            self.assertIn(s["kind"], KINDS, where)
            self.assertTrue(s["label"], where)
            if s["kind"] == "import":
                self.assertEqual(s["site"], "teslamate", where)
                self.assertEqual(s["label"], "Imported from TeslaMate", where)
            else:
                self.assertNotIn("site", s, where)
        self.assertIn(x["source"], OLD, where)
        kinds = [s["kind"] for s in x["sources"]]
        self.assertEqual(x["source"] == "can", "smo" in kinds, where)
        self.assertTrue(x["source_label"].startswith(("Data from", "Imported from")), where)

    def test_every_row_says_where_it_came_from(self):
        q = f"from_ms={NOW - 30 * DAY}&limit=500"
        for car in ("c1", "c2", "c3"):
            for res in ("drives", "charges", "timeline", "states", "activity", "parked", "gaps", "updates", "visits"):
                path = f"/api/v1/cars/{car}/{res}?{q}" if res != "visits" else f"/api/v1/visits?cars={car}&{q}"
                for x in self.c.get(path)["items"]:
                    self.check_row(x, f"{path} {x.get('id')}")

    def test_a_realistic_mix(self):
        seen = set()
        for car, extra in (("c1", ""), ("c2", ""), ("c3", ""), ("c3", "&source=import")):
            for x in self.c.get(f"/api/v1/cars/{car}/drives?limit=500&from_ms=0{extra}")["items"]:
                seen.add((car, tuple(s["kind"] for s in x["sources"])))
        self.assertIn(("c1", ("smo",)), seen)
        self.assertIn(("c1", ("tesla_api",)), seen)
        self.assertIn(("c1", ("smo", "tesla_api")), seen)
        self.assertIn(("c2", ("smo", "tesla_api")), seen)
        self.assertIn(("c3", ("import",)), seen)
        both = next(x for x in self.c.get("/api/v1/cars/c2/drives?limit=5")["items"])
        self.assertEqual(both["source_label"], "Data from SMO and the Tesla API")
        self.assertEqual(both["source"], "can", "the old word stays can while SMO is among the sources")

    def test_filters_and_old_words(self):
        base = f"/api/v1/cars/c1/drives?limit=500&from_ms={NOW - 60 * DAY}"
        every = self.c.get(base)["items"]
        self.assertLess(len(every), 500)
        smo = self.c.get(base + "&source=smo")["items"]
        self.assertEqual(smo, self.c.get(base + "&source=can")["items"], "can is the old word for smo")
        self.assertTrue(all(any(s["kind"] == "smo" for s in x["sources"]) for x in smo))
        api = self.c.get(base + "&source=tesla_api")["items"]
        self.assertTrue(api and all(any(s["kind"] == "tesla_api" for s in x["sources"]) for x in api))
        self.assertEqual({x["id"] for x in smo} | {x["id"] for x in api}, {x["id"] for x in every})
        tm = "/api/v1/cars/c3/drives?limit=500&from_ms=0"
        imported = self.c.get(tm + "&source=import")["items"]
        self.assertTrue(imported)
        self.assertEqual(imported, self.c.get(tm + "&source=teslamate")["items"])
        self.assertEqual(imported, self.c.get(tm + "&source=import:teslamate")["items"])
        self.c.error("GET", base + "&source=fleet_api", 400, "bad_input")
        t = self.c.get("/api/v1/cars/c1/totals?source=can")
        self.assertEqual(t["source"], "smo")
        self.assertEqual([s["kind"] for s in t["sources"]], ["smo"])

    def test_live_values_and_cars(self):
        n = self.c.get("/api/v1/cars/c1/now")
        self.assertEqual(n["speed_kph"]["sources"], [{"kind": "smo", "label": "Data from SMO"}])
        self.assertEqual([i["kind"] for i in n["inputs"]], ["smo", "tesla_api"])
        self.assertTrue(all(isinstance(i["last_ms"], int) for i in n["inputs"]))
        cars = {x["id"]: x for x in self.c.get("/api/v1/cars")["items"]}
        self.assertEqual(cars["c3"]["source_label"], "Data from SMO; imported from TeslaMate")
        self.assertEqual(cars["c2"]["source_label"], "Data from SMO and the Tesla API")

    def test_series_names_its_sources(self):
        s = self.c.get(f"/api/v1/cars/c3/series?fields=soc_pct&from_ms={NOW - 500 * DAY}&to_ms={NOW}&max_points=50")
        self.assertEqual({x["kind"] for x in s["sources"]}, {"smo", "import"})
        only = self.c.get(f"/api/v1/cars/c3/series?fields=soc_pct&from_ms={NOW - 500 * DAY}&to_ms={NOW}"
                          "&max_points=50&source=import")
        self.assertEqual(only["source_label"], "Imported from TeslaMate")

    def test_overlaps_both_spellings(self):
        span = self.c.get("/api/v1/cars/c3/overlaps")["items"][0]
        self.assertEqual(span["shown_source"], "live")
        self.assertEqual(span["live"], span["can"])
        self.assertEqual(span["imported"], span["teslamate"])
        d = self.c.json("PATCH", "/api/v1/cars/c3/overlaps", {"id": span["id"], "shown": "import:teslamate"})
        self.assertEqual((d["changed"]["shown"], d["changed"]["shown_source"]), ("teslamate", "import:teslamate"))
        d = self.c.json("PATCH", "/api/v1/cars/c3/overlaps", {"id": span["id"], "shown": "live"})
        self.assertEqual((d["changed"]["shown"], d["changed"]["shown_source"]), ("can", "live"))

    def test_csv_writes_sources_as_words(self):
        st, hd, raw = self.c.request("GET", "/api/v1/cars/c2/drives?format=csv&limit=3")
        self.assertEqual(st, 200)
        rows = list(csv.reader(io.StringIO(raw.decode("utf-8-sig"))))
        col = rows[0].index("sources")
        self.assertEqual(rows[1][col], "smo;tesla_api")
