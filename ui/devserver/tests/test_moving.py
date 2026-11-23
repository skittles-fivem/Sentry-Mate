"""First-run set-up (claim, start fresh, move here, restore) and moving to another server (move code, export
file, undo): docs/ui-api.md, "Moving and first-run setup"."""

from __future__ import annotations

import base64
import hashlib
import json
import threading

from mockapi.resources.moving import EXPORT_SHA_MS

from .helpers import NOW, Running, ServerCase

PW = "sentry-mate-dev"
SETUP_CODE = "SETUP-DEV-0000"
DEV_KEY = "SMRK-DEVK-EY00-0000-0000-0000-0000-0000-0000-0000-0000-0000-0000-0000"
SEC = 1000
MIN = 60 * SEC


def _code():
    from mockapi.resources.moving import make_move_code
    return make_move_code()[0]


def _claimed(server):
    c = server.client()
    d = c.json("POST", "/api/v1/setup/claim", {"setup_code": SETUP_CODE})
    assert d["claimed"] is True, d
    return c


def _setup_cookie(c):
    return c.cookie and c.cookie.startswith("sentry-mate-setup=")


class Codes(ServerCase):
    def test_move_code_check_matches_the_crate(self):
        from mockapi.resources.moving import read_move_code
        code = _code()
        self.assertRegex(code, r"^SMMV(-[0-9A-HJKMNP-TV-Z]{5}){6}$")
        self.assertIsNone(read_move_code(code))
        self.assertIsNone(read_move_code(code.lower().replace("-", " ").replace("0", "o").replace("1", "l")))
        # every single wrong symbol is found
        body = code[5:]
        for i, ch in enumerate(body):
            if ch == "-":
                continue
            other = "2" if ch != "2" else "3"
            self.assertEqual(read_move_code("SMMV-" + body[:i] + other + body[i + 1:]), "typo")
        self.assertEqual(read_move_code("SMMV-1K3QX"), "typo")


class Fresh(ServerCase):
    server_args = {"needs_setup": True}

    def test_a_fresh_server_needs_the_setup_code(self):
        c = self.server.client()
        d = c.get("/api/v1/setup")
        self.assertEqual(d, {"needs_setup": True, "state": "fresh", "claimed": False, "claim_expires_ms": None,
                             "job": None})
        # no one is signed in by themselves on a fresh server
        c.error("GET", "/api/v1/me", 401, "not_signed_in")
        for path, body in (("/api/v1/setup/fresh", {"name": "owner", "password": "a long password"}),
                           ("/api/v1/setup/move-here", {"from": "192.168.1.20", "code": _code()})):
            c.error("POST", path, 401, "not_claimed", body=body)
        c.error("GET", "/api/v1/setup/job", 401, "not_claimed")
        c.error("POST", "/api/v1/setup/claim", 403, "wrong_setup_code", body={"setup_code": "SETUP-DEV-9999"})
        c.error("POST", "/api/v1/setup/claim", 400, "bad_input", body={"code": SETUP_CODE})
        # case, spaces and dashes do not matter (docs/ui-api.md, and the hint on the set-up page)
        for typed in ("SETUP DEV 0000", "setupdev0000", "Setup-Dev 0000"):
            self.assertTrue(self.server.client().json("POST", "/api/v1/setup/claim", {"setup_code": typed})["claimed"],
                            typed)
        d = c.json("POST", "/api/v1/setup/claim", {"setup_code": " setup-dev-0000 "})
        self.assertTrue(d["claimed"])
        self.assertTrue(_setup_cookie(c))
        self.assertGreater(d["claim_expires_ms"], 0)
        c.error("GET", "/api/v1/setup/job", 404, "no_such_job")
        # a second browser with the code takes the claim over: the first one's ends
        c2 = _claimed(self.server)
        self.assertFalse(c.get("/api/v1/setup")["claimed"])
        self.assertTrue(c2.get("/api/v1/setup")["claimed"])
        e, _ = c2.error("POST", "/api/v1/setup/fresh", 422, "invalid", body={"name": "owner", "password": ""})
        self.assertIn("password", e["fields"])
        d = c2.json("POST", "/api/v1/setup/fresh", {"name": "owner", "password": "a long password",
                                                    "time_zone": "Europe/Berlin"}, expect=201)
        self.assertEqual(d["status"], "done")
        # the browser's zone: new cars start in it
        self.assertEqual(c2.get("/api/v1/settings")["default_time_zone"], "Europe/Berlin")
        self.assertRegex(d["recovery_key"], r"^SMRK(-[0-9A-HJKMNP-TV-Z]{4}){13}$")
        self.assertEqual(c2.get("/api/v1/me")["name"], "owner")
        self.assertEqual(c2.get("/api/v1/setup")["state"], "set_up")
        # the recovery key is never shown again: no answer carries it
        notices = c2.get("/api/v1/notices")["items"]
        self.assertIn("recovery_key_unsaved", [n["code"] for n in notices])
        self.assertNotIn(d["recovery_key"], repr(c2.get("/api/v1/setup")) + repr(notices))
        # a key page closed before the tick: Settings shows the file's key once more, behind the password
        c2.error("POST", "/api/v1/system/recovery-key/show", 400, "bad_input", body={})
        c2.error("POST", "/api/v1/system/recovery-key/show", 403, "wrong_password", body={"password": "nope"})
        self.assertEqual(c2.json("POST", "/api/v1/system/recovery-key/show", {"password": "a long password"}),
                         {"recovery_key": d["recovery_key"]})
        self.assertEqual(c2.json("POST", "/api/v1/system/recovery-key/saved"), {"saved": True})
        self.assertNotIn("recovery_key_unsaved", [n["code"] for n in c2.get("/api/v1/notices")["items"]])
        # the file is gone: nothing shows the key again
        c2.error("POST", "/api/v1/system/recovery-key/show", 409, "recovery_key_saved",
                 body={"password": "a long password"})
        # set up: every set-up call answers 409 setup_done
        for path, body in (("/api/v1/setup/claim", {"setup_code": SETUP_CODE}),
                           ("/api/v1/setup/fresh", {"name": "x", "password": "y" * 12}),
                           ("/api/v1/setup/move-here", {"from": "192.168.1.20", "code": _code()})):
            c.error("POST", path, 409, "setup_done", body=body)
        c.error("GET", "/api/v1/setup/job", 409, "setup_done")


