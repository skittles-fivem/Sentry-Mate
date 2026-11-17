"""Reports (docs/ui-api.md, "Reports"), part two: where the cars have been. Stops (visits), their
summary, stops grouped by place, city, region and country, the days in a city, and the tracks.

A visit is one stop: from arriving to leaving (see derive.visits). Places are matched as they are
now, so renaming a place renames it here too. Days are counted in each car's own time zone.
"""

from __future__ import annotations

import math

from .. import derive
from ..conventions import page
from ..errors import bad_input
from ..placing import place_obj, ref
from ..util import r
from .charges import cached_charge_row
from .drives import cached_drive_row

SORTS = ("arrived_desc", "arrived_asc", "dwell_desc")
GROUP_SORTS = ("visits", "dwell", "last", "name")


def _filters(req):
    a, b = req.period()
    return {
        "a": a, "b": b,
        "q": (req.q("q") or "").strip().lower(),
        "place": req.q("place"),
        "country": (req.q("country") or "").strip().upper() or None,
        "region": (req.q("region") or "").strip().lower() or None,
        "city": req.q("city"),
    }


def _text(v):
    a = v["address"]
    pl = v["_place"]
    return " ".join(filter(None, [pl["name"] if pl else None, a.get("short"), a.get("street"), a.get("neighbourhood"),
                                  a.get("locality"), a.get("region"), a.get("country")])).lower()


def matching(req, f=None):
    """(car ids, visits that pass the filters, each with its place resolved)."""
    f = f or _filters(req)
    car_ids = req.cars()
    out = []
    for cid in car_ids:
        h = req.ds.history(cid)
        if h is None:
            continue
        for v in derive.visits(req.app, h, req.now):
            if not f["a"] <= v["arrived_ms"] < f["b"]:
                continue
            a = v["address"]
            if f["place"] and v["pid"] != f["place"]:
                continue
            if f["country"] and (a.get("country_code") or "").upper() != f["country"]:
                continue
            if f["region"] and (a.get("region") or "").lower() != f["region"]:
                continue
            if f["city"] and v["city_key"] != f["city"]:
                continue
            v = dict(v)
            v["_place"] = place_obj(req.app, v["pid"])
            if f["q"] and f["q"] not in _text(v):
                continue
            out.append(v)
    return car_ids, out


def visit_view(v):
    a = v["address"]
    pl = v["_place"]
    return {"id": v["id"], "car": v["car"], "time_zone": v["time_zone"], "arrived_ms": v["arrived_ms"],
            "left_ms": v["left_ms"], "lat": v["lat"], "lon": v["lon"], "accuracy_m": v["accuracy_m"],
            "place": ref(pl), "label": pl["name"] if pl else a.get("short"),
            "address": {k: a.get(k) for k in ("short", "street", "neighbourhood", "locality", "region", "country_code",
                                              "country", "source")},
            "city_key": v["city_key"], "drive_in": v["drive_in"], "drive_out": v["drive_out"],
            "charges": list(v["charges"]), "unknown_ms": v["unknown_ms"], "source": v["source"]}


def _dwell(v, f, now):
    x = max(v["arrived_ms"], f["a"])
    y = min(v["left_ms"] if v["left_ms"] is not None else now, f["b"], now)
    return max(0, y - x)


def list_visits(req):
    f = _filters(req)
    sort = req.enum("sort", SORTS, "arrived_desc")
    car_ids, vs = matching(req, f)
    rows = [visit_view(v) for v in vs]
    if sort == "dwell_desc":
        for row, v in zip(rows, vs):
            row["_dwell"] = _dwell(v, f, req.now)
        items, nxt = page(req, rows, key=lambda x: (x["_dwell"], x["arrived_ms"], x["id"]), reverse=True)
        for it in items:
            it.pop("_dwell", None)
    else:
        items, nxt = page(req, rows, key=lambda x: (x["arrived_ms"], x["id"]), reverse=sort == "arrived_desc")
    return {"cars": car_ids, "items": items, "next": nxt}


