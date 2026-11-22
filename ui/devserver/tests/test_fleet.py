"""Fleet and cars."""

from __future__ import annotations

import unittest

from .helpers import ServerCase

ITEM = {"id", "name", "model", "year", "vin_prefix", "trim", "profile", "time_zone", "time_zone_source",
        "last_activity_ms", "now", "attention", "sources", "source_label", "sold", "sold_date"}
NOW_KEYS = {"state", "since_ms", "at_ms", "received_ms", "stale", "soc_pct", "range_km", "ideal_range_km", "place", "plugged",
            "charge_power_kw"}


class Cars(ServerCase):
    def test_list(self):
        d = self.c.get("/api/v1/cars")
        self.assertEqual(d["total"], 4)
        ids = {x["id"] for x in d["items"]}
        self.assertEqual(ids, {"c1", "c2", "c3", "c0"})  # c0: the car the owner sold
        by = {x["id"]: x for x in d["items"]}
        self.assertEqual((by["c0"]["sold"], by["c0"]["attention"]), (True, []))
        self.assertFalse(by["c1"]["sold"])
        for x in d["items"]:
            self.assertEqual(set(x), ITEM)
            self.assertEqual(set(x["now"]), NOW_KEYS)
            self.assertEqual(len(x["vin_prefix"]), 11)  # never the serial
        by = {x["id"]: x for x in d["items"]}
        self.assertEqual(by["c1"]["now"]["state"], "driving")
        self.assertEqual(by["c2"]["now"]["state"], "charging")
        self.assertTrue(by["c2"]["now"]["plugged"])
        self.assertGreater(by["c2"]["now"]["charge_power_kw"], 1)
        self.assertEqual(by["c3"]["now"]["state"], "parked")  # silent, its newest log ended parked
        self.assertTrue(by["c3"]["now"]["stale"])
        self.assertIn("upload_silence", by["c3"]["attention"])
        self.assertIn("tyre_low", by["c2"]["attention"])
        self.assertEqual(by["c1"]["time_zone"], "America/Los_Angeles")
        self.assertEqual(by["c2"]["time_zone"], "America/Chicago")
        self.assertEqual((by["c1"]["time_zone_source"], by["c2"]["time_zone_source"]), ("position", "owner"))

    def test_filters_and_sorts(self):
        self.assertEqual([x["id"] for x in self.c.get("/api/v1/cars?state=charging")["items"]], ["c2"])
        self.assertEqual([x["id"] for x in self.c.get("/api/v1/cars?search=atl")["items"]], ["c3"])
        self.assertEqual([x["id"] for x in self.c.get("/api/v1/cars?search=model%203")["items"]], ["c2", "c0"])
        names = [x["name"] for x in self.c.get("/api/v1/cars?sort=name")["items"]]
        self.assertEqual(names, sorted(names, key=str.lower))
        rev = [x["name"] for x in self.c.get("/api/v1/cars?sort=name&dir=desc")["items"]]
        self.assertEqual(rev, names[::-1])
        acts = [x["last_activity_ms"] for x in self.c.get("/api/v1/cars")["items"]]
        self.assertEqual(acts, sorted(acts, reverse=True))
        self.c.error("GET", "/api/v1/cars?state=flying", 400, "bad_input")

    def test_summary_and_attention(self):
        s = self.c.get("/api/v1/fleet/summary")
        self.assertEqual(s["cars"], 3)
        self.assertEqual(sum(s["by_state"].values()), 3)
        self.assertEqual(set(s["by_state"]), {"driving", "charging", "parked", "asleep", "unknown"})
        a = self.c.get("/api/v1/fleet/attention")
        codes = {(x["car"], x["code"]) for x in a["items"]}
        self.assertIn(("c2", "tyre_low"), codes)
        self.assertIn(("c3", "upload_silence"), codes)
        self.assertIn(("c3", "trust_failed"), codes)
        tyre = next(x for x in a["items"] if x["code"] == "tyre_low")
        self.assertEqual(tyre["detail"]["tyre"], "rl")
        self.assertLess(tyre["detail"]["pressure_kpa"], tyre["detail"]["recommended_kpa"])

    def test_detail_and_patch(self):
        d = self.c.get("/api/v1/cars/c1")
        self.assertTrue(ITEM <= set(d))
        self.assertIn("version", d)
        for k, v in d["config"].items():
            self.assertEqual(set(v), {"value", "source", "sources"}, k)
        self.assertEqual(d["config"]["paint"]["source"], "can")
        self.assertEqual([x["kind"] for x in d["config"]["paint"]["sources"]], ["smo"])
        self.assertEqual([x["kind"] for x in d["sources"]], ["smo", "tesla_api"])
        self.assertEqual(d["source_label"], "Data from SMO and the Tesla API")
        up = self.c.json("PATCH", "/api/v1/cars/c1", {"version": d["version"], "config": {"paint": "Ultra Red"},
                                                       "time_zone": "America/Denver"})
        self.assertEqual(up["config"]["paint"], {"value": "Ultra Red", "source": "owner", "sources": []})
        self.assertEqual(up["time_zone"], "America/Denver")
        self.assertEqual(self.c.get("/api/v1/cars/c1/now")["time_zone"], "America/Denver")
        self.c.error("PATCH", "/api/v1/cars/c1", 422, "invalid", body={"version": up["version"], "time_zone": "Mars/Base"})
        self.c.error("PATCH", "/api/v1/cars/c1", 400, "bad_input", body={"version": up["version"], "config": {"colour": 1}})
        self.assertEqual(up["time_zone_source"], "owner")
        back = self.c.json("PATCH", "/api/v1/cars/c1", {"version": up["version"], "config": {"paint": None},
                                                         "time_zone": None})
        self.assertEqual(back["config"]["paint"]["source"], "can")
        # back to automatic: the car's zone from its position
        self.assertEqual((back["time_zone"], back["time_zone_source"]), ("America/Los_Angeles", "position"))
        self.c.error("PATCH", "/api/v1/cars/c1", 400, "bad_input", body={"version": back["version"], "time_zone": 7})

    def test_vehicle_info(self):
        d = self.c.get("/api/v1/cars/c1")
        v = d["vehicle"]
        self.assertEqual(v["vin_prefix"], d["vin_prefix"])
        self.assertEqual(v["model"], {"value": "Model Y", "source": "vin", "confidence": "high"})
        self.assertEqual(v["year_kind"], "model_year")
        self.assertEqual(v["plant"]["value"], "Fremont; California")
        self.assertEqual(v["drive"]["value"], "AWD")
        self.assertEqual(v["steering"]["value"], "left")
        self.assertEqual(v["nominal_kwh"]["source"], "can")
        for k in ("disagreements", "unknown_codes", "notes", "trim_candidates", "wheel_options_in"):
            self.assertEqual(v[k], [], k)
        for k, f in v.items():
            if isinstance(f, dict) and "confidence" in f:
                self.assertIn(f["source"], (None, "owner", "can", "vin", "tesla_api", "teslamate"), k)
        # the owner overrides any field; config keeps its keys
        up = self.c.json("PATCH", "/api/v1/cars/c1", {"version": d["version"], "config": {"model_year": 2024, "motors": 2}})
        self.assertEqual(up["vehicle"]["model_year"]["source"], "owner")
        self.assertEqual(up["vehicle"]["motors"], {"value": 2, "source": "owner", "confidence": "high"})
        self.assertNotIn("model_year", up["config"])
        self.c.json("PATCH", "/api/v1/cars/c1", {"version": up["version"], "config": {"model_year": None, "motors": None}})
        # a Model S from before the refresh
        self.assertEqual(self.c.get("/api/v1/cars/c3")["vehicle"]["platform"]["value"], "legacy")

    def test_owner_keys_are_checked_like_the_server(self):
        d = self.c.get("/api/v1/cars/c1")
        e, _ = self.c.error("PATCH", "/api/v1/cars/c1", 400, "bad_input",
                            body={"version": d["version"], "config": {"chemistry": "foo", "motors": "two"}})
        self.assertEqual(set(e["fields"]), {"config.chemistry", "config.motors"})
        e, _ = self.c.error("PATCH", "/api/v1/cars/c1", 422, "invalid",
                            body={"version": d["version"], "config": {"model_year": 1990, "seats": 2.5,
                                                                      "wheel_in": 30, "model": " "}})
        self.assertEqual(set(e["fields"]), {"config.model_year", "config.seats", "config.wheel_in", "config.model"})
        self.assertEqual(self.c.get("/api/v1/cars/c1")["version"], d["version"], "nothing changed")
        # a typed model and year are also the item's, in the list too
        up = self.c.json("PATCH", "/api/v1/cars/c1", {"version": d["version"],
                                                       "config": {"model_year": 2024, "model": " Model Y L "}})
        self.assertEqual((up["year"], up["model"]), (2024, "Model Y L"))
        self.assertEqual(up["vehicle"]["model"]["value"], "Model Y L")
        one = [x for x in self.c.get("/api/v1/cars")["items"] if x["id"] == "c1"][0]
        self.assertEqual((one["year"], one["model"]), (2024, "Model Y L"))
        back = self.c.json("PATCH", "/api/v1/cars/c1", {"version": up["version"],
                                                         "config": {"model_year": None, "model": None}})
        self.assertEqual((back["year"], back["model"]), (d["year"], d["model"]))

    def test_battery_setting_is_the_owners_chemistry(self):
        v = self.c.get("/api/v1/cars/c1")["vehicle"]
        self.assertNotEqual(v["chemistry"]["source"], "owner")
        s = self.c.get("/api/v1/cars/c1/settings")
        up = self.c.json("PATCH", "/api/v1/cars/c1/settings", {"version": s["version"],
                                                                "battery_chemistry": {"setting": "lfp"}})
        self.assertEqual(self.c.get("/api/v1/cars/c1")["vehicle"]["chemistry"],
                         {"value": "lfp", "source": "owner", "confidence": "high"})
        self.c.json("PATCH", "/api/v1/cars/c1/settings", {"version": up["version"],
                                                          "battery_chemistry": {"setting": "auto"}})
        self.assertNotEqual(self.c.get("/api/v1/cars/c1")["vehicle"]["chemistry"]["source"], "owner")

    def test_a_us_vin_only_hints_at_the_chemistry(self):
        ch = self.c.get("/api/v1/cars/c2")["vehicle"]["chemistry"]
        self.assertEqual((ch["value"], ch["source"], ch["confidence"]), ("lfp", "vin", "low"))
        self.assertIn("does not tell", ch["note"])


