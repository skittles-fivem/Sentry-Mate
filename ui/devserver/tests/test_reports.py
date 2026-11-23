"""Reports: statistics add up to the Drives and Charges lists, mileage, totals, gaps, compare,
visits and their groups, tracks, software updates, fleet versions and car events."""

from __future__ import annotations

from .helpers import DAY, NOW, ServerCase


def money(rows):
    out = {}
    for x in rows:
        for c in x:
            out[c["currency"]] = out.get(c["currency"], 0.0) + c["amount"]
    return out


class Statistics(ServerCase):
    def test_months_add_up_to_the_lists(self):
        a, b = NOW - 120 * DAY, NOW - 20 * DAY
        for car in ("c1", "c2", "c3"):
            rows = self.walk(f"/api/v1/cars/{car}/statistics?period=month&from_ms={a}&to_ms={b}", limit=100)
            charges = self.c.get(f"/api/v1/cars/{car}/charges?from_ms={a}&to_ms={b}&limit=1")["totals"]
            drives = self.c.get(f"/api/v1/cars/{car}/drives?from_ms={a}&to_ms={b}&limit=1")["summary"]
            self.assertEqual(sum(x["charges"] for x in rows), charges["count"], car)
            self.assertAlmostEqual(sum(x["energy_added_kwh"] for x in rows), charges["energy_added_kwh"], delta=0.05)
            cost = money(x["cost_by_currency"] for x in rows)
            for c in charges["cost_by_currency"]:
                self.assertAlmostEqual(cost[c["currency"]], c["amount"], delta=0.05, msg=(car, c))
            self.assertEqual(sum(x["drives"] for x in rows), drives["count"], car)
            self.assertAlmostEqual(sum(x["distance_logged_km"] for x in rows), drives["distance_km"], delta=0.05)
            for x in rows:
                self.assertEqual(x["car"], car)
                self.assertGreaterEqual(x["period_start_ms"], a)
                self.assertLessEqual(x["period_end_ms"], b)
        c2 = self.c.get(f"/api/v1/cars/c2/statistics?period=all&from_ms={a}&to_ms={b}")["items"]
        self.assertEqual(len(c2), 1)
        self.assertEqual(c2[0]["label"], "All")

    def test_periods_in_the_car_time_zone(self):
        day = self.c.get("/api/v1/cars/c2/statistics?period=day&limit=3")["items"]
        import datetime as dt
        import zoneinfo
        tz = zoneinfo.ZoneInfo("America/Chicago")
        start = dt.datetime.fromtimestamp(day[1]["period_start_ms"] / 1000, tz)
        self.assertEqual((start.hour, start.minute), (0, 0))
        self.assertEqual(day[1]["label"], start.date().isoformat())
        weeks = self.c.get("/api/v1/cars/c1/statistics?period=week&limit=3")["items"]
        self.assertRegex(weeks[0]["label"], r"^\d{4}-W\d{2}$")
        wk = dt.datetime.fromtimestamp(weeks[1]["period_start_ms"] / 1000, zoneinfo.ZoneInfo("America/Los_Angeles"))
        self.assertEqual(wk.weekday(), 0)
        years = self.c.get("/api/v1/statistics?period=year&cars=c1,c2")
        self.assertEqual({x["car"] for x in years["items"]}, {"c1", "c2"})
        self.c.error("GET", "/api/v1/statistics?cars=c1,c99", 404, "no_such_car")
        self.c.error("GET", "/api/v1/statistics?period=decade", 400, "bad_input")
        compl = self.c.get("/api/v1/cars/c3/statistics?period=month&limit=1")["items"][0]["completeness"]
        self.assertLess(compl["recorded_pct"], 100)  # the car went quiet

    def test_mileage_totals_gaps(self):
        m = self.c.get("/api/v1/cars/c1/mileage?group=month&limit=100")
        for x in m["items"]:
            self.assertAlmostEqual(x["distance_km"], x["odometer_end_km"] - x["odometer_start_km"], delta=0.2)
            self.assertAlmostEqual(x["logged_km"] + x["not_logged_km"], x["distance_km"], delta=0.05)
        self.assertGreater(m["projection"]["km_per_year"], 5000)
        t = self.c.get("/api/v1/cars/c1/totals")
        all_drives = self.c.get("/api/v1/cars/c1/drives?limit=1")["summary"]
        self.assertEqual(t["drives"], all_drives["count"])
        self.assertEqual(t["ac"]["charges"] + t["dc"]["charges"], t["charges"])
        self.assertGreater(t["not_logged_km"], 0)  # gaps the car drove through
        tm = self.c.get("/api/v1/cars/c3/totals?source=teslamate")
        can = self.c.get("/api/v1/cars/c3/totals?source=can")
        self.assertGreater(tm["drives"], 0)
        self.assertGreater(can["drives"], 0)
        g = self.walk("/api/v1/cars/c1/gaps", limit=100)
        self.assertTrue({"no_log", "not_recorded", "chain_gap", "unknown"} <= {x["cause"] for x in g})
        moved = [x for x in g if x["odometer_jump_km"] > 1]
        self.assertTrue(moved)
        open_gap = self.c.get("/api/v1/cars/c3/gaps?limit=1")["items"][0]
        self.assertIsNone(open_gap["end_ms"])

    def test_compare(self):
        for metric in ("capacity", "full_range", "wh_per_km", "odometer"):
            d = self.c.get(f"/api/v1/compare?cars=c1,c2&metric={metric}&from_ms={NOW - 90 * DAY}")
            self.assertEqual([s["car"] for s in d["series"]], ["c1", "c2"])
            for s in d["series"]:
                self.assertEqual(len(s["t_ms"]), len(s["values"]))
                self.assertTrue(s["values"])
        odo = self.c.get("/api/v1/compare?cars=c1&metric=odometer")["series"][0]["values"]
        self.assertEqual(odo, sorted(odo))


