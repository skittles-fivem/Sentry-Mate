"""GET /api/v1/integrations/overview: every connection on one page (docs/ui-api.md, "Integrations: Overview")."""

from __future__ import annotations

import json
import unittest

from .helpers import NOW, Running, ServerCase

HOUR = 3_600_000
DAY = 86_400_000
PATH = "/api/v1/integrations/overview"
KINDS = ("smo", "tesla", "teslafi", "teslamate_mqtt", "teslamate_db", "mqtt", "tiles", "geocoding", "release_check")
STATUSES = ("live", "idle", "paused", "off", "problem")
CONN_KEYS = {"id", "kind", "label", "car", "account", "status", "status_reason", "last_ok_ms", "next_ms", "pace",
             "pace_s", "unit", "today", "day", "week", "uptime", "last_error", "hourly", "missing_logs",
             "newest_log_ms", "link"}
PROBLEM_KEYS = {"id", "connection", "kind", "label", "start_ms", "end_ms", "duration_ms", "resolved", "code",
                "message"}


class Overview(ServerCase):
    def test_shape(self):
        d = self.c.get(PATH)
        self.assertEqual(set(d), {"read_ms", "connections", "problems", "totals"})
        self.assertEqual(d["read_ms"], NOW)
        ids = [c["id"] for c in d["connections"]]
        self.assertEqual(ids, ["smo:c1", "smo:c2", "smo:c3", "tesla:ta1", "teslafi:fa1", "teslamate_mqtt",
                               "teslamate_db", "mqtt", "tiles", "geocoding", "release_check"])
        for c in d["connections"]:
            self.assertEqual(set(c), CONN_KEYS, c["id"])
            self.assertIn(c["kind"], KINDS)
            self.assertIn(c["status"], STATUSES)
            self.assertIn(c["unit"], ("uploads", "requests", "messages"))
            for p in ("day", "week"):
                x = c[p]
                self.assertEqual(x["count"], x["ok"] + x["errors"], c["id"])
                if x["count"]:
                    self.assertAlmostEqual(x["success_pct"], 100 * x["ok"] / x["count"], places=0)
                else:
                    self.assertIsNone(x["success_pct"])
            self.assertEqual(c["day"]["count"], sum(h["count"] for h in c["hourly"]))
            self.assertEqual(c["day"]["errors"], sum(h["errors"] for h in c["hourly"]))
            self.assertLessEqual(c["today"]["count"], c["day"]["count"])
            self.assertLessEqual(c["day"]["count"], c["week"]["count"])
            if c["status"] == "off":
                self.assertIsNone(c["uptime"]["day_pct"])
            else:
                self.assertTrue(0 <= c["uptime"]["day_pct"] <= 100 and 0 <= c["uptime"]["week_pct"] <= 100)
            if c["kind"] == "smo":
                self.assertEqual(c["id"], "smo:" + c["car"])
                self.assertEqual(c["link"], f"/cars/{c['car']}/health")
            if c["kind"] in ("tesla", "teslafi"):
                self.assertEqual(c["id"], f"{c['kind']}:{c['account']}")
            # where each is changed, as the real server names it (crates/web/src/resources/overview.rs)
            where = {"tiles": "/settings/display#maps", "geocoding": "/settings/display#addresses",
                     "release_check": "/settings/display#release"}
            if c["kind"] in where:
                self.assertEqual(c["link"], where[c["kind"]])
        by = {c["id"]: c for c in d["connections"]}
        self.assertEqual(by["smo:c1"]["status"], "live")  # driving
        self.assertEqual(by["smo:c3"]["status"], "problem")  # quiet for 55 hours
        self.assertEqual(by["smo:c3"]["uptime"]["day_pct"], 0.0)
        self.assertEqual(by["teslafi:fa1"]["status"], "idle")
        self.assertEqual(by["teslamate_db"]["status"], "off")
        self.assertIsNone(by["tesla:ta1"]["today"]["cap"])  # no cap of its own: Tesla does the limiting
        self.assertIsNone(by["teslafi:fa1"]["today"]["cap"])  # nor TeslaFi: it does the limiting
        totals = {s: sum(1 for c in d["connections"] if c["status"] == s) for s in STATUSES}
        self.assertEqual(d["totals"], totals)
        self.assertGreater(sum(c["day"]["errors"] + c["week"]["errors"] for c in d["connections"]), 0)

    def test_errors_of_a_can_bridge_are_named(self):
        """A CAN Bridge row says what its errors were (logs left out, by why, with the page that shows them); logs
        asked of the car again are counted apart and are in neither the tries nor the errors; other rows name none."""
        d = self.c.get(PATH)
        known = {"could_not_sign", "not_signed", "failed_check", "other_vin", "unreadable", "unknown"}
        named = 0
        for c in d["connections"]:
            for p in ("day", "week"):
                x = c[p]
                self.assertEqual(set(x), {"count", "ok", "errors", "success_pct", "expected", "by_kind"}, c["id"])
                self.assertIsInstance(x["expected"], int)
                if c["kind"] != "smo":
                    self.assertEqual((x["expected"], x["by_kind"]), (0, []), c["id"])
                    continue
                self.assertEqual(sum(k["count"] for k in x["by_kind"]), x["errors"], c["id"])
                counts = [k["count"] for k in x["by_kind"]]
                self.assertEqual(counts, sorted(counts, reverse=True), "the most first")
                for k in x["by_kind"]:
                    self.assertIn(k["kind"], known)
                    self.assertTrue(k["link"].startswith(c["link"]), k)
                    named += 1
                    if k["kind"] in ("could_not_sign", "not_signed"):
                        self.assertEqual(k["link"], c["link"] + "#unsigned-logs")
        self.assertGreater(named, 0, "some CAN Bridge row has errors by kind")
        by = {c["id"]: c for c in d["connections"]}
        self.assertGreaterEqual(by["smo:c1"]["week"]["expected"], by["smo:c1"]["day"]["expected"])

    def test_hourly(self):
        d = self.c.get(PATH)
        cur = NOW - NOW % HOUR
        for c in d["connections"]:
            hs = c["hourly"]
            self.assertEqual(len(hs), 24, c["id"])
            self.assertEqual(hs[-1]["hour_ms"], cur)
            self.assertEqual([h["hour_ms"] for h in hs], [cur - (23 - k) * HOUR for k in range(24)])
            for h in hs:
                self.assertLessEqual(h["errors"], h["count"])
        by = {c["id"]: c for c in d["connections"]}
        # nothing comes from a CAN Bridge while its car sleeps or stays quiet
        self.assertEqual(sum(h["count"] for h in by["smo:c3"]["hourly"]), 0)
        self.assertIn(0, [h["count"] for h in by["smo:c1"]["hourly"]])
        # the tiles follow the day: nothing at night
        self.assertIn(0, [h["count"] for h in by["tiles"]["hourly"]])
        self.assertGreater(max(h["count"] for h in by["tiles"]["hourly"]), 100)

    def test_problems(self):
        d = self.c.get(PATH)
        ps = d["problems"]
        self.assertTrue(4 <= len(ps) <= 6, len(ps))
        ids = {c["id"] for c in d["connections"]}
        for p in ps:
            self.assertEqual(set(p), PROBLEM_KEYS)
            self.assertIn(p["connection"], ids)
            self.assertGreater(p["start_ms"], NOW - 7 * DAY - 7 * DAY)
            self.assertTrue(p["end_ms"] is None or p["end_ms"] > NOW - 7 * DAY)
            self.assertEqual(p["resolved"], p["end_ms"] is not None)
            self.assertEqual(p["duration_ms"], (p["end_ms"] or NOW) - p["start_ms"])
        starts = [p["start_ms"] for p in ps]
        self.assertEqual(starts, sorted(starts, reverse=True))
        open_ = [p for p in ps if not p["resolved"]]
        self.assertEqual(len(open_), 1)
        self.assertEqual(open_[0]["connection"], "smo:c3")
        durs = [p["duration_ms"] for p in ps if p["resolved"]]
        self.assertLess(min(durs), HOUR)
        self.assertGreater(max(durs), HOUR)
        # a connection is in `problem` exactly while it has an open problem
        st = {c["id"]: c["status"] for c in d["connections"]}
        self.assertEqual({p["connection"] for p in open_}, {k for k, v in st.items() if v == "problem"})
        # the uptime is the share of the week outside its problems
        by = {c["id"]: c for c in d["connections"]}
        c1 = [p for p in ps if p["connection"] == "smo:c1"][0]
        self.assertAlmostEqual(by["smo:c1"]["uptime"]["week_pct"], 100 * (1 - c1["duration_ms"] / (7 * DAY)), 1)

    def test_live_moves_in_place(self):
        a = self.c.get(PATH)
        self.server.app.clock.advance(5 * 60_000)
        b = self.c.get(PATH)
        try:
            self.assertEqual([c["id"] for c in a["connections"]], [c["id"] for c in b["connections"]])
            ab = {c["id"]: c for c in a["connections"]}
            for c in b["connections"]:
                o = ab[c["id"]]
                self.assertEqual(c["status"], o["status"], c["id"])
                if o["last_ok_ms"] is not None and c["status"] == "live":
                    self.assertGreaterEqual(c["last_ok_ms"], o["last_ok_ms"], c["id"])
                self.assertEqual(c["hourly"][-1]["hour_ms"], o["hourly"][-1]["hour_ms"])
                self.assertGreaterEqual(c["hourly"][-1]["count"], o["hourly"][-1]["count"], c["id"])
                self.assertEqual(c["hourly"][:-1], o["hourly"][:-1], c["id"])
            self.assertGreater(b["connections"][0]["last_ok_ms"], a["connections"][0]["last_ok_ms"])
            open_a = [p for p in a["problems"] if not p["resolved"]][0]
            open_b = [p for p in b["problems"] if not p["resolved"]][0]
            self.assertEqual(open_b["duration_ms"] - open_a["duration_ms"], 5 * 60_000)
        finally:
            self.server.app.clock.advance(-5 * 60_000)


