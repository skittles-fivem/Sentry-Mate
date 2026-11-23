"""A car's live inputs in its settings: `source_order` (highest priority first, the car's or the default), `inputs`
(one per input word, on or off for this car), `imports` and `import_gap_fill_default`; PATCH `source_order`."""

from __future__ import annotations

from .helpers import NOW, ServerCase

DEFAULT = ["smo", "tesla_api", "teslafi_api", "teslamate_local"]
LABELS = {"smo": "SMO", "tesla_api": "Tesla API", "teslafi_api": "TeslaFi", "teslamate_local": "local TeslaMate"}
RULE = "List each input once: smo, tesla_api, teslafi_api, teslamate_local."


class SourceOrder(ServerCase):
    def settings(self, car="c1"):
        return self.c.get(f"/api/v1/cars/{car}/settings")

    def patch(self, order, car="c1", expect=200):
        s = self.settings(car)
        return self.c.json("PATCH", f"/api/v1/cars/{car}/settings", {"version": s["version"], "source_order": order},
                           expect=expect)

    def test_default_get(self):
        for car in ("c1", "c2", "c3"):
            s = self.settings(car)
            self.assertEqual(s["source_order"], DEFAULT, car)
            self.assertEqual(s["source_order_default"], DEFAULT, car)
            self.assertEqual([x["kind"] for x in s["inputs"]], DEFAULT, car)
            for x in s["inputs"]:
                self.assertEqual(set(x), {"kind", "label", "on", "last_ms"}, car)
                self.assertEqual(x["label"], LABELS[x["kind"]], car)
                self.assertIsInstance(x["on"], bool)
                if not x["on"]:
                    self.assertIsNone(x["last_ms"], car)
                elif x["last_ms"] is not None:
                    self.assertLessEqual(x["last_ms"], NOW)
            self.assertIs(s["import_gap_fill_default"], True)
            self.assertIsInstance(s["imports"], list)
        # a car with two inputs on, and inputs off
        c1 = {x["kind"]: x["on"] for x in self.settings("c1")["inputs"]}
        self.assertEqual(c1, {"smo": True, "tesla_api": True, "teslafi_api": False, "teslamate_local": False})
        # the same inputs as /now says
        now = self.c.get("/api/v1/cars/c1/now")
        self.assertEqual({x["kind"] for x in now["inputs"]}, {k for k, on in c1.items() if on})
        # imports: Atlas has its TeslaMate and TeslaFi history, Comet none
        self.assertEqual(self.settings("c1")["imports"], [])
        imp = {x["site"]: x for x in self.settings("c3")["imports"]}
        self.assertEqual(set(imp), {"teslamate", "teslafi"})
        self.assertEqual(imp["teslamate"]["label"], "Imported from TeslaMate")
        self.assertEqual(imp["teslafi"]["label"], "Imported from TeslaFi")
        for x in imp.values():
            self.assertLess(x["from_ms"], x["to_ms"])

    def test_valid_order_round_trips_and_null_resets(self):
        before = self.settings("c2")
        order = ["tesla_api", "teslamate_local", "smo", "teslafi_api"]
        out = self.patch(order, "c2")
        self.assertEqual(out["source_order"], order)
        self.assertEqual(out["source_order_default"], DEFAULT)
        self.assertEqual(out["version"], before["version"] + 1)
        self.assertEqual([x["kind"] for x in out["inputs"]], order)
        self.assertEqual(self.settings("c2")["source_order"], order)
        # the car's rows are fused again: rebuild_pending for a moment
        self.assertTrue(self.c.get("/api/v1/cars/c2/health")["rebuild_pending"])
        # /now lists the inputs in the car's order
        self.assertEqual([x["kind"] for x in self.c.get("/api/v1/cars/c2/now")["inputs"]], ["tesla_api", "smo"])
        # lists of origins follow it too
        q = f"/api/v1/cars/c2/series?fields=speed_kph&from_ms={NOW - 2 * 86_400_000}&to_ms={NOW}&max_points=200"
        self.assertEqual([x["kind"] for x in self.c.get(q)["sources"]], ["tesla_api", "smo"])
        # other cars keep the default
        self.assertEqual(self.settings("c1")["source_order"], DEFAULT)
        out = self.patch(None, "c2")
        self.assertEqual(out["source_order"], DEFAULT)
        self.assertEqual([x["kind"] for x in self.c.get(q)["sources"]], ["smo", "tesla_api"])
        self.assertEqual(self.settings("c2")["source_order"], DEFAULT)

    def test_invalid_orders_are_422(self):
        bad = [
            ["smo", "smo", "tesla_api", "teslafi_api"],  # duplicate
            ["smo", "tesla_api", "teslafi_api"],  # missing
            ["smo", "tesla_api", "teslafi_api", "teslamate_local", "smo"],  # too many
            ["smo", "tesla_api", "teslafi_api", "can"],  # unknown
            "smo,tesla_api,teslafi_api,teslamate_local",  # not a list
            {"smo": 1},
            [1, 2, 3, 4],
            [],
        ]
        v = self.settings("c3")["version"]
        for order in bad:
            e, _ = self.c.error("PATCH", "/api/v1/cars/c3/settings", 422, "invalid",
                                {"version": v, "source_order": order})
            self.assertEqual(e["fields"], {"source_order": RULE}, order)
        after = self.settings("c3")
        self.assertEqual(after["version"], v, "nothing changed")
        self.assertEqual(after["source_order"], DEFAULT)
