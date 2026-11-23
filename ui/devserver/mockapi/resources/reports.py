"""Reports (docs/ui-api.md, "Reports"), part one: statistics per period, mileage, whole-history
totals, gaps and the comparison of cars.

Every figure is summed from the same Drive and Charge rows the lists give (cached_drive_row,
cached_charge_row), so a month's energy and cost here equal the Charges list for that month.
Periods are in each car's own time zone (ISO weeks, Monday first).
"""

from __future__ import annotations

import datetime as dt

from .. import derive
from ..conventions import page
from ..errors import bad_input
from ..placing import eff_pid, place_obj, ref
from ..rows import charge_estimate
from ..util import DAY, local_date, local_ms, r
from .battery import bms_full
from .charges import cached_charge_row
from .drives import cached_drive_row
from .. import sources as S

KINDS = ("day", "week", "month", "year", "all")


# ------------------------------------------------------------------ periods in a car's time zone


def period_start(ms, tz, kind):
    d = local_date(ms, tz)
    if kind == "day":
        s = d
    elif kind == "week":
        s = d - dt.timedelta(days=d.weekday())
    elif kind == "month":
        s = d.replace(day=1)
    else:
        s = d.replace(month=1, day=1)
    return s


def next_start(s, kind):
    if kind == "day":
        return s + dt.timedelta(days=1)
    if kind == "week":
        return s + dt.timedelta(days=7)
    if kind == "month":
        return (s.replace(day=28) + dt.timedelta(days=4)).replace(day=1)
    return s.replace(year=s.year + 1)


def label(s, kind):
    if kind == "day":
        return s.isoformat()
    if kind == "week":
        y, w, _ = s.isocalendar()
        return f"{y}-W{w:02d}"
    if kind == "month":
        return f"{s.year}-{s.month:02d}"
    return str(s.year)


def periods(a, b, tz, kind):
    """[(start_ms, end_ms, label)] covering [a, b) (the first and last clipped to it)."""
    if b <= a:
        return []
    if kind == "all":
        return [(a, b, "All")]
    out = []
    s = period_start(a, tz, kind)
    while True:
        ps = local_ms(s, 0, tz)
        n = next_start(s, kind)
        pe = local_ms(n, 0, tz)
        if ps >= b:
            break
        out.append((max(ps, a), min(pe, b), label(s, kind)))
        s = n
        if len(out) > 20000:
            break
    return out


class Buckets:
    """Map moments to the periods of a list (sorted, touching)."""

    def __init__(self, ps):
        self.ps = ps
        self.starts = [p[0] for p in ps]

    def index(self, t):
        import bisect
        i = bisect.bisect_right(self.starts, t) - 1
        if 0 <= i < len(self.ps) and t < self.ps[i][1]:
            return i
        return None

    def split(self, x, y):
        """[(index, x, y)] for a span cut at period edges."""
        out = []
        while x < y:
            i = self.index(x)
            if i is None:
                import bisect
                j = bisect.bisect_right(self.starts, x)
                if j >= len(self.ps):
                    break
                x = self.ps[j][0]
                continue
            e = min(y, self.ps[i][1])
            out.append((i, x, e))
            x = e
        return out


def _money(d):
    return [{"currency": k, "amount": round(v, 2)} for k, v in sorted(d.items())]


# ------------------------------------------------------------------ statistics


