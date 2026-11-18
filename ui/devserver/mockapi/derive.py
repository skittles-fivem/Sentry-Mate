"""Figures several sections share, worked out from a car's pieces: the odometer at any moment,
stops (visits), parked spells for the drain table, and the energy used while standing still.

Everything here is computed from the same pieces and rows as the lists, so totals agree across
pages (a charge counted in Statistics is the same charge, with the same energy and cost, as in
Charges). Results are cached by what they depend on: the store revision, the place revision and
how many pieces have started by "now".
"""

from __future__ import annotations

import bisect
import math

from . import placing
from .rows import in_progress
from .util import DAY, local_date, local_ms

# ------------------------------------------------------------------ caching


def memo(app, name, key, fn):
    """fn() cached under (name, key); a small cache per name (the newest few keys)."""
    box = app.cache.setdefault("derive:" + name, {})
    if key in box:
        return box[key]
    val = fn()
    if len(box) > 16:
        box.clear()
    box[key] = val
    return val


def started(h, now):
    """How many pieces have started by now (a cache key part that changes only when a piece starts)."""
    return bisect.bisect_right(h.starts, now)


# ------------------------------------------------------------------ odometer


def _odo_end(s):
    if s.kind == "drive":
        return s.odo0 + s.kin(s.data_end)[0] / 1000
    return getattr(s, "odo", None)


def odo_at(h, t):
    """The odometer (km) at t, or None before the history starts. Constant while parked; it jumps at
    the end of a gap the car drove through."""
    if not h.segs or t < h.segs[0].start:
        return None
    i = min(len(h.segs) - 1, bisect.bisect_right(h.starts, t) - 1)
    while i >= 0:
        s = h.segs[i]
        if s.kind == "drive":
            return s.odo0 + s.kin(min(t, s.data_end))[0] / 1000
        o = getattr(s, "odo", None)
        if o is not None and s.kind != "gap":
            return o
        i -= 1
    return None


# ------------------------------------------------------------------ energy used while standing still


def standing_kwh(h, a, b, now):
    """Energy drawn from the pack while not driving in [a, b): parked spells not on a charger, and
    software updates."""
    b = min(b, now)
    tot = 0.0
    for s in h.segs_between(a, b, now):
        x, y = max(s.start, a), min(s.end, b)
        if y <= x:
            continue
        if s.kind == "parked":
            tot += s.kwh_at(y) - s.kwh_at(x)
        elif s.kind == "update":
            tot += 0.6 * (y - x) / 3_600_000
    return tot


# ------------------------------------------------------------------ visits (stops)


def _moved(g):
    return g.after is None or g.before != g.after or (g.odo_jump or 0) > 0.05


def visits(app, h, now):
    """Every stop of a car, oldest first: from arriving (the end of a drive, or the first piece after
    a gap the car moved in) to leaving (the next drive, or a gap it moved in). Charges and software
    updates during the stop belong to it; a gap while it stood still does not end it."""
    key = (app.store.rev, app.store.places_rev, started(h, now))
    return memo(app, f"visits:{h.car.id}", key, lambda: _build_visits(app, h, now))


