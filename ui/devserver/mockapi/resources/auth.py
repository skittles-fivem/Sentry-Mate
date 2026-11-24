"""Sign-in, accounts and preferences (docs/ui-api.md, "Sign-in, accounts and preferences")."""

from __future__ import annotations

import re
import secrets
import threading
import time
from urllib.parse import quote

from ..conventions import page
from ..errors import ApiError, bad_input, conflict, invalid, not_found, throttled
from ..store import keep_recovery_codes, new_recovery_codes, take_code

_throttle_lock = threading.Lock()
THROTTLE_TRIES = 5
THROTTLE_WINDOW_S = 60
THROTTLE_SETUP_TRIES = 10  # wrong set-up codes from one address
# wrong passwords, as the real server counts them (docs/ui-api.md, "Sign-in throttle"): per address and per name,
# five free, then a wait of 30 s that doubles with every further wrong try, up to 15 minutes
STRIKES_FREE = 5
STRIKES_FIRST_WAIT_S = 30
STRIKES_MAX_WAIT_S = 15 * 60
PENDING_S = 300
SESSION_DAYS, IDLE_DAYS, DAY_MS = 30, 7, 86_400_000


def _everyone(store):
    return bool(store.two_step_policy["everyone"])


def setup_only(store, a):
    """The rule asks every account for two-step sign-in and this one has none yet: its session may only set it up."""
    return _everyone(store) and not a.get("two_step")


def account_view(a, store):
    # two_step: this account has two-step sign-in on; two_step_required: the server-wide rule `everyone` (the same
    # for every account: the real server has no per-account rule)
    return {"id": a["id"], "name": a["name"], "role": a["role"], "cars": a["cars"], "two_step": bool(a.get("two_step")),
            "two_step_required": _everyone(store), "created_ms": a["created_ms"], "version": a["version"]}


def me_view(a, store):
    # two_step: the account has two-step sign-in on; two_step_required: the rule applies to this browser now (the dev
    # server is always "at home", so only `everyone` counts); two_step_setup_only: the rule asks it and the account
    # has none yet, so the real server lets this session do nothing but set it up
    two = bool(a.get("two_step"))
    need = _everyone(store)
    return {"id": a["id"], "name": a["name"], "role": a["role"], "cars": a["cars"], "two_step": two,
            "two_step_required": need, "two_step_setup_only": setup_only(store, a)}


# ------------------------------------------------------------------ throttle


def _check_throttle(store, key, tries=THROTTLE_TRIES):
    now = time.monotonic()
    with _throttle_lock:
        fails = [t for t in store.throttle.get(key, []) if now - t < THROTTLE_WINDOW_S]
        if fails:
            store.throttle[key] = fails
        else:
            store.throttle.pop(key, None)
        if len(fails) >= tries:
            raise throttled(max(1, int(THROTTLE_WINDOW_S - (now - fails[0])) + 1))


def _fail(store, *keys):
    now = time.monotonic()
    with _throttle_lock:
        if len(store.throttle) > 10_000:  # forget names whose tries are all old, so the table stays small
            for k in [k for k, v in store.throttle.items()
                      if isinstance(v, list) and (not v or now - v[-1] >= THROTTLE_WINDOW_S)]:
                del store.throttle[k]
        for key in keys:
            store.throttle.setdefault(key, []).append(now)


