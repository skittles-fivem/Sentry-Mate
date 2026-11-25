"""GET /api/v1/cars/{car}/firmware/history: every fingerprint each module had, and imported versions."""

from __future__ import annotations

from .helpers import ServerCase

MODS = ["vcfront", "bms", "dir", "dif"]
MAIN = {"vcfront": "VCFRONT_infoAppCrc", "bms": "BMS_appCrc", "dir": "DIR_applicationCrc",
        "dif": "DIF_applicationCrc"}
ROW_KEYS = {"id", "kind", "module", "module_name", "fingerprint", "previous", "fields", "first_seen_ms",
            "last_seen_ms", "until_ms", "duration_ms", "current", "update", "version", "version_source", "name_inferred", "changed_in", "source",
            "source_label"}


def url(car, q=""):
    return f"/api/v1/cars/{car}/firmware/history" + (f"?{q}" if q else "")


class FirmwareHistory(ServerCase):
    def test_shape_and_order(self):
        for car in ("c1", "c2", "c3"):
            d = self.c.get(url(car, "limit=500"))
            self.assertEqual((d["car"], d["next"]), (car, None))
            self.assertTrue(d["time_zone"])
            self.assertEqual([m["id"] for m in d["modules"]], MODS)
            keys = [(x["first_seen_ms"], x["id"]) for x in d["items"]]
            self.assertEqual(keys, sorted(keys, reverse=True), "newest first")
            for x in d["items"]:
                self.assertEqual(set(x), ROW_KEYS, x["id"])
                if x["kind"] == "module":
                    self.assertEqual(x["id"], f"{x['module']}:{x['first_seen_ms']}")
                    self.assertRegex(x["fingerprint"], r"^[0-9a-f]{8}$")
                    self.assertEqual(x["fields"][0]["field"], MAIN[x["module"]])
                    self.assertEqual(x["fields"][0]["hex"], x["fingerprint"])
                    self.assertEqual(x["duration_ms"], x["last_seen_ms"] - x["first_seen_ms"])
                    self.assertEqual((x["source"], x["source_label"]), ("can", "Data from SMO"))
                    self.assertEqual(x["current"], x["until_ms"] is None)
                    if x["previous"] is None:
                        self.assertIsNone(x["update"])
                        self.assertIsNone(x["changed_in"])
                    else:
                        self.assertNotEqual(x["previous"], x["fingerprint"])
                        self.assertEqual(x["version"], x["update"]["version"])
                        self.assertTrue(x["changed_in"]["log"].endswith(".log"))
                else:
                    self.assertEqual(x["kind"], "version")
                    self.assertTrue(x["id"].startswith("version:"))
                    self.assertIsNone(x["module"])
                    self.assertEqual(x["fields"], [])
                    self.assertIn(x["source"], ("teslamate", "teslafi"))
                    self.assertTrue(x["version"])
            spans = d["spans"]
            self.assertEqual([s["from_ms"] for s in spans], sorted(s["from_ms"] for s in spans))
            mrows = [x for x in d["items"] if x["kind"] == "module"]
            self.assertEqual(len(spans), len(mrows))
            if mrows:
                self.assertEqual(d["range"]["from_ms"], min(x["first_seen_ms"] for x in mrows))
                self.assertEqual(d["range"]["to_ms"], max(x["last_seen_ms"] for x in mrows))

    def test_made_up_history(self):
        for car in ("c1", "c3"):  # cars with a front drive unit: every module has a history
            d = self.c.get(url(car, "limit=500"))
            counts = {m["id"]: m["fingerprints"] for m in d["modules"]}
            self.assertTrue(all(n >= 2 for n in counts.values()), (car, counts))
            self.assertGreaterEqual(max(counts.values()), 3, car)
            ups = {u["id"]: u for u in self.c.get(f"/api/v1/cars/{car}/updates?limit=500")["items"]}
            named = [x for x in d["items"] if x["kind"] == "module" and x["version"]]
            self.assertTrue(named, car)
            for x in d["items"]:
                if x["update"]:
                    self.assertEqual(x["update"]["version"], ups[x["update"]["id"]]["version"])
                    self.assertEqual(x["update"]["version_source"], ups[x["update"]["id"]]["version_source"])
        c3 = self.c.get(url("c3", "limit=500"))["items"]
        vs = [x for x in c3 if x["kind"] == "version"]
        self.assertGreaterEqual(len(vs), 4)
        self.assertEqual([x["current"] for x in vs], [True] + [False] * (len(vs) - 1))
        c2 = self.c.get(url("c2"))
        self.assertEqual(c2["modules"][3], {"id": "dif", "name": "Front drive unit", "can_id": 0x656,
                                            "fingerprints": 0, "current": None})

    def test_current_matches_firmware(self):
        for car in ("c1", "c2", "c3"):
            fw = {m["id"]: m for m in self.c.get(f"/api/v1/cars/{car}/firmware")["modules"]}
            d = self.c.get(url(car, "limit=500"))
            for m in d["modules"]:
                main = next((f for f in fw[m["id"]]["fields"] if f["field"] == MAIN[m["id"]]), None)
                self.assertEqual(m["current"], main["hex"] if main else None, (car, m["id"]))
                if main:
                    self.assertEqual(m["fingerprints"], len(main["history"]))
                    cur = [x for x in d["items"] if x["module"] == m["id"] and x["current"]]
                    self.assertEqual([x["fingerprint"] for x in cur], [main["hex"]])

    def test_paging(self):
        whole = self.c.get(url("c3", "limit=500"))["items"]
        walked = self.walk(url("c3"), limit=3)
        self.assertEqual([x["id"] for x in walked], [x["id"] for x in whole])
        self.assertEqual(len({x["id"] for x in walked}), len(walked))
        for q in ("limit=0", "limit=501", "limit=x"):
            self.c.error("GET", url("c3", q), 400, "bad_input")
        self.c.error("GET", url("c3", "after=nonsense"), 400, "bad_input")

    def test_module_filter(self):
        d = self.c.get(url("c1", "module=bms&limit=500"))
        self.assertTrue(d["items"])
        self.assertTrue(all(x["module"] == "bms" and x["kind"] == "module" for x in d["items"]))
        self.assertEqual(len(d["items"]), d["modules"][1]["fingerprints"])
        self.assertEqual([m["id"] for m in d["modules"]], MODS, "modules are not filtered")
        self.assertEqual(len(d["spans"]), sum(m["fingerprints"] for m in d["modules"]), "spans are not filtered")
        walked = self.walk(url("c1", "module=bms"), limit=1)
        self.assertEqual([x["id"] for x in walked], [x["id"] for x in d["items"]])
        e, _ = self.c.error("GET", url("c1", "module=gateway"), 400, "bad_input")
        self.assertIn("module", e["fields"])
        self.c.error("GET", url("c999"), 404, "no_such_car")

    def first(self, car, module="bms"):
        return self.c.get(url(car, f"module={module}&limit=500"))["items"][-1]

    def test_first_segment_inferred_from_the_last_known_version(self):
        # c3's imported history ends (2025.20.6) before its first CAN fingerprints, with no update between
        for m in MODS:
            f = self.first("c3", m)
            self.assertIsNone(f["version"])
            self.assertEqual(f["name_inferred"], {"version": "2025.20.6", "from": "last known before"}, m)
        d = self.c.get(url("c3", "limit=500"))
        spans = [s for s in d["spans"] if not s["current"]]
        self.assertTrue(any(s["name_inferred"] for s in spans))
        # a later segment is never inferred, and c1 has no import to infer from
        self.assertTrue(all(x["name_inferred"] is None for x in d["items"] if x["previous"] is not None))
        self.assertIsNone(self.first("c1")["name_inferred"])

    def test_naming_a_segment(self):
        f = self.first("c3", "dir")
        path = f"/api/v1/cars/c3/firmware/history/{f['id']}"
        self.c.error("PATCH", path, 400, "bad_input", body={})
        self.c.error("PATCH", path, 422, "invalid", body={"version": "latest"})
        self.c.error("PATCH", "/api/v1/cars/c3/firmware/history/dir:1", 404, "no_such_update",
                     body={"version": "2026.1.1"})
        one = self.c.json("PATCH", path, {"version": " 2026.38.6 "})
        self.assertEqual((one["version"], one["version_source"], one["name_inferred"]), ("2026.38.6", "owner", None))
        again = self.first("c3", "dir")
        self.assertEqual((again["version"], again["version_source"]), ("2026.38.6", "owner"))
        self.assertIsNone(self.first("c3", "bms")["version"])
        self.assertIn("update-version", str(self.c.get("/api/v1/audit?limit=5")))
        self.c.json("PATCH", path, {"version": None})
        self.assertEqual(self.first("c3", "dir")["name_inferred"]["version"], "2025.20.6")
        # a later segment names its update
        later = self.c.get(url("c3", "module=bms&limit=500"))["items"][0]
        self.assertIsNotNone(later["update"])
        self.c.json("PATCH", f"/api/v1/cars/c3/firmware/history/{later['id']}", {"version": "2026.44.2"})
        self.assertEqual(self.c.get(url("c3", "module=bms&limit=500"))["items"][0]["version"], "2026.44.2")


class WatchFirmwareHistory(ServerCase):
    server_args = {"role": "watch"}

    def test_watch_reads_its_car(self):
        self.assertTrue(self.c.get(url("c1"))["items"])
        row = self.c.get(url("c1", "module=bms"))["items"][-1]
        self.c.error("PATCH", f"/api/v1/cars/c1/firmware/history/{row['id']}", 403, "not_allowed",
                     body={"version": "2026.1.1"})
        self.c.error("GET", url("c2"), 404, "no_such_car")
