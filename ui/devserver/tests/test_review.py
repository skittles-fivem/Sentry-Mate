"""Bugs found by the API review, one test each: odd input that used to answer 500, hidden cars a watch
account could see, filters that disagreed with the rows, figures that did not add up, days that ignored
the clock change, and a large fleet that was slow to search."""

from __future__ import annotations

import base64
import csv
import http.client
import io
import json
import socket
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

    def test_head_on_the_event_stream_has_no_body(self):
        # it used to start the stream: a HEAD answer with a body that never ends
        sock = socket.create_connection(("127.0.0.1", self.server.port), timeout=1.5)
        sock.sendall(b"HEAD /api/v1/events HTTP/1.1\r\nHost: x\r\nCookie: " + (self.c.cookie or "").encode()
                     + b"\r\n\r\n")
        data = b""
        try:
            while True:
                chunk = sock.recv(4096)
                if not chunk:
                    break
                data += chunk
        except socket.timeout:
            pass
        sock.close()
        head, _, body = data.partition(b"\r\n\r\n")
        self.assertTrue(head.startswith(b"HTTP/1.1 200"), head[:40])
        self.assertIn(b"text/event-stream", head)
        self.assertEqual(body, b"")

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


class Tracks(ServerCase):
    def test_never_more_points_than_asked(self):
        for q in ("max_points=3000&cars=c1", f"max_points=2400&cars=c1&from_ms={NOW - 400 * DAY}",
                  f"max_points=100&from_ms={NOW - 3 * DAY}", "max_points=10000"):
            d = self.c.get(f"/api/v1/tracks?{q}")
            want = int(q.split("max_points=")[1].split("&")[0])
            self.assertLessEqual(d["points"], want, q)
            self.assertEqual(d["points"], sum(len(x["t_ms"]) for x in d["drives"]), q)
            self.assertTrue(all(len(x["t_ms"]) == len(x["lat"]) == len(x["lon"]) == len(x["accuracy_m"])
                                for x in d["drives"]), q)
        # a drive's points are its own track's points (the same positions as the drive track)
        d = self.c.get(f"/api/v1/tracks?cars=c1&from_ms={NOW - 2 * DAY}&max_points=200000")["drives"][0]
        geo = json.loads(self.c.request("GET", f"/api/v1/cars/c1/drives/{d['drive']}/track")[2])
        pts = {t: (c[1], c[0]) for f in geo["features"] for t, c in zip(f["properties"]["t_ms"],
                                                                       f["geometry"]["coordinates"])}
        for t, la, lo in zip(d["t_ms"], d["lat"], d["lon"]):
            if t in pts:
                self.assertEqual(pts[t], (la, lo))


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
        self.c.json("PATCH", "/api/v1/accounts/a2", {"version": acc["version"], "cars": ["c2"], "confirm_password": "sentry-mate-dev"})
        try:
            p = self.w.get("/api/v1/me/preferences")
            self.assertEqual(p["pinned_cars"], [])
            self.assertNotIn("c1", p["car_order"])
            self.assertNotIn("c3", p["car_order"])
        finally:
            acc = self.c.get("/api/v1/accounts/a2")
            self.c.json("PATCH", "/api/v1/accounts/a2", {"version": acc["version"], "cars": ["c1"], "confirm_password": "sentry-mate-dev"})

    def test_preview_of_a_place_its_cars_never_used(self):
        seen = {p["id"] for p in self.w.get("/api/v1/places?limit=500")["items"]}
        hidden = [p["id"] for p in self.c.get("/api/v1/places?limit=500")["items"] if p["id"] not in seen]
        self.assertTrue(hidden)
        self.w.error("POST", "/api/v1/places/preview", 422, "invalid", {"place": hidden[0], "delete": True})
        self.w.error("POST", "/api/v1/places/preview", 422, "invalid", {"id": hidden[0]})
        self.w.json("POST", "/api/v1/places/preview", {"place": sorted(seen)[0], "delete": True})


