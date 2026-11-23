"""Server, settings and health: server defaults (checks, version, what follows from them), maps and
address lookup, notices, about, per-car data health, rebuilds, CAN and TeslaMate overlaps, and
storage that adds up from the cars to the server."""

from __future__ import annotations

import os
import threading

from .helpers import DAY, NOW, ServerCase
from .test_live import read_events

PACKS = os.path.normpath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", "packs"))


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


class DefaultTimeZone(ServerCase):
    def test_default_time_zone(self):
        s = self.c.get("/api/v1/settings")
        self.assertEqual(s["default_time_zone"], "America/Chicago")
        e, _ = self.c.error("PATCH", "/api/v1/settings", 422, "invalid",
                            body={"version": s["version"], "default_time_zone": "Mars/Olympus_Mons"})
        self.assertIn("default_time_zone", e["fields"])
        e, _ = self.c.error("PATCH", "/api/v1/settings", 400, "bad_input",
                            body={"version": s["version"], "default_time_zone": 5})
        self.assertIn("default_time_zone", e["fields"])
        t = self.c.json("PATCH", "/api/v1/settings", {"version": s["version"], "default_time_zone": " Europe/Berlin "})
        self.assertEqual(t["default_time_zone"], "Europe/Berlin")


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
        self.assertIn("settings-changed", [a["action"] for a in self.c.get("/api/v1/audit?limit=3")["items"]])

    def test_3_maps_geocode_notices_about(self):
        m = self.c.get("/api/v1/maps/config")
        self.assertEqual(m["tiles"]["mode"], "online")
        self.assertIn("{z}", m["tiles"]["online_template"])
        # the ready-made sources (the street map, the default, and satellite), each with its credit, zoom and terms
        self.assertEqual(m["tiles"]["preset"], "osm")
        self.assertTrue({"osm", "satellite"} <= {p["id"] for p in m["tiles"]["presets"]})
        for p in m["tiles"]["presets"]:
            self.assertEqual(set(p), {"id", "name", "short", "kind", "url", "attribution", "max_zoom", "terms",
                                      "overlays", "region"})
        # online tiles: the pages send their origin as Referer (OpenStreetMap blocks tiles without one), the API none
        st, hd, _ = self.c.request("GET", "/")
        self.assertEqual(hd["referrer-policy"], "strict-origin")
        # the OpenStreetMap default and the satellite style viewers may switch to
        self.assertIn("img-src 'self' data: blob: https://tile.openstreetmap.org https://server.arcgisonline.com;",
                      hd["content-security-policy"])
        st, hd, _ = self.c.request("GET", "/api/v1/me")
        self.assertEqual(hd["referrer-policy"], "no-referrer")
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
        self.assertTrue(r["detail"]["notes_url"].startswith("https://github.com/"))
        m = self.c.get("/api/v1/maps/config")
        self.assertEqual((m["tiles"]["mode"], m["tiles"]["online_template"]), ("offline", None))
        st, hd, _ = self.c.request("GET", "/")
        self.assertEqual(hd["referrer-policy"], "no-referrer")
        # as the real server: a page may load tiles only from the source that is on (none while offline)
        self.assertNotIn("tile.openstreetmap.org", hd["content-security-policy"])
        v = self.c.get("/api/v1/settings")["version"]
        self.c.json("PATCH", "/api/v1/settings", {"version": v, "tiles": {"mode": "online", "online_url": "https://tile.opentopomap.org/{z}/{x}/{y}.png", "styles": []}})
        st, hd, _ = self.c.request("GET", "/")
        self.assertIn("img-src 'self' data: blob: https://tile.opentopomap.org;", hd["content-security-policy"])
        self.assertNotIn("tile.openstreetmap.org", hd["content-security-policy"])
        self.assertEqual(hd["referrer-policy"], "strict-origin")
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

    def test_log_checks(self):
        """Every log as the CAN Bridge app is told it, counted per check_reason, with the runs of failed logs."""
        for cid in ("c1", "c2", "c3"):
            lc = self.c.get(f"/api/v1/cars/{cid}/health")["log_checks"]
            self.assertEqual(set(lc), {"verified", "pending", "failed", "mismatch", "bad_signature", "unsigned",
                                       "chain_break", "failed_runs", "runs_total"})
            self.assertEqual(lc["failed"], lc["mismatch"] + lc["bad_signature"] + lc["unsigned"] + lc["chain_break"])
            self.assertEqual(lc["failed"], sum(r["logs"] for r in lc["failed_runs"]))
            self.assertEqual(lc["runs_total"], len(lc["failed_runs"]))
            for r in lc["failed_runs"]:
                self.assertLessEqual(r["first_start_ms"], r["last_start_ms"])
        self.assertGreater(self.c.get("/api/v1/cars/c3/health")["log_checks"]["failed"], 0)

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
        self.assertEqual(self.c.get("/api/v1/audit?limit=1")["items"][0]["action"], "car-rebuild")

    def test_overlaps(self):
        o = self.c.get("/api/v1/cars/c3/overlaps")["items"]
        self.assertEqual([x["kind"] for x in o], ["live", "imports"], "newest first")
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

    def test_two_imports_of_the_same_days(self):
        """A week TeslaMate and TeslaFi both imported (no live data): one import's rows only, TeslaMate's at first
        (the same drives, TeslaMate's positions more often), TeslaFi's once flipped (admin, version checked)."""
        path = "/api/v1/cars/c3/overlaps"
        span = [x for x in self.c.get(path)["items"] if x["kind"] == "imports"][0]
        self.assertEqual((span["shown_source"], span["default_source"], span["default_reason"]),
                         ("import:teslamate", "import:teslamate", "more_samples"))
        self.assertEqual(span["sites"]["teslamate"], span["sites"]["teslafi"])
        self.assertGreater(span["sites"]["teslafi"]["drives"], 0)
        q = f"/api/v1/cars/c3/drives?from_ms={span['start_ms']}&to_ms={span['end_ms']}&limit=500"
        n = span["sites"]["teslamate"]["drives"]

        def sources(extra=""):
            rows = self.c.get(q + extra)["items"]
            return len(rows), {x["source"] for x in rows}

        self.assertEqual(sources(), (n, {"teslamate"}), "one drive per real drive")
        self.assertEqual(span["imported"], span["sites"]["teslamate"], "the shown site's rows, each drive once")

        def visits_point_at_shown_rows():
            # the stops link the drives and charges the lists show, never the hidden site's copies
            r = f"from_ms={span['start_ms']}&to_ms={span['end_ms']}&limit=500"
            drives = {x["id"] for x in self.c.get(f"/api/v1/cars/c3/drives?{r}")["items"]}
            charges = {x["id"] for x in self.c.get(f"/api/v1/cars/c3/charges?{r}")["items"]}
            vs = self.c.get(f"/api/v1/visits?cars=c3&{r}")["items"]
            self.assertTrue(vs)
            links = [v[k] for v in vs for k in ("drive_in", "drive_out")
                     if v[k] is not None and span["start_ms"] <= v["arrived_ms"] and (v["left_ms"] or 0) < span["end_ms"]]
            self.assertTrue(links)
            self.assertEqual([x for x in links if x not in drives], [])
            got = [x if isinstance(x, str) else x["id"] for v in vs for x in v["charges"]]
            self.assertTrue(got)
            self.assertEqual([x for x in got if x not in charges], [])

        visits_point_at_shown_rows()
        self.assertEqual(sources("&source=import"), (n, {"teslamate"}))
        self.assertEqual(sources("&source=import:teslafi"), (n, {"teslafi"}), "TeslaFi alone")
        t0 = self.c.get("/api/v1/cars/c3/totals")
        self.c.error("PATCH", path, 400, "bad_input", body={"id": span["id"], "shown": "import:teslafi"})
        self.c.error("PATCH", path, 422, "invalid", body={"id": span["id"], "shown": "live", "version": span["version"]})
        self.c.error("PATCH", path, 409, "stale_version",
                     body={"id": span["id"], "shown": "import:teslafi", "version": span["version"] + 1})
        d = self.c.json("PATCH", path, {"id": span["id"], "shown": "import:teslafi", "version": span["version"]})
        self.assertEqual(d["changed"]["shown_source"], "import:teslafi")
        self.assertEqual(d["changed"]["version"], span["version"] + 1)
        self.assertEqual(self.c.get("/api/v1/audit?limit=1")["items"][0]["action"], "car-overlap")
        self.assertEqual(sources(), (n, {"teslafi"}))
        self.assertEqual(d["changed"]["imported"], span["sites"]["teslafi"])
        visits_point_at_shown_rows()
        self.assertEqual(sources("&source=import:teslamate"), (n, {"teslamate"}), "TeslaMate alone")
        t1 = self.c.get("/api/v1/cars/c3/totals")
        self.assertEqual(t1["drives"], t0["drives"], "never doubled")
        self.c.json("PATCH", path, {"id": span["id"], "shown": "import:teslamate", "version": span["version"] + 1})
        self.assertEqual(sources(), (n, {"teslamate"}))


