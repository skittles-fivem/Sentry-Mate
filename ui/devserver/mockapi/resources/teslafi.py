"""The TeslaFi input (docs/ui-api.md, "Integrations: TeslaFi"), for owners who pay for TeslaFi: TeslaFi accounts by
token, the car TeslaFi polls, what the server does for it.

Everything is in memory and nothing leaves the dev server: a token is "checked" by its look alone. A token that
starts with `refused` plays one TeslaFi refuses (`422 teslafi_refused`: a wrong token, or the subscription ended),
one that starts with `unreachable` plays TeslaFi being down (`502 teslafi_unreachable`), one that starts with
`same-car` a car already on another TeslaFi account here (`409 account_exists`).

Every change but the car matches asks for the admin's own password again (`confirm_password` or the
`X-Sentry-Mate-Password` header) and, for an admin with two-step sign-in on, a code (the dev code is 000000), like
the real server; the car matches take the admin session alone. No answer ever holds a token.
"""

from __future__ import annotations

import re
from pathlib import Path

from ..errors import ApiError, bad_input, conflict, invalid, not_found
from ..util import DAY, MIN
from . import invoices
from .tesla import _only, _str, reconfirm

HOST = "www.teslafi.com"
MAX_LABEL = 64
# how often TeslaFi is read, by pace (crates/teslafi Pace): `often` as often as TeslaFi itself refreshes its data
PACES = {
    "often": {"driving_s": 15, "charging_s": 15, "parked_s": 60, "asleep_s": 180},
    "normal": {"driving_s": 60, "charging_s": 60, "parked_s": 180, "asleep_s": 600},
    "gentle": {"driving_s": 120, "charging_s": 120, "parked_s": 300, "asleep_s": 1200},
}
# no cap of the server's own: TeslaFi does the limiting (an account's `rate_limited_until_ms`)
POLLS = PACES["normal"]
TOKEN_RX = re.compile(r"[\x21-\x7e]{1,256}")
# the zones the real server takes (chrono-tz's list; crates/teslafi/tests/zones.rs checks it is the same)
ZONES = frozenset((Path(__file__).with_name("time_zones.txt")).read_text(encoding="utf-8").split())


def seed(st, now):
    """One TeslaFi account (the made-up owner pays for TeslaFi): its car is Atlas (`c3`), matched by an admin."""
    st.teslafi = {
        "accounts": {
            "fa1": {
                "id": "fa1", "label": "TeslaFi", "added_ms": now - 20 * DAY, "paused": False,
                "time_zone": "America/Chicago", "state": "ok", "last_ok_ms": now - 50_000, "last_error": None,
                "version": 2,
                "cars": [{"vehicle": "fv1", "vin_prefix": "5YJ3E1EA0PF", "model": "Model 3", "year": 2023,
                          "display_name": "Made-up TeslaFi car", "match": "c3"}],
            },
        },
        "next_account": 2,
        "clients_built": 1,
        "settings": {"poll": "normal", "version": 1},
    }


def _t(req):
    return req.store.teslafi


def _label(body, required=False):
    v = _str(body, "label")
    if v is None:
        return "TeslaFi account" if required else None
    if not v.strip():
        raise invalid({"label": "Must not be empty."})
    if len(v) > MAX_LABEL:
        raise invalid({"label": "At most 64 characters."})
    return v.strip()


def _token(body):
    v = _str(body, "token")
    if v is None:
        return None
    if not TOKEN_RX.fullmatch(v.strip()):
        raise invalid({"token": "Not a TeslaFi token (TeslaFi, Settings, TeslaFi API)."})
    return v.strip()


def _zone(body):
    """Absent: no change (`...`); null: the server's default; else an IANA zone."""
    if "time_zone" not in body:
        return ...
    v = body["time_zone"]
    if v is None:
        return None
    if not isinstance(v, str) or v.strip() not in ZONES:
        raise invalid({"time_zone": "Must be a time zone (e.g. America/Chicago) or null."})
    return v.strip()


