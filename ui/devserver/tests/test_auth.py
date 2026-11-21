"""Sign-in, accounts and preferences."""

from __future__ import annotations

from .helpers import ServerCase

PW = "sentry-mate-dev"


class SignIn(ServerCase):
    server_args = {"signed_out": True}

    def test_sign_in_and_out(self):
        c = self.server.client()
        d = c.json("POST", "/api/v1/session", {"name": "admin", "password": PW})
        self.assertEqual(d["status"], "done")
        self.assertIn("sentry-mate=", c.cookie)
        self.assertEqual(c.get("/api/v1/me"), {"id": "a1", "name": "admin", "role": "admin", "cars": "all",
                                               "two_step_required": False})
        self.assertEqual(c.json("DELETE", "/api/v1/session")["status"], "signed_out")
        c.error("GET", "/api/v1/me", 401, "not_signed_in")

    def test_two_step(self):
        c = self.server.client()
        d = c.json("POST", "/api/v1/session", {"name": "alex", "password": PW})
        self.assertEqual(d["status"], "two_step_pending")
        c.error("POST", "/api/v1/session/two-step", 401, "wrong_code", body={"pending": d["pending"], "code": "123456"})
        c.error("GET", "/api/v1/me", 401, "not_signed_in")
        ok = c.json("POST", "/api/v1/session/two-step", {"pending": d["pending"], "code": "000000"})
        self.assertEqual(ok["status"], "done")
        self.assertEqual(c.get("/api/v1/me")["name"], "alex")
        c.error("POST", "/api/v1/session/two-step", 401, "sign_in_expired", body={"pending": d["pending"], "code": "000000"})

    def test_bad_body(self):
        c = self.server.client()
        e, _ = c.error("POST", "/api/v1/session", 400, "bad_input", body={"name": "admin"})
        self.assertIn("password", e["fields"])


class Setup(ServerCase):
    server_args = {"needs_setup": True, "signed_out": True}

    def test_first_admin(self):
        c = self.server.client()
        self.assertEqual(c.get("/api/v1/setup")["needs_setup"], True)
        c.error("POST", "/api/v1/setup", 403, "wrong_setup_code", body={"code": "nope", "name": "owner", "password": "x" * 12})
        e, _ = c.error("POST", "/api/v1/setup", 422, "invalid", body={"code": "SETUP-DEV-0000", "name": "owner", "password": ""})
        self.assertIn("password", e["fields"])
        e, _ = c.error("POST", "/api/v1/setup", 422, "invalid",
                       body={"code": "SETUP-DEV-0000", "name": "owner", "password": "x" * 1025})
        self.assertIn("password", e["fields"])
        d = c.json("POST", "/api/v1/setup", {"code": "SETUP-DEV-0000", "name": "owner", "password": "a long password"})
        self.assertEqual(d["status"], "done")
        self.assertRegex(d["recovery_key"], r"^SMRK-")
        self.assertEqual(c.get("/api/v1/me")["role"], "admin")
        self.assertEqual(c.get("/api/v1/setup")["needs_setup"], False)
        c.error("POST", "/api/v1/setup", 409, "setup_done", body={"code": "SETUP-DEV-0000", "name": "x", "password": "y" * 12})


