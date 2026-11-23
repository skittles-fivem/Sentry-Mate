"""The local TeslaMate input (docs/ui-api.md, "Integrations: Local TeslaMate"): the owner's own TeslaMate on the local
network. The server subscribes to its MQTT topics (read only, never a publish) and, when an admin turns it on,
catches up from its PostgreSQL database now and then.

Everything is in memory and nothing leaves the dev server: a connection is "tried" by its look alone. A host that
contains `refused` plays a host that refuses the connection (`connection_refused`), one with `timeout` a host that
does not answer (`timeout`); a user name `wrong` plays wrong credentials (`auth_failed`); a pin that starts with `00`
plays a certificate with another fingerprint (`pin_mismatch`); a database named `postgres` has no TeslaMate tables
(`not_teslamate`).

The plain text rule: a password (new or kept) with `tls: false` needs `allow_unencrypted: true`
(`422 plaintext_password`); without a password plain TCP is allowed. A kept password goes only to the host it was
typed for (`422 password_resend`). Adding, replacing, pausing or removing the settings asks for the admin's own
password again (`confirm_password` or the `X-Sentry-Mate-Password` header) and, with two-step sign-in on, a code (the
dev code is 000000), like TeslaFi; a test and the car matches take the admin session alone. No answer ever holds a
password.
"""

from __future__ import annotations

import re

from ..errors import ApiError, bad_input, conflict, invalid, not_found
from ..util import DAY, HOUR
from .tesla import _only, reconfirm

HOST_RX = re.compile(r"[A-Za-z0-9]([A-Za-z0-9-]{0,61}[A-Za-z0-9])?(\.[A-Za-z0-9]([A-Za-z0-9-]{0,61}[A-Za-z0-9])?)*"
                     r"|\[?[0-9A-Fa-f:]+\]?")
PIN_RX = re.compile(r"[0-9A-Fa-f]{64}|([0-9A-Fa-f]{2}:){31}[0-9A-Fa-f]{2}")
MQTT_KEYS = ("host", "port", "tls", "pin_sha256", "username", "password", "prefix", "allow_unencrypted")
DB_KEYS = ("enabled", "host", "port", "database", "username", "password", "tls", "pin_sha256", "allow_unencrypted",
           "interval_s")
ERRORS = {
    "connection_refused": "The host refused the connection: check the host and port, and that it listens there.",
    "timeout": "No answer from the host in 10 s: check the host, and that this server can reach it.",
    "auth_failed": "The user name or password was not taken.",
    "tls_failed": "The TLS handshake failed: the host may not offer TLS on this port.",
    "pin_mismatch": "The host's certificate does not have the pinned fingerprint.",
    "not_teslamate": "The database has no TeslaMate tables: check the database name.",
    "query_failed": "A query on the database failed.",
}


def _mqtt_default():
    return {"host": None, "port": 1883, "tls": False, "pin_sha256": None, "username": None, "password": None,
            "prefix": "teslamate", "allow_unencrypted": False}


def _db_default():
    return {"enabled": False, "host": None, "port": 5432, "database": "teslamate", "username": "teslamate",
            "password": None, "tls": False, "pin_sha256": None, "allow_unencrypted": False, "interval_s": 300}


def _seed_cars(now):
    # TeslaMate's own car ids; the VIN prefix only (the database's `cars` table has the VIN: the serial is dropped)
    return [
        {"teslamate_car": "1", "display_name": "Made-up Car", "vin_prefix": "5YJ3E1EA0N", "match": None,
         "seen_ms": now - 4_000},
        {"teslamate_car": "2", "display_name": "Made-up Roadster", "vin_prefix": "5YJRE1A10B", "match": "leave_out",
         "seen_ms": now - 3 * DAY},
    ]


def seed(st, now):
    """Added by an admin: MQTT on the made-up broker (plain TCP, no password), the database catch-up off, two cars
    seen on MQTT: one not matched yet (Maple, `c2`, is suggested: the same VIN prefix), one left out."""
    st.teslamate = {
        "enabled": True, "paused": False, "added_ms": now - 9 * DAY, "version": 4,
        "mqtt": dict(_mqtt_default(), host="192.0.2.10"),
        "database": dict(_db_default(), host="192.0.2.10"),
        "mqtt_error": None, "db_error": None,
        "cars": _seed_cars(now),
    }


