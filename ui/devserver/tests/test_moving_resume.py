"""A move that stopped carries on with a new move code from the same server; the set-up code ignores case, spaces
and dashes; a new recovery key from Settings (docs/ui-api.md, "Moving and first-run setup")."""

from __future__ import annotations

from .helpers import ServerCase

PW = "sentry-mate-dev"
DEV_KEY = "SMRK-DEVK-EY00-0000-0000-0000-0000-0000-0000-0000-0000-0000-0000-0000"


def _code():
    from mockapi.resources.moving import make_move_code
    return make_move_code()[0]


class FreshCase(ServerCase):
    server_args = {"needs_setup": True}

    def advance(self, ms):
        self.server.app.clock.advance(ms)


class SetupCodeSpelling(FreshCase):
    def test_case_spaces_and_dashes_do_not_matter(self):
        for typed in ("setup dev 0000", "SETUPDEV0000", " Setup-Dev-0000 ", "SETUP-DEV0000"):
            c = self.server.client()
            self.assertTrue(c.json("POST", "/api/v1/setup/claim", {"setup_code": typed})["claimed"], typed)
        self.server.client().error("POST", "/api/v1/setup/claim", 403, "wrong_setup_code",
                                   body={"setup_code": "SETUP-DEV-0001"})


class MoveCarriesOn(FreshCase):
    def test_a_cancelled_move_carries_on_with_a_new_code_from_the_same_server(self):
        c = self.server.client()
        c.json("POST", "/api/v1/setup/claim", {"setup_code": "SETUP-DEV-0000"})
        first = c.json("POST", "/api/v1/setup/move-here", {"from": "cancel.example", "code": _code()}, expect=201)
        self.advance(10_000)
        j = c.get("/api/v1/setup/job")
        self.assertEqual((j["state"], j["problem"]["code"]), ("stopped", "cancelled"))
        kept = j["progress"]["bytes_done"]
        self.assertGreater(kept, 0)
        # another server: busy (give this move up first); a code with a typo: 400, nothing changes
        c.error("POST", "/api/v1/setup/move-here", 409, "setup_busy", body={"from": "192.168.1.20", "code": _code()})
        c.error("POST", "/api/v1/setup/move-here", 400, "bad_input", body={"from": "cancel.example", "code": "SMMV-1"})
        # the same server with a new code: the same job, from where it stopped
        j = c.json("POST", "/api/v1/setup/move-here", {"from": "cancel.example:10100", "code": _code()})
        self.assertEqual((j["id"], j["started_ms"]), (first["id"], first["started_ms"]))
        self.assertEqual(j["state"], "copying")
        self.assertGreaterEqual(j["progress"]["bytes_done"], kept)
        self.assertIsNone(j["problem"])
        self.advance(20_000)
        self.assertEqual(c.get("/api/v1/setup/job")["state"], "done")



class MoveRunningNotCarriedOn(FreshCase):
    def test_a_move_that_runs_is_not_carried_on(self):
        c = self.server.client()
        c.json("POST", "/api/v1/setup/claim", {"setup_code": "SETUP-DEV-0000"})
        c.json("POST", "/api/v1/setup/move-here", {"from": "gone.example", "code": _code()}, expect=201)
        self.advance(60_000)
        self.assertEqual(c.get("/api/v1/setup/job")["state"], "waiting")
        c.error("POST", "/api/v1/setup/move-here", 409, "setup_busy", body={"from": "gone.example", "code": _code()})
        # the last resort needs confirm: true, sent by the person's tick
        c.error("POST", "/api/v1/setup/move-here/finish", 400, "bad_input",
                body={"recovery_key": DEV_KEY, "confirm": False})
        c.json("DELETE", "/api/v1/setup/job")


class NewRecoveryKey(ServerCase):
    def test_new_key_shown_once_then_saved(self):
        c = self.c
        c.error("POST", "/api/v1/system/recovery-key/new", 400, "bad_input", body={})
        c.error("POST", "/api/v1/system/recovery-key/new", 403, "wrong_password", body={"password": "nope"})
        d = c.json("POST", "/api/v1/system/recovery-key/new", {"password": PW}, expect=201)
        self.assertRegex(d["recovery_key"], r"^SMRK(-[0-9A-HJKMNP-TV-Z]{4}){13}$")
        self.assertIsInstance(d["made_ms"], int)
        notices = c.get("/api/v1/notices")["items"]
        self.assertIn("recovery_key_unsaved", [n["code"] for n in notices])
        self.assertNotIn(d["recovery_key"], repr(notices))
        # until it is saved the file holds the new key
        self.assertEqual(c.json("POST", "/api/v1/system/recovery-key/show", {"password": PW})["recovery_key"],
                         d["recovery_key"])
        self.assertEqual(c.json("POST", "/api/v1/system/recovery-key/saved"), {"saved": True})
        self.assertNotIn("recovery_key_unsaved", [n["code"] for n in c.get("/api/v1/notices")["items"]])
        c.error("POST", "/api/v1/system/recovery-key/show", 409, "recovery_key_saved", body={"password": PW})
        actions = {a["action"] for a in c.get("/api/v1/audit?limit=500")["items"]}
        self.assertIn("recovery_key.new", actions)

    def test_watch_may_not(self):
        w = self.server.client()
        w.json("POST", "/api/v1/session", {"name": "watch", "password": PW})
        w.error("POST", "/api/v1/system/recovery-key/new", 403, "not_allowed", body={"password": PW})
