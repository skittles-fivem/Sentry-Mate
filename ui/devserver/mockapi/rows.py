"""Turning history pieces into API rows: Drive, Charge, timeline rows, costs and places.

Every function takes the app (for the store's owner overrides) and `now` (the virtual clock), so a
piece the clock is inside is shown "in progress": `end_ms: null` and figures up to now.
"""

from __future__ import annotations

import datetime as dt

from . import placing
from .util import DAY, MIN, local_dt, local_hour, r
from . import gapfill
from . import sources as S

DAYS = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"]


def place_obj(app, pid):
    """The effective place (the dataset's, with the owner's changes); None when unknown or deleted."""
    return placing.place_obj(app, pid)


def _point(app, lat, lon, pid=None, label=None, at_place=True):
    p = placing.point(app, lat, lon, pid, label, at_place)
    p.pop("_address", None)
    return p


def in_progress(seg, now):
    return seg.start <= now < getattr(seg, "data_end", seg.end) and getattr(seg, "cut_ms", None) is None and (
        seg.kind != "gap" or seg.open_end)


# ------------------------------------------------------------------ purposes


def _day_matches(spec, weekday):
    if not spec or spec == "all":
        return True
    for part in spec.split(","):
        part = part.strip()
        if "-" in part:
            a, b = part.split("-", 1)
            if a in DAYS and b in DAYS:
                ia, ib = DAYS.index(a), DAYS.index(b)
                if ia <= weekday <= ib if ia <= ib else (weekday >= ia or weekday <= ib):
                    return True
        elif part in DAYS and DAYS.index(part) == weekday:
            return True
    return False


def purpose_of(app, car_id, d, tz):
    own = app.store.purposes.get(d.id)
    if own:
        return {"value": own["purpose"], "relation": own.get("relation"), "note": own.get("note"), "source": "owner"}
    wd = local_dt(d.start, tz).weekday()
    rules = app.store.rules.get(car_id, [])
    if not rules:
        return None
    a, b = placing.drive_pids(app, d)
    if d.cut_ms is not None:
        b = None
    for rule in rules:
        if rule["from_place"] == a and rule["to_place"] == b and _day_matches(rule["days"], wd):
            return {"value": rule["purpose"], "relation": None, "note": None, "source": "rule", "rule": rule["id"]}
    return None


# ------------------------------------------------------------------ drives


def drive_row(app, h, d, now):
    car = h.car
    meta = app.store.car_meta(car.id)
    live = in_progress(d, now)
    f = d.figures(now if live else None)
    cut = d.cut_ms is not None
    dist = f["dist_km"]
    energy = f["energy"]
    whkm = energy / dist * 1000 if dist > 0.05 else None
    tm = d.source == "teslamate"
    legacy = car.profile == "tesla-sx-legacy"
    end_label = "Data stopped here" if cut else ("Driving now" if live else None)
    lost = sum(int((b - a) / 1000 * 140) for a, b in d.drops)
    duration = (now if live else d.data_end) - d.start
    row = {
        "id": d.id, "car": car.id, "time_zone": meta["time_zone"],
        "start_ms": d.start, "end_ms": None if live else d.data_end, "duration_ms": duration,
        "start": _point(app, *f["start_pt"], pid=d.from_pid),
        "end": _point(app, *f["end_pt"], pid=d.to_pid, label=end_label, at_place=not cut and not live),
        "start_odometer_km": r(d.odo0, 3), "end_odometer_km": r(d.odo0 + dist, 3), "distance_km": r(dist, 3),
        "speed_avg_kph": r(dist / (duration / 3_600_000), 1) if duration > 0 else None,
        "speed_max_kph": r(f["vmax"], 1),
        "power_max_kw": r(f["pmax"], 1), "power_min_kw": None if tm else r(f["pmin"], 1),
        # (the Tesla API alone: its power read every few seconds to a minute, summed)
        "energy_kwh": r(energy, 3),
        "energy_basis": "estimated" if tm or getattr(d, "inputs", None) == ["tesla_api"] else "measured",
        "regen_kwh": None if tm else r(f["regen"], 3),
        "wh_per_km": r(whkm, 1),
        "efficiency_pct": r(car.rated_wh_per_km / whkm * 100, 1) if whkm else None,
        "start_soc_pct": r(d.soc0, 1), "end_soc_pct": r(f["soc1"], 1),
        "start_range_km": r(f["range0"], 1), "end_range_km": r(f["range1"], 1),
        "outside_avg_c": r(d.outside, 1),
        "cabin_avg_c": None if legacy and not tm else r((d.cabin0 + d.setpoint * 3) / 4, 1),
        "climb_m": r(f["climb"], 0), "descent_m": r(f["descent"], 0),
        "hvac_kwh": None if tm else r(f["hvac"], 3),
        **S.of_seg(d),
        "flags": {"in_progress": live, "gap_ms": int(f["gap_ms"]), "lost_frames": lost,
                  "reduced_range": bool(d.reduced_range)},
        "purpose": purpose_of(app, car.id, d, meta["time_zone"]),
        "_whkm_hill": (energy - d.climb_kwh * f["frac"]) / dist * 1000 if dist > 0.05 else None,
    }
    # what its import has where the live drive has nothing (docs/ui-api.md, "Values filled from an import")
    return gapfill.fill_row(app, h, d, row, lambda t: drive_row(app, h, t, now))