def seed_trouble(st, trouble, now):
    """--teslamate-trouble: the input starts in that trouble (an hour ago; quiet: for a day), its notice up."""
    if trouble == "none":
        return
    if trouble == "catchup_failed":
        st.teslamate["database"]["enabled"] = True
        st.teslamate["db_error"] = ("query_failed", now - HOUR)
    elif trouble == "quiet":
        st.teslamate["mqtt_quiet"] = now - DAY
    else:
        st.teslamate["mqtt_error"] = (trouble, now - HOUR)


def _t(req):
    return req.store.teslamate


# ------------------------------------------------------------------ views


def _settings_view(s, keys):
    out = {k: s[k] for k in keys if k != "password"}
    out["password_set"] = s.get("password") is not None
    return out


def _suggest(req, c):
    if c["match"] is not None or not c["vin_prefix"]:
        return None
    taken = {x["match"] for x in _t(req)["cars"] if x is not c}
    for cid in req.ds.car_ids_featured():
        m = req.store.car_meta(cid)
        vp = m.get("vin_prefix") or ""
        if cid not in taken and not m.get("sold") and vp[:len(c["vin_prefix"])] == c["vin_prefix"]:
            return cid
    return None


def _matched(c):
    return c["match"] not in (None, "new", "leave_out")


def car_view(req, c):
    return {"teslamate_car": c["teslamate_car"], "display_name": c["display_name"], "vin_prefix": c["vin_prefix"],
            "match": c["match"], "suggested": _suggest(req, c), "last_message_ms": c["seen_ms"]}


def _err(code, at_ms):
    return {"code": code, "message": ERRORS[code], "at_ms": at_ms} if code else None


def _mqtt_status(req, t):
    now = req.now
    base = {"state": "off", "connected_since_ms": None, "last_message_ms": None, "messages_today": 0,
            "last_error": None}
    if not t["enabled"]:
        return base
    if t["paused"]:
        return dict(base, state="paused", last_message_ms=max((c["seen_ms"] for c in t["cars"]), default=None))
    if t["mqtt_error"]:
        code, at_ms = t["mqtt_error"]
        return dict(base, state="error", last_error=_err(code, at_ms))
    if t.get("mqtt_quiet"):
        # connected, nothing since
        return dict(base, state="connected", connected_since_ms=t["mqtt_quiet"] - DAY,
                    last_message_ms=t["mqtt_quiet"], messages_today=0)
    last = now - now % 5_000 - 1_000
    midnight = now - now % DAY
    return dict(base, state="connected", connected_since_ms=max(t["added_ms"], now - 3 * HOUR - now % HOUR),
                last_message_ms=last, messages_today=int((now - midnight) / 1000 * 0.8))


def _db_status(req, t):
    now = req.now
    d = t["database"]
    base = {"state": "off", "last_sync_ms": None, "next_sync_ms": None, "drives": 0, "charges": 0, "positions": 0,
            "last_error": None}
    if not t["enabled"] or not d["enabled"]:
        return base
    if t["paused"]:
        return dict(base, state="paused")
    if t["db_error"]:
        code, at_ms = t["db_error"]
        step = d["interval_s"] * 1000
        return dict(base, state="error", last_error=_err(code, at_ms), next_sync_ms=now - now % step + step)
    step = d["interval_s"] * 1000
    last = now - now % step
    return dict(base, state="ok", last_sync_ms=last, next_sync_ms=last + step, drives=1_284, charges=611,
                positions=2_406_118)


def _hosts(t):
    if not t["enabled"]:
        return []
    out = [f"{t['mqtt']['host']}:{t['mqtt']['port']}"]
    if t["database"]["enabled"]:
        out.append(f"{t['database']['host']}:{t['database']['port']}")
    return out


def view(req):
    t = _t(req)
    return {
        "enabled": t["enabled"], "paused": t["paused"],
        "mqtt": _settings_view(t["mqtt"], MQTT_KEYS), "database": _settings_view(t["database"], DB_KEYS),
        "status": {"mqtt": _mqtt_status(req, t), "database": _db_status(req, t)},
        "cars": [car_view(req, c) for c in t["cars"]], "hosts": _hosts(t), "version": t["version"],
    }


# ------------------------------------------------------------------ checking a draft


