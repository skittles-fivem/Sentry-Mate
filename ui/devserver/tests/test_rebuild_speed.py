"""The rebuild speed (docs/ui-api.md): the full rebuild's `speed` while it decodes, and a car's `rebuild_speed` in
its health while a rebuild is pending; null otherwise. Data health shows it as its Rebuild speed line."""

from __future__ import annotations

from .helpers import DAY, NOW, ServerCase

PW = "sentry-mate-dev"
MIN = 60_000
SPEED_KEYS = {"logs_per_s", "stage", "stage_share_pct", "over_ms"}
STAGES = {"read", "samples", "now", "windows", "other"}


class RebuildSpeed(ServerCase):
    def test_the_full_rebuild_says_its_speed_while_it_decodes(self):
        self.assertIsNone(self.c.get("/api/v1/cars/c1/full-rebuild")["speed"])
        self.c.json("POST", "/api/v1/cars/c1/full-rebuild", {"confirm_password": PW}, expect=202)
        self.assertIsNone(self.c.get("/api/v1/system/full-rebuild")["speed"], "copying")
        self.server.app.clock.advance(MIN)
        mid = self.c.get("/api/v1/system/full-rebuild")
        self.assertEqual(mid["phase"], "decoding")
        sp = mid["speed"]
        self.assertEqual(set(sp), SPEED_KEYS)
        self.assertEqual(sp["logs_per_s"], mid["rate_logs_per_s"])
        self.assertIn(sp["stage"], STAGES)
        self.assertTrue(0 <= sp["stage_share_pct"] <= 100)
        self.server.app.clock.advance(30 * MIN)
        done = self.c.get("/api/v1/system/full-rebuild")
        self.assertEqual(done["state"], "done")
        self.assertIsNone(done["speed"])

    def test_a_car_health_says_the_speed_of_its_rebuild_while_one_is_pending(self):
        self.assertIsNone(self.c.get("/api/v1/cars/c1/health")["rebuild_speed"])
        self.c.json("POST", "/api/v1/cars/c1/rebuild", {"from_ms": NOW - 3 * DAY})
        h = self.c.get("/api/v1/cars/c1/health")
        self.assertTrue(h["rebuild_pending"])
        self.assertEqual(set(h["rebuild_speed"]), SPEED_KEYS)
        self.assertIn(h["rebuild_speed"]["stage"], STAGES)
        self.server.app.clock.advance(6000)
        h = self.c.get("/api/v1/cars/c1/health")
        self.assertFalse(h["rebuild_pending"])
        self.assertIsNone(h["rebuild_speed"])
