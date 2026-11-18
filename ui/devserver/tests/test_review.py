"""Bugs found by the API review, one test each: odd input that used to answer 500, hidden cars a watch
account could see, filters that disagreed with the rows, figures that did not add up, days that ignored
the clock change, and a large fleet that was slow to search."""

from __future__ import annotations

import base64
import csv
import http.client
import io
import json
import time

from .helpers import DAY, NOW, Running, ServerCase


def forged(cursor, key):
    """A cursor with this server's filter hash but a key made by hand."""
    raw = json.loads(base64.urlsafe_b64decode(cursor + "=" * (-len(cursor) % 4)))
    raw["k"] = key
    return base64.urlsafe_b64encode(json.dumps(raw).encode()).decode().rstrip("=")


class OddInput(ServerCase):
    def test_cursor_made_by_hand_is_400(self):
        for path in ("/api/v1/cars/c1/drives?limit=2", "/api/v1/cars?limit=2", "/api/v1/places?limit=2",
                     "/api/v1/fleet/versions?limit=1", "/api/v1/cars/c1/timeline?limit=2"):
            nxt = self.c.get(path)["next"]
            for key in ("x", [1], {"a": 1}, [["a"], "b"], [None, None]):
                st, hd, raw = self.c.request("GET", f"{path}&after={forged(nxt, key)}")
                self.assertLess(st, 500, f"{path} {key}: {raw[:200]!r}")
                if st == 400:  # a key that compares with this list's keys just gives some page
                    self.assertIn("after", json.loads(raw)["error"]["fields"], path)
            self.c.error("GET", f"{path}&after={forged(nxt, 'x')}", 400, "bad_input")

    def test_bad_content_length_is_400(self):
        conn = http.client.HTTPConnection("127.0.0.1", self.server.port, timeout=10)
        conn.putrequest("POST", "/api/v1/trips")
        conn.putheader("Content-Length", "abc")
        conn.putheader("Cookie", self.c.cookie or "")
        conn.putheader("X-Sentry-Mate", "1")
        conn.endheaders()
        r = conn.getresponse()
        body = json.loads(r.read())
        conn.close()
        self.assertEqual(r.status, 400)
        self.assertEqual(body["error"]["code"], "bad_input")
        self.assertIn("Content-Length", body["error"]["fields"])

    def test_wrong_types_in_bodies_are_400(self):
        trip = {"car": "c1", "from_ms": NOW - 3 * DAY, "to_ms": NOW - DAY, "name": "T"}
        rule = {"from_place": "p1", "to_place": "p11", "days": "sat,sun", "purpose": "private"}
        for wrong in (5, None, True, ["c1"], {"a": 1}):
            e, _ = self.c.error("POST", "/api/v1/trips", 400, "bad_input", dict(trip, car=wrong))
            self.assertIn("car", e["fields"])
        for k in ("from_place", "to_place"):
            for wrong in (5, ["p1"], {"a": 1}):
                e, _ = self.c.error("POST", "/api/v1/cars/c1/purpose-rules", 400, "bad_input", dict(rule, **{k: wrong}))
                self.assertIn(k, e["fields"])
        for k in ("days", "purpose"):
            e, _ = self.c.error("POST", "/api/v1/cars/c1/purpose-rules", 400, "bad_input", dict(rule, **{k: ["x"]}))
            self.assertIn(k, e["fields"])
        for wrong in ([], ["p1"], {"a": 1}, 7):
            e, _ = self.c.error("POST", "/api/v1/places/preview", 400, "bad_input", {"place": wrong, "delete": True})
            self.assertIn("place", e["fields"])
            e, _ = self.c.error("POST", "/api/v1/places/preview", 400, "bad_input",
                                {"id": wrong, "name": "X", "lat": 34.05, "lon": -118.24})
            self.assertIn("id", e["fields"])
        # an id that is not text is just not a car
        self.assertFalse(self.server.app.ds.exists(["c1"]))
        self.assertFalse(self.server.app.ds.exists("c٤"))  # a non-ASCII digit is not car c4