class PeriodEdges(ServerCase):
    """Rows belong to a period by their start and keep their own start, end and id (docs: Conventions)."""

    def test_states_are_not_cut_at_the_edges(self):
        a = NOW - 10 * DAY + 12_345  # inside a state span
        whole = {x["id"]: x for x in self.walk(f"/api/v1/cars/c1/states?from_ms={NOW - 12 * DAY}", limit=500)}
        part = self.walk(f"/api/v1/cars/c1/states?from_ms={a}&to_ms={a + 2 * DAY}", limit=500)
        self.assertTrue(part)
        for x in part:
            self.assertGreaterEqual(x["start_ms"], a)
            self.assertEqual(whole.get(x["id"]), x)

    def test_awake_spans_by_start(self):
        a = NOW - 10 * DAY + 12_345
        whole = {x["start_ms"]: x for x in self.walk(f"/api/v1/cars/c1/awake-reasons?from_ms={NOW - 12 * DAY}",
                                                      limit=500)}
        for x in self.walk(f"/api/v1/cars/c1/awake-reasons?from_ms={a}&to_ms={a + 2 * DAY}", limit=500):
            self.assertEqual(whole.get(x["start_ms"]), x)

    def test_default_windows_end_at_to_ms(self):
        b = NOW - 5 * DAY
        for path, window in (("series?fields=soc_pct", DAY), ("signals?series=soc_display", 3_600_000),
                             ("activity", DAY), ("trip", 7 * DAY)):
            d = self.c.get(f"/api/v1/cars/c1/{path}{'&' if '?' in path else '?'}to_ms={b}")
            if "t_ms" in d:
                self.assertTrue(d["t_ms"], path)
                self.assertGreaterEqual(d["t_ms"][0], b - window - 3_600_000, path)
                self.assertLess(d["t_ms"][-1], b, path)
            else:
                self.assertEqual(d["from_ms"], b - window, path)
                self.assertEqual(d["to_ms"], b, path)
        lvl = self.c.get(f"/api/v1/cars/c1/battery-level?to_ms={b}")
        self.assertEqual(lvl["from_ms"], b - 30 * DAY)
        self.assertTrue(lvl["buckets"])

    def test_place_delete_version_must_be_a_number(self):
        e, _ = self.c.error("DELETE", "/api/v1/places/p1?version=abc", 400, "bad_input")
        self.assertIn("version", e["fields"])
        self.c.error("DELETE", "/api/v1/places/p1?version=999", 409, "stale_version")


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
            factor = self.c.get(f"/api/v1/cars/{car}/efficiency")["factor"]["rated_wh_per_km"]
            for q in cap:
                ch = charges[q["charge"]]
                stored = (ch["end_range_km"] - ch["start_range_km"]) * factor / 1000
                want = stored / (ch["end_soc_pct"] - ch["start_soc_pct"]) * 100 + buffer
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

    def test_csv_has_the_order_of_the_pages(self):
        self.c.json("PATCH", "/api/v1/me/preferences", {"pinned_cars": ["c400", "c2"], "car_order": ["c7", "c3"]})
        try:
            for q in ("sort=last_activity", "sort=name", "sort=soc&dir=asc", "sort=custom", "sort=custom&dir=desc",
                      "search=a&sort=name&dir=desc", "state=asleep&sort=soc"):
                pages = [x["id"] for x in self.walk(f"/api/v1/cars?{q}", limit=97)]
                st, hd, raw = self.c.request("GET", f"/api/v1/cars?{q}&format=csv")
                rows = [x["id"] for x in csv.DictReader(io.StringIO(raw.decode("utf-8-sig")))]
                self.assertEqual(rows, pages, q)
        finally:
            self.c.json("PATCH", "/api/v1/me/preferences", {"pinned_cars": [], "car_order": []})