def summary(req):
    f = _filters(req)
    car_ids, vs = matching(req, f)
    places, cities, regions, countries, days = set(), set(), set(), set(), set()
    unnamed = 0
    for v in vs:
        a = v["address"]
        places.add(v["pid"] or _pos_key(v))
        cities.add(v["city_key"])
        regions.add((a.get("country_code"), a.get("region")))
        countries.add(a.get("country_code"))
        unnamed += v["pid"] is None
        for d in derive.visit_days(v, req.now):
            days.add((v["car"], d))
    # driving and charging in the same period, for the same cars (as the lists give them)
    dist = 0.0
    added = grid_e = added_g = 0.0
    counted = 0
    cost, unpriced = {}, 0
    for cid in car_ids:
        h = req.ds.history(cid)
        if h is None:
            continue
        for d in derive.shown_drives(h, f["a"], f["b"], req.now):
            dist += cached_drive_row(req, h, d)["distance_km"] or 0
        for c in derive.shown_charges(h, f["a"], f["b"], req.now):
            x = cached_charge_row(req, h, c)
            added += x["energy_added_kwh"] or 0
            if x["energy_grid_kwh"] is not None:
                grid_e += x["energy_grid_kwh"]
                added_g += x["energy_added_kwh"] or 0
                counted += 1
            if x["cost"]:
                cost[x["cost"]["currency"]] = cost.get(x["cost"]["currency"], 0.0) + x["cost"]["amount"]
            else:
                unpriced += 1
    return {
        "cars": car_ids, "visits": len(vs), "places": len(places), "cities": len(cities), "regions": len(regions),
        "countries": len(countries), "days": len(days),
        "first_ms": min((v["arrived_ms"] for v in vs), default=None),
        "last_ms": max(((v["left_ms"] or req.now) for v in vs), default=None),
        "unnamed_stops": unnamed, "distance_km": r(dist, 3), "energy_added_kwh": r(added, 3),
        "energy_grid_kwh": r(grid_e, 3),
        "efficiency": {"value": r(100 * added_g / grid_e, 1) if grid_e else None, "charges_counted": counted},
        "cost_by_currency": [{"currency": k, "amount": round(v, 2)} for k, v in sorted(cost.items())],
        "charges_without_cost": unpriced,
    }


def _pos_key(v):
    k = 1 / 0.0014
    return f"pos:{round(v['lat'] * k)}:{round(v['lon'] * k * math.cos(math.radians(v['lat'])))}"


def _group_key(level, v):
    a = v["address"]
    if level == "places":
        pl = v["_place"]
        return v["pid"] if pl else _pos_key(v)
    if level == "cities":
        return v["city_key"]
    if level == "regions":
        return f"{a.get('country_code') or '-'}/{a.get('region') or '-'}"
    return a.get("country_code") or "-"


def grouped(req, level):
    f = _filters(req)
    sort = req.enum("sort", GROUP_SORTS, "visits")
    car_ids, vs = matching(req, f)
    groups = {}
    for v in vs:
        k = _group_key(level, v)
        a = v["address"]
        g = groups.get(k)
        if g is None:
            pl = v["_place"]
            if level == "places":
                lab = pl["name"] if pl else a.get("short")
            elif level == "cities":
                lab = a.get("locality") or a.get("short")
            elif level == "regions":
                lab = a.get("region") or a.get("short")
            else:
                lab = a.get("country") or a.get("country_code")
            g = groups[k] = {"key": k, "label": lab, "_lat": 0.0, "_lon": 0.0, "visits": 0, "_places": set(),
                             "_days": set(), "dwell_ms": 0, "first_ms": None, "last_ms": None, "_cars": set(),
                             "locality": a.get("locality") if level in ("places", "cities") else None,
                             "region": a.get("region") if level != "countries" else None,
                             "country_code": a.get("country_code"), "country": a.get("country")}
            if level == "places":
                g["place"] = ref(pl)
                g["neighbourhood"] = a.get("neighbourhood")
                g["street"] = a.get("street")
        g["visits"] += 1
        g["_lat"] += v["lat"]
        g["_lon"] += v["lon"]
        g["_places"].add(v["pid"] or _pos_key(v))
        for d in derive.visit_days(v, req.now):
            g["_days"].add((v["car"], d))
        g["dwell_ms"] += _dwell(v, f, req.now)
        left = v["left_ms"] if v["left_ms"] is not None else req.now
        g["first_ms"] = v["arrived_ms"] if g["first_ms"] is None else min(g["first_ms"], v["arrived_ms"])
        g["last_ms"] = left if g["last_ms"] is None else max(g["last_ms"], left)
        g["_cars"].add(v["car"])
    rows = []
    for g in groups.values():
        pl = g.get("place")
        if pl:
            p = place_obj(req.app, pl["id"])
            lat, lon = p["lat"], p["lon"]
        else:
            lat, lon = g["_lat"] / g["visits"], g["_lon"] / g["visits"]
        row = {k: v for k, v in g.items() if not k.startswith("_")}
        row.update({"lat": round(lat, 6), "lon": round(lon, 6), "places": len(g["_places"]), "days": len(g["_days"]),
                    "cars": sorted(g["_cars"], key=lambda c: int(c[1:]))})
        rows.append(row)
    keyf = {"visits": lambda x: (x["visits"], x["dwell_ms"], x["key"]),
            "dwell": lambda x: (x["dwell_ms"], x["visits"], x["key"]),
            "last": lambda x: (x["last_ms"], x["key"]),
            "name": lambda x: ((x["label"] or "").lower(), x["key"])}[sort]
    items, nxt = page(req, rows, key=keyf, reverse=sort != "name")
    return {"cars": car_ids, "level": {"places": "place", "cities": "city", "regions": "region", "countries": "country"}[level],
            "items": items, "next": nxt}