# ------------------------------------------------------------------ charges and costs


def _rate_at(price, ms, tz):
    if not price:
        return None
    d = local_dt(ms, tz)
    hm = d.hour * 60 + d.minute
    for rate in price.get("rates") or []:
        if not _day_matches(rate.get("days"), d.weekday()):
            continue
        a = _hm(rate.get("from", "00:00"))
        b = _hm(rate.get("to", "24:00"))
        if (a <= hm < b) if a <= b else (hm >= a or hm < b):
            return rate
    return None


def _hm(s):
    try:
        h, m = s.split(":")
        return int(h) * 60 + int(m)
    except Exception:
        return 0


def free_supercharging(app, car_id, ms, tz):
    s = app.store.car_settings.get(car_id) or {}
    day = local_dt(ms, tz).date()
    for p in s.get("free_supercharging") or []:
        a = dt.date.fromisoformat(p["from_date"]) if p.get("from_date") else dt.date.min
        b = dt.date.fromisoformat(p["to_date"]) if p.get("to_date") else dt.date.max
        if a <= day <= b:
            return True
    return False


def _midnight(y, m, d, tz):
    from .util import zone
    return int(dt.datetime(y, m, d, tzinfo=zone(tz)).timestamp() * 1000)


def tier_cycle(tiers, ms, tz):
    """[start, end) ms of the billing cycle `ms` is in: midnight on the cycle day (1 to 28) to a month later."""
    day = int(tiers.get("cycle_day") or 1)
    d = local_dt(ms, tz)
    y, m = d.year, d.month
    if d.day < day:
        y, m = (y - 1, 12) if m == 1 else (y, m - 1)
    ny, nm = (y + 1, 1) if m == 12 else (y, m + 1)
    return _midnight(y, m, day, tz), _midnight(ny, nm, day, tz)


_CYCLE_CACHE = {}


def cycle_kwh(app, pid, a, c):
    """kWh charged at place `pid` from `a` up to charge `c` (started before it, or at the same moment and listed
    before it), by every car: each charge's grid energy, else what went into the pack."""
    sig = tuple((p["id"], p["lat"], p["lon"], p["radius_m"]) for p in placing.all_places(app))
    key = (pid, a, sig)
    rows = _CYCLE_CACHE.get(key)
    if rows is None:
        rows = []
        for cid, h in app.ds.featured.items():
            for o in h.charges:
                if o.start < a or not h.shown(o) or placing.piece_pid(app, o) != pid:
                    continue
                ff = o.figures()
                k = ff["grid"] if ff["grid"] is not None else ff["added"]
                rows.append((o.start, cid, o.id, max(0.0, k or 0.0)))
        rows.sort()
        if len(_CYCLE_CACHE) > 512:
            _CYCLE_CACHE.clear()
        _CYCLE_CACHE[key] = rows
    return sum(k for (st, _cid, rid, k) in rows if (st, rid) < (c.start, c.id))


