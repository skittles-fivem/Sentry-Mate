"""Battery and efficiency: health, capacity from charges, charge level, projected range, parked
drain, consumption by temperature and temperatures."""

from __future__ import annotations

from .helpers import DAY, NOW, ServerCase


class Battery(ServerCase):
    def test_whole_life_totals_add_up_and_stay_possible(self):
        for car in ("c1", "c2", "c3"):
            t = self.c.get(f"/api/v1/cars/{car}/totals")
            self.assertEqual(t["ac"]["charges"] + t["dc"]["charges"] + t["unknown_kind"]["charges"], t["charges"])
            if t["driven_since_km"] is not None:
                self.assertGreaterEqual(t["driven_since_km"], 0)
                self.assertAlmostEqual(t["driven_since_km"], t["odometer_last_km"] - t["odometer_first_km"], delta=0.01)
            if t["charging_efficiency_pct"] is not None:
                self.assertLessEqual(t["charging_efficiency_pct"], 100)

    def test_health_and_sources(self):
        b1 = self.c.get("/api/v1/cars/c1/battery")
        cap = b1["capacity"]
        self.assertEqual(cap["new_source"], "early_bms")
        self.assertAlmostEqual(cap["usable_kwh"], cap["full_pack_kwh"] - cap["buffer_kwh"], delta=0.02)
        self.assertGreater(cap["difference_kwh"], 0)
        self.assertAlmostEqual(cap["difference_kwh"], cap["new_kwh"] - cap["usable_kwh"], delta=0.06)
        self.assertAlmostEqual(cap["health_pct"], cap["usable_kwh"] / cap["new_kwh"] * 100, delta=0.11)
        self.assertAlmostEqual(cap["health_pct"], 100 - cap["degradation_pct"], delta=0.11)
        self.assertEqual(b1["band"], {"low_pct": 20, "high_pct": 80, "charge_limit_pct": 80})
        self.assertEqual(self.c.get("/api/v1/cars/c3/battery")["capacity"]["new_source"], "owner")
        b2 = self.c.get("/api/v1/cars/c2/battery")
        chem = b2["chemistry"]
        self.assertEqual((chem["value"], chem["source"]), ("lfp", "detected"))
        self.assertIn(chem["confidence"], ("low", "medium", "high"))
        # the same detection as Car settings shows
        self.assertEqual(self.c.get("/api/v1/cars/c2/settings")["battery_chemistry"]["detected"], chem["value"])
        self.assertEqual(b2["band"]["high_pct"], 100)
        self.assertIn("lfp_charge_to_100_weekly", b2["notes"])
        life = b1["lifetime"]
        self.assertAlmostEqual(life["charged_kwh"], life["ac_kwh"] + life["dc_kwh"] + life["unknown_kwh"], delta=0.11)
        self.assertGreater(life["equivalent_cycles"], 1)

    def test_a_pack_is_never_healthier_than_new(self):
        b = self.c.get("/api/v1/cars/c2/battery")
        self.assertNotIn("reading_above_new", b["notes"])
        self.assertEqual(b["lifetime"]["unknown_kwh"], 0)
        now_kwh = b["capacity"]["usable_kwh"]  # the value when new is the usable capacity, so it is compared with the usable capacity now
        s = self.c.get("/api/v1/cars/c2/settings")
        # a value when new typed below what the pack reads now: no loss, full health, a note, the difference as it is
        up = self.c.json("PATCH", "/api/v1/cars/c2/settings", {"version": s["version"], "capacity_new_kwh": round(now_kwh - 4, 1)})
        cap = self.c.get("/api/v1/cars/c2/battery")
        self.assertEqual((cap["capacity"]["degradation_pct"], cap["capacity"]["health_pct"]), (0, 100))
        self.assertAlmostEqual(cap["capacity"]["difference_kwh"], -4, delta=0.06)
        self.assertIn("reading_above_new", cap["notes"])
        self.c.json("PATCH", "/api/v1/cars/c2/settings", {"version": up["version"], "capacity_new_kwh": None})
        self.assertNotIn("reading_above_new", self.c.get("/api/v1/cars/c2/battery")["notes"])

    def test_capacity_follows_the_charges(self):
        cap = self.c.get(f"/api/v1/cars/c1/battery/capacity?group=charge&from_ms={NOW - 60 * DAY}&limit=500")
        charges = {x["id"]: x for x in self.c.get(f"/api/v1/cars/c1/charges?from_ms={NOW - 60 * DAY}&limit=500")["items"]}
        self.assertGreater(len(cap["items"]), 5)
        buffer = self.c.get("/api/v1/cars/c1/battery")["capacity"]["buffer_kwh"]
        factor = self.c.get("/api/v1/cars/c1/efficiency")["factor"]["rated_wh_per_km"]
        for it in cap["items"]:
            ch = charges[it["charge"]]
            self.assertEqual((it["method"], it["usable_basis"]), ("charge", "rated_range"))
            # the energy stored (rated range added at the car's factor) over the % added; the buffer once
            stored = (ch["end_range_km"] - ch["start_range_km"]) * factor / 1000
            usable = stored / (ch["end_soc_pct"] - ch["start_soc_pct"]) * 100
            self.assertAlmostEqual(it["usable_kwh"], usable, delta=0.4, msg=it["charge"])
            self.assertAlmostEqual(it["full_pack_kwh"], it["usable_kwh"] + buffer, delta=0.02, msg=it["charge"])
            self.assertAlmostEqual(it["stored_kwh"], stored, delta=0.05, msg=it["charge"])
            self.assertEqual(it["energy_added_kwh"], ch["energy_added_kwh"])
        self.assertLess(cap["trend"]["slope_kwh_per_10000_km"], 0)
        day = self.c.get("/api/v1/cars/c1/battery/capacity?group=day&limit=500")
        self.assertTrue(all(x["method"] == "bms" for x in day["items"]))
        month = self.c.get("/api/v1/cars/c1/battery/capacity?group=month&limit=500")["items"]
        self.assertGreater(month[0]["full_pack_kwh"], month[-1]["full_pack_kwh"])
        odo = self.c.get("/api/v1/cars/c3/battery/capacity?x=odometer&limit=500")["items"]
        kms = [x["odometer_km"] for x in odo]
        self.assertEqual(kms, sorted(kms))
        self.c.error("GET", "/api/v1/cars/c1/battery/capacity?group=week", 400, "bad_input")

    def test_charge_level(self):
        d = self.c.get(f"/api/v1/cars/c1/battery-level?from_ms={NOW - 3 * DAY}")
        self.assertEqual(d["bucket"], "hour")
        self.assertEqual(len(d["buckets"]), len(d["moving"]["mean"]))
        self.assertTrue(any(s["state"] == "asleep" for s in d["spans"]))
        vals = [b["soc_pct"] for b in d["buckets"] if b["soc_pct"] is not None]
        self.assertTrue(vals and d["stats"]["min"] <= d["stats"]["mean"] <= d["stats"]["max"])
        y = self.c.get(f"/api/v1/cars/c2/battery-level?from_ms={NOW - 200 * DAY}&window=30d")
        self.assertEqual(y["bucket"], "day")
        self.assertEqual(y["band"]["high_pct"], 100)
        self.assertGreater(y["share_above_80_pct"], 0)
        off = self.c.get(f"/api/v1/cars/c1/battery-level?from_ms={NOW - DAY}&moving=false")
        self.assertEqual(off["moving"]["mean"], [])

    def test_projected_range(self):
        d = self.c.get("/api/v1/cars/c1/projected-range?max_points=20")
        self.assertLessEqual(len(d["t_ms"]), 21)
        self.assertEqual(len(d["state"]), len(d["t_ms"]))
        full = [v for v in d["values"]["full_range_km"] if v is not None]
        self.assertGreater(full[0], full[-1])  # the pack ages
        self.assertTrue(all(v is not None for v in d["values"]["odometer_km"] if v))
        w = self.c.get(f"/api/v1/cars/c2/projected-range?from_ms={NOW - 30 * DAY}&bucket=day")
        self.assertEqual(w["bucket_ms"], DAY)

    def test_parked_drain(self):
        d = self.c.get("/api/v1/cars/c1/parked?min_hours=6&limit=500")
        items = self.walk("/api/v1/cars/c1/parked?min_hours=6", limit=500)
        self.assertEqual(len(items), d["summary"]["spells"])
        for x in items:
            self.assertGreaterEqual(x["duration_ms"], 6 * 3_600_000)
            parts = x["asleep_ms"] + x["awake_ms"] + x["awake_vehicle_bus_only_ms"] + x["unknown_ms"]
            self.assertAlmostEqual(parts, x["duration_ms"], delta=2000, msg=x["id"])
            self.assertEqual(x["energy_method"], "counter")
        self.assertAlmostEqual(d["summary"]["energy_kwh"], sum(x["energy_kwh"] for x in items), delta=0.01)
        self.assertEqual(d["summary"]["cost"]["currency"], "USD")
        # unknown time inside a spell is counted as unknown, never as sleep
        gaps = self.c.get(f"/api/v1/cars/c1/parked?from_ms={NOW - 152 * DAY}&to_ms={NOW - 148 * DAY}"
                          "&min_hours=1&limit=500")["items"]
        self.assertTrue(any(x["unknown_ms"] >= 100 * 60_000 for x in gaps))
        winter = self.c.get(f"/api/v1/cars/c2/parked?from_ms={NOW - 310 * DAY}&to_ms={NOW - 280 * DAY}"
                            "&min_hours=8&limit=500")["items"]
        self.assertTrue(any("keep_warm" in x["awake_reasons"] for x in winter))
        clipped = self.c.get(f"/api/v1/cars/c1/parked?from_ms={NOW - DAY}&limit=500")["items"]
        self.assertTrue(all(x["start_ms"] >= NOW - DAY for x in clipped))

    def test_efficiency_by_temperature(self):
        d = self.c.get("/api/v1/cars/c2/efficiency?temp_group_c=10")
        self.assertEqual(sum(g["drives"] for g in d["by_temperature"]) + d["no_temperature"]["drives"], d["drives"])
        cold = [g for g in d["by_temperature"] if g["temp_c"] < 0]
        mild = [g for g in d["by_temperature"] if 10 <= g["temp_c"] < 20]
        self.assertTrue(cold and mild)
        self.assertGreater(cold[0]["wh_per_km"], mild[0]["wh_per_km"])
        self.assertGreater(d["gross_wh_per_km"], d["net_wh_per_km"])
        self.assertEqual(d["factor"]["rated_wh_per_km"], 132.0)
        tm = self.c.get("/api/v1/cars/c3/efficiency")
        self.assertGreater(tm["estimated_drives"], 0)

    def test_temperatures(self):
        d = self.c.get(f"/api/v1/cars/c1/temperatures?from_ms={NOW - 14 * DAY}")
        o = d["outside"]
        self.assertLessEqual(o["min_c"], o["avg_c"])
        self.assertLessEqual(o["avg_c"], o["max_c"])
        self.assertEqual(len(d["daily"]), 15)
        self.assertGreater(d["missing_ms"]["asleep"], 0)
        legacy = self.c.get(f"/api/v1/cars/c3/temperatures?from_ms={NOW - 14 * DAY}")
        self.assertIsNone(legacy["cabin"]["avg_c"])

    def test_efficiency_groups_match_drives_at_fahrenheit_widths(self):
        # widths of 5, 10 and 20 °F are not whole tenths of a °C: the rounded edges must not decide the group
        for car in ("c1", "c2", "c3"):
            drives = self.walk(f"/api/v1/cars/{car}/drives?from_ms={NOW - 30 * DAY}&to_ms={NOW}&min_distance_km=1",
                               limit=200)
            for w in (2.778, 5.556, 11.111):
                d = self.c.get(f"/api/v1/cars/{car}/efficiency?from_ms={NOW - 30 * DAY}&to_ms={NOW}"
                               f"&min_distance_km=1&temp_group_c={w}")
                for g in d["by_temperature"]:
                    k = round(g["temp_c"] / w)
                    n = sum(1 for x in drives if x["outside_avg_c"] is not None
                            and (x["outside_avg_c"] // w) == k)
                    self.assertEqual(n, g["drives"], (car, w, g["temp_c"]))

    def test_gross_covers_every_drive(self):
        q = f"from_ms={NOW - 30 * DAY}&to_ms={NOW}"
        a = self.c.get(f"/api/v1/cars/c1/efficiency?{q}&min_distance_km=1")
        b = self.c.get(f"/api/v1/cars/c1/efficiency?{q}&min_distance_km=10")
        self.assertEqual(a["gross_wh_per_km"], b["gross_wh_per_km"])
        self.assertEqual(a["all_drives"], b["all_drives"])
        self.assertLess(b["drives"], b["all_drives"]["drives"])

    def test_compare_min_distance(self):
        q = f"cars=c1&metric=wh_per_km&from_ms={NOW - 30 * DAY}&to_ms={NOW}"
        a = self.c.get(f"/api/v1/compare?{q}")["series"][0]
        b = self.c.get(f"/api/v1/compare?{q}&min_distance_km=10")["series"][0]
        self.assertNotEqual(list(zip(a["t_ms"], a["values"])), list(zip(b["t_ms"], b["values"])))

    def test_temperature_extremes_hold_the_series(self):
        for car in ("c1", "c2"):
            for span in (DAY, 7 * DAY):
                q = f"from_ms={NOW - span}&to_ms={NOW}"
                t = self.c.get(f"/api/v1/cars/{car}/temperatures?{q}")
                s = self.c.get(f"/api/v1/cars/{car}/series?fields=outside_c,cabin_c&max_points=800&{q}")
                for f, k in (("outside_c", "outside"), ("cabin_c", "cabin")):
                    v = [x for x in s["values"][f] if x is not None]
                    if not v:
                        continue
                    self.assertLessEqual(t[k]["min_c"], min(v) + 0.05, (car, span, k))
                    self.assertGreaterEqual(t[k]["max_c"], max(v) - 0.05, (car, span, k))
                    self.assertLessEqual(min(x["min_c"] for x in t["daily"] if x["min_c"] is not None) if k == "outside"
                                         else t[k]["min_c"], min(v) + 0.05)
