"""The Tesla API input (docs/ui-api.md, "Integrations: Tesla"): admin only, the password and code asked again,
the sign-in at Tesla and the pasted address, accounts, cars, pause and resume, the poll setting, the error codes,
notices, audit entries, and that no answer ever holds a token. Every token here is made up."""

from __future__ import annotations

import json
from urllib.parse import parse_qs, urlsplit

from .helpers import ServerCase

PW = "sentry-mate-dev"
BASE = "/api/v1/integrations/tesla"
TOKEN = "made-up-refresh-token-0001"


def state_of(url):
    return parse_qs(urlsplit(url).query)["state"][0]


class Tesla(ServerCase):
    def no_token(self, data):
        raw = json.dumps(data)
        self.assertNotIn(TOKEN, raw)
        self.assertNotIn("made-up-code", raw)
        self.assertNotIn("refresh_token", raw)
        self.assertNotIn("access_token", raw)

    def audit(self):
        return self.c.get("/api/v1/audit?limit=500")["items"]

    def test_1_overview_and_accounts(self):
        o = self.c.get(BASE)
        self.assertTrue(o["enabled"])
        self.assertEqual((o["accounts"], o["cars"]), (1, 2))
        self.assertEqual(o["poll"], {"driving_ms": 2500, "charging_ms": 5000, "idle_ms": 60_000, "list_ms": 30_000})
        self.assertEqual(o["poll_limits"]["driving_ms"], {"min": 2500, "default": 2500, "max": 600_000})
        self.assertNotIn("daily_cap", o["poll_limits"])  # no cap of its own: Tesla does the limiting
        self.assertIsNone(o["rate_limited_until_ms"])
        self.assertEqual(o["hosts"], ["auth.tesla.com", "owner-api.teslamotors.com", "streaming.vn.teslamotors.com"])
        a = self.c.get(BASE + "/accounts")["items"][0]
        self.assertEqual((a["id"], a["status"], a["region"]), ("ta1", "ok", "global"))
        self.assertEqual([c["match"] for c in a["cars"]], ["c1", "c2", None])
        self.assertEqual(a["cars"][2]["mode"], "off")  # not matched: not read
        st = self.c.get(BASE + "/status")["items"]
        self.assertEqual({s["vehicle"] for s in st}, {"tv1", "tv2", "tv3"})
        for s in st:
            self.assertIn(s["mode"], ("off", "watching_list", "online_idle", "letting_sleep", "charging", "driving",
                                      "backoff"))
        self.c.error("GET", BASE + "/accounts/ta9", 404, "no_such_tesla_account")
        # the rest of the API knows about it
        self.assertEqual(self.c.get("/api/v1/setup/status")["tesla_accounts"], 1)
        self.assertEqual(self.c.get("/api/v1/system/health")["tesla"]["accounts"], 1)
        net = [x for x in self.c.get("/api/v1/network")["outbound"] if x["purpose"] == "tesla"]
        self.assertEqual({x["host"] for x in net}, set(o["hosts"]))
        self.assertTrue(all(x["enabled"] for x in net))

    def test_2_password_asked_again(self):
        v = self.c.get(BASE)["version"]
        slower = {"idle_ms": 120_000}
        e, _ = self.c.error("PATCH", BASE, 403, "password_needed", body={"version": v, "poll": slower})
        self.assertIn("confirm_password", e["fields"])
        self.c.error("PATCH", BASE, 403, "wrong_password",
                     body={"version": v, "poll": slower, "confirm_password": "not-it"})
        self.assertEqual(self.c.get(BASE)["poll"]["idle_ms"], 60_000)  # nothing changed
        # the header does as well
        o = self.c.json("PATCH", BASE, {"version": v, "poll": slower}, headers={"X-Sentry-Mate-Password": PW})
        self.assertEqual((o["poll"]["idle_ms"], o["poll"]["driving_ms"], o["version"]), (120_000, 2500, v + 1))
        self.c.error("PATCH", BASE, 409, "stale_version", body={"version": v, "poll": {"idle_ms": 90_000},
                                                                 "confirm_password": PW})
        self.assertIn("tesla-poll-changed", {x["action"] for x in self.audit()})
        entry = next(x for x in self.audit() if x["action"] == "tesla-poll-changed")
        self.assertIn("idle every 120 s", entry["summary"])

    def test_2a_the_floors(self):
        """Each read can be made slower, never faster than its floor; there is no cap to set."""
        v = self.c.get(BASE)["version"]
        for field, value in (("driving_ms", 2499), ("charging_ms", 4999), ("idle_ms", 59_999),
                             ("list_ms", 29_999), ("list_ms", 3_600_001)):
            e, _ = self.c.error("PATCH", BASE, 422, "invalid",
                                body={"version": v, "poll": {field: value}, "confirm_password": PW})
            self.assertIn(f"poll.{field}", e["fields"])
        for bad in ("often", {"driving_ms": "2.5"}, {"fast": 1}, {"driving_ms": 2500.5}, {"driving_ms": True},
                    {"daily_cap": 3000}):
            self.c.error("PATCH", BASE, 400, "bad_input", body={"version": v, "poll": bad, "confirm_password": PW})
        self.assertEqual(self.c.get(BASE)["version"], v)  # nothing changed
        slower = {"driving_ms": 5000, "charging_ms": 10_000, "idle_ms": 120_000, "list_ms": 60_000}
        o = self.c.json("PATCH", BASE, {"version": v, "poll": slower, "confirm_password": PW})
        self.assertEqual(o["poll"], slower)
        o = self.c.json("PATCH", BASE, {"version": o["version"], "poll": dict(slower, driving_ms=2500),
                                        "confirm_password": PW})
        self.assertEqual(o["poll"]["driving_ms"], 2500)

    def test_2c_tesla_limiting_is_told(self):
        """A 429 wait: the notice with the next try, the account and its status, the connections row."""
        a = self.server.app.store.tesla["accounts"]["ta1"]
        until = self.server.app.clock.now() + 10 * 60_000
        a["rate_limited_until_ms"] = until
        try:
            n = [x for x in self.c.get("/api/v1/notices")["items"] if x["code"] == "tesla_rate_limited"]
            self.assertEqual(len(n), 1)
            self.assertTrue(n[0]["message"].startswith('Tesla is limiting requests for the Tesla account "Family account", '
                                                       "next try at "), n[0]["message"])
            self.assertEqual(n[0]["detail"], {"account": "ta1", "until_ms": until})
            self.assertEqual(self.c.get(BASE)["rate_limited_until_ms"], until)
            self.assertEqual(self.c.get(BASE + "/accounts/ta1")["rate_limited_until_ms"], until)
            st = [x for x in self.c.get(BASE + "/status")["items"] if x["car"]]
            self.assertTrue(all(x["mode"] == "backoff" and x["next_poll_ms"] == until for x in st), st)
            row = next(x for x in self.c.get("/api/v1/integrations/overview")["connections"] if x["id"] == "tesla:ta1")
            self.assertEqual((row["status"], row["status_reason"], row["next_ms"]), ("problem", "rate_limited", until))
            self.assertIsNone(row["today"]["cap"])
        finally:
            a["rate_limited_until_ms"] = None
        self.assertFalse([x for x in self.c.get("/api/v1/notices")["items"] if x["code"] == "tesla_rate_limited"])

    def test_2b_a_password_that_is_not_ascii(self):
        """Any password is an account's password: it signs in, confirms a change in the body, and in the headers
        the real server takes (UTF-8, or percent-encoded for what a browser's fetch cannot put in a header)."""
        store = self.server.app.store
        admin = store.account_by_name("admin")
        odd = " pässwort-鍵-zwölf "
        admin["password"] = odd
        try:
            c = self.server.client()
            c.cookie = None
            st, _, _ = c.request("POST", "/api/v1/session", {"name": "admin", "password": odd})
            self.assertEqual(st, 200)
            v = self.c.get(BASE)["version"]
            self.c.error("PATCH", BASE, 403, "wrong_password",
                         body={"version": v, "poll": {"driving_ms": 2500}, "confirm_password": odd.strip()})
            o = self.c.json("PATCH", BASE, {"version": v, "poll": {"driving_ms": 2500}, "confirm_password": odd})
            enc = "%20p%C3%A4sswort-%E9%8D%B5-zw%C3%B6lf%20"
            o = self.c.json("PATCH", BASE, {"version": o["version"], "poll": {"driving_ms": 2500}},
                            headers={"X-Sentry-Mate-Password-Encoded": enc})
            e, _ = self.c.error("PATCH", BASE, 400, "bad_input", body={"version": o["version"], "poll": {"driving_ms": 2500}},
                                headers={"X-Sentry-Mate-Password-Encoded": "p%E4sswort"})
            self.assertIn("confirm_password", e["fields"])
            # the plain header: UTF-8 bytes (HTTP drops the spaces at its ends); Latin-1 bytes are not a password
            admin["password"] = odd.strip()
            self.c.json("PATCH", BASE, {"version": o["version"], "poll": {"driving_ms": 2500}},
                        headers={"X-Sentry-Mate-Password": odd.strip().encode("utf-8").decode("latin-1")})
            self.c.error("PATCH", BASE, 400, "bad_input", body={"version": o["version"] + 1, "poll": {"driving_ms": 2500}},
                         headers={"X-Sentry-Mate-Password": "p\xe4sswort"})
        finally:
            admin["password"] = PW
            store.throttle.clear()

    def test_3_two_step_code(self):
        alex = self.server.client()
        d = alex.json("POST", "/api/v1/session", {"name": "alex", "password": PW})
        alex.json("POST", "/api/v1/session/two-step", {"pending": d["pending"], "code": "000000"})
        v = alex.get(BASE)["version"]
        same = {"driving_ms": 2500}
        alex.error("PATCH", BASE, 401, "wrong_code", body={"version": v, "poll": same, "confirm_password": PW})
        alex.error("PATCH", BASE, 401, "wrong_code",
                   body={"version": v, "poll": same, "confirm_password": PW, "code": "123456"})
        o = alex.json("PATCH", BASE, {"version": v, "poll": same, "confirm_password": PW, "code": "000000"})
        self.assertEqual(o["poll"]["driving_ms"], 2500)

    def test_4_sign_in_and_paste(self):
        s = self.c.json("POST", BASE + "/sign-in", {"region": "global"}, expect=201)
        u = urlsplit(s["url"])
        self.assertEqual((u.scheme, u.netloc, u.path), ("https", "auth.tesla.com", "/oauth2/v3/authorize"))
        q = parse_qs(u.query)
        self.assertEqual(q["client_id"], ["ownerapi"])
        self.assertEqual(q["code_challenge_method"], ["S256"])
        self.assertEqual(q["redirect_uri"], ["tesla://auth/callback"])
        st = state_of(s["url"])
        path = f"{BASE}/sign-in/{s['sign_in']}"
        body = {"label": "Second account", "confirm_password": PW}
        # not Tesla's address, another sign-in's state, no code: refused, and the sign-in stays open
        for bad in ("https://evil.example/void/callback?code=x&state=" + st,
                    "tesla://auth/callback?code=made-up-code&state=other",
                    "tesla://auth/callback?state=" + st, "just text"):
            e, _ = self.c.error("POST", path, 422, "invalid", body=dict(body, pasted_url=bad))
            self.assertIn("pasted_url", e["fields"])
        self.c.error("POST", path, 403, "password_needed",
                     body={"pasted_url": f"tesla://auth/callback?code=made-up-code&state={st}"})
        a = self.c.json("POST", path, dict(body, pasted_url=f"tesla://auth/callback?code=made-up-code&state={st}"),
                        expect=201)
        self.no_token(a)
        self.assertEqual((a["label"], a["status"]), ("Second account", "ok"))
        self.assertIsNone(a["cars"][0]["match"])  # not read until matched
        # used once
        self.c.error("POST", path, 404, "no_such_sign_in",
                     body=dict(body, pasted_url=f"tesla://auth/callback?code=made-up-code&state={st}"))
        # runs out after ten minutes
        s2 = self.c.json("POST", BASE + "/sign-in", {"redirect": "void"}, expect=201)
        self.assertIn("redirect_uri=https%3A%2F%2Fauth.tesla.com%2Fvoid%2Fcallback", s2["url"])
        self.server.app.clock.advance(11 * 60_000)
        self.c.error("POST", f"{BASE}/sign-in/{s2['sign_in']}", 404, "no_such_sign_in",
                     body=dict(body, pasted_url=f"https://auth.tesla.com/void/callback?code=x&state={state_of(s2['url'])}"))
        # at most five open
        for _ in range(5):
            self.c.json("POST", BASE + "/sign-in", {}, expect=201)
        self.c.error("POST", BASE + "/sign-in", 409, "too_many_sign_ins", body={})
        self.server.app.clock.advance(11 * 60_000)
        self.c.json("POST", BASE + "/sign-in", {}, expect=201)
        self.c.error("POST", BASE + "/sign-in", 422, "invalid", body={"region": "mars"})
        self.c.error("POST", BASE + "/sign-in", 400, "bad_input", body={"confirm": True})
        self.assertIn("tesla-account-added", {x["action"] for x in self.audit()})

    def test_5_refresh_token_and_errors(self):
        body = {"label": "Token account", "confirm_password": PW}
        self.c.error("POST", BASE + "/accounts", 400, "bad_input", body=body)
        self.c.error("POST", BASE + "/accounts", 422, "tesla_refused", body=dict(body, refresh_token="refused-1"))
        self.c.error("POST", BASE + "/accounts", 502, "tesla_unreachable",
                     body=dict(body, refresh_token="unreachable-1"))
        self.c.error("POST", BASE + "/accounts", 409, "account_exists", body=dict(body, refresh_token="same-cars-1"))
        self.c.error("POST", BASE + "/accounts", 422, "invalid", body=dict(body, refresh_token="has spaces"))
        self.c.error("POST", BASE + "/accounts", 422, "invalid", body=dict(body, refresh_token=TOKEN, label=""))
        a = self.c.json("POST", BASE + "/accounts", dict(body, refresh_token=TOKEN, region="china"), expect=201)
        self.no_token(a)
        self.assertEqual(a["hosts"], ["auth.tesla.cn", "owner-api.vn.cloud.tesla.cn"])
        self.assertIn("auth.tesla.cn", self.c.get(BASE)["hosts"])
        for x in self.audit():
            self.assertNotIn(TOKEN, json.dumps(x))

    def test_6_cars_pause_and_remove(self):
        a = self.c.get(BASE + "/accounts/ta1")
        path = BASE + "/accounts/ta1/cars"
        self.assertEqual(self.c.get(path)["items"][2]["vehicle"], "tv3")
        e, _ = self.c.error("PUT", path, 422, "invalid", body={
            "version": a["version"],
            "items": [{"vehicle": "tv9", "match": "c3"}, {"vehicle": "tv3", "match": "c999"}]})
        self.assertEqual(set(e["fields"]), {"items.0.vehicle", "items.1.match"})
        self.c.error("PUT", path, 400, "bad_input", body={"version": a["version"],
                                                          "items": [{"vehicle": "tv3", "match": 7}]})
        # the admin's session alone: no password asked again (one sent is an unknown field)
        self.c.error("PUT", path, 400, "bad_input", body={"version": a["version"], "confirm_password": PW,
                                                          "items": [{"vehicle": "tv3", "match": "c3"}]})
        d = self.c.json("PUT", path, {"version": a["version"], "items": [{"vehicle": "tv3", "match": "c3"}]})
        self.assertEqual(d["items"][2]["match"], "c3")
        self.assertNotEqual(d["items"][2]["mode"], "off")  # read from now on
        # a car matched on one account can't be matched on another
        other = self.c.json("POST", BASE + "/accounts", {"refresh_token": "made-up-2", "confirm_password": PW},
                            expect=201)
        self.c.error("PUT", f"{BASE}/accounts/{other['id']}/cars", 409, "car_taken",
                     body={"version": other["version"],
                           "items": [{"vehicle": other["cars"][0]["vehicle"], "match": "c1"}]})
        # car off again
        d = self.c.json("PUT", path, {"version": d["version"], "items": [{"vehicle": "tv3", "match": "leave_out"}]})
        self.assertEqual(d["items"][2]["mode"], "off")
        # pause and resume
        a = self.c.json("PATCH", BASE + "/accounts/ta1", {"version": d["version"], "paused": True,
                                                          "confirm_password": PW})
        self.assertEqual(a["status"], "paused")
        self.assertTrue(all(c["mode"] == "off" for c in a["cars"]))
        # as on the server: the account's task keeps going (idle) while it is paused
        self.assertTrue(a["running"])
        self.assertTrue(self.c.get(BASE + "/accounts/ta1")["running"])
        a = self.c.json("PATCH", BASE + "/accounts/ta1", {"version": a["version"], "paused": False,
                                                          "location": "never", "confirm_password": PW})
        self.assertEqual((a["status"], a["location"]), ("ok", "never"))
        self.c.error("PATCH", BASE + "/accounts/ta1", 422, "invalid",
                     body={"version": a["version"], "location": "sometimes", "confirm_password": PW})
        self.c.error("PATCH", BASE + "/accounts/ta1", 400, "bad_input",
                     body={"version": a["version"], "access_token": "x", "confirm_password": PW})
        actions = [x["action"] for x in self.audit()]
        for want in ("tesla-car-matched", "tesla-account-paused", "tesla-account-resumed", "tesla-account-changed"):
            self.assertIn(want, actions)
        # removed: gone, the history stays
        self.c.error("DELETE", f"{BASE}/accounts/{other['id']}", 403, "password_needed")
        r = self.c.json("DELETE", f"{BASE}/accounts/{other['id']}", {"confirm_password": PW})
        self.assertEqual(r, {"deleted": True, "id": other["id"]})
        self.c.error("GET", f"{BASE}/accounts/{other['id']}", 404, "no_such_tesla_account")
        self.assertIn("tesla-account-removed", [x["action"] for x in self.audit()])

    def test_7_sign_in_needed_notice(self):
        from mockapi.resources.tesla import sign_in_needed
        self.assertNotIn("tesla_sign_in_needed", {n["code"] for n in self.c.get("/api/v1/notices")["items"]})
        sign_in_needed(self.server.app.store, "ta1", self.server.app.clock.now())
        n = [n for n in self.c.get("/api/v1/notices")["items"] if n["code"] == "tesla_sign_in_needed"]
        self.assertEqual(len(n), 1)
        self.assertEqual(n[0]["detail"], {"account": "ta1"})
        a = self.c.get(BASE + "/accounts/ta1")
        self.assertEqual(a["status"], "sign_in_needed")
        self.assertTrue(all(c["mode"] == "off" for c in a["cars"]))
        self.assertTrue(a["running"])
        sys = [x for x in self.audit() if x["action"] == "tesla-sign-in-needed"]
        self.assertEqual(sys[0]["account"], None)  # written in the server's name
        # a new refresh token fixes it
        self.c.error("PATCH", BASE + "/accounts/ta1", 422, "tesla_refused",
                     body={"version": a["version"], "refresh_token": "refused-2", "confirm_password": PW})
        a = self.c.json("PATCH", BASE + "/accounts/ta1", {"version": a["version"], "refresh_token": TOKEN,
                                                          "confirm_password": PW})
        self.no_token(a)
        self.assertEqual(a["status"], "ok")
        self.assertNotIn("tesla_sign_in_needed", {n["code"] for n in self.c.get("/api/v1/notices")["items"]})
        self.assertIn("tesla-token-replaced", [x["action"] for x in self.audit()])


    def test_8_an_account_in_error_backs_off(self):
        t = self.server.app.store.tesla
        a = t["accounts"]["ta1"]
        a["state"] = "error"
        a["last_error"] = {"code": "forbidden", "message": "Tesla refuses this account's requests", "at_ms": 1}
        got = self.c.get(BASE + "/accounts/ta1")
        want = ["off" if c["match"] in (None, "leave_out") else "backoff" for c in got["cars"]]
        self.assertIn("backoff", want)
        self.assertEqual([c["mode"] for c in got["cars"]], want)
        self.assertTrue(got["running"])
        st = [s for s in self.c.get(BASE + "/status")["items"] if s["account"] == "ta1"]
        self.assertEqual({s["mode"] for s in st if s["car"]}, {"backoff"})
        a["last_error"] = {"code": "endpoint_gone", "message": "closed", "at_ms": 1}
        self.assertEqual({c["mode"] for c in self.c.get(BASE + "/accounts/ta1")["cars"]}, {"off"})


class TeslaWatch(ServerCase):
    """A watch account reads none of it, and hears nothing about Tesla accounts."""

    server_args = {"role": "watch"}

    def test_admin_only(self):
        for m, path in (("GET", BASE), ("GET", BASE + "/accounts"), ("GET", BASE + "/accounts/ta1"),
                        ("GET", BASE + "/accounts/ta1/cars"), ("PUT", BASE + "/accounts/ta1/cars"),
                        ("GET", BASE + "/status"), ("POST", BASE + "/sign-in"),
                        ("POST", BASE + "/accounts"), ("DELETE", BASE + "/accounts/ta1")):
            self.c.error(m, path, 403, "not_allowed", body={} if m != "GET" else None)
        self.assertIsNone(self.c.get("/api/v1/setup/status")["tesla_accounts"])
        self.assertFalse([n for n in self.c.get("/api/v1/notices")["items"] if n["code"].startswith("tesla")])