class VehicleInfoShape(unittest.TestCase):
    def test_the_year_note_of_a_build_year(self):
        from mockapi.resources.fleet import vehicle_info
        sh = vehicle_info({"vin_prefix": "LRW3E7EB0PC", "model": "Model 3", "year": 2023, "config": {}})
        self.assertEqual(sh["year_kind"], "build_year")
        self.assertIn("calendar year built", sh["model_year"]["note"])
        us = vehicle_info({"vin_prefix": "5YJ3E1EA0PF", "model": "Model 3", "year": 2023, "config": {}})
        self.assertEqual((us["year_kind"], us["model_year"]["note"]), ("model_year", "VIN position 10 (49 CFR 565.15)"))
        typed = vehicle_info({"vin_prefix": "LRW3E7EB0PC", "model": "Model 3", "year": 2023,
                              "config": {"model_year": {"value": 2024, "source": "owner"}}})
        self.assertEqual((typed["model_year"]["value"], typed["year_kind"]), (2024, "model_year"))


class Settings(ServerCase):
    def test_settings(self):
        s = self.c.get("/api/v1/cars/c3/settings")
        self.assertEqual(s["free_supercharging"], [{"from_date": "2017-03-01", "to_date": None}])
        self.assertEqual(self.c.get("/api/v1/cars/c2/settings")["battery_chemistry"]["detected"], "lfp")
        e, _ = self.c.error("PATCH", "/api/v1/cars/c3/settings", 422, "invalid",
                            body={"version": s["version"], "warranty_threshold_pct": 120,
                                  "software_version_typed": "latest"})
        self.assertEqual(set(e["fields"]), {"warranty_threshold_pct", "software_version_typed"})
        up = self.c.json("PATCH", "/api/v1/cars/c3/settings", {"version": s["version"], "software_version_typed": "2026.38.4"})
        self.assertEqual(up["version"], s["version"] + 1)
        sw = self.c.get("/api/v1/cars/c3/now")["software"]
        self.assertEqual((sw["version"], sw["version_source"]), ("2026.38.4", "owner"))


