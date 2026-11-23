"""The full rebuild (docs/ui-api.md: /cars/{car}/full-rebuild, /system/full-rebuild): made up, as the real server's
crates/engine/src/full.rs and crates/web/src/resources/full_rebuild.rs answer it.

A job started here runs on the virtual clock: each car's copying, decoding (its logs and bytes made up from its
history's length), days and switch take a set share of FULL_MS; it ends done. `--full-rebuild running` starts the
server with one of every car under way (a long one, a third through: screenshots), `--full-rebuild failed` with one
that failed on c2 (the old history kept), `--full-rebuild offered` with c1's history made with an older decoding and
the automatic run off (the notice offers it). The POST asks for the admin's password again (as tesla.reconfirm);
one at a time (409 full_rebuild_running).
"""

from __future__ import annotations

from ..conventions import Response, json_bytes
from ..errors import ApiError, bad_input
from .tesla import reconfirm

DAY = 86_400_000
MIN = 60_000
# how long one car takes here (the real one: minutes to hours)
FULL_MS = 3 * MIN
# the shares of a car's time: copying, decoding, days, switching
SHARES = (("copying", 0.05), ("decoding", 0.80), ("days", 0.13), ("switching", 0.02))
SERVER_VERSION = "decode-3.rules-1.dbc-614c9494"
OLD_VERSION = "decode-2.rules-1.dbc-614c9494"


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


def _job(cars, scope, reason, by, start, long=False):
    return {"id": 1, "cars": list(cars), "scope": scope, "reason": reason, "by": by, "started_ms": start,
            "per_car_ms": 6 * 3_600_000 if long else FULL_MS, "cancelled_ms": None, "failed_at": None,
            "error": None, "audited": False}


def _view(req, j):
    """The status of job `j` at the virtual now."""
    now = req.now
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
    if state == "running":
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
    if phase == "decoding":
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
        "logs": {"done": logs_done, "total": logs_total}, "days": {"done": days_done, "total": days_total},
        "bytes": {"done": bytes_done, "total": bytes_total, "copied": int(bytes_total * 0.18 / max(1, len(cars)))},
        "percent": percent, "rate_bytes_per_s": rate, "rate_logs_per_s": rate_logs, "rate_days_per_s": rate_days,
        "eta_ms": eta, "started_ms": j["started_ms"], "finished_ms": finished,
        "last_progress_ms": (now - 1500) if state == "running" else finished, "stalled": False,
        "errors": errors, "error": j["error"] if state == "failed" else None, "by": j["by"],
    }


def status(req, car=None):
    st = req.store
    j = st.full_rebuild
    base = {"state": "idle", "id": None, "scope": None, "cars": [], "car": None, "cars_done": 0, "cars_total": 0,
            "reason": None, "phase": None, "logs": {"done": 0, "total": 0}, "days": {"done": 0, "total": 0},
            "bytes": {"done": 0, "total": 0, "copied": 0}, "percent": None, "rate_bytes_per_s": None,
            "rate_logs_per_s": None, "rate_days_per_s": None, "eta_ms": None,
            "started_ms": None, "finished_ms": None, "last_progress_ms": None, "stalled": False, "errors": [],
            "error": None, "by": None}
    v = _view(req, j) if j else base
    if j and v["state"] != "running" and not j.get("audited"):
        _audit_end(req, j, v)
    if j and v["state"] == "done":
        st.full_stale -= set(j["cars"])
    v["version"] = {"server": SERVER_VERSION, "stale_cars": sorted(st.full_stale), "auto": _auto(st)}
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


def _start(req, cars, scope):
    st = req.store
    body = req.json(required=False)
    bad = {k: "Unknown field." for k in body if k not in ("confirm_password", "code")}
    if bad:
        raise bad_input(bad)
    j = st.full_rebuild
    if j and _view(req, j)["state"] == "running":
        raise ApiError(409, "full_rebuild_running", "A full rebuild is under way already: wait for it, or cancel it.")
    reconfirm(req, body)
    st.full_rebuild = _job(cars, scope, "owner", req.account["name"], req.now)
    st.full_rebuild["id"] = (j["id"] + 1) if j else 1
    who = "every car" if scope == "all" else _name(req, cars[0])
    st.record(req.account, "car-full-rebuild", f"car:{cars[0]}" if scope == "car" else None,
              f"Started a full rebuild of {who}: everything worked out made again from the stored data.")
    return Response(202, json_bytes(status(req, cars[0] if scope == "car" else None)))


def car_get(req):
    return status(req, req.car)


def car_post(req):
    return _start(req, [req.car], "car")


def system_get(req):
    return status(req)


def system_post(req):
    return _start(req, _cars(req), "all")


def system_delete(req):
    st = req.store
    body = req.json(required=False)
    if body:
        raise bad_input({k: "Unknown field." for k in body})
    j = st.full_rebuild
    if not j or _view(req, j)["state"] != "running":
        raise ApiError(409, "full_rebuild_not_running", "No full rebuild is under way.")
    j["cancelled_ms"] = req.now
    return status(req)


def notices(req):
    """The admins' notices: one under way, one that failed, one offered."""
    if not req.is_admin:
        return []
    st = req.store
    v = status(req)
    link = "/settings/server#full-rebuild"
    if v["state"] == "running":
        what = "every car" if v["scope"] == "all" else _name(req, v["cars"][0])
        why = " (this server decodes the stored logs better)" if v["reason"] == "version" else ""
        return [{"code": "full_rebuild_running", "level": "info", "since_ms": v["started_ms"], "fix_link": link,
                 "message": f"A full rebuild of {what} is under way{why}. Pages show the old history until it is done."}]
    if v["state"] == "failed":
        return [{"code": "full_rebuild_failed", "level": "warning", "since_ms": v["finished_ms"], "fix_link": link,
                 "message": f"The full rebuild failed; the old history was kept. {v['error']}"}]
    if st.full_stale and not _auto(st):
        names = ", ".join(_name(req, c) for c in sorted(st.full_stale))
        return [{"code": "full_rebuild_offered", "level": "warning", "since_ms": req.now - DAY, "fix_link": link,
                 "message": f"This server decodes the stored logs better than when {names}'s history was made: a full "
                            "rebuild makes it again."}]
    return []


def register(router):
    router.add("/api/v1/cars/{car}/full-rebuild", GET=car_get, POST=car_post, admin=("GET", "POST"))
    router.add("/api/v1/system/full-rebuild", GET=system_get, POST=system_post, DELETE=system_delete,
               admin=("GET", "POST", "DELETE"))