def _ask_teslafi(token):
    if token.startswith("refused"):
        raise ApiError(422, "teslafi_refused", "TeslaFi refused the token: check it on TeslaFi's API page (Settings, "
                                               "TeslaFi API), and that the TeslaFi subscription is active.")
    if token.startswith("unreachable"):
        raise ApiError(502, "teslafi_unreachable", "TeslaFi could not be reached, or answered with an error. Try "
                                                   "again in a few minutes.")


def _account(req):
    a = _t(req)["accounts"].get(req.params["teslafi_account"])
    if a is None:
        raise not_found("no_such_teslafi_account", "There is no such TeslaFi account.")
    return a


def _status_word(a):
    if a["paused"]:
        return "paused"
    return "token_refused" if a["state"] == "refused" else a["state"]


def _matched(c):
    return c["match"] not in (None, "new", "leave_out")


def _mode(req, a):
    """What the server does for the account now: from its car's own state on the virtual clock."""
    if a["paused"] or a["state"] == "refused" or not any(_matched(c) for c in a["cars"]):
        return "off", None
    if a["state"] == "error" or limited_until(req, a):
        return "backoff", None
    car = next(c["match"] for c in a["cars"] if _matched(c))
    if req.ds.history(car) is None:
        return "asleep", "Sleeping"
    from ..live import now_object
    state = now_object(req.app, car, req.now).get("state")
    return {"driving": ("driving", "Driving"), "charging": ("charging", "Charging"),
            "parked": ("idle", "Idling")}.get(state, ("asleep", "Sleeping"))


def _step(mode, poll="normal"):
    p = PACES[poll]
    return {"driving": p["driving_s"] * 1000, "charging": p["charging_s"] * 1000, "idle": p["parked_s"] * 1000,
            "asleep": p["asleep_s"] * 1000, "backoff": 2 * MIN}.get(mode)


def _poll(req):
    return _t(req).setdefault("settings", {"poll": "normal", "version": 1})


def _polls_today(req, a):
    """Requests since midnight UTC, for information (there is no cap)."""
    if a["paused"] or a["state"] != "ok":
        return 0
    midnight = req.now - req.now % DAY
    return int((req.now - midnight) / 60_000 * 0.4)


def limited_until(req, a):
    """When TeslaFi's 429 wait of the account ends, while it lasts (a test or a page check sets
    `rate_limited_until_ms` on the account)."""
    t = a.get("rate_limited_until_ms")
    return t if t and t > req.now and not a["paused"] and a["state"] == "ok" else None


NO_DATA_AFTER_MS = 2 * 3_600_000  # an awake car with no new poll for this long: TeslaFi has no new data
FRESH_MS = 10 * 60_000  # a poll this recent: polls keep coming
TIME_OFFSET_MIN_MS = 3_600_000  # a fresh poll stamped this far off: the account's time zone is wrong


def data_health(req, a):
    """How old TeslaFi's data is and whose problem it is (crates/teslafi/src/health.rs), from what a page check sets
    on the account: `data_age_ms` (nothing new for that long) or `time_offset_ms` (polls come, stamped that far off).
    -> ("fine", None) | ("no_data", {age_ms, last_data_ms}) | ("time_offset", {skew_ms, behind})."""
    if a["paused"] or a["state"] != "ok" or limited_until(req, a) or not any(_matched(c) for c in a["cars"]):
        return "fine", None
    age = a.get("data_age_ms")
    if age and age >= NO_DATA_AFTER_MS and _mode(req, a)[0] not in ("asleep", "off"):
        return "no_data", {"age_ms": age - 30_000, "last_data_ms": req.now - age}
    off = a.get("time_offset_ms")
    if off and abs(off) >= TIME_OFFSET_MIN_MS:
        return "time_offset", {"skew_ms": abs(off), "behind": off > 0}
    return "fine", None


def _span(ms):
    h, m = ms // 3_600_000, ms // 60_000 % 60
    if h >= 48:
        return f"{h // 24} days"
    if h == 0:
        return f"{m} min"
    return f"{h} h" if m == 0 or h >= 10 else f"{h} h {m} min"


def _utc_minute(ms):
    import datetime
    return datetime.datetime.fromtimestamp(ms / 1000, datetime.timezone.utc).strftime("%Y-%m-%d %H:%M UTC")


