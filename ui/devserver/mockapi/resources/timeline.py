"""Timeline and states (docs/ui-api.md, "Timeline and states")."""

from __future__ import annotations

from ..conventions import page
from ..errors import bad_input
from ..live import delay_s
from ..placing import drive_pids, eff_pid, piece_pid, place_obj
from ..rows import in_progress, timeline_row
from ..util import DAY, r
from .. import sources as S

KINDS = ("drive", "charge", "parked", "gap", "update")
ACT_STATE = {"driving": ("online", ("vehicle", "chassis")), "charging": ("online", ("vehicle", "chassis")),
             "online": ("online", ("vehicle", "chassis")), "updating": ("online", ("vehicle", "chassis")),
             "online_vehicle_bus_only": ("online", ("vehicle",)), "asleep": ("asleep", ()),
             "unknown": ("unknown", ())}


# The words a page shows for the codes, so that q finds a row by what it reads (docs/ui-api.md, Decisions).
KIND_TEXT = {"drive": "drive driving", "charge": "charge charging", "parked": "parked",
             "gap": "gap in the data no data missing", "update": "software update"}
GAP_TEXT = {"no_log": "no log arrived for this time", "not_recorded": "not recorded recording was paused",
            "failed_check": "the log failed its check", "dropped_frames": "frames were dropped"}
AWAKE_TEXT = {"sentry": "sentry mode", "keep_climate_on": "keep-climate on keep climate", "keep_climate_dog": "dog mode",
              "keep_climate_camp": "camp mode", "user_present": "someone in the car",
              "preconditioning_cabin": "cabin preconditioning", "preconditioning_battery": "battery preconditioning",
              "charging": "charging", "keep_warm": "battery kept warm", "door_open": "a door open",
              "accessory_power": "accessory power on"}


def _text(row):
    parts = []
    for side in ("start", "end"):
        p = row.get(side)
        if p:
            parts.append(p.get("label") or "")
            parts.append(p.get("address") or "")
            if p.get("place"):
                parts.append(p["place"]["name"])
    parts += [row["kind"], KIND_TEXT.get(row["kind"], ""), row.get("gap_reason") or "",
              GAP_TEXT.get(row.get("gap_reason"), "")]
    for x in row.get("awake_reasons") or []:
        parts += [x, AWAKE_TEXT.get(x, x.replace("_", " "))]
    if row.get("version"):
        parts.append(row["version"])
    return " ".join(parts).lower()


def _pids(app, s, now):
    """The places a piece is at now (live matching, as its row shows them)."""
    if s.kind == "drive":
        a, b = drive_pids(app, s)
        return (a, None if s.cut_ms is not None or in_progress(s, now) else b)
    if s.kind == "gap":
        return tuple(eff_pid(app, p[0], p[1], None) for p in (s.before, s.after) if p)
    return (piece_pid(app, s),)


def get_timeline(req):
    a, b = req.period()
    kinds = req.list_param("kinds", KINDS)
    place = req.q("place")
    if place and place_obj(req.app, place) is None:
        raise bad_input({"place": "Unknown place."})
    q = (req.q("q") or "").strip().lower()
    h = req.history()
    rows = []
    if h is not None:
        segs = [s for s in h.segs_between(a, b, req.now) if a <= s.start < b]
        if kinds:
            segs = [s for s in segs if s.kind in kinds]
        # overlaps: show the chosen side's drives and charges
        segs = [s for s in segs if s.kind not in ("drive", "charge") or h.shown(s)]
        extra = [s for s in h.shadow if a <= s.start < min(b, req.now + 1) and h.shown(s)
                 and (not kinds or s.kind in kinds)]
        # cheap filters before building rows
        if place:
            segs = [s for s in segs + extra if place in _pids(req.app, s, req.now)]
            extra = []
        if not q:  # page the pieces first, then build only this page's rows
            chunk, nxt = page(req, segs + extra, key=lambda s: (s.start, s.kind), reverse=True)
            items = [timeline_row(req.app, h, s, req.now) for s in chunk]
            return {"car": req.car, "time_zone": req.tz(), "items": items, "next": nxt}
        built = [timeline_row(req.app, h, s, req.now) for s in segs + extra]
        rows = [x for x in built if q in _text(x)]
    items, nxt = page(req, rows, key=lambda x: (x["start_ms"], x["kind"]), reverse=True)
    return {"car": req.car, "time_zone": req.tz(), "items": items, "next": nxt}