class Travel(ServerCase):
    def test_visits(self):
        v = self.c.get("/api/v1/visits?cars=c2&limit=5")["items"]
        self.assertIsNone(v[0]["left_ms"])  # charging at work now
        self.assertTrue(v[0]["charges"])
        for a, b in zip(v[1:], v[2:]):
            self.assertGreaterEqual(a["arrived_ms"], b["arrived_ms"])
        ca = self.walk("/api/v1/visits?country=CA", limit=100)
        self.assertTrue(ca and all(x["address"]["country_code"] == "CA" for x in ca))
        q = self.c.get("/api/v1/visits?q=costco&limit=10")["items"]
        self.assertTrue(q and all(x["place"]["name"] == "Costco" for x in q))
        one = self.c.get("/api/v1/visits?place=p1&limit=10")["items"]
        self.assertTrue(all(x["place"]["id"] == "p1" for x in one))

    def test_summary_and_groups(self):
        s = self.c.get("/api/v1/visits/summary")
        self.assertEqual(s["countries"], 2)
        self.assertGreater(s["cities"], 10)
        self.assertGreater(s["unnamed_stops"], 0)
        countries = self.c.get("/api/v1/visits/countries")["items"]
        self.assertEqual(sum(x["visits"] for x in countries), s["visits"])
        cities = self.walk("/api/v1/visits/cities", limit=100)
        self.assertEqual(len(cities), s["cities"])
        toronto = next(x for x in cities if x["label"] == "Toronto")
        days = self.walk(f"/api/v1/visits/days?city={toronto['key']}", limit=100)
        self.assertEqual(len(days), toronto["days"])
        self.assertTrue(any(d["charges"] for d in days))
        reg = self.c.get("/api/v1/visits/regions?country=US")["items"]
        self.assertTrue(all(x["country_code"] == "US" for x in reg))
        pl = self.c.get("/api/v1/visits/places?sort=dwell&limit=3")["items"]
        self.assertEqual(pl[0]["place"]["name"], "Home")

    def test_tracks(self):
        d = self.c.get(f"/api/v1/tracks?cars=c1&from_ms={NOW - 2 * DAY}&max_points=400")
        self.assertTrue(d["drives"])
        self.assertLessEqual(d["points"], 450)
        for x in d["drives"]:
            self.assertEqual(len(x["t_ms"]), len(x["lat"]))
            self.assertEqual(len(x["lon"]), len(x["accuracy_m"]))
        self.c.error("GET", "/api/v1/tracks?max_points=4", 400, "bad_input")