class Accounts(ServerCase):
    def test_crud_and_last_admin(self):
        c = self.c
        lst = c.get("/api/v1/accounts")
        self.assertEqual({a["name"] for a in lst["items"]}, {"admin", "watch", "alex"})
        for a in lst["items"]:
            self.assertEqual(set(a), {"id", "name", "role", "cars", "two_step_required", "created_ms", "version"})
        st, hd, raw = c.request("POST", "/api/v1/accounts", {"name": "guest", "password": "guest password",
                                                             "role": "watch", "cars": ["c2"]})
        self.assertEqual(st, 201)
        import json
        new = json.loads(raw)
        self.assertEqual(new["cars"], ["c2"])
        c.error("POST", "/api/v1/accounts", 409, "name_taken", body={"name": "Guest", "password": "x" * 12, "role": "watch"})
        c.error("POST", "/api/v1/accounts", 422, "invalid", body={"name": "g2", "password": "x" * 12, "role": "boss"})
        up = c.json("PATCH", f"/api/v1/accounts/{new['id']}", {"version": new["version"], "cars": "all"})
        self.assertEqual((up["cars"], up["version"]), ("all", new["version"] + 1))
        c.error("PATCH", f"/api/v1/accounts/{new['id']}", 409, "stale_version", body={"version": new["version"], "cars": []})
        # an empty password keeps the current one, as on the real server; a huge one is refused
        same = c.json("PATCH", f"/api/v1/accounts/{new['id']}", {"version": up["version"], "password": ""})
        self.assertEqual(same["version"], up["version"])
        e, _ = c.error("PATCH", f"/api/v1/accounts/{new['id']}", 422, "invalid",
                       body={"version": up["version"], "password": "x" * 1025})
        self.assertIn("password", e["fields"])
        # the new account can sign in
        g = self.server.client()
        g.cookie = "sentry-mate=out"
        g.json("POST", "/api/v1/session", {"name": "guest", "password": "guest password"})
        self.assertEqual(g.get("/api/v1/me")["role"], "watch")
        self.assertEqual(c.json("DELETE", f"/api/v1/accounts/{new['id']}")["deleted"], True)
        g.error("GET", "/api/v1/me", 401, "not_signed_in")
        c.error("GET", f"/api/v1/accounts/{new['id']}", 404, "no_such_account")
        # two admins: one may be demoted, the last may not
        alex = next(a for a in c.get("/api/v1/accounts")["items"] if a["name"] == "alex")
        alex = c.json("PATCH", f"/api/v1/accounts/{alex['id']}", {"version": alex["version"], "role": "watch"})
        admin = c.get("/api/v1/accounts/a1")
        c.error("PATCH", "/api/v1/accounts/a1", 409, "last_admin", body={"version": admin["version"], "role": "watch"})
        c.error("DELETE", "/api/v1/accounts/a1", 409, "last_admin")
        c.json("PATCH", f"/api/v1/accounts/{alex['id']}", {"version": alex["version"], "role": "admin"})


class Preferences(ServerCase):
    def test_get_patch(self):
        p = self.c.get("/api/v1/me/preferences")
        self.assertEqual(set(p), {"units", "range", "theme", "language", "interface_size", "follow_screen", "ui_pack",
                                  "car_order", "pinned_cars"})
        self.assertIs(p["follow_screen"], True)
        self.assertEqual(p["language"], "auto")  # a new account follows its browser until it picks one
        self.assertEqual(p["ui_pack"], "instrument")
        q = self.c.json("PATCH", "/api/v1/me/preferences", {"units": {"distance": "mi"}, "pinned_cars": ["c3"]})
        self.assertEqual(q["units"], {"distance": "mi", "temperature": "c", "pressure": "bar"})
        self.assertEqual(q["pinned_cars"], ["c3"])
        e, _ = self.c.error("PATCH", "/api/v1/me/preferences", 422, "invalid", body={"units": {"pressure": "atm"}})
        self.assertIn("units.pressure", e["fields"])
        self.c.error("PATCH", "/api/v1/me/preferences", 422, "invalid", body={"car_order": ["c99"]})
        self.c.error("PATCH", "/api/v1/me/preferences", 400, "bad_input", body={"colour": "red"})
        self.c.error("PATCH", "/api/v1/me/preferences", 400, "bad_input", body={"follow_screen": "yes"})
        q = self.c.json("PATCH", "/api/v1/me/preferences", {"follow_screen": False, "interface_size": 125})
        self.assertIs(q["follow_screen"], False)
        self.assertEqual(q["interface_size"], 125)
        self.c.json("PATCH", "/api/v1/me/preferences", {"follow_screen": True, "interface_size": 100})
        # custom sort follows the pins
        cars = self.c.get("/api/v1/cars?sort=custom")
        self.assertEqual(cars["items"][0]["id"], "c3")
        self.c.json("PATCH", "/api/v1/me/preferences", {"units": {"distance": "km"}, "pinned_cars": []})


class Sessions(ServerCase):
    def test_list_and_end(self):
        c = self.server.client()
        c.get("/api/v1/me")
        d = c.get("/api/v1/me/sessions")
        mine = [s for s in d["items"] if s["this_one"]]
        self.assertEqual(len(mine), 1)
        for s in d["items"]:
            self.assertEqual(set(s) >= {"id", "created_ms", "last_seen_ms", "this_one"}, True)
        other = next(s for s in d["items"] if not s["this_one"])
        c.json("DELETE", f"/api/v1/me/sessions/{other['id']}")
        c.error("DELETE", f"/api/v1/me/sessions/{other['id']}", 404, "no_such_session")
        n = c.json("DELETE", "/api/v1/me/sessions")["ended"]
        self.assertGreaterEqual(n, 1)
        left = c.get("/api/v1/me/sessions")["items"]
        self.assertEqual([s["this_one"] for s in left], [True])
