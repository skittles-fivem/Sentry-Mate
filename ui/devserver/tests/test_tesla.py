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
        self.assertEqual((o["accounts"], o["cars"], o["poll"]), (1, 2, "normal"))
        self.assertEqual(o["hosts"], ["auth.tesla.com", "owner-api.teslamotors.com", "streaming.vn.teslamotors.com"])
        self.assertEqual(o["polls"]["parked_s"], 60)
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
        e, _ = self.c.error("PATCH", BASE, 403, "password_needed", body={"version": v, "poll": "often"})
        self.assertIn("confirm_password", e["fields"])
        self.c.error("PATCH", BASE, 403, "wrong_password",
                     body={"version": v, "poll": "often", "confirm_password": "not-it"})
        self.assertEqual(self.c.get(BASE)["poll"], "normal")  # nothing changed
        # the header does as well
        o = self.c.json("PATCH", BASE, {"version": v, "poll": "often"}, headers={"X-Sentry-Mate-Password": PW})
        self.assertEqual((o["poll"], o["polls"]["parked_s"], o["version"]), ("often", 30, v + 1))
        self.c.error("PATCH", BASE, 409, "stale_version", body={"version": v, "poll": "gentle",
                                                                 "confirm_password": PW})
        self.c.error("PATCH", BASE, 422, "invalid", body={"version": v + 1, "poll": "always",
                                                           "confirm_password": PW})
        self.assertIn("tesla-poll-changed", {x["action"] for x in self.audit()})

    def test_3_two_step_code(self):
        alex = self.server.client()
        d = alex.json("POST", "/api/v1/session", {"name": "alex", "password": PW})
        alex.json("POST", "/api/v1/session/two-step", {"pending": d["pending"], "code": "000000"})
        v = alex.get(BASE)["version"]
        alex.error("PATCH", BASE, 401, "wrong_code", body={"version": v, "poll": "normal", "confirm_password": PW})
        alex.error("PATCH", BASE, 401, "wrong_code",
                   body={"version": v, "poll": "normal", "confirm_password": PW, "code": "123456"})
        o = alex.json("PATCH", BASE, {"version": v, "poll": "normal", "confirm_password": PW, "code": "000000"})
        self.assertEqual(o["poll"], "normal")

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
            "version": a["version"], "confirm_password": PW,
            "items": [{"vehicle": "tv9", "match": "c3"}, {"vehicle": "tv3", "match": "c999"}]})
        self.assertEqual(set(e["fields"]), {"items.0.vehicle", "items.1.match"})
        self.c.error("PUT", path, 400, "bad_input", body={"version": a["version"], "confirm_password": PW,
                                                          "items": [{"vehicle": "tv3", "match": 7}]})
        d = self.c.json("PUT", path, {"version": a["version"], "confirm_password": PW,
                                      "items": [{"vehicle": "tv3", "match": "c3"}]})
        self.assertEqual(d["items"][2]["match"], "c3")
        self.assertNotEqual(d["items"][2]["mode"], "off")  # read from now on
        # a car matched on one account can't be matched on another
        other = self.c.json("POST", BASE + "/accounts", {"refresh_token": "made-up-2", "confirm_password": PW},
                            expect=201)
        self.c.error("PUT", f"{BASE}/accounts/{other['id']}/cars", 409, "car_taken",
                     body={"version": other["version"], "confirm_password": PW,
                           "items": [{"vehicle": other["cars"][0]["vehicle"], "match": "c1"}]})
        # car off again
        d = self.c.json("PUT", path, {"version": d["version"], "confirm_password": PW,
                                      "items": [{"vehicle": "tv3", "match": "leave_out"}]})
        self.assertEqual(d["items"][2]["mode"], "off")
        # pause and resume
        a = self.c.json("PATCH", BASE + "/accounts/ta1", {"version": d["version"], "paused": True,
                                                          "confirm_password": PW})
        self.assertEqual(a["status"], "paused")
        self.assertTrue(all(c["mode"] == "off" for c in a["cars"]))
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


class TeslaWatch(ServerCase):
    """A watch account reads none of it, and hears nothing about Tesla accounts."""

    server_args = {"role": "watch"}

    def test_admin_only(self):
        for m, path in (("GET", BASE), ("GET", BASE + "/accounts"), ("GET", BASE + "/accounts/ta1"),
                        ("GET", BASE + "/accounts/ta1/cars"), ("GET", BASE + "/status"), ("POST", BASE + "/sign-in"),
                        ("POST", BASE + "/accounts"), ("DELETE", BASE + "/accounts/ta1")):
            self.c.error(m, path, 403, "not_allowed", body={} if m != "GET" else None)
        self.assertIsNone(self.c.get("/api/v1/setup/status")["tesla_accounts"])
        self.assertFalse([n for n in self.c.get("/api/v1/notices")["items"] if n["code"].startswith("tesla")])
