"""Drives (docs/ui-api.md, "Drives"): lists, detail, track (GeoJSON, GPX), purposes, stats, the
logbook, the trip report and saved trips.

The pattern every resource module follows:
- read filters with req.period(), req.enum(), req.list_param() ... (they answer 400 with fields);
- build rows with mockapi.rows (they handle "in progress" and the owner's overrides);
- page with conventions.page(req, rows, key=..., reverse=...) (CSV comes for free);
- changes: validate (400 shape, 422 rules), change the store under st.lock, then st.record(...).
"""

from __future__ import annotations

import math

from .. import sources as S
from ..conventions import Response, decode_cursor, encode_cursor, page
from ..errors import bad_input, invalid, not_found
from ..pdf import text_pdf
from ..placing import drive_pids
from .. import gapfill
from ..rows import charge_row, drive_row, in_progress, place_obj
from ..util import DAY, HOUR, iso_utc, local_dt, median, r

SORTS = {"start": lambda d: d["start_ms"], "distance": lambda d: d["distance_km"], "duration": lambda d: d["duration_ms"],
         "energy": lambda d: d["energy_kwh"], "wh_per_km": lambda d: _figure_whkm(d),
         "speed_max": lambda d: d["speed_max_kph"]}


def _figure_whkm(d):
    """The consumption a page shows for a drive, or None: a drive still under way, a very short one or one with a
    gap in its data has no figure, so it sorts with the other drives without one (last in either direction)."""
    if d["flags"].get("in_progress") or (d["distance_km"] or 0) < 1 or d["flags"].get("gap_ms", 0) > 60000:
        return None
    return d["wh_per_km"]


PURPOSES = ("business", "private", "commute")


def cached_drive_row(req, h, d):
    if in_progress(d, req.now) or d.start > req.now:
        return drive_row(req.app, h, d, req.now)
    cache = req.app.cache.setdefault("drive_rows", {})
    if cache.get("_rev") != req.store.rev:  # something changed: start the cache again
        cache.clear()
        cache["_rev"] = req.store.rev
    key = (d.id, req.store.rev)
    row = cache.get(key)
    if row is None:
        row = drive_row(req.app, h, d, req.now)
        cache[key] = row
    return dict(row)


def _drives(req, h, a, b, source="all"):
    return [d for d in h.drives if a <= d.start < b and d.start <= req.now and h.shown(d, source)]


def _place_ids(req, name):
    ids = req.list_param(name)
    if ids:
        unknown = [p for p in ids if place_obj(req.app, p) is None]
        if unknown:
            raise bad_input({name: "Unknown place: " + ", ".join(unknown[:5]) + "."})
    return ids


def _filtered(req, h):
    a, b = req.period()
    places = _place_ids(req, "places")
    side = req.enum("place_side", ("start", "end", "any"), "any")
    q = (req.q("q") or "").strip().lower()
    min_d = req.float_param("min_distance_km", None, 0, 100000)
    min_v = req.float_param("min_avg_kph", None, 0, 1000)
    source = S.canon(req.enum("source", S.FILTER_WORDS, "all"))
    flags = req.list_param("flags", ("gap", "in_progress", "reduced_range"))
    eff = req.enum("efficiency", ("plain", "hill_corrected"), "plain")
    rows = []
    for d in _drives(req, h, a, b, source):
        if places:
            a_pid, b_pid = drive_pids(req.app, d)
            s_ok = a_pid in places
            e_ok = b_pid in places and d.cut_ms is None
            if not (s_ok if side == "start" else e_ok if side == "end" else s_ok or e_ok):
                continue
        row = cached_drive_row(req, h, d)
        if source != "all":  # one source alone: nothing filled from an import
            row = gapfill.unfill(row)
        if min_d is not None and (row["distance_km"] or 0) < min_d:
            continue
        if min_v is not None and (row["speed_avg_kph"] or 0) < min_v:
            continue
        if flags:
            f = row["flags"]
            ok = (("gap" in flags and (f["gap_ms"] > 0 or f["lost_frames"] > 0))
                  or ("in_progress" in flags and f["in_progress"])
                  or ("reduced_range" in flags and f["reduced_range"]))
            if not ok:
                continue
        if q:
            text = " ".join(filter(None, [row["start"]["label"], row["start"]["address"]["short"], row["end"]["label"],
                                          row["end"]["address"]["short"],
                                          (row["purpose"] or {}).get("value"), (row["purpose"] or {}).get("note"),
                                          (row["purpose"] or {}).get("relation")])).lower()
            if q not in text:
                continue
        if eff == "hill_corrected":
            hill = row["_whkm_hill"]
            row["wh_per_km"] = r(hill, 1)
            # the efficiency score follows the same basis: rated consumption against the hill-corrected one
            rated = h.car.rated_wh_per_km
            row["efficiency_pct"] = r(rated / hill * 100, 1) if hill and hill > 0 else None
        rows.append(row)
    return rows


def summary(rows, eff="plain"):
    dist = sum(x["distance_km"] or 0 for x in rows)
    e_meas = sum(x["energy_kwh"] or 0 for x in rows if x["energy_basis"] == "measured")
    e_est = sum(x["energy_kwh"] or 0 for x in rows if x["energy_basis"] == "estimated")
    without = sum(1 for x in rows if x["energy_kwh"] is None)
    e = e_meas + e_est
    basis_e = e
    if eff == "hill_corrected":  # the climb taken off each drive, as in the rows
        basis_e = sum(((x["_whkm_hill"] * (x["distance_km"] or 0) / 1000) if x.get("_whkm_hill") is not None
                       else (x["energy_kwh"] or 0)) for x in rows)
    return {"count": len(rows), "distance_km": r(dist, 3), "duration_ms": sum(x["duration_ms"] for x in rows),
            "energy_kwh": r(e, 3), "energy_measured_kwh": r(e_meas, 3), "energy_estimated_kwh": r(e_est, 3),
            "wh_per_km": r(basis_e / dist * 1000, 1) if dist > 0.1 else None, "without_energy": without,
            "efficiency": eff}


