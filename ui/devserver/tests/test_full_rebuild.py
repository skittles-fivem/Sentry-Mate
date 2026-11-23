"""The full rebuild (docs/ui-api.md: /cars/{car}/full-rebuild, /system/full-rebuild): the password asked again, one
at a time, its progress on the virtual clock to the end, the audit entries, the notices, cancel, and the servers
that start with one running (--full-rebuild running), one that failed (failed) and one offered (offered)."""

from __future__ import annotations

from .helpers import ServerCase

PW = "sentry-mate-dev"
MIN = 60_000
KEYS = ("state", "id", "scope", "cars", "car", "cars_done", "cars_total", "reason", "phase", "logs", "days", "bytes",
        "rate_bytes_per_s", "eta_ms", "started_ms", "finished_ms", "last_progress_ms", "stalled", "errors", "error",
        "by", "version")


class FullRebuild(ServerCase):
    def test_a_car_runs_to_the_end_once_at_a_time_with_the_password(self):
        idle = self.c.get("/api/v1/cars/c1/full-rebuild")
        for k in KEYS + ("this_car", "car_stale"):
            self.assertIn(k, idle)
        self.assertEqual(idle["state"], "idle")
        self.c.error("POST", "/api/v1/cars/c1/full-rebuild", 403, "password_needed", body={})
        self.c.error("POST", "/api/v1/cars/c1/full-rebuild", 403, "wrong_password", body={"confirm_password": "no"})
        s = self.c.json("POST", "/api/v1/cars/c1/full-rebuild", {"confirm_password": PW}, expect=202)
        self.assertEqual((s["state"], s["phase"], s["this_car"]), ("running", "copying", True))
        self.c.error("POST", "/api/v1/system/full-rebuild", 409, "full_rebuild_running", body={"confirm_password": PW})
        self.c.error("POST", "/api/v1/cars/c1/rebuild", 409, "full_rebuild_running", body={})
        notices = [n["code"] for n in self.c.get("/api/v1/notices")["items"]]
        self.assertIn("full_rebuild_running", notices)
        self.server.app.clock.advance(MIN)
        mid = self.c.get("/api/v1/system/full-rebuild")
        self.assertEqual(mid["phase"], "decoding")
        self.assertTrue(0 < mid["logs"]["done"] < mid["logs"]["total"], mid["logs"])
        self.assertGreater(mid["rate_bytes_per_s"], 0)
        self.assertGreater(mid["eta_ms"], 0)
        self.server.app.clock.advance(5 * MIN)
        done = self.c.get("/api/v1/system/full-rebuild")
        self.assertEqual(done["state"], "done")
        self.assertEqual(done["logs"]["done"], done["logs"]["total"])
        self.assertIsNone(done["phase"])
        actions = [e for e in self.c.get("/api/v1/audit?limit=100")["items"]
                   if e["action"] == "car-full-rebuild"]
        self.assertEqual(len(actions), 2, actions)
        self.c.error("DELETE", "/api/v1/system/full-rebuild", 409, "full_rebuild_not_running", body={})

    def test_settings_turn_the_automatic_run_off(self):
        s = self.c.get("/api/v1/settings")
        self.assertTrue(s["full_rebuild"]["auto"])
        t = self.c.json("PATCH", "/api/v1/settings", {"version": s["version"], "full_rebuild": {"auto": False}})
        self.assertFalse(t["full_rebuild"]["auto"])
        self.assertFalse(self.c.get("/api/v1/system/full-rebuild")["version"]["auto"])
        self.c.json("PATCH", "/api/v1/settings", {"version": t["version"], "full_rebuild": {"auto": True}})


class FullRebuildRunning(ServerCase):
    server_args = {"full_rebuild": "running"}

    def test_one_is_under_way_and_can_be_cancelled(self):
        s = self.c.get("/api/v1/system/full-rebuild")
        self.assertEqual((s["state"], s["scope"], s["phase"]), ("running", "all", "decoding"))
        self.assertTrue(s["errors"], "a log it could not decode")
        self.assertFalse(s["stalled"])
        c = self.c.json("DELETE", "/api/v1/system/full-rebuild", {})
        self.assertEqual(c["state"], "cancelled")


class FullRebuildFailed(ServerCase):
    server_args = {"full_rebuild": "failed"}

    def test_it_failed_on_the_second_car_and_says_why(self):
        s = self.c.get("/api/v1/system/full-rebuild")
        self.assertEqual((s["state"], s["car"]), ("failed", "c2"))
        self.assertIn("no space left", s["error"])
        self.assertEqual(s["errors"][-1]["message"], s["error"])
        notices = [n["code"] for n in self.c.get("/api/v1/notices")["items"]]
        self.assertIn("full_rebuild_failed", notices)


class FullRebuildOffered(ServerCase):
    server_args = {"full_rebuild": "offered"}

    def test_a_car_made_with_an_older_decoding_is_offered(self):
        s = self.c.get("/api/v1/cars/c1/full-rebuild")
        self.assertEqual((s["state"], s["car_stale"], s["version"]["stale_cars"]), ("idle", True, ["c1"]))
        notices = [n["code"] for n in self.c.get("/api/v1/notices")["items"]]
        self.assertIn("full_rebuild_offered", notices)