class MissingLogs(ServerCase):
    """The "Missing on the server" figures (ingest::missing): car c2 with 3 logs asked of the car again."""

    KEYS = {"count", "oldest_ms", "found_ms", "asked", "asked_ms", "gone", "gone_oldest_ms", "scanned_ms"}

    def test_a_car_with_three_wanted_logs(self):
        h = self.c.get("/api/v1/cars/c2/health")
        m = h["missing_logs"]
        self.assertEqual(set(m), self.KEYS)
        self.assertEqual(m["count"], 3)
        self.assertTrue(m["asked"])
        self.assertEqual(m["gone"], 1)
        self.assertLess(m["oldest_ms"], m["found_ms"])
        self.assertLessEqual(m["found_ms"], m["asked_ms"])
        self.assertLessEqual(m["scanned_ms"], NOW)
        self.assertIn("logs_missing", h["reasons"])
        self.assertFalse(h["healthy"])

    def test_nothing_missing_elsewhere(self):
        m = self.c.get("/api/v1/cars/c1/health")["missing_logs"]
        self.assertEqual((m["count"], m["gone"], m["asked"], m["oldest_ms"]), (0, 0, False, None))
        self.assertIsNotNone(m["scanned_ms"])

    def test_the_connections_overview_counts_them(self):
        rows = self.c.get("/api/v1/integrations/overview")["connections"]
        by_id = {r["id"]: r for r in rows}
        self.assertEqual(by_id["smo:c2"]["missing_logs"], 3)
        self.assertEqual(by_id["smo:c1"]["missing_logs"], 0)
        self.assertTrue(all(r["missing_logs"] is None for r in rows if r["kind"] != "smo"))

    def test_the_scan_is_in_the_audit_log(self):
        rows = [r for r in self.walk("/api/v1/audit?group=cars", limit=100) if r["action"] == "can-logs-missing"]
        self.assertEqual(len(rows), 1)
        self.assertEqual(rows[0]["group"], "cars")
        self.assertEqual(rows[0]["car"], "c2")


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
        self.assertEqual(h["db"]["cars"], 4)
        self.assertEqual(h["versions"]["api"], "1")
        self.assertGreater(h["uptime_ms"], 0)