def _tier_split(steps, at, kwh, parts):
    """Adds `kwh` used from `at` of the cycle on to the steps' parts; answers where the cycle is after it."""
    while kwh > 1e-12:
        i = max(0, sum(1 for s in steps if s["from_kwh"] <= at) - 1)
        nxt = steps[i + 1]["from_kwh"] if i + 1 < len(steps) else None
        take = kwh if nxt is None or nxt <= at else min(kwh, nxt - at)
        name = f"Step {i + 1}"
        p = parts.setdefault(name, {"rate": name, "per_kwh": steps[i]["per_kwh"], "per_minute": None, "kwh": 0.0,
                                    "minutes": 0.0, "cost": 0.0, "from_kwh": steps[i]["from_kwh"], "to_kwh": nxt})
        p["kwh"] += take
        p["cost"] += take * steps[i]["per_kwh"]
        at += take
        kwh -= take
    return at


def _no_cost(gap_ms):
    return {"parts": [], "session_fee": None, "total": None, "gap_ms": gap_ms, "mode": None, "cycle": None}


def charge_cost(app, car_id, c, f, now, pricing=None):
    """(cost, price_per_kwh, cost_breakdown) for a charge, applying owner overrides first.

    `pricing` is (place, price) to cost it with instead of its own (a preview or a reprice)."""
    end = min(now, c.data_end)
    minutes = (end - c.start) / MIN
    added = f["added"]
    grid = f["grid"]
    inlet = f["inlet"]
    pl, price = pricing if pricing is not None else placing.pricing(app, c)
    tz = pl["time_zone"] if pl else app.store.car_meta(car_id)["time_zone"]
    gap_ms = int(f["gap_ms"])
    over = app.store.costs.get(c.id)
    if over:
        basis = over.get("energy_basis") or ("grid" if grid is not None else "pack")
        kwh = grid if basis == "grid" and grid is not None else added
        mode = over["mode"]
        if mode == "total":
            amount = over["amount"]
        elif mode == "per_kwh":
            amount = over["amount"] * kwh
        else:
            amount = over["amount"] * minutes
        cost = {"amount": round(amount, 2), "currency": over["currency"], "source": "manual"}
        # an entered rate is given back exactly as entered (up to 5 decimals), not worked back from the total
        per_kwh = over["amount"] if mode == "per_kwh" else (round(amount / kwh, 4) if kwh else None)
        ppk = {"value": per_kwh, "basis": basis} if per_kwh is not None else None
        bd = {"parts": [{"rate": "Entered", "per_kwh": per_kwh, "kwh": r(kwh, 3),
                         "per_minute": over["amount"] if mode == "per_minute" else None,
                         "minutes": r(minutes, 1) if mode == "per_minute" else None,
                         "cost": round(amount, 2)}], "session_fee": None, "total": round(amount, 2),
              "gap_ms": gap_ms, "mode": mode, "cycle": None}
        return cost, ppk, bd
    if pricing is None:  # a Tesla invoice matched to the charge (a preview or a reprice prices by the place)
        from .resources import invoices
        inv = invoices.charge_cost(app, c, f, gap_ms)
        if inv is not None:
            return inv
    if c.source == "teslamate":
        if c.tm_cost is None:
            return None, None, _no_cost(gap_ms)
        amount = round(c.tm_cost * (grid or added), 2)
        cost = {"amount": amount, "currency": "USD", "source": "teslamate"}
        return cost, {"value": c.tm_cost, "basis": "grid" if grid else "pack"}, {
            "parts": [{"rate": "Imported", "per_kwh": c.tm_cost, "kwh": r(grid or added, 3), "cost": amount}],
            "session_fee": None, "total": amount, "gap_ms": gap_ms, "mode": "teslamate", "cycle": None}
    if c.ckind == "dc" and c.station_kw >= 250 and free_supercharging(app, car_id, c.start, tz):
        cur = price["currency"] if price else "USD"
        return ({"amount": 0.0, "currency": cur, "source": "free_supercharging"}, {"value": 0.0, "basis": "pack"},
                {"parts": [], "session_fee": None, "total": 0.0, "gap_ms": gap_ms, "mode": "free_supercharging",
                 "cycle": None})
    if not price:
        return None, None, _no_cost(gap_ms)
    return _tariff_cost(app, c, f, end, price, tz, pl)