class AccountCarsChange(ServerCase):
    """Taking a car off a watch account, or giving one, reaches that account's open streams."""

    def drain(self, s):
        out = []
        while not s.q.empty():
            out.append(s.q.get_nowait())
        return out

    def test_streams_hear_of_cars_added_and_removed(self):
        st, hub = self.server.app.store, self.server.app.events
        acc = st.accounts["a2"]
        self.assertEqual(acc["cars"], ["c1"])
        s = hub.subscribe({"account": acc, "token": "test-stream"})
        try:
            hub.tick()
            self.assertIn(("now", "c1"), [(e, d["car"]) for e, d in self.drain(s) if e == "now"])
            v = self.c.get("/api/v1/accounts/a2")["version"]
            self.c.json("PATCH", "/api/v1/accounts/a2", {"version": v, "cars": ["c2"], "confirm_password": "sentry-mate-dev"})
            got = self.drain(s)
            fleet = sorted((d["car"], d["change"]) for e, d in got if e == "fleet")
            self.assertEqual(fleet, [("c1", "removed"), ("c2", "added")])
            prefs = [d for e, d in got if e == "prefs"]
            self.assertEqual(len(prefs), 1)
            self.assertNotIn("c1", prefs[0]["car_order"])
            # given back later, the car's first `now` on this stream is whole again, not a delta
            v = self.c.get("/api/v1/accounts/a2")["version"]
            self.c.json("PATCH", "/api/v1/accounts/a2", {"version": v, "cars": ["c1", "c2"], "confirm_password": "sentry-mate-dev"})
            self.drain(s)
            hub.tick()
            first = [d["now"] for e, d in self.drain(s) if e == "now" and d["car"] == "c1"]
            self.assertTrue(first and "state" in first[0] and "tyres_kpa" in first[0], first[:1])
        finally:
            hub.unsubscribe(s)
            v = self.c.get("/api/v1/accounts/a2")["version"]
            self.c.json("PATCH", "/api/v1/accounts/a2", {"version": v, "cars": ["c1"], "confirm_password": "sentry-mate-dev"})


class FleetVersionsSearch(ServerCase):
    """A search of the fleet's versions pages through exactly the cars that match, each once."""

    server_args = {"cars": 3000}

    def test_search_walk_matches_a_plain_filter(self):
        every = self.walk("/api/v1/fleet/versions", limit=500)
        self.assertEqual(len(every), 3000)
        for q in ("2026.38", "willow", "c", "zzzz"):
            want = [x["car"] for x in every if q in (x["version"] or "").lower() or q in x["name"].lower()]
            got = [x["car"] for x in self.walk(f"/api/v1/fleet/versions?search={q}", limit=37)]
            self.assertEqual(got, want, q)


class ConcurrentWrites(ServerCase):
    """Two changes sent at once with the same `version`: one wins, the other gets 409, never both 200."""

    def test_same_version_at_once(self):
        import sys
        import threading
        old = sys.getswitchinterval()
        sys.setswitchinterval(1e-6)  # switch threads as often as possible, so a gap between check and change shows
        self.addCleanup(sys.setswitchinterval, old)
        for path, field in (("/api/v1/cars/c1", "name"), ("/api/v1/accounts/a2", "name"),
                            ("/api/v1/places/p2", "name"), ("/api/v1/cars/c1/settings", "capacity_new_kwh")) * 3:
            v = self.c.get(path)["version"]
            got = []
            go = threading.Barrier(24)

            def one(i):
                c = self.server.client()
                c.cookie = self.c.cookie
                go.wait()
                st, _, _ = c.request("PATCH", path, {"version": v, field: f"Name {i}" if field == "name" else 70 + i})
                got.append(st)

            ts = [threading.Thread(target=one, args=(i,)) for i in range(24)]
            for t in ts:
                t.start()
            for t in ts:
                t.join()
            self.assertEqual(sorted(got), [200] + [409] * 23, path)
            self.assertEqual(self.c.get(path)["version"], v + 1, path)