class WatchSeesOnlyItsCars(ServerCase):
    """A watch account must never learn of a hidden car, not even its id in an order or a pin."""

    def setUp(self):
        self.w = self.server.client()
        self.w.json("POST", "/api/v1/session", {"name": "watch", "password": "sentry-mate-dev"})

    def test_car_order_and_pins(self):
        v = self.c.get("/api/v1/settings")["version"]
        self.c.json("PATCH", "/api/v1/settings", {"version": v, "car_order": ["c3", "c2", "c1"]})
        self.assertEqual(self.c.get("/api/v1/settings")["car_order"], ["c3", "c2", "c1"])  # the admin sees all
        self.assertEqual(self.w.get("/api/v1/me/preferences")["car_order"], ["c1"])
        self.assertEqual(self.w.get("/api/v1/settings")["car_order"], ["c1"])
        # the prefs event pushed after the change carries the same filtered list
        self.assertEqual(self.server.app.store.get_prefs(self.server.app.store.accounts["a2"])["car_order"], ["c1"])
        # a car taken off the account's list leaves its pins and order too
        self.w.json("PATCH", "/api/v1/me/preferences", {"pinned_cars": ["c1"], "car_order": ["c1"]})
        acc = self.c.get("/api/v1/accounts/a2")
        self.c.json("PATCH", "/api/v1/accounts/a2", {"version": acc["version"], "cars": ["c2"]})
        try:
            p = self.w.get("/api/v1/me/preferences")
            self.assertEqual(p["pinned_cars"], [])
            self.assertNotIn("c1", p["car_order"])
            self.assertNotIn("c3", p["car_order"])
        finally:
            acc = self.c.get("/api/v1/accounts/a2")
            self.c.json("PATCH", "/api/v1/accounts/a2", {"version": acc["version"], "cars": ["c1"]})

    def test_preview_of_a_place_its_cars_never_used(self):
        seen = {p["id"] for p in self.w.get("/api/v1/places?limit=500")["items"]}
        hidden = [p["id"] for p in self.c.get("/api/v1/places?limit=500")["items"] if p["id"] not in seen]
        self.assertTrue(hidden)
        self.w.error("POST", "/api/v1/places/preview", 422, "invalid", {"place": hidden[0], "delete": True})
        self.w.error("POST", "/api/v1/places/preview", 422, "invalid", {"id": hidden[0]})
        self.w.json("POST", "/api/v1/places/preview", {"place": sorted(seen)[0], "delete": True})


class TimelinePlaceFilter(ServerCase):
    def rows(self, q):
        return self.walk(f"/api/v1/cars/c1/timeline?from_ms={NOW - 20 * DAY}{q}", limit=500)

    @staticmethod
    def shows(row, pid):
        return any(((row.get(side) or {}).get("place") or {}).get("id") == pid for side in ("start", "end"))

    def test_place_filter_follows_the_places_as_they_are_now(self):
        home = self.c.get("/api/v1/places/p1")
        before = self.rows("&place=p1")
        self.assertTrue(before)
        self.assertTrue(all(self.shows(x, "p1") for x in before))
        self.c.json("DELETE", "/api/v1/places/p1")
        try:
            self.assertEqual([x for x in self.rows("") if self.shows(x, "p1")], [])
            self.c.error("GET", f"/api/v1/cars/c1/timeline?place=p1", 400, "bad_input")  # gone: like /drives
            new = self.c.json("POST", "/api/v1/places", {"name": "Home again", "lat": home["lat"], "lon": home["lon"],
                                                          "radius_m": home["radius_m"], "kind": "home"}, expect=201)
            got = self.rows(f"&place={new['id']}")
            want = [x for x in self.rows("") if self.shows(x, new["id"])]
            self.assertTrue(want)
            self.assertEqual([x["id"] for x in got], [x["id"] for x in want])
        finally:
            pass  # this class's server is not shared with other classes


class CapacityFollowsCharges(ServerCase):
    def test_every_reading_is_the_documented_sum(self):
        for car in ("c1", "c2", "c3"):
            buffer = self.c.get(f"/api/v1/cars/{car}/battery")["capacity"]["buffer_kwh"]
            charges = {x["id"]: x for x in self.walk(f"/api/v1/cars/{car}/charges", limit=500)}
            cap = self.walk(f"/api/v1/cars/{car}/battery/capacity", limit=500)
            self.assertTrue(cap)
            for q in cap:
                ch = charges[q["charge"]]
                want = ch["energy_added_kwh"] / (ch["end_soc_pct"] - ch["start_soc_pct"]) * 100 + buffer
                self.assertAlmostEqual(q["full_pack_kwh"], want, delta=0.2, msg=f"{car} {q['charge']} {q['source']}")


