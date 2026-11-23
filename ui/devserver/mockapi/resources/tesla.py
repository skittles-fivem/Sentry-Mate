"""The Tesla API input (docs/ui-api.md, "Integrations: Tesla"): Tesla accounts, signing in at Tesla, the cars of
an account and what the server does for each.

Everything is in memory and nothing leaves the dev server: the sign-in address is built but never fetched, a
pasted `tesla://auth/callback?code=...&state=<state>` (or the `void` one) is taken as Tesla's answer, and a
refresh token is "checked" by its look alone. A token that starts with `refused` plays one Tesla refuses
(`422 tesla_refused`), one that starts with `unreachable` plays Tesla being down (`502 tesla_unreachable`).

Every change but starting a sign-in and the car matches asks for the admin's own password again
(`confirm_password` or the `X-Sentry-Mate-Password` header) and, for an admin with two-step sign-in on, a code
(`code` or `X-Sentry-Mate-Code`; the dev code is 000000), like the real server; the car matches take the admin
session alone. No answer ever holds a token.
"""

from __future__ import annotations

import re
import secrets
from urllib.parse import parse_qs, unquote_to_bytes, urlencode, urlsplit

from ..errors import ApiError, bad_input, conflict, invalid, not_found
from ..store import DEV_PASSWORD, DEV_TWO_STEP_CODE
from ..util import DAY, MIN
from . import invoices

SIGN_IN_MS = 10 * MIN
MAX_SIGN_INS = 5
MAX_LABEL = 64
HOSTS = {"global": ["auth.tesla.com", "owner-api.teslamotors.com", "streaming.vn.teslamotors.com"],
         "china": ["auth.tesla.cn", "owner-api.vn.cloud.tesla.cn"]}
AUTH = {"global": "https://auth.tesla.com", "china": "https://auth.tesla.cn"}
REDIRECT = {"app": lambda region: "tesla://auth/callback", "void": lambda region: AUTH[region] + "/void/callback"}
LOCATIONS = ("driving_or_charging", "always", "never")
PACES = {
    "often": {"list_s": 30, "parked_s": 30, "charging_s": 15, "driving_s": 15, "driving_streaming_s": 30},
    "normal": {"list_s": 60, "parked_s": 60, "charging_s": 30, "driving_s": 30, "driving_streaming_s": 60},
    "gentle": {"list_s": 120, "parked_s": 300, "charging_s": 120, "driving_s": 60, "driving_streaming_s": 120},
}
TOKEN_RX = re.compile(r"[\x21-\x7e]{1,8192}")


def seed(st, now):
    """One Tesla account with the two cars that have Tesla API data (c1 with SMO too, c2), and a third vehicle
    not matched yet."""
    st.tesla = {
        "settings": {"poll": "normal", "version": 1},
        "accounts": {
            "ta1": {
                "id": "ta1", "label": "Family account", "region": "global", "redirect": "app",
                "added_ms": now - 60 * DAY, "paused": False, "location": "driving_or_charging",
                "state": "ok", "last_ok_ms": now - 45_000, "last_error": None, "version": 3,
                "cars": [
                    {"vehicle": "tv1", "vin_prefix": "7SAYGDEE0SF", "model": "Model Y", "year": 2025,
                     "display_name": "Comet", "match": "c1"},
                    {"vehicle": "tv2", "vin_prefix": "5YJ3E1EA0NF", "model": "Model 3", "year": 2022,
                     "display_name": "Maple", "match": "c2"},
                    {"vehicle": "tv3", "vin_prefix": "LRW3E7EK0PC", "model": "Model 3", "year": 2023,
                     "display_name": "Guest car", "match": None},
                ],
            },
        },
        "sign_ins": {},
        "next_account": 2,
        "clients_built": 1,
    }


# ------------------------------------------------------------------ helpers


def _t(req):
    return req.store.tesla


def _only(body, allowed):
    extra = sorted(set(body) - set(allowed))
    if extra:
        raise bad_input({k: "Unknown field." for k in extra})


def _str(body, key):
    v = body.get(key)
    if v is not None and not isinstance(v, str):
        raise bad_input({key: "Must be text."})
    return v


