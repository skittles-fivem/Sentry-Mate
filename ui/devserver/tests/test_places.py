"""Places: the list, checks, the match rule, and what a change does to history: names change
everywhere at once (drives, charges, timeline, visits), costs stay until repriced."""

from __future__ import annotations

from .helpers import DAY, NOW, Running, ServerCase

LA = {"lat": 34.0522, "lon": -118.2437}  # a made-up stop in downtown Los Angeles on c1's road trip


class PlacesRead(ServerCase):
    def test_list_and_one(self):
        rows = self.walk("/api/v1/places", limit=100)
        names = [x["name"].lower() for x in rows]
        self.assertEqual(names, sorted(names))
        home = self.c.get("/api/v1/places/p1")
        self.assertGreater(home["counts"]["drives"], 100)
        self.assertGreater(home["counts"]["charges"], 10)
        self.assertEqual(home["price"]["currency"], "USD")
        self.assertEqual(home["version"], 1)
        chargers = self.c.get("/api/v1/places?kind=charger&limit=100")["items"]
        self.assertTrue(chargers and all(x["kind"] == "charger" for x in chargers))
        self.assertEqual([x["id"] for x in self.c.get("/api/v1/places?search=logan")["items"]], ["p30"])
        self.c.error("GET", "/api/v1/places/p999", 404, "no_such_place")
        self.c.error("GET", "/api/v1/places?kind=castle", 400, "bad_input")

    def test_counts_for_one_car(self):
        rows = self.walk("/api/v1/places?car=c1&sort=drives", limit=100)
        fleet = {x["id"]: x for x in self.walk("/api/v1/places", limit=100)}
        self.assertEqual(len(rows), len(fleet))  # every place, this car's counts
        drives = [x["counts"]["drives"] for x in rows]
        self.assertEqual(drives, sorted(drives, reverse=True))
        self.assertEqual(rows[0]["id"], "p1")
        self.assertTrue(all(x["counts"]["drives"] <= fleet[x["id"]]["counts"]["drives"] for x in rows))
        self.assertTrue(any(x["counts"]["drives"] < fleet[x["id"]]["counts"]["drives"] for x in rows))
        self.c.error("GET", "/api/v1/places?car=c999", 404, "no_such_car")

    def test_counts_for_a_car_without_history(self):
        # a car with a summary only (c4 and up in a larger fleet) answers every place with no drive, never a 500
        big = Running(cars=6)
        try:
            c = big.client()
            for car in ("c4", "c5"):
                self.assertEqual(c.get(f"/api/v1/cars/{car}")["id"], car)
                rows = c.get(f"/api/v1/places?car={car}&sort=drives&limit=100")["items"]
                self.assertTrue(rows)
                self.assertTrue(all(x["counts"] == {"drives": 0, "charges": 0, "visits": 0} for x in rows))
        finally:
            big.close()

    def test_nearest_centre_wins(self):
        # Home (60 m) sits inside Noe Valley (400 m): at Home's centre, Home wins and both are candidates
        d = self.c.get("/api/v1/places/at?lat=37.7508&lon=-122.433")
        self.assertEqual(d["place"]["id"], "p1")
        self.assertEqual([x["id"] for x in d["candidates"]], ["p1", "p24"])
        self.assertEqual(d["rule"], "nearest_centre")
        out = self.c.get("/api/v1/places/at?lat=37.7525&lon=-122.4300")
        self.assertEqual(out["place"]["id"], "p24")
        none = self.c.get("/api/v1/places/at?lat=10.0&lon=10.0")
        self.assertIsNone(none["place"])
        self.c.error("GET", "/api/v1/places/at?lat=91&lon=0", 400, "bad_input")

    def test_suggestions(self):
        s = self.c.get("/api/v1/places/suggestions?limit=5")["items"]
        self.assertTrue(s)
        self.assertTrue(any(x["address"]["locality"] == "Toronto" for x in s))
        for x in s:
            at = self.c.get(f"/api/v1/places/at?lat={x['lat']}&lon={x['lon']}")
            self.assertIsNone(at["place"])  # suggestions are where no saved place is

    def test_checks(self):
        e, _ = self.c.error("POST", "/api/v1/places", 400, "bad_input", body={"name": 5, "lat": "north"})
        self.assertIn("name", e["fields"])
        self.assertIn("lat", e["fields"])
        self.assertIn("lon", e["fields"])
        e, _ = self.c.error("POST", "/api/v1/places", 422, "invalid",
                            body={"name": "x", "lat": 1, "lon": 1, "radius_m": 5, "time_zone": "Mars/Base",
                                  "price": {"currency": "XXX", "per_kwh": 0.3, "session_fee": -1}})
        for f in ("radius_m", "time_zone", "price.currency", "price.session_fee"):
            self.assertIn(f, e["fields"])
        e, _ = self.c.error("POST", "/api/v1/places", 422, "invalid",
                            body={"name": "x", "lat": 1, "lon": 1, "price": {"currency": "USD", "rates": [
                                {"name": "Night", "per_kwh": 0.1, "days": "someday", "from": "25:00", "to": "06:00"}]}})
        self.assertIn("price.rates.0.days", e["fields"])
        self.assertIn("price.rates.0.from", e["fields"])
        self.c.error("POST", "/api/v1/places/preview", 422, "invalid", body={"place": "p999", "delete": True})