class TokensAndKeys(ServerCase):
    def test_tokens(self):
        lst = self.c.get("/api/v1/cars/c1/upload-tokens")
        self.assertEqual(len(lst["items"]), 2)
        st, hd, raw = self.c.request("POST", "/api/v1/cars/c1/upload-tokens")
        self.assertEqual(st, 201)
        import json
        t = json.loads(raw)
        self.assertTrue(t["secret"].startswith("smu_"))
        again = self.c.get(f"/api/v1/cars/c1/upload-tokens/{t['id']}")
        self.assertNotIn("secret", again)
        gone = self.c.json("DELETE", f"/api/v1/cars/c1/upload-tokens/{t['id']}")
        self.assertTrue(gone["revoked"])

    def test_new_car_tokens(self):
        self.assertEqual(self.c.get("/api/v1/upload-tokens")["items"], [])
        import json
        st, hd, raw = self.c.request("POST", "/api/v1/upload-tokens")
        self.assertEqual(st, 201)
        t = json.loads(raw)
        self.assertTrue(t["secret"].startswith("smu_"))
        self.assertIsNone(t["car"])
        listed = self.c.get("/api/v1/upload-tokens")["items"]
        self.assertEqual([x["id"] for x in listed], [t["id"]])
        self.assertNotIn("secret", listed[0])
        gone = self.c.json("DELETE", f"/api/v1/upload-tokens/{t['id']}")
        self.assertTrue(gone["revoked"])
        self.c.error("DELETE", "/api/v1/upload-tokens/nope", 404, "no_such_token")

    def test_keys(self):
        waiting = self.c.get("/api/v1/can-keys?assigned=false")["items"]
        self.assertEqual(len(waiting), 1)
        k = waiting[0]
        self.assertEqual(set(k) >= {"key", "first_seen_ms", "last_upload_ms", "logs_waiting", "car", "preferred"}, True)
        status = self.c.get("/api/v1/setup/status")
        self.assertEqual(status["keys_waiting"], 1)
        self.assertIn("key_waiting", [f["cause"] for f in status["fixes"]])
        self.c.error("PATCH", f"/api/v1/can-keys/{k['key']}", 422, "invalid", body={"car": "c42"})
        up = self.c.json("PATCH", f"/api/v1/can-keys/{k['key']}", {"car": "c3", "preferred": True})
        self.assertEqual((up["car"], up["preferred"]), ("c3", True))
        c3keys = [x for x in self.c.get("/api/v1/can-keys?assigned=true")["items"] if x["car"] == "c3"]
        self.assertEqual(sum(1 for x in c3keys if x["preferred"]), 1)
        self.assertEqual(self.c.get("/api/v1/setup/status")["keys_waiting"], 0)
        self.c.json("PATCH", f"/api/v1/can-keys/{k['key']}", {"car": None})

    def test_keys_of_one_car(self):
        mine = self.c.get("/api/v1/can-keys?car=c1")["items"]
        self.assertTrue(mine)
        self.assertTrue(all(x["car"] == "c1" for x in mine))
        self.c.error("GET", "/api/v1/can-keys?car=c42", 404, "no_such_car")


