"""Drives: list, detail, track, purposes, stats, logbook, trip and saved trips."""

from __future__ import annotations

import json
import xml.etree.ElementTree as ET

from .helpers import NOW, ServerCase

DRIVE = {"id", "car", "start_ms", "end_ms", "duration_ms", "start", "end", "start_odometer_km", "end_odometer_km",
         "distance_km", "speed_avg_kph", "speed_max_kph", "power_max_kw", "power_min_kw", "energy_kwh", "energy_basis",
         "regen_kwh", "wh_per_km", "efficiency_pct", "start_soc_pct", "end_soc_pct", "start_range_km", "end_range_km",
         "outside_avg_c", "cabin_avg_c", "climb_m", "descent_m", "hvac_kwh", "source", "flags", "purpose"}
POINT = {"lat", "lon", "place", "address", "label"}
WEEK = 7 * 86_400_000


class Drives(ServerCase):
    def test_list_fields_and_summary(self):
        d = self.c.get(f"/api/v1/cars/c1/drives?from_ms={NOW - 30 * 86_400_000}&limit=500")
        self.assertTrue(d["items"])
        for x in d["items"]:
            self.assertTrue(DRIVE <= set(x), DRIVE - set(x))
            self.assertEqual(set(x["start"]), POINT)
            self.assertEqual(set(x["flags"]), {"in_progress", "gap_ms", "lost_frames", "reduced_range"})
            if x["end_ms"]:
                self.assertAlmostEqual(x["end_odometer_km"] - x["start_odometer_km"], x["distance_km"], delta=0.01)
        s = d["summary"]
        self.assertEqual(s["count"], len(d["items"]))
        self.assertAlmostEqual(s["distance_km"], sum(x["distance_km"] for x in d["items"]), delta=0.05)
        self.assertEqual(set(s), {"count", "distance_km", "duration_ms", "energy_kwh", "energy_measured_kwh",
                                  "energy_estimated_kwh", "wh_per_km", "without_energy"})

    def test_filters_and_sorts(self):
        base = f"/api/v1/cars/c1/drives?from_ms={NOW - 120 * 86_400_000}&limit=500"
        long = self.c.get(base + "&min_distance_km=40")["items"]
        self.assertTrue(long)
        self.assertTrue(all(x["distance_km"] >= 40 for x in long))
        home = self.c.get(base + "&places=p1&place_side=end")["items"]
        self.assertTrue(all(x["end"]["place"] and x["end"]["place"]["id"] == "p1" for x in home))
        by = self.c.get(base + "&sort=distance&dir=asc")["items"]
        dists = [x["distance_km"] for x in by]
        self.assertEqual(dists, sorted(dists))
        gaps = self.c.get(f"/api/v1/cars/c1/drives?flags=gap&limit=500")["items"]
        self.assertTrue(gaps)
        self.assertTrue(all(x["flags"]["gap_ms"] > 0 or x["flags"]["lost_frames"] > 0 for x in gaps))
        live = self.c.get("/api/v1/cars/c1/drives?flags=in_progress")["items"]
        self.assertEqual(len(live), 1)
        cold = self.c.get("/api/v1/cars/c2/drives?flags=reduced_range&limit=500")["items"]
        self.assertTrue(cold)  # Chicago winter
        hill = self.c.get(base + "&efficiency=hill_corrected")["items"]
        self.assertEqual(len(hill), len(self.c.get(base)["items"]))
        self.c.error("GET", base + "&places=p999", 400, "bad_input")
        self.c.error("GET", base + "&min_distance_km=-1", 400, "bad_input")

    def test_detail(self):
        lst = self.c.get(f"/api/v1/cars/c1/drives?from_ms={NOW - WEEK}&limit=10")["items"]
        x = lst[3]
        d = self.c.get(f"/api/v1/cars/c1/drives/{x['id']}")
        self.assertEqual(d["id"], x["id"])
        self.assertEqual(d["next"], lst[2]["id"])
        self.assertEqual(d["prev"], lst[4]["id"])
        self.assertAlmostEqual(sum(b["seconds"] for b in d["speed_bands"]) * 1000, d["duration_ms"], delta=2000)
        self.assertEqual(d["rated_wh_per_km"], 143.0)
        self.assertIsInstance(d["gaps"], list)

    def test_stopped_mid_drive(self):
        rows = self.c.get("/api/v1/cars/c1/drives?flags=gap&limit=500")["items"]
        cut = [x for x in rows if x["end"]["label"] == "Data stopped here"]
        self.assertEqual(len(cut), 1)
        d = self.c.get(f"/api/v1/cars/c1/drives/{cut[0]['id']}")
        self.assertIn("no_log", [g["kind"] for g in d["gaps"]])

    def test_track(self):
        x = self.c.get(f"/api/v1/cars/c1/drives?from_ms={NOW - WEEK}&min_distance_km=3&limit=1")["items"][0]
        st, hd, raw = self.c.request("GET", f"/api/v1/cars/c1/drives/{x['id']}/track?extensions=speed,soc")
        self.assertEqual(st, 200)
        self.assertTrue(hd["content-type"].startswith("application/geo+json"))
        g = json.loads(raw)
        self.assertEqual(g["type"], "FeatureCollection")
        f = g["features"][0]
        coords = f["geometry"]["coordinates"]
        self.assertEqual(len(coords[0]), 3)  # lon, lat, elevation
        self.assertEqual(len(coords), len(f["properties"]["t_ms"]))
        self.assertEqual(len(f["properties"]["speed_kph"]), len(coords))
        total = sum(len(ft["properties"]["t_ms"]) for ft in g["features"])
        self.assertAlmostEqual(total, x["duration_ms"] / 1000, delta=40)  # 1 Hz
        st, hd, raw = self.c.request("GET", f"/api/v1/cars/c1/drives/{x['id']}/track?simplify_m=10")
        simple = json.loads(raw)
        self.assertLess(len(simple["features"][0]["geometry"]["coordinates"]), len(coords) / 3)
        st, hd, raw = self.c.request("GET", f"/api/v1/cars/c1/drives/{x['id']}/track?format=gpx&extensions=speed")
        self.assertEqual(st, 200)
        self.assertIn("attachment", hd["content-disposition"])
        root = ET.fromstring(raw)
        ns = {"g": "http://www.topografix.com/GPX/1/1"}
        pts = root.findall(".//g:trkpt", ns)
        self.assertGreater(len(pts), 50)
        self.assertIsNotNone(pts[0].find("g:time", ns))

    def test_purposes_and_rules(self):
        rules = self.c.get("/api/v1/cars/c1/purpose-rules")["items"]
        self.assertEqual(len(rules), 2)
        commute = [x for x in self.c.get(f"/api/v1/cars/c1/drives?from_ms={NOW - WEEK}&limit=100")["items"]
                   if x["purpose"] and x["purpose"]["source"] == "rule"]
        self.assertTrue(commute)
        x = commute[0]
        d = self.c.json("PUT", f"/api/v1/cars/c1/drives/{x['id']}/purpose",
                        {"purpose": "business", "relation": "Client", "note": "Site visit"})
        self.assertEqual(d["purpose"], {"value": "business", "relation": "Client", "note": "Site visit", "source": "owner"})
        self.c.error("PUT", f"/api/v1/cars/c1/drives/{x['id']}/purpose", 422, "invalid", body={"purpose": "fun"})
        d = self.c.json("DELETE", f"/api/v1/cars/c1/drives/{x['id']}/purpose")
        self.assertEqual(d["purpose"]["source"], "rule")
        st, hd, raw = self.c.request("POST", "/api/v1/cars/c1/purpose-rules",
                                     {"from_place": "p1", "to_place": "p11", "days": "sat,sun", "purpose": "private"})
        self.assertEqual(st, 201)
        rid = json.loads(raw)["id"]
        self.c.error("POST", "/api/v1/cars/c1/purpose-rules", 422, "invalid",
                     body={"from_place": "p1", "to_place": "p2", "days": "someday", "purpose": "commute"})
        self.c.json("DELETE", f"/api/v1/cars/c1/purpose-rules/{rid}")
        self.c.error("DELETE", f"/api/v1/cars/c1/purpose-rules/{rid}", 404, "no_such_rule")

    def test_drive_stats(self):
        d = self.c.get(f"/api/v1/cars/c1/drive-stats?from_ms={NOW - 90 * 86_400_000}")
        for k in ("count", "distance_km", "energy_kwh", "median_distance_km", "per_day", "max_speed", "speed_bands",
                  "projection", "top_destinations"):
            self.assertIn(k, d)
        self.assertEqual(set(d["max_speed"]), {"period", "last_30d", "last_7d"})
        self.assertEqual(set(d["max_speed"]["period"]), {"kph", "drive"})
        self.assertAlmostEqual(sum(b["share_pct"] for b in d["speed_bands"]), 100, delta=0.5)
        self.assertEqual(d["top_destinations"][0]["label"], "Home")
        self.assertEqual(sum(p["count"] for p in d["per_day"]), d["count"])
        ex = self.c.get(f"/api/v1/cars/c1/drive-stats?from_ms={NOW - 90 * 86_400_000}&exclude_places=p1")
        self.assertLess(ex["count"], d["count"])

    def test_logbook(self):
        a = NOW - 30 * 86_400_000
        d = self.c.get(f"/api/v1/cars/c1/logbook?from_ms={a}")
        rows = d["rows"]
        self.assertEqual([r["seq"] for r in rows], list(range(1, len(rows) + 1)))
        self.assertEqual(set(d["totals_by_purpose"]), {"business", "private", "commute", "none"})
        self.assertEqual(sum(t["drives"] for t in d["totals_by_purpose"].values()), len(rows))
        self.assertTrue(all(r["purpose_source"] in (None, "owner", "rule") for r in rows))
        self.assertTrue(all((r["purpose"] is None) == (r["purpose_source"] is None) for r in rows))
        st, hd, raw = self.c.request("GET", f"/api/v1/cars/c1/logbook?from_ms={a}&format=csv")
        self.assertTrue(hd["content-type"].startswith("text/csv"))
        self.assertEqual(len(raw.decode("utf-8-sig").strip().splitlines()), len(rows) + 1)
        st, hd, raw = self.c.request("GET", f"/api/v1/cars/c1/logbook?from_ms={a}&format=pdf")
        self.assertEqual(hd["content-type"], "application/pdf")
        self.assertTrue(raw.startswith(b"%PDF-1.4") and raw.rstrip().endswith(b"%%EOF"))
        gaps = self.c.get("/api/v1/cars/c1/logbook")["rows"]
        self.assertTrue(any((r["continuity_gap_km"] or 0) > 1 for r in gaps))  # km driven while no log came in

    def test_trip(self):
        rt = 137
        a = NOW - rt * 86_400_000 - 12 * 3_600_000
        b = a + 5 * 86_400_000
        d = self.c.get(f"/api/v1/cars/c1/trip?from_ms={a}&to_ms={b}")
        self.assertGreater(d["distance"]["drives_km"], 1000)
        self.assertGreater(d["time_ms"]["charging_dc"], 3_600_000)
        self.assertGreater(d["energy_kwh"]["added_dc"], 50)
        self.assertTrue(d["cost"]["by_currency"])
        kinds = {m["kind"] for m in d["markers"]}
        self.assertTrue({"start", "end", "charge", "overnight"} <= kinds, kinds)
        self.assertEqual(set(d["time_ms"]), {"driving", "charging_ac", "charging_dc", "parked_awake", "asleep",
                                             "not_recorded"})

    def test_saved_trips(self):
        lst = self.c.get("/api/v1/trips")["items"]
        self.assertEqual(lst[0]["name"], "Los Angeles in July")
        st, hd, raw = self.c.request("POST", "/api/v1/trips", {"car": "c2", "from_ms": NOW - WEEK, "to_ms": NOW,
                                                              "name": "Last week"})
        self.assertEqual(st, 201)
        t = json.loads(raw)
        self.assertEqual(self.c.json("PATCH", f"/api/v1/trips/{t['id']}", {"name": "The week"})["name"], "The week")
        self.c.error("PATCH", f"/api/v1/trips/{t['id']}", 422, "invalid", body={"from_ms": NOW, "to_ms": NOW - 1})
        self.c.json("DELETE", f"/api/v1/trips/{t['id']}")
        self.c.error("GET", f"/api/v1/trips/{t['id']}", 404, "no_such_trip")
        self.c.error("POST", "/api/v1/trips", 422, "invalid", body={"car": "c9", "from_ms": 1, "to_ms": 2, "name": "x"})


class WatchCannotChange(ServerCase):
    server_args = {"role": "watch"}

    def test_watch_may_save_trips_not_purposes(self):
        x = self.c.get("/api/v1/cars/c1/drives?limit=1")["items"][0]
        self.c.error("PUT", f"/api/v1/cars/c1/drives/{x['id']}/purpose", 403, "not_allowed", body={"purpose": "private"})
        st, _, _ = self.c.request("POST", "/api/v1/trips", {"car": "c1", "from_ms": 1, "to_ms": 2, "name": "Mine"})
        self.assertEqual(st, 201)
        st, hd, raw = self.c.request("GET", f"/api/v1/cars/c1/drives/{x['id']}/track?format=gpx")
        self.assertEqual(st, 200)  # watch accounts may export
