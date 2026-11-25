"""The full rebuild (docs/ui-api.md: /cars/{car}/full-rebuild, /system/full-rebuild): made up, as the real server's
crates/engine/src/full.rs and crates/web/src/resources/full_rebuild.rs answer it.

A job started here runs on the virtual clock: each car's copying, decoding (its logs and bytes made up from its
history's length), days and switch take a set share of FULL_MS; it ends done. `--full-rebuild running` starts the
server with one of every car under way (a long one, a third through: screenshots), `--full-rebuild failed` with one
that failed on c2 (the old history kept), `--full-rebuild offered` with c1's history made with an older decoding and
the automatic run off (the notice offers it), `--full-rebuild planned` with a new version's run of every car waiting
its grace, `--full-rebuild paused` with a run paused part way after an earlier one was cancelled. The POST asks for the admin's password again (as tesla.reconfirm); one while another runs joins it
(`joined: true`). PATCH `{ when }` starts the one under way now, puts it off until tonight or pauses it: a held run
does not move on the virtual clock. Each status says why it runs (`causes`) and has the last rebuilds (`history`).
"""

from __future__ import annotations

from ..conventions import Response, json_bytes
from ..errors import ApiError, bad_input, invalid
from .tesla import reconfirm

DAY = 86_400_000
MIN = 60_000
# how long one car takes here (the real one: minutes to hours)
FULL_MS = 3 * MIN
# the shares of a car's time: copying, decoding, days, switching
SHARES = (("copying", 0.05), ("decoding", 0.80), ("days", 0.13), ("switching", 0.02))
SERVER_VERSION = "decode-3.rules-3.dbc-614c9494"
OLD_VERSION = "decode-3.rules-1.dbc-614c9494"
DOCS = "https://github.com/skittles-fivem/Sentry-Mate/blob/main/docs/rebuilds.md"
# a new version's grace before it starts by itself
GRACE_MS = 10 * MIN
POSITIONS = {"code": "server_update", "text": "Server updated: positions are worked out differently: fixes that "
             "cannot be real are left out", "link": f"{DOCS}#rules-2"}
EFFICIENCY = {"code": "server_update", "text": "Server updated: charging efficiency is worked out differently: the "
              "energies are compared over one span", "link": f"{DOCS}#rules-3"}
HISTORY_SHOWN = 5


def _owner(name):
    return {"code": "owner", "text": f"Asked for by {name}", "link": None}


def _seed_history(now, cars):
    """Three made-up rebuilds from before the start, newest first (for Data health's list)."""
    c1 = cars[0] if cars else "c1"
    c2 = cars[1] if len(cars) > 1 else c1
    return [
        {"kind": "positions", "scope": "car", "cars": [c1], "state": "done", "logs": 214,
         "causes": [{"code": "gps_layout", "text": "The car's GPS layout changed: its positions are read again", "link": None}],
         "started_ms": now - 2 * DAY - 4 * MIN, "finished_ms": now - 2 * DAY},
        {"kind": "inputs", "scope": "car", "cars": [c2], "state": "done", "logs": 0,
         "causes": [{"code": "source_order", "text": "The car's source order changed: its inputs are joined again", "link": None}],
         "started_ms": now - 5 * DAY - 2 * MIN, "finished_ms": now - 5 * DAY},
        {"kind": "full", "scope": "all", "cars": list(cars), "state": "done", "logs": 41_280,
         "causes": [POSITIONS], "started_ms": now - 9 * DAY - 130 * MIN, "finished_ms": now - 9 * DAY},
    ]


def _cars(req):
    return [c for c in req.store.ds.featured]


