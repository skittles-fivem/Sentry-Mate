"""Charges (docs/ui-api.md, "Charges"): lists for one car or several, detail with the cost
breakdown, the 1 Hz curve, manual costs, charging stats, stats per place and the DC curve.
"""

from __future__ import annotations

from ..conventions import nk, page
from ..dataset.segments import CURVE_FIELDS
from ..errors import bad_input, invalid, not_found
from ..placing import piece_pid
from ..rows import charge_row, in_progress, place_obj
from ..util import DAY, MIN, local_dt, median, r

SORTS = {"start": lambda c: c["start_ms"], "energy": lambda c: c["energy_added_kwh"],
         "cost": lambda c: (c["cost"] or {}).get("amount"), "duration": lambda c: c["charging_ms"],
         "power_max": lambda c: c["power_max_kw"], "end_soc": lambda c: c["end_soc_pct"]}
PLACE_KINDS = ("home", "work", "charger", "service", "other", "none")
CURRENCIES = ("USD", "CAD", "EUR", "GBP", "NOK", "SEK", "DKK", "CHF", "AUD", "NZD", "JPY", "CNY")


def cached_charge_row(req, h, c):
    if in_progress(c, req.now):
        return charge_row(req.app, h, c, req.now, with_breakdown=True)
    cache = req.app.cache.setdefault("charge_rows", {})
    if cache.get("_rev") != req.store.rev:  # something changed: start the cache again
        cache.clear()
        cache["_rev"] = req.store.rev
    key = (c.id, req.store.rev)
    row = cache.get(key)
    if row is None:
        row = charge_row(req.app, h, c, req.now, with_breakdown=True)
        cache[key] = row
    return dict(row)


def _charges_of(req, h, a, b, source="all"):
    return [c for c in h.charges if a <= c.start < b and c.start <= req.now and h.shown(c, source)]


def _place_ids(req, name="places"):
    ids = req.list_param(name)
    if ids:
        unknown = [p for p in ids if place_obj(req.app, p) is None]
        if unknown:
            raise bad_input({name: "Unknown place: " + ", ".join(unknown[:5]) + "."})
    return ids


def _filtered(req, car_ids):
    a, b = req.period()
    places = _place_ids(req)
    pkind = req.enum("place_kind", PLACE_KINDS)
    kind = req.enum("kind", ("ac", "dc", "unknown"))
    q = (req.q("q") or "").strip().lower()
    min_cost = req.float_param("min_cost", None, 0, 1e9)
    incl_unpriced = req.bool_param("include_unpriced", False)
    min_dur = req.float_param("min_duration_min", None, 0, 100000)
    source = req.enum("source", ("can", "teslamate", "all"), "all")
    flags = req.list_param("flags", ("gap", "in_progress"))
    rows = []
    for cid in car_ids:
        h = req.ds.history(cid)
        if h is None:
            continue
        for c in _charges_of(req, h, a, b, source):
            if places and piece_pid(req.app, c) not in places:
                continue
            if kind and c.ckind != kind:
                continue
            x = cached_charge_row(req, h, c)
            if pkind:
                pk = (x["place"] or {}).get("kind") or "none"
                if pk != pkind:
                    continue
            if min_dur is not None and x["charging_ms"] < min_dur * MIN:
                continue
            if min_cost is not None:
                if x["cost"] is None:
                    if not incl_unpriced:
                        continue
                elif x["cost"]["amount"] < min_cost:
                    continue
            if flags:
                if not (("gap" in flags and x["has_gaps"]) or ("in_progress" in flags and x["flags"]["in_progress"])):
                    continue
            if q:
                text = " ".join(filter(None, [(x["place"] or {}).get("name"), x["address"]["short"],
                                              x["address"]["locality"], x["kind"],
                                              (x["fast_charger"] or {}).get("type")])).lower()
                if q not in text:
                    continue
            rows.append(x)
    return rows


def totals(rows):
    by_cur = {}
    for x in rows:
        if x["cost"]:
            by_cur[x["cost"]["currency"]] = by_cur.get(x["cost"]["currency"], 0.0) + x["cost"]["amount"]
    grid_known = [x for x in rows if x["energy_grid_kwh"] is not None]
    return {
        "count": len(rows),
        "energy_added_kwh": r(sum(x["energy_added_kwh"] or 0 for x in rows), 3),
        "energy_grid_kwh": r(sum(x["energy_grid_kwh"] for x in grid_known), 3),
        "energy_grid_known": len(grid_known),
        "dc_inlet_kwh": r(sum(x["dc_inlet_kwh"] or 0 for x in rows), 3),
        "cost_by_currency": [{"currency": k, "amount": round(v, 2)} for k, v in sorted(by_cur.items())],
        "unpriced": sum(1 for x in rows if x["cost"] is None),
        "avg_duration_ms": int(sum(x["charging_ms"] for x in rows) / len(rows)) if rows else None,
    }


