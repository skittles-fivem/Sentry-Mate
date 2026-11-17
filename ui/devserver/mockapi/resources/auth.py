"""Sign-in, accounts and preferences (docs/ui-api.md, "Sign-in, accounts and preferences")."""

from __future__ import annotations

import secrets
import threading
import time

from ..conventions import page
from ..errors import ApiError, bad_input, conflict, invalid, not_found, throttled
from ..store import DEV_SETUP_CODE, DEV_TWO_STEP_CODE

_throttle_lock = threading.Lock()
THROTTLE_TRIES = 5
THROTTLE_WINDOW_S = 60
PENDING_S = 300


def account_view(a):
    return {"id": a["id"], "name": a["name"], "role": a["role"], "cars": a["cars"],
            "two_step_required": a["two_step_required"], "created_ms": a["created_ms"], "version": a["version"]}


def me_view(a):
    return {"id": a["id"], "name": a["name"], "role": a["role"], "cars": a["cars"],
            "two_step_required": a["two_step_required"]}


# ------------------------------------------------------------------ throttle


def _check_throttle(store, key):
    now = time.monotonic()
    with _throttle_lock:
        fails = [t for t in store.throttle.get(key, []) if now - t < THROTTLE_WINDOW_S]
        store.throttle[key] = fails
        if len(fails) >= THROTTLE_TRIES:
            raise throttled(max(1, int(THROTTLE_WINDOW_S - (now - fails[0])) + 1))


def _fail(store, key):
    with _throttle_lock:
        store.throttle.setdefault(key, []).append(time.monotonic())


def _signed_in(req, account):
    from ..server import set_cookie
    s = req.store.start_session(account, device="Browser")
    req.set_cookie = set_cookie(s["token"])
    req.store.record(account, "session.start", f"account:{account['id']}", "Signed in.")
    return s


# ------------------------------------------------------------------ setup and session


def get_setup(req):
    return {"needs_setup": req.store.needs_setup}


def post_setup(req):
    st = req.store
    body = req.json()
    _types(body, {"code": str, "name": str, "password": str})
    if not st.needs_setup:
        raise conflict("setup_done", "Set-up is already done. Sign in instead.")
    if body["code"] != DEV_SETUP_CODE:
        raise ApiError(403, "wrong_setup_code", "That set-up code is not right.")
    errs = _name_password_rules(st, body["name"], body["password"])
    if errs:
        raise invalid(errs)
    with st.lock:
        a = {"id": "a1", "name": body["name"].strip(), "role": "admin", "cars": "all", "two_step_required": False,
             "two_step": False, "password": body["password"], "created_ms": req.now, "version": 1}
        st.accounts[a["id"]] = a
        st.needs_setup = False
    st.record(a, "setup.done", "account:a1", "Set up the first admin account.")
    _signed_in(req, a)
    return {"status": "done", "me": me_view(a)}


def post_session(req):
    st = req.store
    body = req.json()
    _types(body, {"name": str, "password": str})
    key = body["name"].strip().lower()
    _check_throttle(st, key)
    a = st.account_by_name(body["name"].strip())
    if a is None or not secrets.compare_digest(a["password"], body["password"]):
        _fail(st, key)
        raise ApiError(401, "wrong_credentials", "That name and password do not match an account.")
    if a.get("two_step"):
        token = "pend_" + secrets.token_urlsafe(16)
        with st.lock:
            st.pending[token] = {"account": a["id"], "expires": time.monotonic() + PENDING_S, "tries": 0}
        return {"status": "two_step_pending", "pending": token}
    _signed_in(req, a)
    return {"status": "done", "me": me_view(a)}


def post_two_step(req):
    st = req.store
    body = req.json()
    _types(body, {"pending": str, "code": str})
    with st.lock:
        p = st.pending.get(body["pending"])
        if p is None or p["expires"] < time.monotonic():
            st.pending.pop(body["pending"], None)
            raise ApiError(401, "sign_in_expired", "This sign-in took too long. Start again.")
        if body["code"].strip() != DEV_TWO_STEP_CODE:
            p["tries"] += 1
            if p["tries"] >= 5:
                st.pending.pop(body["pending"], None)
            raise ApiError(401, "wrong_code", "That code is not right.")
        st.pending.pop(body["pending"], None)
        a = st.accounts.get(p["account"])
    if a is None:
        raise ApiError(401, "sign_in_expired", "This sign-in took too long. Start again.")
    _signed_in(req, a)
    return {"status": "done", "me": me_view(a)}