class StorageDetail(ServerCase):
    GROUPS = {"logs", "series", "rows", "kv", "imports", "server"}

    def detail(self):
        return self.c.get("/api/v1/system/storage/detail")

    def test_1_detail_idle(self):
        d = self.detail()
        cats = d["categories"]
        self.assertEqual([c["disk_bytes"] for c in cats], sorted((c["disk_bytes"] for c in cats), reverse=True))
        self.assertEqual(len({c["key"] for c in cats}), len(cats))
        for c in cats:
            self.assertIn(c["group"], self.GROUPS)
            self.assertTrue(c["label"])
            self.assertGreater(c["files"], 0)
            self.assertGreaterEqual(c["disk_bytes"], c["bytes"])
            self.assertEqual(c["disk_bytes"] % 4096, 0)
        for key in ("logs.raw", "logs.sidecars", "logs.packs", "logs.ledger", "series.can.snapshots",
                    "series.teslamate.snapshots", "series.teslamate-local.snapshots", "curves.can.snapshots",
                    "rows.snapshots", "kv.engine", "kv.day-parts", "imports.uploads", "server.accounts",
                    "server.audit", "server.db", "server.web", "server.tiles"):
            self.assertIn(key, {c["key"] for c in cats})
        self.assertEqual(d["total_bytes"], sum(c["bytes"] for c in cats))
        self.assertEqual(d["total_disk_bytes"], sum(c["disk_bytes"] for c in cats))
        self.assertEqual(d["free_bytes"], self.c.get("/api/v1/system/storage")["free_bytes"])
        comp = d["compaction"]
        self.assertEqual(set(comp), {"status", "automatic", "started_ms", "finished_ms", "step", "done", "total",
                                     "last_saved_bytes", "saved_bytes", "error", "phase", "step_number", "steps",
                                     "unit", "pass_saved_bytes", "rate_per_s", "eta_ms", "progress_ms", "stalled",
                                     "unit_errors", "last_unit_error"})
        self.assertEqual((comp["phase"], comp["step_number"], comp["steps"], comp["pass_saved_bytes"],
                          comp["eta_ms"], comp["stalled"], comp["unit_errors"]), (None, None, 3, None, None, False, 0))
        # idle on the first load, and reading it again changes nothing (the pixel checks rely on it)
        self.assertEqual(comp["status"], "idle")
        self.assertIs(comp["automatic"], True)
        self.assertIsNone(comp["step"])
        self.assertIsNone(comp["error"])
        self.assertLess(comp["started_ms"], comp["finished_ms"])
        self.assertLess(comp["finished_ms"], NOW)
        self.assertGreater(comp["saved_bytes"], comp["last_saved_bytes"])
        self.assertEqual(self.detail(), d)

    def test_2_compact(self):
        before = self.detail()
        size = {c["key"]: c for c in before["categories"]}
        self.c.error("POST", "/api/v1/system/storage/compact", 403, "csrf", csrf=False)
        r = self.c.json("POST", "/api/v1/system/storage/compact", expect=202)
        self.assertEqual(set(r), {"compaction"})
        run = r["compaction"]
        self.assertEqual((run["status"], run["step"], run["done"], run["total"]), ("running", "logs", 0, 0))
        self.assertEqual((run["phase"], run["step_number"], run["unit"], run["pass_saved_bytes"], run["progress_ms"],
                          run["rate_per_s"], run["eta_ms"], run["stalled"]), ("counting", 1, "days", 0, NOW, None, None, False))
        self.assertEqual(run["started_ms"], NOW)
        # a second ask while it runs starts no second pass
        again = self.c.json("POST", "/api/v1/system/storage/compact", expect=202)["compaction"]
        self.assertEqual(again, run)
        log = [x for x in self.c.get("/api/v1/audit?limit=5")["items"] if x["action"] == "storage-compacted"]
        self.assertEqual(len(log), 1, "the pass that started is in the audit log, once")
        self.assertEqual((log[0]["account"], log[0]["target"], log[0]["group"]), ("admin", "server:storage", "settings"))
        seen, d, saved_so_far = [], None, 0
        for _ in range(12):
            d = self.detail()
            comp = d["compaction"]
            if comp["status"] != "running":
                break
            self.assertIn(comp["step"], ("logs", "series", "records"))
            self.assertLessEqual(comp["done"], comp["total"])
            self.assertGreater(comp["total"], 0)
            self.assertEqual(d["categories"], before["categories"], "nothing moves until the pass is done")
            self.assertEqual(comp["phase"], "packing")
            self.assertEqual(comp["step_number"], ("logs", "series", "records").index(comp["step"]) + 1)
            self.assertEqual(comp["unit"], "records" if comp["step"] == "records" else "days")
            self.assertGreater(comp["rate_per_s"], 0)
            self.assertGreaterEqual(comp["eta_ms"], 0)
            self.assertLessEqual(comp["progress_ms"], NOW)
            self.assertFalse(comp["stalled"])
            self.assertGreaterEqual(comp["pass_saved_bytes"], saved_so_far)
            saved_so_far = comp["pass_saved_bytes"]
            seen.append(comp["step"])
        self.assertEqual(seen, ["logs", "logs", "series", "records"])
        comp = d["compaction"]
        self.assertEqual(comp["status"], "idle")
        self.assertEqual(comp["finished_ms"], NOW)
        self.assertEqual((comp["phase"], comp["pass_saved_bytes"], comp["eta_ms"]), (None, None, None))
        self.assertGreater(saved_so_far, 0)
        after = {c["key"]: c for c in d["categories"]}
        # logs and sidecars as uploaded went into the day packs, and a few categories shrank
        for key in ("logs.raw", "logs.sidecars", "series.can.snapshots", "kv.engine", "kv.day-parts"):
            self.assertLess(after[key]["disk_bytes"], size[key]["disk_bytes"], key)
        self.assertGreater(after["logs.packs"]["bytes"], size["logs.packs"]["bytes"])
        self.assertGreater(after["logs.packs"]["files"], size["logs.packs"]["files"])
        for key in ("server.tiles", "imports.uploads", "server.audit", "logs.ledger"):
            self.assertEqual(after[key], size[key], key)
        freed = before["total_disk_bytes"] - d["total_disk_bytes"]
        self.assertGreater(freed, 0)
        self.assertEqual(comp["last_saved_bytes"], freed)
        self.assertEqual(comp["saved_bytes"], before["compaction"]["saved_bytes"] + freed)
        self.assertEqual([c["disk_bytes"] for c in d["categories"]],
                         sorted((c["disk_bytes"] for c in d["categories"]), reverse=True))
        self.assertEqual(self.detail(), d)

    def test_3_nothing_left(self):
        before = self.detail()
        self.c.json("POST", "/api/v1/system/storage/compact", expect=202)
        d = before
        for _ in range(12):
            d = self.detail()
            if d["compaction"]["status"] != "running":
                break
        self.assertEqual(d["compaction"]["status"], "idle")
        self.assertEqual(d["compaction"]["last_saved_bytes"], 0)
        self.assertEqual(d["compaction"]["saved_bytes"], before["compaction"]["saved_bytes"])
        self.assertEqual(d["categories"], before["categories"])

    def test_4_failed(self):
        # a pass that failed says why, and the next ask starts a new one without the error
        self.detail()
        with self.server.app.store.lock:
            self.server.app.store.compaction.update(status="failed", step=None, error="The disk is full.")
        comp = self.detail()["compaction"]
        self.assertEqual((comp["status"], comp["error"]), ("failed", "The disk is full."))
        run = self.c.json("POST", "/api/v1/system/storage/compact", expect=202)["compaction"]
        self.assertEqual((run["status"], run["error"]), ("running", None))
        # a pass with no progress for two minutes says it is stalled
        with self.server.app.store.lock:
            self.server.app.store.compaction["progress_ms"] = NOW - 10 * 60_000
        r = self.c.json("POST", "/api/v1/system/storage/compact", expect=202)["compaction"]
        self.assertIs(r["stalled"], True)


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
        self.c.error("GET", "/api/v1/system/storage/detail", 403, "not_allowed")
        self.c.error("POST", "/api/v1/system/storage/compact", 403, "not_allowed")
        self.c.error("GET", "/api/v1/system/health", 403, "not_allowed")
        self.c.error("POST", "/api/v1/cars/c1/rebuild", 403, "not_allowed", body={})
        self.c.error("GET", "/api/v1/cars/c3/health", 404, "no_such_car")
        self.assertIn("healthy", self.c.get("/api/v1/cars/c1/health"))
        self.assertIn("tiles", self.c.get("/api/v1/maps/config"))
        self.assertTrue(self.c.get("/api/v1/notices")["items"])