def _header_password(req):
    """The password from a header, as the real server reads it: X-Sentry-Mate-Password-Encoded (percent-encoded
    UTF-8, any password), else X-Sentry-Mate-Password (its bytes as UTF-8). Neither: None."""
    not_text = bad_input({"confirm_password": "The password header is not UTF-8: send it percent-encoded in "
                                              "X-Sentry-Mate-Password-Encoded."})
    enc = req.headers.get("X-Sentry-Mate-Password-Encoded")
    try:
        if enc is not None:
            return unquote_to_bytes(enc).decode("utf-8")
        pw = req.headers.get("X-Sentry-Mate-Password")
        # http.server reads header bytes as Latin-1
        return None if pw is None else pw.encode("latin-1").decode("utf-8")
    except (UnicodeDecodeError, UnicodeEncodeError):
        raise not_text


def reconfirm(req, body):
    """The admin's own password again, and a code when the admin has two-step sign-in on."""
    pw = _str(body, "confirm_password")
    if pw is None:
        pw = _header_password(req)
    if pw is None:
        raise ApiError(403, "password_needed", "Type your own password again to change this.",
                       fields={"confirm_password": "Your own password, again."})
    if not secrets.compare_digest(str(pw).encode(), req.account.get("password", DEV_PASSWORD).encode()):
        raise ApiError(403, "wrong_password", "That is not your password.",
                       fields={"confirm_password": "Not your password."})
    if req.account.get("two_step"):
        code = _str(body, "code")
        if code is None:
            code = req.headers.get("X-Sentry-Mate-Code")
        if code is None or code.strip() != DEV_TWO_STEP_CODE:
            raise ApiError(401, "wrong_code", "Type the code from your authenticator app.",
                           fields={"code": "The 6-digit code."})


def _label(body, required=False):
    v = _str(body, "label")
    if v is None:
        return "Tesla account" if required else None
    if not v.strip():
        raise invalid({"label": "Must not be empty."})
    if len(v) > MAX_LABEL:
        raise invalid({"label": "At most 64 characters."})
    return v.strip()


def _region(body):
    v = _str(body, "region")
    if v is None:
        return "global"
    if v not in HOSTS:
        raise invalid({"region": "Must be global or china."})
    return v


def _token(body, key="refresh_token"):
    v = _str(body, key)
    if v is None:
        return None
    if not TOKEN_RX.fullmatch(v.strip()):
        raise invalid({key: "Not a Tesla refresh token."})
    return v.strip()


def _ask_tesla(token):
    """What the made-up Tesla says to a token."""
    if token.startswith("refused"):
        raise ApiError(422, "tesla_refused", "Tesla refused the sign-in or the token (it may have run out, been "
                                             "used, or the password changed). Sign in again.")
    if token.startswith("unreachable"):
        raise ApiError(502, "tesla_unreachable", "Tesla could not be reached, or answered with an error. Try again "
                                                 "in a few minutes.")


def _account(req):
    a = _t(req)["accounts"].get(req.params["tesla_account"])
    if a is None:
        raise not_found("no_such_tesla_account", "There is no such Tesla account.")
    return a


def _status_word(a):
    if a["paused"] and a["state"] != "sign_in_needed":
        return "paused"
    return a["state"]