def _build_visits(app, h, now):
    out = []
    cur = None
    prev_drive = None
    shadow_of = {getattr(s, "tm_copy_of", None): s.id for s in h.shadow}
    tz = app.store.car_meta(h.car.id)["time_zone"]
    for s in h.segs:
        if s.start > now:
            break
        if s.kind == "drive":
            if cur is not None:
                cur["left_ms"] = s.start
                cur["drive_out"] = s.id if h.shown(s) else shadow_of.get(s.id, s.id)
                out.append(cur)
                cur = None
            prev_drive = s if s.cut_ms is None else None
            continue
        if s.kind == "gap":
            if _moved(s):
                if cur is not None:
                    cur["left_ms"] = s.start
                    out.append(cur)
                    cur = None
                prev_drive = None
            elif cur is not None:
                cur["unknown_ms"] += min(s.end, now) - s.start
            continue
        if cur is None:
            pid = placing.eff_pid(app, s.lat, s.lon, s.pid)
            addr = placing.full_address(app, s.lat, s.lon, s.pid)
            din = None
            if prev_drive is not None:
                din = prev_drive.id if h.shown(prev_drive) else shadow_of.get(prev_drive.id, prev_drive.id)
            cur = {"id": f"{h.car.id}-{s.start}", "car": h.car.id, "time_zone": tz, "arrived_ms": s.start,
                   "left_ms": None, "lat": round(s.lat, 6), "lon": round(s.lon, 6), "accuracy_m": 5.0,
                   "pid": pid, "address": addr, "drive_in": din, "drive_out": None, "charges": [],
                   "source": s.source, "unknown_ms": 0}
        if s.kind == "charge":
            cur["charges"].append(s.id if h.shown(s) else shadow_of.get(s.id, s.id))
        prev_drive = None
    if cur is not None:
        out.append(cur)
    for v in out:
        a = v["address"]
        v["city_key"] = city_key(a)
    return out


def city_key(a):
    return "/".join([a.get("country_code") or "-", a.get("region") or "-", a.get("locality") or "-"])


def visit_days(v, now):
    """The local dates a visit covers (in the car's time zone)."""
    tz = v["time_zone"]
    a = v["arrived_ms"]
    b = v["left_ms"] if v["left_ms"] is not None else now
    d0 = local_date(a, tz)
    d1 = local_date(max(a, b - 1), tz)
    out = []
    d = d0
    while d <= d1 and len(out) < 400:
        out.append(d)
        d = d.fromordinal(d.toordinal() + 1)
    return out


def day_bounds(d, tz):
    return local_ms(d, 0, tz), local_ms(d.fromordinal(d.toordinal() + 1), 0, tz)


# ------------------------------------------------------------------ parked spells (the drain table)


def parked_spells(app, h, now):
    """Spells parked off a charger, oldest first: consecutive parked pieces at one spot, with any
    software update or still-standing gap between them, until the car drives, charges or moves."""
    key = (app.store.rev, started(h, now))
    return memo(app, f"spells:{h.car.id}", key, lambda: _build_spells(h, now))


def _build_spells(h, now):
    out = []
    cur = None

    def close():
        nonlocal cur
        if cur is not None and cur["pieces"]:
            out.append(cur)
        cur = None

    for s in h.segs:
        if s.start > now:
            break
        if s.kind == "parked" and not s.plugged:
            if cur is None:
                cur = {"start_ms": s.start, "end_ms": s.end, "pieces": [], "unknown": []}
            cur["pieces"].append(s)
            cur["end_ms"] = s.end
        elif s.kind == "update" and cur is not None:
            cur["pieces"].append(s)
            cur["end_ms"] = s.end
        elif s.kind == "gap" and cur is not None and not _moved(s):
            cur["unknown"].append((s.start, s.end))
            cur["end_ms"] = s.end
        else:
            close()
    close()
    return out