class UiPacks(ServerCase):
    """GET /ui-packs lists every built-in pack; a person's ui_pack picks the pack their browser is served."""

    def test_list_and_choice(self):
        r = self.c.get("/api/v1/ui-packs")
        self.assertEqual([p["id"] for p in r["items"]], ["instrument", "logbook", "horizon", "console", "pebble", "manager"])
        self.assertEqual(r["chosen"], "instrument")
        self.assertEqual(r["default"], "instrument")
        for p in r["items"]:
            self.assertTrue(p["name"] and p["version"] and p["description"])
            self.assertTrue(p["api_supported"] and p["builtin"])
            self.assertEqual(p["chosen"], p["id"] == "instrument")
        e, _ = self.c.error("PATCH", "/api/v1/me/preferences", 422, "invalid", body={"ui_pack": "nope"})
        self.assertIn("ui_pack", e["fields"])
        try:
            self.c.json("PATCH", "/api/v1/me/preferences", {"ui_pack": "pebble"})
            self.assertEqual(self.c.get("/api/v1/about")["ui_pack"]["name"], "Pebble")
            st, _hd, body = self.c.request("GET", "/settings/display")
            self.assertEqual(st, 200)
            with open(os.path.join(PACKS, "pebble", "web", "index.html"), "rb") as f:
                self.assertEqual(body, f.read())
            st, _hd, body = self.c.request("GET", "/pages/settings-display.js")
            with open(os.path.join(PACKS, "pebble", "web", "pages", "settings-display.js"), "rb") as f:
                self.assertEqual(body, f.read())
        finally:
            p = self.c.json("PATCH", "/api/v1/me/preferences", {"ui_pack": None})
        self.assertEqual(p["ui_pack"], "instrument")
        self.assertEqual(self.c.get("/api/v1/about")["ui_pack"]["name"], "Instrument")