def delete_session(req):
    from ..server import clear_cookie
    token = req.session["token"]
    req.store.end_session(token)
    req.app.events.end_session(token)
    req.set_cookie = clear_cookie()
    req.store.record(req.account, "session.end", f"account:{req.account['id']}", "Signed out.")
    return {"status": "signed_out"}


def get_me(req):
    return me_view(req.account)


# ------------------------------------------------------------------ own sessions


def _my_sessions(req):
    st = req.store
    acc = req.account
    out = []
    with st.lock:
        for s in st.sessions.values():
            if s["account"]["id"] == acc["id"]:
                out.append({"id": s["id"], "created_ms": s["created_ms"], "last_seen_ms": s["last_seen_ms"],
                            "this_one": s is req.session, "device": s["device"], "_token": s["token"]})
        for s in st.session_rows.get(acc["id"], []):
            out.append(dict(s, this_one=False))
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
    st.record(req.account, "session.end_others", f"account:{req.account['id']}", f"Ended {n} other sessions.")
    return {"ended": n}


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
            st.record(req.account, "session.end", f"session:{sid}", "Ended a session.")
            return {"ended": 1, "id": sid}
    raise not_found("no_such_session", "There is no such session.")


# ------------------------------------------------------------------ preferences

UNITS = {"distance": ("km", "mi"), "temperature": ("c", "f"), "pressure": ("bar", "psi")}
PREF_ENUMS = {"range": ("rated", "ideal"), "theme": ("light", "dark", "device")}


def get_prefs(req):
    return req.store.get_prefs(req.account)


def patch_prefs(req):
    st = req.store
    body = req.json()
    bad, rule = {}, {}
    allowed = {"units", "range", "theme", "language", "interface_size", "follow_screen", "ui_pack", "car_order",
               "pinned_cars"}
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
    if "ui_pack" in body and (not isinstance(body["ui_pack"], str) or not 1 <= len(body["ui_pack"]) <= 64):
        rule["ui_pack"] = "A pack name of 1 to 64 characters."
    for k, most in (("car_order", 10000), ("pinned_cars", 50)):
        if k in body:
            v = body[k]
            if not isinstance(v, list) or not all(isinstance(x, str) for x in v):
                bad[k] = "Must be a list of car ids."
            elif len(v) > most:
                rule[k] = f"At most {most} cars."
            elif any(not req.ds.exists(c) or not st.sees(req.account, c) for c in v):
                rule[k] = "Unknown car in the list."
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
    st.record(req.account, "preferences.update", f"account:{req.account['id']}",
              "Changed preferences: " + ", ".join(sorted(body)) + ".")
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
    if password is not None and len(password) < 10:
        errs["password"] = "At least 10 characters."
    return errs


def _cars_value(req, v, errs):
    if v == "all":
        return "all"
    if not isinstance(v, list) or not all(isinstance(x, str) for x in v):
        raise bad_input({"cars": 'Must be "all" or a list of car ids.'})
    if any(not req.ds.exists(c) for c in v):
        errs["cars"] = "Unknown car in the list."
    return list(dict.fromkeys(v))


def list_accounts(req):
    rows = [account_view(a) for a in req.store.accounts.values()]
    items, nxt = page(req, rows, key=lambda a: (a["name"].lower(), a["id"]))
    return {"items": items, "next": nxt, "total": len(rows)}


def create_account(req):
    st = req.store
    body = req.json()
    _types(body, {"name": str, "password": str, "role": str})
    errs = _name_password_rules(st, body["name"], body["password"])
    if errs.get("name") == "That name is taken.":
        raise conflict("name_taken", "That name is taken.")
    if body["role"] not in ("admin", "watch"):
        errs["role"] = "Must be admin or watch."
    cars = _cars_value(req, body.get("cars", "all"), errs)
    two = body.get("two_step_required", False)
    if not isinstance(two, bool):
        raise bad_input({"two_step_required": "Must be true or false."})
    if errs:
        raise invalid(errs)
    a = {"id": st.new_id("a"), "name": body["name"].strip(), "role": body["role"], "cars": cars,
         "two_step_required": two, "two_step": False, "password": body["password"], "created_ms": req.now,
         "version": 1}
    with st.lock:
        st.accounts[a["id"]] = a
    st.record(req.account, "account.create", f"account:{a['id']}", f"Made the account {a['name']} ({a['role']}).")
    out = account_view(a)
    out["_created"] = True
    return out