def _suggest(req, c):
    if c["match"] is not None:
        return None
    for cid in req.ds.car_ids_featured():
        m = req.store.car_meta(cid)
        if m["vin_prefix"] == c["vin_prefix"] and not m["sold"]:  # a sold car is never suggested
            return cid
    return None


def car_view(req, a, c, car_state):
    current = c is a["cars"][-1]
    return {"vehicle": c["vehicle"], "vin_prefix": c["vin_prefix"], "model": c["model"], "year": c["year"],
            "display_name": c["display_name"], "match": c["match"], "suggested": _suggest(req, c),
            "current": current, "car_state": car_state if current else None}


def account_view(req, a):
    mode, car_state = _mode(req, a)
    return {
        "id": a["id"], "label": a["label"], "hosts": [HOST], "added_ms": a["added_ms"], "status": _status_word(a),
        "paused": a["paused"], "time_zone": a["time_zone"], "running": True, "mode": mode,
        "last_ok_ms": a["last_ok_ms"], "last_error": a["last_error"], "polls_today": _polls_today(req, a),
        "rate_limited_until_ms": limited_until(req, a), "tls": "1.3", "cars": [car_view(req, a, c, car_state) for c in a["cars"]], "version": a["version"],
    }


def _n_matched(t):
    return sum(1 for a in t["accounts"].values() for c in a["cars"] if _matched(c))


def _last_error(t):
    errs = [dict(a["last_error"], account=a["id"]) for a in t["accounts"].values() if a["last_error"]]
    return max(errs, key=lambda e: e["at_ms"]) if errs else None


# ------------------------------------------------------------------ the input as a whole


def overview(req):
    t = _t(req)
    return {
        "enabled": bool(t["accounts"]), "accounts": len(t["accounts"]), "cars": _n_matched(t),
        "hosts": [HOST] if t["accounts"] else [], "poll": _poll(req)["poll"], "polls": dict(PACES[_poll(req)["poll"]]),
        "polls_today": sum(_polls_today(req, a) for a in t["accounts"].values()),
        "rate_limited_until_ms": max((u for a in t["accounts"].values() if (u := limited_until(req, a))), default=None),
        "last_error": _last_error(t),
        "clients_built": t["clients_built"], "tls": "1.3" if t["accounts"] else None,
        "version": _poll(req)["version"],
        "invoices": invoices.block(req, "teslafi"),
    }


def patch_overview(req):
    """`PATCH /integrations/teslafi` `{ version, poll }`: how often TeslaFi is read."""
    body = req.json()
    _only(body, ("version", "poll", "invoices", "confirm_password", "code"))
    poll = _str(body, "poll")
    if poll is not None and poll not in PACES:
        raise invalid({"poll": "Must be often, normal or gentle."})
    inv = invoices.patch_value(body)
    reconfirm(req, body)
    st = req.store
    s = _poll(req)
    with st.lock:
        req.version_check(s["version"])
        changed = False
        if poll is not None and poll != s["poll"]:
            s["poll"] = poll
            changed = True
            st.record(req.account, "teslafi-poll-changed", "settings:teslafi",
                      f"Set how often TeslaFi is read to {poll}.")
        if invoices.apply_patch(req, "teslafi", inv):
            changed = True
        if changed:
            s["version"] += 1
            st.rev += 1
    return overview(req)


def status(req):
    items = []
    for a in _t(req)["accounts"].values():
        mode, car_state = _mode(req, a)
        step = _step(mode, _poll(req)["poll"])
        last = req.now - req.now % step if step else None
        until = limited_until(req, a)
        car = next((c for c in a["cars"] if _matched(c)), None)
        items.append({
            "account": a["id"], "car": car["match"] if car else None, "vehicle": a["cars"][-1]["vehicle"]
            if a["cars"] else None, "mode": mode, "car_state": car_state, "last_request_ms": last,
            "next_request_ms": until or (last + step if step else None),
            "last_data_ms": last - 20_000 if mode in ("driving", "charging", "idle") else a["last_ok_ms"]
            if mode != "off" else None,
            "last_new_ms": last if mode in ("driving", "charging", "idle") else None,
            "data": data_health(req, a)[0],
            "polls_today": _polls_today(req, a), "rate_limited_until_ms": until, "tls": "1.3",
            "last_error": {"code": "rate_limited", "message": "TeslaFi is limiting requests", "at_ms": until - 120_000}
            if until else a["last_error"],
        })
    return {"items": items, "next": None}


