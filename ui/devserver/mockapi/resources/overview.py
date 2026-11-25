"""Integrations: Overview (docs/ui-api.md, "Integrations: Overview"): every connection the server keeps, on one
page, with made-up hourly counters for 7 days and the problems of that week.

Built from the mock's own state: the cars of the dataset (the CAN Bridge, one row per car), the Tesla and TeslaFi
accounts (tesla.py, teslafi.py), local TeslaMate (teslamate.py), the MQTT settings, the map tiles, address lookup
and the release check (server settings). Nothing is stored: each hour's counters are worked out from the seed and
the hour (a featured car's uploads from the minutes it was awake, so nothing comes while it sleeps; the others from
a day-time shape), and the current hour's grow with the virtual clock. Past problems are made up relative to the
start "now" (`--now-ms`); a connection in trouble now (a Tesla sign-in refused, TeslaMate's broker refusing, ...)
has an open problem since its error.

A large fleet lists the CAN Bridge of the first MAX_CAR_ROWS cars only; `totals` still counts every car.
"""

from __future__ import annotations

import math

from ..util import DAY, HOUR, MIN, h01
from . import system, tesla, teslafi, teslamate

MAX_CAR_ROWS = 50
WEEK_H = 168
# rough UTC offsets (standard time) for the day-time shapes of the synthetic cars and the server's own traffic
ZONE_OFFSET_H = {"America/Los_Angeles": -8, "America/Chicago": -6, "America/New_York": -5, "America/Denver": -7,
                 "America/Phoenix": -7}
SERVER_ZONE = "America/Los_Angeles"
STATUSES = ("live", "idle", "paused", "off", "problem")


# ------------------------------------------------------------------ shapes


