"""Fields the real server sends that pages may come to show: two-step sign-in on /me and the accounts, a session's
address and end, the inputs of an awake span, a car token's car, the period of the drive figures and the logbook,
`truncated`, and a car's `created_ms` and the `model`, `year` and `plant` of its `config`."""

from __future__ import annotations

from .helpers import DAY, NOW, ServerCase


class RealFields(ServerCase):
    def test_me_and_accounts_say_two_step(self):
        me = self.c.get("/api/v1/me")
        self.assertIs(me["two_step"], False)
        self.assertIs(me["two_step_setup_only"], False)
        for a in self.c.get("/api/v1/accounts")["items"]:
            self.assertIsInstance(a["two_step"], bool, a)
            self.assertEqual(self.c.get(f"/api/v1/accounts/{a['id']}")["two_step"], a["two_step"])

    def test_a_session_has_its_address_and_its_end(self):
        rows = self.c.get("/api/v1/me/sessions")["items"]
        self.assertGreaterEqual(len(rows), 3)
        for s in rows:
            self.assertIsInstance(s["address"], str, s)
            # 30 days from sign-in at most, 7 days unused
            self.assertEqual(s["expires_ms"], min(s["created_ms"] + 30 * DAY, s["last_seen_ms"] + 7 * DAY), s)
        mine = next(s for s in rows if s["this_one"])
        self.assertEqual(mine["address"], "127.0.0.1")
        older = [s for s in rows if s["id"].startswith("s-older-")]
        self.assertEqual(len(older), 2)
        self.assertTrue(all(s["address"].startswith("192.0.2.") for s in older))

    def test_an_awake_span_names_its_inputs(self):
        rows = self.c.get("/api/v1/cars/c1/awake-reasons?limit=50")["items"]
        self.assertTrue(rows)
        for x in rows:
            self.assertIn(x["source"], ("can", "tesla_api", "teslafi_api", "teslamate", "teslafi", "teslamate_local"))
            self.assertTrue(x["sources"], x)
            self.assertIsInstance(x["source_label"], str)

    def test_a_car_token_names_its_car(self):
        rows = self.c.get("/api/v1/cars/c1/upload-tokens")["items"]
        self.assertTrue(rows)
        self.assertEqual({t["car"] for t in rows}, {"c1"})

    def test_the_drive_figures_the_logbook_and_the_trip(self):
        a, b = NOW - 30 * DAY, NOW - DAY
        st = self.c.get(f"/api/v1/cars/c1/drive-stats?from_ms={a}&to_ms={b}")
        self.assertEqual((st["from_ms"], st["to_ms"]), (a, b))
        self.assertEqual(self.c.get(f"/api/v1/cars/c1/drive-stats?from_ms={a}")["to_ms"], NOW)  # never after now
        lb = self.c.get(f"/api/v1/cars/c1/logbook?from_ms={a}&to_ms={b}")
        self.assertIn("rows", lb)  # the whole form
        self.assertEqual((lb["from_ms"], lb["to_ms"], lb["truncated"]), (a, b, False))
        self.assertIs(self.c.get(f"/api/v1/cars/c1/trip?from_ms={a}&to_ms={b}")["truncated"], False)

    def test_a_car_has_created_ms_and_its_model_year_and_plant_in_config(self):
        car = self.c.get("/api/v1/cars/c1")
        self.assertIsInstance(car["created_ms"], int)
        self.assertLess(car["created_ms"], NOW)
        v = car["vehicle"]
        self.assertEqual(car["config"]["model"]["value"], v["model"]["value"])
        self.assertEqual(car["config"]["year"]["value"], v["model_year"]["value"])
        self.assertEqual(car["config"]["plant"], {"value": car["vin_prefix"][10], "source": "vin",
                                                  "sources": car["config"]["plant"]["sources"]})
        for k in ("model", "year", "plant"):
            self.assertEqual(set(car["config"][k]), {"value", "source", "sources"})
        # a typed model is the config's too
        self.c.json("PATCH", "/api/v1/cars/c1", {"version": car["version"], "config": {"model": "Model Q"}})
        self.assertEqual(self.c.get("/api/v1/cars/c1")["config"]["model"],
                         {"value": "Model Q", "source": "owner", "sources": []})


class CarsNamedInTheQuery(ServerCase):
    """`cars=` or `car=` on any route, as the real server's guard reads them: each car named must be one the account
    sees (else 404 no_such_car), and the cars of both are the ones a report covers."""

    def test_car_filters_like_cars(self):
        for path in ("/api/v1/charges",):
            every = {x["car"] for x in self.walk(f"{path}?from_ms={NOW - 30 * DAY}", limit=200)}
            self.assertGreater(len(every), 1, path)
            self.assertEqual({x["car"] for x in self.walk(f"{path}?car=c1&from_ms={NOW - 30 * DAY}", limit=200)}, {"c1"})
            both = {x["car"] for x in self.walk(f"{path}?cars=c1&car=c2&from_ms={NOW - 30 * DAY}", limit=200)}
            self.assertEqual(both, {"c1", "c2"})
            self.assertEqual({x["car"] for x in self.walk(f"{path}?cars=all&car=c2&from_ms={NOW - 30 * DAY}", limit=200)},
                             {"c2"})

    def test_an_unknown_car_in_the_query_is_404_on_any_route(self):
        for path in ("/api/v1/charges?car=zz", "/api/v1/charges?cars=c1,zz", "/api/v1/statistics?car=c999",
                     "/api/v1/me?car=zz", "/api/v1/cars/c1/drives?cars=zz", "/api/v1/places?car=zz"):
            self.c.error("GET", path, 404, "no_such_car")
        self.assertEqual(self.c.get("/api/v1/me?cars=all&car=")["id"], "a1")
