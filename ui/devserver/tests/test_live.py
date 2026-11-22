"""Live values and server-sent events."""

from __future__ import annotations

import http.client
import json
import time

from .helpers import NOW, ServerCase

VALUES = ["speed_kph", "power_kw", "soc_pct", "soc_bms_pct", "usable_soc_pct", "range_km", "ideal_range_km",
          "est_range_km", "energy_remaining_kwh", "odometer_km", "outside_c", "cabin_c", "battery_c",
          "setpoint_left_c", "setpoint_right_c", "lv_v", "gear"]
FLAGS = ["sentry", "sentry_event_guess", "service_mode", "user_present", "keep_climate", "climate",
         "battery_preconditioning", "keep_warm", "accessory_power", "parking_brake", "reduced_range", "trunk", "frunk",
         "liftgate_deg", "charge_port_open", "display_on"]
CHARGE = ["status", "plugged", "kind", "power_kw", "volts_v", "amps_a", "phases", "pilot_a", "limit_source",
          "added_kwh", "limit_pct", "to_full_kwh", "to_full_min"]


class Now(ServerCase):
    def check_shape(self, n):
        for k in ("state", "since_ms", "received_ms", "delay_s", "buses", "driving", "time_zone", "flags", "charge",
                  "location", "tyres_kpa", "tyres_slots_mapped", "tyre_recommended_kpa", "drive", "awake_reasons",
                  "software", "est_basis"):
            self.assertIn(k, n)
        for k in VALUES:
            self.assertIn(k, n)
            self.assertWrapped(n[k], k)
        for k in FLAGS:
            self.assertWrapped(n["flags"][k], k)
        for k in ("fl", "fr", "rl", "rr"):
            self.assertWrapped(n["flags"]["doors"][k])
        for k in CHARGE:
            self.assertWrapped(n["charge"][k], k)
        for k in ("lat", "lon", "accuracy_m", "elevation_m", "heading_deg"):
            self.assertWrapped(n["location"][k], k)
        self.assertEqual(set(n["software"]), {"version", "version_source", "version_since_ms", "changed_since_ms"})
        self.assertEqual([b["bus"] for b in n["buses"]], ["vehicle", "chassis"])

    def test_c1_is_driving(self):
        n = self.c.get("/api/v1/cars/c1/now")
        self.check_shape(n)
        self.assertEqual(n["state"], "driving")
        self.assertTrue(n["driving"])
        self.assertGreater(n["speed_kph"]["value"], 0)
        self.assertEqual(n["gear"]["value"], "D")
        self.assertIsNotNone(n["drive"]["drive_id"])
        self.assertGreater(n["drive"]["distance_km"], 0)
        self.assertLess(NOW - n["at_ms"], 120_000)  # about a minute late
        self.assertGreater(n["delay_s"], 30)
        self.assertEqual(len(n["tyres_kpa"]), 4)
        self.assertEqual(n["tyre_recommended_kpa"]["value"], 290)
        d = self.c.get(f"/api/v1/cars/c1/drives/{n['drive']['drive_id']}")
        self.assertTrue(d["flags"]["in_progress"])
        self.assertIsNone(d["end_ms"])

    def test_c2_is_charging(self):
        n = self.c.get("/api/v1/cars/c2/now")
        self.check_shape(n)
        ch = n["charge"]
        self.assertEqual(n["state"], "charging")
        self.assertEqual(ch["status"]["value"], "charging")
        self.assertEqual(ch["kind"]["value"], "ac")
        self.assertGreater(ch["power_kw"]["value"], 3)
        self.assertGreater(ch["to_full_min"]["value"], 10)
        self.assertGreater(ch["finish_ms"], NOW)
        self.assertLess(n["power_kw"]["value"], 0)  # into the pack
        self.assertIn("charging", n["awake_reasons"])
        c = self.c.get(f"/api/v1/cars/c2/charges/{ch['charge_id']}")
        self.assertTrue(c["flags"]["in_progress"])

    def test_c3_is_silent(self):
        n = self.c.get("/api/v1/cars/c3/now")
        # its newest log ended parked: it stays parked (stale), not unknown
        self.assertEqual(n["state"], "parked")
        self.assertTrue(n["stale"])
        self.assertLess(n["at_ms"], NOW - 2 * 86_400_000)  # values keep their own time
        self.assertIsNone(n["tyres_kpa"])  # the old Model S has no mapped tyre sensors
        self.assertIsNone(n["setpoint_left_c"])

    def test_awake_reasons(self):
        d = self.c.get(f"/api/v1/cars/c1/awake-reasons?from_ms={NOW - 60 * 86_400_000}&limit=500")
        self.assertTrue(d["items"])
        seen = set()
        for s in d["items"]:
            self.assertEqual(set(s), {"start_ms", "end_ms", "buses", "reasons", "avg_power_kw"})
            self.assertLess(s["start_ms"], s["end_ms"])
            seen |= set(s["reasons"])
        self.assertTrue({"sentry", "user_present", "charging"} <= seen, seen)


