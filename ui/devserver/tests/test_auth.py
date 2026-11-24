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
                                               "two_step": False, "two_step_required": False,
                                           "two_step_setup_only": False})
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
    def test_an_account_change_asks_for_the_admins_own_password_again(self):
        """As the real server: making an account, changing its role, cars or password, and removing it need
        confirm_password (or the X-Sentry-Mate-Password header)."""
        c = self.c
        pw = {"confirm_password": "sentry-mate-dev"}
        new = {"name": "pat", "password": "made-up-pass-1", "role": "watch", "cars": ["c2"]}
        e, _ = c.error("POST", "/api/v1/accounts", 403, "password_needed", body=new)
        self.assertEqual(e["fields"], {"confirm_password": "Your own password, again."})
        c.error("POST", "/api/v1/accounts", 403, "wrong_password", body={**new, "confirm_password": "not it"})
        self.assertNotIn("pat", {a["name"] for a in c.get("/api/v1/accounts")["items"]})
        a = c.json("POST", "/api/v1/accounts", {**new, **pw}, expect=201)
        self.assertNotIn("confirm_password", a)
        path = f"/api/v1/accounts/{a['id']}"
        # what a page sends along unchanged asks for nothing; a real change does
        a = c.json("PATCH", path, {"version": a["version"], "role": "watch", "cars": ["c2"], "password": ""})
        self.assertEqual((a["role"], a["cars"]), ("watch", ["c2"]))
        for change in ({"role": "admin"}, {"cars": "all"}, {"password": "made-up-pass-2"}):
            c.error("PATCH", path, 403, "password_needed", body={"version": a["version"], **change})
            c.error("PATCH", path, 403, "wrong_password", body={"version": a["version"], **change,
                                                                 "confirm_password": "not it"})
        self.assertEqual(c.get(path)["version"], a["version"])
        up = c.json("PATCH", path, {"version": a["version"], "cars": "all", **pw})
        self.assertEqual(up["cars"], "all")
        up = c.json("PATCH", path, {"version": up["version"], "role": "admin"},
                    headers={"X-Sentry-Mate-Password": "sentry-mate-dev"})
        self.assertEqual(up["role"], "admin")
        c.error("DELETE", path, 403, "password_needed")
        c.error("DELETE", path, 403, "wrong_password", body={"confirm_password": "not it"})
        self.assertEqual(c.json("DELETE", path, pw)["deleted"], True)

    def test_crud_and_last_admin(self):
        c = self.c
        pw = {"confirm_password": "sentry-mate-dev"}
        lst = c.get("/api/v1/accounts")
        self.assertEqual({a["name"] for a in lst["items"]}, {"admin", "watch", "alex"})
        for a in lst["items"]:
            self.assertEqual(set(a), {"id", "name", "role", "cars", "two_step", "two_step_required", "created_ms", "version"})
        st, hd, raw = c.request("POST", "/api/v1/accounts", {"name": "guest", "password": "guest password",
                                                             "role": "watch", "cars": ["c2"], **pw})
        self.assertEqual(st, 201)
        import json
        new = json.loads(raw)
        self.assertEqual(new["cars"], ["c2"])
        c.error("POST", "/api/v1/accounts", 409, "name_taken", body={"name": "Guest", "password": "x" * 12, "role": "watch", **pw})
        c.error("POST", "/api/v1/accounts", 422, "invalid", body={"name": "g2", "password": "x" * 12, "role": "boss", **pw})
        up = c.json("PATCH", f"/api/v1/accounts/{new['id']}", {"version": new["version"], "cars": "all", **pw})
        self.assertEqual((up["cars"], up["version"]), ("all", new["version"] + 1))
        c.error("PATCH", f"/api/v1/accounts/{new['id']}", 409, "stale_version", body={"version": new["version"], "cars": [], **pw})
        # an empty password keeps the current one, as on the real server; a huge one is refused
        same = c.json("PATCH", f"/api/v1/accounts/{new['id']}", {"version": up["version"], "password": ""})
        self.assertEqual(same["version"], up["version"])
        e, _ = c.error("PATCH", f"/api/v1/accounts/{new['id']}", 422, "invalid",
                       body={"version": up["version"], "password": "x" * 1025, **pw})
        self.assertIn("password", e["fields"])
        # the new account can sign in
        g = self.server.client()
        g.cookie = "sentry-mate=out"
        g.json("POST", "/api/v1/session", {"name": "guest", "password": "guest password"})
        self.assertEqual(g.get("/api/v1/me")["role"], "watch")
        self.assertEqual(c.json("DELETE", f"/api/v1/accounts/{new['id']}", pw)["deleted"], True)
        g.error("GET", "/api/v1/me", 401, "not_signed_in")
        c.error("GET", f"/api/v1/accounts/{new['id']}", 404, "no_such_account")
        # two admins: one may be demoted, the last may not
        alex = next(a for a in c.get("/api/v1/accounts")["items"] if a["name"] == "alex")
        alex = c.json("PATCH", f"/api/v1/accounts/{alex['id']}", {"version": alex["version"], "role": "watch", **pw})
        admin = c.get("/api/v1/accounts/a1")
        c.error("PATCH", "/api/v1/accounts/a1", 409, "last_admin", body={"version": admin["version"], "role": "watch", **pw})
        c.error("DELETE", "/api/v1/accounts/a1", 409, "last_admin", body=pw)
        c.json("PATCH", f"/api/v1/accounts/{alex['id']}", {"version": alex["version"], "role": "admin", **pw})