def _list(req, car_ids, one_car):
    sort = req.enum("sort", tuple(SORTS), "start")
    d = req.enum("dir", ("asc", "desc"), "desc")
    rows = _filtered(req, car_ids)
    rev = d == "desc"
    items, nxt = page(req, rows, key=lambda x: (nk(SORTS[sort](x), rev), x["start_ms"], x["id"]), reverse=rev)
    out = {"items": items, "next": nxt, "totals": totals(rows)}
    if one_car:
        out.update({"car": req.car, "time_zone": req.tz()})
    else:
        out["cars"] = car_ids
    return out


def list_all(req):
    return _list(req, req.cars(), False)


def list_car(req):
    return _list(req, [req.car], True)


def _charge(req):
    h = req.history()
    c = h.by_id.get(req.params["charge"]) if h else None
    if c is None or c.kind != "charge" or c.start > req.now:
        raise not_found("no_such_charge", "There is no such charge for this car.")
    return h, c


def get_charge(req):
    h, c = _charge(req)
    row = cached_charge_row(req, h, c)
    vis = [x for x in h.charges if x.start <= req.now and h.shown(x)]
    ids = [x.id for x in vis]
    i = ids.index(c.id) if c.id in ids else None
    row["prev"] = ids[i - 1] if i else None
    row["next"] = ids[i + 1] if i is not None and i + 1 < len(ids) else None
    row["cost_breakdown"] = row.pop("_breakdown")
    gaps = [{"start_ms": a, "end_ms": b, "kind": k} for a, b, k in c.gaps]
    if c.cut_ms is not None:
        gaps.append({"start_ms": c.cut_ms, "end_ms": None, "kind": "unknown"})
    row["gaps"] = gaps
    row["manual_cost"] = req.store.costs.get(c.id)
    return row


def get_curve(req):
    h, c = _charge(req)
    fields = req.list_param("fields", CURVE_FIELDS) or ["power_kw", "soc_pct", "volts_v", "amps_a", "battery_c"]
    end = min(req.now, c.data_end)
    span = max(1000, end - c.start)
    max_points = req.int_param("max_points", 4000, 2, 100000)
    step = 1000 if c.source == "can" else 30_000
    while span / step > max_points:
        step *= 2
    t_ms, cols = [], {f: [] for f in fields}
    t = c.start
    while t <= end:
        v = c.curve(t)
        t_ms.append(t)
        for f in fields:
            cols[f].append(None if v is None else v.get(f))
        t += step
    return {"car": req.car, "time_zone": req.tz(), "charge": c.id, "kind": c.ckind, "t_ms": t_ms, "values": cols,
            "step_ms": step}


def put_cost(req):
    h, c = _charge(req)
    body = req.json()
    bad, rule = {}, {}
    for k in body:
        if k not in ("mode", "amount", "currency", "energy_basis"):
            bad[k] = "Unknown field."
    for k in ("mode", "amount", "currency"):
        if k not in body:
            bad[k] = "Required."
    if "amount" in body and (isinstance(body["amount"], bool) or not isinstance(body["amount"], (int, float))):
        bad["amount"] = "Must be a number."
    if bad:
        raise bad_input(bad)
    if body["mode"] not in ("total", "per_kwh", "per_minute"):
        rule["mode"] = "Must be total, per_kwh or per_minute."
    if not 0 <= body["amount"] <= 100000:
        rule["amount"] = "Must be between 0 and 100000."
    if body["currency"] not in CURRENCIES:
        rule["currency"] = "A currency code such as USD, CAD or EUR."
    if body.get("energy_basis") not in (None, "grid", "pack"):
        rule["energy_basis"] = "Must be grid or pack."
    if rule:
        raise invalid(rule)
    st = req.store
    with st.lock:
        st.costs[c.id] = {"mode": body["mode"], "amount": float(body["amount"]), "currency": body["currency"],
                          "energy_basis": body.get("energy_basis")}
    st.record(req.account, "charge.cost", f"charge:{c.id}",
              f"Entered a cost for a charge: {body['amount']} {body['currency']} ({body['mode']}).")
    return get_charge(req)


def delete_cost(req):
    h, c = _charge(req)
    st = req.store
    with st.lock:
        st.costs.pop(c.id, None)
    st.record(req.account, "charge.cost", f"charge:{c.id}", "Removed the entered cost; the place price applies.")
    return get_charge(req)


# ------------------------------------------------------------------ stats