def _tariff_cost(app, c, f, end, price, tz, pl):
    """A charge costed with a place's price (rates by time of day, per kWh and per minute, a session fee)."""
    added, grid, inlet = f["added"], f["grid"], f["inlet"]
    gap_ms = int(f["gap_ms"])
    basis = "grid" if grid is not None else "pack"
    # AC is billed by the grid meter; a fast charger bills what it delivered (about the car's inlet energy)
    kwh = grid if grid is not None else (inlet if inlet is not None else added)
    parts = {}
    total_ms = max(1, end - c.start)
    step = 15 * MIN
    tiers = price.get("tiers")
    steps = sorted((tiers or {}).get("steps") or [], key=lambda x: x["from_kwh"])
    cycle = None
    if steps:
        a, b = tier_cycle(tiers, c.start, tz)
        before = (tiers.get("base_kwh") or 0) + cycle_kwh(app, pl["id"] if pl else None, a, c)
        cycle = {"start_ms": a, "end_ms": b, "before_kwh": r(before, 3)}
        at = before
    t = c.start
    while t < end:
        e = min(end, t + step)
        rate = _rate_at(price, t, tz)
        share = kwh * (e - t) / total_ms
        if steps and not (rate and rate.get("per_kwh") is not None):
            # tiered prices: a step's price where no time-of-use rate sets one
            at = _tier_split(steps, at, share, parts)
            t = e
            continue
        name = rate["name"] if rate else "Standard"
        per = rate["per_kwh"] if rate and rate.get("per_kwh") is not None else price.get("per_kwh")
        pm = rate["per_minute"] if rate and rate.get("per_minute") is not None else price.get("per_minute")
        p = parts.setdefault(name, {"rate": name, "per_kwh": per, "per_minute": pm, "kwh": 0.0, "minutes": 0.0,
                                    "cost": 0.0, "from_kwh": None, "to_kwh": None})
        p["kwh"] += share
        p["minutes"] += (e - t) / MIN
        p["cost"] += share * (per or 0) + (e - t) / MIN * (pm or 0)
        if steps:
            at += share
        t = e
    order = sorted(parts.values(), key=lambda p: (p["from_kwh"] is not None, p["from_kwh"] or 0))
    plist = [{"rate": p["rate"], "per_kwh": p["per_kwh"], "per_minute": p["per_minute"], "kwh": r(p["kwh"], 3),
              "minutes": r(p["minutes"], 1) if p["from_kwh"] is None else None, "cost": round(p["cost"], 2),
              "from_kwh": p["from_kwh"], "to_kwh": p["to_kwh"]} for p in order]
    fee = price.get("session_fee")
    amount = sum(p["cost"] for p in parts.values()) + (fee or 0)
    cost = {"amount": round(amount, 2), "currency": price["currency"], "source": "tariff"}
    per = grid if basis == "grid" else added
    ppk = {"value": round(amount / per, 4), "basis": basis} if per else None
    return cost, ppk, {"parts": plist, "session_fee": fee, "total": round(amount, 2), "gap_ms": gap_ms,
                       "mode": "tariff", "cycle": cycle}


def charge_estimate(app, car_id, c, row, now):
    """{amount, currency} for a charge that has no cost (row["cost"] is None), or None: the price of the
    place it is at now (a price set after the charge ended), else the car's `default_price_per_kwh` on its
    energy from the grid, else the energy added. Statistics shows it apart; it never becomes the cost."""
    added, grid = row["energy_added_kwh"], row["energy_grid_kwh"]
    pl = place_obj(app, placing.piece_pid(app, c))
    if pl and pl.get("price"):
        f = {"added": added or 0.0, "grid": grid, "inlet": row["dc_inlet_kwh"], "gap_ms": row["flags"]["gap_ms"]}
        end = min(now, c.data_end)
        cost = _tariff_cost(app, c, f, end, pl["price"], pl["time_zone"], pl)[0]
        return {"amount": cost["amount"], "currency": cost["currency"]}
    dp = (app.store.car_settings.get(car_id) or {}).get("default_price_per_kwh")
    kwh = grid if grid is not None else added
    if dp is None or kwh is None:
        return None
    return {"amount": round(dp["value"] * kwh, 2), "currency": dp["currency"]}


