"""Server, settings and health: server defaults (checks, version, what follows from them), maps and
address lookup, notices, about, per-car data health, rebuilds, CAN and TeslaMate overlaps, and
storage that adds up from the cars to the server."""

from __future__ import annotations

import threading

from .helpers import DAY, NOW, ServerCase
from .test_live import read_events


class OutboundProxy(ServerCase):
    def test_proxy(self):
        s = self.c.get("/api/v1/settings")
        t = self.c.json("PATCH", "/api/v1/settings", {"version": s["version"], "outbound": {"proxy": "http://proxy.lan:3128"}})
        self.assertEqual(t["outbound"]["proxy"], "http://proxy.lan:3128")
        e, _ = self.c.error("PATCH", "/api/v1/settings", 422, "invalid",
                            body={"version": t["version"], "outbound": {"proxy": "ftp://proxy.lan"}})
        self.assertIn("outbound.proxy", e["fields"])
        u = self.c.json("PATCH", "/api/v1/settings", {"version": t["version"], "outbound": {"proxy": None}})
        self.assertIsNone(u["outbound"]["proxy"])


class Settings(ServerCase):
    def test_1_checks(self):
        s = self.c.get("/api/v1/settings")
        self.assertEqual(s["version"], 1)
        e, _ = self.c.error("PATCH", "/api/v1/settings", 400, "bad_input",
                            body={"version": 1, "units": "km", "colour": "red", "release_check": {"enabled": "yes"}})
        for f in ("units", "colour", "release_check.enabled"):
            self.assertIn(f, e["fields"])
        e, _ = self.c.error("PATCH", "/api/v1/settings", 422, "invalid",
                            body={"version": 1, "units": {"distance": "furlong"}, "range": "far",
                                  "tiles": {"mode": "online", "online_url": "https://tiles.example/x.png"},
                                  "geocode": {"mode": "online"}, "host_names": ["bad host"],
                                  "car_order": ["c1", "c99"], "outbound": {"proxy": "http://me:pw@proxy.lan:3128"}})
        for f in ("units.distance", "range", "tiles.online_url", "geocode.url", "host_names.0", "car_order",
                  "outbound.proxy"):
            self.assertIn(f, e["fields"])
        e, _ = self.c.error("PATCH", "/api/v1/settings", 409, "stale_version", body={"version": 7, "range": "ideal"})
        self.assertEqual(e["version"], 1)
        self.assertEqual(self.c.get("/api/v1/settings"), s)  # nothing saved
        self.assertIsNone(s["outbound"]["proxy"])

    def test_2_defaults_follow(self):
        watch = self.server.client()
        watch.json("POST", "/api/v1/session", {"name": "watch", "password": "sentry-mate-dev"})
        self.c.get("/api/v1/me")
        got = {}

        def change():
            got["d"] = self.c.json("PATCH", "/api/v1/settings", {"version": 1, "units": {"pressure": "psi"},
                                                                 "range": "ideal", "car_order": ["c2", "c1"]})
        t = threading.Timer(0.6, change)
        t.start()
        events = read_events(self.server.port, self.c.cookie, lambda ev: any(e["event"] == "prefs" for e in ev))
        t.join()
        self.assertEqual(got["d"]["version"], 2)
        pushed = [e["data"] for e in events if e["event"] == "prefs"][0]
        self.assertEqual(pushed["units"]["pressure"], "psi")
        p = self.c.get("/api/v1/me/preferences")
        self.assertEqual((p["units"], p["range"], p["car_order"]),
                         ({"distance": "km", "temperature": "c", "pressure": "psi"}, "ideal", ["c2", "c1"]))
        # the watch account chose its own units and range: those stay
        w = watch.get("/api/v1/me/preferences")
        self.assertEqual((w["units"]["distance"], w["units"]["pressure"], w["range"]), ("mi", "psi", "ideal"))
        self.assertIn("settings.update", [a["action"] for a in self.c.get("/api/v1/audit?limit=3")["items"]])

    def test_3_maps_geocode_notices_about(self):
        m = self.c.get("/api/v1/maps/config")
        self.assertEqual(m["tiles"]["mode"], "online")
        self.assertIn("{z}", m["tiles"]["online_template"])
        g = self.c.get("/api/v1/geocode/status")
        self.assertEqual(g["mode"], "offline")
        self.assertTrue(g["offline_data"]["present"])
        n = {x["code"]: x for x in self.c.get("/api/v1/notices")["items"]}
        # a certificate the server makes is valid until 4096: only the owner's own expires
        self.assertNotIn("certificate_expiring", n)
        self.server.app.store.cert_own = True
        try:
            n = {x["code"]: x for x in self.c.get("/api/v1/notices")["items"]}
        finally:
            self.server.app.store.cert_own = False
        self.assertEqual(n["certificate_expiring"]["detail"]["days"], 12)
        self.assertIn("12 days", n["certificate_expiring"]["message"])
        self.assertNotIn("release_available", n)  # the release check is off
        v = self.c.get("/api/v1/settings")["version"]
        self.c.json("PATCH", "/api/v1/settings", {"version": v, "release_check": {"enabled": True},
                                                  "tiles": {"mode": "offline"}})
        r = {x["code"]: x for x in self.c.get("/api/v1/notices")["items"]}["release_available"]
        self.assertTrue(r["detail"]["notes_url"].startswith("https://"))
        m = self.c.get("/api/v1/maps/config")
        self.assertEqual((m["tiles"]["mode"], m["tiles"]["online_template"]), ("offline", None))
        a = self.c.get("/api/v1/about")
        self.assertEqual(a["api_version"], "1")
        self.assertEqual(a["ui_pack"]["name"], "Instrument")
        self.assertTrue(a["licences"] and a["data_credits"])
        self.assertEqual(a["licence"], {"name": None, "url": None})  # not chosen yet