def list_drives(req):
    h = req.history()
    sort = req.enum("sort", tuple(SORTS), "start")
    d = req.enum("dir", ("asc", "desc"), "desc")
    from ..conventions import nk
    rows = _filtered(req, h) if h else []
    rev = d == "desc"
    items, nxt = page(req, rows, key=lambda x: (nk(SORTS[sort](x), rev), x["start_ms"], x["id"]), reverse=rev)
    eff = req.enum("efficiency", ("plain", "hill_corrected"), "plain")
    return {"car": req.car, "time_zone": req.tz(), "items": items, "next": nxt, "summary": summary(rows, eff)}


def _drive(req):
    h = req.history()
    d = h.by_id.get(req.params["drive"]) if h else None
    if d is None or d.kind != "drive" or d.start > req.now:
        raise not_found("no_such_drive", "There is no such drive for this car.")
    return h, d


def get_drive(req):
    h, d = _drive(req)
    row = cached_drive_row(req, h, d)
    vis = [x for x in h.drives if x.start <= req.now and h.shown(x)]
    ids = [x.id for x in vis]
    i = ids.index(d.id) if d.id in ids else None
    row["prev"] = ids[i - 1] if i else None
    row["next"] = ids[i + 1] if i is not None and i + 1 < len(ids) else None
    # bands of `speed_band_kph` (default 10; 16.09344 gives 10 mph bands)
    w = req.float_param("speed_band_kph", 10.0, 1, 100)
    bands = d.speed_bands(req.now if in_progress(d, req.now) else None, width=w)
    row["speed_band_kph"] = w
    row["speed_bands"] = [{"from_kph": r(k * w, 3), "to_kph": r((k + 1) * w, 3), "seconds": round(v, 1)}
                          for k, v in sorted(bands.items())]
    # ideal range at the first and last sample with a value, as start_range_km and end_range_km are rated
    end = min(req.now, d.data_end)
    row["start_ideal_range_km"] = r(_first_value(d, "ideal_range_km", d.start, end, 1000), 1)
    row["end_ideal_range_km"] = r(_first_value(d, "ideal_range_km", end, d.start, -1000), 1)
    # the charges just before and just after the drive (ids), for links to them
    ch = [x for x in h.charges if x.start <= req.now and h.shown(x)]
    before = [x for x in ch if x.start < d.start]
    after = [x for x in ch if x.start >= d.start]
    row["prev_charge"] = before[-1].id if before else None
    row["next_charge"] = None if in_progress(d, req.now) or not after else after[0].id
    # the consumption Sentry Mate's range estimate uses: the 30 days of drives before this one
    from ..live import avg_wh_per_km
    row["est_wh_per_km"] = r(avg_wh_per_km(req.app, h, d.start), 1)
    row["est_basis"] = "wh_per_km_30d"
    gaps = [{"start_ms": a, "end_ms": b, "kind": "dropped"} for a, b in d.drops]
    if d.cut_ms is not None:
        gaps.append({"start_ms": d.cut_ms, "end_ms": None, "kind": "no_log"})
    row["gaps"] = gaps
    row["rated_wh_per_km"] = h.car.rated_wh_per_km
    return row


def _first_value(d, field, a, b, step):
    """The first non-null `field` of the drive from a towards b (step ms, negative to go back)."""
    t, n = a, 0
    while (t <= b if step > 0 else t >= b) and n < 600:
        v = d.values(t)
        if v is not None and v.get(field) is not None:
            return v[field]
        t += step
        n += 1
    return None


def track_filename(req, d):
    """`<car>-drive-<YYYY-MM-DD-HHMM>` with the start in the car's zone (no extension)."""
    import re
    car = req.store.car_meta(req.car)["name"]
    stem = re.sub(r"[^A-Za-z0-9._-]+", "-", car).strip("-") or "car"
    return f"{stem}-drive-{local_dt(d.start, req.tz()).strftime('%Y-%m-%d-%H%M')}"


# ------------------------------------------------------------------ track


def _rdp(pts, eps):
    """Douglas-Peucker on (x, y) metres; returns the kept indexes."""
    n = len(pts)
    if n < 3 or eps <= 0:
        return list(range(n))
    keep = [False] * n
    keep[0] = keep[-1] = True
    stack = [(0, n - 1)]
    while stack:
        i, j = stack.pop()
        (x1, y1), (x2, y2) = pts[i], pts[j]
        dx, dy = x2 - x1, y2 - y1
        L = math.hypot(dx, dy)
        best, bi = -1.0, None
        for k in range(i + 1, j):
            x0, y0 = pts[k]
            dist = abs(dy * x0 - dx * y0 + x2 * y1 - y2 * x1) / L if L > 0 else math.hypot(x0 - x1, y0 - y1)
            if dist > best:
                best, bi = dist, k
        if bi is not None and best > eps:
            keep[bi] = True
            stack.append((i, bi))
            stack.append((bi, j))
    return [k for k in range(n) if keep[k]]


TRACK_KEYS = ("lat", "lon", "elevation_m", "accuracy_m", "speed_kph", "power_kw", "soc_pct")


