"""The local TeslaMate input (docs/ui-api.md, "Integrations: Local TeslaMate"): admin only, the password asked again,
the made-up broker and cars, the plain text rule, the kept password, tests, matching, pause, removal, audit entries,
the network rows, health, and that no answer ever holds a password. Every host and VIN here is made up."""

from __future__ import annotations

import json

from .helpers import ServerCase

PW = "sentry-mate-dev"
BASE = "/api/v1/integrations/teslamate"
SECRET = "made-up-broker-secret"


class Teslamate(ServerCase):
    def no_password(self, data):
        raw = json.dumps(data)
        self.assertNotIn(SECRET, raw)
        self.assertNotIn('"password"', raw)

    def put(self, mqtt, database=None, expect=200, **kw):
        v = self.c.get(BASE)["version"]
        body = {"version": v, "mqtt": mqtt, "confirm_password": PW}
        if database is not None:
            body["database"] = database
        return self.c.json("PUT", BASE, body, expect=expect)

    def put_error(self, status, code, mqtt, database=None):
        v = self.c.get(BASE)["version"]
        body = {"version": v, "mqtt": mqtt, "confirm_password": PW}
        if database is not None:
            body["database"] = database
        e, _ = self.c.error("PUT", BASE, status, code, body=body)
        return e

    def test_1_seeded(self):
        t = self.c.get(BASE)
        self.assertTrue(t["enabled"])
        self.assertEqual((t["mqtt"]["host"], t["mqtt"]["port"], t["mqtt"]["prefix"], t["mqtt"]["password_set"]),
                         ("192.0.2.10", 1883, "teslamate", False))
        self.assertFalse(t["database"]["enabled"])
        self.assertEqual(t["status"]["mqtt"]["state"], "connected")
        self.assertEqual(t["status"]["database"]["state"], "off")
        self.assertEqual(t["hosts"], ["192.0.2.10:1883"])
        cars = {c["teslamate_car"]: c for c in t["cars"]}
        self.assertEqual((cars["1"]["match"], cars["1"]["suggested"]), (None, "c2"))
        self.assertEqual(cars["2"]["match"], "leave_out")
        net = {x["purpose"]: x for x in self.c.get("/api/v1/network")["outbound"]}
        self.assertEqual((net["teslamate_mqtt"]["host"], net["teslamate_mqtt"]["enabled"]), ("192.0.2.10:1883", True))
        self.assertFalse(net["teslamate_db"]["enabled"])
        h = self.c.get("/api/v1/system/health")["teslamate_local"]
        self.assertEqual((h["enabled"], h["mqtt_state"], h["database_state"]), (True, "connected", "off"))
        self.assertEqual(self.c.get("/api/v1/cars/c1/drives?source=teslamate_local")["items"], [])

    def test_2_rules(self):
        m = {"host": "192.0.2.10", "port": 1883, "tls": False, "prefix": "teslamate"}
        self.c.error("PUT", BASE, 403, "password_needed", body={"version": 4, "mqtt": m})
        # a password over plain TCP needs the tick
        e = self.put_error(422, "plaintext_password", dict(m, password=SECRET))
        self.assertIn("mqtt.password", e["fields"])
        e = self.put_error(422, "plaintext_password", m, {"enabled": True, "host": "192.0.2.10", "password": SECRET})
        self.assertIn("database.password", e["fields"])
        self.put_error(422, "invalid", dict(m, pin_sha256="ab" * 32))  # a pin needs TLS
        for bad in ("tm/+", "tm/#", "$SYS", "tm//x", "/tm"):
            self.put_error(422, "invalid", dict(m, prefix=bad))
        self.put_error(422, "invalid", dict(m, host="mqtt://192.0.2.10"))
        t = self.put(dict(m, password=SECRET, allow_unencrypted=True))
        self.no_password(t)
        self.assertTrue(t["mqtt"]["password_set"])
        # kept with the same host: fine; a new host needs it again
        self.put(dict(m, allow_unencrypted=True))
        e = self.put_error(422, "password_resend", dict(m, host="192.0.2.12", allow_unencrypted=True))
        self.assertIn("mqtt.password", e["fields"])
        # a kept password with the tick off again is refused
        self.put_error(422, "plaintext_password", m)
        # with TLS (and a pin) it is fine
        t = self.put(dict(m, port=8883, tls=True, pin_sha256=":".join(["AB"] * 32), password=SECRET))
        self.assertEqual(t["mqtt"]["pin_sha256"], "ab" * 32)
        t = self.put(dict(m, password=None))
        self.assertFalse(t["mqtt"]["password_set"])
        self.no_password(self.c.get("/api/v1/audit?limit=500"))

    def test_3_test_match_pause_remove(self):
        self.c.error("POST", BASE + "/test", 403, "password_needed", body={"target": "mqtt"})
        r = self.c.json("POST", BASE + "/test", {"target": "mqtt", "confirm_password": PW})
        self.assertTrue(r["ok"])
        self.assertEqual(r["cars_seen"], ["1", "2"])
        r = self.c.json("POST", BASE + "/test", {"target": "mqtt", "mqtt": {"host": "refused.example", "port": 1883},
                                                 "confirm_password": PW})
        self.assertEqual((r["ok"], r["error"]), (False, "connection_refused"))
        r = self.c.json("POST", BASE + "/test", {"target": "database", "database": {
            "host": "192.0.2.10", "database": "postgres"}, "confirm_password": PW})
        self.assertEqual(r["error"], "not_teslamate")
        self.c.error("POST", BASE + "/test", 422, "plaintext_password", body={
            "target": "database", "database": {"host": "192.0.2.10", "password": SECRET}, "confirm_password": PW})
        cars = self.c.get(BASE + "/cars")
        self.c.error("PUT", BASE + "/cars", 409, "car_taken", body={
            "version": cars["version"], "items": [{"teslamate_car": "1", "match": "c2"},
                                                  {"teslamate_car": "2", "match": "c2"}], "confirm_password": PW})
        cars = self.c.json("PUT", BASE + "/cars", {"version": cars["version"], "items": [
            {"teslamate_car": "1", "match": "c2"}], "confirm_password": PW})
        self.assertEqual(cars["items"][0]["match"], "c2")
        t = self.c.json("PATCH", BASE, {"version": cars["version"], "paused": True, "confirm_password": PW})
        self.assertEqual((t["paused"], t["status"]["mqtt"]["state"]), (True, "paused"))
        self.c.error("PATCH", BASE, 409, "stale_version", body={"version": 1, "paused": False, "confirm_password": PW})
        self.assertEqual(self.c.json("DELETE", BASE, {"confirm_password": PW}), {"deleted": True})
        t = self.c.get(BASE)
        self.assertEqual((t["enabled"], t["version"], t["status"]["mqtt"]["state"], t["cars"]), (False, 0, "off", []))
        # added again: 201
        t = self.c.json("PUT", BASE, {"version": 0, "mqtt": {"host": "192.0.2.20", "port": 1883},
                                      "confirm_password": PW}, expect=201)
        self.assertTrue(t["enabled"])
        actions = {e["action"] for e in self.c.get("/api/v1/audit?limit=500")["items"]}
        for want in ("teslamate-local-added", "teslamate-local-car-matched", "teslamate-local-paused",
                     "teslamate-local-removed"):
            self.assertIn(want, actions)