def _check_one(part, cur, body, keys, fields):
    """The settings `body` names (a whole object: a field left out is its default, but the password: a string sets
    it, null removes it, left out keeps `cur`'s), checked; field errors go to `fields` as `part.key`."""
    if not isinstance(body, dict):
        raise bad_input({part: "Must be an object."})
    extra = sorted(set(body) - set(keys))
    if extra:
        raise bad_input({f"{part}.{k}": "Unknown field." for k in extra})
    new = dict(_mqtt_default() if part == "mqtt" else _db_default(), password=cur["password"])
    for k in keys:
        if k in body:
            new[k] = body[k]
    f = lambda k, msg: fields.setdefault(f"{part}.{k}", msg)  # noqa: E731
    for k in ("tls", "allow_unencrypted") + (("enabled",) if part == "database" else ()):
        if not isinstance(new[k], bool):
            f(k, "Must be true or false.")
    host = new["host"]
    on = part == "mqtt" or new.get("enabled") is True
    if host is not None and not isinstance(host, str):
        f("host", "Must be text.")
    elif isinstance(host, str):
        host = new["host"] = host.strip() or None
        if host and ("://" in host or "/" in host or not HOST_RX.fullmatch(host)):
            f("host", "A host name or an IP address, without http:// or a path (for example 192.0.2.10).")
    if on and not new["host"]:
        f("host", "Required.")
    if not isinstance(new["port"], int) or isinstance(new["port"], bool) or not 1 <= new["port"] <= 65535:
        f("port", "A port from 1 to 65535.")
    for k in ("username",) + (("database",) if part == "database" else ()):
        v = new[k]
        if v is not None and not isinstance(v, str):
            f(k, "Must be text.")
        elif isinstance(v, str):
            new[k] = v.strip() or None
            if len(v) > 128:
                f(k, "At most 128 characters.")
    if part == "database" and on and not new["database"]:
        f("database", "Required (TeslaMate's is teslamate).")
    pw = new["password"]
    if pw is not None and (not isinstance(pw, str) or not pw or len(pw) > 256):
        f("password", "Must be text, 1 to 256 characters (null removes it).")
    pin = new["pin_sha256"]
    if pin is not None:
        if not isinstance(pin, str) or not PIN_RX.fullmatch(pin.strip()):
            f("pin_sha256", "The certificate's SHA-256 fingerprint: 64 hex digits, colons allowed.")
        else:
            new["pin_sha256"] = pin.strip().replace(":", "").lower()
            if new["tls"] is False:
                f("pin_sha256", "A pin needs TLS: turn TLS on, or leave the pin empty.")
    if part == "mqtt":
        p = new["prefix"]
        if not isinstance(p, str) or not p:
            f("prefix", "Required (TeslaMate's default is teslamate).")
        elif "+" in p or "#" in p:
            f("prefix", "No + or # (they are wildcards).")
        elif p.startswith("$"):
            f("prefix", "A prefix must not start with $ (those topics belong to the broker).")
        elif any(not x for x in p.split("/")) or len(p) > 200:
            f("prefix", "No empty level (a / at either end, or //), at most 200 characters.")
    else:
        iv = new["interval_s"]
        if not isinstance(iv, int) or isinstance(iv, bool) or not 60 <= iv <= 86_400:
            f("interval_s", "Seconds between catch-ups, from 60 to 86400.")
    return new


def _rules(part, cur, new, body, saved_set, out):
    """The two password rules (the 422 codes), once the shapes are fine."""
    if "password" not in body and saved_set and any(new[k] != cur[k] for k in ("host", "port", "tls")):
        out.append(("password_resend", f"{part}.password",
                    "A saved password goes only to the host it was typed for: type it again for the new host, port "
                    "or TLS setting, or remove it."))
        return
    if new["password"] is not None and new["tls"] is False and not new["allow_unencrypted"]:
        out.append(("plaintext_password", f"{part}.password",
                    "A password over plain TCP can be read by anyone on the network: turn TLS on, or tick \"Allow "
                    "unencrypted (local network only)\"."))


def _draft(t, body, parts):
    """{part: settings} for the parts in `body` (the saved ones for the others), checked; raises the errors."""
    fields = {}
    out = {}
    for part, keys in (("mqtt", MQTT_KEYS), ("database", DB_KEYS)):
        if part in parts and body.get(part) is not None:
            out[part] = _check_one(part, t[part], body[part], keys, fields)
        else:
            out[part] = dict(t[part])
    if fields:
        raise invalid(fields)
    rules = []
    for part in ("mqtt", "database"):
        if part in parts and body.get(part) is not None:
            _rules(part, t[part], out[part], body[part], t[part]["password"] is not None, rules)
    if rules:
        code = rules[0][0]
        raise ApiError(422, code, rules[0][2], fields={p: m for c, p, m in rules if c == code})
    return out