def track_parts(d, now, step_ms=None, twin=None):
    """[[{t_ms, lat, lon, ele, accuracy_m, speed_kph, power_kw, soc_pct}]] split where frames were dropped.
    `twin`: the drive's import copy, whose points fill where the drive's own frames were dropped (they carry
    `imported`, every field)."""
    end = min(now, d.data_end)
    step = step_ms or (1000 if d.source == "can" else 5000)
    parts, cur = [], []
    t = d.start
    while t <= end:
        v = d.values(t)
        if v is None and twin is not None:
            v = twin.values(t)
            if v is not None:
                cur.append({"t_ms": t, "lat": v["lat"], "lon": v["lon"], "ele": v["elevation_m"],
                            "accuracy_m": v["accuracy_m"], "speed_kph": v["speed_kph"], "power_kw": v["power_kw"],
                            "soc_pct": v["soc_pct"], "imported": list(TRACK_KEYS)})
                t += step
                continue
        if v is None:
            if cur:
                parts.append(cur)
                cur = []
        else:
            cur.append({"t_ms": t, "lat": v["lat"], "lon": v["lon"], "ele": v["elevation_m"],
                        "accuracy_m": v["accuracy_m"], "speed_kph": v["speed_kph"], "power_kw": v["power_kw"],
                        "soc_pct": v["soc_pct"]})
        t += step
    if cur:
        parts.append(cur)
    return parts


def get_track(req):
    h, d = _drive(req)
    fmt = req.enum("format", ("geojson", "gpx", "csv"), "geojson")
    simplify = req.float_param("simplify_m", 0.0, 0, 5000)
    ext = req.list_param("extensions", ("speed", "power", "soc", "accuracy")) or []
    # a live drive's dropped frames: its import's points there, with import gap filling on
    tw = gapfill.twin(h, d) if gapfill.on(req.store, h.car.id) else None
    parts = track_parts(d, req.now, twin=tw)
    value_sources = {}
    if tw is not None and any("imported" in q for p in parts for q in p):
        value_sources = {k: [S.ref("import", S.site_of(tw))] for k in TRACK_KEYS}
    if simplify:
        out = []
        for p in parts:
            lat0 = p[0]["lat"]
            k = 111_320 * math.cos(math.radians(lat0))
            xy = [((q["lon"]) * k, q["lat"] * 110_540) for q in p]
            out.append([p[i] for i in _rdp(xy, simplify)])
        parts = out
    name = track_filename(req, d)
    if fmt == "gpx":
        return Response(200, _gpx(req.store.car_meta(req.car)["name"], d, parts, ext), "application/gpx+xml; charset=utf-8",
                        {"Content-Disposition": f'attachment; filename="{name}.gpx"'})
    if req.csv or fmt == "csv":
        req.csv = True
        return {"items": [{k: v for k, v in dict(pt, part=i).items() if k != "imported"}
                          for i, p in enumerate(parts) for pt in p]}
    feats = []
    for i, p in enumerate(parts):
        props = {"drive": d.id, "car": h.car.id, "part": i, "t_ms": [q["t_ms"] for q in p]}
        if not ext or "accuracy" in ext:
            props["accuracy_m"] = [q["accuracy_m"] for q in p]
        if "speed" in ext:
            props["speed_kph"] = [q["speed_kph"] for q in p]
        if "power" in ext:
            props["power_kw"] = [q["power_kw"] for q in p]
        if "soc" in ext:
            props["soc_pct"] = [q["soc_pct"] for q in p]
        if any("imported" in q for q in p):
            props["imported"] = [q.get("imported", []) for q in p]
        feats.append({"type": "Feature", "properties": props,
                      "geometry": {"type": "LineString", "coordinates": [[q["lon"], q["lat"], q["ele"]] for q in p]}})
    return Response(200, _json(
        {"type": "FeatureCollection", "car": h.car.id, "drive": d.id, "time_zone": req.tz(), "features": feats,
         "value_sources": value_sources}),
        "application/geo+json; charset=utf-8", {"Content-Disposition": f'attachment; filename="{name}.geojson"'})


def _json(obj):
    from ..conventions import json_bytes
    return json_bytes(obj)


def _gpx(car_name, d, parts, ext):
    from xml.sax.saxutils import escape
    lines = ['<?xml version="1.0" encoding="UTF-8"?>',
             '<gpx version="1.1" creator="Sentry Mate" xmlns="http://www.topografix.com/GPX/1/1" '
             'xmlns:sm="https://sentry-mate.invalid/gpx/1">',
             f"<metadata><name>{escape(car_name)}: drive {d.id}</name><time>{iso_utc(d.start)}</time></metadata>",
             f"<trk><name>{escape(car_name)} {iso_utc(d.start)}</name>"]
    for p in parts:
        lines.append("<trkseg>")
        for q in p:
            e = []
            if "speed" in ext:
                e.append(f"<sm:speed_kph>{q['speed_kph']}</sm:speed_kph>")
            if "power" in ext:
                e.append(f"<sm:power_kw>{q['power_kw']}</sm:power_kw>")
            if "soc" in ext:
                e.append(f"<sm:soc_pct>{q['soc_pct']}</sm:soc_pct>")
            if "accuracy" in ext:
                e.append(f"<sm:accuracy_m>{q['accuracy_m']}</sm:accuracy_m>")
            xe = f"<extensions>{''.join(e)}</extensions>" if e else ""
            lines.append(f'<trkpt lat="{q["lat"]}" lon="{q["lon"]}"><ele>{q["ele"]}</ele>'
                         f"<time>{iso_utc(q['t_ms'])}</time>{xe}</trkpt>")
        lines.append("</trkseg>")
    lines.append("</trk></gpx>")
    return "\n".join(lines)


# ------------------------------------------------------------------ purposes


def put_purpose(req):
    h, d = _drive(req)
    body = req.json()
    bad, rule = {}, {}
    for k in body:
        if k not in ("purpose", "relation", "note"):
            bad[k] = "Unknown field."
    if "purpose" not in body:
        bad["purpose"] = "Required."
    for k, most in (("relation", 100), ("note", 500)):
        v = body.get(k)
        if v is not None and not isinstance(v, str):
            bad[k] = "Must be text or null."
        elif isinstance(v, str) and len(v) > most:
            rule[k] = f"At most {most} characters."
    if bad:
        raise bad_input(bad)
    if body["purpose"] not in PURPOSES:
        rule["purpose"] = "Must be business, private or commute."
    if rule:
        raise invalid(rule)
    st = req.store
    with st.lock:
        st.purposes[d.id] = {"purpose": body["purpose"], "relation": body.get("relation"), "note": body.get("note"),
                             "at_ms": req.now}
    st.record(req.account, "drive-purpose-set", f"drive:{req.car}:{d.id}",
              f"Set the purpose of a drive of {st.car_meta(req.car)['name']} to {body['purpose']}.", car=req.car)
    return get_drive(req)


