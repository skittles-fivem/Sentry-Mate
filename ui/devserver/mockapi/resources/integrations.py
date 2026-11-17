"""Integrations (docs/ui-api.md, "Integrations"): MQTT settings, status, a preview of what a broker
would get, a connection test, and every network connection the server makes.

The preview follows the server's MQTT publisher (crates/mqtt): one retained topic per value under
`<namespace>/cars/<car>/`, events on `<namespace>/cars/<car>/event`, values as text in the server's
units, nothing published for a value the car did not send, position topics empty unless the owner
turns location publishing on. The VIN is never published. Nothing here leaves the dev server.
"""

from __future__ import annotations

import re

from ..errors import bad_input, invalid
from ..live import now_object
from ..util import h01, iso_utc

HOST_RX = re.compile(r"[A-Za-z0-9]([A-Za-z0-9-]{0,61}[A-Za-z0-9])?(\.[A-Za-z0-9]([A-Za-z0-9-]{0,61}[A-Za-z0-9])?)*")
PIN_RX = re.compile(r"[0-9A-Fa-f]{64}|([0-9A-Fa-f]{2}:){31}[0-9A-Fa-f]{2}")
FIELDS = ("enabled", "host", "port", "username", "password", "tls", "pin_sha256", "namespace", "publish_location",
          "ha_discovery", "no_place_text")


def mqtt_view(m):
    out = {k: m[k] for k in FIELDS if k != "password"}
    out["password"] = None  # write-only
    out["password_set"] = bool(m.get("password_set"))
    out["ha_discovery"] = dict(m["ha_discovery"])
    out["version"] = m["version"]
    return out


def get_mqtt(req):
    return mqtt_view(req.store.mqtt)


def _check_prefix(v):
    """A topic prefix a broker and Home Assistant both take."""
    if not isinstance(v, str) or not v:
        return "Must be text."
    if len(v) > 200:
        return "At most 200 characters."
    if "+" in v or "#" in v:
        return "No + or # (they are wildcards)."
    if v.startswith("$"):
        return "Topics starting with $ belong to the broker."
    if any(ord(c) < 32 for c in v) or any(not p for p in v.split("/")):
        return "No empty level (a / at either end, or //) and no control characters."
    return None


def put_mqtt(req):
    st = req.store
    cur = st.mqtt
    body = req.json()
    req.version_check(cur["version"])
    bad, rule = {}, {}
    for k in body:
        if k not in FIELDS + ("version",):
            bad[k] = "Unknown field."
    for k in FIELDS:
        if k not in body and k != "password":
            bad[k] = "Required (PUT sends the whole setting; leave out only password to keep it)."
    for k in ("enabled", "tls", "publish_location"):
        if k in body and not isinstance(body[k], bool):
            bad[k] = "Must be true or false."
    for k in ("host", "namespace", "no_place_text"):
        if k in body and not isinstance(body[k], str):
            bad[k] = "Must be text."
    for k in ("username", "password", "pin_sha256"):
        if k in body and body[k] is not None and not isinstance(body[k], str):
            bad[k] = "Must be text or null."
    if "port" in body and (not isinstance(body["port"], int) or isinstance(body["port"], bool)):
        bad["port"] = "Must be a whole number."
    ha = body.get("ha_discovery")
    if "ha_discovery" in body:
        if not isinstance(ha, dict):
            bad["ha_discovery"] = "Must be { enabled, prefix, url }."
        else:
            for k in ha:
                if k not in ("enabled", "prefix", "url"):
                    bad[f"ha_discovery.{k}"] = "Unknown field."
            if not isinstance(ha.get("enabled"), bool):
                bad["ha_discovery.enabled"] = "Must be true or false."
            if not isinstance(ha.get("prefix"), str):
                bad["ha_discovery.prefix"] = "Must be text."
            if ha.get("url") is not None and not isinstance(ha.get("url"), str):
                bad["ha_discovery.url"] = "Must be text or null."
    if bad:
        raise bad_input(bad)
    if body["enabled"] and not HOST_RX.fullmatch(body["host"] or ""):
        rule["host"] = "A host name such as broker.local."
    elif body["host"] and not HOST_RX.fullmatch(body["host"]):
        rule["host"] = "A host name such as broker.local."
    if not 1 <= body["port"] <= 65535:
        rule["port"] = "Must be between 1 and 65535."
    if body["pin_sha256"] is not None and not PIN_RX.fullmatch(body["pin_sha256"].strip()):
        rule["pin_sha256"] = "The broker certificate's SHA-256: 64 hex digits, with or without colons."
    if body["pin_sha256"] and not body["tls"]:
        rule["pin_sha256"] = "A certificate pin needs TLS."
    for k, v in (("namespace", body["namespace"]), ("ha_discovery.prefix", ha["prefix"])):
        why = _check_prefix(v)
        if why:
            rule[k] = why
    if ha["url"] is not None and not re.fullmatch(r"https://[^\s/]+(/\S*)?", ha["url"]):
        rule["ha_discovery.url"] = "An https:// address, or null."
    if len(body["no_place_text"]) > 40:
        rule["no_place_text"] = "At most 40 characters."
    if body["username"] is not None and not 1 <= len(body["username"]) <= 128:
        rule["username"] = "1 to 128 characters, or null."
    has_pw = body.get("password") if "password" in body else (cur.get("password_set") or None)
    if has_pw and not body["tls"]:
        rule["tls"] = "A password is only ever sent over TLS. Turn TLS on, or remove the password."
    if rule:
        raise invalid(rule)
    with st.lock:
        for k in FIELDS:
            if k == "password":
                continue
            cur[k] = dict(body[k]) if k == "ha_discovery" else body[k]
        if "password" in body:
            cur["password_set"] = bool(body["password"])
            cur["_password"] = body["password"]
        cur["pin_sha256"] = cur["pin_sha256"].strip() if cur["pin_sha256"] else None
        cur["version"] += 1
        stt = cur["status"]
        stt["connected"] = bool(cur["enabled"]) and not cur["host"].startswith("fail")
        if not cur["enabled"]:
            stt["waiting"] = 0
    what = "on" if body["enabled"] else "off"
    st.record(req.account, "mqtt.update", "settings:mqtt",
              f"Changed the MQTT settings (publishing {what}, broker {body['host']}:{body['port']}).")
    return mqtt_view(cur)