class TwoStep(ServerCase):
    """Two-step sign-in, one's own and the server-wide rule: as the real server (crates/web/src/auth.rs)."""

    def test_no_per_account_rule(self):
        c = self.c
        pw = {"confirm_password": "sentry-mate-dev"}
        msg = ("Two-step sign-in is required for every account or from outside the home network "
               "(Accounts, two-step rule), not for one account.")
        e, _ = c.error("POST", "/api/v1/accounts", 422, "invalid",
                       body={"name": "pat", "password": "made-up-pass-1", "role": "watch", "two_step_required": True, **pw})
        self.assertEqual(e["fields"], {"two_step_required": msg})
        alex = next(a for a in c.get("/api/v1/accounts")["items"] if a["name"] == "alex")
        e, _ = c.error("PATCH", f"/api/v1/accounts/{alex['id']}", 422, "invalid",
                       body={"version": alex["version"], "two_step_required": True, **pw})
        self.assertEqual(e["fields"], {"two_step_required": msg})
        # false is accepted and changes nothing; two_step is the account's own, two_step_required the rule
        c.json("PATCH", f"/api/v1/accounts/{alex['id']}", {"version": alex["version"], "two_step_required": False})
        by = {a["name"]: a for a in c.get("/api/v1/accounts")["items"]}
        self.assertEqual((by["alex"]["two_step"], by["admin"]["two_step"]), (True, False))
        self.assertTrue(all(a["two_step_required"] is False for a in by.values()))

    def test_enrol_recovery_codes_and_turn_off(self):
        c = self.c
        pw = "sentry-mate-dev"
        self.assertEqual(c.get("/api/v1/me/two-step"),
                         {"enabled": False, "recovery_codes_left": None, "required": {"everyone": False, "outside": False}})
        c.error("POST", "/api/v1/me/two-step/confirm", 409, "two_step_first", body={"password": pw, "code": "123456"})
        c.error("POST", "/api/v1/me/two-step/recovery-codes", 409, "two_step_off", body={"password": pw, "code": "123456"})
        c.error("DELETE", "/api/v1/me/two-step", 409, "two_step_off", body={"password": pw, "code": "123456"})
        d = c.json("POST", "/api/v1/me/two-step", {}, expect=201)
        self.assertRegex(d["secret"], r"^[A-Z2-7]{32}$")
        self.assertTrue(d["uri"].startswith("otpauth://totp/") and d["secret"] in d["uri"])
        c.error("POST", "/api/v1/me/two-step/confirm", 403, "wrong_password", body={"password": "nope", "code": "123456"})
        c.error("POST", "/api/v1/me/two-step/confirm", 401, "wrong_code", body={"password": pw, "code": "12345"})
        self.assertIs(c.get("/api/v1/me")["two_step"], False)
        out = c.json("POST", "/api/v1/me/two-step/confirm", {"password": pw, "code": "123456"})
        self.assertIs(out["enabled"], True)
        self.assertEqual(len(out["recovery_codes"]), 10)
        self.assertEqual(c.get("/api/v1/me/two-step")["recovery_codes_left"], 10)
        self.assertIs(c.get("/api/v1/me")["two_step"], True)
        c.error("POST", "/api/v1/me/two-step", 409, "two_step_on")
        new = c.json("POST", "/api/v1/me/two-step/recovery-codes", {"password": pw, "code": "654321"})
        self.assertEqual(len(new["recovery_codes"]), 10)
        self.assertNotEqual(new["recovery_codes"], out["recovery_codes"])
        c.error("DELETE", "/api/v1/me/two-step", 401, "wrong_code", body={"password": pw})
        c.error("DELETE", "/api/v1/me/two-step", 403, "wrong_password", body={"password": "nope", "code": "123456"})
        self.assertEqual(c.json("DELETE", "/api/v1/me/two-step", {"password": pw, "code": "123456"}), {"enabled": False})
        self.assertIs(c.get("/api/v1/me")["two_step"], False)
        actions = [a["action"] for a in c.get("/api/v1/audit?limit=20")["items"]]
        self.assertEqual(actions[:3], ["two-step-off", "recovery-codes-new", "two-step-on"])

    def test_policy(self):
        c = self.c
        self.assertEqual(c.get("/api/v1/two-step-policy"), {"everyone": False, "outside": False})
        c.error("PUT", "/api/v1/two-step-policy", 403, "password_needed", body={"everyone": True})
        c.error("PUT", "/api/v1/two-step-policy", 403, "wrong_password", body={"everyone": True, "confirm_password": "no"})
        c.error("PUT", "/api/v1/two-step-policy", 400, "bad_input",
                body={"everyone": "yes", "confirm_password": "sentry-mate-dev"})
        out = c.json("PUT", "/api/v1/two-step-policy", {"everyone": True, "confirm_password": "sentry-mate-dev"})
        self.assertEqual(out, {"everyone": True, "outside": False})
        me = c.get("/api/v1/me")
        self.assertEqual((me["two_step_required"], me["two_step_setup_only"]), (True, True))
        self.assertEqual(c.get("/api/v1/me/two-step")["required"], {"everyone": True, "outside": False})
        # this admin has no two-step sign-in: until it has, it may only set it up (see SetUpTwoStepFirst); alex has it
        c.error("GET", "/api/v1/accounts", 403, "two_step_setup")
        a = self.server.client()
        a.cookie = "sentry-mate=out"
        d = a.json("POST", "/api/v1/session", {"name": "alex", "password": PW})
        a.json("POST", "/api/v1/session/two-step", {"pending": d["pending"], "code": "000000"})
        self.assertTrue(all(x["two_step_required"] for x in a.get("/api/v1/accounts")["items"]))
        top = next(x for x in a.get("/api/v1/audit?limit=5")["items"] if x["action"] == "two-step-policy")
        self.assertEqual(top["target"], "settings:server")
        a.json("PUT", "/api/v1/two-step-policy", {"everyone": False, "outside": True, "confirm_password": "sentry-mate-dev", "code": "000000"})
        self.assertEqual(c.get("/api/v1/two-step-policy"), {"everyone": False, "outside": True})
        w = self.server.client()
        w.cookie = "sentry-mate=out"
        w.json("POST", "/api/v1/session", {"name": "watch", "password": PW})
        w.error("GET", "/api/v1/two-step-policy", 403, "not_allowed")