def visit_places(req):
    return grouped(req, "places")


def visit_cities(req):
    return grouped(req, "cities")


def visit_regions(req):
    return grouped(req, "regions")


def visit_countries(req):
    return grouped(req, "countries")


def days(req):
    f = _filters(req)
    car_ids, vs = matching(req, f)
    per = {}
    for v in vs:
        tz = v["time_zone"]
        for d in derive.visit_days(v, req.now):
            a, b = derive.day_bounds(d, tz)
            x = max(a, v["arrived_ms"])
            y = min(b, v["left_ms"] if v["left_ms"] is not None else req.now)
            if y <= x:
                continue
            k = (d.isoformat(), v["car"])
            e = per.setdefault(k, {"date": d.isoformat(), "car": v["car"], "time_zone": tz, "first_ms": x,
                                   "last_ms": y, "drives": [], "charges": [], "places": []})
            e["first_ms"] = min(e["first_ms"], x)
            e["last_ms"] = max(e["last_ms"], y)
            for did in (v["drive_in"], v["drive_out"]):
                if did and did not in e["drives"]:
                    h = req.ds.history(v["car"])
                    dd = h.by_id.get(did)
                    if dd is not None and (a <= dd.start < b or a <= dd.data_end < b):
                        e["drives"].append(did)
            for cid in v["charges"]:
                h = req.ds.history(v["car"])
                cc = h.by_id.get(cid)
                if cc is not None and a <= cc.start < b and cid not in e["charges"]:
                    e["charges"].append(cid)
            lab = v["_place"]["name"] if v["_place"] else v["address"].get("short")
            if lab not in e["places"]:
                e["places"].append(lab)
    rows = list(per.values())
    items, nxt = page(req, rows, key=lambda x: (x["date"], -int(x["car"][1:])), reverse=True)
    return {"cars": car_ids, "city": f["city"], "items": items, "next": nxt}


# ------------------------------------------------------------------ tracks


def tracks(req):
    a, b = req.period()
    max_points = req.int_param("max_points", 10000, 2, 200000)
    car_ids = req.cars()
    end = min(b, req.now)
    drives = []
    for cid in car_ids:
        h = req.ds.history(cid)
        if h is None:
            continue
        for d in derive.shown_drives(h, a, end, req.now):
            drives.append((h, d))
    if len(drives) > max_points // 2:
        raise bad_input({"max_points": f"{len(drives)} drives need at least {len(drives) * 2} points; "
                                       "ask for more points or a shorter period."})
    total_ms = sum(max(1000, min(req.now, d.data_end) - d.start) for _, d in drives) or 1
    out = []
    used = 0
    for h, d in drives:
        de = min(req.now, d.data_end)
        dur = max(1000, de - d.start)
        n = max(2, min(int(dur / 1000) + 1, int(max_points * dur / total_ms)))
        step = max(1000, dur // (n - 1))
        t_ms, lat, lon, acc = [], [], [], []
        t = d.start
        while t <= de:
            v = d.values(t)
            if v is not None:
                t_ms.append(t)
                lat.append(v["lat"])
                lon.append(v["lon"])
                acc.append(v["accuracy_m"])
            if t == de:
                break
            t = min(de, t + step)
        used += len(t_ms)
        out.append({"car": h.car.id, "drive": d.id, "source": d.source, "t_ms": t_ms, "lat": lat, "lon": lon,
                    "accuracy_m": acc})
    return {"cars": car_ids, "from_ms": a, "to_ms": end, "points": used, "drives": out}


def register(router):
    router.add("/api/v1/visits", GET=list_visits)
    router.add("/api/v1/visits/summary", GET=summary)
    router.add("/api/v1/visits/places", GET=visit_places)
    router.add("/api/v1/visits/cities", GET=visit_cities)
    router.add("/api/v1/visits/regions", GET=visit_regions)
    router.add("/api/v1/visits/countries", GET=visit_countries)
    router.add("/api/v1/visits/days", GET=days)
    router.add("/api/v1/tracks", GET=tracks)
