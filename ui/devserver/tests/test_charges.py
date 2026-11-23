"""Charges: lists, detail, curve, costs and stats."""

from __future__ import annotations

from .helpers import NOW, ServerCase

CHARGE = {"id", "car", "start_ms", "end_ms", "charging_ms", "plugged_ms", "kind", "fast_charger", "place", "address",
          "position", "odometer_km", "energy_added_kwh", "energy_grid_kwh", "dc_inlet_kwh", "losses_kwh",
          "efficiency_pct", "efficiency_note", "cost", "price_per_kwh", "start_soc_pct", "end_soc_pct", "start_range_km", "end_range_km",
          "range_added_km", "range_per_hour_km", "power_max_kw", "power_avg_kw", "outside_avg_c", "battery_avg_c",
          "source", "has_gaps"}


class Charges(ServerCase):
    def test_all_cars(self):
        d = self.c.get("/api/v1/charges?limit=500")
        cars = {x["car"] for x in d["items"]}
        self.assertEqual(cars, {"c1", "c2", "c3", "c0"})  # the sold car's history counts in "all cars"
        for x in d["items"]:
            self.assertTrue(CHARGE <= set(x), CHARGE - set(x))
        t = d["totals"]
        self.assertEqual(set(t), {"count", "energy_added_kwh", "energy_grid_kwh", "energy_grid_known", "dc_inlet_kwh",
                                  "cost_by_currency", "unpriced", "avg_duration_ms"})
        self.assertGreater(t["count"], 500)
        currencies = {c["currency"] for c in t["cost_by_currency"]}
        self.assertEqual(currencies, {"USD", "CAD"})
        self.assertGreater(t["unpriced"], 0)
        two = self.c.get("/api/v1/charges?cars=c1,c2&limit=500")
        self.assertEqual({x["car"] for x in two["items"]}, {"c1", "c2"})

    def test_kinds_and_costs(self):
        d = self.c.get("/api/v1/cars/c1/charges?limit=500")
        kinds = {x["kind"] for x in d["items"]}
        self.assertEqual(kinds, {"ac", "dc"})
        dc = next(x for x in d["items"] if x["kind"] == "dc")
        self.assertIsNone(dc["energy_grid_kwh"])
        self.assertGreater(dc["dc_inlet_kwh"], dc["energy_added_kwh"])
        self.assertEqual(dc["fast_charger"]["type"], "supercharger")
        ac = next(x for x in d["items"] if x["kind"] == "ac" and x["cost"])
        self.assertGreater(ac["energy_grid_kwh"], ac["energy_added_kwh"])
        self.assertAlmostEqual(ac["losses_kwh"], ac["energy_grid_kwh"] - ac["energy_added_kwh"], delta=0.01)
        free = self.c.get("/api/v1/cars/c3/charges?kind=dc&limit=500")["items"]
        self.assertTrue(any(x["cost"] and x["cost"]["source"] == "free_supercharging" for x in free))
        dcs = self.c.get("/api/v1/cars/c1/charges?kind=dc&limit=500")["items"]
        self.assertTrue(all(x["kind"] == "dc" for x in dcs))
        home = self.c.get("/api/v1/cars/c1/charges?place_kind=home&limit=500")["items"]
        self.assertTrue(home and all(x["place"]["kind"] == "home" for x in home))
        priced = self.c.get("/api/v1/cars/c1/charges?min_cost=5&limit=500")["items"]
        self.assertTrue(all(x["cost"]["amount"] >= 5 for x in priced))

    def test_phases(self):
        c1 = self.c.get("/api/v1/cars/c1/charges?place_kind=home&limit=1")["items"][0]
        c2 = self.c.get("/api/v1/cars/c2/charges?place_kind=home&limit=1")["items"][0]
        p1 = self.c.get(f"/api/v1/cars/c1/charges/{c1['id']}/curve?fields=phases&max_points=50")
        p2 = self.c.get(f"/api/v1/cars/c2/charges/{c2['id']}/curve?fields=phases&max_points=50")
        self.assertEqual({v for v in p1["values"]["phases"] if v}, {1})
        self.assertEqual({v for v in p2["values"]["phases"] if v}, {3})

    def test_detail_curve_and_manual_cost(self):
        x = self.c.get("/api/v1/cars/c1/charges?kind=dc&limit=3")["items"][1]
        d = self.c.get(f"/api/v1/cars/c1/charges/{x['id']}")
        self.assertEqual(set(d["cost_breakdown"]), {"parts", "session_fee", "total", "gap_ms", "mode"})
        self.assertTrue(d["prev"])
        cv = self.c.get(f"/api/v1/cars/c1/charges/{x['id']}/curve")
        self.assertEqual(cv["step_ms"], 1000)  # 1 Hz
        self.assertEqual(len(cv["t_ms"]), len(cv["values"]["power_kw"]))
        self.assertAlmostEqual(len(cv["t_ms"]), x["charging_ms"] / 1000, delta=3)
        self.assertGreater(max(v for v in cv["values"]["power_kw"] if v), 30)
        up = self.c.json("PUT", f"/api/v1/cars/c1/charges/{x['id']}/cost",
                         {"mode": "total", "amount": 12.5, "currency": "USD"})
        self.assertEqual(up["cost"], {"amount": 12.5, "currency": "USD", "source": "manual"})
        self.c.error("PUT", f"/api/v1/cars/c1/charges/{x['id']}/cost", 422, "invalid",
                     body={"mode": "total", "amount": 5, "currency": "XYZ"})
        self.assertEqual([i["applied"] for i in up["invoices"]], [False] * len(up["invoices"]))  # an entered cost wins
        back = self.c.json("DELETE", f"/api/v1/cars/c1/charges/{x['id']}/cost")
        # back to the Tesla invoice matched to the charge, else the place price
        self.assertEqual(back["cost"]["source"], "tesla_invoice" if d["invoices"] else "tariff")

    def test_stopped_mid_charge(self):
        g = self.c.get("/api/v1/cars/c1/charges?flags=gap&limit=500")["items"]
        self.assertTrue(g)
        cut = [x for x in g if x["flags"]["gap_ms"] > 3_600_000]
        self.assertTrue(cut)

    def test_stats(self):
        s = self.c.get("/api/v1/charging-stats")
        for k in ("count", "energy_added_kwh", "by_kind", "cost_by_currency", "price_per_kwh", "supercharger_cost",
                  "cost_per_100km", "start_bands", "end_bands", "heatmap", "runs", "share_above_80_pct",
                  "share_below_20_pct"):
            self.assertIn(k, s)
        self.assertEqual(len(s["heatmap"]["start"]), 7)
        self.assertEqual(len(s["heatmap"]["start"][0]), 24)
        self.assertEqual(sum(b["count"] for b in s["start_bands"]), s["count"])
        self.assertTrue(s["runs"])
        one = self.c.get("/api/v1/cars/c2/charging-stats")
        self.assertEqual(one["cars"], ["c2"])
        pl = self.c.get("/api/v1/charging-stats/places?cars=c1&limit=5")
        self.assertEqual(pl["items"][0]["label"], "Home")
        for x in pl["items"]:
            self.assertTrue({"place", "label", "lat", "lon", "sessions", "energy_added_kwh", "cost_by_currency",
                             "price_per_kwh"} <= set(x))
        dc = self.c.get("/api/v1/cars/c1/charging-stats/dc-curve")
        self.assertTrue(dc["sessions"])
        socs = [p["soc_pct"] for p in dc["median"]]
        self.assertEqual(socs, sorted(socs))

    def test_in_progress(self):
        live = self.c.get("/api/v1/charges?flags=in_progress")["items"]
        self.assertEqual([x["car"] for x in live], ["c2"])
        self.assertIsNone(live[0]["end_ms"])
        self.assertLessEqual(live[0]["start_ms"], NOW)