class Throttle(ServerCase):
    server_args = {"needs_setup": True}

    def test_wrong_setup_codes_are_throttled(self):
        c = self.server.client()
        codes = [c.request("POST", "/api/v1/setup/claim", {"setup_code": f"SETUP-{i:04d}"})[0] for i in range(11)]
        self.assertEqual(codes[:10], [403] * 10)
        self.assertEqual(codes[10], 429)
        e, hd = c.error("POST", "/api/v1/setup/claim", 429, "throttled", body={"setup_code": SETUP_CODE})
        self.assertIn("retry-after", hd)


class OneCallSetUp(ServerCase):
    """POST /api/v1/setup answers like the real server (crates/web auth.rs post_setup, accounts::Accounts::setup).
    Each test has a fresh server of its own (they set it up and use up tries)."""

    @classmethod
    def setUpClass(cls):
        pass

    @classmethod
    def tearDownClass(cls):
        pass

    def setUp(self):
        self.server = Running(needs_setup=True)
        self.addCleanup(self.server.close)

    def body(self, **kw):
        return {"code": SETUP_CODE, "name": "owner", "password": "a long password", **kw}

    def test_the_code_reads_like_the_real_one(self):
        # the SETUP- in front does not matter, nor O/I/L for 0/1
        for typed in ("DEV-0000", "dev 0000", "SETUP-DEV-OOOO"):
            self.assertTrue(self.server.client().json("POST", "/api/v1/setup/claim", {"setup_code": typed})["claimed"],
                            typed)

    def test_unknown_fields_an_empty_password_and_the_claim(self):
        c = _claimed(self.server)
        other = self.server.client()
        other.error("POST", "/api/v1/setup", 400, "bad_input", body=self.body(role="admin"))
        other.error("POST", "/api/v1/setup", 422, "invalid", body=self.body(password=""))
        # a weak one is the owner's choice: the page warns, the server takes it
        d = other.json("POST", "/api/v1/setup", self.body(code="DEV-0000", name="owner", password="owner1"))
        self.assertEqual(d["status"], "done")
        # the claim is left alone: it may be another browser's
        self.assertEqual((c.get("/api/v1/setup")["claimed"], c.get("/api/v1/setup")["state"]), (True, "set_up"))

    def test_five_wrong_codes_then_429_counted_apart_from_the_claim(self):
        c = self.server.client()
        codes = [c.request("POST", "/api/v1/setup", self.body(code="SETUP-0000-0000"))[0] for _ in range(6)]
        self.assertEqual(codes, [403] * 5 + [429])
        c.error("POST", "/api/v1/setup/claim", 403, "wrong_setup_code", body={"setup_code": "0000"})

    def test_a_wrong_code_is_wrong_while_a_move_runs(self):
        c = _claimed(self.server)
        c.json("POST", "/api/v1/setup/move-here", {"from": "192.168.1.20", "code": _code()}, expect=201)
        other = self.server.client()
        other.error("POST", "/api/v1/setup", 403, "wrong_setup_code", body=self.body(code="nope"))
        other.error("POST", "/api/v1/setup", 409, "setup_busy", body=self.body())