def mqtt_status(req):
    m = req.store.mqtt
    s = m["status"]
    on = m["enabled"]
    # the dev server pretends a broker takes about one message a second while connected
    ticks = max(0, (req.now - req.app.cfg.now_ms) // 1000)
    return {"enabled": on, "connected": bool(on and s["connected"]),
            "delivered": s["delivered"] + (ticks if on and s["connected"] else 0),
            "waiting": s["waiting"] if on else 0, "dropped": s["dropped"],
            "last_error": s["last_error"], "last_error_ms": s["last_error_ms"],
            "broker": f"{m['host']}:{m['port']}" if m["host"] else None, "tls": m["tls"]}


# ------------------------------------------------------------------ preview

ENTITIES = [  # key, name, unit kind, device class
    ("state", "State", None, "enum"), ("driving", "Driving", None, "moving"),
    ("charging", "Charging", None, "battery_charging"), ("place", "Place", None, None),
    ("soc", "Battery", "%", "battery"), ("range", "Range", "distance", "distance"),
    ("speed", "Speed", "speed", "speed"), ("power", "Power", "kW", "power"),
    ("charge_power", "Charge power", "kW", "power"), ("charge_kind", "Charger", None, "enum"),
    ("energy_remaining", "Energy remaining", "kWh", "energy_storage"),
    ("battery_temp", "Battery temperature", "temperature", "temperature"),
    ("outside_temp", "Outside temperature", "temperature", "temperature"),
    ("inside_temp", "Inside temperature", "temperature", "temperature"),
    ("tire_pressure_fl", "Tire pressure front left", "pressure", "pressure"),
    ("tire_pressure_fr", "Tire pressure front right", "pressure", "pressure"),
    ("tire_pressure_rl", "Tire pressure rear left", "pressure", "pressure"),
    ("tire_pressure_rr", "Tire pressure rear right", "pressure", "pressure"),
    ("odometer", "Odometer", "distance", "distance"), ("gear", "Gear", None, "enum"),
    ("lv_voltage", "12 V battery", "V", "voltage"), ("updated", "Last data", None, "timestamp"),
]


def _units(req):
    u = req.store.server_settings["units"]
    return u


def _unit_text(kind, u):
    return {"distance": "mi" if u["distance"] == "mi" else "km", "speed": "mph" if u["distance"] == "mi" else "km/h",
            "temperature": "°F" if u["temperature"] == "f" else "°C",
            "pressure": "psi" if u["pressure"] == "psi" else "bar"}.get(kind, kind)


def _num(v, d):
    if v is None:
        return None
    x = round(float(v), d)
    return str(int(x)) if d == 0 else (str(x) if x != 0 else "0.0")


def preview(req):
    st = req.store
    m = st.mqtt
    car = req.q("car") or next((c for c in req.ds.car_ids_featured() if st.sees(req.account, c)), None)
    if car is None:
        return {"car": None, "messages": [], "ha_entities": []}
    if not req.ds.exists(car) or not st.sees(req.account, car):
        from ..errors import no_such_car
        raise no_such_car()
    u = _units(req)
    mi = u["distance"] == "mi"
    n = now_object(req.app, car, req.now)

    def v(w):
        return None if w is None else w["value"]

    def dist(x):
        return None if x is None else (x / 1.609344 if mi else x)

    def temp(x):
        return None if x is None else (x * 9 / 5 + 32 if u["temperature"] == "f" else x)

    def pres(kpa):
        if kpa is None:
            return None
        bar = kpa / 100
        return bar * 14.5038 if u["pressure"] == "psi" else bar

    ch = n.get("charge") or {}
    charging = v(ch.get("status")) == "charging"
    state = "driving" if n.get("driving") else "charging" if charging else (
        "online" if n["state"] in ("parked",) else n["state"])
    loc = n.get("location") or {}
    place = (loc.get("place") or {}).get("name")
    tyres = n.get("tyres_kpa") or [None] * 4
    vals = {
        "state": state, "driving": "true" if n.get("driving") else "false",
        "charging": None if ch.get("status") is None else ("true" if charging else "false"),
        "place": place or m["no_place_text"],
        "soc": _num(v(n.get("soc_pct")), 1), "range": _num(dist(v(n.get("range_km"))), 1),
        "speed": _num(dist(v(n.get("speed_kph"))), 1), "power": _num(v(n.get("power_kw")), 2),
        "charge_power": _num(v(ch.get("power_kw")), 2), "charge_kind": v(ch.get("kind")),
        "energy_remaining": _num(v(n.get("energy_remaining_kwh")), 2),
        "battery_temp": _num(temp(v(n.get("battery_c"))), 1), "outside_temp": _num(temp(v(n.get("outside_c"))), 1),
        "inside_temp": _num(temp(v(n.get("cabin_c"))), 1),
        "tire_pressure_fl": _num(pres(v(tyres[0])), 2 if u["pressure"] == "bar" else 1),
        "tire_pressure_fr": _num(pres(v(tyres[1])), 2 if u["pressure"] == "bar" else 1),
        "tire_pressure_rl": _num(pres(v(tyres[2])), 2 if u["pressure"] == "bar" else 1),
        "tire_pressure_rr": _num(pres(v(tyres[3])), 2 if u["pressure"] == "bar" else 1),
        "odometer": _num(dist(v(n.get("odometer_km"))), 1), "gear": v(n.get("gear")),
        "lv_voltage": _num(v(n.get("lv_v")), 2),
        "updated": iso_utc(n["at_ms"]).replace("Z", "+00:00") if n.get("at_ms") else None,
    }
    lat, lon = v(loc.get("lat")), v(loc.get("lon"))
    if m["publish_location"] and lat is not None:
        vals["latitude"] = _num(lat, 6)
        vals["longitude"] = _num(lon, 6)
        acc = v(loc.get("accuracy_m"))
        vals["location"] = ('{"latitude":%s,"longitude":%s%s}' %
                            (_num(lat, 6), _num(lon, 6), f',"gps_accuracy":{round(acc)}' if acc is not None else ""))
    else:
        vals.update({"latitude": "", "longitude": "", "location": ""})
    ns = m["namespace"]
    msgs = [{"topic": f"{ns}/status", "payload": "online", "retained": True}]
    for k, p in vals.items():
        if p is None:
            continue  # not known: nothing is published, the broker keeps the last value
        msgs.append({"topic": f"{ns}/cars/{car}/{k}", "payload": p, "retained": True})
    node = re.sub(r"[^A-Za-z0-9_-]", "_", ns) + f"_{car}"
    ents = []
    if m["ha_discovery"]["enabled"]:
        for key, name, unit, dc in ENTITIES:
            ents.append({"id": f"{node}_{key}", "name": name,
                         "unit": _unit_text(unit, u) if unit else None, "device_class": dc,
                         "state_topic": f"{ns}/cars/{car}/{key}",
                         "config_topic": f"{m['ha_discovery']['prefix']}/"
                                         f"{'binary_sensor' if key in ('driving', 'charging') else 'sensor'}"
                                         f"/{node}/{key}/config"})
        if m["publish_location"]:
            ents.append({"id": f"{node}_location", "name": "Location", "unit": None, "device_class": None,
                         "state_topic": f"{ns}/cars/{car}/location",
                         "config_topic": f"{m['ha_discovery']['prefix']}/device_tracker/{node}/location/config"})
        ents.append({"id": f"{node}_event", "name": "Event", "unit": None, "device_class": None,
                     "state_topic": f"{ns}/cars/{car}/event",
                     "config_topic": f"{m['ha_discovery']['prefix']}/event/{node}/event/config",
                     "event_types": ["drive_started", "drive_ended", "charge_started", "charge_ended"]})
    meta = st.car_meta(car)
    return {"car": car, "name": meta["name"], "units": u, "messages": msgs, "ha_entities": ents,
            "device": {"name": meta["name"], "model": f"{meta['model']} {meta['year']}", "identifiers": [node]}}


def test(req):
    st = req.store
    m = st.mqtt
    body = req.json(required=False)
    if body:  # the test uses the saved settings
        raise bad_input({k: "Unknown field (the test uses the saved settings)." for k in body})
    host = m["host"]
    t0 = req.now
    if not host:
        res = {"ok": False, "error": "no_host", "message": "Set a broker host first.", "latency_ms": None}
    elif host.startswith("fail") or host.endswith(".invalid"):
        res = {"ok": False, "error": "connection_refused",
               "message": f"{host}:{m['port']} refused the connection.", "latency_ms": None}
        with st.lock:
            m["status"]["last_error"] = "connection_refused"
            m["status"]["last_error_ms"] = t0
    elif m["tls"] and m["pin_sha256"] and m["pin_sha256"].lower().startswith("00"):
        res = {"ok": False, "error": "pin_mismatch",
               "message": "The broker's certificate does not match the pinned SHA-256.", "latency_ms": None}
    else:
        res = {"ok": True, "error": None, "message": f"Connected to {host}:{m['port']} and published one test message.",
               "latency_ms": 8 + int(h01("mqtt", t0 // 1000) * 30),
               "topic": f"{m['namespace']}/test", "tls": m["tls"]}
    st.record(req.account, "mqtt.test", "settings:mqtt",
              "Tested the MQTT broker: " + ("connected." if res["ok"] else res["error"] + "."))
    res["at_ms"] = t0
    return res


# ------------------------------------------------------------------ network


def network(req):
    st = req.store
    ss = st.server_settings
    m = st.mqtt
    now = req.now
    geo_online = ss["geocode"]["mode"] == "online"
    geo_host = None
    if ss["geocode"].get("url"):
        geo_host = re.sub(r"^https?://([^/:]+).*$", r"\1", ss["geocode"]["url"])
    tiles_online = ss["tiles"]["mode"] == "online"
    tile_host = re.sub(r"^https?://([^/:]+).*$", r"\1", ss["tiles"]["online_url"]) if ss["tiles"].get(
        "online_url") else None
    return {
        "outbound": [
            {"purpose": "geocoding", "host": geo_host, "enabled": geo_online and bool(geo_host),
             "last_used_ms": now - 40 * 60_000 if geo_online and geo_host else None},
            {"purpose": "tiles", "host": tile_host, "enabled": tiles_online,
             "last_used_ms": now - 3 * 60_000 if tiles_online else None},
            {"purpose": "geonames_download", "host": "download.geonames.org", "enabled": False,
             "last_used_ms": None},
            {"purpose": "mqtt", "host": m["host"] or None, "enabled": bool(m["enabled"]),
             "last_used_ms": now - 1500 if m["enabled"] else None},
            {"purpose": "release_check", "host": "api.github.com", "enabled": bool(ss["release_check"]["enabled"]),
             "last_used_ms": now - 6 * 3_600_000 if ss["release_check"]["enabled"] else None},
        ],
        "inbound": [{"purpose": "web", "port": 443, "tls": True}, {"purpose": "upload", "port": 8443, "tls": True}],
    }


def register(router):
    router.add("/api/v1/settings/mqtt", GET=get_mqtt, PUT=put_mqtt, admin=("PUT",))
    router.add("/api/v1/settings/mqtt/status", GET=mqtt_status)
    router.add("/api/v1/settings/mqtt/preview", GET=preview)
    router.add("/api/v1/settings/mqtt/test", POST=test, admin=("POST",))
    router.add("/api/v1/network", GET=network, admin=("GET",))