def _try(s, part):
    """What connecting with `s` would give: an error code or None."""
    host = s["host"] or ""
    if "refused" in host:
        return "connection_refused"
    if "timeout" in host:
        return "timeout"
    if s["pin_sha256"] and s["pin_sha256"].startswith("00"):
        return "pin_mismatch"
    if (s["username"] or "") == "wrong":
        return "auth_failed"
    if part == "database" and s["database"] == "postgres":
        return "not_teslamate"
    return None


# ------------------------------------------------------------------ handlers


def get(req):
    return view(req)


def put(req):
    body = req.json()
    _only(body, ("version", "mqtt", "database", "confirm_password", "code"))
    if not isinstance(body.get("mqtt"), dict):
        raise bad_input({"mqtt": "Required: the MQTT settings."})
    t = _t(req)
    st = req.store
    with st.lock:
        req.version_check(t["version"])
    new = _draft(t, body, ("mqtt", "database"))
    reconfirm(req, body)
    with st.lock:
        req.version_check(t["version"])
        added = not t["enabled"]
        t["mqtt"], t["database"] = new["mqtt"], new["database"]
        t["mqtt_error"] = (code, req.now) if (code := _try(new["mqtt"], "mqtt")) else None
        t["mqtt_quiet"] = None
        t["db_error"] = ((code, req.now) if (code := _try(new["database"], "database")) else None) \
            if new["database"]["enabled"] else None
        if added:
            t.update(enabled=True, paused=False, added_ms=req.now, version=1, cars=_seed_cars(req.now))
            for c in t["cars"]:
                c["match"] = None
        else:
            t["version"] += 1
        st.rev += 1
    m = new["mqtt"]
    db = new["database"]
    where = f"MQTT {m['host']}:{m['port']}" + (f", database {db['host']}:{db['port']}" if db["enabled"] else "")
    if added:
        st.record(req.account, "teslamate-local-added", "teslamate:local", f"Added the local TeslaMate ({where}).")
    else:
        st.record(req.account, "teslamate-local-changed", "teslamate:local",
                  f"Changed the local TeslaMate's settings ({where}).")
    out = view(req)
    if added:
        out["_created"] = True
    return out


def _added(t):
    if not t["enabled"]:
        raise not_found("teslamate_not_added", "No local TeslaMate is added.")


def patch(req):
    body = req.json()
    _only(body, ("version", "paused", "confirm_password", "code"))
    paused = body.get("paused")
    if not isinstance(paused, bool):
        raise bad_input({"paused": "Must be true or false."})
    t = _t(req)
    _added(t)
    reconfirm(req, body)
    st = req.store
    with st.lock:
        req.version_check(t["version"])
        if paused == t["paused"]:
            return view(req)
        t["paused"] = paused
        t["version"] += 1
        st.rev += 1
    if paused:
        st.record(req.account, "teslamate-local-paused", "teslamate:local",
                  "Paused the local TeslaMate: nothing is read from it.")
    else:
        st.record(req.account, "teslamate-local-resumed", "teslamate:local", "Resumed the local TeslaMate.")
    return view(req)


def delete(req):
    body = req.json(required=False)
    _only(body, ("confirm_password", "code"))
    reconfirm(req, body)
    st = req.store
    t = _t(req)
    with st.lock:
        was = t["enabled"]
        st.teslamate = {"enabled": False, "paused": False, "added_ms": None, "version": 0, "mqtt": _mqtt_default(),
                        "database": _db_default(), "mqtt_error": None, "db_error": None, "cars": []}
        st.rev += 1
    if was:
        st.record(req.account, "teslamate-local-removed", "teslamate:local",
                  "Removed the local TeslaMate: its sealed settings are deleted; the history already stored stays.")
    return {"deleted": True}