class RecoveryCodes(ServerCase):
    """A recovery code is xxxx-xxxx-xxxx-xxxx (16 symbols of lower-case Crockford base32), read with or without dashes
    and spaces, in any case; it works once in place of the app's code, wherever a code is asked."""

    server_args = {"signed_out": True}
    CODE = r"^[0-9a-hjkmnp-tv-z]{4}(-[0-9a-hjkmnp-tv-z]{4}){3}$"

    def sign_in(self, name="admin"):
        c = self.server.client()
        c.cookie = "sentry-mate=out"
        d = c.json("POST", "/api/v1/session", {"name": name, "password": PW})
        return c, d

    def test_new_codes_have_the_real_shape_and_work_once_at_sign_in(self):
        c, _ = self.sign_in()
        c.json("POST", "/api/v1/me/two-step", {}, expect=201)
        codes = c.json("POST", "/api/v1/me/two-step/confirm", {"password": PW, "code": "123456"})["recovery_codes"]
        self.assertEqual(len(codes), 10)
        for code in codes:
            self.assertRegex(code, self.CODE)
        # signing in again: the app's code is asked; a recovery code does instead, typed any way, once
        for typed in (codes[0].upper(), codes[1].replace("-", " "), codes[2].replace("-", "")):
            _, d = self.sign_in()
            self.assertEqual(d["status"], "two_step_pending")
        e = self.server.client()
        e.cookie = "sentry-mate=out"
        d = e.json("POST", "/api/v1/session", {"name": "admin", "password": PW})
        e.error("POST", "/api/v1/session/two-step", 401, "wrong_code", body={"pending": d["pending"], "code": "a3k7-m2p9-q4r8"})
        d = e.json("POST", "/api/v1/session", {"name": "admin", "password": PW})
        ok = e.json("POST", "/api/v1/session/two-step", {"pending": d["pending"], "code": " " + codes[0].upper() + " "})
        self.assertEqual(ok["status"], "done")
        self.assertEqual(e.get("/api/v1/me/two-step")["recovery_codes_left"], 9)
        # used up
        d = e.json("POST", "/api/v1/session", {"name": "admin", "password": PW})
        e.error("POST", "/api/v1/session/two-step", 401, "wrong_code", body={"pending": d["pending"], "code": codes[0]})
        d = e.json("POST", "/api/v1/session", {"name": "admin", "password": PW})
        self.assertEqual(e.json("POST", "/api/v1/session/two-step", {"pending": d["pending"], "code": codes[3].replace("-", " ")})["status"], "done")
        # a recovery code also stands in for the code of a change, once, and new codes stop the old ones
        e.error("POST", "/api/v1/me/two-step/recovery-codes", 401, "wrong_code", body={"password": PW, "code": "zzzz-zzzz-zzzz-zzzz"})
        new = e.json("POST", "/api/v1/me/two-step/recovery-codes", {"password": PW, "code": codes[4]})["recovery_codes"]
        for code in new:
            self.assertRegex(code, self.CODE)
        e.error("POST", "/api/v1/me/two-step/recovery-codes", 401, "wrong_code", body={"password": PW, "code": codes[5]})
        e.json("POST", "/api/v1/me/two-step/recovery-codes", {"password": PW, "code": new[0]})

    def test_the_seeded_account_has_codes(self):
        _, d = self.sign_in("alex")
        c = self.server.client()
        c.cookie = "sentry-mate=out"
        d = c.json("POST", "/api/v1/session", {"name": "alex", "password": PW})
        ok = c.json("POST", "/api/v1/session/two-step", {"pending": d["pending"], "code": "A3K7 M2P9 Q4R8 S5T6"})
        self.assertEqual(ok["status"], "done")
        self.assertEqual(c.get("/api/v1/me/two-step")["recovery_codes_left"], 9)


