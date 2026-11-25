"""Unsigned CAN logs of a paired car (docs/security.md, "Pairing and the owner's word"): the made-up logs of c1 say
why each is unsigned; an admin uses those that failed only for being unsigned, the password asked again, never a
bad or another key's log, and stops using them; each change is in the audit log; a watch account may not."""

from __future__ import annotations

from .helpers import ServerCase

PW = "sentry-mate-dev"
LOGS = "/api/v1/cars/c1/logs?limit=500"
USE = "/api/v1/cars/c1/logs/use-unsigned"


class UnsignedLogs(ServerCase):
    def rows(self):
        return {r["name"]: r for r in self.c.get(LOGS)["items"]}

    def tearDown(self):
        # every test starts with nothing used
        self.c.json("DELETE", USE, {"all": True, "confirm_password": PW})

    def test_each_unsigned_log_says_why(self):
        rows = list(self.rows().values())
        kinds = sorted({r["unsigned_kind"] for r in rows if r["unsigned_kind"]})
        self.assertEqual(kinds, ["could_not_sign", "no_integrity", "no_sidecar", "signed_false"])
        for r in rows:
            if r["unsigned_kind"]:
                self.assertIn(r["result"], ("failed", "could_not_sign"))
                self.assertFalse(r["used"] or r["override_use"])
            if r["result"] == "verified":
                self.assertIsNone(r["unsigned_kind"])
        bad = [r for r in rows if r["result"] == "failed" and r["unsigned_kind"] is None]
        self.assertEqual(sorted(r["reason"] for r in bad), ["signed by another key", "the signature does not match"])
        u = self.c.get("/api/v1/cars/c1/logs/unsigned")
        self.assertEqual((u["can_use"], u["can_use_before_first_signed"], u["in_use"], u["never"]), (11, 9, 0, 2))
        self.assertEqual(u["kinds"], {"no_integrity": 7, "no_sidecar": 1, "signed_false": 2, "could_not_sign": 1})
        self.assertEqual(len(u["items"]), 11)
        # the other cars have none
        self.assertEqual(self.c.get("/api/v1/cars/c2/logs")["items"], [])
        self.assertEqual(self.c.get("/api/v1/cars/c2/logs/unsigned")["can_use"], 0)

    def test_the_password_is_asked_and_bad_logs_are_refused(self):
        self.c.error("POST", USE, 403, "password_needed", body={"all": True})
        self.c.error("POST", USE, 403, "wrong_password", body={"all": True, "confirm_password": "not it"})
        self.c.error("POST", USE, 400, "bad_input", body={"confirm_password": PW})
        self.c.error("POST", USE, 400, "bad_input", body={"all": True, "names": [], "confirm_password": PW})
        rows = self.rows()
        bad = [n for n, r in rows.items() if r["result"] == "failed" and r["unsigned_kind"] is None]
        good = [n for n, r in rows.items() if r["unsigned_kind"]]
        e, _ = self.c.error("POST", USE, 422, "invalid", body={"names": [good[0], *bad], "confirm_password": PW})
        for n in bad:
            self.assertIn(n, e["fields"]["names"])
        self.assertFalse(self.rows()[good[0]]["override_use"], "nothing changed")
        self.c.error("POST", USE, 422, "invalid", body={"names": ["no-such-log"], "confirm_password": PW})

    def test_used_then_stopped_and_audited(self):
        before = sum(1 for a in self.walk("/api/v1/audit?group=cars", limit=100)
                     if a["action"] == "can-logs-unsigned-used")
        out = self.c.json("POST", USE, {"all": True, "before_first_signed": True, "confirm_password": PW})
        self.assertEqual(out["changed"], 9)
        self.assertEqual(out["kinds"], {"no_integrity": 6, "no_sidecar": 1, "signed_false": 1, "could_not_sign": 1})
        rows = self.rows()
        for n in out["names"]:
            self.assertTrue(rows[n]["override_use"] and rows[n]["used"] and rows[n]["before_first_signed"])
        self.assertFalse(any(r["used"] for r in rows.values() if r["result"] == "failed" and not r["unsigned_kind"]))
        u = self.c.get("/api/v1/cars/c1/logs/unsigned")
        self.assertEqual((u["can_use"], u["in_use"]), (2, 9))
        # again: nothing more to change
        self.assertEqual(self.c.json("POST", USE, {"names": out["names"][:1], "confirm_password": PW})["changed"], 0)
        audit = [a for a in self.walk("/api/v1/audit?group=cars", limit=100) if a["action"] == "can-logs-unsigned-used"]
        self.assertEqual(len(audit), before + 1)
        self.assertEqual(audit[0]["car"], "c1")
        self.assertIn("9 unsigned CAN logs", audit[0]["summary"])
        # back
        self.c.error("DELETE", USE, 403, "password_needed", body={"all": True})
        out = self.c.json("DELETE", USE, {"names": out["names"][:2], "confirm_password": PW})
        self.assertEqual(out["changed"], 2)
        self.assertEqual(self.c.get("/api/v1/cars/c1/logs/unsigned")["in_use"], 7)
        stopped = [a for a in self.walk("/api/v1/audit?group=cars", limit=100)
                   if a["action"] == "can-logs-unsigned-stopped"]
        self.assertIn("2 unsigned CAN logs", stopped[0]["summary"])

    def test_a_watch_account_may_not(self):
        w = self.server.client()
        w.cookie = "sentry-mate=out"
        w.json("POST", "/api/v1/session", {"name": "watch", "password": PW})
        w.error("POST", USE, 403, "not_allowed", body={"all": True, "confirm_password": PW})
        w.error("GET", "/api/v1/cars/c1/logs/unsigned", 403, "not_allowed")
        self.c.error("POST", "/api/v1/cars/c99/logs/use-unsigned", 404, "no_such_car",
                     body={"all": True, "confirm_password": PW})