def read_events(port, cookie, want, seconds=8):
    """Read server-sent events until `want(events)` is true or time runs out."""
    conn = http.client.HTTPConnection("127.0.0.1", port, timeout=seconds)
    conn.request("GET", "/api/v1/events", headers={"Cookie": cookie})
    r = conn.getresponse()
    assert r.status == 200, r.status
    assert r.getheader("Content-Type").startswith("text/event-stream")
    events, cur = [], {}
    deadline = time.time() + seconds
    try:
        while time.time() < deadline and not want(events):
            line = r.fp.readline().decode().rstrip("\n")
            if line.startswith("event: "):
                cur["event"] = line[7:]
            elif line.startswith("data: "):
                cur["data"] = json.loads(line[6:])
            elif line == "" and cur:
                events.append(cur)
                cur = {}
    except TimeoutError:
        pass
    finally:
        conn.close()
    return events


def nows_for(events, car):
    return [e["data"]["now"] for e in events if e["event"] == "now" and e["data"]["car"] == car]


class Events(ServerCase):
    def test_sse_delivers_now_events(self):
        self.c.get("/api/v1/me")
        events = read_events(self.server.port, self.c.cookie, lambda ev: nows_for(ev, "c1") and nows_for(ev, "c2"))
        first = nows_for(events, "c1")[0]
        self.assertEqual(first["state"], "driving")  # the first event for a car carries the whole object
        self.assertIn("location", first)
        self.assertEqual(nows_for(events, "c2")[0]["state"], "charging")

    def test_prefs_event(self):
        import threading
        self.c.get("/api/v1/me")
        t = threading.Timer(0.6, lambda: self.c.json("PATCH", "/api/v1/me/preferences", {"theme": "dark"}))
        t.start()
        events = read_events(self.server.port, self.c.cookie, lambda ev: any(e["event"] == "prefs" for e in ev))
        t.join()
        prefs = [e["data"] for e in events if e["event"] == "prefs"]
        self.assertEqual(prefs[0]["theme"], "dark")


class EventDeltas(ServerCase):
    server_args = {"freeze_clock": False}

    def test_later_events_carry_only_changes(self):
        self.c.get("/api/v1/me")
        events = read_events(self.server.port, self.c.cookie, lambda ev: len(nows_for(ev, "c1")) >= 3)
        c1 = nows_for(events, "c1")
        self.assertGreaterEqual(len(c1), 3)
        later = c1[-1]
        self.assertNotIn("time_zone", later)  # unchanged, so not sent again
        self.assertIn("location", later)  # the car is moving
        self.assertIn("lat", later["location"])


class LiveClock(ServerCase):
    server_args = {"freeze_clock": True}

    def test_values_change_with_time(self):
        a = self.c.get("/api/v1/cars/c1/now")
        self.server.app.clock.advance(30_000)
        b = self.c.get("/api/v1/cars/c1/now")
        self.assertGreater(b["at_ms"], a["at_ms"])
        self.assertNotEqual(a["location"]["lat"]["value"], b["location"]["lat"]["value"])
        self.assertGreater(b["odometer_km"]["value"], a["odometer_km"]["value"])
        # after the drive ends the car is parked at the destination
        self.server.app.clock.advance(3 * 3_600_000)
        c = self.c.get("/api/v1/cars/c1/now")
        self.assertIn(c["state"], ("charging", "parked", "asleep"))
        self.assertEqual(c["location"]["place"]["name"], "Stanford Shopping Center")


class ForcedRows(ServerCase):
    server_args = {"freeze_clock": False, "rows_every_s": 1}

    def test_rows_events_at_the_set_pace(self):
        self.c.get("/api/v1/me")
        start = time.time()
        events = read_events(self.server.port, self.c.cookie,
                             lambda ev: len([e for e in ev if e["event"] == "rows" and e["data"]["car"] == "c1"]) >= 3)
        took = time.time() - start
        rows = [e["data"] for e in events if e["event"] == "rows" and e["data"]["car"] == "c1"]
        self.assertGreaterEqual(len(rows), 3, events)
        self.assertLess(took, 5)  # one a second, not one per piece of history
        self.assertEqual(rows[0]["kinds"], ["charges", "drives", "states", "updates", "visits"])
        self.assertLess(rows[0]["from_ms"], rows[0]["to_ms"])
        self.assertIn("c2", {e["data"]["car"] for e in events if e["event"] == "rows"})


class NoForcedRows(ServerCase):
    server_args = {"freeze_clock": True}

    def test_no_rows_events_without_the_flag(self):
        self.c.get("/api/v1/me")
        events = read_events(self.server.port, self.c.cookie, lambda ev: False, seconds=2)
        self.assertTrue(any(e["event"] == "now" for e in events))
        self.assertFalse(any(e["event"] == "rows" for e in events))