# ------------------------------------------------------------------ accounts


def list_accounts(req):
    return {"items": [account_view(req, a) for a in _t(req)["accounts"].values()], "next": None}


def get_account(req):
    return account_view(req, _account(req))


def add_account(req):
    body = req.json()
    _only(body, ("label", "token", "time_zone", "confirm_password", "code"))
    label = _label(body, required=True)
    token = _token(body)
    if token is None:
        raise bad_input({"token": "Required."})
    zone = _zone(body)
    reconfirm(req, body)
    _ask_teslafi(token)
    if token.startswith("same-car"):
        raise conflict("account_exists", "This TeslaFi token's car is already on another TeslaFi account here (or the "
                                         "token is another car's).")
    t = _t(req)
    st = req.store
    with st.lock:
        n = t["next_account"]
        t["next_account"] += 1
        t["clients_built"] += 1
        aid = f"fa{n}"
        a = {"id": aid, "label": label, "added_ms": req.now, "paused": False,
             "time_zone": None if zone is ... else zone, "state": "ok", "last_ok_ms": req.now, "last_error": None,
             "version": 1,
             "cars": [{"vehicle": "fv1", "vin_prefix": "7SAYGDEE0PF", "model": "Model Y", "year": 2023,
                       "display_name": "New TeslaFi car", "match": None}]}
        t["accounts"][aid] = a
        st.rev += 1
    st.record(req.account, "teslafi-account-added", f"teslafi:{aid}",
              f"Added the TeslaFi account {label} (1 car; checked with TeslaFi).")
    return dict(account_view(req, a), _created=True)


def patch_account(req):
    body = req.json()
    _only(body, ("version", "label", "paused", "time_zone", "token", "confirm_password", "code"))
    a = _account(req)
    label = _label(body)
    paused = body.get("paused")
    if paused is not None and not isinstance(paused, bool):
        raise bad_input({"paused": "Must be true or false."})
    zone = _zone(body)
    token = _token(body)
    reconfirm(req, body)
    st = req.store
    with st.lock:
        req.version_check(a["version"])
        if token is not None:
            _ask_teslafi(token)
        before = dict(a)
        target = f"teslafi:{a['id']}"
        if paused is not None and paused != a["paused"]:
            a["paused"] = paused
            if paused:
                st.record(req.account, "teslafi-account-paused", target,
                          f"Paused the TeslaFi account {a['label']}: its car is not read.")
            else:
                st.record(req.account, "teslafi-account-resumed", target, f"Resumed the TeslaFi account {a['label']}.")
        what = []
        if label is not None and label != a["label"]:
            what.append(f"renamed it from {a['label']}")
            a["label"] = label
        if zone is not ... and zone != a["time_zone"]:
            was = a["time_zone"] or req.store.server_settings.get("default_time_zone") or "UTC"
            what.append("set its time zone to " + (zone or "the server's default") + f" (from {was})")
            a["time_zone"] = zone
        if what:
            st.record(req.account, "teslafi-account-changed", target,
                      f"Changed the TeslaFi account {a['label']}: {', '.join(what)}.")
        if token is not None:
            a["state"], a["last_error"], a["last_ok_ms"] = "ok", None, req.now
            st.record(req.account, "teslafi-token-replaced", target,
                      f"Gave the TeslaFi account {a['label']} a new token (checked with TeslaFi).")
        if a != before or token is not None:
            a["version"] += 1
            st.rev += 1
    return account_view(req, a)


def delete_account(req):
    body = req.json(required=False)
    _only(body, ("confirm_password", "code"))
    a = _account(req)
    reconfirm(req, body)
    st = req.store
    with st.lock:
        _t(req)["accounts"].pop(a["id"], None)
        st.rev += 1
    st.record(req.account, "teslafi-account-removed", f"teslafi:{a['id']}",
              f"Removed the TeslaFi account {a['label']}: its token is deleted here; the history already stored stays.")
    return {"deleted": True, "id": a["id"]}