class Software(ServerCase):
    def test_updates_and_versions(self):
        u = self.c.get("/api/v1/cars/c2/updates")
        self.assertEqual(u["summary"]["count"], 2)
        newest = u["items"][0]
        # named from the owner's local TeslaMate (the first new version it reported after the update)
        self.assertEqual((newest["version"], newest["version_source"], newest["named_by"]),
                         ("2026.44.1", "teslamate_local", "auto"))
        self.assertEqual(newest["detected_by"], "fingerprint")
        self.assertEqual((u["summary"]["current_version"], u["summary"]["current_version_source"]),
                         ("2026.44.1", "teslamate_local"))
        self.assertIsNone(u["summary"]["build_changed_since"])
        self.c.error("PATCH", f"/api/v1/cars/c2/updates/{newest['id']}", 422, "invalid", body={"version": "soon"})
        d = self.c.json("PATCH", f"/api/v1/cars/c2/updates/{newest['id']}", {"version": "2026.38.4"})
        self.assertEqual((d["version"], d["version_source"], d["named_by"]), ("2026.38.4", "owner", "owner"))
        u = self.c.get("/api/v1/cars/c2/updates")
        self.assertEqual((u["summary"]["current_version"], u["summary"]["current_version_source"]),
                         ("2026.38.4", "owner"))
        self.assertEqual((u["summary"]["last_update"]["version"], u["summary"]["last_update"]["version_source"]),
                         ("2026.38.4", "owner"))
        now = self.c.get("/api/v1/cars/c2/now")
        self.assertEqual(now["software"]["version"], "2026.38.4")
        fv = self.c.get("/api/v1/fleet/versions")["items"]
        self.assertEqual(next(x for x in fv if x["car"] == "c2")["version"], "2026.38.4")
        self.assertTrue(all(x["time_zone"] for x in fv))
        self.assertEqual(self.c.get("/api/v1/fleet/versions?search=2025.45")["items"][0]["car"], "c3")
        tm = self.c.get("/api/v1/cars/c3/updates")["items"]
        self.assertTrue(any(x["version_source"] == "teslamate" for x in tm))
        # a cleared name stays cleared: the input does not name it again
        d = self.c.json("PATCH", f"/api/v1/cars/c2/updates/{newest['id']}", {"version": None})
        self.assertEqual((d["version"], d["version_source"], d["named_by"]), (None, None, None))
        u = self.c.get("/api/v1/cars/c2/updates")
        self.assertIsNone(u["summary"]["current_version"])  # local TeslaMate is not read for Maple yet
        self.assertIsNotNone(u["summary"]["build_changed_since"])

    def test_firmware_and_fingerprint_updates(self):
        fw = self.c.get("/api/v1/cars/c2/firmware")
        self.assertEqual([m["id"] for m in fw["modules"]], ["vcfront", "bms", "dir", "dif"])
        self.assertEqual(fw["modules"][1]["can_id"], 0x300)
        self.assertEqual(fw["modules"][3]["fields"], [], "c2 is rear drive only")
        self.assertEqual(fw["updates"], 2)
        bms = next(f for f in fw["modules"][1]["fields"] if f["field"] == "BMS_appCrc")
        self.assertRegex(bms["hex"], r"^[0-9a-f]{8}$")
        self.assertEqual(len(bms["history"]), 3, "two updates changed it")
        self.assertEqual(bms["history"][-1]["hex"], bms["hex"])
        h = next(f for f in fw["modules"][2]["fields"] if f["field"] == "DIR_subcomponentGitHash")
        self.assertRegex(h["hex"], r"^[0-9a-f]{14}$")
        self.assertIsNotNone(fw["updated_ms"])
        ups = sorted(self.c.get("/api/v1/cars/c2/updates")["items"], key=lambda u: u["start_ms"])
        named, unnamed = ups
        for u in ups:
            self.assertEqual(u["detected_by"], "fingerprint")
            self.assertEqual(u["between_ms"], [u["start_ms"], u["end_ms"]])
            self.assertIn("bms", u["changed_modules"])
            self.assertEqual(sorted({c["module"] for c in u["changes"]}), sorted(u["changed_modules"]))
            # the change's new value is the one seen from the end of the update on
            c = next(c for c in u["changes"] if c["field"] == "BMS_appCrc")
            run = next(x for x in bms["history"] if x["first_seen_ms"] == u["end_ms"])
            self.assertEqual(run["hex"], c["new"])
            self.assertNotEqual(c["old"], c["new"])
        # local TeslaMate also reported 2025.44.25 after the first one, but the typed name wins
        self.assertEqual((named["version"], named["version_source"], named["named_by"]), ("2025.44.25", "owner", "owner"))
        self.assertEqual((unnamed["version"], unnamed["version_source"], unnamed["named_by"]),
                         ("2026.44.1", "teslamate_local", "auto"))
        hist = self.walk("/api/v1/cars/c2/firmware/history", limit=500)
        marked = [x for x in hist if x["update"] and x["update"]["id"] == unnamed["id"]]
        self.assertTrue(marked)
        self.assertTrue(all(x["update"]["version_source"] == "teslamate_local" for x in marked))
        tl = self.c.get(f"/api/v1/cars/c2/timeline?kinds=update&from_ms={named['start_ms']}&limit=10")["items"]
        self.assertTrue(tl and all(r["detected_by"] == "fingerprint" and r["changed_modules"] for r in tl))
        self.assertEqual(self.c.get("/api/v1/cars/c1/firmware")["modules"][3]["fields"][0]["field"],
                         "DIF_applicationCrc")

    def test_fingerprint_update_never_in_progress(self):
        # like the real server: between_ms and end_ms are set even while "now" is inside the update's window
        from types import SimpleNamespace
        from mockapi.resources.software import update_rows
        app = self.server.app
        h = app.ds.history("c2")
        u = next(u for u in h.updates if u.source == "can" and u.detected_by == "fingerprint")
        self.assertLess(u.start + 1, u.end)
        rows = update_rows(SimpleNamespace(app=app, now=u.start + 1), h)
        row = next(x for x in rows if x["id"] == u.id)
        self.assertEqual(row["between_ms"], [u.start, u.end])
        self.assertEqual((row["end_ms"], row["duration_ms"]), (u.end, u.end - u.start))
        self.c.error("GET", "/api/v1/cars/c999/firmware", 404, "no_such_car")

    def test_events(self):
        d = self.c.get("/api/v1/cars/c1/events?limit=20")
        types = {x["type"] for x in d["items"]}
        self.assertIn("drive_started", types)
        ended = next(x for x in d["items"] if x["type"] == "drive_ended")["payload"]
        for k in ("event_type", "car", "drive", "start", "end", "distance", "energy_used_kwh", "units"):
            self.assertIn(k, ended)
        self.assertNotIn("start_latitude", ended)  # positions only when the owner turns location publishing on
        self.assertTrue(all(x["topic"] == "sentrymate/cars/c1/event" for x in d["items"]))
        sw = self.c.get("/api/v1/cars/c1/events?types=software_changed")["items"]
        self.assertEqual(len(sw), 3)  # one per software update of c1
        ch = self.c.get("/api/v1/cars/c2/events?types=charge_started&limit=1")["items"][0]
        self.assertEqual(ch["payload"]["place"], "Work garage")
        self.c.error("GET", "/api/v1/cars/c1/events?types=wake_up", 400, "bad_input")


