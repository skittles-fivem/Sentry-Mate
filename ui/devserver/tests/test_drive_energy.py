"""A drive's energy figures and its 1 Hz power readings agree: the readings add up to the drive's net energy
and regen, so a selection of the whole drive on the Drive page shows the drive's own figures."""

from __future__ import annotations

from .helpers import NOW, ServerCase

MONTH = 30 * 86_400_000


class DriveEnergyMatchesSeries(ServerCase):
    def test_power_adds_up(self):
        items = self.c.get(f"/api/v1/cars/c1/drives?from_ms={NOW - MONTH}&limit=12")["items"]
        checked = 0
        for x in items:
            if x["end_ms"] is None or x["source"] != "can" or x["energy_kwh"] is None or x["energy_kwh"] < 0.3:
                continue
            s = self.c.get(f"/api/v1/cars/c1/series?fields=power_kw&from_ms={x['start_ms']}"
                           f"&to_ms={x['end_ms'] + 1}&max_points=20000")
            self.assertEqual(s["bucket_ms"], 1000)
            ps = [p for p in s["values"]["power_kw"] if p is not None]
            net = sum(ps) / 3600
            regen = -sum(p for p in ps if p < 0) / 3600
            self.assertAlmostEqual(net / x["energy_kwh"], 1, delta=0.08, msg=x["id"])
            if x["regen_kwh"] and x["regen_kwh"] > 0.03:
                self.assertAlmostEqual(regen / x["regen_kwh"], 1, delta=0.12, msg=x["id"])
            self.assertLessEqual(max(ps), x["power_max_kw"] + 0.5)
            self.assertGreaterEqual(min(ps), x["power_min_kw"] - 0.5)
            checked += 1
        self.assertGreater(checked, 3)