def charge_row(app, h, c, now, with_breakdown=False):
    car = h.car
    meta = app.store.car_meta(car.id)
    live = in_progress(c, now)
    f = c.figures(now if live else None)
    pl = place_obj(app, placing.piece_pid(app, c))
    addr = pl["address"] if pl else _point(app, c.lat, c.lon, at_place=False)["address"]
    cost, ppk, bd = charge_cost(app, car.id, c, f, now)
    end = now if live else c.data_end
    plug_end = now if live else c.plug_end
    dc = c.ckind == "dc"
    tm = c.source == "teslamate"
    hours = (end - c.start) / 3_600_000
    row = {
        "id": c.id, "car": car.id, "time_zone": meta["time_zone"],
        "start_ms": c.start, "end_ms": None if live else c.data_end,
        "charging_ms": end - c.start, "plugged_ms": max(end, plug_end) - min(c.start, c.plug_start),
        "kind": c.ckind,
        # what the charger says about itself (CAN), not the place name
        "fast_charger": ({"type": "supercharger" if c.station_kw >= 250 else "ccs",
                          "brand": "Tesla" if c.station_kw >= 250 else None,
                          "class": "V3" if c.station_kw >= 250 else None, "billed_kwh": r(f["inlet"], 3)}
                         if dc else None),
        "place": {"id": pl["id"], "name": pl["name"], "kind": pl["kind"]} if pl else None,
        "address": {k: addr.get(k) for k in ("short", "locality", "region", "country_code")},
        "position": {"lat": round(c.lat, 6), "lon": round(c.lon, 6), "accuracy_m": 6.0},
        "odometer_km": r(getattr(c, "odo", None), 3),
        "energy_added_kwh": r(f["added"], 3),
        "energy_grid_kwh": r(f["grid"], 3),
        "dc_inlet_kwh": r(f["inlet"], 3),
        "losses_kwh": r(f["grid"] - f["added"], 3) if f["grid"] is not None else None,
        "efficiency_pct": r(f["added"] / f["grid"] * 100, 1) if f["grid"] else None,
        # the server's reason when added and grid energy were measured over different spans (none here)
        "efficiency_note": None,
        "cost": cost, "price_per_kwh": ppk,
        "start_soc_pct": r(c.soc0, 1), "end_soc_pct": r(f["soc1"], 1),
        "start_range_km": r(f["range0"], 1), "end_range_km": r(f["range1"], 1),
        "range_added_km": r(f["range_added"], 1),
        "range_per_hour_km": r(f["range_added"] / hours, 1) if hours > 0.01 else None,
        "power_max_kw": r(f["pmax"], 1), "power_avg_kw": r(f["pavg"], 1),
        "outside_avg_c": r(c.outside, 1), "battery_avg_c": None if tm else r(f["batt_avg"], 1),
        **S.of_seg(c), "has_gaps": bool(c.gaps) or c.cut_ms is not None,
        "flags": {"in_progress": live, "gap_ms": int(f["gap_ms"])},
    }
    gapfill.fill_row(app, h, c, row, lambda t: charge_row(app, h, t, now))
    if with_breakdown:
        row["_breakdown"] = bd
    return row


# ------------------------------------------------------------------ timeline


def _tl_point(app, lat, lon, pid=None):
    p = _point(app, lat, lon, pid)
    return {"place": p["place"], "address": p["address"]["short"], "lat": p["lat"], "lon": p["lon"],
            "label": p["label"]}