class SlowReader(ServerCase):
    """A stream whose reader falls far behind is ended (it reconnects and reloads), never left with holes."""

    def test_full_queue_ends_the_stream(self):
        hub = self.server.app.events
        s = hub.subscribe({"account": self.server.app.store.accounts["a1"], "token": "slow"})
        try:
            for i in range(1005):
                s.push("notice", {"n": i})
            self.assertTrue(s.closed)
            n = s.q.qsize()
            s.push("notice", {"n": "late"})
            self.assertEqual(s.q.qsize(), n)
        finally:
            hub.unsubscribe(s)

    def test_sign_out_ends_the_stream_at_once(self):
        import http.client
        c = self.server.client()
        st, _, _ = c.request("POST", "/api/v1/session", {"name": "watch", "password": "sentry-mate-dev"})
        self.assertEqual(st, 200)
        conn = http.client.HTTPConnection("127.0.0.1", self.server.port, timeout=10)
        conn.request("GET", "/api/v1/events", headers={"Cookie": c.cookie})
        resp = conn.getresponse()
        self.assertEqual(resp.status, 200)
        resp.fp.readline()
        t = time.perf_counter()
        c.request("DELETE", "/api/v1/session")
        while resp.fp.readline():
            pass
        self.assertLess(time.perf_counter() - t, 3)
        conn.close()


class SeriesPointCount(ServerCase):
    """Aligned buckets start before from_ms, yet a series never holds more points than max_points."""

    def test_never_more_than_max_points(self):
        from .helpers import DAY, NOW
        for f, t, mp in ((NOW - 400 * DAY, NOW, 2), (NOW - 400 * DAY, NOW, 7), (NOW - 1_000_500, NOW, 1000),
                         (NOW - 3 * DAY + 123, NOW, 3), (NOW - 30 * DAY - 7, NOW - 7, 720)):
            d = self.c.get(f"/api/v1/cars/c1/series?fields=soc_pct,power_kw&from_ms={f}&to_ms={t}&max_points={mp}")
            self.assertLessEqual(len(d["t_ms"]), mp, (f, t, mp, d["bucket_ms"]))
            self.assertLessEqual(d["t_ms"][0], f)
            self.assertGreater(d["t_ms"][0] + d["bucket_ms"], f)
            self.assertGreater(d["t_ms"][-1] + d["bucket_ms"], t - 1)


class SignalsLatestAndSeries(ServerCase):
    def test_every_signal_marked_as_charted_can_be_charted(self):
        rows = self.walk("/api/v1/cars/c1/signals/latest")
        self.assertTrue(rows)
        for x in rows:
            self.assertIn("has_series", x)
            st, _, _ = self.c.request("GET", f"/api/v1/cars/c1/signals?series={x['series']}")
            self.assertEqual(st, 200 if x["has_series"] else 400, x["series"])