# ------------------------------------------------------------------ the cars of an account


def get_cars(req):
    a = _account(req)
    _, car_state = _mode(req, a)
    return {"items": [car_view(req, a, c, car_state) for c in a["cars"]], "next": None, "version": a["version"]}


def put_cars(req):
    body = req.json()
    _only(body, ("version", "items"))
    a = _account(req)
    rows = body.get("items")
    if not isinstance(rows, list) or not all(isinstance(x, dict) for x in rows):
        raise bad_input({"items": "Required: a list of { vehicle, match }."})
    st = req.store
    known = {c["vehicle"]: c for c in a["cars"]}
    shape = 'Must be a car id, "new", "leave_out" or null.'
    bad, changes = {}, []
    for i, x in enumerate(rows):
        extra = set(x) - {"vehicle", "match"}
        if extra:
            raise bad_input({f"items.{i}.{k}": "Unknown field." for k in extra})
        v, m = x.get("vehicle"), x.get("match")
        if not isinstance(v, str):
            raise bad_input({f"items.{i}.vehicle": "Required: the car's id here (fv1)."})
        if m is not None and not isinstance(m, str):
            raise bad_input({f"items.{i}.match": shape})
        if v not in known:
            bad[f"items.{i}.vehicle"] = "No such car on this account."
            continue
        if any(v == c for c, _ in changes):
            bad[f"items.{i}.vehicle"] = "Named twice."
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
        req.version_check(a["version"])
        taken = {c["match"] for o in st.teslafi["accounts"].values() if o["id"] != a["id"] for c in o["cars"]
                 if _matched(c)}
        if any(m in taken for _, m in changes):
            raise conflict("car_taken", "That car is already matched to another TeslaFi car.")
        changed = False
        what = {None: "is not matched to a car: it is not read", "new":
                "gets a new car of its own: it is read from now on", "leave_out": "is left out: it is not read"}
        for v, m in changes:
            c = known[v]
            if c["match"] == m:
                continue
            c["match"] = m
            changed = True
            st.record(req.account, "teslafi-car-matched", f"teslafi:{a['id']}:{v}",
                      f"TeslaFi car {c['display_name'] or v} of the TeslaFi account {a['label']} "
                      + what.get(m, f"is car {m}: it is read from now on") + ".",
                      car=m if m not in what else None)
        if changed:
            a["version"] += 1
            st.rev += 1
    return get_cars(req)


# ------------------------------------------------------------------ data stored in a wrong zone, dated again


def _zone_changes(req, a):
    """The changes of the account's zone in the audit log, newest first: (when, the zone set; None: the default, the
    zone read in until then where the entry says it)."""
    out = []
    for e in sorted(req.store.audit, key=lambda e: e["at_ms"], reverse=True):
        if e["action"] != "teslafi-account-changed" or e.get("target") != f"teslafi:{a['id']}":
            continue
        m = re.search(r"set its time zone to ([^,.(]+?)(?: \(from ([^)]+)\))?(?:[,.]|$)", e.get("summary") or "")
        if m:
            z, was = m.group(1).strip(), (m.group(2) or "").strip()
            out.append((e["at_ms"], z if z in ZONES else None, was if was in ZONES else None))
    return out


def _zone_now(req, a):
    return a["time_zone"] or req.store.server_settings.get("default_time_zone") or "UTC"


def get_redate(req):
    a = _account(req)
    ch = _zone_changes(req, a)
    before = (ch[0][2] if ch else None) or (ch[1][1] if len(ch) > 1 else None)
    # (the zone the data is in now, once a re-dating said it)
    cars = {c["match"] for c in a["cars"] if _matched(c)}
    before = next((d["zone"] for d in reversed(a.get("redated", [])) if d["car"] in cars), None) or before
    return {"zone": _zone_now(req, a), "before_ms": ch[0][0] if ch else None,
            "zone_before": before or (None if ch else req.store.server_settings.get("default_time_zone") or "UTC")}