def test(req):
    body = req.json()
    _only(body, ("target", "mqtt", "database"))
    target = body.get("target")
    if target not in ("mqtt", "database"):
        raise bad_input({"target": "Must be mqtt or database."})
    t = _t(req)
    draft = body.get(target)
    if draft is None and not t["enabled"]:
        raise bad_input({target: "Send the settings to try: nothing is saved yet."})
    if isinstance(draft, dict) and target == "database":
        draft = dict(draft, enabled=True)
    s = _draft(t, {target: draft}, (target,))[target]
    if target == "database" and draft is None and not s["host"]:
        raise bad_input({"database": "Send the settings to try: no database is saved yet."})
    code = _try(s, target)
    lat = 3 + len(s["host"] or "") % 7
    if code:
        return {"ok": False, "error": code, "message": ERRORS[code], "latency_ms": None, "at_ms": req.now,
                "cars_seen": []}
    seen = [c["teslamate_car"] for c in (t["cars"] or _seed_cars(req.now))]
    msg = (f"Connected and subscribed to {s['prefix']}/cars/+/#: {len(seen)} cars seen in 3 s."
           if target == "mqtt" else f"Connected: the database has TeslaMate's tables and {len(seen)} cars.")
    return {"ok": True, "error": None, "message": msg, "latency_ms": lat, "at_ms": req.now, "cars_seen": seen}


def get_cars(req):
    t = _t(req)
    return {"items": [car_view(req, c) for c in t["cars"]], "version": t["version"]}


def put_cars(req):
    body = req.json()
    _only(body, ("version", "items"))
    t = _t(req)
    _added(t)
    rows = body.get("items")
    if not isinstance(rows, list) or not all(isinstance(x, dict) for x in rows):
        raise bad_input({"items": "Required: a list of { teslamate_car, match }."})
    st = req.store
    known = {c["teslamate_car"]: c for c in t["cars"]}
    shape = 'Must be a car id, "new", "leave_out" or null.'
    bad, changes = {}, []
    for i, x in enumerate(rows):
        extra = set(x) - {"teslamate_car", "match"}
        if extra:
            raise bad_input({f"items.{i}.{k}": "Unknown field." for k in extra})
        v, m = x.get("teslamate_car"), x.get("match")
        if not isinstance(v, str):
            raise bad_input({f"items.{i}.teslamate_car": "Required: TeslaMate's car id (1)."})
        if m is not None and not isinstance(m, str):
            raise bad_input({f"items.{i}.match": shape})
        if v not in known:
            bad[f"items.{i}.teslamate_car"] = "TeslaMate has no such car here."
            continue
        if any(v == c for c, _ in changes):
            bad[f"items.{i}.teslamate_car"] = "Named twice."
            continue
        if m not in (None, "new", "leave_out"):
            if not re.fullmatch(r"c\d+", m):
                raise bad_input({f"items.{i}.match": shape})
            if not st.sees(req.account, m) or not req.ds.exists(m):
                bad[f"items.{i}.match"] = "No such car."
                continue
        changes.append((v, m))
    if bad:
        raise invalid(bad)
    with st.lock:
        req.version_check(t["version"])
        after = {c["teslamate_car"]: c["match"] for c in t["cars"]}
        after.update(dict(changes))
        cars_here = [m for m in after.values() if m not in (None, "new", "leave_out")]
        if len(cars_here) != len(set(cars_here)):
            raise conflict("car_taken", "That car is already matched to another TeslaMate car.")
        what = {None: "is not matched to a car: it is not read", "new":
                "gets a new car of its own: it is read from now on", "leave_out": "is left out: it is not read"}
        changed = False
        for v, m in changes:
            c = known[v]
            if c["match"] == m:
                continue
            c["match"] = m
            changed = True
            st.record(req.account, "teslamate-local-car-matched", f"teslamate:local:{v}",
                      f"TeslaMate car {c['display_name'] or v} " + what.get(m, f"is car {m}: it is read from now on")
                      + ".", car=m if m not in what else None)
        if changed:
            t["version"] += 1
            st.rev += 1
    return get_cars(req)


# ------------------------------------------------------------------ for other resources


def network_rows(req):
    t = _t(req)
    m, d = t["mqtt"], t["database"]
    on = t["enabled"] and not t["paused"]
    mqtt_last = _mqtt_status(req, t)["last_message_ms"] if on else None
    db_last = _db_status(req, t)["last_sync_ms"] if on and d["enabled"] else None
    return [
        {"purpose": "teslamate_mqtt", "host": f"{m['host']}:{m['port']}" if m["host"] else None, "enabled": on,
         "last_used_ms": mqtt_last},
        {"purpose": "teslamate_db", "host": f"{d['host']}:{d['port']}" if d["host"] else None,
         "enabled": on and d["enabled"], "last_used_ms": db_last},
    ]


def health(req):
    t = _t(req)
    ms, ds = _mqtt_status(req, t), _db_status(req, t)
    return {"enabled": t["enabled"], "mqtt_state": ms["state"], "database_state": ds["state"],
            "cars": sum(1 for c in t["cars"] if _matched(c) or c["match"] == "new"),
            "last_message_ms": ms["last_message_ms"], "last_sync_ms": ds["last_sync_ms"]}