def _check_strikes(store, keys):
    """429 while any of the keys (an address, a name) has to wait after its wrong tries. Nothing is counted."""
    now = time.monotonic()
    with _throttle_lock:
        wait = max((store.throttle[k]["until"] - now for k in keys if k in store.throttle), default=0)
    if wait > 0:
        raise throttled(max(1, int(-(-wait // 1))))


def _strike(store, keys):
    """A wrong try for every key: from the fifth on, the next one waits (30 s, then twice as long each time)."""
    now = time.monotonic()
    with _throttle_lock:
        for k in keys:
            s = store.throttle.setdefault(k, {"failures": 0, "until": 0.0})
            s["failures"] += 1
            if s["failures"] >= STRIKES_FREE:
                doublings = min(s["failures"] - STRIKES_FREE, 16)
                s["until"] = now + min(STRIKES_FIRST_WAIT_S << doublings, STRIKES_MAX_WAIT_S)


def _clear_strikes(store, keys):
    with _throttle_lock:
        for k in keys:
            store.throttle.pop(k, None)


def _signed_in(req, account):
    from ..server import set_cookie
    s = req.store.start_session(account, device="Browser")
    req.set_cookie = set_cookie(s["token"])
    req.store.record(account, "sign-in", f"session:{s['id']}", "Signed in.")
    return s


# ------------------------------------------------------------------ session


def post_session(req):
    st = req.store
    body = req.json()
    _types(body, {"name": str, "password": str})
    keys = ("sign-in-name:" + body["name"].strip().lower(), "sign-in-address:" + (req.client_ip or ""))
    _check_strikes(st, keys)
    a = st.account_by_name(body["name"].strip())
    if a is None or not secrets.compare_digest(a["password"].encode(), body["password"].encode()):
        _strike(st, keys)
        st.record(a, "sign-in-failed", None, f"Wrong password for {a['name']}." if a else "Wrong name or password.",
                  address=getattr(req, "client_ip", None) or "127.0.0.1")
        raise ApiError(401, "wrong_credentials", "That name and password do not match an account.")
    _clear_strikes(st, keys)
    if a.get("two_step"):
        token = "pend_" + secrets.token_urlsafe(16)
        with st.lock:
            st.pending[token] = {"account": a["id"], "expires": time.monotonic() + PENDING_S, "tries": 0}
        return {"status": "two_step_pending", "pending": token}
    _signed_in(req, a)
    return {"status": "done", "me": me_view(a, st)}


def post_two_step(req):
    st = req.store
    body = req.json()
    _types(body, {"pending": str, "code": str})
    with st.lock:
        p = st.pending.get(body["pending"])
        if p is None or p["expires"] < time.monotonic():
            st.pending.pop(body["pending"], None)
            raise ApiError(401, "sign_in_expired", "This sign-in took too long. Start again.")
        who = st.accounts.get(p["account"])
        if who is None or not take_code(who, body["code"]):
            p["tries"] += 1
            if p["tries"] >= 5:
                st.pending.pop(body["pending"], None)
            st.record(who, "two-step-failed", None, f"Wrong two-step code for {who['name'] if who else 'an account'}.",
                      address=getattr(req, "client_ip", None) or "127.0.0.1")
            raise ApiError(401, "wrong_code", "That code is not right.")
        st.pending.pop(body["pending"], None)
        a = st.accounts.get(p["account"])
    if a is None:
        raise ApiError(401, "sign_in_expired", "This sign-in took too long. Start again.")
    _signed_in(req, a)
    return {"status": "done", "me": me_view(a, st)}


def delete_session(req):
    from ..server import clear_cookie
    token = req.session["token"]
    req.store.end_session(token)
    req.app.events.end_session(token)
    req.set_cookie = clear_cookie()
    req.store.record(req.account, "sign-out", f"session:{req.session['id']}", "Signed out.")
    return {"status": "signed_out"}


def get_me(req):
    return me_view(req.account, req.store)


# ------------------------------------------------------------------ own sessions


def _my_sessions(req):
    st = req.store
    acc = req.account
    out = []
    with st.lock:
        for s in st.sessions.values():
            if s["account"]["id"] == acc["id"]:
                out.append({"id": s["id"], "created_ms": s["created_ms"], "last_seen_ms": s["last_seen_ms"],
                            "this_one": s is req.session, "device": s["device"],
                            "address": s.get("address") or "127.0.0.1", "_token": s["token"]})
        for s in st.session_rows.get(acc["id"], []):
            out.append(dict(s, this_one=False))
    for s in out:  # as the real server: 30 days from sign-in at most, ended after 7 days unused
        s["expires_ms"] = min(s["created_ms"] + SESSION_DAYS * DAY_MS, s["last_seen_ms"] + IDLE_DAYS * DAY_MS)
    return out


def get_sessions(req):
    rows = _my_sessions(req)
    items, nxt = page(req, rows, key=lambda s: (-s["last_seen_ms"], s["id"]))
    return {"items": items, "next": nxt}


def delete_sessions(req):
    """Ends every other session of this account (this one stays; sign out with DELETE /session)."""
    st = req.store
    n = 0
    with st.lock:
        for s in _my_sessions(req):
            if s["this_one"]:
                continue
            if s.get("_token"):
                st.end_session(s["_token"])
                req.app.events.end_session(s["_token"])
            n += 1
        st.session_rows[req.account["id"]] = []
    st.record(req.account, "sessions-revoked", f"account:{req.account['id']}",
              "Ended 1 other session." if n == 1 else f"Ended {n} other sessions.")
    return {"ended": n}


def put_password(req):
    """PUT /me/password { password, new_password, code? }: anyone's own password, with the current one again (and a
    code with two-step sign-in on). The account's other sessions end; this one stays."""
    from ..store import DEV_PASSWORD
    st = req.store
    acc = req.account
    body = req.json()
    for k in body:
        if k not in ("password", "new_password", "code"):
            raise bad_input({k: "Unknown field."})
    for k in ("password", "new_password"):
        if not isinstance(body.get(k), str):
            raise bad_input({k: "Needed: text."})
    if not secrets.compare_digest(body["password"].encode(), acc.get("password", DEV_PASSWORD).encode()):
        raise ApiError(403, "wrong_password", "That is not your password.")
    if acc.get("two_step"):
        code = body.get("code")
        if not take_code(acc, code):
            raise ApiError(401, "wrong_code", "Type the code from your authenticator app.")
    new = body["new_password"]
    if not new:
        raise invalid({"new_password": "Type a password."})
    if len(new.encode()) > 1024:
        raise invalid({"new_password": "That password is too long (at most 1,024 bytes)."})
    n = 0
    with st.lock:
        acc["password"] = new
        for s in _my_sessions(req):
            if s["this_one"]:
                continue
            if s.get("_token"):
                st.end_session(s["_token"])
                req.app.events.end_session(s["_token"])
            n += 1
        st.session_rows[acc["id"]] = []
    st.record(acc, "password-changed", f"account:{acc['id']}", f"Changed the password of {acc['name']}.")
    return {"signed_out_others": n}


def get_one_session(req):
    for s in _my_sessions(req):
        if s["id"] == req.params["session"]:
            return {k: v for k, v in s.items() if not k.startswith("_")}
    raise not_found("no_such_session", "There is no such session.")


def delete_one_session(req):
    from ..server import clear_cookie
    st = req.store
    sid = req.params["session"]
    for s in _my_sessions(req):
        if s["id"] == sid:
            with st.lock:
                if s.get("_token"):
                    st.end_session(s["_token"])
                    req.app.events.end_session(s["_token"])
                rows = st.session_rows.get(req.account["id"], [])
                st.session_rows[req.account["id"]] = [x for x in rows if x["id"] != sid]
            if s["this_one"]:
                req.set_cookie = clear_cookie()
            st.record(req.account, "session-revoked", f"session:{sid}", f"Ended a session ({s.get('device') or 'a browser'}).")
            return {"ended": 1, "id": sid}
    raise not_found("no_such_session", "There is no such session.")


# ------------------------------------------------------------------ preferences

UNITS = {"distance": ("km", "mi"), "temperature": ("c", "f"), "pressure": ("bar", "psi")}
PREF_ENUMS = {"range": ("rated", "ideal"), "theme": ("light", "dark", "device"), "map_colors": ("dimmed", "original")}


def get_prefs(req):
    return req.store.get_prefs(req.account)


# preference fields in words, as the server's audit summaries name them
PREF_WORDS = {"units": "units", "range": "range kind", "theme": "theme", "language": "language",
              "interface_size": "text size", "follow_screen": "follow the screen", "map_colors": "map colours",
              "map_style": "map style", "car_order": "car order", "pinned_cars": "pinned cars", "main_car": "main car",
              "last_car": "last car"}


def patch_prefs(req):
    st = req.store
    body = req.json()
    bad, rule = {}, {}
    allowed = {"units", "range", "theme", "language", "interface_size", "follow_screen", "map_colors", "map_style", "ui_pack",
               "car_order", "pinned_cars", "main_car", "last_car"}
    for k in body:
        if k not in allowed:
            bad[k] = "Unknown field."
    if "units" in body:
        u = body["units"]
        if not isinstance(u, dict):
            bad["units"] = "Must be an object."
        else:
            for k, v in u.items():
                if k not in UNITS:
                    bad[f"units.{k}"] = "Unknown unit."
                elif v not in UNITS[k]:
                    rule[f"units.{k}"] = "Must be one of: " + ", ".join(UNITS[k]) + "."
    for k, vals in PREF_ENUMS.items():
        if k in body and body[k] not in vals:
            rule[k] = "Must be one of: " + ", ".join(vals) + "."
    if "map_style" in body:
        from .system import TILE_PRESETS
        ids = [p["id"] for p in TILE_PRESETS]
        if body["map_style"] != "default" and body["map_style"] not in ids:
            rule["map_style"] = "Must be default or one of: " + ", ".join(ids) + "."
    if "language" in body and (not isinstance(body["language"], str) or not 2 <= len(body["language"]) <= 10):
        rule["language"] = "A language code such as en or de."
    if "interface_size" in body:
        v = body["interface_size"]
        if not isinstance(v, int) or isinstance(v, bool):
            bad["interface_size"] = "Must be a whole number (percent)."
        elif not 50 <= v <= 200:
            rule["interface_size"] = "Must be between 50 and 200 (percent)."
    if "follow_screen" in body and not isinstance(body["follow_screen"], bool):
        bad["follow_screen"] = "Must be true or false."
    if "ui_pack" in body and body["ui_pack"] is not None:
        if not isinstance(body["ui_pack"], str) or not 1 <= len(body["ui_pack"]) <= 64:
            bad["ui_pack"] = "Must be a pack id, or null for the server's choice."
        elif req.app.packs.get(body["ui_pack"]) is None:
            rule["ui_pack"] = "No such UI pack on this server."
    for k, most in (("car_order", 10000), ("pinned_cars", 50)):
        if k in body:
            v = body[k]
            if not isinstance(v, list) or not all(isinstance(x, str) for x in v):
                bad[k] = "Must be a list of car ids."
            elif len(v) > most:
                rule[k] = f"At most {most} cars."
            elif any(not req.ds.exists(c) or not st.sees(req.account, c) for c in v):
                rule[k] = "Unknown car in the list."
    for k in ("main_car", "last_car"):
        if k in body and body[k] is not None:
            if not isinstance(body[k], str):
                bad[k] = "Must be a car id, or null."
            elif not req.ds.exists(body[k]) or not st.sees(req.account, body[k]):
                rule[k] = "Unknown car."
    if bad:
        raise bad_input(bad)
    if rule:
        raise invalid(rule)
    with st.lock:
        own = st.prefs.setdefault(req.account["id"], {})
        for k, v in body.items():
            if k == "units":
                own.setdefault("units", {}).update(v)
            elif k in ("car_order", "pinned_cars"):
                own[k] = list(dict.fromkeys(v))
            else:
                own[k] = v
    if "ui_pack" in body:
        packs = req.app.packs
        pid = body["ui_pack"] or packs.served
        name = (packs.get(pid) or {}).get("name") or pid
        st.record(req.account, "ui-pack-chosen", f"ui-pack:{pid}",
                  f"Chose the {name} design." if body["ui_pack"] else f"Went back to the design for everyone ({name}).")
    rest = [k for k in body if k != "ui_pack"]
    if rest:
        st.record(req.account, "preferences-changed", f"account:{req.account['id']}",
                  "Changed: " + ", ".join(PREF_WORDS.get(k, k.replace("_", " ")) for k in sorted(rest)) + ".")
    prefs = st.get_prefs(req.account)
    req.app.events.publish("prefs", prefs, account_id=req.account["id"])
    return prefs


# ------------------------------------------------------------------ accounts (admin)


def _types(body, spec):
    errs = {}
    for k, t in spec.items():
        if k not in body:
            errs[k] = "Required."
        elif not isinstance(body[k], t) or isinstance(body[k], bool) and t is not bool:
            errs[k] = "Wrong type."
    if errs:
        raise bad_input(errs)


def _name_password_rules(st, name, password, exclude=None):
    errs = {}
    if name is not None:
        n = name.strip()
        if not 1 <= len(n) <= 64:
            errs["name"] = "1 to 64 characters."
        elif any(ord(c) < 32 for c in n):
            errs["name"] = "No control characters."
        else:
            other = st.account_by_name(n)
            if other is not None and other["id"] != exclude:
                errs["name"] = "That name is taken."
    # like the real server: only an empty or a huge password is refused; the pages warn about weak ones
    if password is not None and not password:
        errs["password"] = "Type a password."
    elif password is not None and len(password.encode()) > 1024:
        errs["password"] = "That password is too long (at most 1,024 bytes)."
    return errs


def _cars_value(req, v, errs):
    if v == "all":
        return "all"
    if not isinstance(v, list) or not all(isinstance(x, str) for x in v):
        raise bad_input({"cars": 'Must be "all" or a list of car ids.'})
    if any(not req.ds.exists(c) for c in v):
        errs["cars"] = "Unknown car in the list."
    return list(dict.fromkeys(v))


def _no_per_account_two_step(body, errs):
    """The real server refuses `two_step_required: true` on one account (422): the rule is server-wide."""
    v = body.get("two_step_required")
    if v is not None and not isinstance(v, bool):
        raise bad_input({"two_step_required": "Must be true or false."})
    if v is True:
        errs["two_step_required"] = ("Two-step sign-in is required for every account or from outside the home network "
                                     "(Accounts, two-step rule), not for one account.")


def _reconfirm(req, body):
    """The acting admin's own password again (`confirm_password`, or the X-Sentry-Mate-Password header), and a code
    when that admin has two-step sign-in on: as the real server asks before every account change."""
    from .tesla import reconfirm
    reconfirm(req, body)


def list_accounts(req):
    rows = [account_view(a, req.store) for a in req.store.accounts.values()]
    items, nxt = page(req, rows, key=lambda a: (a["name"].lower(), a["id"]))
    return {"items": items, "next": nxt, "total": len(rows)}


def create_account(req):
    st = req.store
    body = req.json()
    for k in body:
        if k not in ("name", "password", "role", "cars", "two_step_required", "confirm_password", "code"):
            raise bad_input({k: "Unknown field."})
    _reconfirm(req, body)
    _types(body, {"name": str, "password": str, "role": str})
    errs = _name_password_rules(st, body["name"], body["password"])
    if errs.get("name") == "That name is taken.":
        raise conflict("name_taken", "That name is taken.")
    if body["role"] not in ("admin", "watch"):
        errs["role"] = "Must be admin or watch."
    cars = _cars_value(req, body.get("cars", "all"), errs)
    _no_per_account_two_step(body, errs)
    if errs:
        raise invalid(errs)
    a = {"id": st.new_id("a"), "name": body["name"].strip(), "role": body["role"], "cars": cars,
         "two_step": False, "password": body["password"], "created_ms": req.now,
         "version": 1}
    with st.lock:
        st.accounts[a["id"]] = a
    st.record(req.account, "account-created", f"account:{a['id']}", f"Made the account {a['name']} ({a['role']}).")
    out = account_view(a, st)
    out["_created"] = True
    return out


def _account(req):
    a = req.store.accounts.get(req.params["account"])
    if a is None:
        raise not_found("no_such_account", "There is no such account.")
    return a


def get_account(req):
    return account_view(_account(req), req.store)


def _admins(st):
    return [a for a in st.accounts.values() if a["role"] == "admin"]


def patch_account(req):
    st = req.store
    a = _account(req)
    body = req.json()
    req.version_check(a["version"])
    errs = {}
    for k in body:
        if k not in ("version", "name", "role", "cars", "two_step_required", "password", "confirm_password", "code"):
            raise bad_input({k: "Unknown field."})
    given = {k: body.pop(k) for k in ("confirm_password", "code") if k in body}
    if "name" in body and not isinstance(body["name"], str) or "password" in body and not isinstance(
            body["password"], str):
        raise bad_input({"name" if "name" in body else "password": "Wrong type."})
    if body.get("password") == "":
        # like the real server: an empty password means "keep the current one"
        body = {k: v for k, v in body.items() if k != "password"}
    errs.update(_name_password_rules(st, body.get("name"), body.get("password"), exclude=a["id"]))
    if "role" in body and body["role"] not in ("admin", "watch"):
        errs["role"] = "Must be admin or watch."
    cars = _cars_value(req, body["cars"], errs) if "cars" in body else None
    _no_per_account_two_step(body, errs)
    if errs:
        raise invalid(errs)
    # asked for only when something really changes (a page may send the unchanged role and cars along)
    if ("role" in body and body["role"] != a["role"]) or (cars is not None and cars != a["cars"]) or "password" in body:
        _reconfirm(req, given)
    if body.get("role") == "watch" and a["role"] == "admin" and len(_admins(st)) == 1:
        raise conflict("last_admin", "This is the last admin account. Make another admin first.")
    old_cars = a["cars"]
    with st.lock:
        if "name" in body:
            a["name"] = body["name"].strip()
        if "role" in body:
            a["role"] = body["role"]
        if cars is not None:
            a["cars"] = cars
        if "password" in body:
            a["password"] = body["password"]
        if not (set(body) - {"version", "two_step_required"}):
            return account_view(a, st)
        a["version"] += 1
    target = f"account:{a['id']}"
    if "role" in body:
        st.record(req.account, "role-changed", target, f"Made {a['name']} {'an admin' if a['role'] == 'admin' else 'a watch account'}.")
    if "password" in body:
        st.record(req.account, "password-changed", target, f"Changed the password of {a['name']}.")
    if cars is not None:
        seen = "every car" if cars == "all" else ", ".join(st.car_meta(c)["name"] for c in cars) or "no car"
        st.record(req.account, "cars-changed", target, f"{a['name']} now sees {seen}.")
    if "name" in body:
        st.record(req.account, "account-changed", target, f"Changed the account {a['name']}: name.")
    if cars is not None and cars != old_cars:
        req.app.events.account_cars_changed(a, old_cars, cars, st.get_prefs(a))
    return account_view(a, st)


def delete_account(req):
    st = req.store
    a = _account(req)
    body = req.json(required=False)
    for k in body:
        if k not in ("confirm_password", "code", "version"):
            raise bad_input({k: "Unknown field."})
    _reconfirm(req, body)
    if a["role"] == "admin" and len(_admins(st)) == 1:
        raise conflict("last_admin", "This is the last admin account. Make another admin first.")
    with st.lock:
        st.accounts.pop(a["id"], None)
        for tok, s in list(st.sessions.items()):
            if s["account"]["id"] == a["id"]:
                st.end_session(tok)
                req.app.events.end_session(tok)
    st.record(req.account, "account-removed", f"account:{a['id']}", f"Removed the account {a['name']}.")
    return {"deleted": True, "id": a["id"]}


# ------------------------------------------------------------------ two-step sign-in (one's own) and the rule

SECRET_ALPHABET = "ABCDEFGHIJKLMNOPQRSTUVWXYZ234567"


def _own_confirm(req, body, need_code):
    """The account's own password (403 wrong_password) and a code (401 wrong_code): the dev server takes any six
    digits, or one of the account's recovery codes (xxxx-xxxx-xxxx-xxxx, which works once)."""
    from ..store import DEV_PASSWORD
    for k in body:
        if k not in ("password", "code"):
            raise bad_input({k: "Unknown field."})
    if not isinstance(body.get("password"), str):
        raise bad_input({"password": "Needed: text."})
    if not secrets.compare_digest(body["password"].encode(), req.account.get("password", DEV_PASSWORD).encode()):
        raise ApiError(403, "wrong_password", "That is not your password.", fields={"password": "Not your password."})
    code = body.get("code")
    if code is not None and not isinstance(code, str):
        raise bad_input({"code": "Must be text."})
    if need_code:
        c = (code or "").replace(" ", "").strip()
        if not (re.fullmatch(r"\d{6}", c) or take_code(req.account, code)):
            raise ApiError(401, "wrong_code", "That code is not right.", fields={"code": "The 6-digit code."})


def get_two_step(req):
    a, st = req.account, req.store
    on = bool(a.get("two_step"))
    p = st.two_step_policy
    return {"enabled": on, "recovery_codes_left": a.get("recovery_codes_left", 10) if on else None,
            "required": {"everyone": p["everyone"], "outside": p["outside"]}}


def post_two_step_start(req):
    a, st = req.account, req.store
    req.json(required=False)
    if a.get("two_step"):
        raise conflict("two_step_on", "Two-step sign-in is on already.")
    secret = "".join(secrets.choice(SECRET_ALPHABET) for _ in range(32))
    with st.lock:
        a["two_step_pending"] = secret
    uri = f"otpauth://totp/Sentry%20Mate:{quote(a['name'])}?secret={secret}&issuer=Sentry%20Mate"
    return {"secret": secret, "uri": uri, "_created": True}


def post_two_step_confirm(req):
    a, st = req.account, req.store
    body = req.json()
    if a.get("two_step"):
        raise conflict("two_step_on", "Two-step sign-in is on already.")
    if "code" not in body:
        raise bad_input({"code": "Required: the 6-digit code."})
    if not a.get("two_step_pending"):
        raise conflict("two_step_first", "Start setting two-step sign-in up first.")
    _own_confirm(req, body, True)
    codes = new_recovery_codes()
    with st.lock:
        a.pop("two_step_pending", None)
        a["two_step"] = True
        keep_recovery_codes(a, codes)
        a["version"] += 1
    st.record(a, "two-step-on", f"account:{a['id']}", f"Turned two-step sign-in on for {a['name']}.")
    return {"enabled": True, "recovery_codes": codes}


def delete_two_step(req):
    a, st = req.account, req.store
    body = req.json()
    if not a.get("two_step"):
        raise conflict("two_step_off", "Two-step sign-in is off already.")
    _own_confirm(req, body, True)
    if _everyone(st):
        raise conflict("two_step_kept", "Two-step sign-in is required for every account: it cannot be turned off.")
    with st.lock:
        a["two_step"] = False
        a.pop("recovery_codes_left", None)
        a.pop("recovery_codes", None)
        a["version"] += 1
    st.record(a, "two-step-off", f"account:{a['id']}", f"Turned two-step sign-in off for {a['name']}.")
    return {"enabled": False}


def post_recovery_codes(req):
    a, st = req.account, req.store
    body = req.json()
    if not a.get("two_step"):
        raise conflict("two_step_off", "Two-step sign-in is off: turn it on first.")
    _own_confirm(req, body, True)
    codes = new_recovery_codes()
    with st.lock:
        keep_recovery_codes(a, codes)
    st.record(a, "recovery-codes-new", f"account:{a['id']}", f"Made new recovery codes for {a['name']}.")
    return {"recovery_codes": codes}


def get_policy(req):
    p = req.store.two_step_policy
    return {"everyone": p["everyone"], "outside": p["outside"]}


def put_policy(req):
    st = req.store
    body = req.json()
    for k in body:
        if k not in ("everyone", "outside", "confirm_password", "code"):
            raise bad_input({k: "Unknown field."})
    new = dict(st.two_step_policy)
    for k in ("everyone", "outside"):
        if k in body:
            if not isinstance(body[k], bool):
                raise bad_input({k: "Must be true or false."})
            new[k] = body[k]
    _reconfirm(req, body)
    with st.lock:
        st.two_step_policy.update(new)
    if new["everyone"]:
        words = "Two-step sign-in is now needed for every account."
    elif new["outside"]:
        words = "Two-step sign-in is now needed from outside the home network."
    else:
        words = "Two-step sign-in is no longer required."
    st.record(req.account, "two-step-policy", "settings:server", words)
    return {"everyone": new["everyone"], "outside": new["outside"]}


def get_audit(req):
    """Newest first. group=<id>[,<id>] (an entry's group, "other" for a code the list lacks) and who=<account name>
    (who=*server: the entries the server made itself) filter before paging, so every page is full."""
    from ..audit import GROUPS, group_of
    groups = req.list_param("group", allowed=GROUPS + ("other",))
    who = req.q("who")
    rows = []
    for i, e in enumerate(req.store.audit):
        e.setdefault("_seq", i)
        rows.append(dict(e, group=group_of(e["action"])))
    a, b = req.period()
    rows = [e for e in rows if a <= e["at_ms"] < b]
    if groups:
        rows = [e for e in rows if e["group"] in groups]
    if who == "*server":
        rows = [e for e in rows if e["account"] is None]
    elif who:
        rows = [e for e in rows if e["account"] is not None and e["account"].lower() == who.lower()]
    items, nxt = page(req, rows, key=lambda e: (e["at_ms"], e["_seq"]), reverse=True)
    return {"items": items, "next": nxt}


def register(router):
    # /api/v1/setup and the first-run set-up: resources/moving.py
    router.add("/api/v1/session", POST=post_session, DELETE=delete_session, public=("POST",))
    router.add("/api/v1/session/two-step", POST=post_two_step, public=("POST",))
    router.add("/api/v1/me", GET=get_me)
    router.add("/api/v1/me/sessions", GET=get_sessions, DELETE=delete_sessions)
    router.add("/api/v1/me/sessions/{session}", GET=get_one_session, DELETE=delete_one_session)
    router.add("/api/v1/me/password", PUT=put_password)
    router.add("/api/v1/me/two-step", GET=get_two_step, POST=post_two_step_start, DELETE=delete_two_step)
    router.add("/api/v1/me/two-step/confirm", POST=post_two_step_confirm)
    router.add("/api/v1/me/two-step/recovery-codes", POST=post_recovery_codes)
    router.add("/api/v1/two-step-policy", GET=get_policy, PUT=put_policy, admin=("GET", "PUT"))
    router.add("/api/v1/me/preferences", GET=get_prefs, PATCH=patch_prefs)
    router.add("/api/v1/accounts", GET=list_accounts, POST=create_account, admin=("GET", "POST"))
    router.add("/api/v1/accounts/{account}", GET=get_account, PATCH=patch_account, DELETE=delete_account,
               admin=("GET", "PATCH", "DELETE"))
    router.add("/api/v1/audit", GET=get_audit, admin=("GET",))