class ReleaseCheck(ServerCase):
    """/about `build`, GET /release and POST /release/check: off by default, the switch saves, a check now, the
    admins' `release_available` notice only while a newer build is found."""

    def test_release_check(self):
        b = self.c.get("/api/v1/about")["build"]
        self.assertEqual((b["version"], b["short"], b["dev"]), ("0.1.0", "2f44a52", False))
        self.assertEqual(b["commit"][:7], b["short"])
        r = self.c.get("/api/v1/release")
        self.assertEqual((r["enabled"], r["status"], r["newest"], r["next_ms"]), (False, "off", None, None))
        self.assertEqual(r["current"]["short"], "2f44a52")
        self.c.error("POST", "/api/v1/release/check", 409, "release_check_off", body={})
        self.assertNotIn("release_available", {x["code"] for x in self.c.get("/api/v1/notices")["items"]})
        v = self.c.get("/api/v1/settings")["version"]
        s = self.c.json("PATCH", "/api/v1/settings", {"version": v, "release_check": {"enabled": True}})
        self.assertTrue(s["release_check"]["enabled"])
        r = self.c.get("/api/v1/release")
        self.assertEqual(r["status"], "newer")
        self.assertEqual(r["checked_ms"], NOW)
        self.assertEqual(r["next_ms"], NOW + DAY)
        nw = r["newest"]
        self.assertEqual((nw["changes"], len(nw["titles"]), nw["more"]), (4, 4, 0))
        self.assertTrue(nw["compare_url"].startswith("https://github.com/skittles-fivem/Sentry-Mate/compare/"))
        self.assertIn("docker", r["pull"])
        n = {x["code"]: x for x in self.c.get("/api/v1/notices")["items"]}["release_available"]
        self.assertEqual((n["detail"]["commit"], n["detail"]["changes"]), (nw["commit"], 4))
        self.assertEqual((b["number"], nw["build"]), (41, 42))
        self.assertTrue(n["message"].startswith("A newer build is ready: build 42, "), n["message"])
        self.assertNotIn(nw["short"], n["message"])
        self.assertEqual(n["detail"]["notes_url"], nw["compare_url"])
        self.server.app.store.release_result = "error"
        r = self.c.json("POST", "/api/v1/release/check", {})
        self.assertEqual((r["status"], r["error"]["code"]), ("error", "rate_limited"))
        self.assertNotIn("release_available", {x["code"] for x in self.c.get("/api/v1/notices")["items"]})
        self.server.app.store.release_result = "up_to_date"
        r = self.c.json("POST", "/api/v1/release/check", {})
        self.assertEqual((r["status"], r["newest"]["commit"], r["newest"]["changes"]), ("up_to_date", b["commit"], 0))
        self.server.app.store.release_result = "newer"
        v = self.c.get("/api/v1/settings")["version"]
        self.c.json("PATCH", "/api/v1/settings", {"version": v, "release_check": {"enabled": False}})
        r = self.c.get("/api/v1/release")
        self.assertEqual((r["status"], r["checked_ms"]), ("off", None))