class SoftwareFromInputs(ServerCase):
    """The current version and a fingerprint update's name from a live input (TeslaFi, local TeslaMate), typed
    names winning, the car settings' typed version last, and `summary.last_update` whatever the period."""

    def test_current_version_from_teslafi(self):
        u = self.c.get("/api/v1/cars/c3/updates")
        s = u["summary"]
        self.assertEqual((s["current_version"], s["current_version_source"]), ("2025.45.8", "teslafi_api"))
        newest = u["items"][0]
        # TeslaFi first said 2025.45.8 eight days after the update: too late to name it, the build match stays
        self.assertEqual((newest["version"], newest["version_source"], newest["named_by"]),
                         ("2025.45.8", "build_match", "auto"))
        self.assertEqual(s["current_version_since_ms"], newest["end_ms"])
        sw = self.c.get("/api/v1/cars/c3/now")["software"]
        self.assertEqual((sw["version"], sw["version_source"]), ("2025.45.8", "teslafi_api"))
        fv = {x["car"]: x for x in self.c.get("/api/v1/fleet/versions")["items"]}
        self.assertEqual((fv["c3"]["version"], fv["c3"]["version_source"]), ("2025.45.8", "teslafi_api"))
        self.assertEqual(self.c.get("/api/v1/cars/c3/now")["off_can"]["version"]["sources"][0]["kind"], "teslafi_api")
        # Comet has no input version: as before
        c1 = self.c.get("/api/v1/cars/c1/updates")["summary"]
        self.assertEqual((c1["current_version"], c1["current_version_source"]), ("2026.32.5", "build_match"))

    def test_last_update_with_a_narrow_period(self):
        every = self.c.get("/api/v1/cars/c3/updates?from_ms=0")
        u = self.c.get(f"/api/v1/cars/c3/updates?from_ms={NOW - DAY}&to_ms={NOW}")
        self.assertEqual((u["items"], u["summary"]["count"]), ([], 0))
        top = every["items"][0]
        self.assertEqual(u["summary"]["last_update"], {k: top[k] for k in
                                                       ("id", "start_ms", "end_ms", "version", "version_source")})
        self.assertEqual(u["summary"]["last_update"]["version"], "2025.45.8")
        self.assertEqual(u["summary"]["current_version"], "2025.45.8")  # whatever the period

    def test_maple_from_local_teslamate(self):
        newest = self.c.get("/api/v1/cars/c2/updates")["items"][0]
        self.assertEqual((newest["version_source"], newest["named_by"]), ("teslamate_local", "auto"))
        self.assertNotIn("teslamate_local", [i["kind"] for i in self.c.get("/api/v1/cars/c2/now")["inputs"]])
        # matched in Settings: Integrations: local TeslaMate is read for Maple from now on
        base = "/api/v1/integrations/teslamate"
        cars = self.c.get(base + "/cars")
        self.c.json("PUT", base + "/cars", {"version": cars["version"], "items": [
            {"teslamate_car": "1", "match": "c2"}]})
        n = self.c.get("/api/v1/cars/c2/now")
        self.assertIn("teslamate_local", [i["kind"] for i in n["inputs"]])
        self.assertEqual((n["software"]["version"], n["software"]["version_source"]), ("2026.44.1", "teslamate_local"))
        self.assertEqual(n["off_can"]["version"]["value"], "2026.44.1")
        # typed on the newest update wins over the live input
        self.c.json("PATCH", f"/api/v1/cars/c2/updates/{newest['id']}", {"version": "2026.44.2"})
        s = self.c.get("/api/v1/cars/c2/updates")["summary"]
        self.assertEqual((s["current_version"], s["current_version_source"]), ("2026.44.2", "owner"))
        # cleared: the update stays without a name, the live input's version is the current one
        self.c.json("PATCH", f"/api/v1/cars/c2/updates/{newest['id']}", {"version": None})
        u = self.c.get("/api/v1/cars/c2/updates")
        self.assertIsNone(u["items"][0]["version"])
        self.assertEqual((u["summary"]["current_version"], u["summary"]["current_version_source"]),
                         ("2026.44.1", "teslamate_local"))
        self.assertEqual(u["summary"]["last_update"]["version"], None)
        # not read any more, nothing names the newest update: the car settings' typed version
        cars = self.c.get(base + "/cars")
        self.c.json("PUT", base + "/cars", {"version": cars["version"], "items": [
            {"teslamate_car": "1", "match": None}]})
        s = self.c.get("/api/v1/cars/c2/updates")["summary"]
        self.assertEqual((s["current_version"], s["current_version_source"]), (None, None))
        st = self.c.get("/api/v1/cars/c2/settings")
        self.c.json("PATCH", "/api/v1/cars/c2/settings", {"version": st["version"], "software_version_typed": "2026.44.1"})
        s = self.c.get("/api/v1/cars/c2/updates")["summary"]
        self.assertEqual((s["current_version"], s["current_version_source"]), ("2026.44.1", "owner"))


class WatchReports(ServerCase):
    server_args = {"role": "watch"}

    def test_watch_sees_its_car_only(self):
        self.assertEqual(self.c.get("/api/v1/statistics?period=year")["cars"], ["c1"])
        self.c.error("GET", "/api/v1/statistics?cars=c2", 404, "no_such_car")
        self.assertEqual({x["car"] for x in self.c.get("/api/v1/visits?limit=100")["items"]}, {"c1"})
        self.assertEqual([x["car"] for x in self.c.get("/api/v1/fleet/versions")["items"]], ["c1"])
        up = self.c.get("/api/v1/cars/c1/updates")["items"][0]
        self.c.error("PATCH", f"/api/v1/cars/c1/updates/{up['id']}", 403, "not_allowed", body={"version": "2026.1.1"})
        self.c.error("GET", "/api/v1/cars/c2/battery", 404, "no_such_car")
