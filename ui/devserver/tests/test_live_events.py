"""The live figures on /api/v1/events: `job` (full rebuild, compaction, export, missing_scan, import, invoices),
`health`, `connections` and `storage`; sent when they change, at most once per topic's gap, admin-only ones to
admins' streams only."""

from __future__ import annotations

import threading
import time

from .helpers import ServerCase
from .test_live import read_events

PW = "sentry-mate-dev"
MIN = 60_000


def of(events, name, kind=None, jid=None):
    out = [e for e in events if e["event"] == name]
    if kind is not None:
        out = [e for e in out if e["data"]["kind"] == kind and (jid is None or e["data"]["id"] == jid)]
    return out


class Ticking:
    """Moves the (frozen) virtual clock on by `step_ms` every `every_s` while the block runs."""

    def __init__(self, app, step_ms, every_s=0.1):
        self.app, self.step, self.every = app, step_ms, every_s
        self.stop = threading.Event()

    def __enter__(self):
        def run():
            while not self.stop.wait(self.every):
                self.app.clock.advance(self.step)
        self.t = threading.Thread(target=run, daemon=True)
        self.t.start()
        return self

    def __exit__(self, *a):
        self.stop.set()
        self.t.join()


class RunningRebuild(ServerCase):
    server_args = {"full_rebuild": "running"}

    def test_the_get_body_has_percent_and_rates(self):
        s = self.c.get("/api/v1/system/full-rebuild")
        self.assertEqual((s["state"], s["phase"]), ("running", "decoding"))
        self.assertGreater(s["days"]["total"], 0)
        self.assertEqual(s["days"]["done"], 0)
        self.assertGreater(s["bytes"]["done"], 0)
        self.assertEqual(s["percent"], 100 * s["logs"]["done"] // s["logs"]["total"])
        self.assertGreater(s["rate_logs_per_s"], 0)
        self.assertGreater(s["rate_bytes_per_s"], 0)
        self.assertIsNone(s["rate_days_per_s"])
        left = s["logs"]["total"] - s["logs"]["done"]
        self.assertAlmostEqual(s["eta_ms"] / 1000, left / s["rate_logs_per_s"], delta=1)

    def test_job_events_rise_and_are_throttled(self):
        self.c.get("/api/v1/me")  # signed in (the cookie)
        with Ticking(self.server.app, 20_000):
            t0 = time.monotonic()
            events = read_events(self.server.port, self.c.cookie, lambda ev: False, seconds=4)
            took = time.monotonic() - t0
        jobs = [e["data"]["status"] for e in of(events, "job", "full_rebuild")]
        self.assertGreaterEqual(len(jobs), 3, [e["event"] for e in events])
        # at most one every 0.5 s (and the first at once)
        self.assertLessEqual(len(jobs), took / 0.5 + 2, len(jobs))
        for a, b in zip(jobs, jobs[1:]):
            self.assertGreater(b["logs"]["done"], a["logs"]["done"])
            self.assertGreater(b["bytes"]["done"], a["bytes"]["done"])
        for j in jobs:
            self.assertEqual(j["phase"], "decoding")
            self.assertGreater(j["days"]["total"], 0)
            self.assertEqual(j["percent"], 100 * j["logs"]["done"] // j["logs"]["total"])
            self.assertGreater(j["rate_logs_per_s"], 0)
        for e in of(events, "job"):
            self.assertEqual(set(e["data"]), {"kind", "id", "status"})
        kinds = {e["data"]["kind"] for e in of(events, "job")}
        self.assertTrue({"full_rebuild", "compaction", "missing_scan", "import", "invoices"} <= kinds, kinds)
        ids = {e["data"]["id"] for e in of(events, "job", "invoices")}
        self.assertEqual(ids, {"tesla", "teslafi"})


class PageRebuild(ServerCase):
    def test_a_rebuild_started_here_goes_through_its_phases_on_the_stream(self):
        self.c.json("POST", "/api/v1/cars/c1/full-rebuild", {"confirm_password": PW}, expect=202)
        seen = []
        self.server.app.clock.advance(140_000)  # most of the decoding done (a car takes 3 min here)
        with Ticking(self.server.app, 2_000):
            events = read_events(self.server.port, self.c.cookie,
                                 lambda ev: any(e["data"]["status"]["state"] == "done"
                                                for e in of(ev, "job", "full_rebuild")), seconds=10)
        for e in of(events, "job", "full_rebuild"):
            s = e["data"]["status"]
            seen.append(s["phase"] or s["state"])
            if s["phase"] == "days":
                self.assertEqual(s["logs"]["done"], s["logs"]["total"])
                self.assertGreater(s["rate_days_per_s"], 0)
                self.assertEqual(s["percent"], 100 * s["days"]["done"] // s["days"]["total"])
            if s["state"] == "done":
                self.assertEqual(s["percent"], 100)
        self.assertIn("decoding", seen)
        self.assertIn("days", seen)
        self.assertEqual(seen[-1], "done")


class HealthConnectionsStorage(ServerCase):
    def setUp(self):
        self.c.get("/api/v1/me")  # signed in (the cookie)

    def test_first_values_then_changes_as_the_clock_moves(self):
        with Ticking(self.server.app, 15_000, every_s=0.2):
            events = read_events(self.server.port, self.c.cookie,
                                 lambda ev: len([e for e in of(ev, "health") if e["data"]["car"] == "c1"]) >= 2
                                 and len(of(ev, "connections")) >= 2, seconds=8)
        c1 = [e["data"]["health"] for e in of(events, "health") if e["data"]["car"] == "c1"]
        self.assertGreaterEqual(len(c1), 2)
        self.assertEqual(c1[0]["car"], "c1")
        keys = [next(k["last_upload_ms"] for k in h["keys"] if k["preferred"]) for h in c1]
        self.assertGreater(keys[-1], keys[0], "the CAN Bridge keeps uploading")
        conns = [e["data"] for e in of(events, "connections")]
        self.assertGreaterEqual(len(conns), 2)
        self.assertIn("connections", conns[0])
        a = {c["id"]: c["last_ok_ms"] for c in conns[0]["connections"]}
        b = {c["id"]: c["last_ok_ms"] for c in conns[-1]["connections"]}
        self.assertGreater(b["smo:c1"], a["smo:c1"])
        st = of(events, "storage")
        self.assertTrue(st)
        self.assertIn("compaction", st[0]["data"])
        self.assertIn("free_bytes", st[0]["data"])

    def test_a_frozen_clock_sends_each_topic_once(self):
        events = read_events(self.server.port, self.c.cookie, lambda ev: False, seconds=2.5)
        topics = [(e["event"], e["data"]["car"] if e["event"] == "health" else
                   (e["data"]["kind"], e["data"]["id"]) if e["event"] == "job" else None)
                  for e in events if e["event"] in ("job", "health", "connections", "storage")]
        self.assertEqual(len(topics), len(set(topics)), topics)

    def test_a_compaction_moves_on_and_is_sent(self):
        self.c.json("POST", "/api/v1/system/storage/compact", {}, expect=202)
        def ended(ev):
            c = of(ev, "job", "compaction")
            return len(c) > 1 and c[-1]["data"]["status"]["status"] == "idle"
        events = read_events(self.server.port, self.c.cookie, ended, seconds=16)
        comp = [e["data"]["status"] for e in of(events, "job", "compaction")]
        self.assertEqual(comp[0]["status"], "running")
        self.assertEqual(comp[-1]["status"], "idle")
        self.assertGreater(len(comp), 2)
        self.assertTrue(any(e["data"]["compaction"]["status"] == "running" for e in of(events, "storage")))


class WatchStream(ServerCase):
    server_args = {"full_rebuild": "running"}

    def test_admin_only_events_are_not_sent_to_a_watch_stream(self):
        w = self.server.client()
        w.json("POST", "/api/v1/session", {"name": "watch", "password": PW})
        self.assertEqual(w.get("/api/v1/me")["role"], "watch")
        with Ticking(self.server.app, 20_000):
            events = read_events(self.server.port, w.cookie, lambda ev: False, seconds=3)
        names = {e["event"] for e in events}
        self.assertIn("health", names)
        self.assertEqual({e["data"]["car"] for e in of(events, "health")}, {"c1"})
        self.assertFalse(names & {"job", "connections", "storage"}, names)
