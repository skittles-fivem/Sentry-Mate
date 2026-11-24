"""Timeline, states and the activity strip."""

from __future__ import annotations

from .helpers import NOW, ServerCase

ROW = {"id", "kind", "start_ms", "end_ms", "start", "end", "odometer_km", "distance_km", "duration_ms", "soc_end_pct",
       "soc_delta_pct", "range_end_km", "range_delta_km", "energy_delta_kwh", "energy_basis", "outside_avg_c", "cost",
       "ref", "parked_ms", "awake_reasons", "gap_reason", "source"}


class Timeline(ServerCase):
    def test_rows(self):
        d = self.c.get("/api/v1/cars/c1/timeline?limit=100")
        kinds = {x["kind"] for x in d["items"]}
        self.assertTrue({"drive", "parked", "charge"} <= kinds)
        for x in d["items"]:
            self.assertTrue(ROW <= set(x), ROW - set(x))
            self.assertEqual(x["id"], f"{x['kind']}:{x['start_ms']}")
        first = d["items"][0]
        self.assertEqual(first["kind"], "drive")
        self.assertIsNone(first["end_ms"])  # in progress
        parked = next(x for x in d["items"] if x["kind"] == "parked")
        self.assertEqual(set(parked["parked_ms"]), {"asleep", "awake", "awake_vehicle_bus_only", "unknown"})
        charge = next(x for x in d["items"] if x["kind"] == "charge")
        self.assertIn("charge", charge["ref"])
        self.assertGreater(charge["energy_delta_kwh"], 0)

    def test_all_gap_kinds_and_updates(self):
        reasons = set()
        for car in ("c1", "c2", "c3"):
            g = self.c.get(f"/api/v1/cars/{car}/timeline?kinds=gap&limit=500")["items"]
            reasons |= {x["gap_reason"] for x in g}
            self.assertTrue(all(x["kind"] == "gap" for x in g))
        # every word the server has for a gap (a log left out is said by its own reason)
        self.assertEqual(reasons, {"no_log", "not_recorded", "failed_check", "unreadable", "other_vin",
                                   "dropped_frames"})
        u = self.c.get("/api/v1/cars/c1/timeline?kinds=update&limit=50")["items"]
        self.assertEqual(len(u), 3)  # c1 has three software updates
        self.assertTrue(all("update" in x["ref"] for x in u))

    def test_filters(self):
        q = self.c.get("/api/v1/cars/c1/timeline?q=stanford&limit=500")["items"]
        self.assertTrue(q)
        self.assertTrue(all("stanford" in str(x).lower() for x in q))
        # q also finds the words a page shows for the codes
        sw = self.c.get("/api/v1/cars/c1/timeline?q=software&limit=500")["items"]
        self.assertTrue(sw)
        self.assertTrue(all(x["kind"] == "update" or "software" in str(x).lower() for x in sw))
        for word, check in (("gap in the data", lambda x: x["kind"] == "gap"),
                            ("someone in the car", lambda x: "user_present" in (x.get("awake_reasons") or []))):
            got = self.c.get(f"/api/v1/cars/c1/timeline?q={word.replace(' ', '+')}&limit=500")["items"]
            self.assertTrue(got, word)
            self.assertTrue(all(check(x) for x in got), word)
        p = self.c.get("/api/v1/cars/c1/timeline?place=p5&limit=500")["items"]
        self.assertTrue(p)
        self.c.error("GET", "/api/v1/cars/c1/timeline?kinds=flight", 400, "bad_input")

    def test_teslamate_rows_and_overlap(self):
        a = NOW - 405 * 86_400_000
        b = NOW - 390 * 86_400_000
        rows = self.walk(f"/api/v1/cars/c3/timeline?from_ms={a}&to_ms={b}&kinds=drive", limit=200)
        sources = {x["source"] for x in rows}
        self.assertEqual(sources, {"can", "teslamate"})
        # where both exist only one side is listed
        starts = sorted(x["start_ms"] for x in rows)
        self.assertEqual(len(starts), len(set(starts)))


class States(ServerCase):
    def test_states(self):
        d = self.c.get(f"/api/v1/cars/c1/states?from_ms={NOW - 7 * 86_400_000}&limit=500")
        for x in d["items"]:
            self.assertIn(x["state"], ("online", "asleep", "unknown"))
            self.assertTrue(set(x) >= {"id", "state", "start_ms", "end_ms", "buses", "source"})
        self.assertTrue(any(x["buses"] == ["vehicle"] for x in d["items"]))  # vehicle bus only
        self.assertEqual(d["current"]["state"], "online")
        self.assertEqual(set(d["share_pct"]), {"parked", "asleep", "awake", "unknown"})
        self.assertGreater(d["share_pct"]["asleep"], 30)
        # parked is a share of the known time, so it and driving make up the known time: it is at least
        # the asleep share scaled up to the known time
        sh = d["share_pct"]
        self.assertLessEqual(sh["parked"], 100)
        self.assertGreaterEqual(sh["parked"] + 0.1, 100 * sh["asleep"] / (100 - sh["unknown"]))
        c3 = self.c.get("/api/v1/cars/c3/states?limit=5")
        self.assertEqual(c3["current"]["state"], "unknown")

    def test_activity(self):
        d = self.c.get("/api/v1/cars/c2/activity")
        items = d["items"]
        self.assertTrue(items)
        kinds = {x["kind"] for x in items}
        self.assertIn("charging", kinds)
        for a, b in zip(items, items[1:]):
            self.assertEqual(a["end_ms"], b["start_ms"])  # one unbroken strip
        self.assertEqual(items[0]["start_ms"], d["from_ms"])
        allowed = {"driving", "charging", "online", "online_vehicle_bus_only", "asleep", "unknown", "updating"}
        self.assertTrue(kinds <= allowed)