def post_redate(req):
    """The made-up cars' TeslaFi data is not kept by time here: made-up counts, asked once per zones and moment."""
    body = req.json()
    _only(body, ("zone_before", "before_ms", "confirm_password", "code"))
    a = _account(req)
    zb = body.get("zone_before")
    if not isinstance(zb, str) or zb.strip() not in ZONES:
        raise invalid({"zone_before": "Must be the time zone the times were read in (e.g. UTC)."})
    zb = zb.strip()
    before = body.get("before_ms")
    if before is not None and (not isinstance(before, int) or isinstance(before, bool) or before < 0):
        raise bad_input({"before_ms": "Must be a time (ms since 1970)."})
    if before is not None and before > req.now:
        raise invalid({"before_ms": "Must not be in the future."})
    reconfirm(req, body)
    if before is None:
        ch = _zone_changes(req, a)
        if not ch:
            raise invalid({"before_ms": "The audit log has no change of this account's zone: give the moment."})
        before = ch[0][0]
    zone = _zone_now(req, a)
    st = req.store
    out, errors = [], []
    with st.lock:
        done = a.setdefault("redated", [])
        for c in (c for c in a["cars"] if _matched(c)):
            # the zone the car's data is in now (made up: one zone for all of it, once a re-dating said it)
            now_in = next((d["zone"] for d in reversed(done) if d["car"] == c["match"]), None)
            if now_in == zone:
                out.append({"car": c["match"], "samples": 0, "spans": 0, "drives": 0, "charges": 0, "already": True})
                continue
            if zb == zone:
                # the same zone: nothing moves
                out.append({"car": c["match"], "samples": 0, "spans": 0, "drives": 0, "charges": 0, "already": False})
                continue
            if now_in is not None and now_in != zb:
                errors.append({"car": c["match"], "message": f"the data stored before {_utc_minute(before)} is in "
                               f"{now_in} now: ask from {now_in}"})
                continue
            moved = {"samples": 5_760, "spans": 24, "drives": 6, "charges": 2}
            done.append({"car": c["match"], "zone_before": zb, "zone": zone, "before_ms": before})
            st.record(req.account, "teslafi-data-redated", f"teslafi:{a['id']}",
                      f"Dated the data the TeslaFi account {a['label']} stored for car {c['match']} before "
                      f"{_utc_minute(before)} again: read in {zone}, not {zb} ({moved['samples']} samples, "
                      f"{moved['spans']} state spans, {moved['drives']} drives and {moved['charges']} charges moved). "
                      "What was built from it is built again, and TeslaFi's charge costs are read again.")
            out.append({"car": c["match"], **moved, "already": False})
        st.rev += 1
    if errors and not out:
        raise conflict("redate_failed", errors[0]["message"])
    return {"zone_before": zb, "zone": zone, "before_ms": before, "cars": out, "errors": errors}


# ------------------------------------------------------------------ for other resources