class TeslamateNotices(ServerCase):
    def codes(self):
        return [n["code"] for n in self.c.get("/api/v1/notices")["items"] if n["code"].startswith("teslamate_")]

    def test_troubles_are_notices_with_their_link_and_audit(self):
        from mockapi.resources.teslamate import fail
        st, now = self.server.app.store, self.server.app.clock.now()
        self.assertEqual(self.codes(), [])
        for code, want in (("connection_refused", "teslamate_connection_refused"), ("timeout", "teslamate_connection_refused"),
                           ("auth_failed", "teslamate_auth_failed"), ("pin_mismatch", "teslamate_pin_mismatch"),
                           ("quiet", "teslamate_quiet")):
            fail(st, "mqtt", code, now)
            self.assertEqual(self.codes(), [want], code)
            n = [x for x in self.c.get("/api/v1/notices")["items"] if x["code"] == want][0]
            self.assertEqual((n["level"], n["fix_link"], n["detail"]["part"]),
                             ("warning", "/settings/integrations#teslamate", "mqtt"))
            self.assertEqual(n["detail"]["error"], None if code == "quiet" else code)
        self.assertEqual(self.c.get(BASE)["status"]["mqtt"]["state"], "connected", "quiet: still connected")
        fail(st, "mqtt", None, now)
        self.assertEqual(self.codes(), [])
        # the database catch-up: only while it is on
        fail(st, "database", "query_failed", now)
        self.assertEqual(self.codes(), [])
        t = self.c.get(BASE)
        self.c.json("PUT", BASE, {"version": t["version"], "mqtt": {"host": "192.0.2.10", "port": 1883},
                                  "database": {"enabled": True, "host": "192.0.2.10"}, "confirm_password": PW})
        fail(st, "database", "query_failed", now)
        n = [x for x in self.c.get("/api/v1/notices")["items"] if x["code"] == "teslamate_catchup_failed"]
        self.assertEqual((len(n), n[0]["detail"]), (1, {"part": "database", "error": "query_failed"}))
        actions = {e["action"] for e in self.c.get("/api/v1/audit?limit=500")["items"]}
        for want in ("teslamate-local-connection-refused", "teslamate-local-auth-failed", "teslamate-local-pin-mismatch",
                     "teslamate-local-quiet", "teslamate-local-catchup-failed"):
            self.assertIn(want, actions)


class TeslamateNoticesWatch(ServerCase):
    server_args = {"role": "watch"}

    def test_admins_only(self):
        from mockapi.resources.teslamate import fail
        fail(self.server.app.store, "mqtt", "auth_failed", self.server.app.clock.now())
        self.assertFalse([n for n in self.c.get("/api/v1/notices")["items"] if n["code"].startswith("teslamate_")])


class TeslamateWatch(ServerCase):
    server_args = {"role": "watch"}

    def test_admin_only(self):
        for m, path in (("GET", BASE), ("GET", BASE + "/cars"), ("PUT", BASE), ("POST", BASE + "/test"),
                        ("DELETE", BASE)):
            self.c.error(m, path, 403, "not_allowed", body={} if m != "GET" else None)