class FreshCase(ServerCase):
    server_args = {"needs_setup": True}

    def advance(self, ms):
        self.server.app.clock.advance(ms)


class MoveHereErrors(FreshCase):
    def test_errors_before_anything_starts(self):
        c = _claimed(self.server)
        e, _ = c.error("POST", "/api/v1/setup/move-here", 400, "bad_input", body={"from": "http://192.168.1.20",
                                                                                   "code": _code()})
        self.assertIn("from", e["fields"])
        e, _ = c.error("POST", "/api/v1/setup/move-here", 400, "bad_input", body={"from": "192.168.1.20",
                                                                                   "code": "SMMV-1K3QX-7WD2M"})
        self.assertIn("code", e["fields"])
        for host, status, code in (("unreachable.example", 502, "old_server_unreachable"),
                                   ("old.invalid", 502, "old_server_unreachable"),
                                   ("wrong.example", 422, "move_code_wrong"),
                                   ("expired.example", 422, "move_code_expired"),
                                   ("used.example", 409, "move_code_used"),
                                   ("newer.example", 422, "move_too_old"),
                                   ("away.example:10100", 422, "move_not_home_network"),
                                   ("nomove.example", 422, "move_not_offered")):
            e, _ = c.error("POST", "/api/v1/setup/move-here", status, code, body={"from": host, "code": _code()})
            self.assertTrue(e["message"])
        self.assertIsNone(c.get("/api/v1/setup")["job"])


class MoveHere(FreshCase):
    def test_a_move_runs_to_the_end_then_people_sign_in(self):
        c = _claimed(self.server)
        j = c.json("POST", "/api/v1/setup/move-here", {"from": "https://192.168.1.20:10100/api/can/logs",
                                                       "code": _code()}, expect=201)
        self.assertEqual((j["kind"], j["state"], j["from"]), ("move_here", "connecting", "192.168.1.20:10100"))
        other = self.server.client()
        self.assertEqual(other.get("/api/v1/setup")["state"], "moving_here")
        self.assertIsNone(other.get("/api/v1/setup")["job"])
        c.error("POST", "/api/v1/setup/fresh", 409, "setup_busy", body={"name": "owner", "password": "a long password"})
        seen = []
        for _ in range(30):
            j = c.get("/api/v1/setup/job")
            if not seen or seen[-1] != j["state"]:
                seen.append(j["state"])
            if j["state"] == "copying":
                p = j["progress"]
                self.assertEqual(p["round"], 1)
                self.assertLessEqual(p["bytes_done"], p["bytes_total"])
                self.assertEqual(p["cars_total"], 3)
                self.assertLessEqual(p["cars_done"], p["cars_total"])
                self.assertIsNotNone(p["eta_s"])
            if j["state"] == "done":
                break
            self.advance(500)
        self.assertEqual(seen, ["connecting", "copying", "catching_up", "finishing", "switching", "done"])
        self.assertEqual((j["progress"]["cars_done"], j["progress"]["cars_total"]), (3, 3))
        self.assertEqual(j["result"]["accounts"], 3)
        self.assertFalse(j["result"]["forced"])
        d = c.get("/api/v1/setup")
        self.assertEqual((d["state"], d["needs_setup"], d["job"]["state"]), ("set_up", False, "done"))
        c.error("DELETE", "/api/v1/setup/job", 409, "setup_done")
        # the old server's accounts: nobody is signed in by themselves
        s = self.server.client()
        s.error("GET", "/api/v1/me", 401, "not_signed_in")
        self.assertEqual(s.json("POST", "/api/v1/session", {"name": "admin", "password": PW})["status"], "done")


class MoveFlaky(FreshCase):
    def test_flaky_waits_then_carries_on(self):
        c = _claimed(self.server)
        c.json("POST", "/api/v1/setup/move-here", {"from": "flaky.example", "code": _code()}, expect=201)
        self.advance(1000 + 2600)
        j = c.get("/api/v1/setup/job")
        self.assertEqual(j["state"], "waiting")
        self.assertEqual(j["problem"]["code"], "unreachable")
        self.assertIsNotNone(j["problem"]["retry_ms"])
        self.advance(5000)
        self.assertEqual(c.get("/api/v1/setup/job")["state"], "copying")
        self.advance(20_000)
        self.assertEqual(c.get("/api/v1/setup/job")["state"], "done")


