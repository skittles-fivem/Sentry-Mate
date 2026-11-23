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
        self.assertEqual(c2["garage/tesla/cars/c2/charge_status"], "charging")
        self.assertEqual(c2["garage/tesla/cars/c2/plugged_in"], "true")
        self.assertIn("garage/tesla/cars/c2/charger_current", c2)
        self.assertNotIn("garage/tesla/cars/c2/energy_at_destination", c2)  # only while navigating
        ents = {x["id"]: x for x in self.c.get("/api/v1/settings/mqtt/preview?car=c2")["ha_entities"]}
        self.assertTrue(ents["garage_tesla_c2_doors_open"]["config_topic"].startswith("homeassistant/binary_sensor/"))
        self.assertEqual(ents["garage_tesla_c2_charge_energy_added"]["unit"], "kWh")
        # a password set to null is removed
        d = self.put(password=None)
        self.assertFalse(d["password_set"])
        audit = [a["action"] for a in self.c.get("/api/v1/audit?limit=10")["items"]]
        self.assertIn("mqtt-changed", audit)
        self.assertIn("settings-changed", audit)

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
        self.assertEqual(set(out), {"geocoding", "tiles", "geonames_download", "mqtt", "release_check", "tesla",
                                    "teslafi", "teslamate_mqtt", "teslamate_db"})
        self.assertFalse(out["geocoding"]["enabled"])  # offline address lookup: nothing leaves
        self.assertEqual(out["tiles"]["host"], "tile.openstreetmap.org")
        self.assertEqual(out["tiles"]["hosts"], ["tile.openstreetmap.org", "server.arcgisonline.com"])
        self.assertEqual({x["purpose"] for x in net["inbound"]}, {"web", "upload"})
        v = self.c.get("/api/v1/settings")["version"]
        self.c.json("PATCH", "/api/v1/settings", {"version": v, "release_check": {"enabled": True},
                                                  "geocode": {"mode": "online", "url": "https://geo.example/rev"}})
        out = {x["purpose"]: x for x in self.c.get("/api/v1/network")["outbound"]}
        self.assertTrue(out["release_check"]["enabled"])
        self.assertEqual((out["geocoding"]["host"], out["geocoding"]["enabled"]), ("geo.example", True))


class NewCertificate(ServerCase):
    """POST /network/certificate: the password again, the saved host names in it, a new SHA-256 in /network,
    refused while a move code waits and with the owner's own certificate, recorded in the audit log."""

    def test_new_certificate(self):
        before = self.c.get("/api/v1/network")
        old = before["certificate"]["sha256"]
        # the real server's certificate always names localhost first, from the first start on
        self.assertEqual(before["certificate"]["names"][0], "localhost")
        self.assertEqual(before["certificate"]["own"], False)
        self.assertEqual(self.c.get("/api/v1/settings")["host_names"], before["certificate"]["names"][1:])
        self.assertRegex(old, r"^([0-9A-F]{2}:){31}[0-9A-F]{2}$")
        self.assertTrue(before["upload_address"].startswith("https://"))
        v = self.c.get("/api/v1/settings")["version"]
        self.c.json("PATCH", "/api/v1/settings", {"version": v, "host_names": ["nas.example", "192.0.2.57"]})
        self.assertEqual(self.c.get("/api/v1/network")["certificate"]["sha256"], old, "saving names changes nothing")
        path = "/api/v1/network/certificate"
        # no password at all: 400, as the real server answers a missing field
        e, _ = self.c.error("POST", path, 400, "bad_input", body={})
        self.assertEqual(e["fields"], {"password": "Required."})
        self.c.error("POST", path, 403, "wrong_password", body={"password": "nope"})
        e, _ = self.c.error("POST", path, 400, "bad_input", body={"password": "sentry-mate-dev", "names": []})
        self.assertIn("names", e["fields"])
        made = self.c.json("POST", path, {"password": "sentry-mate-dev"})
        self.assertEqual(made["names"], ["localhost", "nas.example", "192.0.2.57"])
        self.assertEqual(made["upload_address"], "https://nas.example:10100/api/can/logs")
        self.assertRegex(made["sha256"], r"^([0-9A-F]{2}:){31}[0-9A-F]{2}$")
        self.assertNotEqual(made["sha256"], old)
        net = self.c.get("/api/v1/network")
        self.assertEqual(net["certificate"], {"names": made["names"], "sha256": made["sha256"], "own": False})
        self.assertEqual(net["upload_address"], made["upload_address"])
        again = self.c.json("POST", path, {"password": "sentry-mate-dev"})
        self.assertNotEqual(again["sha256"], made["sha256"], "a new one each time")
        audit = self.c.get("/api/v1/audit?limit=5")["items"]
        self.assertEqual((audit[0]["action"], audit[0]["target"]), ("certificate-made", "server:certificate"))
        # a move code waiting: the new server would pin this certificate
        self.c.json("POST", "/api/v1/system/move", {"password": "sentry-mate-dev"}, expect=201)
        self.c.error("POST", path, 409, "move_under_way", body={"password": "sentry-mate-dev"})
        self.c.json("DELETE", "/api/v1/system/move")
        # the owner's own certificate stays: /network says so, and it is refused before the password is checked
        self.server.app.store.cert_own = True
        try:
            self.assertEqual(self.c.get("/api/v1/network")["certificate"]["own"], True)
            self.c.error("POST", path, 409, "own_certificate", body={"password": "nope"})
            e, _ = self.c.error("POST", path, 409, "own_certificate", body={"password": "sentry-mate-dev"})
            self.assertNotIn("remove those two files", e["message"])
        finally:
            self.server.app.store.cert_own = False
        self.c.json("POST", path, {"password": "sentry-mate-dev"})