def timeline_row(app, h, s, now):
    car = h.car
    live = in_progress(s, now)
    base = {"id": f"{s.kind}:{s.start}", "kind": s.kind, "start_ms": s.start, **S.of_seg(s),
            "cost": None, "ref": None, "parked_ms": None, "awake_reasons": [], "gap_reason": None,
            "energy_basis": None, "value_sources": {}}
    if s.kind == "drive":
        d = drive_row(app, h, s, now)
        base.update({
            "end_ms": d["end_ms"], "start": _tl_point(app, d["start"]["lat"], d["start"]["lon"], s.from_pid),
            "end": None if live else {"place": d["end"]["place"], "address": d["end"]["address"]["short"],
                                      "lat": d["end"]["lat"], "lon": d["end"]["lon"], "label": d["end"]["label"]},
            "odometer_km": d["end_odometer_km"], "distance_km": d["distance_km"], "duration_ms": d["duration_ms"],
            "soc_end_pct": d["end_soc_pct"], "soc_delta_pct": r(d["end_soc_pct"] - d["start_soc_pct"], 1),
            "range_end_km": d["end_range_km"], "range_delta_km": r(d["end_range_km"] - d["start_range_km"], 1),
            "energy_delta_kwh": r(-d["energy_kwh"], 3), "energy_basis": d["energy_basis"],
            "outside_avg_c": d["outside_avg_c"], "ref": {"drive": s.id}, "flags": d["flags"],
            # as the drives list shows it: with what the chosen import filled (mockapi/gapfill.py)
            **{k: d[k] for k in ("sources", "source_label", "value_sources")},
        })
        return base
    if s.kind == "charge":
        c = charge_row(app, h, s, now)
        base.update({
            "end_ms": c["end_ms"], "start": _tl_point(app, s.lat, s.lon, s.pid), "end": None,
            "odometer_km": c["odometer_km"], "distance_km": None, "duration_ms": c["charging_ms"],
            "soc_end_pct": c["end_soc_pct"], "soc_delta_pct": r(c["end_soc_pct"] - c["start_soc_pct"], 1),
            "range_end_km": c["end_range_km"], "range_delta_km": c["range_added_km"],
            "energy_delta_kwh": c["energy_added_kwh"], "energy_basis": (
                "measured" if s.source == "can" and getattr(s, "inputs", None) != ["tesla_api"] else "estimated"
            ),
            "outside_avg_c": c["outside_avg_c"], "cost": c["cost"], "ref": {"charge": s.id},
            "awake_reasons": ["charging"], "flags": c["flags"],
        })
        return base
    end = now if live else s.end
    if s.kind == "parked":
        soc0 = s.soc_at(s.start)
        soc1 = s.soc_at(end)
        full0 = car.full_range_km(s.start)
        full1 = car.full_range_km(end)
        split = s.split_ms() if not live else s.clip(s.start, end).split_ms()
        base.update({
            "end_ms": None if live else s.end, "start": _tl_point(app, s.lat, s.lon, s.pid), "end": None,
            "odometer_km": r(getattr(s, "odo", None), 3), "distance_km": None, "duration_ms": end - s.start,
            "soc_end_pct": r(soc1, 1), "soc_delta_pct": r(soc1 - soc0, 2),
            "range_end_km": r(soc1 / 100 * full1, 1), "range_delta_km": r(soc1 / 100 * full1 - soc0 / 100 * full0, 1),
            "energy_delta_kwh": r(-(s.kwh_at(end) - s.kwh_at(s.start)), 3), "energy_basis": "measured",
            "outside_avg_c": r(s.outside0, 1), "parked_ms": split, "awake_reasons": s.reasons(),
        })
        return base
    if s.kind == "gap":
        bp = _tl_point(app, *s.before) if s.before else None
        ap = _tl_point(app, *s.after) if s.after and s.after != s.before else None
        base.update({
            "end_ms": None if live else s.end, "start": bp, "end": ap,
            "odometer_km": None, "distance_km": r(s.odo_jump, 3) if s.odo_jump else None,
            "duration_ms": end - s.start, "soc_end_pct": None, "soc_delta_pct": None, "range_end_km": None,
            "range_delta_km": None, "energy_delta_kwh": None, "outside_avg_c": None, "gap_reason": s.reason,
            "parked_ms": {"asleep": 0, "awake": 0, "awake_vehicle_bus_only": 0, "unknown": end - s.start},
        })
        return base
    # software update
    from .live import update_version
    ver, vsrc = update_version(app, s)
    base.update({
        "end_ms": None if live else s.end, "start": _tl_point(app, s.lat, s.lon, s.pid), "end": None,
        "odometer_km": None, "distance_km": None, "duration_ms": end - s.start, "soc_end_pct": r(s.soc, 1),
        "soc_delta_pct": 0.0, "range_end_km": None, "range_delta_km": None, "energy_delta_kwh": None,
        "outside_avg_c": None, "ref": {"update": s.id}, "version": ver, "version_source": vsrc if ver else None,
    })
    if s.source == "can" and s.detected_by == "fingerprint":  # found from the modules' firmware fingerprints
        base.update({"detected_by": "fingerprint", "changed_modules": list(s.changed_modules)})
    return base


def local_date_str(ms, tz):
    return local_dt(ms, tz).date().isoformat()


def local_hour_of(ms, tz):
    return local_hour(ms, tz)


__all__ = ["drive_row", "charge_row", "timeline_row", "charge_cost", "place_obj", "in_progress", "DAY"]