class MoveCancelled(FreshCase):
    def test_cancelled_on_the_old_server_stops_and_can_be_given_up(self):
        c = _claimed(self.server)
        c.json("POST", "/api/v1/setup/move-here", {"from": "cancel.example", "code": _code()}, expect=201)
        self.advance(10_000)
        j = c.get("/api/v1/setup/job")
        self.assertEqual((j["state"], j["problem"]["code"], j["problem"]["retry_ms"]), ("stopped", "cancelled", None))
        d = c.json("DELETE", "/api/v1/setup/job")
        self.assertTrue(d["deleted"])
        self.assertEqual(c.get("/api/v1/setup")["state"], "fresh")


class MoveGone(FreshCase):
    def test_old_server_gone_finish_with_the_recovery_key(self):
        c = _claimed(self.server)
        c.json("POST", "/api/v1/setup/move-here", {"from": "gone.example", "code": _code()}, expect=201)
        c.error("POST", "/api/v1/setup/move-here/finish", 409, "not_ready",
                body={"recovery_key": DEV_KEY, "confirm": True})
        self.advance(60_000)
        self.assertEqual(c.get("/api/v1/setup/job")["state"], "waiting")
        c.error("POST", "/api/v1/setup/move-here/finish", 400, "bad_input", body={"recovery_key": DEV_KEY})
        c.error("POST", "/api/v1/setup/move-here/finish", 400, "bad_input",
                body={"recovery_key": "SMRK-1234", "confirm": True})
        c.error("POST", "/api/v1/setup/move-here/finish", 422, "wrong_recovery_key",
                body={"recovery_key": DEV_KEY.replace("DEVK", "DEVX"), "confirm": True})
        j = c.json("POST", "/api/v1/setup/move-here/finish", {"recovery_key": DEV_KEY.lower(), "confirm": True})
        self.assertEqual(j["state"], "switching")
        self.advance(2000)
        j = c.get("/api/v1/setup/job")
        self.assertEqual(j["state"], "done")
        self.assertTrue(j["result"]["forced"])


class Uploads:
    def export_bytes(self):
        from mockapi.resources.moving import EXPORT_MAGIC
        return EXPORT_MAGIC + b'{"made_up": true}\n' + bytes(range(256)) * 9000

    def start(self, c, data, name="sentry-mate-2026-11-17.smexport", key=DEV_KEY, expect=201):
        head = base64.b64encode(data[:65536]).decode()
        return c.json("POST", "/api/v1/setup/restore", {"name": name, "size": len(data), "recovery_key": key,
                                                        "header": head}, expect=expect)

    def send(self, c, data, start, n, sha=True):
        chunk = data[start:start + n]
        h = {"Upload-Offset": str(start), "Upload-Length": str(len(data)),
             "Content-Type": "application/offset+octet-stream"}
        if sha:
            h["Upload-SHA256"] = hashlib.sha256(chunk).hexdigest()
        return c.request("PATCH", "/api/v1/setup/restore", chunk, headers=h)


class Restore(Uploads, FreshCase):
    def test_checks_before_the_upload(self):
        c = _claimed(self.server)
        data = self.export_bytes()
        head = base64.b64encode(data[:65536]).decode()
        c.error("POST", "/api/v1/setup/restore", 422, "not_an_export",
                body={"name": "x.smexport", "size": 100, "recovery_key": DEV_KEY,
                      "header": base64.b64encode(b"x" * 100).decode()})
        c.error("POST", "/api/v1/setup/restore", 400, "bad_input",
                body={"name": "x.smexport", "size": len(data), "recovery_key": DEV_KEY, "header": "!!"})
        c.error("POST", "/api/v1/setup/restore", 422, "export_too_new",
                body={"name": "newer.smexport", "size": len(data), "recovery_key": DEV_KEY, "header": head})
        e, _ = c.error("POST", "/api/v1/setup/restore", 422, "wrong_recovery_key",
                       body={"name": "x.smexport", "size": len(data), "recovery_key": DEV_KEY.replace("EY00", "EY01"),
                             "header": head})
        self.assertIn("recovery_key", e["fields"])

    def test_upload_resume_and_finish(self):
        c = _claimed(self.server)
        data = self.export_bytes()
        j = self.start(c, data)
        self.assertEqual((j["kind"], j["state"], j["received"]), ("restore", "uploading", 0))
        self.assertEqual(j["upload_url"], "/api/v1/setup/restore")
        n = j["chunk_max_bytes"]
        st, _, _ = self.send(c, data, 0, n)
        self.assertEqual(st, 200)
        # stopped half way: the same file again carries on from what the server holds
        j = self.start(c, data, expect=200)
        self.assertEqual(j["received"], n)
        st, hd, raw = self.send(c, data, 0, n)
        self.assertEqual(st, 409)
        self.assertEqual(hd["upload-offset"], str(n))
        off = n
        while off < len(data):
            st, _, raw = self.send(c, data, off, n, sha=False)
            self.assertEqual(st, 200, raw)
            off += n
        self.assertEqual(c.get("/api/v1/setup/job")["state"], "checking")
        self.advance(3000)
        self.assertEqual(c.get("/api/v1/setup/job")["state"], "switching")
        self.advance(2000)
        j = c.get("/api/v1/setup/job")
        self.assertEqual(j["state"], "done")
        self.assertEqual(j["result"]["file_name"], "sentry-mate-2026-11-17.smexport")
        self.assertEqual(c.get("/api/v1/setup")["state"], "set_up")