class SetUpTwoStepFirst(ServerCase):
    """The rule asks every account for two-step sign-in: an account without it is answered 403 two_step_setup on every
    route but /me*, DELETE /session and the public ones, as the real server does (crates/web/src/policy.rs)."""

    def test_only_setting_it_up_is_allowed(self):
        c = self.c
        pw = {"confirm_password": PW}
        c.json("PUT", "/api/v1/two-step-policy", {"everyone": True, **pw})
        me = c.get("/api/v1/me")
        self.assertEqual((me["two_step"], me["two_step_setup_only"]), (False, True))
        for path in ("/api/v1/cars", "/api/v1/accounts", "/api/v1/two-step-policy", "/api/v1/notices", "/api/v1/settings",
                     "/api/v1/events"):
            e, _ = c.error("GET", path, 403, "two_step_setup")
            self.assertEqual(e["message"], "Set up two-step sign-in for your account first.")
        c.error("PUT", "/api/v1/two-step-policy", 403, "two_step_setup", body={"everyone": False, **pw})
        c.error("POST", "/api/v1/accounts", 403, "two_step_setup", body={"name": "x", "password": "y" * 12, **pw})
        # what it may do: its own account, and the set-up
        c.get("/api/v1/me/preferences")
        c.get("/api/v1/me/sessions")
        c.json("POST", "/api/v1/me/two-step", {}, expect=201)
        c.json("POST", "/api/v1/me/two-step/confirm", {"password": PW, "code": "123456"})
        me = c.get("/api/v1/me")
        self.assertEqual((me["two_step"], me["two_step_setup_only"]), (True, False))
        self.assertEqual(len(c.get("/api/v1/cars")["items"]) > 0, True)

    def test_signing_out_and_in_is_allowed(self):
        w = self.server.client()
        w.cookie = "sentry-mate=out"
        w.json("POST", "/api/v1/session", {"name": "watch", "password": PW})
        self.assertTrue(w.get("/api/v1/me")["two_step_setup_only"])
        w.error("GET", "/api/v1/cars", 403, "two_step_setup")
        self.assertEqual(w.json("DELETE", "/api/v1/session")["status"], "signed_out")
        w.error("GET", "/api/v1/me", 401, "not_signed_in")
        self.assertEqual(w.get("/api/v1/setup")["needs_setup"], False)