def notices(req):
    """For admins: an account whose token TeslaFi refuses, or one that keeps failing."""
    out = []
    for a in _t(req)["accounts"].values():
        if a["paused"]:
            continue
        since = a["last_error"]["at_ms"] if a["last_error"] else None
        if a["state"] == "refused":
            out.append({"code": "teslafi_token_refused", "level": "warning",
                        "message": f"TeslaFi refused the token of the TeslaFi account \"{a['label']}\": a wrong token, "
                                   "or the TeslaFi subscription ended. Its car is not read until you give a new token "
                                   "(Settings, TeslaFi).",
                        "since_ms": since, "fix_link": "/settings/integrations#teslafi",
                        "detail": {"account": a["id"]}})
        elif a["state"] == "error":
            msg = a["last_error"]["message"] if a["last_error"] else "TeslaFi answers with errors"
            out.append({"code": "teslafi_account_error", "level": "warning",
                        "message": f"The TeslaFi account \"{a['label']}\" is not read: {msg}.", "since_ms": since,
                        "fix_link": "/settings/integrations#teslafi", "detail": {"account": a["id"]}})
        until = limited_until(req, a)
        if until:
            out.append({"code": "teslafi_rate_limited", "level": "warning",
                        "message": f"TeslaFi is limiting requests for the TeslaFi account \"{a['label']}\", next try "
                                   f"at {_utc_minute(until)}. This server sets no limit of its own; it waits as long "
                                   "as TeslaFi asks.",
                        "since_ms": since, "fix_link": "/settings/integrations#teslafi",
                        "detail": {"account": a["id"], "until_ms": until}})
        word, d = data_health(req, a)
        if word == "no_data":
            out.append({"code": "teslafi_no_data", "level": "warning",
                        "message": f"TeslaFi has had no new data from the car for {_span(d['age_ms'])} (the TeslaFi "
                                   f"account \"{a['label']}\"; its last data is from {_utc_minute(d['last_data_ms'])}). "
                                   "This server reads TeslaFi without errors, so the car is not sending TeslaFi "
                                   "anything newer: the problem is on TeslaFi's side or between TeslaFi and the car, "
                                   "not here.",
                        "since_ms": d["last_data_ms"], "fix_link": "/settings/integrations#teslafi",
                        "detail": {"account": a["id"], "last_data_ms": d["last_data_ms"], "age_ms": d["age_ms"]}})
        elif word == "time_offset":
            out.append({"code": "teslafi_time_offset", "level": "warning",
                        "message": f"The times TeslaFi gives for the TeslaFi account \"{a['label']}\" are "
                                   f"{_span(d['skew_ms'])} {'before' if d['behind'] else 'after'} the moment this "
                                   f"server reads them, so every value from it looks "
                                   f"{'old' if d['behind'] else 'to come'}. Check the account's time zone (Settings, "
                                   "TeslaFi): TeslaFi writes local time without a zone.",
                        "since_ms": None, "fix_link": "/settings/integrations#teslafi",
                        "detail": {"account": a["id"], "skew_ms": d["skew_ms"], "behind": d["behind"]}})
    return out


def network_rows(req):
    t = _t(req)
    if not t["accounts"]:
        return [{"purpose": "teslafi", "host": HOST, "enabled": False, "last_used_ms": None}]
    reading = any(_status_word(a) == "ok" for a in t["accounts"].values())
    last = max((a["last_ok_ms"] for a in t["accounts"].values() if a["last_ok_ms"]), default=None)
    return [{"purpose": "teslafi", "host": HOST, "enabled": reading, "last_used_ms": last}]


def health(req):
    t = _t(req)
    return {"accounts": len(t["accounts"]), "cars": _n_matched(t),
            "requests_today": sum(_polls_today(req, a) for a in t["accounts"].values()), "last_error": _last_error(t)}


def token_refused(st, account_id, now):
    """For tests and the dev data: TeslaFi stops taking an account's token (as the poller sees it)."""
    with st.lock:
        a = st.teslafi["accounts"][account_id]
        a["state"] = "refused"
        a["last_error"] = {"code": "token_refused", "message": "TeslaFi refused the token (a wrong token, or the "
                                                              "TeslaFi subscription ended)", "at_ms": now}
        a["version"] += 1
        st.rev += 1
    st.record(None, "teslafi-token-refused", f"teslafi:{account_id}",
              f"TeslaFi refused the token of the TeslaFi account {a['label']} (a wrong token, or the TeslaFi "
              "subscription ended): its car is not read.")


def register(router):
    admin_all = ("GET", "POST", "PUT", "PATCH", "DELETE")
    router.add("/api/v1/integrations/teslafi", GET=overview, PATCH=patch_overview, admin=admin_all)
    router.add("/api/v1/integrations/teslafi/accounts", GET=list_accounts, POST=add_account, admin=admin_all)
    router.add("/api/v1/integrations/teslafi/accounts/{teslafi_account}", GET=get_account, PATCH=patch_account,
               DELETE=delete_account, admin=admin_all)
    router.add("/api/v1/integrations/teslafi/accounts/{teslafi_account}/cars", GET=get_cars, PUT=put_cars,
               admin=admin_all)
    router.add("/api/v1/integrations/teslafi/accounts/{teslafi_account}/redate", GET=get_redate, POST=post_redate,
               admin=admin_all)
    router.add("/api/v1/integrations/teslafi/status", GET=status, admin=admin_all)