class RestoreBroken(Uploads, FreshCase):
    def test_a_damaged_file_fails(self):
        c = _claimed(self.server)
        data = self.export_bytes()
        j = self.start(c, data, name="broken.smexport")
        off, n = 0, j["chunk_max_bytes"]
        while off < len(data):
            st, _, raw = self.send(c, data, off, n)
            if st != 200:
                break
            off += n
        j = c.get("/api/v1/setup/job")
        self.assertEqual((j["state"], j["problem"]["code"]), ("failed", "export_damaged"))
        self.assertEqual(c.get("/api/v1/setup")["state"], "fresh")
        # another file can be tried at once
        self.start(c, data)


class OldServer(ServerCase):
    def advance(self, ms):
        self.server.app.clock.advance(ms)

    def test_move_code_progress_moved_and_undo(self):
        c = self.c
        self.assertEqual(c.get("/api/v1/system/move")["state"], "none")
        e, _ = c.error("POST", "/api/v1/system/move", 400, "bad_input", body={})
        self.assertEqual(e["fields"], {"password": "Required."})
        c.error("POST", "/api/v1/system/move", 403, "wrong_password", body={"password": "nope"})
        d = c.json("POST", "/api/v1/system/move", {"password": PW}, expect=201)
        self.assertRegex(d["move_code"], r"^SMMV(-[0-9A-HJKMNP-TV-Z]{5}){6}$")
        self.assertEqual(d["state"], "offered")
        self.assertEqual(d["code"]["expires_ms"] - d["code"]["made_ms"], 24 * 3600 * SEC)
        g = c.get("/api/v1/system/move")
        self.assertNotIn(d["move_code"], repr(g))  # shown once
        self.assertIn("move_code_waiting", [n["code"] for n in c.get("/api/v1/notices")["items"]])
        self.advance(15 * SEC)
        g = c.get("/api/v1/system/move")
        self.assertEqual(g["state"], "copying")
        self.assertEqual(g["peer"]["address"], "192.168.1.30")
        self.assertEqual(g["progress"]["round"], 1)
        c.error("POST", "/api/v1/system/move", 409, "move_under_way", body={"password": PW})
        c.error("POST", "/api/v1/system/move/undo", 409, "not_moved", body={"password": PW, "confirm": True})
        self.advance(13 * SEC + 500)
        self.assertEqual(c.get("/api/v1/system/move")["state"], "paused")
        self.advance(2 * SEC)
        g = c.get("/api/v1/system/move")
        self.assertEqual(g["state"], "moved")
        self.assertIsNone(g["moved"]["switched_ms"])
        # a moved server takes no changes; reading and the move itself still work
        c.error("PATCH", "/api/v1/settings", 409, "moved", body={"version": 1, "range": "rated"})
        self.advance(2 * SEC)
        g = c.get("/api/v1/system/move")
        self.assertEqual(g["state"], "done")
        self.assertIsNotNone(g["moved"]["switched_ms"])
        self.assertIn("moved", [n["code"] for n in c.get("/api/v1/notices")["items"]])
        c.error("DELETE", "/api/v1/system/move", 409, "finished")
        c.error("POST", "/api/v1/system/move", 409, "moved", body={"password": PW})
        c.error("POST", "/api/v1/system/move/undo", 400, "bad_input", body={"password": PW})
        g = c.json("POST", "/api/v1/system/move/undo", {"password": PW, "confirm": True})
        self.assertEqual(g["state"], "none")
        self.assertIsNotNone(g["last_undone_ms"])
        actions = {a["action"] for a in c.get("/api/v1/audit?limit=500")["items"]}
        self.assertTrue({"move-code-made", "move-undone"} <= actions)

    def test_cancel_a_code_and_end_a_move(self):
        c = self.c
        c.json("POST", "/api/v1/system/move", {"password": PW, "anywhere": True}, expect=201)
        d = c.json("DELETE", "/api/v1/system/move")
        self.assertEqual((d["cancelled"], d["state"]), ("revoked", "none"))
        c.json("POST", "/api/v1/system/move", {"password": PW}, expect=201)
        self.advance(16 * SEC)
        d = c.json("POST", "/api/v1/system/move", {"password": PW, "replace": True}, expect=201)
        self.assertEqual(d["state"], "offered")
        self.assertEqual(d["last_cancelled"]["why"], "replaced by a new move code")
        self.advance(16 * SEC)
        d = c.json("DELETE", "/api/v1/system/move")
        self.assertEqual((d["cancelled"], d["state"]), ("ended", "none"))
        self.assertEqual(c.json("DELETE", "/api/v1/system/move")["cancelled"], "nothing")

    def test_watch_may_not(self):
        w = self.server.client()
        w.json("POST", "/api/v1/session", {"name": "watch", "password": PW})
        w.error("GET", "/api/v1/system/move", 403, "not_allowed")
        w.error("POST", "/api/v1/system/exports", 403, "not_allowed", body={"password": PW})
        w.error("POST", "/api/v1/system/recovery-key/show", 403, "not_allowed", body={"password": PW})
        # the move's notices are an admin's to read too
        self.c.json("POST", "/api/v1/system/move", {"password": PW}, expect=201)
        self.assertIn("move_code_waiting", [n["code"] for n in self.c.get("/api/v1/notices")["items"]])
        self.assertNotIn("move_code_waiting", [n["code"] for n in w.get("/api/v1/notices")["items"]])