def delete_purpose(req):
    h, d = _drive(req)
    st = req.store
    with st.lock:
        st.purposes.pop(d.id, None)
    st.record(req.account, "drive-purpose-removed", f"drive:{req.car}:{d.id}",
              f"Removed the purpose of a drive of {st.car_meta(req.car)['name']}.", car=req.car)
    return get_drive(req)


def list_rules(req):
    rows = req.store.rules.get(req.car, [])
    items, nxt = page(req, rows, key=lambda x: (x["id"],))
    return {"car": req.car, "time_zone": req.tz(), "items": items, "next": nxt}


def create_rule(req):
    body = req.json()
    bad, rule = {}, {}
    for k in ("from_place", "to_place", "days", "purpose"):
        if k not in body:
            bad[k] = "Required."
    for k in body:
        if k not in ("from_place", "to_place", "days", "purpose"):
            bad[k] = "Unknown field."
    for k in ("from_place", "to_place"):
        if k in body and body[k] is not None and not isinstance(body[k], str):
            bad[k] = "Must be a place id or null."
    for k in ("days", "purpose"):
        if k in body and not isinstance(body[k], str):
            bad[k] = "Must be text."
    if bad:
        raise bad_input(bad)
    for k in ("from_place", "to_place"):
        if body[k] is not None and place_obj(req.app, body[k]) is None:
            rule[k] = "No such place."
    if body["purpose"] not in PURPOSES:
        rule["purpose"] = "Must be business, private or commute."
    days = body["days"]
    words = ("mon", "tue", "wed", "thu", "fri", "sat", "sun")
    ok = isinstance(days, str) and (days == "all" or all(
        all(w in words for w in part.strip().split("-")) and 1 <= len(part.strip().split("-")) <= 2
        for part in days.split(",")))
    if not ok:
        rule["days"] = "Days such as mon-fri, sat,sun or all."
    if rule:
        raise invalid(rule)
    st = req.store
    row = {"id": st.new_id("r"), "from_place": body["from_place"], "to_place": body["to_place"], "days": days,
           "purpose": body["purpose"]}
    with st.lock:
        st.rules.setdefault(req.car, []).append(row)
    st.record(req.account, "purpose-rule-created", f"car:{req.car}", f"Added a purpose rule ({body['purpose']}).")
    return dict(row, _created=True)


def get_rule(req):
    for x in req.store.rules.get(req.car, []):
        if x["id"] == req.params["rule"]:
            return dict(x, car=req.car, time_zone=req.tz())
    raise not_found("no_such_rule", "There is no such purpose rule.")


def delete_rule(req):
    st = req.store
    rows = st.rules.get(req.car, [])
    rid = req.params["rule"]
    if not any(x["id"] == rid for x in rows):
        raise not_found("no_such_rule", "There is no such purpose rule.")
    with st.lock:
        st.rules[req.car] = [x for x in rows if x["id"] != rid]
    st.record(req.account, "purpose-rule-removed", f"car:{req.car}", "Removed a purpose rule.")
    return {"deleted": True, "id": rid}


# ------------------------------------------------------------------ drive stats