class LocalDays(ServerCase):
    """Days are the car's own days: 25 hours on 2026-11-01 in Los Angeles, when the clocks go back."""

    A = 1793516400000 - 3 * DAY  # three days before 2026-11-01 00:00 PDT
    B = 1793516400000 + 4 * DAY

    def check_midnights(self, ts):
        from mockapi.util import local_dt
        self.assertGreaterEqual(len(ts), 6)
        for t in ts:
            d = local_dt(t, "America/Los_Angeles")
            self.assertEqual((d.hour, d.minute), (0, 0), d)
        self.assertIn(25 * 3_600_000, [b - a for a, b in zip(ts, ts[1:])])

    def test_temperatures_daily(self):
        d = self.c.get(f"/api/v1/cars/c1/temperatures?from_ms={self.A}&to_ms={self.B}")
        self.check_midnights([x["t_ms"] for x in d["daily"]])
        self.assertEqual(d["daily"][3]["date"], "2026-11-01")

    def test_battery_level_days(self):
        d = self.c.get(f"/api/v1/cars/c1/battery-level?from_ms={self.A}&to_ms={self.B}&bucket=day")
        self.check_midnights([x["t_ms"] for x in d["buckets"]])
        self.assertEqual(len(d["moving"]["mean"]), len(d["buckets"]))
        self.assertTrue(all(v is not None for v in d["moving"]["mean"]))

    def test_projected_range_days(self):
        d = self.c.get(f"/api/v1/cars/c1/projected-range?from_ms={self.A}&to_ms={self.B}&bucket=day")
        self.check_midnights(d["t_ms"])


class BigFleetFirstCalls(ServerCase):
    """--cars 200000: every fleet page is quick from the very first call, rare filters included."""

    server_args = {"cars": 200_000}

    def test_first_calls_are_quick(self):
        slow = []
        for q in ("/api/v1/cars?sort=name", "/api/v1/cars?sort=soc&dir=asc", "/api/v1/fleet/summary",
                  "/api/v1/fleet/attention", "/api/v1/cars?state=unknown", "/api/v1/cars?search=zzzz",
                  "/api/v1/cars?search=zzzz&sort=soc", "/api/v1/cars?search=plaid&state=asleep&sort=name",
                  "/api/v1/fleet/versions?search=zzzz"):
            t = time.perf_counter()
            d = self.c.get(q)
            dt = time.perf_counter() - t
            if dt > 0.2:
                slow.append(f"{q}: {dt * 1000:.0f} ms")
            if d.get("next"):
                t = time.perf_counter()
                self.c.get(q + ("&" if "?" in q else "?") + f"after={d['next']}")
                if time.perf_counter() - t > 0.2:
                    slow.append(f"{q} page 2")
        self.assertEqual(slow, [])


class SmallFleetFilters(ServerCase):
    """Filtered and sorted pages of synthetic cars hold exactly the cars that match, each once."""

    server_args = {"cars": 3000}

    def test_filtered_walks_match_a_plain_filter(self):
        st, hd, raw = self.c.request("GET", "/api/v1/cars?format=csv")
        every = list(csv.DictReader(io.StringIO(raw.decode("utf-8-sig"))))
        self.assertEqual(len(every), 3000)
        for q, keep in (("search=willow", lambda x: "willow" in x["name"].lower()),
                        ("search=plaid&state=asleep", lambda x: x["now.state"] == "asleep" and "plaid" in (
                            x["name"] + x["model"] + x["vin_prefix"]).lower()),
                        ("state=charging", lambda x: x["now.state"] == "charging"),
                        ("search=7sax", lambda x: "7sax" in x["vin_prefix"].lower())):
            want = {x["id"] for x in every if keep(x)}
            for sort in ("last_activity", "name", "soc", "custom"):
                for d in ("asc", "desc"):
                    got = [x["id"] for x in self.walk(f"/api/v1/cars?{q}&sort={sort}&dir={d}", limit=37)]
                    self.assertEqual(len(got), len(set(got)), (q, sort, d))
                    self.assertEqual(set(got), want, (q, sort, d))
        att = self.walk("/api/v1/fleet/attention", limit=41)
        keys = [(a["since_ms"], a["car"], a["code"]) for a in att]
        self.assertEqual(len(keys), len(set(keys)))
        self.assertEqual([k[0] for k in keys], sorted((k[0] for k in keys), reverse=True))
        st, hd, raw = self.c.request("GET", "/api/v1/fleet/attention?format=csv")
        self.assertEqual(len(list(csv.DictReader(io.StringIO(raw.decode("utf-8-sig"))))), len(att))


__all__ = ["Running"]