# a notice per trouble: (code, audit action, part, words); the words take the error's own
NOTICES = {
    "connection_refused": ("teslamate_connection_refused", "teslamate-local-connection-refused", "mqtt",
                           "The server cannot reach local TeslaMate's MQTT broker ({why}): its live values are not "
                           "read. Check the host and port, and that the broker runs; the server tries again by "
                           "itself."),
    "auth_failed": ("teslamate_auth_failed", "teslamate-local-auth-failed", "mqtt",
                    "Local TeslaMate's MQTT broker refused the user name or password ({why}): nothing is read from it "
                    "until you give the right ones."),
    "pin_mismatch": ("teslamate_pin_mismatch", "teslamate-local-pin-mismatch", "mqtt",
                     "The certificate of local TeslaMate's MQTT broker does not have the pinned SHA-256 fingerprint, "
                     "so the server does not connect. If the broker's certificate was renewed, pin the new one."),
    "quiet": ("teslamate_quiet", "teslamate-local-quiet", "mqtt",
              "Local TeslaMate's MQTT broker is connected but has sent nothing for {hours} hours: check that "
              "TeslaMate runs and publishes to this broker under the topic prefix set (a car asleep for days sends "
              "nothing either)."),
    "catchup_failed": ("teslamate_catchup_failed", "teslamate-local-catchup-failed", "database",
                       "The catch-up from local TeslaMate's database failed ({why}). It is tried again at the next "
                       "interval; live values from MQTT still come."),
}


def _troubles(t, now):
    """[(trouble, error code or None, since_ms)] of the input now, MQTT's first (as the server's Status::problems)."""
    out = []
    if not t["enabled"] or t["paused"]:
        return out
    if t["mqtt_error"]:
        code, at_ms = t["mqtt_error"]
        trouble = code if code in ("auth_failed", "pin_mismatch") else "connection_refused"
        out.append((trouble, code, at_ms))
    elif t.get("mqtt_quiet"):
        out.append(("quiet", None, t["mqtt_quiet"]))
    if t["database"]["enabled"] and t["db_error"]:
        code, at_ms = t["db_error"]
        out.append(("catchup_failed", code, at_ms))
    return out


def notices(req):
    """For admins: the local TeslaMate in trouble (its broker refused, a password refused, a pin that does not
    match, the broker gone quiet, a catch-up failed), each linking to Settings: Integrations (#teslamate)."""
    out = []
    for trouble, code, since in _troubles(_t(req), req.now):
        ncode, _, part, words = NOTICES[trouble]
        why = ERRORS[code].rstrip(".") if code else ""
        why = why[:1].lower() + why[1:]
        msg = words.format(why=why, hours=max(0, (req.now - since) // HOUR))
        out.append({"code": ncode, "level": "warning", "message": msg, "since_ms": since,
                    "fix_link": "/settings/integrations#teslamate", "detail": {"part": part, "error": code}})
    return out


def fail(st, part, code, now):
    """For tests and the dev data: the MQTT broker or the database starts failing with `code` (for MQTT, `quiet`: it
    stays connected and sends nothing from now on); None ends it. A trouble that begins writes its audit entry."""
    with st.lock:
        t = st.teslamate
        before = {x[0] for x in _troubles(t, now)}
        if part == "mqtt" and code == "quiet":
            t["mqtt_error"], t["mqtt_quiet"] = None, now
        elif part == "mqtt":
            t["mqtt_error"], t["mqtt_quiet"] = ((code, now) if code else None), None
        else:
            t["db_error"] = (code, now) if code else None
        new = [x for x in _troubles(t, now) if x[0] not in before]
        st.rev += 1
    for trouble, _, _ in new:
        st.record(None, NOTICES[trouble][1], "settings:teslamate_local", f"Local TeslaMate: {trouble.replace('_', ' ')}.")


def register(router):
    admin_all = ("GET", "POST", "PUT", "PATCH", "DELETE")
    router.add("/api/v1/integrations/teslamate", GET=get, PUT=put, PATCH=patch, DELETE=delete, admin=admin_all)
    router.add("/api/v1/integrations/teslamate/test", POST=test, admin=admin_all)
    router.add("/api/v1/integrations/teslamate/cars", GET=get_cars, PUT=put_cars, admin=admin_all)


__all__ = ["register", "seed", "network_rows", "health", "fail", "notices"]