class WatchSetupStatus(ServerCase):
    server_args = {"role": "watch"}

    def test_only_its_own_cars(self):
        # a key waiting for a car and the tokens for a new car are an admin's to read
        self.c.error("GET", "/api/v1/can-keys", 403, "not_allowed")
        st = self.c.get("/api/v1/setup/status")
        self.assertEqual(st["keys_waiting"], 0)
        self.assertNotIn("key_waiting", [f["cause"] for f in st["fixes"]])
        self.assertFalse(any("key" in f for f in st["fixes"]))


class EmptyFleet(ServerCase):
    server_args = {"cars": 0}

    def test_empty(self):
        self.assertEqual(self.c.get("/api/v1/cars")["items"], [])
        self.assertEqual(self.c.get("/api/v1/fleet/summary")["cars"], 0)
        st = self.c.get("/api/v1/setup/status")
        self.assertEqual(st["cars"], 0)
        self.assertIn("no_token", [f["cause"] for f in st["fixes"]])


class SoldCar(ServerCase):
    def test_sold_and_undone(self):
        s = self.c.get("/api/v1/fleet/summary")
        self.assertEqual((s["cars"], s["sold"]), (3, 1))
        self.assertFalse([a for a in self.c.get("/api/v1/fleet/attention")["items"] if a["car"] == "c0"])
        v = self.c.get("/api/v1/cars/c1")["version"]
        r = self.c.json("PATCH", "/api/v1/cars/c1", {"version": v, "sold": True, "sold_date": "2026-10-01"})
        self.assertEqual((r["sold"], r["sold_date"], r["attention"]), (True, "2026-10-01", []))
        self.assertEqual(self.c.get("/api/v1/fleet/summary")["sold"], 2)
        r = self.c.json("PATCH", "/api/v1/cars/c1", {"version": r["version"], "sold": False})
        self.assertEqual((r["sold"], r["sold_date"]), (False, None))