class TwoStepReauth(ServerCase):
    def test_two_step_code_too(self):
        a = self.server.client()
        d = a.json("POST", "/api/v1/session", {"name": "alex", "password": PW})
        a.json("POST", "/api/v1/session/two-step", {"pending": d["pending"], "code": "000000"})
        e, _ = a.error("POST", "/api/v1/system/exports", 403, "wrong_two_step", body={"password": PW})
        self.assertIn("two_step", e["fields"])
        a.json("POST", "/api/v1/system/exports", {"password": PW, "two_step": "000000"}, expect=201)


class Exports(ServerCase):
    def advance(self, ms):
        self.server.app.clock.advance(ms)

    def test_prepare_download_resume(self):
        c = self.c
        e = c.json("POST", "/api/v1/system/exports", {"password": PW}, expect=201)
        self.assertEqual((e["state"], e["size"], e["url"]), ("preparing", None, None))
        c.error("POST", "/api/v1/system/exports", 409, "export_preparing", body={"password": PW})
        c.error("GET", f"/api/v1/system/exports/{e['id']}/file", 409, "not_ready")
        self.advance(3 * SEC)
        e = c.get(f"/api/v1/system/exports/{e['id']}")
        self.assertEqual(e["state"], "ready")
        self.assertTrue(e["file_name"].endswith(".smexport"))
        self.assertEqual(c.get("/api/v1/system/exports")["items"][0]["id"], e["id"])
        st, hd, whole = c.request("GET", e["url"])
        self.assertEqual(st, 200)
        self.assertEqual(len(whole), e["size"])
        self.assertIn("attachment", hd["content-disposition"])
        self.assertEqual(hd["accept-ranges"], "bytes")
        self.assertNotIn("SMRK", whole[:4096].decode("latin-1"))  # never the key
        st, hd, part = c.request("GET", e["url"], headers={"Range": "bytes=1000-"})
        self.assertEqual(st, 206)
        self.assertEqual(part, whole[1000:])
        self.assertEqual(hd["content-range"], f"bytes 1000-{e['size'] - 1}/{e['size']}")
        st, _, _ = c.request("GET", e["url"], headers={"Range": f"bytes={e['size']}-"})
        self.assertEqual(st, 416)
        self.assertGreater(c.get(f"/api/v1/system/exports/{e['id']}")["sent_bytes"], 0)
        # the file the dev server made restores on a fresh dev server
        fresh = Running(needs_setup=True)
        try:
            f = _claimed(fresh)
            j = f.json("POST", "/api/v1/setup/restore", {
                "name": e["file_name"], "size": len(whole), "recovery_key": DEV_KEY,
                "header": base64.b64encode(whole[:65536]).decode()}, expect=201)
            self.assertEqual(j["state"], "uploading")
        finally:
            fresh.close()
        self.assertTrue(c.json("DELETE", f"/api/v1/system/exports/{e['id']}")["deleted"])
        c.error("GET", f"/api/v1/system/exports/{e['id']}", 404, "no_such_export")


    def test_progress_phases_checksum_and_cancel(self):
        c = self.c
        e = c.json("POST", "/api/v1/system/exports", {"password": PW}, expect=201)
        self.assertEqual((e["phase"], e["progress"]["step"], e["progress"]["steps"]), ("counting", 1, 3))
        self.assertIsNone(e["progress"]["bytes_total"])
        self.advance(600)
        e = c.get(f"/api/v1/system/exports/{e['id']}")
        self.assertEqual((e["phase"], e["progress"]["step"]), ("waiting", 2))
        self.advance(1_200)
        e = c.get(f"/api/v1/system/exports/{e['id']}")
        p = e["progress"]
        self.assertEqual(e["phase"], "collecting")
        self.assertTrue(0 < p["percent"] < 100 and p["bytes_done"] < p["bytes_total"], p)
        self.assertGreater(p["bytes_per_s"], 0)
        self.assertIsNotNone(p["eta_s"])
        self.assertFalse(p["stalled"])
        self.advance(1_500)
        e = c.get(f"/api/v1/system/exports/{e['id']}")
        self.assertEqual((e["state"], e["checksum"]["state"], e["sha256"]), ("ready", "working", None))
        self.assertIsNone(e["progress"])
        self.advance(EXPORT_SHA_MS)
        e = c.get(f"/api/v1/system/exports/{e['id']}")
        self.assertEqual(e["checksum"]["state"], "done")
        _, _, whole = c.request("GET", e["url"])
        self.assertEqual(e["sha256"], hashlib.sha256(whole).hexdigest())
        # cancelled while it is made
        c.json("DELETE", f"/api/v1/system/exports/{e['id']}")
        e = c.json("POST", "/api/v1/system/exports", {"password": PW}, expect=201)
        r = c.json("DELETE", f"/api/v1/system/exports/{e['id']}")
        self.assertEqual((r["deleted"], r["cancelled"]), (True, True))
        self.assertEqual(c.get("/api/v1/system/exports")["items"], [])