class WatchIntegrations(ServerCase):
    server_args = {"role": "watch"}

    def test_watch(self):
        self.c.error("GET", "/api/v1/settings/mqtt", 403, "not_allowed")
        self.c.error("PUT", "/api/v1/settings/mqtt", 403, "not_allowed", body={})
        self.c.error("POST", "/api/v1/settings/mqtt/test", 403, "not_allowed")
        self.c.error("GET", "/api/v1/network", 403, "not_allowed")
        self.c.error("POST", "/api/v1/network/certificate", 403, "not_allowed", body={"password": "sentry-mate-dev"})
        self.assertIn("connected", self.c.get("/api/v1/settings/mqtt/status"))
        p = self.c.get("/api/v1/settings/mqtt/preview")
        self.assertEqual(p["car"], "c1")
        self.c.error("GET", "/api/v1/settings/mqtt/preview?car=c2", 404, "no_such_car")


API_TOPICS = ["version", "update_available", "update_version", "update_download", "locked", "windows_open",
              "window_driver_front", "window_passenger_front", "window_driver_rear", "window_passenger_rear",
              "sunroof_state", "sunroof_open", "scheduled_charge_start", "scheduled_departure", "route_destination",
              "route_distance", "route_minutes", "route_traffic_delay"]


class OffCanTopics(ServerCase):
    """The topics an API input gives: where each value came from, stale ones published empty, the car's inputs,
    and a Home Assistant entity for each."""

    def preview(self, car):
        p = self.c.get(f"/api/v1/settings/mqtt/preview?car={car}")
        ns = self.c.get("/api/v1/settings/mqtt")["namespace"]
        return p, {m["topic"][len(f"{ns}/cars/{car}/"):]: m["payload"] for m in p["messages"]
                   if m["topic"].startswith(f"{ns}/cars/{car}/")}

    def test_tesla_api_car(self):
        p, t = self.preview("c1")
        self.assertEqual([i["kind"] for i in p["inputs"]], ["smo", "tesla_api"])
        self.assertTrue(all(set(i) == {"kind", "label"} for i in p["inputs"]))
        self.assertEqual(t["locked"], "false")
        self.assertEqual(t["update_available"], "true")
        self.assertEqual(t["update_version"], "2026.38.4")
        self.assertEqual(t["update_download"], "64")
        self.assertRegex(t["scheduled_charge_start"], r"^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\+00:00$")
        self.assertIn("route_destination", t)
        self.assertNotIn("sunroof_state", t)  # no input gives it: not published
        self.assertNotIn("sunroof_state", p["sources"])
        s = p["sources"]["locked"]
        self.assertEqual(set(s), {"kind", "label", "at_ms", "stale"})
        self.assertEqual((s["kind"], s["label"], s["stale"]), ("tesla_api", "Data from the Tesla API", False))
        self.assertTrue(set(p["sources"]) <= set(API_TOPICS) | {"installing"})
        self.assertIn("since", t)
        self.assertEqual(t["trim"], "Long Range AWD")
        self.assertNotIn("vin", t)

    def test_stale_is_published_empty(self):
        p, t = self.preview("c2")
        self.assertEqual(t["scheduled_departure"], "")
        self.assertTrue(p["sources"]["scheduled_departure"]["stale"])
        self.assertIn(t["sunroof_state"], ("vent", "closed"))
        self.assertNotIn("update_version", t)

    def test_teslafi_car(self):
        p, t = self.preview("c3")
        self.assertIn({"kind": "teslafi_api", "label": "Data from TeslaFi"}, p["inputs"])
        self.assertEqual(p["sources"]["window_driver_rear"]["kind"], "teslafi_api")
        self.assertEqual(t["windows_open"], "false")

    def test_smo_only_car(self):
        p, t = self.preview("c0")
        self.assertEqual([i["kind"] for i in p["inputs"]], ["smo"])
        self.assertEqual(p["sources"], {})
        for k in API_TOPICS:
            self.assertNotIn(k, t)

    def test_home_assistant_entities(self):
        cur = self.c.get("/api/v1/settings/mqtt")
        body = {k: v for k, v in cur.items() if k not in ("password", "password_set")}
        body["ha_discovery"] = dict(cur["ha_discovery"], enabled=True)
        self.c.json("PUT", "/api/v1/settings/mqtt", body)
        p, _ = self.preview("c1")
        ents = {e["state_topic"].rsplit("/", 1)[1]: e for e in p["ha_entities"]}
        for k in API_TOPICS + ["since", "trim"]:
            self.assertIn(k, ents)
        self.assertIn("/binary_sensor/", ents["locked"]["config_topic"])
        self.assertEqual(ents["locked"]["device_class"], "lock")
        self.assertEqual(ents["window_driver_front"]["device_class"], "window")
        self.assertIn("/sensor/", ents["scheduled_departure"]["config_topic"])
        self.assertEqual(ents["scheduled_departure"]["device_class"], "timestamp")
        self.assertEqual(ents["update_download"]["unit"], "%")
        self.assertIn(ents["route_distance"]["unit"], ("km", "mi"))