def stat_rows(req, h, kind, a, b):
    # the car's settings (its default price per kWh) and the places' prices change the estimates
    sv = (req.store.car_settings.get(h.car.id) or {}).get("version")
    key = (kind, a, b, req.store.rev, req.store.places_rev, sv, derive.started(h, req.now), req.now // 10_000)
    return derive.memo(req.app, f"stats:{h.car.id}", key, lambda: _stat_rows(req, h, kind, a, b))


def _stat_rows(req, h, kind, a, b):
    car = h.car
    tz = req.tz(car.id)
    lo = max(a, h.first_ms)
    end = min(b, req.now)
    ps = periods(lo, end, tz, kind)
    if not ps:
        return []
    bk = Buckets(ps)
    acc = [{"ms": 0, "drives": 0, "km": 0.0, "kwh": 0.0, "temp_w": 0.0, "temp_ms": 0, "added": 0.0, "grid": 0.0,
            "charges": 0, "cost": {}, "basis": {}, "priced": 0, "estimated": 0, "est": {}, "stand": 0.0, "unknown": 0, "dropped": 0, "open_drive": False,
            "open_charge": False, "tm": 0} for _ in ps]
    for d in derive.shown_drives(h, lo, end, req.now):
        i = bk.index(d.start)
        if i is None:
            continue
        x = cached_drive_row(req, h, d)
        g = acc[i]
        g["drives"] += 1
        g["ms"] += x["duration_ms"]
        g["km"] += x["distance_km"] or 0
        g["kwh"] += x["energy_kwh"] or 0
        if x["outside_avg_c"] is not None:
            g["temp_w"] += x["outside_avg_c"] * x["duration_ms"]
            g["temp_ms"] += x["duration_ms"]
        g["dropped"] += len(d.drops or [])
        g["open_drive"] |= x["flags"]["in_progress"] or d.cut_ms is not None
        g["tm"] += d.source == "teslamate"
    for c in derive.shown_charges(h, lo, end, req.now):
        i = bk.index(c.start)
        if i is None:
            continue
        x = cached_charge_row(req, h, c)
        g = acc[i]
        g["charges"] += 1
        g["added"] += x["energy_added_kwh"] or 0
        g["grid"] += x["energy_grid_kwh"] or 0
        if not x["cost"]:
            e = charge_estimate(req.app, car.id, c, x, req.now)
            if e is not None:
                g["estimated"] += 1
                g["est"][e["currency"]] = g["est"].get(e["currency"], 0.0) + e["amount"]
        if x["cost"]:
            g["priced"] += 1
            cur = x["cost"]["currency"]
            g["cost"][cur] = g["cost"].get(cur, 0.0) + x["cost"]["amount"]
            if x["price_per_kwh"]:
                basis = x["energy_grid_kwh"] if x["price_per_kwh"]["basis"] == "grid" else (
                    x["dc_inlet_kwh"] or x["energy_added_kwh"])
                g["basis"][cur] = g["basis"].get(cur, 0.0) + (basis or 0)
        g["dropped"] += len(c.gaps or [])
        g["open_charge"] |= x["flags"]["in_progress"] or c.cut_ms is not None
        g["tm"] += c.source == "teslamate"
    for s in h.segs_between(lo, end, req.now):
        for k, x, y in s.activity():
            if k != "unknown":
                continue
            for i, p, q in bk.split(max(x, lo), min(y, end)):
                acc[i]["unknown"] += q - p
        if s.kind in ("parked", "update"):
            for i, p, q in bk.split(max(s.start, lo), min(s.end, end)):
                acc[i]["stand"] += (s.kwh_at(q) - s.kwh_at(p)) if s.kind == "parked" else 0.6 * (q - p) / 3_600_000
    rows = []
    for (ps_, pe_, lab), g in zip(ps, acc):
        o0, o1 = derive.odo_at(h, ps_), derive.odo_at(h, pe_ - 1)
        span = max(1, pe_ - ps_)
        total = dict(g["cost"])
        for k, v in g["est"].items():
            total[k] = total.get(k, 0.0) + v
        days = max(1.0, (pe_ - ps_) / DAY)
        net = g["kwh"] / g["km"] * 1000 if g["km"] > 0.1 else None
        gross = (g["kwh"] + g["stand"]) / g["km"] * 1000 if g["km"] > 0.1 else None
        rows.append({
            "car": car.id, "time_zone": tz, "period_start_ms": ps_, "period_end_ms": pe_, "label": lab,
            "time_driven_ms": g["ms"], "drives": g["drives"],
            "distance_odometer_km": r(max(0.0, o1 - o0), 3) if o0 is not None and o1 is not None else None,
            "distance_logged_km": r(g["km"], 3),
            "outside_avg_c": r(g["temp_w"] / g["temp_ms"], 1) if g["temp_ms"] else None,
            "driving_efficiency_pct": r(car.rated_wh_per_km / net * 100, 1) if net else None,
            "energy_used_kwh": r(g["kwh"], 3), "energy_standing_kwh": r(g["stand"], 3),
            "energy_added_kwh": r(g["added"], 3), "energy_grid_kwh": r(g["grid"], 3), "charges": g["charges"],
            "energy_per_charge_kwh": r(g["added"] / g["charges"], 3) if g["charges"] else None,
            "cost_by_currency": _money(g["cost"]),
            "price_per_kwh": [{"currency": k, "value": round(v / g["basis"][k], 4)}
                              for k, v in sorted(g["cost"].items()) if g["basis"].get(k)],
            "cost_per_100km": [{"currency": k, "amount": round(v / g["km"] * 100, 2)}
                               for k, v in sorted(g["cost"].items())] if g["km"] > 1 else [],
            "charges_priced": g["priced"], "charges_estimated": g["estimated"],
            "cost_estimated_by_currency": _money(g["est"]),
            "cost_total_by_currency": _money(total),
            "cost_total_per_100km": [{"currency": k, "amount": round(v / g["km"] * 100, 2)}
                                     for k, v in sorted(total.items())] if g["km"] > 1 else [],
            "cost_total_per_day": [{"currency": k, "amount": round(v / days, 2)} for k, v in sorted(total.items())],
            "net_wh_per_km": r(net, 1), "gross_wh_per_km": r(gross, 1),
            "overhead_pct": r((gross - net) / net * 100, 1) if net and gross else None,
            "completeness": {"recorded_pct": r(100 * (span - g["unknown"]) / span, 2), "unknown_ms": g["unknown"],
                             "dropped_stretches": g["dropped"], "open_drive": g["open_drive"],
                             "open_charge": g["open_charge"], "teslamate_rows": g["tm"],
                             # the made-up odometer is one clean monotonic series per car: nothing to drop
                             "odometer_dropped": 0},
        })
    return rows


def statistics(req):
    kind = req.enum("period", KINDS, "month")
    a, b = req.period()
    car_ids = [req.car] if "car" in req.params else req.cars()
    rows = []
    for cid in car_ids:
        h = req.ds.history(cid)
        if h is not None:
            rows += stat_rows(req, h, kind, a, b)
    items, nxt = page(req, rows, key=lambda x: (-x["period_start_ms"], int(x["car"][1:])))
    out = {"period": kind, "items": items, "next": nxt}
    if "car" in req.params:
        out.update({"car": req.car, "time_zone": req.tz()})
    else:
        out["cars"] = car_ids
    return out


# ------------------------------------------------------------------ mileage


def mileage(req):
    group = req.enum("group", ("day", "week", "month", "year"), "month")
    a, b = req.period()
    h = req.history()
    out = {"car": req.car, "time_zone": req.tz(), "group": group}
    if h is None:
        out.update({"items": [], "next": None, "projection": {"km_per_year": None, "basis_days": 0}})
        return out
    rows = stat_rows(req, h, group, a, b)
    items = []
    for x in rows:
        o0, o1 = derive.odo_at(h, x["period_start_ms"]), derive.odo_at(h, x["period_end_ms"] - 1)
        dist = x["distance_odometer_km"]
        items.append({"period_start_ms": x["period_start_ms"], "period_end_ms": x["period_end_ms"],
                      "label": x["label"], "distance_km": dist, "odometer_start_km": r(o0, 1),
                      "odometer_end_km": r(o1, 1), "logged_km": x["distance_logged_km"],
                      "not_logged_km": r(max(0.0, (dist or 0) - (x["distance_logged_km"] or 0)), 3)
                      if dist is not None else None})
    lo = max(a, h.first_ms)
    end = min(b, req.now)
    o0, o1 = derive.odo_at(h, lo), derive.odo_at(h, end - 1)
    days = derive.days_between(lo, end)
    page_items, nxt = page(req, items, key=lambda x: (x["period_start_ms"],))
    out.update({"items": page_items, "next": nxt,
                "projection": {"km_per_year": r((o1 - o0) / days * 365.25, 0) if days >= 7 and o0 is not None else None,
                               "basis_days": r(days, 1)}})
    return out


# ------------------------------------------------------------------ totals


def totals(req):
    source = S.canon(req.enum("source", S.FILTER_WORDS, "all"))
    h = req.history()
    out = {"car": req.car, "time_zone": req.tz(), "source": source, "sources": S.filter_refs(source)}
    if h is None:
        from ..live import list_item
        it = list_item(req.app, req.car, req.now)
        out.update({"drives": 0, "distance_km": 0.0, "odometer_first_km": None, "odometer_last_km": None,
                    "odometer_last_ms": it["now"]["at_ms"], "not_logged_km": None, "charges": 0,
                    "energy_added_kwh": 0.0, "energy_grid_kwh": 0.0, "grid_coverage_pct": None,
                    "charging_efficiency_pct": None, "ac": {"charges": 0, "kwh": 0.0}, "dc": {"charges": 0, "kwh": 0.0},
                    "updates": 0})
        return out
    key = (source, req.store.rev, derive.started(h, req.now), req.now // 10_000)
    out.update(derive.memo(req.app, f"totals:{h.car.id}", key, lambda: _totals(req, h, source)))
    return out


def _totals(req, h, source):
    drows = [cached_drive_row(req, h, d) for d in derive.shown_drives(h, 0, req.now + 1, req.now, source)]
    crows = [cached_charge_row(req, h, c) for c in derive.shown_charges(h, 0, req.now + 1, req.now, source)]
    dist = sum(x["distance_km"] or 0 for x in drows)
    first = drows[0]["start_odometer_km"] if drows else None
    last = drows[-1]["end_odometer_km"] if drows else None
    last_ms = drows[-1]["end_ms"] or req.now if drows else None
    added = sum(x["energy_added_kwh"] or 0 for x in crows)
    with_grid = [x for x in crows if x["energy_grid_kwh"] is not None]
    grid = sum(x["energy_grid_kwh"] for x in with_grid)
    added_grid = sum(x["energy_added_kwh"] or 0 for x in with_grid)
    ac = [x for x in crows if x["kind"] == "ac"]
    dc = [x for x in crows if x["kind"] == "dc"]
    ups = [u for u in h.updates if u.start <= req.now and S.keeps(source, u.source, u.inputs)]
    return {
        "drives": len(drows), "distance_km": r(dist, 3), "odometer_first_km": first, "odometer_last_km": last,
        "odometer_last_ms": last_ms,
        "not_logged_km": r(max(0.0, (last - first) - dist), 3) if first is not None else None,
        "charges": len(crows), "energy_added_kwh": r(added, 3), "energy_grid_kwh": r(grid, 3),
        "grid_coverage_pct": r(100 * len(with_grid) / len(crows), 1) if crows else None,
        "charging_efficiency_pct": r(100 * added_grid / grid, 1) if grid else None,
        "ac": {"charges": len(ac), "kwh": r(sum(x["energy_added_kwh"] or 0 for x in ac), 3)},
        "dc": {"charges": len(dc), "kwh": r(sum(x["energy_added_kwh"] or 0 for x in dc), 3)},
        "updates": len(ups),
    }


# ------------------------------------------------------------------ gaps

CAUSE = {"no_log": "no_log", "not_recorded": "not_recorded", "failed_check": "unknown", "dropped_frames": "unknown"}


def gap_rows(req, h):
    rows = []
    for s in h.segs:
        if s.start > req.now:
            break
        if s.kind == "gap":
            live = s.open_end and s.end > req.now
            fp = eff_pid(req.app, *s.before, None) if s.before else None
            tp = eff_pid(req.app, *s.after, None) if s.after else None
            rows.append({"id": f"gap:{s.start}", "start_ms": s.start, "end_ms": None if live else s.end,
                         "duration_ms": (req.now if live else s.end) - s.start,
                         "odometer_jump_km": r(s.odo_jump, 3) if s.odo_jump else 0.0,
                         "from_place": ref(place_obj(req.app, fp)), "to_place": ref(place_obj(req.app, tp)),
                         "cause": CAUSE[s.reason], "gap_reason": s.reason, **S.of_seg(s), "inside": None})
        elif s.kind == "charge":
            for a, b, k in s.gaps or []:
                if k == "chain" and a <= req.now:
                    pid = eff_pid(req.app, s.lat, s.lon, s.pid)
                    rows.append({"id": f"gap:{a}", "start_ms": a, "end_ms": b, "duration_ms": b - a,
                                 "odometer_jump_km": 0.0, "from_place": ref(place_obj(req.app, pid)),
                                 "to_place": ref(place_obj(req.app, pid)), "cause": "chain_gap",
                                 "gap_reason": "chain_gap", **S.of_seg(s), "inside": {"charge": s.id}})
    return rows


def gaps(req):
    a, b = req.period()
    h = req.history()
    rows = [x for x in gap_rows(req, h) if a <= x["start_ms"] < b] if h is not None else []
    items, nxt = page(req, rows, key=lambda x: (x["start_ms"],), reverse=True)
    return {"car": req.car, "time_zone": req.tz(), "items": items, "next": nxt}


# ------------------------------------------------------------------ compare

METRICS = {"capacity": "kWh", "full_range": "km", "wh_per_km": "Wh/km", "odometer": "km"}


def compare(req):
    metric = req.enum("metric", tuple(METRICS), "capacity")
    a, b = req.period()
    min_d = req.float_param("min_distance_km", 0.0, 0, 100000)  # wh_per_km only: shorter drives are left out
    car_ids = req.cars()
    end = min(b, req.now)
    series = []
    for cid in car_ids:
        h = req.ds.history(cid)
        meta = req.store.car_meta(cid)
        t_ms, vals = [], []
        if h is not None:
            lo = max(a, h.first_ms)
            span = end - lo
            step = 7 * DAY if span > 120 * DAY else DAY
            tz = meta["time_zone"]
            ps = periods(lo, end, tz, "week" if step == 7 * DAY else "day")
            bk = Buckets(ps)
            if metric == "wh_per_km":
                acc = [[0.0, 0.0] for _ in ps]
                for d in derive.shown_drives(h, lo, end, req.now):
                    i = bk.index(d.start)
                    if i is not None:
                        x = cached_drive_row(req, h, d)
                        if (x["distance_km"] or 0) < min_d:
                            continue
                        acc[i][0] += x["energy_kwh"] or 0
                        acc[i][1] += x["distance_km"] or 0
                for (ps_, _pe, _l), (e, km) in zip(ps, acc):
                    if km > 1:
                        t_ms.append(ps_)
                        vals.append(r(e / km * 1000, 1))
            else:
                for ps_, pe_, _l in ps:
                    t = (ps_ + pe_) // 2
                    s = h.seg_at(t)
                    if metric == "odometer":
                        o = derive.odo_at(h, pe_ - 1)
                        if o is not None:
                            t_ms.append(ps_)
                            vals.append(r(o, 1))
                    elif s is not None and s.kind != "gap":
                        t_ms.append(ps_)
                        if metric == "capacity":
                            vals.append(r(bms_full(h.car, t) - h.car.buffer_kwh, 2))
                        else:
                            vals.append(r(h.car.full_range_km(t), 1))
        series.append({"car": cid, "name": meta["name"], "time_zone": meta["time_zone"], "t_ms": t_ms, "values": vals})
    return {"metric": metric, "unit": METRICS[metric], "cars": car_ids, "series": series}


def register(router):
    router.add("/api/v1/statistics", GET=statistics)
    router.add("/api/v1/cars/{car}/statistics", GET=statistics)
    router.add("/api/v1/cars/{car}/mileage", GET=mileage)
    router.add("/api/v1/cars/{car}/totals", GET=totals)
    router.add("/api/v1/cars/{car}/gaps", GET=gaps)
    router.add("/api/v1/compare", GET=compare)


__all__ = ["register", "stat_rows", "periods", "bad_input"]