class SlowExport(ServerCase):
    server_args = {"export_profile": "stall"}

    def test_a_slow_export_that_stops_says_it_stalled(self):
        c = self.c
        e = c.json("POST", "/api/v1/system/exports", {"password": PW}, expect=201)
        self.server.app.clock.advance(6_000 + 30_000)
        p = c.get(f"/api/v1/system/exports/{e['id']}")["progress"]
        self.assertEqual(p["phase"], "collecting")
        self.assertGreater(p["bytes_total"], 10 * 1024 ** 3)  # gigabytes: x of y GB
        self.assertFalse(p["stalled"])
        self.server.app.clock.advance(18_000 + 61_000)  # stopped at 40 %, a minute ago
        e = c.get(f"/api/v1/system/exports/{e['id']}")
        self.assertEqual((e["state"], e["progress"]["percent"], e["progress"]["stalled"]), ("preparing", 40.0, True))
        self.assertEqual(e["progress"]["bytes_per_s"], 0)
        self.assertIsNone(e["progress"]["eta_s"])
        self.server.app.clock.advance(10 * MIN)
        self.assertEqual(c.get(f"/api/v1/system/exports/{e['id']}")["state"], "preparing")  # never further
        c.error("POST", "/api/v1/system/exports", 409, "export_preparing", body={"password": PW})
        self.assertTrue(c.json("DELETE", f"/api/v1/system/exports/{e['id']}")["cancelled"])


class FailedExport(ServerCase):
    server_args = {"export_profile": "fail"}

    def test_a_failed_export_says_why_and_what_to_do(self):
        c = self.c
        e = c.json("POST", "/api/v1/system/exports", {"password": PW}, expect=201)
        self.server.app.clock.advance(6_000 + 22_500)
        e = c.get(f"/api/v1/system/exports/{e['id']}")
        self.assertEqual((e["state"], e["phase"], e["problem"]["code"]), ("failed", "failed", "no_space"))
        self.assertIn("space", e["problem"]["what_to_do"])
        self.assertEqual(e["error"], e["problem"]["message"])
        c.error("GET", f"/api/v1/system/exports/{e['id']}/file", 404, "export_gone")
        # a failed one does not hold up a new one
        c.json("POST", "/api/v1/system/exports", {"password": PW}, expect=201)