class CarTimeZoneChange(ServerCase):
    """After an admin sets a car's time zone, everything grouped by local day or hour uses the new zone."""

    def test_days_follow_the_new_zone(self):
        from datetime import datetime
        from zoneinfo import ZoneInfo
        v = self.c.get("/api/v1/cars/c1")["version"]
        self.c.json("PATCH", "/api/v1/cars/c1", {"version": v, "time_zone": "Asia/Tokyo"})
        try:
            tz = ZoneInfo("Asia/Tokyo")
            days = self.c.get("/api/v1/visits/days?cars=c1&limit=200")["items"]
            self.assertTrue(days)
            for x in days:
                self.assertEqual(x["date"], datetime.fromtimestamp(x["first_ms"] / 1000, tz).date().isoformat(), x)
            visits = self.c.get("/api/v1/visits?cars=c1&limit=20")["items"]
            self.assertEqual({x.get("time_zone", "Asia/Tokyo") for x in visits}, {"Asia/Tokyo"})
            hm = self.c.get("/api/v1/cars/c1/charging-stats")["heatmap"]
            charges = self.walk("/api/v1/cars/c1/charges", limit=500)
            want = [[0] * 24 for _ in range(7)]
            for ch in charges:
                t = datetime.fromtimestamp(ch["start_ms"] / 1000, tz)
                want[t.weekday()][t.hour] += 1
            self.assertEqual(hm["start"], want)
        finally:
            v = self.c.get("/api/v1/cars/c1")["version"]
            self.c.json("PATCH", "/api/v1/cars/c1", {"version": v, "time_zone": "America/Los_Angeles"})

    def test_gpx_has_the_name_given(self):
        import xml.dom.minidom
        v = self.c.get("/api/v1/cars/c1")["version"]
        self.c.json("PATCH", "/api/v1/cars/c1", {"version": v, "name": "Rosie <&> \"Y\""})
        try:
            d = self.c.get("/api/v1/cars/c1/drives?limit=2")["items"][1]["id"]
            st, _, raw = self.c.request("GET", f"/api/v1/cars/c1/drives/{d}/track?format=gpx")
            self.assertEqual(st, 200)
            doc = xml.dom.minidom.parseString(raw)
            name = doc.getElementsByTagName("trk")[0].getElementsByTagName("name")[0].firstChild.data
            self.assertTrue(name.startswith('Rosie <&> "Y"'), name)
        finally:
            v = self.c.get("/api/v1/cars/c1")["version"]
            self.c.json("PATCH", "/api/v1/cars/c1", {"version": v, "name": "Comet"})


class CsvFormulas(ServerCase):
    """Text that a spreadsheet would run as a formula is written with a leading ' in CSV; numbers stay numbers."""

    def test_formula_text_is_defused(self):
        p = self.c.json("POST", "/api/v1/places", {"name": "=HYPERLINK(\"x\")", "lat": 37.7, "lon": -122.4},
                        expect=201)
        try:
            st, _, raw = self.c.request("GET", "/api/v1/places?format=csv&limit=500")
            rows = list(csv.DictReader(io.StringIO(raw.decode("utf-8-sig"))))
            mine = [r for r in rows if r["id"] == p["id"]][0]
            self.assertEqual(mine["name"], "'=HYPERLINK(\"x\")")
            self.assertEqual(mine["lon"], "-122.4")
        finally:
            self.c.json("DELETE", f"/api/v1/places/{p['id']}")


class SignInSpray(ServerCase):
    """Wrong passwords spread over many names from one address are throttled too, not only per name."""

    def test_many_names_one_address(self):
        c = self.server.client()
        codes = []
        for i in range(40):
            st, _, _ = c.request("POST", "/api/v1/session", {"name": f"guess{i}", "password": "nope"})
            codes.append(st)
        # five from one address, whatever the names (the real server's count, not thirty)
        self.assertEqual(codes[:5], [401] * 5)
        self.assertEqual(set(codes[5:]), {429})
        self.server.app.store.throttle.clear()
        st, _, _ = c.request("POST", "/api/v1/session", {"name": "admin", "password": "sentry-mate-dev"})
        self.assertEqual(st, 200)


class SetupCodeGuessing(ServerCase):
    """Guessing the one-time set-up code is throttled like a password."""

    server_args = {"needs_setup": True}

    def test_wrong_codes_are_throttled(self):
        c = self.server.client()
        codes = [c.request("POST", "/api/v1/setup", {"code": f"SETUP-{i:04d}", "name": "owner",
                                                      "password": "a-long-password"})[0] for i in range(7)]
        # the accounts throttle, as on the real server: five wrong codes from one address
        self.assertEqual(codes, [403] * 5 + [429] * 2)
        self.server.app.store.throttle.clear()
        d = c.json("POST", "/api/v1/setup", {"code": "SETUP-DEV-0000", "name": "owner", "password": "a-long-password"})
        self.assertEqual(d["status"], "done")