def _mode(req, a, c):
    """What the server does for the car now: from the made-up car's own state on the virtual clock."""
    if a["paused"] or a["state"] == "sign_in_needed" or c["match"] in (None, "leave_out"):
        return "off", "unknown"
    if a["state"] == "error":
        # the server waits after Tesla's errors (an hour after a 403); a closed endpoint is not asked again at all
        gone = (a["last_error"] or {}).get("code") == "endpoint_gone"
        return ("off" if gone else "backoff"), "unknown"
    if c["match"] == "new" or req.ds.history(c["match"]) is None:
        return "watching_list", "asleep"
    from ..live import now_object
    state = now_object(req.app, c["match"], req.now).get("state")
    if state == "driving":
        return "driving", "online"
    if state == "charging":
        return "charging", "online"
    if state == "parked":
        # an awake car with nothing going on is left alone after 10 minutes so it can fall asleep
        return ("letting_sleep" if (req.now // (10 * MIN)) % 3 == 2 else "online_idle"), "online"
    return "watching_list", "asleep" if state == "asleep" else "offline"


def _polls_today(req, a):
    if a["paused"] or a["state"] != "ok":
        return 0
    midnight = req.now - req.now % DAY
    return int((req.now - midnight) / 60_000 * 1.4) % 3000


def _suggest(req, c):
    if c["match"] is not None:
        return None
    for cid in req.ds.car_ids_featured():
        m = req.store.car_meta(cid)
        if m["vin_prefix"] == c["vin_prefix"] and not m["sold"]:  # a sold car is never suggested
            return cid
    return None


def car_view(req, a, c):
    mode, state = _mode(req, a, c)
    return {"vehicle": c["vehicle"], "vin_prefix": c["vin_prefix"], "model": c["model"], "year": c["year"],
            "display_name": c["display_name"], "match": c["match"], "suggested": _suggest(req, c),
            "state": state, "mode": mode}


def account_view(req, a):
    return {
        "id": a["id"], "label": a["label"], "region": a["region"], "redirect": a["redirect"],
        "hosts": list(HOSTS[a["region"]]), "added_ms": a["added_ms"], "status": _status_word(a),
        "paused": a["paused"], "location": a["location"], "running": True,
        "last_ok_ms": a["last_ok_ms"], "last_error": a["last_error"], "polls_today": _polls_today(req, a),
        "cars": [car_view(req, a, c) for c in a["cars"]], "version": a["version"],
    }


def _matched(t):
    return sum(1 for a in t["accounts"].values() for c in a["cars"] if c["match"] not in (None, "leave_out"))


def _last_error(t):
    errs = [dict(a["last_error"], account=a["id"]) for a in t["accounts"].values() if a["last_error"]]
    return max(errs, key=lambda e: e["at_ms"]) if errs else None


def _hosts(t):
    return sorted({h for a in t["accounts"].values() for h in HOSTS[a["region"]]})


# ------------------------------------------------------------------ the input as a whole


def overview(req):
    t = _t(req)
    poll = t["settings"]["poll"]
    return {
        "enabled": bool(t["accounts"]), "accounts": len(t["accounts"]), "cars": _matched(t), "hosts": _hosts(t),
        "poll": poll, "polls": dict(PACES[poll], daily_cap=3000),
        "polls_today": sum(_polls_today(req, a) for a in t["accounts"].values()), "last_error": _last_error(t),
        "clients_built": t["clients_built"], "version": t["settings"]["version"],
        "invoices": invoices.block(req, "tesla"),
    }


def patch_overview(req):
    body = req.json()
    _only(body, ("version", "poll", "invoices", "confirm_password", "code"))
    poll = _str(body, "poll")
    if poll is not None and poll not in PACES:
        raise invalid({"poll": "Must be often, normal or gentle."})
    inv = invoices.patch_value(body)
    reconfirm(req, body)
    st = req.store
    s = _t(req)["settings"]
    with st.lock:
        req.version_check(s["version"])
        changed = False
        if poll is not None and poll != s["poll"]:
            s["poll"] = poll
            changed = True
            st.record(req.account, "tesla-poll-changed", "settings:tesla",
                      f"Set how often the Tesla API is asked to {poll}.")
        if invoices.apply_patch(req, "tesla", inv):
            changed = True
        if changed:
            s["version"] += 1
            st.rev += 1
    return overview(req)


def status(req):
    items = []
    for a in _t(req)["accounts"].values():
        for c in a["cars"]:
            mode, state = _mode(req, a, c)
            step = {"driving": 30_000, "charging": 30_000, "online_idle": 60_000, "letting_sleep": 15 * MIN,
                    "watching_list": 60_000, "backoff": 120_000}.get(mode)
            last = req.now - req.now % step if step else None
            items.append({
                "car": c["match"] if c["match"] not in (None, "new", "leave_out") else None,
                "account": a["id"], "vehicle": c["vehicle"], "mode": mode, "car_state": state,
                "last_poll_ms": last, "next_poll_ms": last + step if step else None,
                "last_data_ms": last if mode in ("driving", "charging", "online_idle") else a["last_ok_ms"]
                if mode != "off" else None,
                "polls_today": _polls_today(req, a), "streaming": mode == "driving" and a["region"] == "global",
                "last_error": None,
            })
    return {"items": items, "next": None}


# ------------------------------------------------------------------ accounts


def list_accounts(req):
    return {"items": [account_view(req, a) for a in _t(req)["accounts"].values()], "next": None}


def get_account(req):
    return account_view(req, _account(req))


def _new_account(req, label, region, redirect, token):
    """The made-up Tesla's answer: an account with one new vehicle (its VIN prefix from the token's look)."""
    t = _t(req)
    st = req.store
    with st.lock:
        n = t["next_account"]
        t["next_account"] += 1
        t["clients_built"] += 1
        aid = f"ta{n}"
        a = {
            "id": aid, "label": label, "region": region, "redirect": redirect, "added_ms": req.now,
            "paused": False, "location": "driving_or_charging", "state": "ok", "last_ok_ms": req.now,
            "last_error": None, "version": 1,
            "cars": [{"vehicle": f"tv{10 * n + 1}", "vin_prefix": "5YJ3E7EB0RF", "model": "Model 3", "year": 2024,
                      "display_name": "New Tesla", "match": None}],
        }
        t["accounts"][aid] = a
        st.rev += 1
    n_cars = len(a["cars"])
    st.record(req.account, "tesla-account-added", f"tesla:{aid}",
              f"Added the Tesla account {label} {'by signing in at Tesla' if token is None else 'with a refresh token'}"
              f" ({n_cars} car{'' if n_cars == 1 else 's'}).")
    return a


def add_account(req):
    body = req.json()
    _only(body, ("label", "region", "refresh_token", "confirm_password", "code"))
    label = _label(body, required=True)
    region = _region(body)
    token = _token(body)
    if token is None:
        raise bad_input({"refresh_token": "Required."})
    reconfirm(req, body)
    _ask_tesla(token)
    if token.startswith("same-cars"):
        raise conflict("account_exists", "The cars of this Tesla account are already on another one here.")
    a = _new_account(req, label, region, "app", token)
    return dict(account_view(req, a), _created=True)


def sign_in_start(req):
    body = req.json(required=False)
    _only(body, ("region", "redirect"))
    region = _region(body)
    redirect = _str(body, "redirect") or "app"
    if redirect not in REDIRECT:
        raise invalid({"redirect": "Must be app or void."})
    t = _t(req)
    with req.store.lock:
        t["sign_ins"] = {k: s for k, s in t["sign_ins"].items() if s["expires_ms"] >= req.now}
        if len(t["sign_ins"]) >= MAX_SIGN_INS:
            raise conflict("too_many_sign_ins",
                           "Too many sign-ins are open: finish one, or wait ten minutes for them to run out.")
        sid = "si" + secrets.token_hex(6)
        state = secrets.token_urlsafe(16)
        s = {"id": sid, "region": region, "redirect": redirect, "state": state,
             "expires_ms": req.now + SIGN_IN_MS}
        t["sign_ins"][sid] = s
    # the PKCE challenge of the real server is made up here: nothing is ever sent to it
    q = urlencode({"client_id": "ownerapi", "code_challenge": secrets.token_urlsafe(32),
                   "code_challenge_method": "S256", "redirect_uri": REDIRECT[redirect](region),
                   "response_type": "code", "scope": "openid email offline_access", "state": state})
    return {"sign_in": sid, "url": f"{AUTH[region]}/oauth2/v3/authorize?{q}", "expires_ms": s["expires_ms"],
            "_created": True}


def _code_from(s, pasted):
    pasted = pasted.strip()
    if "?" not in pasted:
        raise invalid({"pasted_url": "Paste the whole address, with ?code= in it."})
    base, _, query = pasted.partition("?")
    if base.rstrip("/").lower() != REDIRECT[s["redirect"]](s["region"]).lower():
        raise invalid({"pasted_url": "This is not Tesla's sign-in callback address."})
    q = parse_qs(query.split("#", 1)[0])
    if "error" in q:
        raise ApiError(422, "tesla_refused", "Tesla refused the sign-in. Sign in again.")
    if q.get("state", [None])[0] != s["state"]:
        raise invalid({"pasted_url": "This address belongs to another sign-in: start a new one."})
    code = q.get("code", [""])[0]
    if not code:
        raise invalid({"pasted_url": "The address has no code in it."})
    return code


def sign_in_finish(req):
    body = req.json()
    _only(body, ("pasted_url", "label", "confirm_password", "code"))
    pasted = _str(body, "pasted_url")
    if pasted is None:
        raise bad_input({"pasted_url": "Required."})
    if len(pasted) > 8192:
        raise invalid({"pasted_url": "Too long for Tesla's callback address."})
    label = _label(body, required=True)
    reconfirm(req, body)
    t = _t(req)
    with req.store.lock:
        s = t["sign_ins"].get(req.params["sign_in"])
        if s is None or s["expires_ms"] < req.now:
            t["sign_ins"].pop(req.params["sign_in"], None)
            raise not_found("no_such_sign_in", "That sign-in ran out or was used: start a new one.")
        code = _code_from(s, pasted)  # a bad paste leaves the sign-in open for another try
        t["sign_ins"].pop(s["id"], None)
    _ask_tesla(code)
    a = _new_account(req, label, s["region"], s["redirect"], None)
    return dict(account_view(req, a), _created=True)


def patch_account(req):
    body = req.json()
    _only(body, ("version", "label", "paused", "location", "refresh_token", "confirm_password", "code"))
    a = _account(req)
    label = _label(body)
    paused = body.get("paused")
    if paused is not None and not isinstance(paused, bool):
        raise bad_input({"paused": "Must be true or false."})
    location = _str(body, "location")
    if location is not None and location not in LOCATIONS:
        raise invalid({"location": "Must be driving_or_charging, always or never."})
    token = _token(body)
    reconfirm(req, body)
    st = req.store
    with st.lock:
        req.version_check(a["version"])
        if token is not None:
            _ask_tesla(token)
        before = dict(a)
        changed = False
        if paused is not None and paused != a["paused"]:
            a["paused"] = paused
            changed = True
            if paused:
                st.record(req.account, "tesla-account-paused", f"tesla:{a['id']}",
                          f"Paused the Tesla account {a['label']}: its cars are not read.")
            else:
                st.record(req.account, "tesla-account-resumed", f"tesla:{a['id']}",
                          f"Resumed the Tesla account {a['label']}.")
        what = []
        if label is not None and label != a["label"]:
            what.append(f"renamed it from {a['label']}")
            a["label"] = label
        if location is not None and location != a["location"]:
            what.append(f"set when its cars' location is read to {location}")
            a["location"] = location
        if what:
            changed = True
            st.record(req.account, "tesla-account-changed", f"tesla:{a['id']}",
                      f"Changed the Tesla account {a['label']}: {', '.join(what)}.")
        if token is not None:
            changed = True
            a["state"], a["last_error"], a["last_ok_ms"] = "ok", None, req.now
            st.record(req.account, "tesla-token-replaced", f"tesla:{a['id']}",
                      f"Gave the Tesla account {a['label']} a new refresh token (checked with Tesla).")
        if changed and a != before:
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
    st.record(req.account, "tesla-account-removed", f"tesla:{a['id']}",
              f"Removed the Tesla account {a['label']}: its tokens are deleted here; the history already stored "
              "stays.")
    return {"deleted": True, "id": a["id"]}


# ------------------------------------------------------------------ the cars of an account


def get_cars(req):
    a = _account(req)
    return {"items": [car_view(req, a, c) for c in a["cars"]], "next": None, "version": a["version"]}


def put_cars(req):
    body = req.json()
    _only(body, ("version", "items"))
    a = _account(req)
    rows = body.get("items")
    if not isinstance(rows, list) or not all(isinstance(x, dict) for x in rows):
        raise bad_input({"items": "Required: a list of { vehicle, match }."})
    st = req.store
    known = {c["vehicle"]: c for c in a["cars"]}
    bad, changes = {}, []
    for i, x in enumerate(rows):
        extra = set(x) - {"vehicle", "match"}
        if extra:
            raise bad_input({f"items.{i}.{k}": "Unknown field." for k in extra})
        v, m = x.get("vehicle"), x.get("match")
        if not isinstance(v, str):
            raise bad_input({f"items.{i}.vehicle": "Required: the vehicle id (tv1)."})
        if m is not None and not isinstance(m, str):
            raise bad_input({f"items.{i}.match": 'Must be a car id, "new", "leave_out" or null.'})
        if v not in known:
            bad[f"items.{i}.vehicle"] = "No such vehicle on this account."
            continue
        if any(v == c for c, _ in changes):
            bad[f"items.{i}.vehicle"] = "Named twice."
            continue
        if m not in (None, "new", "leave_out"):
            if not re.fullmatch(r"c\d+", m):
                raise bad_input({f"items.{i}.match": 'Must be a car id, "new", "leave_out" or null.'})
            if not st.sees(req.account, m) or not req.ds.exists(m):
                bad[f"items.{i}.match"] = "No such car."
                continue
        changes.append((v, m))
    if bad:
        raise invalid(bad)
    with st.lock:
        req.version_check(a["version"])
        taken = {c["match"]: o["id"] for o in st.tesla["accounts"].values() if o["id"] != a["id"]
                 for c in o["cars"] if c["match"] not in (None, "new", "leave_out")}
        for v, m in changes:
            if m in taken:
                raise conflict("car_taken", "That car is already matched to another Tesla vehicle.")
        changed = False
        for v, m in changes:
            c = known[v]
            if c["match"] == m:
                continue
            c["match"] = m
            changed = True
            what = {None: "is not matched to a car: it is not read", "new":
                    "gets a new car of its own: it is read from now on", "leave_out": "is left out: it is not read"}
            st.record(req.account, "tesla-car-matched", f"tesla:{a['id']}:{v}",
                      f"Tesla vehicle {c['display_name'] or v} of the Tesla account {a['label']} "
                      + what.get(m, f"is car {m}: it is read from now on") + ".",
                      car=m if m not in what else None)
        if changed:
            a["version"] += 1
            st.rev += 1
    return get_cars(req)


# ------------------------------------------------------------------ for other resources


def notices(req):
    """For admins: an account Tesla stopped taking, or one it refuses."""
    out = []
    for a in _t(req)["accounts"].values():
        since = a["last_error"]["at_ms"] if a["last_error"] else None
        if a["state"] == "sign_in_needed":
            out.append({"code": "tesla_sign_in_needed", "level": "warning",
                        "message": f"Tesla stopped taking the sign-in of the Tesla account \"{a['label']}\". Its cars "
                                   "are not read until you sign in again (Settings, Tesla account).",
                        "since_ms": since, "fix_link": "/settings/integrations#tesla", "detail": {"account": a["id"]}})
        elif a["state"] == "error":
            msg = a["last_error"]["message"] if a["last_error"] else "Tesla refuses it"
            out.append({"code": "tesla_account_error", "level": "warning",
                        "message": f"The Tesla account \"{a['label']}\" is not read: {msg}.", "since_ms": since,
                        "fix_link": "/settings/integrations#tesla", "detail": {"account": a["id"]}})
    return out


def network_rows(req):
    t = _t(req)
    if not t["accounts"]:
        return [{"purpose": "tesla", "host": "owner-api.teslamotors.com", "enabled": False, "last_used_ms": None}]
    reading = any(_status_word(a) == "ok" for a in t["accounts"].values())
    last = max((a["last_ok_ms"] for a in t["accounts"].values() if a["last_ok_ms"]), default=None)
    return [{"purpose": "tesla", "host": h, "enabled": reading, "last_used_ms": last} for h in _hosts(t)]


def health(req):
    t = _t(req)
    return {"accounts": len(t["accounts"]), "cars": _matched(t),
            "requests_today": sum(_polls_today(req, a) for a in t["accounts"].values()), "last_error": _last_error(t)}


def sign_in_needed(st, account_id, now):
    """For tests and the dev data: Tesla stops taking an account's token (as the poller sees it)."""
    with st.lock:
        a = st.tesla["accounts"][account_id]
        a["state"] = "sign_in_needed"
        a["last_error"] = {"code": "sign_in_needed", "message": "Tesla refused the refresh token", "at_ms": now}
        a["version"] += 1
        st.rev += 1
    st.record(None, "tesla-sign-in-needed", f"tesla:{account_id}",
              f"Tesla stopped taking the sign-in of the Tesla account {a['label']}: its cars are not read.")


def register(router):
    admin_all = ("GET", "POST", "PUT", "PATCH", "DELETE")
    router.add("/api/v1/integrations/tesla", GET=overview, PATCH=patch_overview, admin=admin_all)
    router.add("/api/v1/integrations/tesla/accounts", GET=list_accounts, POST=add_account, admin=admin_all)
    router.add("/api/v1/integrations/tesla/sign-in", POST=sign_in_start, admin=admin_all)
    router.add("/api/v1/integrations/tesla/sign-in/{sign_in}", POST=sign_in_finish, admin=admin_all)
    router.add("/api/v1/integrations/tesla/accounts/{tesla_account}", GET=get_account, PATCH=patch_account,
               DELETE=delete_account, admin=admin_all)
    router.add("/api/v1/integrations/tesla/accounts/{tesla_account}/cars", GET=get_cars, PUT=put_cars,
               admin=admin_all)
    router.add("/api/v1/integrations/tesla/status", GET=status, admin=admin_all)