def drive_stats(req):
    h = req.history()
    a, b = req.period()
    excl = set(_place_ids(req, "exclude_places") or [])
    tz = req.tz()
    end = min(b, req.now)
    empty = {"car": req.car, "time_zone": tz, "from_ms": a, "to_ms": max(a, end), "count": 0, "distance_km": 0.0,
             "energy_kwh": 0.0,
             "median_distance_km": None, "per_day": [], "days": 0,
             "max_speed": {"period": None, "last_30d": None, "last_7d": None}, "speed_bands": [],
             "projection": {"per_month_km": None, "per_year_km": None, "basis_days": 0}, "top_destinations": []}
    if h is None:
        return empty

    # every local day from the later of the period start and the car's first data to the period end (or now),
    # days without driving as 0, so per_day is a whole daily series
    per = {}
    first = max(a, h.first_ms) if h.first_ms is not None else None
    if first is not None and first < end:
        day = local_dt(first, tz).date()
        last = local_dt(end - 1, tz).date()
        while day <= last:
            k = day.isoformat()
            per[k] = {"date": k, "count": 0, "distance_km": 0.0, "energy_kwh": 0.0}
            day = day.fromordinal(day.toordinal() + 1)

    # top speeds over the 30 and 7 days to the period end (or now), whatever the period and the places left out
    def fastest_since(since):
        best = None
        for d in _drives(req, h, since, end):
            x = cached_drive_row(req, h, d)
            if x["speed_max_kph"] is not None and (best is None or x["speed_max_kph"] > best["kph"]):
                best = {"kph": x["speed_max_kph"], "drive": d.id}
        return best

    windows = {"last_30d": fastest_since(end - 30 * DAY), "last_7d": fastest_since(end - 7 * DAY)}
    ds = [d for d in _drives(req, h, a, b) if not excl or not (set(drive_pids(req.app, d)) & excl)]
    if not ds:
        return dict(empty, per_day=[per[k] for k in sorted(per)], days=len(per),
                    max_speed={"period": None, **windows})
    rows = [(d, cached_drive_row(req, h, d)) for d in ds]
    for d, x in rows:
        day = local_dt(d.start, tz).date().isoformat()
        p = per.setdefault(day, {"date": day, "count": 0, "distance_km": 0.0, "energy_kwh": 0.0})
        p["count"] += 1
        p["distance_km"] += x["distance_km"] or 0
        p["energy_kwh"] += x["energy_kwh"] or 0
    for p in per.values():
        p["distance_km"] = r(p["distance_km"], 3)
        p["energy_kwh"] = r(p["energy_kwh"], 3)

    best = None
    for d, x in rows:
        if x["speed_max_kph"] is not None and (best is None or x["speed_max_kph"] > best["kph"]):
            best = {"kph": x["speed_max_kph"], "drive": d.id}

    bands = {}
    for d, _x in rows:
        for k, v in d.speed_bands(req.now if in_progress(d, req.now) else None).items():
            bands[k] = bands.get(k, 0.0) + v
    tot = sum(bands.values()) or 1.0
    dist = sum(x["distance_km"] or 0 for _, x in rows)
    # the same calendar days as per_day, so the projection and a per-day rate agree
    span_days = float(len(per)) if per else max(1.0, (end - max(a, h.first_ms)) / DAY)
    dest = {}
    for d, x in rows:
        # a drive still going, or whose data stopped, has no destination yet
        if x["end_ms"] is None or d.cut_ms is not None:
            continue
        pid = (x["end"]["place"] or {}).get("id")
        key = pid or x["end"]["label"]
        e = dest.setdefault(key, {"place": pid, "label": x["end"]["label"], "count": 0})
        e["count"] += 1
    top = sorted(dest.values(), key=lambda e: (-e["count"], e["label"]))[:10]
    return {
        "car": req.car, "time_zone": tz, "from_ms": a, "to_ms": max(a, end), "count": len(rows),
        "distance_km": r(dist, 3),
        "energy_kwh": r(sum(x["energy_kwh"] or 0 for _, x in rows), 3),
        "median_distance_km": r(median([x["distance_km"] for _, x in rows]), 3),
        "per_day": [per[k] for k in sorted(per)], "days": len(per),
        "max_speed": {"period": best, **windows},
        "speed_bands": [{"from_kph": k * 10, "to_kph": (k + 1) * 10, "seconds": round(v, 1),
                         "share_pct": r(100 * v / tot, 2)} for k, v in sorted(bands.items())],
        "projection": {"per_month_km": r(dist / span_days * 30.44, 1), "per_year_km": r(dist / span_days * 365.25, 0),
                       "basis_days": round(span_days, 1)},
        "top_destinations": top,
    }


# ------------------------------------------------------------------ logbook


GAP_KM = 0.05  # below 50 m the odometer and the logged start agree (rounding)
LOGBOOK_SORTS = ("seq", "distance", "gap", "odometer", "from", "to", "purpose")