class ServerUpdatesCard(ServerCase):
    """What Settings: Server and access's Server updates card reads (GET /about `build`, GET /release) in each state
    of --release-check it is shown in: check off, update available, up to date, check failed."""

    def state(self, result, on=True):
        st = self.server.app.store
        v = self.c.get("/api/v1/settings")["version"]
        self.c.json("PATCH", "/api/v1/settings", {"version": v, "release_check": {"enabled": on}})
        st.release_result = result
        st.release_checked_ms = NOW - 2 * 3600 * 1000 if on else None
        return self.c.get("/api/v1/release")

    def test_states(self):
        b = self.c.get("/api/v1/about")["build"]
        # this build: version, number and when it was built, the same in /about and in /release's `current`
        self.assertEqual((b["version"], b["number"], b["dev"]), ("0.1.0", 41, False))
        self.assertEqual(b["built_ms"], NOW - 2 * DAY)
        r = self.state("newer", on=False)
        self.assertEqual((r["enabled"], r["status"], r["checked_ms"], r["newest"]), (False, "off", None, None))
        self.assertEqual((r["current"]["number"], r["current"]["built_ms"]), (b["number"], b["built_ms"]))
        r = self.state("newer")
        self.assertEqual((r["status"], r["checked_ms"], r["newest"]["build"]), ("newer", NOW - 2 * 3600 * 1000, 42))
        self.assertGreater(r["newest"]["date_ms"], b["built_ms"])
        r = self.state("up_to_date")
        self.assertEqual((r["status"], r["newest"]["build"], r["newest"]["date_ms"]), ("up_to_date", 41, b["built_ms"]))
        r = self.state("error")
        self.assertEqual((r["status"], r["newest"]), ("error", None))
        self.assertTrue(r["error"]["message"])
        # Check now answers with the same object; while off it is refused with the code the card words
        self.assertEqual(self.c.json("POST", "/api/v1/release/check", {})["status"], "error")
        self.state("newer", on=False)
        self.c.error("POST", "/api/v1/release/check", 409, "release_check_off", body={})


class ReleaseCheckWatcher(ServerCase):
    """A watch account sees the result but gets no notice and can't check now."""

    server_args = {"role": "watch", "release_check": "newer"}

    def test_watcher(self):
        self.assertEqual(self.c.get("/api/v1/release")["status"], "newer")
        self.assertNotIn("release_available", {x["code"] for x in self.c.get("/api/v1/notices")["items"]})
        self.c.error("POST", "/api/v1/release/check", 403, "not_allowed", body={})