class Health(ServerCase):
    def test_health(self):
        c1 = self.c.get("/api/v1/cars/c1/health")
        self.assertTrue(c1["healthy"])
        self.assertEqual(c1["reasons"], [])
        self.assertEqual(c1["last_log_trust"]["result"], "verified")
        self.assertEqual(len(c1["keys"]), 2)
        self.assertTrue(c1["dropped_frames"])  # a drive with lost frames and a dropped-frames gap
        c3 = self.c.get("/api/v1/cars/c3/health")
        self.assertFalse(c3["healthy"])
        self.assertTrue({"upload_silence", "trust_failed"} <= set(c3["reasons"]))
        self.assertEqual(c3["last_log_trust"]["result"], "failed")
        self.assertIsNone(c3["unknown_spans"][-1]["end_ms"])  # still quiet now
        self.assertLess(c3["last_upload_ms"], NOW - 50 * 3_600_000)
        old = self.c.get(f"/api/v1/cars/c1/health?from_ms={NOW - 400 * DAY}")
        self.assertGreaterEqual(len(old["unknown_spans"]), len(c1["unknown_spans"]))

    def test_rebuild(self):
        self.c.error("POST", "/api/v1/cars/c1/rebuild", 400, "bad_input", body={"from_ms": "yesterday"})
        self.c.error("POST", "/api/v1/cars/c1/rebuild", 422, "invalid", body={"from_ms": NOW, "to_ms": NOW - DAY})
        d = self.c.json("POST", "/api/v1/cars/c1/rebuild", {"from_ms": NOW - 3 * DAY})
        self.assertEqual(d["status"], "done")
        self.assertEqual(d["kinds"], ["drives", "charges", "states", "visits"])
        h = self.c.get("/api/v1/cars/c1/health")
        self.assertTrue(h["rebuild_pending"])
        self.assertIn("rebuild_pending", h["reasons"])
        self.server.app.clock.advance(6000)
        self.assertFalse(self.c.get("/api/v1/cars/c1/health")["rebuild_pending"])
        self.assertEqual(self.c.get("/api/v1/audit?limit=1")["items"][0]["action"], "car.rebuild")

    def test_overlaps(self):
        o = self.c.get("/api/v1/cars/c3/overlaps")["items"]
        self.assertEqual(len(o), 1)
        span = o[0]
        self.assertEqual(span["shown"], "can")
        self.assertEqual(self.c.get("/api/v1/cars/c1/overlaps")["items"], [])
        q = f"/api/v1/cars/c3/drives?from_ms={span['start_ms']}&to_ms={span['end_ms']}&limit=500"
        self.assertEqual({x["source"] for x in self.c.get(q)["items"]}, {"can"})
        self.c.error("PATCH", "/api/v1/cars/c3/overlaps", 404, "no_such_overlap", body={"id": "overlap:1", "shown": "can"})
        self.c.error("PATCH", "/api/v1/cars/c3/overlaps", 422, "invalid", body={"id": span["id"], "shown": "both"})
        d = self.c.json("PATCH", "/api/v1/cars/c3/overlaps", {"id": span["id"], "shown": "teslamate"})
        self.assertEqual(d["changed"]["shown"], "teslamate")
        rows = self.c.get(q)["items"]
        self.assertEqual({x["source"] for x in rows}, {"teslamate"})
        self.assertEqual(len(rows), span["teslamate"]["drives"])
        self.c.json("PATCH", "/api/v1/cars/c3/overlaps", {"id": span["id"], "shown": "can"})
        self.assertEqual({x["source"] for x in self.c.get(q)["items"]}, {"can"})