def _day_shape(hour_ms, zone):
    """0 at night, up to 1 in the afternoon (local time)."""
    local = ((hour_ms // HOUR) + ZONE_OFFSET_H.get(zone, -8)) % 24
    if local < 6 or local >= 23:
        return 0.0
    return max(0.0, math.sin(math.pi * (local - 6 + 0.5) / 17))


def _noise(seed, cid, hour_ms):
    return 0.75 + 0.5 * h01("overview", seed, cid, hour_ms)


def _awake_ms(h, a, b, now):
    """How long a featured car was awake with its CAN Bridge sending in [a, b) (up to now)."""
    b = min(b, now)
    if b <= a:
        return 0
    out = 0
    for s in h.segs_between(a, b, now):
        if s.kind == "gap" or s.source == "teslamate":
            continue
        if s.kind == "parked":
            for x, y, *_ in s.awake_spans():
                out += max(0, min(y, b) - max(x, a))
        else:
            out += max(0, min(s.end, b) - max(s.start, a))
    return out


def _awake_now(h, now):
    s = h.seg_at(now)
    if s is None or s.kind == "gap":
        return False
    return s.kind != "parked" or s.state_at(now) == "parked"


def _last_awake(h, now):
    """The last moment before now the car was awake with data (None if never)."""
    i = h.seg_index_at(now)
    if i is None:
        import bisect
        i = bisect.bisect_right(h.starts, now) - 1
    while i >= 0:
        s = h.segs[i]
        if s.kind != "gap" and s.source != "teslamate" and s.start <= now:
            if s.kind == "parked":
                ends = [min(y, now) for x, y, *_ in s.awake_spans() if x <= now]
                if ends:
                    return max(ends)
            else:
                return min(s.end, now)
        i -= 1
    return None


# ------------------------------------------------------------------ the connections


class Conn:
    """One connection: what it is, its status now, and how to count an hour."""

    def __init__(self, cid, kind, label, link, unit, car=None, account=None):
        self.id, self.kind, self.label, self.link, self.unit = cid, kind, label, link, unit
        self.car, self.account = car, account
        self.status, self.reason = "live", None
        self.last_ok_ms = self.next_ms = self.pace = self.pace_s = None
        self.cap = None
        self.rate = lambda a, b: 0  # tries in [a, b)
        self.open_since = None  # an open problem: (start_ms, code, message)
        self.last_error = None  # the input's own last error {code, message, at_ms}
        self.extra = []  # [(start_ms, count, errors)]: single tries to add (a log that failed its check)


def _smo_featured(req, car, now):
    h = req.ds.history(car)
    name = req.store.car_meta(car)["name"]
    c = Conn(f"smo:{car}", "smo", f"CAN Bridge: {name}", f"/cars/{car}/health", "uploads", car=car)
    c.rate = lambda a, b: _awake_ms(h, a, b, now) // MIN  # a log a minute while awake
    seg = h.seg_at(now)
    last = _last_awake(h, now)
    if seg is not None and seg.kind == "gap":
        # back to the first of the gaps the car is in now (a log that failed its check, then no log at all)
        i = h.seg_index_at(now)
        while i > 0 and h.segs[i - 1].kind == "gap" and h.segs[i - 1].end >= h.segs[i].start:
            i -= 1
        seg = h.segs[i]
        c.status, c.reason = "problem", "quiet"
        failed = getattr(seg, "reason", None) == "failed_check"
        c.open_since = (seg.start, "quiet", "Nothing has come from the CAN Bridge since its last log, which "
                                            "failed its check." if failed else
                        "Nothing has come from the CAN Bridge, though the car was last seen awake.")
        if failed:
            c.extra.append((seg.start, 1, 1))
            c.last_error = {"code": "log_check_failed", "message": "The last log failed its check.",
                            "at_ms": seg.start}
        c.last_ok_ms = last
    elif _awake_now(h, now):
        c.last_ok_ms = now - now % MIN
        c.next_ms = c.last_ok_ms + MIN
        c.pace_s = 60
    else:
        c.status, c.reason = "idle", "car_asleep"
        c.last_ok_ms = last
    return c


def _smo_synthetic(req, i, now):
    syn = req.ds.synthetic
    seed = req.app.cfg.seed
    car = f"c{i}"
    st = syn.state(i)
    zone = req.ds.car_tz(car)
    c = Conn(f"smo:{car}", "smo", f"CAN Bridge: {syn.name(i)}", f"/cars/{car}/health", "uploads", car=car)
    last = req.app.cfg.now_ms - syn.age_ms(i)
    live = st in ("driving", "charging", "parked")
    weight = 0.15 + 0.35 * h01("overview-car", seed, i)

    def rate(a, b):
        if not live and a >= last:
            return 0
        part = (min(b, now) - a) / HOUR
        base = 60 * min(1.0, weight * (0.2 + _day_shape(a, zone)) * _noise(seed, car, a))
        if live and a >= now - now % HOUR - HOUR:
            base = max(base, 45)  # awake now: a log a minute
        return int(base * max(0.0, min(1.0, part)))

    c.rate = rate
    if live:
        c.last_ok_ms = now - now % MIN
        c.next_ms = c.last_ok_ms + MIN
        c.pace_s = 60
    else:
        c.status, c.reason = "idle", "car_asleep" if st == "asleep" else "parked"
        c.last_ok_ms = last
    return c


def _synthetic_status(st):
    return "live" if st in ("driving", "charging", "parked") else "idle"


def _tesla_rows(req, now):
    out = []
    t = req.store.tesla
    q = t["settings"]["poll"]
    # the waits in seconds (the real server's pace_s is whole seconds too)
    p = {"list_s": q["list_ms"] / 1000, "parked_s": q["idle_ms"] / 1000, "charging_s": q["charging_ms"] / 1000,
         "driving_s": q["driving_ms"] / 1000}
    word = "default" if q == tesla.POLL_DEFAULT else "custom"
    for a in t["accounts"].values():
        c = Conn(f"tesla:{a['id']}", "tesla", f"Tesla API: {a['label']}", "/settings/integrations#tesla", "requests",
                 account=a["id"])
        c.pace, c.cap = word, None  # no cap of its own: Tesla does the limiting
        cars = [x["match"] for x in a["cars"] if x["match"] not in (None, "new", "leave_out")]
        hs = [req.ds.history(x) for x in cars if req.ds.history(x) is not None]

        def rate(x, y, hs=hs, p=p):
            part = max(0, min(y, now) - x)
            n = part / 1000 / p["list_s"]  # the vehicle list, asleep or not
            for h in hs:
                n += _awake_ms(h, x, y, now) / 1000 / p["parked_s"]
            return int(n)

        c.rate = rate
        word = tesla._status_word(a)
        modes = [tesla._mode(req, a, x)[0] for x in a["cars"]]
        steps = {"driving": q["driving_ms"], "charging": q["charging_ms"], "online_idle": q["idle_ms"],
                 "letting_sleep": 15 * 60_000, "watching_list": q["list_ms"], "backoff": 120_000}
        step_ms = min((steps[m] for m in modes if m in steps), default=None)
        c.last_error = a["last_error"]
        if word == "paused":
            c.status, c.reason, c.pace_s = "paused", "paused", None
            c.last_ok_ms = a["last_ok_ms"]
        elif word in ("sign_in_needed", "error"):
            code = "token_refused" if word == "sign_in_needed" else (a["last_error"] or {}).get("code", "unreachable")
            c.status, c.reason = "problem", code
            since = (a["last_error"] or {}).get("at_ms") or now
            c.open_since = (since, code, (a["last_error"] or {}).get("message") or "Tesla refuses the account.")
            c.last_ok_ms = a["last_ok_ms"]
            c.pace_s = 120 if word == "error" else None
            c.next_ms = (now - now % 120_000 + 120_000) if word == "error" else None
        elif tesla.limited_until(req, a):
            until = tesla.limited_until(req, a)
            c.status, c.reason = "problem", "rate_limited"
            c.open_since = (until - 120_000, "rate_limited", "Tesla is limiting requests")
            c.last_ok_ms, c.next_ms, c.pace_s = a["last_ok_ms"], until, step_ms // 1000 if step_ms else None
        else:
            c.pace_s = step_ms // 1000 if step_ms else None
            c.last_ok_ms = now - now % step_ms if step_ms else a["last_ok_ms"]
            c.next_ms = c.last_ok_ms + step_ms if step_ms else None
            if not any(m in ("driving", "charging", "online_idle", "letting_sleep") for m in modes):
                c.status, c.reason = "idle", "car_asleep"
        out.append(c)
    return out


def _teslafi_rows(req, now):
    out = []
    t = req.store.teslafi
    poll = teslafi._poll(req)["poll"]
    for a in t["accounts"].values():
        c = Conn(f"teslafi:{a['id']}", "teslafi", f"TeslaFi: {a['label']}", "/settings/integrations#teslafi",
                 "requests", account=a["id"])
        c.pace, c.cap = poll, None  # no cap of its own: TeslaFi does the limiting
        mode, _ = teslafi._mode(req, a)
        step = teslafi._step(mode, poll)
        car = next((x["match"] for x in a["cars"] if teslafi._matched(x)), None)
        h = req.ds.history(car) if car else None
        pp = teslafi.PACES[poll]

        def rate(x, y, h=h, pp=pp):
            part = max(0, min(y, now) - x)
            awake = _awake_ms(h, x, y, now) if h is not None else 0
            # TeslaFi's own copy of the car: parked polls while the car rests (c3's CAN Bridge is quiet, TeslaFi
            # still sees it parked)
            return int(awake / 1000 / pp["driving_s"] + (part - awake) / 1000 / pp["parked_s"])

        c.rate = rate
        word = teslafi._status_word(a)
        c.last_error = a["last_error"]
        if word == "paused":
            c.status, c.reason = "paused", "paused"
            c.last_ok_ms = a["last_ok_ms"]
        elif word in ("token_refused", "error"):
            code = "token_refused" if word == "token_refused" else (a["last_error"] or {}).get("code", "unreachable")
            c.status, c.reason = "problem", code
            since = (a["last_error"] or {}).get("at_ms") or now
            c.open_since = (since, code, (a["last_error"] or {}).get("message") or "TeslaFi refuses the token.")
            c.last_ok_ms = a["last_ok_ms"]
        elif mode == "off":
            c.status, c.reason = "off", "no_car"
            c.rate = lambda x, y: 0
        elif teslafi.limited_until(req, a):
            until = teslafi.limited_until(req, a)
            c.status, c.reason = "problem", "rate_limited"
            c.open_since = (until - 120_000, "rate_limited", "TeslaFi is limiting requests")
            c.last_ok_ms, c.next_ms, c.pace_s = a["last_ok_ms"], until, step // 1000 if step else None
        else:
            c.pace_s = step // 1000 if step else None
            c.last_ok_ms = now - now % step if step else a["last_ok_ms"]
            c.next_ms = c.last_ok_ms + step if step else None
            if mode == "asleep":
                c.status, c.reason = "idle", "car_asleep"
            elif mode == "idle":
                c.status, c.reason = "idle", "parked"
        out.append(c)
    return out


def _teslamate_rows(req, now):
    seed = req.app.cfg.seed
    t = req.store.teslamate
    ms, ds = teslamate._mqtt_status(req, t), teslamate._db_status(req, t)
    m = Conn("teslamate_mqtt", "teslamate_mqtt", "Local TeslaMate (MQTT)", "/settings/integrations#teslamate",
             "messages")
    m.rate = lambda a, b: int(2880 * (0.1 + 0.9 * _day_shape(a, SERVER_ZONE)) * _noise(seed, "tm", a)
                              * max(0.0, min(1.0, (min(b, now) - a) / HOUR)))
    m.last_ok_ms = ms["last_message_ms"]
    m.last_error = ms["last_error"]
    if ms["state"] == "off":
        m.status, m.reason = "off", "not_added"
        m.rate = lambda a, b: 0
    elif ms["state"] == "paused":
        m.status, m.reason = "paused", "paused"
    elif ms["state"] == "error":
        e = ms["last_error"]
        m.status, m.reason = "problem", e["code"]
        m.open_since = (e["at_ms"], e["code"], e["message"])
    elif t.get("mqtt_quiet"):
        m.status, m.reason = "problem", "quiet"
        m.open_since = (t["mqtt_quiet"], "quiet", "The broker is connected but sends nothing.")
    else:
        m.next_ms = m.last_ok_ms + 5_000 if m.last_ok_ms else None

    d = Conn("teslamate_db", "teslamate_db", "Local TeslaMate (database)", "/settings/integrations#teslamate",
             "requests")
    step_s = t["database"]["interval_s"]
    d.rate = lambda a, b: int(max(0, min(b, now) - a) / 1000 / step_s)
    d.last_ok_ms, d.next_ms, d.last_error = ds["last_sync_ms"], ds["next_sync_ms"], ds["last_error"]
    if ds["state"] == "off":
        d.status, d.reason = "off", "not_added" if not t["enabled"] else "turned_off"
        d.rate = lambda a, b: 0
        d.next_ms = None
    elif ds["state"] == "paused":
        d.status, d.reason = "paused", "paused"
    elif ds["state"] == "error":
        e = ds["last_error"]
        d.status, d.reason = "problem", e["code"]
        d.open_since = (e["at_ms"], e["code"], e["message"])
        d.pace_s = step_s
    else:
        d.pace_s = step_s
    return [m, d]


def _server_rows(req, now):
    seed = req.app.cfg.seed
    st = req.store
    ss = st.server_settings
    mq = st.mqtt
    out = []

    def part(a, b):
        return max(0.0, min(1.0, (min(b, now) - a) / HOUR))

    c = Conn("mqtt", "mqtt", "MQTT broker", "/settings/integrations#mqtt", "messages")
    c.rate = lambda a, b: int(3600 * (0.15 + 0.85 * _day_shape(a, SERVER_ZONE)) * _noise(seed, "mqtt", a)
                              * part(a, b))
    s = mq["status"]
    if s.get("last_error"):
        c.last_error = {"code": s["last_error"], "message": s.get("last_error_message") or s["last_error"],
                        "at_ms": s["last_error_ms"]}
    if not mq["enabled"]:
        c.status, c.reason = "off", "turned_off"
        c.rate = lambda a, b: 0
    elif not s["connected"]:
        c.status, c.reason = "problem", "unreachable"
        since = s.get("last_error_ms") or now
        c.open_since = (since, s.get("last_error") or "unreachable",
                        s.get("last_error_message") or f"The broker {mq['host']}:{mq['port']} cannot be reached.")
    else:
        c.last_ok_ms = now - now % 1000 - 1000
    out.append(c)

    c = Conn("tiles", "tiles", "Online map tiles", "/settings/display#maps", "requests")
    if ss["tiles"]["mode"] == "online":
        c.rate = lambda a, b: int(350 * _day_shape(a, SERVER_ZONE) ** 2 * _noise(seed, "tiles", a) * part(a, b))
        c.last_ok_ms = now - now % MIN - 3 * MIN
    else:
        c.status, c.reason = "off", "offline_mode"
    out.append(c)

    c = Conn("geocoding", "geocoding", "Address lookup", "/settings/display#addresses", "requests")
    if ss["geocode"]["mode"] == "online" and ss["geocode"].get("url"):
        c.rate = lambda a, b: int(9 * _day_shape(a, SERVER_ZONE) * _noise(seed, "geo", a) * part(a, b))
        c.last_ok_ms = now - now % MIN - 40 * MIN
    else:
        c.status, c.reason = "off", "offline_mode"
    out.append(c)

    c = Conn("release_check", "release_check", "Release check", "/settings/display#release", "requests")
    checked = st.release_checked_ms
    if ss["release_check"]["enabled"]:
        anchor = checked if checked is not None else req.app.cfg.now_ms - 2 * HOUR

        def rate(a, b, anchor=anchor):
            # one check a day, at the hour of the last one
            k = math.ceil((a - anchor) / DAY)
            t0 = anchor + k * DAY
            return 1 if a <= t0 < min(b, now + 1) else 0

        c.rate = rate
        c.last_ok_ms = checked
        c.next_ms = (checked if checked is not None else now) + DAY
        c.pace_s = DAY // 1000
        if st.release_result == "error":
            c.status, c.reason = "problem", "rate_limited"
            msg = "GitHub's rate limit for this address is used up."
            c.open_since = (anchor, "rate_limited", msg)
            c.last_error = {"code": "rate_limited", "message": msg, "at_ms": anchor}
            c.next_ms = now + 40 * MIN
    else:
        c.status, c.reason = "off", "turned_off"
    out.append(c)
    return out


# ------------------------------------------------------------------ problems


def _seeded_problems(req, conns):
    """The week's made-up problems that have ended (relative to the start "now"), for the connections that have
    them; plus the CAN Bridge of c1 offline for an afternoon two days before (the dataset's own story)."""
    seed = req.app.cfg.seed
    n0 = req.app.cfg.now_ms
    d0 = n0 - n0 % DAY

    def j(name):
        return int(h01("overview-problem", seed, name) * 20) * MIN

    plan = [
        ("smo:c1", d0 - 2 * DAY + 12 * HOUR, 6 * HOUR, "unreachable",
         "The CAN Bridge could not reach the server: its logs waited in the car."),
        ("tesla:ta1", d0 - 5 * DAY + 19 * HOUR + j("tesla"), 100 * MIN, "tesla_unreachable",
         "Tesla answered every request with an error (503)."),
        ("teslamate_mqtt", d0 - 4 * DAY + 1 * HOUR + j("tm"), 48 * MIN, "connection_refused",
         "Local TeslaMate's MQTT broker refused the connection."),
        ("mqtt", d0 - 3 * DAY + 22 * HOUR + j("mqtt"), 12 * MIN, "connection_refused",
         "broker.local:8883 refused the connection."),
        ("tiles", d0 - 6 * DAY + 20 * HOUR + j("tiles"), 25 * MIN, "unreachable",
         "The tile server did not answer (timed out)."),
    ]
    by_id = {c.id: c for c in conns}
    out = []
    for cid, start, dur, code, msg in plan:
        c = by_id.get(cid)
        if c is None or c.status == "off":
            continue
        out.append({"connection": cid, "start_ms": start, "end_ms": start + dur, "code": code, "message": msg})
    return out


def _problems(req, conns, now):
    week0 = now - 7 * DAY
    plist = _seeded_problems(req, conns)
    for c in conns:
        if c.open_since is not None:
            start, code, msg = c.open_since
            plist.append({"connection": c.id, "start_ms": min(start, now), "end_ms": None, "code": code,
                          "message": msg})
    by_id = {c.id: c for c in conns}
    out = []
    for p in plist:
        if p["start_ms"] > now or (p["end_ms"] is not None and p["end_ms"] <= week0):
            continue
        c = by_id[p["connection"]]
        end = p["end_ms"]
        out.append({"id": f"{p['connection']}@{p['start_ms']}", "connection": c.id, "kind": c.kind,
                    "label": c.label, "start_ms": p["start_ms"], "end_ms": end,
                    "duration_ms": (end if end is not None else now) - p["start_ms"], "resolved": end is not None,
                    "code": p["code"], "message": p["message"]})
    out.sort(key=lambda p: (-p["start_ms"], p["connection"]))
    return out


def _overlap(spans, a, b):
    return sum(max(0, min(e, b) - max(s, a)) for s, e in spans)


# ------------------------------------------------------------------ counting


def _hours(req, c, spans, now):
    """[(hour_ms, count, errors)] for the 168 hours up to the current one, oldest first."""
    seed = req.app.cfg.seed
    cache = req.app.cache.setdefault("overview:hours", {})
    if len(cache) > 50_000:
        cache.clear()
    cur = now - now % HOUR
    out = []
    for k in range(WEEK_H - 1, -1, -1):
        a = cur - k * HOUR
        b = a + HOUR
        key = (c.id, c.status, a, len(spans))
        done = b <= now
        if done and key in cache:
            out.append(cache[key])
            continue
        n = c.rate(a, b)
        bad = 0
        if n:
            elapsed = min(b, now) - a
            ov = _overlap(spans, a, min(b, now))
            if ov:
                bad = min(n, round(n * ov / elapsed))
            elif h01("overview-err", seed, c.id, a) < 0.025:
                bad = 1  # the odd failed try
        for t, n1, e1 in c.extra:
            if a <= t < b and t <= now:
                n += n1
                bad += e1
        row = (a, n, bad)
        if done:
            cache[key] = row
        out.append(row)
    return out


def _pct(ok, n):
    return None if n == 0 else round(100.0 * ok / n, 1)


def _view(req, c, problems, now):
    spans = [(p["start_ms"], p["end_ms"] if p["end_ms"] is not None else now) for p in problems
             if p["connection"] == c.id]
    hours = _hours(req, c, spans, now)
    midnight = now - now % DAY

    def period(rows):
        n = sum(r[1] for r in rows)
        e = sum(r[2] for r in rows)
        return {"count": n, "ok": n - e, "errors": e, "success_pct": _pct(n - e, n)}

    off = c.status == "off"

    def up(ms):
        if off:
            return None
        return round(100.0 * (1 - _overlap(spans, now - ms, now) / ms), 1)

    last_error = None
    if c.last_error:
        last_error = {k: c.last_error[k] for k in ("code", "message", "at_ms")}
    mine = [p for p in problems if p["connection"] == c.id]
    if mine:
        p = mine[0]  # newest first
        at = p["end_ms"] - MIN if p["end_ms"] is not None else p["start_ms"]
        if last_error is None or (last_error["at_ms"] or 0) < at:
            last_error = {"code": p["code"], "message": p["message"], "at_ms": at}
    return {
        "id": c.id, "kind": c.kind, "label": c.label, "car": c.car, "account": c.account, "status": c.status,
        "status_reason": c.reason, "last_ok_ms": c.last_ok_ms, "next_ms": c.next_ms, "pace": c.pace,
        "pace_s": c.pace_s, "unit": c.unit,
        "today": {"count": sum(r[1] for r in hours if r[0] >= midnight), "cap": c.cap},
        "day": period(hours[-24:]), "week": period(hours),
        "uptime": {"day_pct": up(DAY), "week_pct": up(7 * DAY)},
        "last_error": last_error,
        "hourly": [{"hour_ms": a, "count": n, "errors": e} for a, n, e in hours[-24:]],
        # a CAN Bridge row: the car's logs the server is missing and has asked the car for (null on other rows)
        "missing_logs": system.missing_count(req, c.car) if c.kind == "smo" else None,
        # a CAN Bridge row: when the newest log arrived (here a log a minute while awake: its last contact)
        "newest_log_ms": c.last_ok_ms if c.kind == "smo" else None,
        "link": c.link,
    }


# ------------------------------------------------------------------ the resource


def _cars(req):
    """(featured car ids not sold, number of synthetic cars)."""
    feat = [cid for cid in req.ds.car_ids_featured() if not req.store.car_meta(cid).get("sold")]
    return feat, req.ds.synthetic.count


def overview(req):
    now = req.now
    feat, n_syn = _cars(req)
    conns = [_smo_featured(req, car, now) for car in feat]
    shown_syn = max(0, min(n_syn, MAX_CAR_ROWS - len(conns)))
    conns += [_smo_synthetic(req, i, now) for i in range(4, 4 + shown_syn)]
    conns += _tesla_rows(req, now) + _teslafi_rows(req, now) + _teslamate_rows(req, now) + _server_rows(req, now)
    problems = _problems(req, conns, now)
    totals = {s: 0 for s in STATUSES}
    for c in conns:
        totals[c.status] += 1
    if n_syn > shown_syn:  # the cars not listed: counted from the fleet's cached state counts
        by, _stale = req.ds.synthetic.counts()
        by = dict(by)
        for i in range(4, 4 + shown_syn):
            by[req.ds.synthetic.state(i)] -= 1
        for st, n in by.items():
            totals[_synthetic_status(st)] += n
    return {"read_ms": now, "connections": [_view(req, c, problems, now) for c in conns], "problems": problems,
            "totals": totals}


def register(router):
    router.add("/api/v1/integrations/overview", GET=overview, admin=("GET",))