class Preferences(ServerCase):
    def test_get_patch(self):
        p = self.c.get("/api/v1/me/preferences")
        self.assertEqual(set(p), {"units", "range", "theme", "language", "interface_size", "follow_screen", "map_colors",
                                  "map_style", "ui_pack", "car_order", "pinned_cars", "main_car", "last_car"})
        self.assertEqual((p["main_car"], p["last_car"]), (None, None))
        self.assertEqual(p["map_colors"], "dimmed")
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

    def test_main_and_last_car(self):
        q = self.c.json("PATCH", "/api/v1/me/preferences", {"last_car": "c2", "main_car": "c3"})
        self.assertEqual((q["main_car"], q["last_car"]), ("c3", "c2"))
        self.assertEqual(self.c.get("/api/v1/me/preferences")["last_car"], "c2")
        e, _ = self.c.error("PATCH", "/api/v1/me/preferences", 422, "invalid", body={"last_car": "c99"})
        self.assertIn("last_car", e["fields"])
        self.c.error("PATCH", "/api/v1/me/preferences", 400, "bad_input", body={"main_car": 3})
        q = self.c.json("PATCH", "/api/v1/me/preferences", {"main_car": None, "last_car": None})
        self.assertEqual((q["main_car"], q["last_car"]), (None, None))


class OwnPassword(ServerCase):
    def test_anyone_changes_their_own_password_and_the_other_sessions_end(self):
        """PUT /me/password { password, new_password, code? }, as the real server: the current password again, any
        new one but an empty or a huge one, the account's other sessions ended."""
        w = self.server.client()
        w.cookie = "sentry-mate=out"
        w.json("POST", "/api/v1/session", {"name": "watch", "password": "sentry-mate-dev"})
        other = self.server.client()
        other.cookie = "sentry-mate=out"
        other.json("POST", "/api/v1/session", {"name": "watch", "password": "sentry-mate-dev"})
        path = "/api/v1/me/password"
        w.error("PUT", path, 403, "wrong_password", body={"password": "not it", "new_password": "made-up-new-1"})
        e, _ = w.error("PUT", path, 422, "invalid", body={"password": "sentry-mate-dev", "new_password": ""})
        self.assertIn("new_password", e["fields"])
        e, _ = w.error("PUT", path, 422, "invalid", body={"password": "sentry-mate-dev", "new_password": "x" * 1025})
        self.assertIn("new_password", e["fields"])
        w.error("PUT", path, 400, "bad_input", body={"password": "sentry-mate-dev"})
        w.error("PUT", path, 400, "bad_input", body={"password": "sentry-mate-dev", "new_password": "a", "name": "x"})
        self.assertEqual(other.get("/api/v1/me")["name"], "watch")
        out = w.json("PUT", path, {"password": "sentry-mate-dev", "new_password": "made-up-new-1"})
        self.assertEqual(out, {"signed_out_others": 1})
        self.assertEqual(w.get("/api/v1/me")["name"], "watch")
        other.error("GET", "/api/v1/me", 401, "not_signed_in")
        n = self.server.client()
        n.cookie = "sentry-mate=out"
        n.error("POST", "/api/v1/session", 401, "wrong_credentials", body={"name": "watch", "password": "sentry-mate-dev"})
        n.json("POST", "/api/v1/session", {"name": "watch", "password": "made-up-new-1"})
        recent = [(e["action"], e["account"]) for e in self.c.get("/api/v1/audit?limit=5")["items"]]
        self.assertIn(("password-changed", "watch"), recent)


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
