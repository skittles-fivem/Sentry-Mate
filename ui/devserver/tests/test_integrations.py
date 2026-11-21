"""Integrations: MQTT settings (checks, version, write-only password), status, the preview of what a
broker would get (units, places, location only when turned on, Home Assistant entities), the
connection test and the list of outside connections."""

from __future__ import annotations

from .helpers import ServerCase


def topics(preview):
    return {m["topic"]: m["payload"] for m in preview["messages"]}


class Mqtt(ServerCase):
    def put(self, **change):
        cur = self.c.get("/api/v1/settings/mqtt")
        body = {k: v for k, v in cur.items() if k not in ("password", "password_set")}
        body.update(change)
        return self.c.json("PUT", "/api/v1/settings/mqtt", body)

    def test_1_read(self):
        m = self.c.get("/api/v1/settings/mqtt")
        self.assertIsNone(m["password"])  # write-only
        self.assertTrue(m["password_set"])
        self.assertEqual((m["host"], m["port"], m["tls"]), ("broker.local", 8883, True))
        s = self.c.get("/api/v1/settings/mqtt/status")
        self.assertTrue(s["connected"])
        for k in ("delivered", "waiting", "dropped", "last_error", "last_error_ms"):
            self.assertIn(k, s)

    def test_2_checks(self):
        cur = self.c.get("/api/v1/settings/mqtt")
        e, _ = self.c.error("PUT", "/api/v1/settings/mqtt", 400, "bad_input",
                            body={"version": cur["version"], "enabled": "yes", "colour": 1})
        for f in ("enabled", "colour", "host", "namespace"):
            self.assertIn(f, e["fields"])
        body = {k: v for k, v in cur.items() if k not in ("password", "password_set")}
        e, _ = self.c.error("PUT", "/api/v1/settings/mqtt", 409, "stale_version", body=dict(body, version=99))
        self.assertEqual(e["version"], cur["version"])
        e, _ = self.c.error("PUT", "/api/v1/settings/mqtt", 422, "invalid",
                            body=dict(body, tls=False, namespace="cars/#", port=70000, pin_sha256="12ab",
                                      ha_discovery={"enabled": True, "prefix": "/ha", "url": "http://ha.local"}))
        for f in ("tls", "namespace", "port", "pin_sha256", "ha_discovery.prefix", "ha_discovery.url"):
            self.assertIn(f, e["fields"])
        # a password is only sent over TLS
        e, _ = self.c.error("PUT", "/api/v1/settings/mqtt", 422, "invalid", body=dict(body, tls=False))
        self.assertIn("tls", e["fields"])
        self.assertEqual(self.c.get("/api/v1/settings/mqtt")["version"], cur["version"])  # nothing saved

    def test_3_save_and_preview(self):
        before = topics(self.c.get("/api/v1/settings/mqtt/preview?car=c1"))
        self.assertEqual(before["sentrymate/status"], "online")
        self.assertEqual(before["sentrymate/cars/c1/state"], "driving")
        self.assertEqual(before["sentrymate/cars/c1/latitude"], "")  # location is off
        d = self.put(namespace="garage/tesla", publish_location=True)
        self.assertEqual(d["version"], 2)
        self.assertTrue(d["password_set"])  # left out: kept
        after = topics(self.c.get("/api/v1/settings/mqtt/preview?car=c1"))
        self.assertTrue(all(t.startswith("garage/tesla/") for t in after))
        self.assertNotEqual(after["garage/tesla/cars/c1/latitude"], "")
        self.assertIn('"latitude":', after["garage/tesla/cars/c1/location"])
        p = self.c.get("/api/v1/settings/mqtt/preview?car=c1")
        ids = {x["id"] for x in p["ha_entities"]}
        self.assertIn("garage_tesla_c1_location", ids)
        self.assertTrue(all(x["config_topic"].startswith("homeassistant/") for x in p["ha_entities"]))
        # values come in the server's units
        km = float(after["garage/tesla/cars/c1/range"])
        self.c.json("PATCH", "/api/v1/settings", {"version": self.c.get("/api/v1/settings")["version"],
                                                  "units": {"distance": "mi"}})
        mi = float(topics(self.c.get("/api/v1/settings/mqtt/preview?car=c1"))["garage/tesla/cars/c1/range"])
        self.assertAlmostEqual(mi, km / 1.609344, delta=1.0)
        unit = next(x["unit"] for x in self.c.get("/api/v1/settings/mqtt/preview?car=c1")["ha_entities"]
                    if x["id"].endswith("_range"))
        self.assertEqual(unit, "mi")
        # the place of a charging car, and its charge figures
        c2 = topics(self.c.get("/api/v1/settings/mqtt/preview?car=c2"))
        self.assertEqual(c2["garage/tesla/cars/c2/place"], "Work garage")
        self.assertEqual(c2["garage/tesla/cars/c2/charging"], "true")
        # a password set to null is removed
        d = self.put(password=None)
        self.assertFalse(d["password_set"])
        audit = [a["action"] for a in self.c.get("/api/v1/audit?limit=10")["items"]]
        self.assertIn("mqtt.update", audit)
        self.assertIn("settings.update", audit)

    def test_4_test_and_status(self):
        ok = self.c.json("POST", "/api/v1/settings/mqtt/test")
        self.assertTrue(ok["ok"])
        self.assertIsNotNone(ok["latency_ms"])
        self.c.error("POST", "/api/v1/settings/mqtt/test", 400, "bad_input", body={"host": "other"})
        self.put(host="fail.example")
        bad = self.c.json("POST", "/api/v1/settings/mqtt/test")
        self.assertEqual((bad["ok"], bad["error"]), (False, "connection_refused"))
        s = self.c.get("/api/v1/settings/mqtt/status")
        self.assertFalse(s["connected"])
        self.assertEqual(s["last_error"], "connection_refused")
        self.assertIsNotNone(s["last_error_ms"])
        self.put(enabled=False, host="broker.local")
        s = self.c.get("/api/v1/settings/mqtt/status")
        self.assertEqual((s["enabled"], s["connected"], s["waiting"]), (False, False, 0))
        net = {x["purpose"]: x for x in self.c.get("/api/v1/network")["outbound"]}
        self.assertFalse(net["mqtt"]["enabled"])
        self.assertIsNone(net["mqtt"]["last_used_ms"])

    def test_5_network(self):
        net = self.c.get("/api/v1/network")
        out = {x["purpose"]: x for x in net["outbound"]}
        self.assertEqual(set(out), {"geocoding", "tiles", "geonames_download", "mqtt", "release_check", "tesla"})
        self.assertFalse(out["geocoding"]["enabled"])  # offline address lookup: nothing leaves
        self.assertEqual(out["tiles"]["host"], "tile.openstreetmap.org")
        self.assertEqual({x["purpose"] for x in net["inbound"]}, {"web", "upload"})
        v = self.c.get("/api/v1/settings")["version"]
        self.c.json("PATCH", "/api/v1/settings", {"version": v, "release_check": {"enabled": True},
                                                  "geocode": {"mode": "online", "url": "https://geo.example/rev"}})
        out = {x["purpose"]: x for x in self.c.get("/api/v1/network")["outbound"]}
        self.assertTrue(out["release_check"]["enabled"])
        self.assertEqual((out["geocoding"]["host"], out["geocoding"]["enabled"]), ("geo.example", True))


class WatchIntegrations(ServerCase):
    server_args = {"role": "watch"}

    def test_watch(self):
        self.c.error("GET", "/api/v1/settings/mqtt", 403, "not_allowed")
        self.c.error("PUT", "/api/v1/settings/mqtt", 403, "not_allowed", body={})
        self.c.error("POST", "/api/v1/settings/mqtt/test", 403, "not_allowed")
        self.c.error("GET", "/api/v1/network", 403, "not_allowed")
        self.assertIn("connected", self.c.get("/api/v1/settings/mqtt/status"))
        p = self.c.get("/api/v1/settings/mqtt/preview")
        self.assertEqual(p["car"], "c1")
        self.c.error("GET", "/api/v1/settings/mqtt/preview?car=c2", 404, "no_such_car")