def _logbook_book(req, h, a, b):
    """The period's rows, totals and continuity check, kept while the data (and the drives started) stay the same,
    so paging reads no drive again (as the real server keeps it). A period that ends now (a page sends its own now as
    to_ms) is kept as one that goes on, so every load shares one kept answer."""
    if b >= req.now - 5 * 60_000:
        b = 2 ** 62
    drives = _drives(req, h, a, b)
    live = any(in_progress(d, req.now) for d in drives[-2:])
    key = (req.car, a, b, req.store.rev, len(drives), drives[-1].id if drives else None,
           req.now // 5000 if live else None)
    cache = req.app.cache.setdefault("logbook", {})
    got = cache.get(key)
    if got is not None:
        return got
    rows = []
    totals = {k: {"drives": 0, "distance_km": 0.0} for k in PURPOSES + ("none",)}
    prev_end, prev_end_ms = None, None
    for i, d in enumerate(drives, 1):
        x = cached_drive_row(req, h, d)
        p = x["purpose"] or {}
        gap = None
        if prev_end is not None:
            gap = r(max(0.0, x["start_odometer_km"] - prev_end), 3)
        prev_end = x["end_odometer_km"]
        rows.append({
            "seq": i, "drive": d.id, "start_ms": x["start_ms"], "end_ms": x["end_ms"],
            "start_odometer_km": x["start_odometer_km"], "end_odometer_km": x["end_odometer_km"],
            "distance_km": x["distance_km"], "start_address": x["start"]["address"]["short"],
            "end_address": x["end"]["address"]["short"],
            "start_place": (x["start"]["place"] or {}).get("name"), "end_place": (x["end"]["place"] or {}).get("name"),
            "start_place_id": (x["start"]["place"] or {}).get("id"), "end_place_id": (x["end"]["place"] or {}).get("id"),
            "duration_ms": x["duration_ms"], "purpose": p.get("value"), "relation": p.get("relation"),
            "note": p.get("note"), "purpose_source": p.get("source"), "purpose_rule": p.get("rule"),
            "in_progress": x["end_ms"] is None, "continuity_gap_km": gap,
            "prev_end_ms": prev_end_ms if i > 1 else None,
        })
        prev_end_ms = x["end_ms"]
        t = totals[p.get("value") or "none"]
        t["drives"] += 1
        t["distance_km"] += x["distance_km"] or 0
    for t in totals.values():
        t["distance_km"] = r(t["distance_km"], 3)
    counts = {k: 0 for k in PURPOSES + ("none",)}
    used = {}
    for x in rows:
        counts[x["purpose"] or "none"] += 1
        if x["purpose_rule"]:
            used[x["purpose_rule"]] = used.get(x["purpose_rule"], 0) + 1
    gaps = [x for x in rows if (x["continuity_gap_km"] or 0) > GAP_KM]
    worst = sorted(gaps, key=lambda x: -x["continuity_gap_km"])[:4]
    first, last = (rows[0], rows[-1]) if rows else (None, None)
    continuity = {
        "rows": len(rows), "first_seq": first and first["seq"],
        "first_start_odometer_km": first and first["start_odometer_km"],
        "last_seq": last and last["seq"], "last_end_odometer_km": last and last["end_odometer_km"],
        "logged_km": r(sum(x["distance_km"] or 0 for x in rows), 3),
        "missing_km": r(sum(x["continuity_gap_km"] for x in gaps), 3), "gaps": len(gaps), "worst": worst,
    }
    if len(cache) > 32:
        cache.clear()
    got = {"rows": rows, "totals": totals, "counts": counts, "rules_used": used, "continuity": continuity}
    cache[key] = got
    return got


def _logbook_shown(req, rows):
    """The rows the purpose filter and the search words show, in the asked order (ties by seq; null last)."""
    purpose = req.enum("purpose", PURPOSES + ("none",))
    words = (req.q("q") or "").lower().split()
    gaps = req.bool_param("gaps", False)
    sort = req.enum("sort", LOGBOOK_SORTS, "seq")
    desc = req.enum("dir", ("asc", "desc"), "asc") == "desc"
    out = []
    for x in rows:
        if purpose and (x["purpose"] or "none") != purpose:
            continue
        if gaps and not (x["continuity_gap_km"] or 0) > GAP_KM:
            continue
        if words:
            text = "\x1f".join(v for v in (x["start_place"], x["end_place"], x["start_address"], x["end_address"],
                                            x["relation"], x["note"]) if v).lower()
            if not all(w in text for w in words):
                continue
        out.append(x)
    val = {
        "seq": lambda x: x["seq"], "distance": lambda x: x["distance_km"], "gap": lambda x: x["continuity_gap_km"],
        "odometer": lambda x: x["start_odometer_km"],
        "from": lambda x: (x["start_place"] or x["start_address"] or "").lower() or None,
        "to": lambda x: (x["end_place"] or x["end_address"] or "").lower() or None,
        "purpose": lambda x: x["purpose"],
    }[sort]
    out.sort(key=lambda x: x["seq"], reverse=desc)
    if sort != "seq":
        known = sorted((x for x in out if val(x) is not None), key=val, reverse=desc)  # stable: ties keep seq order
        out = known + [x for x in out if val(x) is None]
    return out, sort, desc, purpose is not None or bool(words) or gaps or sort != "seq" or desc


def logbook(req):
    h = req.history()
    a, b = req.period()
    fmt = req.enum("format", ("json", "csv", "pdf"), "json")
    if fmt == "csv":
        req.csv = True
    tz = req.tz()
    book = _logbook_book(req, h, a, b) if h is not None else {
        "rows": [], "totals": {k: {"drives": 0, "distance_km": 0.0} for k in PURPOSES + ("none",)},
        "counts": {k: 0 for k in PURPOSES + ("none",)}, "rules_used": {},
        "continuity": {"rows": 0, "first_seq": None, "first_start_odometer_km": None, "last_seq": None,
                       "last_end_odometer_km": None, "logged_km": 0.0, "missing_km": 0.0, "gaps": 0, "worst": []}}
    rows, totals = book["rows"], book["totals"]
    if fmt == "pdf":
        meta = req.store.car_meta(req.car)
        lines = [f"{'#':>4} {'Start':16} {'End':5} {'Odo start km':>13} {'Odo end km':>12} {'km':>8}  "
                 f"{'From':28} {'To':28} {'Purpose':9} Note"]
        for x in rows:
            s = local_dt(x["start_ms"], tz)
            e = local_dt(x["end_ms"], tz) if x["end_ms"] else None
            lines.append(f"{x['seq']:>4} {s:%Y-%m-%d %H:%M} {e.strftime('%H:%M') if e else '--:--':5} "
                         f"{x['start_odometer_km']:>13.1f} {x['end_odometer_km']:>12.1f} {x['distance_km']:>8.1f}  "
                         f"{(x['start_place'] or x['start_address'])[:28]:28} {(x['end_place'] or x['end_address'])[:28]:28} "
                         f"{(x['purpose'] or '-'):9} {(x['note'] or '')[:30]}")
            if x["continuity_gap_km"] and x["continuity_gap_km"] > GAP_KM:
                lines.append(f"     ! {x['continuity_gap_km']:.1f} km not logged before this drive")
        lines.append("")
        for k, t in totals.items():
            lines.append(f"{k:>10}: {t['drives']} drives, {t['distance_km']:.1f} km")
        title = f"Logbook: {meta['name']} ({meta['vin_prefix']}), times in {tz}"
        return Response(200, text_pdf(title, lines), "application/pdf",
                        {"Content-Disposition": f'attachment; filename="logbook-{req.car}.pdf"'})
    shown, sort, desc, filtered = _logbook_shown(req, rows)
    paged = any(req.q(k) is not None for k in ("limit", "after", "offset", "at_ms"))
    if req.csv or not (paged or filtered):
        return {"car": req.car, "time_zone": tz, "from_ms": a, "to_ms": min(b, req.now),
                "rows": shown if filtered else rows, "totals_by_purpose": totals, "truncated": False}
    # a page: by cursor (the offset it reaches), by offset, or from a date (seq order only)
    limit = req.limit()
    total = len(shown)
    after = req.q("after")
    at = None if after else req.int_param("at_ms", None, 0, 10 ** 15)
    if at is not None and sort != "seq":
        raise bad_input({"at_ms": "Only in the order of the drives (sort=seq)."})
    if after:
        k = decode_cursor(after, req.fingerprint())
        if not (isinstance(k, (list, tuple)) and len(k) == 2 and k[0] == "o" and isinstance(k[1], int)):
            raise bad_input({"after": "Not a cursor this server gave out."})
        offset = k[1]
    elif at is not None:
        offset = next((i for i, x in enumerate(shown) if (x["start_ms"] < at if desc else x["start_ms"] >= at)),
                      max(0, total - 1))
    else:
        offset = req.int_param("offset", 0, 0, 20000)
    offset = min(offset, total)
    items = shown[offset:offset + limit]
    nxt = encode_cursor(["o", offset + len(items)], req.fingerprint()) if offset + len(items) < total else None
    return {"car": req.car, "time_zone": tz, "from_ms": a, "to_ms": min(b, req.now), "items": items, "next": nxt,
            "offset": offset, "total": total, "count": len(rows), "counts_by_purpose": book["counts"],
            "totals_by_purpose": totals, "continuity": book["continuity"], "rules_used": book["rules_used"],
            "truncated": False}


# ------------------------------------------------------------------ trip report


def trip(req):
    h = req.history()
    a, b = req.period(7 * DAY)
    tz = req.tz()
    # truncated: the real server lists 5,000 drives, charges and markers at most; no made-up car has as many
    out = {"car": req.car, "time_zone": tz, "from_ms": a, "to_ms": min(b, req.now), "truncated": False}
    if h is None:
        out.update({"distance": {"odometer_km": None, "drives_km": 0.0},
                    "time_ms": dict.fromkeys(("driving", "charging_ac", "charging_dc", "charging_unknown",
                                              "parked_awake", "asleep", "not_recorded"), 0),
                    "speed_kph": {"moving": None, "incl_dc": None},
                    "energy_kwh": {"drive_net": 0.0, "gross": 0.0, "added_ac": 0.0, "added_dc": 0.0,
                                   "added_unknown": 0.0},
                    "wh_per_km": {"net": None, "gross": None},
                    "cost": {"by_currency": [], "per_100km": [], "priced": 0, "unpriced": 0}, "markers": [],
                    "value_sources": {}})
        return out
    end = min(b, req.now)
    tm = dict.fromkeys(("driving", "charging_ac", "charging_dc", "charging_unknown", "parked_awake", "asleep",
                        "not_recorded"), 0)
    parked_kwh = 0.0
    odo_first = odo_last = None
    markers = []
    for s in h.segs_between(a, end, req.now):
        x, y = max(s.start, a), min(s.end, end)
        if s.kind == "drive":
            de = min(s.data_end, end)
            tm["driving"] += max(0, de - x)
            tm["not_recorded"] += max(0, y - de)
        elif s.kind == "charge":
            de = min(s.data_end, end)
            part = {"dc": "charging_dc", "ac": "charging_ac"}.get(s.ckind, "charging_unknown")
            tm[part] += max(0, de - x)
        elif s.kind == "parked":
            sp = s.clip(x, y).split_ms()
            tm["asleep"] += sp["asleep"]
            tm["parked_awake"] += sp["awake"] + sp["awake_vehicle_bus_only"]
            parked_kwh += s.kwh_at(y) - s.kwh_at(x)
            if s.end - s.start > 6 * HOUR and local_dt(s.start, tz).date() != local_dt(s.end, tz).date() \
                    and s.pid != h.car.home:
                markers.append({"kind": "overnight", "lat": r(s.lat, 6), "lon": r(s.lon, 6), "t_ms": s.start})
        elif s.kind == "gap":
            tm["not_recorded"] += y - x
        else:
            tm["parked_awake"] += y - x
    drives = [d for d in _drives(req, h, a, end)]
    drows = [cached_drive_row(req, h, d) for d in drives]
    charges = [c for c in h.charges if a <= c.start < end and c.start <= req.now and h.shown(c)]
    crows = [charge_row(req.app, h, c, req.now) for c in charges]
    vs = {}
    if drows:
        # the first and last odometer readings of the drives that have one, as the real server
        with_odo = [x for x in drows if x["start_odometer_km"] is not None]
        if with_odo:
            odo_first, odo_last = with_odo[0]["start_odometer_km"], with_odo[-1]["end_odometer_km"]
            got = with_odo[0]["value_sources"].get("start_odometer_km", []) + \
                with_odo[-1]["value_sources"].get("end_odometer_km", [])
            if got:
                vs["odometer_km"] = [g for i, g in enumerate(got) if g not in got[:i]]
        if drows[0]["value_sources"].get("start"):
            vs["markers"] = list(drows[0]["value_sources"]["start"])
        markers.insert(0, {"kind": "start", "lat": drows[0]["start"]["lat"], "lon": drows[0]["start"]["lon"],
                           "t_ms": drows[0]["start_ms"]})
        markers.append({"kind": "end", "lat": drows[-1]["end"]["lat"], "lon": drows[-1]["end"]["lon"],
                        "t_ms": drows[-1]["end_ms"] or req.now})
    for c in crows:
        markers.append({"kind": "charge", "lat": c["position"]["lat"], "lon": c["position"]["lon"],
                        "t_ms": c["start_ms"], "charge": c["id"]})
    markers.sort(key=lambda m: (m["t_ms"], m["kind"] != "start"))
    dist = sum(x["distance_km"] or 0 for x in drows)
    net = sum(x["energy_kwh"] or 0 for x in drows)
    # Wh/km over the drives with energy, as the drives list; the speed over the drives' own time
    dist_e = sum(x["distance_km"] or 0 for x in drows if x["energy_kwh"] is not None)
    gross = net + parked_kwh
    by_cur = {}
    priced = unpriced = 0
    for c in crows:
        if c["cost"]:
            priced += 1
            by_cur[c["cost"]["currency"]] = by_cur.get(c["cost"]["currency"], 0.0) + c["cost"]["amount"]
        else:
            unpriced += 1
    odo_km = r(odo_last - odo_first, 3) if odo_first is not None else None
    drive_h = sum(x["duration_ms"] or 0 for x in drows) / HOUR
    out.update({
        "distance": {"odometer_km": odo_km, "drives_km": r(dist, 3)},
        "time_ms": tm,
        "speed_kph": {"moving": r(dist / drive_h, 1) if drive_h > 0 else None,
                      "incl_dc": r(dist / (drive_h + tm["charging_dc"] / HOUR), 1) if drive_h > 0 else None},
        "energy_kwh": {"drive_net": r(net, 3), "gross": r(gross, 3),
                       "added_ac": r(sum(c["energy_added_kwh"] or 0 for c in crows if c["kind"] == "ac"), 3),
                       "added_dc": r(sum(c["energy_added_kwh"] or 0 for c in crows if c["kind"] == "dc"), 3),
                       "added_unknown": r(sum(c["energy_added_kwh"] or 0 for c in crows
                                              if c["kind"] not in ("ac", "dc")), 3)},
        "wh_per_km": {"net": r(net / dist_e * 1000, 1) if dist_e > 0.1 else None,
                      "gross": r(gross / dist_e * 1000, 1) if dist_e > 0.1 else None},
        "cost": {"by_currency": [{"currency": k, "amount": round(v, 2)} for k, v in sorted(by_cur.items())],
                 "per_100km": [{"currency": k, "amount": round(v / dist * 100, 2)} for k, v in sorted(by_cur.items())]
                 if dist > 1 else [], "priced": priced, "unpriced": unpriced},
        "markers": markers,
        "drives": [x["id"] for x in drows], "charges": [c["id"] for c in crows],
        "value_sources": vs,
    })
    return out


# ------------------------------------------------------------------ saved trips (per account)


def _trip_view(t):
    return {k: t[k] for k in ("id", "car", "from_ms", "to_ms", "name")}


def list_trips(req):
    st = req.store
    car = req.q("car")
    rows = [t for t in st.trips.values() if t["account"] == req.account["id"] and st.sees(req.account, t["car"])
            and (car is None or t["car"] == car)]
    items, nxt = page(req, [_trip_view(t) for t in rows], key=lambda t: (t["from_ms"], t["id"]), reverse=True)
    return {"items": items, "next": nxt}


def _trip_body(req, body, partial):
    bad, rule = {}, {}
    for k in body:
        if k not in ("car", "from_ms", "to_ms", "name") or (partial and k == "car"):
            bad[k] = "Unknown field." if k != "car" else "A saved trip cannot move to another car."
    for k in ("from_ms", "to_ms"):
        if k in body and (not isinstance(body[k], int) or isinstance(body[k], bool)):
            bad[k] = "Must be epoch milliseconds."
    if "name" in body and not isinstance(body["name"], str):
        bad["name"] = "Must be text."
    if "car" in body and not partial and not isinstance(body["car"], str):
        bad["car"] = "Must be a car id."
    if not partial:
        for k in ("car", "from_ms", "to_ms", "name"):
            if k not in body:
                bad[k] = "Required."
    if bad:
        raise bad_input(bad)
    if "name" in body and not 1 <= len(body["name"].strip()) <= 80:
        rule["name"] = "1 to 80 characters."
    return rule


def create_trip(req):
    st = req.store
    body = req.json()
    rule = _trip_body(req, body, False)
    if not req.ds.exists(body["car"]) or not st.sees(req.account, body["car"]):
        rule["car"] = "No such car."
    if body["from_ms"] >= body["to_ms"]:
        rule["to_ms"] = "Must be after from_ms."
    if rule:
        raise invalid(rule)
    t = {"id": st.new_id("tr"), "car": body["car"], "account": req.account["id"], "from_ms": body["from_ms"],
         "to_ms": body["to_ms"], "name": body["name"].strip()}
    with st.lock:
        st.trips[t["id"]] = t
    st.record(req.account, "trip-saved", f"trip:{t['id']}", f"Saved the trip {t['name']}.", car=t["car"])
    return dict(_trip_view(t), _created=True)


def _trip(req):
    t = req.store.trips.get(req.params["trip"])
    if t is None or t["account"] != req.account["id"] or not req.store.sees(req.account, t["car"]):
        raise not_found("no_such_trip", "There is no such saved trip.")
    return t


def get_trip(req):
    return _trip_view(_trip(req))


def patch_trip(req):
    st = req.store
    t = _trip(req)
    body = req.json()
    rule = _trip_body(req, body, True)
    a = body.get("from_ms", t["from_ms"])
    b = body.get("to_ms", t["to_ms"])
    if a >= b:
        rule["to_ms"] = "Must be after from_ms."
    if rule:
        raise invalid(rule)
    with st.lock:
        for k in ("from_ms", "to_ms"):
            if k in body:
                t[k] = body[k]
        if "name" in body:
            t["name"] = body["name"].strip()
    st.record(req.account, "trip-changed", f"trip:{t['id']}", f"Changed the trip {t['name']}.", car=t["car"])
    return _trip_view(t)


def delete_trip(req):
    st = req.store
    t = _trip(req)
    with st.lock:
        st.trips.pop(t["id"], None)
    st.record(req.account, "trip-removed", f"trip:{t['id']}", f"Removed the trip {t['name']}.", car=t["car"])
    return {"deleted": True, "id": t["id"]}


def register(router):
    router.add("/api/v1/cars/{car}/drives", GET=list_drives)
    router.add("/api/v1/cars/{car}/drives/{drive}", GET=get_drive)
    router.add("/api/v1/cars/{car}/drives/{drive}/track", GET=get_track)
    router.add("/api/v1/cars/{car}/drives/{drive}/purpose", PUT=put_purpose, DELETE=delete_purpose,
               admin=("PUT", "DELETE"))
    router.add("/api/v1/cars/{car}/purpose-rules", GET=list_rules, POST=create_rule, admin=("POST",))
    router.add("/api/v1/cars/{car}/purpose-rules/{rule}", GET=get_rule, DELETE=delete_rule, admin=("DELETE",))
    router.add("/api/v1/cars/{car}/drive-stats", GET=drive_stats)
    router.add("/api/v1/cars/{car}/logbook", GET=logbook)
    router.add("/api/v1/cars/{car}/trip", GET=trip)
    router.add("/api/v1/trips", GET=list_trips, POST=create_trip)
    router.add("/api/v1/trips/{trip}", GET=get_trip, PATCH=patch_trip, DELETE=delete_trip)

