"""The TeslaFi input (docs/ui-api.md, "Integrations: TeslaFi"): admin only, the password asked again, the made-up
account and car, adding by token, matching, pause, the time zone, the error codes, notices, audit entries, the
network row, and that no answer ever holds a token. Every token here is made up."""

from __future__ import annotations

import json

from .helpers import ServerCase

PW = "sentry-mate-dev"
BASE = "/api/v1/integrations/teslafi"
TOKEN = "0000madeup0000aaaabbbb"


class Teslafi(ServerCase):
    def no_token(self, data):
        raw = json.dumps(data)
        self.assertNotIn(TOKEN, raw)
        self.assertNotIn('"token"', raw)

    def audit(self):
        return self.c.get("/api/v1/audit?limit=500")["items"]

    def test_1_overview_account_and_car(self):
        o = self.c.get(BASE)
        self.assertTrue(o["enabled"])
        self.assertEqual((o["accounts"], o["cars"], o["hosts"], o["tls"]), (1, 1, ["www.teslafi.com"], "1.3"))
        self.assertEqual(o["polls"]["driving_s"], 60)
        self.assertNotIn("daily_cap", o["polls"])  # no cap of its own: TeslaFi does the limiting
        self.assertIsNone(o["rate_limited_until_ms"])
        a = self.c.get(BASE + "/accounts")["items"][0]
        self.assertEqual((a["id"], a["status"], a["time_zone"]), ("fa1", "ok", "America/Chicago"))
        self.assertEqual([(c["vehicle"], c["match"], c["vin_prefix"], c["model"]) for c in a["cars"]],
                         [("fv1", "c3", "5YJ3E1EA0PF", "Model 3")])
        st = self.c.get(BASE + "/status")["items"]
        self.assertEqual(len(st), 1)
        self.assertIn(st[0]["mode"], ("off", "asleep", "idle", "charging", "driving", "backoff"))
        self.c.error("GET", BASE + "/accounts/fa9", 404, "no_such_teslafi_account")
        self.assertEqual(self.c.get("/api/v1/system/health")["teslafi"]["accounts"], 1)
        net = [x for x in self.c.get("/api/v1/network")["outbound"] if x["purpose"] == "teslafi"]
        self.assertEqual([(x["host"], x["enabled"]) for x in net], [("www.teslafi.com", True)])

    def test_1b_pace(self):
        o = self.c.get(BASE)
        self.assertEqual((o["poll"], o["version"]), ("normal", 1))
        self.c.error("PATCH", BASE, 403, "password_needed", body={"version": 1, "poll": "often"})
        self.c.error("PATCH", BASE, 422, "invalid", body={"version": 1, "poll": "fast", "confirm_password": PW})
        o = self.c.json("PATCH", BASE, {"version": 1, "poll": "often", "confirm_password": PW})
        self.assertEqual((o["poll"], o["polls"]["driving_s"], o["polls"]["parked_s"], o["polls"]["asleep_s"]),
                         ("often", 15, 60, 180))
        self.c.error("PATCH", BASE, 409, "stale_version", body={"version": 1, "poll": "gentle", "confirm_password": PW})
        self.assertIn("teslafi-poll-changed", {x["action"] for x in self.audit()})
        self.c.json("PATCH", BASE, {"version": o["version"], "poll": "normal", "confirm_password": PW})

    def test_2_add_match_pause_remove(self):
        e, _ = self.c.error("POST", BASE + "/accounts", 403, "password_needed", body={"token": TOKEN})
        self.c.error("POST", BASE + "/accounts", 422, "invalid", body={"token": "has space", "confirm_password": PW})
        # the zones the real server takes (chrono-tz's), no others: Mars/Base is refused, EST5EDT taken
        for zone in ("not a zone", "Mars/Base"):
            self.c.error("POST", BASE + "/accounts", 422, "invalid",
                         body={"token": TOKEN, "time_zone": zone, "confirm_password": PW})
        self.c.error("POST", BASE + "/accounts", 422, "teslafi_refused",
                     body={"token": "refused-" + TOKEN, "confirm_password": PW})
        self.c.error("POST", BASE + "/accounts", 502, "teslafi_unreachable",
                     body={"token": "unreachable-" + TOKEN, "confirm_password": PW})
        self.c.error("POST", BASE + "/accounts", 409, "account_exists",
                     body={"token": "same-car-" + TOKEN, "confirm_password": PW})
        a = self.c.json("POST", BASE + "/accounts", {"label": "Second", "token": TOKEN, "confirm_password": PW},
                        expect=201)
        self.no_token(a)
        self.assertEqual((a["status"], a["time_zone"], a["cars"][0]["match"]), ("ok", None, None))
        aid = a["id"]
        # the car is taken by fa1: refused; a new car is fine
        self.c.error("PUT", f"{BASE}/accounts/{aid}/cars", 409, "car_taken",
                     body={"version": a["version"], "items": [{"vehicle": "fv1", "match": "c3"}]})
        # the admin's session alone: no password asked again
        cars = self.c.json("PUT", f"{BASE}/accounts/{aid}/cars",
                           {"version": a["version"], "items": [{"vehicle": "fv1", "match": "new"}]})
        self.assertEqual(cars["items"][0]["match"], "new")
        a = self.c.json("PATCH", f"{BASE}/accounts/{aid}",
                        {"version": cars["version"], "paused": True, "time_zone": "Europe/Oslo", "confirm_password": PW})
        self.assertEqual((a["status"], a["time_zone"], a["mode"]), ("paused", "Europe/Oslo", "off"))
        a = self.c.json("PATCH", f"{BASE}/accounts/{aid}",
                        {"version": a["version"], "time_zone": "EST5EDT", "confirm_password": PW})
        self.assertEqual(a["time_zone"], "EST5EDT")
        a = self.c.json("PATCH", f"{BASE}/accounts/{aid}",
                        {"version": a["version"], "token": TOKEN + "x", "confirm_password": PW})
        self.no_token(a)
        self.c.error("PATCH", f"{BASE}/accounts/{aid}", 409, "stale_version",
                     body={"version": a["version"] - 1, "label": "x", "confirm_password": PW})
        d = self.c.json("DELETE", f"{BASE}/accounts/{aid}", {"confirm_password": PW})
        self.assertEqual(d, {"deleted": True, "id": aid})
        actions = [e["action"] for e in self.audit() if e["action"].startswith("teslafi")]
        for want in ("teslafi-account-added", "teslafi-car-matched", "teslafi-account-paused",
                     "teslafi-account-changed", "teslafi-token-replaced", "teslafi-account-removed"):
            self.assertIn(want, actions)
        self.no_token(self.audit())

    def test_3_refused_token_notice(self):
        from mockapi.resources.teslafi import token_refused
        st = self.server.app.store
        token_refused(st, "fa1", self.server.app.clock.now())
        n = [x for x in self.c.get("/api/v1/notices")["items"] if x["code"] == "teslafi_token_refused"]
        self.assertEqual(len(n), 1)
        self.assertEqual(n[0]["fix_link"], "/settings/integrations#teslafi")
        a = self.c.get(BASE + "/accounts/fa1")
        self.assertEqual((a["status"], a["mode"]), ("token_refused", "off"))
        self.assertIn("teslafi-token-refused", [e["action"] for e in self.audit()])


    def test_1b_teslafi_limiting_is_told(self):
        """A 429 wait: the notice with the next try, the account and its status, the connections row, and no cap."""
        a = self.server.app.store.teslafi["accounts"]["fa1"]
        until = self.server.app.clock.now() + 10 * 60_000
        a["rate_limited_until_ms"] = until
        try:
            n = [x for x in self.c.get("/api/v1/notices")["items"] if x["code"] == "teslafi_rate_limited"]
            self.assertEqual(len(n), 1)
            self.assertTrue(n[0]["message"].startswith('TeslaFi is limiting requests for the TeslaFi account "TeslaFi", '
                                                       "next try at "), n[0]["message"])
            self.assertEqual(n[0]["detail"], {"account": "fa1", "until_ms": until})
            self.assertEqual(n[0]["fix_link"], "/settings/integrations#teslafi")
            self.assertEqual(self.c.get(BASE)["rate_limited_until_ms"], until)
            self.assertEqual(self.c.get(BASE + "/accounts/fa1")["rate_limited_until_ms"], until)
            st = self.c.get(BASE + "/status")["items"][0]
            self.assertEqual((st["mode"], st["next_request_ms"], st["rate_limited_until_ms"]), ("backoff", until, until))
            row = next(x for x in self.c.get("/api/v1/integrations/overview")["connections"] if x["id"] == "teslafi:fa1")
            self.assertEqual((row["status"], row["status_reason"], row["next_ms"]), ("problem", "rate_limited", until))
            self.assertIsNone(row["today"]["cap"])
        finally:
            a["rate_limited_until_ms"] = None
        self.assertFalse([x for x in self.c.get("/api/v1/notices")["items"] if x["code"] == "teslafi_rate_limited"])
        row = next(x for x in self.c.get("/api/v1/integrations/overview")["connections"] if x["id"] == "teslafi:fa1")
        self.assertIsNone(row["today"]["cap"], "no cap shown, whatever the pace")
        self.assertIn("teslafi-rate-limited", [e["action"] for e in self.audit()])

    def test_1c_teslafis_data_age_is_told(self):
        """TeslaFi's data is old while the connection works: no new data from an awake car (its side), or polls that
        come but are stamped hours off (the account's zone). The car's input line has both times and the notices,
        the status and the settings say whose problem it is (docs/ui-api.md, "TeslaFi's data age")."""
        a = self.server.app.store.teslafi["accounts"]["fa1"]
        now = self.server.app.clock.now()
        tf = lambda: next(i for i in self.c.get("/api/v1/cars/c3/now")["inputs"] if i["kind"] == "teslafi_api")
        codes = lambda: [x["code"] for x in self.c.get("/api/v1/notices")["items"] if x["code"].startswith("teslafi_")]
        st = lambda: self.c.get(BASE + "/status")["items"][0]
        # current: TeslaFi's own time of the poll, and when it came a moment later
        i = tf()
        self.assertEqual((i["last_ms"], i["received_ms"]), (now - 50_000, now - 45_000))
        self.assertEqual((codes(), st()["data"]), ([], "fine"))
        smo = next(i for i in self.c.get("/api/v1/cars/c1/now")["inputs"] if i["kind"] == "smo")
        self.assertIsNone(smo["received_ms"])
        try:
            # TeslaFi has had nothing new for six hours
            a["data_age_ms"] = 6 * 3_600_000
            i = tf()
            self.assertEqual(i["last_ms"], now - 6 * 3_600_000)
            self.assertGreater(i["received_ms"] - i["last_ms"], 0)
            if st()["mode"] != "asleep":  # a car TeslaFi says sleeps is silent as expected
                n = [x for x in self.c.get("/api/v1/notices")["items"] if x["code"] == "teslafi_no_data"]
                self.assertEqual(len(n), 1, codes())
                self.assertTrue(n[0]["message"].startswith("TeslaFi has had no new data from the car for 5 h 59 min"),
                                n[0]["message"])
                self.assertIn("the problem is on TeslaFi's side", n[0]["message"])
                self.assertEqual((n[0]["fix_link"], n[0]["detail"]["account"]), ("/settings/integrations#teslafi", "fa1"))
                self.assertEqual(st()["data"], "no_data")
            a.pop("data_age_ms")
            # polls come, stamped six hours before they are read: the zone is wrong
            a["time_offset_ms"] = 6 * 3_600_000
            i = tf()
            self.assertEqual(i["received_ms"] - i["last_ms"], 6 * 3_600_000 + 5_000)
            n = [x for x in self.c.get("/api/v1/notices")["items"] if x["code"] == "teslafi_time_offset"]
            self.assertEqual(len(n), 1, codes())
            self.assertIn("are 6 h before the moment this server reads them", n[0]["message"])
            self.assertEqual(n[0]["detail"], {"account": "fa1", "skew_ms": 6 * 3_600_000, "behind": True})
            self.assertEqual(st()["data"], "time_offset")
        finally:
            a.pop("data_age_ms", None)
            a.pop("time_offset_ms", None)
        self.assertEqual((codes(), st()["data"]), ([], "fine"))


class TeslafiWatch(ServerCase):
    """A watch account reads none of it, and hears nothing about TeslaFi accounts."""

    server_args = {"role": "watch"}

    def test_admin_only(self):
        for m, path in (("GET", BASE), ("GET", BASE + "/accounts"), ("GET", BASE + "/accounts/fa1"),
                        ("GET", BASE + "/accounts/fa1/cars"), ("PUT", BASE + "/accounts/fa1/cars"),
                        ("GET", BASE + "/status"),
                        ("POST", BASE + "/accounts"), ("DELETE", BASE + "/accounts/fa1")):
            self.c.error(m, path, 403, "not_allowed", body={} if m != "GET" else None)
        self.assertFalse([n for n in self.c.get("/api/v1/notices")["items"] if n["code"].startswith("teslafi")])