def _account(req):
    a = req.store.accounts.get(req.params["account"])
    if a is None:
        raise not_found("no_such_account", "There is no such account.")
    return a


def get_account(req):
    return account_view(_account(req))


def _admins(st):
    return [a for a in st.accounts.values() if a["role"] == "admin"]


def patch_account(req):
    st = req.store
    a = _account(req)
    body = req.json()
    req.version_check(a["version"])
    errs = {}
    for k in body:
        if k not in ("version", "name", "role", "cars", "two_step_required", "password"):
            raise bad_input({k: "Unknown field."})
    if "name" in body and not isinstance(body["name"], str) or "password" in body and not isinstance(
            body["password"], str):
        raise bad_input({"name" if "name" in body else "password": "Wrong type."})
    errs.update(_name_password_rules(st, body.get("name"), body.get("password"), exclude=a["id"]))
    if "role" in body and body["role"] not in ("admin", "watch"):
        errs["role"] = "Must be admin or watch."
    cars = _cars_value(req, body["cars"], errs) if "cars" in body else None
    if "two_step_required" in body and not isinstance(body["two_step_required"], bool):
        raise bad_input({"two_step_required": "Must be true or false."})
    if errs:
        raise invalid(errs)
    if body.get("role") == "watch" and a["role"] == "admin" and len(_admins(st)) == 1:
        raise conflict("last_admin", "This is the last admin account. Make another admin first.")
    with st.lock:
        if "name" in body:
            a["name"] = body["name"].strip()
        if "role" in body:
            a["role"] = body["role"]
        if cars is not None:
            a["cars"] = cars
        if "two_step_required" in body:
            a["two_step_required"] = body["two_step_required"]
        if "password" in body:
            a["password"] = body["password"]
        a["version"] += 1
    changed = ", ".join(k for k in sorted(body) if k not in ("version", "password"))
    st.record(req.account, "account.update", f"account:{a['id']}",
              f"Changed the account {a['name']}: {changed or 'password'}.")
    return account_view(a)


def delete_account(req):
    st = req.store
    a = _account(req)
    if a["role"] == "admin" and len(_admins(st)) == 1:
        raise conflict("last_admin", "This is the last admin account. Make another admin first.")
    with st.lock:
        st.accounts.pop(a["id"], None)
        for tok, s in list(st.sessions.items()):
            if s["account"]["id"] == a["id"]:
                st.end_session(tok)
                req.app.events.end_session(tok)
    st.record(req.account, "account.delete", f"account:{a['id']}", f"Removed the account {a['name']}.")
    return {"deleted": True, "id": a["id"]}


def get_audit(req):
    rows = list(req.store.audit)
    for i, e in enumerate(rows):
        e.setdefault("_seq", i)
    a, b = req.period()
    rows = [e for e in rows if a <= e["at_ms"] < b]
    items, nxt = page(req, rows, key=lambda e: (e["at_ms"], e["_seq"]), reverse=True)
    return {"items": items, "next": nxt}


def register(router):
    router.add("/api/v1/setup", GET=get_setup, POST=post_setup, public=("GET", "POST"))
    router.add("/api/v1/session", POST=post_session, DELETE=delete_session, public=("POST",))
    router.add("/api/v1/session/two-step", POST=post_two_step, public=("POST",))
    router.add("/api/v1/me", GET=get_me)
    router.add("/api/v1/me/sessions", GET=get_sessions, DELETE=delete_sessions)
    router.add("/api/v1/me/sessions/{session}", GET=get_one_session, DELETE=delete_one_session)
    router.add("/api/v1/me/preferences", GET=get_prefs, PATCH=patch_prefs)
    router.add("/api/v1/accounts", GET=list_accounts, POST=create_account, admin=("GET", "POST"))
    router.add("/api/v1/accounts/{account}", GET=get_account, PATCH=patch_account, DELETE=delete_account,
               admin=("GET", "PATCH", "DELETE"))
    router.add("/api/v1/audit", GET=get_audit, admin=("GET",))