def _states(h, a, b, now):
    out = []
    for s in h.segs_between(a, b, now):
        for kind, x, y in s.activity():
            x, y = max(x, a), min(y, b, now)
            if y <= x:
                continue
            st, buses = ACT_STATE[kind]
            if out and out[-1]["state"] == st and tuple(out[-1]["buses"]) == buses and out[-1]["end_ms"] == x \
                    and out[-1]["sources"] == S.of_seg(s)["sources"]:
                out[-1]["end_ms"] = y
            else:
                out.append({"id": f"state:{x}", "state": st, "start_ms": x, "end_ms": y, "buses": list(buses),
                            **S.of_seg(s)})
    return out


def get_states(req):
    a, b = req.period()
    h = req.history()
    now = req.now
    if h is None:
        return {"car": req.car, "time_zone": req.tz(), "items": [], "next": None,
                "current": {"state": "unknown", "since_ms": None, "at_ms": None}, "last_change_ms": None,
                "share_pct": {"parked": None, "asleep": None, "awake": None, "unknown": None}}
    lo = max(a, h.first_ms)
    hi = min(b, now)
    spans = _states(h, lo, hi, now)
    tot = {"online": 0, "asleep": 0, "unknown": 0}
    driving = 0
    for s in h.segs_between(lo, hi, now):
        if s.kind == "drive":
            x, y = max(s.start, lo), min(s.data_end, hi)
            driving += max(0, y - x)
    for sp in spans:
        tot[sp["state"]] += sp["end_ms"] - sp["start_ms"]
    span = max(1, hi - lo)
    known = span - tot["unknown"]  # parked is a share of the known time (docs/ui-api.md); none known gives null
    share = {"parked": r(100 * max(0, known - driving) / known, 1) if known > 0 else None, "asleep": r(100 * tot["asleep"] / span, 1),
             "awake": r(100 * tot["online"] / span, 1), "unknown": r(100 * tot["unknown"] / span, 1)}
    # the current state, at the newest data
    tv = now - delay_s(now) * 1000
    cur_spans = _states(h, tv - 3 * DAY, tv, now)
    cur = cur_spans[-1] if cur_spans else None
    # rows belong to the period by their start and keep their own start, end and id: work them out on a
    # wider window (the shares above stay clipped to the period)
    wide = _states(h, max(h.first_ms, lo - 3 * DAY), min(hi + 14 * DAY, now), now) if hi > lo else []
    rows = [dict(sp, end_ms=None if sp["end_ms"] >= now - 1 else sp["end_ms"]) for sp in wide if a <= sp["start_ms"] < b]
    items, nxt = page(req, rows, key=lambda x: (x["start_ms"],), reverse=True)
    return {
        "car": req.car, "time_zone": req.tz(), "items": items, "next": nxt,
        "current": {"state": cur["state"] if cur else "unknown", "since_ms": cur["start_ms"] if cur else None,
                    "at_ms": tv},
        "last_change_ms": cur["start_ms"] if cur else None,
        "share_pct": share,
    }


def get_activity(req):
    a, b = req.period(DAY)
    h = req.history()
    items = []
    if h is not None:
        for s in h.segs_between(a, b, req.now):
            ref = None
            if s.kind in ("drive", "charge", "update"):
                ref = {s.kind: s.id}
            for kind, x, y in s.activity():
                x, y = max(x, a), min(y, b, req.now)
                if y <= x:
                    continue
                if items and items[-1]["kind"] == kind and items[-1]["end_ms"] == x and items[-1]["ref"] == ref:
                    items[-1]["end_ms"] = y
                else:
                    items.append({"kind": kind, "start_ms": x, "end_ms": y, "ref": ref, **S.of_seg(s)})
    return {"car": req.car, "time_zone": req.tz(), "from_ms": a, "to_ms": min(b, req.now), "items": items,
            "next": None}


def register(router):
    router.add("/api/v1/cars/{car}/timeline", GET=get_timeline)
    router.add("/api/v1/cars/{car}/states", GET=get_states)
    router.add("/api/v1/cars/{car}/activity", GET=get_activity)