class Storage(ServerCase):
    def test_storage_adds_up(self):
        per_car = {}
        for car in ("c1", "c2", "c3"):
            s = self.c.get(f"/api/v1/cars/{car}/storage?limit=500")
            self.assertEqual(s["total_bytes"], sum(k["bytes"] for k in s["kinds"]))
            per_car[car] = {k["kind"]: k["bytes"] for k in s["kinds"]}
            days = [x["date"] for x in s["items"]]
            self.assertEqual(days, sorted(days, reverse=True))
        sysst = self.c.get("/api/v1/system/storage")
        kinds = {k["kind"]: k["bytes"] for k in sysst["kinds"]}
        self.assertEqual(kinds["logs"], sum(x["logs"] for x in per_car.values()))
        self.assertEqual(sysst["total_bytes"], sum(kinds.values()))
        self.assertGreater(kinds["imported"], 0)
        self.assertEqual(len(sysst["growth"]), 30)
        self.assertGreater(sysst["full_in_days"], 100)
        # c3's imported days carry no CAN logs, and its quiet days store nothing
        c3 = self.walk("/api/v1/cars/c3/storage", limit=500)
        self.assertFalse([x for x in c3 if x["source"] == "teslamate" and x["series"] == "logs"])
        self.assertTrue([x for x in c3 if x["source"] == "teslamate"])
        self.assertEqual(max(x["date"] for x in c3), "2026-11-14")
        only = self.c.get("/api/v1/cars/c1/storage?kinds=logs&limit=5")["items"]
        self.assertEqual({x["series"] for x in only}, {"logs"})

    def test_system_health(self):
        h = self.c.get("/api/v1/system/health")
        self.assertEqual(h["db"]["cars"], 3)
        self.assertEqual(h["versions"]["api"], "1")
        self.assertGreater(h["uptime_ms"], 0)


class BigFleetSystem(ServerCase):
    server_args = {"cars": 500}

    def test_counts_scale(self):
        self.assertEqual(self.c.get("/api/v1/system/health")["db"]["cars"], 500)
        small = self.c.get("/api/v1/system/storage")
        self.assertGreater({k["kind"]: k["bytes"] for k in small["kinds"]}["logs"], 497 * 10 ** 9)
        s = self.c.get("/api/v1/cars/c42/storage")
        self.assertEqual(s["items"], [])
        self.assertGreater(s["total_bytes"], 0)
        h = self.c.get("/api/v1/cars/c42/health")
        self.assertIn("healthy", h)


class WatchSystem(ServerCase):
    server_args = {"role": "watch"}

    def test_watch(self):
        self.c.error("PATCH", "/api/v1/settings", 403, "not_allowed", body={"version": 1, "range": "ideal"})
        self.c.error("GET", "/api/v1/system/storage", 403, "not_allowed")
        self.c.error("GET", "/api/v1/system/health", 403, "not_allowed")
        self.c.error("POST", "/api/v1/cars/c1/rebuild", 403, "not_allowed", body={})
        self.c.error("GET", "/api/v1/cars/c3/health", 404, "no_such_car")
        self.assertIn("healthy", self.c.get("/api/v1/cars/c1/health"))
        self.assertIn("tiles", self.c.get("/api/v1/maps/config"))
        self.assertTrue(self.c.get("/api/v1/notices")["items"])