class WatchRefused(ServerCase):
    server_args = {"role": "watch"}

    def test_watch_gets_403(self):
        self.c.error("GET", PATH, 403, "not_allowed")
        self.c.error("GET", "/api/v1/integrations/tesla", 403, "not_allowed")


class LargeFleet(ServerCase):
    server_args = {"cars": 500}

    def test_capped_rows_honest_totals(self):
        d = self.c.get(PATH)
        smo = [c for c in d["connections"] if c["kind"] == "smo"]
        self.assertEqual(len(smo), 50)
        self.assertEqual(smo[0]["id"], "smo:c1")
        others = len(d["connections"]) - len(smo)
        # 500 cars with the sold one among them: 499 CAN Bridges
        self.assertEqual(sum(d["totals"].values()), 499 + others)


class Determinism(unittest.TestCase):
    def answer(self, **kw):
        r = Running(**kw)
        try:
            st, _hd, raw = r.client().request("GET", PATH)
            self.assertEqual(st, 200)
            return raw
        finally:
            r.close()

    def test_same_seed_same_answer(self):
        a, b = self.answer(seed=7), self.answer(seed=7)
        self.assertEqual(a, b)
        c = self.answer(seed=8)
        self.assertNotEqual(json.loads(a)["connections"][7]["hourly"], json.loads(c)["connections"][7]["hourly"])


if __name__ == "__main__":
    unittest.main()