class ExportPresets(ServerCase):
    def test_each_preset_starts_where_it_says(self):
        for state, want in (("preparing", "collecting"), ("stalled", "collecting"), ("failed", "failed"),
                            ("ready", "ready")):
            r = Running(export_state=state)
            try:
                items = r.client().get("/api/v1/system/exports")["items"]
                self.assertEqual(items[0]["phase"], want, state)
                if state == "stalled":
                    self.assertTrue(items[0]["progress"]["stalled"])
                if state == "ready":
                    self.assertEqual(items[0]["checksum"]["state"], "done")
            finally:
                r.close()


class Presets(ServerCase):
    server_args = {"move_state": "moved"}

    def test_start_moved(self):
        g = self.c.get("/api/v1/system/move")
        self.assertEqual(g["state"], "moved")
        self.assertEqual(g["moved"]["to"], "192.168.1.30")


class RecoveryKeyLikeTheServer(ServerCase):
    """/system/recovery-key answers as the real server's (crates/web/src/recovery.rs)."""

    def test_saved_takes_no_fields(self):
        e, _ = self.c.error("POST", "/api/v1/system/recovery-key/saved", 400, "bad_input", body={"a": 1})
        self.assertEqual(e["fields"], {"a": "Unknown field."})
        self.assertEqual(self.c.json("POST", "/api/v1/system/recovery-key/saved", {}), {"saved": True})
        self.assertEqual(self.c.json("POST", "/api/v1/system/recovery-key/saved"), {"saved": True})

    def test_one_new_key_at_a_time(self):
        out = []

        def one():
            c = self.server.client()
            c.json("POST", "/api/v1/session", {"name": "alex", "password": PW})
            out.append(c)
        for _ in range(3):
            one()
        answers = []

        def new(c):
            answers.append(c.request("POST", "/api/v1/system/recovery-key/new", {"password": PW}))
        ts = [threading.Thread(target=new, args=(c,)) for c in out]
        for t in ts:
            t.start()
        for t in ts:
            t.join()
        got = sorted(st for st, _, _ in answers)
        self.assertEqual(got[0], 201, got)
        self.assertIn(503, got)
        for st, hd, raw in answers:
            if st == 503:
                self.assertEqual(json.loads(raw)["error"]["code"], "busy")
                self.assertEqual(hd.get("retry-after"), "30")

    def test_no_new_key_while_an_import_runs(self):
        st = self.server.app.store
        with st.lock:
            st.setup["job"] = {"id": "job-test", "kind": "restore", "started_ms": NOW, "from": None,
                               "file_name": "x.smexport", "size": 1000, "received": 0, "uploaded_ms": None,
                               "failed": None}
        try:
            self.c.error("POST", "/api/v1/system/recovery-key/new", 409, "setup_busy", body={"password": PW})
        finally:
            with st.lock:
                st.setup["job"] = None
        self.c.json("POST", "/api/v1/system/recovery-key/new", {"password": PW}, expect=201)


class RecoveryKeyMoved(ServerCase):
    server_args = {"move_state": "moved"}

    def test_a_moved_server_makes_no_new_key(self):
        self.c.error("POST", "/api/v1/system/recovery-key/new", 409, "moved", body={"password": PW})
        # the file is still shown and removed
        st, _, _ = self.c.request("POST", "/api/v1/system/recovery-key/show", {"password": PW})
        self.assertIn(st, (200, 409))
        self.assertEqual(self.c.json("POST", "/api/v1/system/recovery-key/saved"), {"saved": True})


class Bridge(ServerCase):
    def test_new_token_gives_the_three_app_fields(self):
        d = self.c.json("POST", "/api/v1/upload-tokens", expect=201)
        self.assertEqual(d["app"]["token"], d["secret"])
        self.assertTrue(d["app"]["address"].startswith("https://") and d["app"]["address"].endswith("/api/can/logs"))
        self.assertRegex(d["app"]["certificate_sha256"], r"^([0-9A-F]{2}:){31}[0-9A-F]{2}$")
        n = self.c.get("/api/v1/network")
        self.assertEqual(n["certificate"]["sha256"], d["app"]["certificate_sha256"])
        self.assertEqual(n["upload_address"], d["app"]["address"])
        d = self.c.json("POST", "/api/v1/cars/c1/upload-tokens", expect=201)
        self.assertEqual(d["app"]["token"], d["secret"])