def _sizes(req, cid):
    """A car's made-up logs and bytes: a log a minute awake, about 180 kB each."""
    h = req.store.ds.featured.get(cid)
    days = max(1, (req.now - (h.first_ms if h else req.now - 30 * DAY)) // DAY) if h else 30
    logs = int(days * 1440 * 0.35)
    return logs, logs * 180_000, int(days)


def seed(store, cfg, now):
    """--full-rebuild: the state the server starts in."""
    store.full_rebuild = None
    store.server_settings.setdefault("full_rebuild", {"auto": True})
    store.full_stale = set()
    mode = getattr(cfg, "full_rebuild", "none")
    cars = list(store.ds.featured)
    store.full_history = _seed_history(now, cars)
    if mode == "running" and cars:
        store.full_rebuild = _job(cars, "all", "owner", "admin", now - 20 * MIN, long=True)
    elif mode == "failed" and cars:
        j = _job(cars, "all", "owner", "admin", now - 2 * 3_600_000)
        j["failed_at"] = {"index": min(1, len(cars) - 1), "share": 0.42}
        j["error"] = "Writing the new history: no space left on the disk (os error 28)"
        store.full_rebuild = j
    elif mode == "offered" and cars:
        store.server_settings["full_rebuild"]["auto"] = False
        store.full_stale = {cars[0]}
    elif mode == "planned" and cars:
        store.full_stale = set(cars)
        j = _job(cars, "all", "version", None, now - MIN, long=True)
        j["causes"] = [POSITIONS, EFFICIENCY]
        j["hold"] = {"kind": "grace", "until_ms": now - MIN + GRACE_MS, "since_ms": now - MIN}
        store.full_rebuild = j
    elif mode == "paused" and cars:
        # one cancelled 73 s ago, and the run started after it paused part way through the decoding
        store.full_history.insert(0, {"kind": "full", "scope": "all", "cars": list(cars), "state": "cancelled", "logs": 3_120,
                                      "causes": [_owner("admin")], "started_ms": now - 9 * MIN, "finished_ms": now - 73_000})
        j = _job(cars, "all", "owner", "admin", now - 50 * MIN, long=True)
        j["id"] = 2
        j["hold"] = {"kind": "paused", "until_ms": None, "since_ms": now - 20_000}
        store.full_rebuild = j
    if store.full_rebuild and mode in ("running", "failed", "paused"):
        store.full_rebuild["causes"] = [_owner("admin")]


def _job(cars, scope, reason, by, start, long=False):
    return {"id": 1, "cars": list(cars), "scope": scope, "reason": reason, "by": by, "started_ms": start,
            "per_car_ms": 6 * 3_600_000 if long else FULL_MS, "cancelled_ms": None, "failed_at": None,
            "error": None, "audited": False, "causes": [_owner(by)] if by else [], "hold": None}


def _settle_hold(now, j):
    """A hold whose time came: the run goes on (its start moved on by the time it waited)."""
    h = j.get("hold")
    if h and h.get("until_ms") is not None and now >= h["until_ms"]:
        j["started_ms"] += h["until_ms"] - h["since_ms"]
        j["hold"] = None


def _release(now, j):
    """Start a held run now."""
    h = j.get("hold")
    if h:
        j["started_ms"] += max(0, now - h["since_ms"])
        j["hold"] = None


def _view(req, j):
    """The status of job `j` at the virtual now."""
    _settle_hold(req.now, j)
    held = j.get("hold")
    # a held run does not move on: seen as it was when the hold began
    now = held["since_ms"] if held else req.now
    cars = j["cars"]
    per = j["per_car_ms"]
    total_ms = per * len(cars)
    elapsed = max(0, now - j["started_ms"])
    stop = None
    if j.get("failed_at"):
        stop = j["failed_at"]["index"] * per + int(j["failed_at"]["share"] * per)
    if j.get("cancelled_ms") is not None:
        stop = j["cancelled_ms"] - j["started_ms"]
    at = min(elapsed, stop) if stop is not None else elapsed
    state = "running"
    if j.get("failed_at") and elapsed >= stop:
        state = "failed"
    elif j.get("cancelled_ms") is not None:
        state = "cancelled"
    elif elapsed >= total_ms:
        state, at = "done", total_ms
    index = min(len(cars) - 1, int(at // per)) if cars else 0
    within = (at - index * per) / per if per else 1
    if state == "done":
        index, within = len(cars), 0
    logs_done = logs_total = bytes_done = bytes_total = days_done = days_total = 0
    cur = (0, 0, 0)  # the logs, bytes and days of the car under way
    for i, cid in enumerate(cars[: min(index + 1, len(cars))]):
        n, b, d = _sizes(req, cid)
        logs_total += n
        bytes_total += b
        days_total += d  # known from the start: the car's days (an estimate while decoding)
        if i < index or state == "done":
            logs_done += n
            bytes_done += b
            days_done += d
            continue
        cur = (n, b, d)
        f_dec = min(1.0, max(0.0, (within - 0.05) / 0.80))
        f_days = min(1.0, max(0.0, (within - 0.85) / 0.13))
        logs_done += int(n * f_dec)
        bytes_done += int(b * f_dec)  # moves with the logs
        days_done += int(d * f_days)
    phase = None
    if state == "running" and held and at == 0:
        phase = "queued"
    elif state == "running":
        acc = 0.0
        for name, share in SHARES:
            acc += share
            if within < acc:
                phase = name
                break
        phase = phase or "switching"
    shares = dict(SHARES)
    rate = rate_logs = rate_days = None
    eta = None
    if held:
        pass
    elif phase == "decoding":
        secs = per * shares["decoding"] / 1000
        rate, rate_logs = cur[1] / secs, cur[0] / secs
        eta = int((logs_total - logs_done) / rate_logs * 1000) if rate_logs else None
    elif phase == "days":
        rate_days = cur[2] / (per * shares["days"] / 1000)
        eta = int((days_total - days_done) / rate_days * 1000) if rate_days else None
    elif state == "running":
        eta = int(total_ms - at)
    percent = None
    if state == "done" or phase == "switching":
        percent = 100
    elif phase in ("copying", "queued"):
        percent = 0
    elif phase == "decoding":
        percent = 100 * min(logs_done, logs_total) // logs_total if logs_total else None
    elif phase == "days":
        percent = 100 * min(days_done, days_total) // days_total if days_total else None
    errors = []
    if state in ("running", "done") and len(cars) and (state == "done" or index > 0 or within > 0.05):
        errors.append({"car": cars[0], "log": "feedc0ffee42_20261003T0412Z_88.log.xz", "at_ms": j["started_ms"] + per // 2,
                       "message": "The log feedc0ffee42_20261003T0412Z_88.log.xz could not be decoded: its frames end "
                                  "half way (the file was cut short); it is left out as any failed log is"})
    if state == "failed":
        errors.append({"car": cars[index], "log": None, "at_ms": j["started_ms"] + at, "message": j["error"]})
    finished = j["started_ms"] + at if state != "running" else None
    return {
        "state": state, "id": str(j["id"]), "scope": j["scope"], "cars": list(cars),
        "car": cars[index] if state == "running" or state == "failed" else None,
        "cars_done": min(index, len(cars)), "cars_total": len(cars), "reason": j["reason"], "phase": phase,
        "logs": {"done": logs_done, "total": logs_total, "failed": 0}, "days": {"done": days_done, "total": days_total},
        "bytes": {"done": bytes_done, "total": bytes_total, "copied": int(bytes_total * 0.18 / max(1, len(cars)))},
        "percent": percent, "rate_bytes_per_s": rate, "rate_logs_per_s": rate_logs, "rate_days_per_s": rate_days,
        "eta_ms": eta, "started_ms": j["started_ms"], "finished_ms": finished,
        "last_progress_ms": (now - 1500) if state == "running" else finished, "stalled": False,
        "errors": errors, "error": j["error"] if state == "failed" else None, "by": j["by"],
        "causes": list(j.get("causes") or []),
        "hold": {"kind": held["kind"], "until_ms": held["until_ms"]} if held and state == "running" else None,
        "cancelling": False,
    }


def status(req, car=None):
    st = req.store
    j = st.full_rebuild
    base = {"state": "idle", "id": None, "scope": None, "cars": [], "car": None, "cars_done": 0, "cars_total": 0,
            "reason": None, "phase": None, "logs": {"done": 0, "total": 0, "failed": 0}, "days": {"done": 0, "total": 0},
            "bytes": {"done": 0, "total": 0, "copied": 0}, "percent": None, "rate_bytes_per_s": None,
            "rate_logs_per_s": None, "rate_days_per_s": None, "eta_ms": None,
            "started_ms": None, "finished_ms": None, "last_progress_ms": None, "stalled": False, "errors": [],
            "error": None, "by": None, "causes": [], "hold": None, "cancelling": False}
    v = _view(req, j) if j else base
    if j and v["state"] != "running" and not j.get("audited"):
        _audit_end(req, j, v)
    if j and v["state"] == "done":
        st.full_stale -= set(j["cars"])
    v["version"] = {"server": SERVER_VERSION, "stale_cars": sorted(st.full_stale), "auto": _auto(st)}
    v["history"] = [dict(r, took_ms=r["finished_ms"] - r["started_ms"]) for r in getattr(st, "full_history", [])[:HISTORY_SHOWN]]
    if car is not None:
        v["this_car"] = bool(j) and car in j["cars"]
        v["car_stale"] = car in st.full_stale
    return v


def _auto(st):
    return bool((st.server_settings.get("full_rebuild") or {}).get("auto", True))


def _name(req, cid):
    return req.store.car_meta(cid)["name"]


def _audit_end(req, j, v):
    j["audited"] = True
    who = "every car" if j["scope"] == "all" else _name(req, j["cars"][0])
    target = f"car:{j['cars'][0]}" if j["scope"] == "car" else None
    if v["state"] == "done":
        words = f"Full rebuild of {who} done: {v['logs']['total']:,} logs decoded again"
    elif v["state"] == "failed":
        words = f"Full rebuild of {who} failed (the old history was kept): {v['error']}"
    else:
        words = f"Full rebuild of {who} cancelled (the cars not switched yet kept their history)"
    req.store.record(None, "car-full-rebuild", target, words + ".")
    hist = getattr(req.store, "full_history", None)
    if hist is not None:
        hist.insert(0, {"kind": "full", "scope": j["scope"], "cars": list(j["cars"]), "causes": list(j.get("causes") or []),
                        "state": v["state"], "started_ms": j["started_ms"], "finished_ms": v["finished_ms"] or req.now,
                        "logs": v["logs"]["done"]})
        del hist[10:]


def _start(req, cars, scope):
    st = req.store
    body = req.json(required=False)
    bad = {k: "Unknown field." for k in body if k not in ("confirm_password", "code")}
    if bad:
        raise bad_input(bad)
    j = st.full_rebuild
    reconfirm(req, body)
    who = "every car" if scope == "all" else _name(req, cars[0])
    target = f"car:{cars[0]}" if scope == "car" else None
    if j and _view(req, j)["state"] == "running":
        # one under way takes the ask in: its cars added, the admin's cause noted, started now if it waited
        for c in cars:
            if c not in j["cars"]:
                j["cars"].append(c)
        if scope == "all":
            j["scope"] = "all"
        cause = _owner(req.account["name"])
        if cause not in j["causes"]:
            j["causes"].append(cause)
        _release(req.now, j)
        st.record(req.account, "car-full-rebuild", target,
                  f"Asked for a full rebuild while one ran: it does {who} in the same run.")
        out = status(req, cars[0] if scope == "car" else None)
        out["joined"] = True
        return Response(202, json_bytes(out))
    st.full_rebuild = _job(cars, scope, "owner", req.account["name"], req.now)
    st.full_rebuild["id"] = (j["id"] + 1) if j else 1
    st.record(req.account, "car-full-rebuild", target,
              f"Started a full rebuild of {who}: everything worked out made again from the stored data.")
    out = status(req, cars[0] if scope == "car" else None)
    out["joined"] = False
    return Response(202, json_bytes(out))


def car_get(req):
    return status(req, req.car)


def car_post(req):
    return _start(req, [req.car], "car")


def system_get(req):
    return status(req)


def system_post(req):
    return _start(req, _cars(req), "all")


def _tonight(now):
    """The next 02:00 UTC after now (the made-up server's time zone)."""
    at = now - now % DAY + 2 * 3_600_000
    return at if at > now else at + DAY


def system_patch(req):
    """PATCH { when: now | tonight | pause }: when the one under way goes on."""
    st = req.store
    body = req.json(required=False)
    bad = {k: "Unknown field." for k in body if k != "when"}
    if bad:
        raise bad_input(bad)
    when = body.get("when")
    if when not in ("now", "tonight", "pause"):
        raise invalid({"when": "One of now, tonight, pause."})
    j = st.full_rebuild
    if not j or _view(req, j)["state"] != "running":
        raise ApiError(409, "full_rebuild_not_running", "No full rebuild is under way.")
    since = (j.get("hold") or {}).get("since_ms", req.now)
    if when == "now":
        _release(req.now, j)
        words = "Started the full rebuild of {} now."
    elif when == "tonight":
        j["hold"] = {"kind": "tonight", "until_ms": _tonight(req.now), "since_ms": since}
        words = "Put the full rebuild of {} off until tonight."
    else:
        j["hold"] = {"kind": "paused", "until_ms": None, "since_ms": since}
        words = "Paused the full rebuild of {} (its progress is kept)."
    who = "every car" if j["scope"] == "all" else _name(req, j["cars"][0])
    st.record(req.account, "car-full-rebuild", f"car:{j['cars'][0]}" if j["scope"] == "car" else None, words.format(who))
    return status(req)


def system_delete(req):
    st = req.store
    body = req.json(required=False)
    if body:
        raise bad_input({k: "Unknown field." for k in body})
    j = st.full_rebuild
    if not j or _view(req, j)["state"] != "running":
        raise ApiError(409, "full_rebuild_not_running", "No full rebuild is under way.")
    held = j.get("hold")
    # (a held run stops where it was held)
    j["cancelled_ms"] = held["since_ms"] if held else req.now
    j["hold"] = None
    return status(req)


def notices(req):
    """The admins' notices: one under way, one that failed, one offered."""
    if not req.is_admin:
        return []
    st = req.store
    v = status(req)
    link = "/settings/server#full-rebuild"
    causes = v.get("causes") or []
    why = ""
    if causes:
        first = causes[0]["text"]
        why = f" ({first[0].lower()}{first[1:]}" + (f", and {len(causes) - 1} more)" if len(causes) > 1 else ")")
    what = "every car" if v["scope"] == "all" else (_name(req, v["cars"][0]) if v["cars"] else "")
    if v["state"] == "running" and v.get("hold"):
        h = v["hold"]
        if h["kind"] == "paused":
            when = "It is paused until you start it"
        elif h["kind"] == "tonight":
            when = "It waits for tonight"
        else:
            when = f"It starts in about {max(1, -(-(h['until_ms'] - req.now) // MIN))} min"
        return [{"code": "full_rebuild_planned", "level": "info", "since_ms": v["started_ms"], "fix_link": link,
                 "detail": {"causes": causes},
                 "message": f"A full rebuild of {what} is planned{why}. {when}: start it now, tonight, or pause it."}]
    if v["state"] == "running":
        return [{"code": "full_rebuild_running", "level": "info", "since_ms": v["started_ms"], "fix_link": link,
                 "detail": {"causes": causes},
                 "message": f"A full rebuild of {what} is under way{why}. Pages show the old history until it is done."}]
    if v["state"] == "failed":
        return [{"code": "full_rebuild_failed", "level": "warning", "since_ms": v["finished_ms"], "fix_link": link,
                 "message": f"The full rebuild failed; the old history was kept. {v['error']}"}]
    if st.full_stale and not _auto(st):
        names = ", ".join(_name(req, c) for c in sorted(st.full_stale))
        return [{"code": "full_rebuild_offered", "level": "warning", "since_ms": req.now - DAY, "fix_link": link,
                 "detail": {"cars": sorted(st.full_stale), "causes": [POSITIONS]},
                 "message": f"This server decodes the stored logs differently from the one that made the history of "
                            f"{names} (server updated: positions are worked out differently): a full rebuild makes it "
                            "again."}]
    return []


def register(router):
    router.add("/api/v1/cars/{car}/full-rebuild", GET=car_get, POST=car_post, admin=("GET", "POST"))
    router.add("/api/v1/system/full-rebuild", GET=system_get, POST=system_post, PATCH=system_patch,
               DELETE=system_delete, admin=("GET", "POST", "PATCH", "DELETE"))