def spell_row(app, h, sp, a, b, now):
    """One drain row, clipped to [a, b) and to now."""
    car = h.car
    x = max(sp["start_ms"], a)
    y = min(sp["end_ms"], b, now)
    split = {"asleep": 0, "awake": 0, "awake_vehicle_bus_only": 0, "unknown": 0}
    kwh = 0.0
    awake_kwh = 0.0
    reasons = []
    batt_min = None
    outs = []
    first = last = None
    for s in sp["pieces"]:
        p, q = max(s.start, x), min(s.end, y)
        if q <= p:
            continue
        if s.kind == "update":
            split["awake"] += q - p
            e = 0.6 * (q - p) / 3_600_000
            kwh += e
            awake_kwh += e
            continue
        c = s.clip(p, q)
        sp_ms = c.split_ms()
        for k in split:
            split[k] += sp_ms[k]
        e = s.kwh_at(q) - s.kwh_at(p)
        kwh += e
        for s0, e0, st, rs, kw in s.subs:
            u, v = max(s0, p), min(e0, q)
            if v > u and st != "asleep":
                awake_kwh += kw * (v - u) / 3_600_000
        for r_ in c.reasons():
            if r_ not in reasons:
                reasons.append(r_)
        for t in (p, (p + q) // 2, q - 1):
            bt = s.batt_at(t)
            batt_min = bt if batt_min is None else min(batt_min, bt)
        from .dataset.cars import outside_c
        outs.append((outside_c(car, (p + q) // 2, s.lat, s.lon), q - p))
        if first is None:
            first = (s, p)
        last = (s, q)
    for u, v in sp["unknown"]:
        u, v = max(u, x), min(v, y)
        if v > u:
            split["unknown"] += v - u
    if first is None:
        return None
    s0, p0 = first
    s1, q1 = last
    soc0 = s0.soc_at(p0)
    soc1 = s1.soc_at(q1) if s1.kind == "parked" else s1.soc
    full0 = car.full_range_km(p0)
    full1 = car.full_range_km(q1)
    dur = y - x
    awake_ms = split["awake"] + split["awake_vehicle_bus_only"]
    batt_end = s1.batt_at(q1 - 1) if s1.kind == "parked" else None
    cold = batt_end is not None and batt_end < 2.0 and car.chemistry != "lfp" or (
        batt_end is not None and batt_end < 5.0 and car.chemistry == "lfp")
    tm = s0.source == "teslamate"
    pid = placing.eff_pid(app, s0.lat, s0.lon, s0.pid)
    pl = placing.place_obj(app, pid)
    addr = pl["address"] if pl else placing.full_address(app, s0.lat, s0.lon, None)
    ow = sum(w for _, w in outs) or 1
    return {
        "id": f"parked:{sp['start_ms']}", "car": car.id, "start_ms": x, "end_ms": y if y < now else None,
        "duration_ms": dur, "clipped": x > sp["start_ms"] or y < min(sp["end_ms"], now),
        "asleep_ms": split["asleep"], "awake_ms": split["awake"],
        "awake_vehicle_bus_only_ms": split["awake_vehicle_bus_only"], "unknown_ms": split["unknown"],
        "soc_start_pct": _r(soc0, 2), "soc_end_pct": _r(soc1, 2),
        "range_start_km": _r(soc0 / 100 * full0, 1), "range_end_km": _r(soc1 / 100 * full1, 1),
        "range_lost_km": _r(soc0 / 100 * full0 - soc1 / 100 * full1, 1),
        "range_lost_per_hour_km": _r((soc0 / 100 * full0 - soc1 / 100 * full1) / (dur / 3_600_000), 2)
        if dur > 60_000 else None,
        "energy_kwh": _r(kwh, 3), "energy_method": "range" if tm else "counter",
        "avg_power_w": _r(kwh * 1000 / (dur / 3_600_000), 1) if dur > 60_000 else None,
        "awake_avg_power_w": _r(awake_kwh * 1000 / (awake_ms / 3_600_000), 1) if awake_ms > 60_000 else None,
        "battery_min_c": None if tm or batt_min is None else _r(batt_min, 1),
        "outside_avg_c": _r(sum(o * w for o, w in outs) / ow, 1),
        "cold": bool(cold), "place": placing.ref(pl), "address": addr["short"],
        "awake_reasons": reasons, "source": s0.source,
    }


def _r(v, d):
    if v is None or (isinstance(v, float) and not math.isfinite(v)):
        return None
    return round(float(v), d)


# ------------------------------------------------------------------ small helpers


def shown_drives(h, a, b, now, source="all"):
    return [d for d in h.drives if a <= d.start < b and d.start <= now and h.shown(d, source)]


def shown_charges(h, a, b, now, source="all"):
    return [c for c in h.charges if a <= c.start < b and c.start <= now and h.shown(c, source)]


def is_live(s, now):
    return in_progress(s, now)


def days_between(a, b):
    return max(0.0, (b - a) / DAY)