class PlacesWrite(ServerCase):
    def test_1_new_place_names_history(self):
        before = self.walk("/api/v1/cars/c1/drives?q=near", limit=500)
        prev = self.c.json("POST", "/api/v1/places/preview", dict(LA, name="Grand Park", radius_m=200))
        self.assertGreater(prev["drives_named"], 0)
        d = self.c.json("POST", "/api/v1/places", dict(LA, name="Grand Park", kind="other", radius_m=200), expect=201)
        self.assertEqual(d["effect"]["drives_named"], prev["drives_named"])
        self.assertEqual(d["time_zone"], "America/Los_Angeles")
        pid = d["id"]
        named = self.walk(f"/api/v1/cars/c1/drives?places={pid}", limit=500)
        self.assertEqual(len(named), prev["drives_named"])
        self.assertLess(len(self.walk("/api/v1/cars/c1/drives?q=near", limit=500)), len(before) + 1)
        self.assertTrue(all(pid in (x["start"]["place"] or {}).get("id", "") + (x["end"]["place"] or {}).get("id", "")
                            for x in named))
        v = self.c.get(f"/api/v1/visits?place={pid}")["items"]
        self.assertTrue(v and v[0]["label"] == "Grand Park")
        self.assertEqual(self.c.get(f"/api/v1/places/{pid}")["counts"]["visits"], len(v))
        self.assertEqual(self.c.get("/api/v1/places/at?" + f"lat={LA['lat']}&lon={LA['lon']}")["place"]["id"], pid)
        audit = self.c.get("/api/v1/audit?limit=5")["items"]
        self.assertEqual(audit[0]["action"], "place-created")

    def test_2_rename_shows_everywhere(self):
        p = self.c.get("/api/v1/places/p31")
        self.c.error("PATCH", "/api/v1/places/p31", 409, "stale_version", body={"version": 99, "name": "x"})
        d = self.c.json("PATCH", "/api/v1/places/p31", {"version": p["version"], "name": "Garage on LaSalle"})
        self.assertEqual(d["version"], p["version"] + 1)
        drive = self.c.get("/api/v1/cars/c2/drives?places=p31&place_side=end&limit=1")["items"][0]
        self.assertEqual(drive["end"]["place"]["name"], "Garage on LaSalle")
        self.assertEqual(drive["end"]["label"], "Garage on LaSalle")
        detail = self.c.get(f"/api/v1/cars/c2/drives/{drive['id']}")
        self.assertEqual(detail["end"]["label"], "Garage on LaSalle")
        ch = self.c.get("/api/v1/cars/c2/charges?places=p31&limit=1")["items"][0]
        self.assertEqual(ch["place"]["name"], "Garage on LaSalle")
        tl = self.c.get(f"/api/v1/cars/c2/timeline?kinds=drive&to_ms={drive['end_ms'] + 1}&limit=1")["items"][0]
        self.assertEqual(tl["end"]["label"], "Garage on LaSalle")
        visit = self.c.get("/api/v1/visits?cars=c2&place=p31&limit=1")["items"][0]
        self.assertEqual(visit["label"], "Garage on LaSalle")
        now = self.c.get("/api/v1/cars/c2/now")
        self.assertEqual(now["location"]["place"]["name"], "Garage on LaSalle")
        city = self.c.get("/api/v1/visits/places?cars=c2&limit=100")["items"]
        self.assertIn("Garage on LaSalle", {x["label"] for x in city})
        ev = self.c.get("/api/v1/cars/c2/events?types=charge_started&limit=1")["items"][0]
        self.assertEqual(ev["payload"]["place"], "Garage on LaSalle")

    def test_3_price_change_keeps_costs_until_repriced(self):
        a, b = NOW - 60 * DAY, NOW - 2 * DAY
        q = f"/api/v1/cars/c2/charges?places=p30&from_ms={a}&to_ms={b}&limit=500"
        old = {x["id"]: x["cost"]["amount"] for x in self.c.get(q)["items"]}
        self.assertTrue(old)
        stats0 = self.walk(f"/api/v1/cars/c2/statistics?period=month&from_ms={a}&to_ms={b}", limit=50)
        p = self.c.get("/api/v1/places/p30")
        prev = self.c.json("POST", "/api/v1/places/preview",
                           {"id": "p30", "price": {"currency": "USD", "per_kwh": 0.25}})
        self.assertGreater(prev["charges_repriced"], 0)
        self.c.json("PATCH", "/api/v1/places/p30", {"version": p["version"],
                                                    "price": {"currency": "USD", "per_kwh": 0.25}})
        same = {x["id"]: x["cost"]["amount"] for x in self.c.get(q)["items"]}
        self.assertEqual(same, old)  # a charge keeps the cost it had
        dry = self.c.json("POST", "/api/v1/places/p30/reprice", {"from_ms": a, "to_ms": b, "dry_run": True})
        self.assertEqual(dry["repriced"], len(old))
        self.assertEqual({x["id"]: x["cost"]["amount"] for x in self.c.get(q)["items"]}, old)
        done = self.c.json("POST", "/api/v1/places/p30/reprice", {"from_ms": a, "to_ms": b})
        self.assertEqual(done["repriced"], len(old))
        new = {x["id"]: x["cost"]["amount"] for x in self.c.get(q)["items"]}
        for cid, amount in new.items():
            self.assertGreater(amount, old[cid])
        stats1 = self.walk(f"/api/v1/cars/c2/statistics?period=month&from_ms={a}&to_ms={b}", limit=50)
        tot0 = sum(c["amount"] for x in stats0 for c in x["cost_by_currency"])
        tot1 = sum(c["amount"] for x in stats1 for c in x["cost_by_currency"])
        self.assertAlmostEqual(tot1 - tot0, sum(new.values()) - sum(old.values()), delta=0.1)
        only = self.c.json("POST", "/api/v1/places/p30/reprice", {"only_without_cost": True, "dry_run": True})
        self.assertEqual(only["charges"], 0)

    def test_4_delete_unnames_history(self):
        prev = self.c.json("POST", "/api/v1/places/preview", {"place": "p4", "delete": True})
        self.assertGreater(prev["drives_unnamed"], 0)
        cost_before = self.c.get("/api/v1/cars/c1/charges?limit=1")["totals"]["cost_by_currency"]
        d = self.c.json("DELETE", "/api/v1/places/p4")
        self.assertEqual((d["deleted"], d["id"]), (True, "p4"))
        self.assertEqual(d["effect"]["drives_unnamed"], prev["drives_unnamed"])
        self.c.error("GET", "/api/v1/places/p4", 404, "no_such_place")
        self.c.error("GET", "/api/v1/cars/c1/drives?places=p4", 400, "bad_input")
        tl = self.walk("/api/v1/cars/c1/timeline?q=alabama&kinds=drive", limit=200)
        self.assertTrue(tl)
        self.assertTrue(all(x["end"] is None or x["end"]["place"] is None or x["end"]["place"]["id"] != "p4"
                            for x in tl))
        self.assertEqual(self.c.get("/api/v1/cars/c1/charges?limit=1")["totals"]["cost_by_currency"], cost_before)
        s = self.c.get("/api/v1/places/suggestions?limit=50")["items"]
        self.assertTrue(any((x["address"].get("street") or "") == "Alabama St" for x in s))

    def test_5_watch_may_not_change(self):
        w = self.server.client()
        st, _, _ = w.request("POST", "/api/v1/session", {"name": "watch", "password": "sentry-mate-dev"})
        self.assertEqual(st, 200)
        w.error("POST", "/api/v1/places", 403, "not_allowed", body=dict(LA, name="x"))
        w.error("PATCH", "/api/v1/places/p1", 403, "not_allowed", body={"version": 1, "name": "x"})
        w.error("DELETE", "/api/v1/places/p1", 403, "not_allowed")
        w.error("POST", "/api/v1/places/p1/reprice", 403, "not_allowed", body={})
        # a watch account sees only the places its car (c1) used
        ids = {x["id"] for x in w.json("GET", "/api/v1/places?limit=100")["items"]}
        self.assertIn("p1", ids)
        self.assertNotIn("p30", ids)
        w.error("GET", "/api/v1/places/p30", 404, "no_such_place")

    def test_6_preview_matches_what_create_does(self):
        # A new place at Costco's centre (p5) ties with Costco everywhere: the older place wins a tie,
        # so the preview must promise no relabel, and create must then count the same.
        costco = self.c.get("/api/v1/places/p5")
        at = {"lat": costco["lat"], "lon": costco["lon"]}
        prev = self.c.json("POST", "/api/v1/places/preview", dict(at, name="Tie", radius_m=costco["radius_m"]))
        self.assertEqual((prev["drives_named"], prev["charges_named"], prev["drives_taken"]), (0, 0, 0))
        d = self.c.json("POST", "/api/v1/places", dict(at, name="Tie", kind="other", radius_m=costco["radius_m"]),
                        expect=201)
        self.assertEqual(d["effect"]["drives_named"], prev["drives_named"])
        self.assertEqual(self.c.get(f"/api/v1/places/{d['id']}")["counts"]["drives"], 0)
        self.assertEqual(self.c.get("/api/v1/places/p5")["counts"]["drives"], costco["counts"]["drives"])
        self.c.json("DELETE", f"/api/v1/places/{d['id']}")
        # An older place moved onto Costco's centre wins the tie and takes Costco's stops: the preview says
        # how many it takes from Costco, and the change does exactly that.
        p2 = self.c.get("/api/v1/places/p2")
        prev = self.c.json("POST", "/api/v1/places/preview", dict(at, id="p2"))
        self.assertGreater(prev["drives_taken"], 0)
        self.assertEqual(prev["taken_from"][0]["place"], {"id": "p5", "name": costco["name"]})
        self.assertEqual(prev["taken_from"][0]["drives"], prev["drives_taken"])
        d = self.c.json("PATCH", "/api/v1/places/p2", dict(at, version=p2["version"]))
        self.assertEqual(d["effect"]["drives_taken"], prev["drives_taken"])
        self.assertEqual(self.c.get("/api/v1/places/p5")["counts"]["drives"],
                         costco["counts"]["drives"] - prev["drives_taken"])
        self.c.json("PATCH", "/api/v1/places/p2", {"lat": p2["lat"], "lon": p2["lon"], "version": d["version"]})
        self.assertEqual(self.c.get("/api/v1/places/p5")["counts"]["drives"], costco["counts"]["drives"])
