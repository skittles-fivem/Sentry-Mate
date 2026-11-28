"""Battery and efficiency (docs/ui-api.md, "Battery and efficiency"): health and capacity, charge
level history, projected range, parked drain, consumption by temperature and temperatures.

Capacity readings come from the pack's own controller (one per awake day, method "bms") and from
each charge (method "charge"): the energy the charge stored over the battery % it added, where the
energy stored is the rated range it added times the car's own stored energy per km of rated range
(as the server works it out, never the energy added, which counts charging losses too), so capacity
follows the charges the Charges page lists. Lifetime counters are the pack's own: the years before logging
plus everything logged, with AC, DC and regen adding up exactly.
"""

from __future__ import annotations

import bisect
import datetime as dt
import math

from .. import derive
from .. import series as S
from ..conventions import page
from ..errors import bad_input
from ..live import now_object
from ..placing import place_obj
from ..util import DAY, HOUR, h01, local_date, local_midnights, local_ms, median, percentile, r
from .charges import cached_charge_row
from .drives import cached_drive_row
from .. import sources as SRC


def _v(w):
    return None if w is None else w["value"]


def bms_full(car, t):
    """The full-pack energy the battery controller reports on a day (kWh), with a little reading noise."""
    return car.full_pack_kwh(t) + (h01("bms", car.id, t // DAY) - 0.5) * 0.3


def _new_capacity(req, h):
    """The usable capacity when new (kWh) and where it comes from: the same basis as the usable capacity now
    (full pack less the buffer), so the difference and the health compare like with like. A typed value is
    usable already; values worked out from readings are full-pack values, less the buffer."""
    car = h.car
    s = req.store.car_settings.get(car.id) or {}
    if s.get("pack_replaced_date") and s.get("pack_new_kwh"):
        return float(s["pack_new_kwh"]), "owner"
    if s.get("capacity_new_kwh"):
        return float(s["capacity_new_kwh"]), "owner"
    if car.odo_start_km < 5000:
        first = h.first_ms
        days = [bms_full(car, first + i * DAY) for i in range(30)]
        return median(days) - car.buffer_kwh, "early_bms"
    tm = [c for c in h.charges if c.source == "teslamate"][:10]
    if tm:
        got = [u for u in (_charge_usable(car, c) for c in tm) if u is not None]
        if got:
            return median([u[0] for u in got]), "imported_charges"
    return car.full_new_kwh - car.buffer_kwh, "vehicle_info"


def _new_range(req, h):
    car = h.car
    s = req.store.car_settings.get(car.id) or {}
    if s.get("full_range_new_km"):
        return float(s["full_range_new_km"]), "owner"
    if car.odo_start_km < 5000:
        return car.full_range_km(h.first_ms + 15 * DAY), "first_30_days"
    return car.full_range_new_km, "vehicle_info"


def _charge_usable(car, c):
    """The usable capacity one charge measured, (usable, energy stored, energy added): the energy it stored (the
    rated range it added times the car's stored energy per km of rated range) over the battery % it added. The same
    figures the Charges list shows (imported charges included), so a page can redo the sum; the full pack is that
    plus the buffer, once."""
    f = c.figures()
    soc0, soc1 = r(c.soc0, 1), r(f["soc1"], 1)  # as the charge row shows them
    r0, r1 = r(f["range0"], 1), r(f["range1"], 1)
    if soc1 - soc0 < 10 or r1 <= r0:
        return None
    stored = (r1 - r0) * car.rated_wh_per_km / 1000
    return stored / (soc1 - soc0) * 100, stored, r(f["added"], 3)


def _chemistry(req, h):
    s = (req.store.car_settings.get(h.car.id) or {}).get("battery_chemistry") or {}
    if s.get("setting") in ("lfp", "nca_nmc"):
        return {"value": s["setting"], "source": "owner", "confidence": None, "evidence": None}
    # the build's detection, as the build card and Car settings show it
    value = s.get("detected") or h.car.chemistry
    return {"value": value, "source": "detected" if value else None,
            "confidence": ("low" if h.car.vin_prefix[:1] in "57" else "medium") if value else None,
            "evidence": s.get("evidence")}


def lifetime(req, h):
    """The pack's lifetime counters (kWh): before logging, plus everything logged."""
    key = (req.store.rev, derive.started(h, req.now), req.now // 60_000)
    return derive.memo(req.app, f"lifetime:{h.car.id}", key, lambda: _lifetime(req, h))


def _lifetime(req, h):
    car = h.car
    pre_drive = car.odo_start_km * car.base_wh_per_km / 1000 * 1.06
    pre_regen = pre_drive * 0.11
    pre_stand = car.odo_start_km * car.base_wh_per_km / 1000 * 0.05
    pre_in = pre_drive - pre_regen + pre_stand
    ac = pre_in * 0.86
    dc = pre_in * 0.14
    regen = pre_regen
    drive = pre_drive
    for d in derive.shown_drives(h, 0, req.now + 1, req.now):
        x = cached_drive_row(req, h, d)
        e = x["energy_kwh"] or 0.0
        rg = x["regen_kwh"] if x["regen_kwh"] is not None else e * d.regen_share
        regen += rg
        drive += e + rg
    for c in derive.shown_charges(h, 0, req.now + 1, req.now):
        x = cached_charge_row(req, h, c)
        if x["kind"] == "dc":
            dc += x["energy_added_kwh"] or 0.0
        else:
            ac += x["energy_added_kwh"] or 0.0
    stand = pre_stand + derive.standing_kwh(h, 0, req.now + 1, req.now)
    discharged = drive + stand
    new_usable = car.full_new_kwh - car.buffer_kwh
    return {"charged_kwh": r(ac + dc, 1), "discharged_kwh": r(discharged, 1), "ac_kwh": r(ac, 1), "dc_kwh": r(dc, 1),
            "unknown_kwh": 0.0,  # the pack's own counters count every charge as AC or DC
            "regen_kwh": r(regen, 1), "drive_kwh": r(drive, 1), "standing_kwh": r(stand, 1),
            "equivalent_cycles": r(discharged / new_usable, 1), "basis": "pack counters"}


# ------------------------------------------------------------------ /battery


def _pack(req):
    """The pack replacement the owner typed (the mock never detects one): { replaced_ms, source }."""
    d = (req.store.car_settings.get(req.car) or {}).get("pack_replaced_date")
    if not d:
        return {"replaced_ms": None, "source": None}
    from ..util import parse_date
    return {"replaced_ms": local_ms(parse_date(d), 0, req.tz()), "source": "owner"}


def get_battery(req):
    h = req.history()
    tz = req.tz()
    if h is None:
        n = now_object(req.app, req.car, req.now)
        return {"car": req.car, "time_zone": tz, "as_of_ms": n.get("at_ms"),
                "capacity": {k: None for k in ("full_pack_kwh", "buffer_kwh", "usable_kwh", "basis", "usable_basis",
                                               "spread_kwh", "readings",
                                               "new_kwh", "new_source", "difference_kwh", "degradation_pct",
                                               "health_pct", "warranty_pct")},
                "range": {k: None for k in ("full_now_km", "full_now_from_energy_km", "full_new_km", "new_source",
                                            "lost_km")},
                "energy_now_kwh": None, "soc_pct": _v(n.get("soc_pct")), "usable_soc_pct": None,
                "band": {"low_pct": 20, "high_pct": 80, "charge_limit_pct": None},
                "chemistry": {"value": None, "source": None, "confidence": None, "evidence": None},
                "brick_spread_v": None,
                "lifetime": None, "notes": ["no_history"], "pack": _pack(req)}
    car = h.car
    n = now_object(req.app, req.car, req.now)
    at = n.get("at_ms") or req.now
    full = bms_full(car, at)
    recent = [bms_full(car, at - i * DAY) for i in range(30)]
    new_kwh, new_src = _new_capacity(req, h)
    usable_now = full - car.buffer_kwh
    diff = new_kwh - usable_now
    # a pack is never healthier than new: no loss where the reading is above the value when new, and a note
    deg = max(0.0, diff / new_kwh * 100)
    s = req.store.car_settings.get(car.id) or {}
    rng_new, rng_src = _new_range(req, h)
    full_km = car.full_range_km(at)
    from_energy = (full - car.buffer_kwh) * 1000 / car.rated_wh_per_km
    chem = _chemistry(req, h)
    soc = _v(n.get("soc_pct"))
    usable_soc = _v(n.get("usable_soc_pct"))
    energy = _v(n.get("energy_remaining_kwh"))
    if usable_soc is None and energy is not None and full - car.buffer_kwh > 0:
        # the pack's energy above its buffer over its usable capacity, as the server works it out
        usable_soc = r(min(100.0, max(0.0, (energy - car.buffer_kwh) / (full - car.buffer_kwh) * 100)), 1)
    notes = []
    notes.append({"owner": "new_value_typed", "early_bms": "new_value_from_early_readings",
                  "imported_charges": "new_value_from_imported_charges",
                  "vehicle_info": "new_value_from_vehicle_info"}[new_src])
    if diff <= -0.05:
        notes.append("reading_above_new")
    if soc is not None and usable_soc is not None and usable_soc < soc - 0.5:
        notes.append("cold_battery_locks_part")
    if chem["value"] == "lfp":
        notes.append("lfp_charge_to_100_weekly")
    if any(c.source == "teslamate" for c in h.charges):
        notes.append("imported_history_estimated")
    pack = _pack(req)
    if pack["replaced_ms"] is not None:
        notes.append("pack_replaced")
    legacy = car.profile == "tesla-sx-legacy"
    return {
        "car": req.car, "time_zone": req.tz(), "as_of_ms": at,
        "capacity": {
            "full_pack_kwh": r(full, 2), "buffer_kwh": car.buffer_kwh, "usable_kwh": r(full - car.buffer_kwh, 2),
            "basis": "pack", "usable_basis": "buffer",
            "spread_kwh": r(max(recent) - min(recent), 2), "readings": 30 * 24 * 12,
            "new_kwh": r(new_kwh, 2), "new_source": new_src, "difference_kwh": r(diff, 2),
            "degradation_pct": r(deg, 1), "health_pct": r(100 - deg, 1),
            "warranty_pct": s.get("warranty_threshold_pct", 70),
        },
        "range": {"full_now_km": r(full_km, 1), "full_now_from_energy_km": r(from_energy, 1),
                  "full_new_km": r(rng_new, 1), "new_source": rng_src, "lost_km": r(rng_new - full_km, 1)},
        "energy_now_kwh": _v(n.get("energy_remaining_kwh")), "soc_pct": soc, "usable_soc_pct": usable_soc,
        "band": {"low_pct": 20, "high_pct": 100 if chem["value"] == "lfp" else 80,
                 "charge_limit_pct": car.charge_limit},
        "chemistry": chem,
        "brick_spread_v": None if legacy else r(0.008 + 0.01 * h01("brick", car.id, at // DAY), 3),
        "lifetime": lifetime(req, h),
        "notes": notes,
        "pack": pack,
    }


# ------------------------------------------------------------------ /battery/capacity


def get_capacity(req):
    a, b = req.period()
    group = req.enum("group", ("day", "charge", "month"), "charge")
    x = req.enum("x", ("time", "odometer"), "time")
    h = req.history()
    tz = req.tz()
    out = {"car": req.car, "time_zone": tz, "group": group, "x": x, "pack": _pack(req)}
    if h is None:
        out.update({"items": [], "next": None, "trend": {"slope_kwh_per_10000_km": None, "projection": None}})
        return out
    car = h.car
    end = min(b, req.now)
    rows = []
    if group == "charge":
        for c in derive.shown_charges(h, a, end, req.now):
            if derive.is_live(c, req.now):
                continue
            got = _charge_usable(car, c)
            if got is None:
                continue
            usable, stored, added = got
            rows.append({"t_ms": c.data_end, "odometer_km": r(getattr(c, "odo", None), 1),
                         "full_pack_kwh": r(usable + car.buffer_kwh, 2), "usable_kwh": r(usable, 2),
                         "usable_basis": "rated_range", "energy_added_kwh": added, "stored_kwh": r(stored, 3),
                         **SRC.of_seg(c), "method": "charge", "charge": c.id})
    else:
        lo = max(a, h.first_ms)
        d0 = local_date(lo, tz)
        d1 = local_date(end - 1, tz) if end > lo else d0
        d = d0
        per_month = {}
        while d <= d1:
            t = local_ms(d, 12, tz)
            if lo <= t < end:
                s = h.seg_at(t)
                src = s.source if s is not None else "can"
                if s is not None and s.kind != "gap":
                    fp = bms_full(car, t)
                    o = derive.odo_at(h, t)
                    row = {"t_ms": t, "odometer_km": r(o, 1), "full_pack_kwh": r(fp, 2),
                           "usable_kwh": r(fp - car.buffer_kwh, 2), "usable_basis": "buffer", **SRC.fields(src, getattr(s, "inputs", None)),
                           "method": "bms", "charge": None}
                    if src == "teslamate":  # imported history has no controller readings, only charges
                        row = None
                    if row:
                        if group == "day":
                            rows.append(row)
                        else:
                            per_month.setdefault((d.year, d.month), []).append(row)
            d = d.fromordinal(d.toordinal() + 1)
        for (yy, mm), rs in sorted(per_month.items()):
            t = local_ms(dt.date(yy, mm, 1), 0, tz)
            fp = median([q["full_pack_kwh"] for q in rs])
            rows.append({"t_ms": t, "odometer_km": rs[-1]["odometer_km"], "full_pack_kwh": r(fp, 2),
                         "usable_kwh": r(fp - car.buffer_kwh, 2), "usable_basis": "buffer", "method": "bms", "charge": None,
                         **SRC.fields("can", sorted({x["kind"] for q in rs for x in q["sources"]})),
                         "readings": len(rs)})
    trend = _trend(req, h, rows)
    if x == "odometer":
        rows = [q for q in rows if q["odometer_km"] is not None]
        key = lambda q: (q["odometer_km"], q["t_ms"])  # noqa: E731
    else:
        key = lambda q: (q["t_ms"],)  # noqa: E731
    items, nxt = page(req, rows, key=key)
    out.update({"items": items, "next": nxt, "trend": trend})
    return out


def _trend(req, h, rows):
    pts = [(q["odometer_km"], q["full_pack_kwh"], q["t_ms"]) for q in rows
           if q["odometer_km"] is not None and q["full_pack_kwh"] is not None]
    if len(pts) < 3 or max(p[0] for p in pts) - min(p[0] for p in pts) < 500:
        return {"slope_kwh_per_10000_km": None, "projection": None}
    n = len(pts)
    mx = sum(p[0] for p in pts) / n
    my = sum(p[1] for p in pts) / n
    sxx = sum((p[0] - mx) ** 2 for p in pts)
    sxy = sum((p[0] - mx) * (p[1] - my) for p in pts)
    slope = sxy / sxx if sxx else 0.0
    ts = [p[2] for p in pts]
    os_ = [p[0] for p in pts]
    km_per_ms = (max(os_) - min(os_)) / max(1, max(ts) - min(ts))
    new_kwh, _src = _new_capacity(req, h)
    warranty = (req.store.car_settings.get(h.car.id) or {}).get("warranty_threshold_pct", 70)
    target = (new_kwh + h.car.buffer_kwh) * warranty / 100  # on the full-pack basis of the points
    proj = None
    if slope < -1e-9:
        at_km = mx + (target - my) / slope
        last_km = max(os_)
        t_at = max(ts) + (at_km - last_km) / km_per_ms if km_per_ms > 0 else None
        proj = {"warranty_pct": warranty, "full_pack_kwh_at_warranty": r(target, 2), "odometer_km": r(at_km, 0),
                "t_ms": int(t_at) if t_at is not None and t_at < 4_102_444_800_000 else None, "method": "linear"}
    return {"slope_kwh_per_10000_km": r(slope * 10_000, 3), "projection": proj}


# ------------------------------------------------------------------ /battery-level


def get_battery_level(req):
    a, b = req.period(30 * DAY)
    bucket_name = req.enum("bucket", ("hour", "day"), None)
    moving_on = req.bool_param("moving", True)
    window = req.enum("window", ("7d", "30d"), "7d")
    h = req.history()
    tz = req.tz()
    end = min(b, req.now)
    span = max(1, end - a)
    if bucket_name is None:
        bucket_name = "hour" if span <= 31 * DAY else "day"
    bucket = HOUR if bucket_name == "hour" else DAY
    band = {"low_pct": 20, "high_pct": 80}
    out = {"car": req.car, "time_zone": tz, "from_ms": a, "to_ms": end, "bucket": bucket_name, "bucket_ms": bucket,
           "window": window}
    if h is None:
        out.update({"buckets": [], "moving": {"mean": [], "median": [], "p7_5": [], "p92_5": []}, "spans": [],
                    "spans_truncated": False,
                    "band": band, "charge_limit": [], "share_above_80_pct": None, "share_below_20_pct": None,
                    "stats": {"mean": None, "median": None, "min": None, "max": None}})
        return out
    car = h.car
    if car.chemistry == "lfp":
        band["high_pct"] = 100
    if span / bucket > 20000:
        raise bad_input({"bucket": "Too many buckets for this period; use day."})
    wdays = 7 if window == "7d" else 30
    # day buckets are the car's own days (23 or 25 hours when the clocks change); hours start at local midnight
    if bucket == DAY:
        cols = S.build(h, ["soc_pct", "usable_soc_pct"], a, end, 0, ["mean"], req.now, bucket=DAY,
                       edges=local_midnights(a, end, tz))
    else:
        cols = S.build(h, ["soc_pct", "usable_soc_pct"], a, end, 0, ["mean"], req.now, bucket=bucket,
                       align=local_ms(local_date(a, tz), 0, tz))
    buckets = [{"t_ms": t, "soc_pct": r(s, 2), "usable_soc_pct": r(u, 2)}
               for t, s, u in zip(cols["t_ms"], cols["values"]["soc_pct"], cols["values"]["usable_soc_pct"])]
    # moving lines: a fixed window of daily means, ending at each bucket's day
    moving = {"mean": [], "median": [], "p7_5": [], "p92_5": []}
    if moving_on and buckets:
        dedges = local_midnights(local_ms(local_date(a, tz) - dt.timedelta(days=wdays), 0, tz), end, tz)
        if bucket == DAY and dedges[-(len(buckets) + 1):] == cols["t_ms"] + [dedges[-1]]:
            # the day buckets are the last days of the window's days: build only the days before them
            head = S.build(h, ["soc_pct"], dedges[0], cols["t_ms"][0], 0, ["mean"], req.now, bucket=DAY,
                           edges=dedges[:len(dedges) - len(buckets)])
            dts = head["t_ms"] + cols["t_ms"]
            dvs = head["values"]["soc_pct"] + cols["values"]["soc_pct"]
        else:
            daily = S.build(h, ["soc_pct"], dedges[0], end, 0, ["mean"], req.now, bucket=DAY, edges=dedges)
            dts, dvs = daily["t_ms"], daily["values"]["soc_pct"]
        per_day = []
        for i in range(len(dts)):
            win = [v for v in dvs[max(0, i - wdays + 1):i + 1] if v is not None]
            per_day.append((sum(win) / len(win), median(win), percentile(win, 0.075), percentile(win, 0.925))
                           if win else (None, None, None, None))
        for bk in buckets:
            j = bisect.bisect_right(dts, bk["t_ms"]) - 1
            m = per_day[j] if 0 <= j < len(per_day) else (None, None, None, None)
            for name, v in zip(("mean", "median", "p7_5", "p92_5"), m):
                moving[name].append(r(v, 2))
    # asleep and unknown spans
    spans = []
    for s in h.segs_between(a, end, req.now):
        for kind, x, y in s.activity():
            x, y = max(x, a), min(y, end)
            if y <= x or kind not in ("asleep", "unknown"):
                continue
            st = "asleep" if kind == "asleep" else "unknown"
            if spans and spans[-1]["state"] == st and spans[-1]["end_ms"] == x:
                spans[-1]["end_ms"] = y
            else:
                spans.append({"state": st, "start_ms": x, "end_ms": y})
    vals = [q["soc_pct"] for q in buckets if q["soc_pct"] is not None]
    n = len(vals) or 1
    limits = [{"t_ms": a, "pct": car.charge_limit}]
    for c in derive.shown_charges(h, a, end, req.now):
        tgt = round(c.target)
        if c.ckind == "ac" and tgt != limits[-1]["pct"] and tgt > car.charge_limit:
            limits.append({"t_ms": c.start, "pct": tgt})
            limits.append({"t_ms": c.data_end, "pct": car.charge_limit})
    out.update({
        "buckets": buckets, "moving": moving, "spans": spans, "spans_truncated": False, "band": band, "charge_limit": limits,
        "share_above_80_pct": r(100 * sum(1 for v in vals if v > 80) / n, 1) if vals else None,
        "share_below_20_pct": r(100 * sum(1 for v in vals if v < 20) / n, 1) if vals else None,
        "stats": {"mean": r(sum(vals) / n, 2) if vals else None, "median": r(median(vals), 2),
                  "min": r(min(vals), 2) if vals else None, "max": r(max(vals), 2) if vals else None,
                  "p7_5": r(percentile(vals, 0.075), 2), "p92_5": r(percentile(vals, 0.925), 2)},
    })
    return out


# ------------------------------------------------------------------ /projected-range

PR_FIELDS = ("full_range_km", "full_range_from_energy_km", "full_range_usable_km", "soc_pct", "soc_bms_pct",
             "odometer_km", "outside_c", "battery_c")
BUCKETS = {"hour": HOUR, "day": DAY, "week": 7 * DAY}


def _pr_values(s, t):
    v = s.values(t)
    if v is None:
        return None
    car = s.car
    soc, usable_soc = v.get("soc_pct"), v.get("usable_soc_pct")
    out = {k: v.get(k) for k in ("soc_pct", "soc_bms_pct", "odometer_km", "outside_c", "battery_c")}
    rng = v.get("range_km")
    e = v.get("energy_remaining_kwh")
    out["full_range_km"] = round(rng / soc * 100, 1) if rng is not None and soc else None
    out["full_range_from_energy_km"] = round(e / soc * 100 * 1000 / car.rated_wh_per_km, 1) \
        if e is not None and soc else None
    # the full-charge range the usable % points to (higher than the plain one while part of a cold pack is locked)
    out["full_range_usable_km"] = round(rng / usable_soc * 100, 1) if rng is not None and usable_soc else None
    return out


def get_projected_range(req):
    a, b = req.period()
    bname = req.q("bucket")
    bucket = None
    if bname not in (None, "", "auto"):
        if bname in BUCKETS:
            bucket = BUCKETS[bname]
        else:
            bucket = req.int_param("bucket", None, 60_000, 30 * DAY)
    max_points = req.int_param("max_points", 1000, 2, 20000)
    h = req.history()
    out = {"car": req.car, "time_zone": req.tz(), "fields": list(PR_FIELDS)}
    if h is None:
        out.update({"t_ms": [], "values": {f: [] for f in PR_FIELDS}, "bucket_ms": bucket or 1000,
                    "coverage_pct": [], "state": []})
        return out
    lo = max(a, h.first_ms)
    end = min(b, req.now)
    if bucket and (end - lo) / bucket > 20000:
        raise bad_input({"bucket": "Too many buckets for this period."})
    # day and week buckets are the car's own days and ISO weeks; hours and a number of ms are aligned to UTC
    edges = local_midnights(lo, end, req.tz(), 7 if bname == "week" else 1) if bname in ("day", "week") \
        and end > lo else None
    cols = S.build(h, list(PR_FIELDS), lo, end, max_points, ["mean"], req.now, value_fn=_pr_values, bucket=bucket,
                   edges=edges)
    out.update(cols)
    return out


# ------------------------------------------------------------------ /parked


def get_parked(req):
    a, b = req.period()
    min_h = req.float_param("min_hours", 0.0, 0, 10000)
    h = req.history()
    rows = []
    if h is not None:
        for sp in derive.parked_spells(req.app, h, req.now):
            if sp["end_ms"] <= a or sp["start_ms"] >= b:
                continue
            x = derive.spell_row(req.app, h, sp, a, b, req.now)
            if x is None or x["duration_ms"] < min_h * HOUR:
                continue
            rows.append(x)
    energy = sum(x["energy_kwh"] or 0 for x in rows)
    span_ms = sum(x["duration_ms"] for x in rows)
    cost = None
    if h is not None:
        home = place_obj(req.app, h.car.home)
        price = (home or {}).get("price")
        if price and price.get("per_kwh") is not None:
            cost = {"amount": round(energy * price["per_kwh"], 2), "currency": price["currency"],
                    "basis": "home price per kWh"}
    items, nxt = page(req, rows, key=lambda x: (x["start_ms"],), reverse=True)
    return {"car": req.car, "time_zone": req.tz(), "items": items, "next": nxt,
            "summary": {"spells": len(rows), "energy_kwh": r(energy, 3), "hours": r(span_ms / HOUR, 1),
                        "kwh_per_day": r(energy / (span_ms / DAY), 3) if span_ms > HOUR else None, "cost": cost}}


# ------------------------------------------------------------------ /efficiency


def get_efficiency(req):
    a, b = req.period()
    min_d = req.float_param("min_distance_km", 1.0, 0, 100000)
    group = req.float_param("temp_group_c", 5.0, 1, 20)
    h = req.history()
    out = {"car": req.car, "time_zone": req.tz(), "from_ms": a, "to_ms": min(b, req.now), "temp_group_c": group,
           "min_distance_km": min_d}
    empty = {"net_wh_per_km": None, "gross_wh_per_km": None, "energy_kwh": 0.0, "parked_kwh": 0.0, "distance_km": 0.0,
             "drives": 0,
             "without_energy": 0, "estimated_drives": 0, "all_drives": {"drives": 0, "distance_km": 0.0, "energy_kwh": 0.0},
             "by_temperature": [], "no_temperature": {"drives": 0, "distance_km": 0.0},
             "factor": {"rated_wh_per_km": None, "ideal_wh_per_km": None, "source": None, "readings": 0,
                        "as_of_ms": None}}
    if h is None:
        out.update(empty)
        return out
    car = h.car
    end = min(b, req.now)
    every = [cached_drive_row(req, h, d) for d in derive.shown_drives(h, a, end, req.now)]
    rows = [x for x in every if (x["distance_km"] or 0) >= min_d]
    dist = sum(x["distance_km"] or 0 for x in rows)
    energy = sum(x["energy_kwh"] or 0 for x in rows)
    # gross is what owning the car costs: every drive and all parked energy, whatever min_distance_km says
    all_dist = sum(x["distance_km"] or 0 for x in every)
    all_energy = sum(x["energy_kwh"] or 0 for x in every)
    lo = max(a, h.first_ms)
    parked = derive.standing_kwh(h, lo, end, req.now)
    groups = {}
    no_t = {"drives": 0, "distance_km": 0.0}
    for x in rows:
        t = x["outside_avg_c"]
        if t is None:
            no_t["drives"] += 1
            no_t["distance_km"] += x["distance_km"] or 0
            continue
        k = math.floor(t / group) * group
        g = groups.setdefault(k, {"drives": 0, "distance_km": 0.0, "energy_kwh": 0.0, "ms": 0})
        g["drives"] += 1
        g["distance_km"] += x["distance_km"] or 0
        g["energy_kwh"] += x["energy_kwh"] or 0
        g["ms"] += x["duration_ms"]
    by_t = []
    for k in sorted(groups):
        g = groups[k]
        whkm = g["energy_kwh"] / g["distance_km"] * 1000 if g["distance_km"] > 0.1 else None
        by_t.append({"temp_c": r(k, 1), "temp_to_c": r(k + group, 1), "drives": g["drives"],
                     "distance_km": r(g["distance_km"], 3), "energy_kwh": r(g["energy_kwh"], 3), "wh_per_km": r(whkm, 1),
                     "range_efficiency_pct": r(car.rated_wh_per_km / whkm * 100, 1) if whkm else None,
                     "avg_kph": r(g["distance_km"] / (g["ms"] / HOUR), 1) if g["ms"] > 0 else None})
    no_t["distance_km"] = r(no_t["distance_km"], 3)
    readings = sum(1 for x in rows if x["source"] != "teslamate")
    over = (req.store.car_settings.get(car.id) or {}).get("efficiency_wh_per_km_override")
    out.update({
        "net_wh_per_km": r(energy / dist * 1000, 1) if dist > 0.1 else None,
        "gross_wh_per_km": r((all_energy + parked) / all_dist * 1000, 1) if all_dist > 0.1 else None,
        "all_drives": {"drives": len(every), "distance_km": r(all_dist, 3), "energy_kwh": r(all_energy, 3)},
        "parked_kwh": r(parked, 3), "distance_km": r(dist, 3), "energy_kwh": r(energy, 3), "drives": len(rows),
        "without_energy": sum(1 for x in rows if x["energy_kwh"] is None),
        "estimated_drives": sum(1 for x in rows if x["energy_basis"] == "estimated"),
        "by_temperature": by_t, "no_temperature": no_t,
        "factor": {"rated_wh_per_km": over or car.rated_wh_per_km,
                   "ideal_wh_per_km": r(car.rated_wh_per_km / car.ideal_factor, 1),
                   "source": "owner" if over else "energy_and_range", "readings": readings * 60,
                   "as_of_ms": rows[-1]["end_ms"] if rows and rows[-1]["end_ms"] else None},
    })
    return out


# ------------------------------------------------------------------ /temperatures


def get_temperatures(req):
    a, b = req.period()
    h = req.history()
    tz = req.tz()
    out = {"car": req.car, "time_zone": tz}
    none = {"min_c": None, "avg_c": None, "max_c": None, "coverage_pct": 0.0}
    if h is None:
        out.update({"outside": dict(none), "cabin": dict(none), "battery": dict(none),
                    "missing_ms": {"asleep": 0, "unknown": 0}, "daily": []})
        return out
    lo = max(a, h.first_ms)
    end = min(b, req.now)
    edges = local_midnights(lo, end, tz)  # the car's own days
    cols = S.build(h, ["outside_c", "cabin_c", "battery_c"], lo, end, 0, ["mean", "min", "max", "p15", "p85"],
                   req.now, bucket=DAY, edges=edges, sample_ms=60_000) if end > lo else None
    span = max(1, end - lo)
    asleep = unknown = 0
    awake_by_day = {}
    for s in h.segs_between(lo, end, req.now):
        for kind, x, y in s.activity():
            x, y = max(x, lo), min(y, end)
            if y <= x:
                continue
            if kind == "asleep":
                asleep += y - x
            elif kind == "unknown":
                unknown += y - x
            else:  # temperatures are read while the car is awake
                while x < y:
                    i = bisect.bisect_right(edges, x) - 1
                    e = min(y, edges[i + 1])
                    awake_by_day[edges[i]] = awake_by_day.get(edges[i], 0) + (e - x)
                    x = e
    known_pct = 100 * (span - asleep - unknown) / span

    def summary(f):
        if cols is None:
            return dict(none)
        v = cols["values"]
        mins = [x for x in v[f + ".min"] if x is not None]
        maxs = [x for x in v[f + ".max"] if x is not None]
        means = [x for x in v[f + ".mean"] if x is not None]
        if not means:
            return dict(none)
        return {"min_c": r(min(mins), 1), "avg_c": r(sum(means) / len(means), 1), "max_c": r(max(maxs), 1),
                "coverage_pct": r(known_pct, 1)}

    daily = []
    if cols is not None:
        v = cols["values"]
        for i, t in enumerate(cols["t_ms"]):
            day_ms = min(edges[i + 1], end) - max(t, lo)
            cov = r(100 * awake_by_day.get(t, 0) / day_ms, 1) if day_ms > 0 else 0.0
            daily.append({"date": local_date(t, tz).isoformat(), "t_ms": t, "min_c": v["outside_c.min"][i],
                          "p15_c": r(v["outside_c.p15"][i], 1), "p85_c": r(v["outside_c.p85"][i], 1),
                          "max_c": v["outside_c.max"][i], "coverage_pct": cov})
    out.update({"outside": summary("outside_c"), "cabin": summary("cabin_c"), "battery": summary("battery_c"),
                "missing_ms": {"asleep": asleep, "unknown": unknown}, "daily": daily})
    return out


def register(router):
    router.add("/api/v1/cars/{car}/battery", GET=get_battery)
    router.add("/api/v1/cars/{car}/battery/capacity", GET=get_capacity)
    router.add("/api/v1/cars/{car}/battery-level", GET=get_battery_level)
    router.add("/api/v1/cars/{car}/projected-range", GET=get_projected_range)
    router.add("/api/v1/cars/{car}/parked", GET=get_parked)
    router.add("/api/v1/cars/{car}/efficiency", GET=get_efficiency)
    router.add("/api/v1/cars/{car}/temperatures", GET=get_temperatures)