def _band(soc):
    b = min(9, int(soc // 10))
    return f"{b * 10}-{b * 10 + 10}"


def _stats_rows(req):
    car_ids = req.cars() if "car" not in req.params else [req.car]
    a, b = req.period()
    places = _place_ids(req)
    min_dur = req.float_param("min_duration_min", None, 0, 100000)
    rows = []
    for cid in car_ids:
        h = req.ds.history(cid)
        if h is None:
            continue
        for c in _charges_of(req, h, a, b):
            if places and piece_pid(req.app, c) not in places:
                continue
            x = cached_charge_row(req, h, c)
            if min_dur is not None and x["charging_ms"] < min_dur * MIN:
                continue
            x["_tz"] = h.car.time_zone
            rows.append(x)
    return car_ids, a, b, rows


def charging_stats(req):
    car_ids, a, b, rows = _stats_rows(req)
    by_kind = {k: {"kwh": 0.0, "ms": 0} for k in ("ac", "dc", "unknown")}
    for x in rows:
        k = by_kind[x["kind"] if x["kind"] in by_kind else "unknown"]
        k["kwh"] += x["energy_added_kwh"] or 0
        k["ms"] += x["charging_ms"]
    for k in by_kind.values():
        k["kwh"] = r(k["kwh"], 3)
    t = totals(rows)

    def ppk(sel):
        priced = [x for x in sel if x["cost"] and x["price_per_kwh"]]
        cur = {}
        for x in priced:
            e = cur.setdefault(x["cost"]["currency"], [0.0, 0.0])
            basis_kwh = (x["energy_grid_kwh"] if x["price_per_kwh"]["basis"] == "grid" else
                         (x["dc_inlet_kwh"] or x["energy_added_kwh"]))
            e[0] += x["cost"]["amount"]
            e[1] += basis_kwh or 0
        return [{"currency": k, "value": round(v[0] / v[1], 4) if v[1] else None} for k, v in sorted(cur.items())]

    sc = {}
    for x in rows:
        if x["fast_charger"] and x["fast_charger"]["type"] == "supercharger" and x["cost"]:
            sc[x["cost"]["currency"]] = sc.get(x["cost"]["currency"], 0.0) + x["cost"]["amount"]
    # cost per 100 km: money spent over distance driven in the period
    dist = 0.0
    for cid in car_ids:
        h = req.ds.history(cid)
        if h is None:
            continue
        for d in h.drives:
            if a <= d.start < min(b, req.now) and h.shown(d):
                dist += d.figures(req.now if in_progress(d, req.now) else None)["dist_km"]
    heat_s = [[0] * 24 for _ in range(7)]
    heat_e = [[0] * 24 for _ in range(7)]
    sb, eb = {}, {}
    for x in rows:
        ls = local_dt(x["start_ms"], x["_tz"])
        heat_s[ls.weekday()][ls.hour] += 1
        if x["end_ms"]:
            le = local_dt(x["end_ms"], x["_tz"])
            heat_e[le.weekday()][le.hour] += 1
        sb[_band(x["start_soc_pct"])] = sb.get(_band(x["start_soc_pct"]), 0) + 1
        eb[_band(x["end_soc_pct"])] = eb.get(_band(x["end_soc_pct"]), 0) + 1
    bands = [f"{i * 10}-{i * 10 + 10}" for i in range(10)]
    runs = []
    cur = None
    for x in sorted(rows, key=lambda z: (z["car"], z["start_ms"])):
        if cur and cur["car"] == x["car"] and x["start_ms"] - cur["_last_end"] < DAY:
            cur["charges"] += 1
            cur["min_start_soc_pct"] = min(cur["min_start_soc_pct"], x["start_soc_pct"])
            cur["max_end_soc_pct"] = max(cur["max_end_soc_pct"], x["end_soc_pct"])
            cur["_last_end"] = x["end_ms"] or req.now
        else:
            cur = {"car": x["car"], "charges": 1, "start_ms": x["start_ms"], "min_start_soc_pct": x["start_soc_pct"],
                   "max_end_soc_pct": x["end_soc_pct"], "_last_end": x["end_ms"] or req.now}
            runs.append(cur)
    runs = [z for z in runs if z["charges"] > 1]
    n = len(rows) or 1
    out = {
        "cars": car_ids, "count": len(rows), "energy_added_kwh": t["energy_added_kwh"], "by_kind": by_kind,
        "cost_by_currency": t["cost_by_currency"],
        "price_per_kwh": {"all": ppk(rows), "ac": ppk([x for x in rows if x["kind"] == "ac"]),
                          "dc": ppk([x for x in rows if x["kind"] == "dc"]), "basis": "grid for AC, billed kWh for DC",
                          "priced": sum(1 for x in rows if x["cost"])},
        "supercharger_cost": [{"currency": k, "amount": round(v, 2)} for k, v in sorted(sc.items())],
        "cost_per_100km": {"value": [{"currency": c["currency"], "amount": round(c["amount"] / dist * 100, 2)}
                                     for c in t["cost_by_currency"]] if dist > 1 else [],
                           "method": "cost of the charges in the period over the distance driven in it",
                           "distance_km": r(dist, 1)},
        "start_bands": [{"band": bd, "count": sb.get(bd, 0)} for bd in bands],
        "end_bands": [{"band": bd, "count": eb.get(bd, 0)} for bd in bands],
        "heatmap": {"start": heat_s, "end": heat_e, "rows": "weekday, Monday first", "columns": "hour, local time"},
        "runs": [{k: v for k, v in z.items() if not k.startswith("_")} for z in runs],
        "share_above_80_pct": r(100 * sum(1 for x in rows if x["end_soc_pct"] > 80) / n, 1),
        "share_below_20_pct": r(100 * sum(1 for x in rows if x["start_soc_pct"] < 20) / n, 1),
    }
    return out


def charging_places(req):
    car_ids, a, b, rows = _stats_rows(req)
    sort = req.enum("sort", ("energy", "cost", "sessions"), "energy")
    groups = {}
    for x in rows:
        pid = (x["place"] or {}).get("id")
        key = pid or f"addr:{x['address']['short']}"
        g = groups.setdefault(key, {"place": pid, "label": (x["place"] or {}).get("name") or x["address"]["short"],
                                    "lat": x["position"]["lat"], "lon": x["position"]["lon"], "sessions": 0,
                                    "energy_added_kwh": 0.0, "_cost": {}, "_basis_kwh": 0.0, "cars": set()})
        g["sessions"] += 1
        g["energy_added_kwh"] += x["energy_added_kwh"] or 0
        g["cars"].add(x["car"])
        if x["cost"]:
            g["_cost"][x["cost"]["currency"]] = g["_cost"].get(x["cost"]["currency"], 0.0) + x["cost"]["amount"]
            g["_basis_kwh"] += x["energy_grid_kwh"] or x["dc_inlet_kwh"] or x["energy_added_kwh"] or 0
    out = []
    for g in groups.values():
        cbc = [{"currency": k, "amount": round(v, 2)} for k, v in sorted(g["_cost"].items())]
        total = sum(v for v in g["_cost"].values())
        out.append({"place": g["place"], "label": g["label"], "lat": g["lat"], "lon": g["lon"],
                    "sessions": g["sessions"], "energy_added_kwh": r(g["energy_added_kwh"], 3),
                    "cost_by_currency": cbc,
                    "price_per_kwh": round(total / g["_basis_kwh"], 4) if g["_basis_kwh"] and len(cbc) == 1 else None,
                    "cars": sorted(g["cars"], key=lambda c: int(c[1:])), "_total": total})
    keyf = {"energy": lambda g: (g["energy_added_kwh"], g["label"]), "cost": lambda g: (g["_total"], g["label"]),
            "sessions": lambda g: (g["sessions"], g["label"])}[sort]
    items, nxt = page(req, out, key=keyf, reverse=True)
    return {"cars": car_ids, "items": items, "next": nxt}


def dc_curve(req):
    h = req.history()
    a, b = req.period()
    places = _place_ids(req)
    out = {"car": req.car, "time_zone": req.tz(), "median": [], "sessions": []}
    if h is None:
        return out
    by_soc = {}
    for c in _charges_of(req, h, a, b):
        if c.ckind != "dc" or (places and piece_pid(req.app, c) not in places):
            continue
        pts = []
        last = None
        end = min(req.now, c.data_end)
        t = c.start + 30_000
        while t <= end:
            v = c.curve(t)
            if v is not None:
                s = int(v["soc_pct"])
                if s != last:
                    pts.append({"soc_pct": s, "power_kw": v["power_kw"], "battery_c": v["battery_c"]})
                    by_soc.setdefault(s, []).append(v["power_kw"])
                    last = s
            t += 5000
        pl = place_obj(req.app, piece_pid(req.app, c))
        out["sessions"].append({"charge": c.id, "label": pl["name"] if pl else "Fast charger", "start_ms": c.start,
                                "points": pts})
    out["median"] = [{"soc_pct": s, "power_kw": r(median(v), 1)} for s, v in sorted(by_soc.items())]
    return out


def register(router):
    router.add("/api/v1/charges", GET=list_all)
    router.add("/api/v1/cars/{car}/charges", GET=list_car)
    router.add("/api/v1/cars/{car}/charges/{charge}", GET=get_charge)
    router.add("/api/v1/cars/{car}/charges/{charge}/curve", GET=get_curve)
    router.add("/api/v1/cars/{car}/charges/{charge}/cost", PUT=put_cost, DELETE=delete_cost, admin=("PUT", "DELETE"))
    router.add("/api/v1/charging-stats", GET=charging_stats)
    router.add("/api/v1/cars/{car}/charging-stats", GET=charging_stats)
    router.add("/api/v1/charging-stats/places", GET=charging_places)
    router.add("/api/v1/cars/{car}/charging-stats/dc-curve", GET=dc_curve)

