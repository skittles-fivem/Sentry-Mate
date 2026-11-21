"""Reports (docs/ui-api.md, "Reports"), part three: software updates, the versions across the
fleet, and a car's events as MQTT publishes them.

A version is only ever what someone said: imported from TeslaMate, typed by the owner, or learned
from another update with the same module build ids ("build_match"). An update seen only as
changed build ids has `version: null`.
"""
from __future__ import annotations

import bisect

import re

from .. import derive
from ..conventions import bad_cursor, decode_cursor, encode_cursor, page
from ..errors import bad_input, invalid, not_found
from ..live import software_at, update_version
from ..placing import drive_pids, piece_pid, place_obj
from ..util import iso_utc, median, r
from .battery import bms_full
from .charges import cached_charge_row
from .drives import cached_drive_row
from .. import sources as S

VERSION_RX = re.compile(r"\d{4}\.\d{1,2}(\.\d{1,3}){0,2}")


def _visible_updates(h, now):
    return [u for u in h.updates if u.start <= now]


def update_rows(req, h):
    car = h.car
    ups = _visible_updates(h, req.now)
    rows = []
    prev_end = None
    for i, u in enumerate(ups):
        live = u.start <= req.now < u.end
        nxt_start = ups[i + 1].start if i + 1 < len(ups) else req.now
        v, vs = update_version(req.app, u)
        on = [c for c in h.charges if u.end <= c.start < nxt_start and c.start <= req.now and h.shown(c)]
        mid = (u.end + nxt_start) // 2
        rows.append({
            "id": u.id, "car": car.id, "start_ms": u.start, "end_ms": None if live else u.end,
            "duration_ms": (req.now if live else u.end) - u.start,
            "since_previous_ms": u.start - prev_end if prev_end is not None else None,
            "version": v, "version_source": vs if v else None,
            "detected_by": u.detected_by, "changed_modules": list(u.changed_modules) if u.source == "can" else [],
            "build_id": u.build_id if u.source == "can" else None,
            "charges": len(on), "full_range_km": r(car.full_range_km(mid), 1),
            "full_pack_kwh": r(bms_full(car, mid), 2) if u.source == "can" else None,
            "until_ms": ups[i + 1].start if i + 1 < len(ups) else None,
            **S.of_seg(u),
        })
        prev_end = u.end
    return rows


def list_updates(req):
    a, b = req.period()
    h = req.history()
    tz = req.tz()
    if h is None:
        return {"car": req.car, "time_zone": tz, "items": [], "next": None,
                "summary": {"count": 0, "median_gap_ms": None, "current_version": None,
                            "current_version_source": None, "current_version_since_ms": None,
                            "build_changed_since": None}}
    rows = update_rows(req, h)
    sel = [x for x in rows if a <= x["start_ms"] < b]
    items, nxt = page(req, sel, key=lambda x: (x["start_ms"],), reverse=True)
    sw = software_at(req.app, h, req.now)
    gaps = [x["since_previous_ms"] for x in sel if x["since_previous_ms"] is not None]
    return {"car": req.car, "time_zone": tz, "items": items, "next": nxt,
            "summary": {"count": len(sel), "median_gap_ms": int(median(gaps)) if gaps else None,
                        "current_version": sw["version"], "current_version_source": sw["version_source"],
                        "current_version_since_ms": sw["version_since_ms"],
                        "build_changed_since": sw["changed_since_ms"]}}


def patch_update(req):
    h = req.history()
    u = h.by_id.get(req.params["update"]) if h else None
    if u is None or u.kind != "update" or u.start > req.now:
        raise not_found("no_such_update", "There is no such software update for this car.")
    body = req.json()
    bad = {k: "Unknown field." for k in body if k != "version"}
    if "version" not in body:
        bad["version"] = "Required (a version such as 2026.38.4, or null to forget it)."
    elif body["version"] is not None and not isinstance(body["version"], str):
        bad["version"] = "Must be text or null."
    if bad:
        raise bad_input(bad)
    v = body["version"].strip() if isinstance(body["version"], str) else None
    if v is not None and not VERSION_RX.fullmatch(v):
        raise invalid({"version": "A version such as 2026.38.4."})
    st = req.store
    with st.lock:
        if v is None:
            st.update_versions.pop(u.id, None)
            if st.builds.get(u.build_id, {}).get("from_update") == u.id:
                st.builds.pop(u.build_id, None)
        else:
            st.update_versions[u.id] = v
            if u.source == "can":  # remember this set of build ids: other cars on the same build get the version
                st.builds[u.build_id] = {"version": v, "from_update": u.id}
    st.record(req.account, "update.version", f"update:{u.id}",
              f"Set the software version of an update to {v}." if v else "Forgot the version of an update.", car=req.car)
    req.app.events.publish("rows", {"car": req.car, "kinds": ["updates"], "from_ms": u.start, "to_ms": u.end},
                           car=req.car)
    return next(x for x in update_rows(req, h) if x["id"] == u.id) | {"time_zone": req.tz()}


# ------------------------------------------------------------------ fleet versions

SYN_VERSIONS = [("2026.32.5", "build_match"), ("2026.38.4", "owner"), (None, None), ("2026.26.7", "build_match"),
                ("2025.44.25", "owner")]


def _syn_version(syn, i):
    from ..util import u01
    k = int(u01(i, syn.salt + 21) * len(SYN_VERSIONS))
    v, src = SYN_VERSIONS[k]
    last = syn.now_ms - int(u01(i, syn.salt + 22) * 120 * 86_400_000) - 3 * 86_400_000
    return {"car": f"c{i}", "name": syn.name(i), "version": v, "version_source": src,
            "build_changed_since": last if v is None else None, "last_update_ms": last}


def warm(app):
    """The synthetic cars' versions, as indexes into SYN_VERSIONS (for a fast search of a large fleet)."""
    syn = app.ds.synthetic
    cache = app.cache.get("syn_versions")
    if cache is None or cache[0] is not syn:
        from ..util import u01
        ks = [int(u01(j, syn.salt + 21) * len(SYN_VERSIONS)) for j in range(4, syn.n + 1)]
        cache = app.cache["syn_versions"] = (syn, ks)
    return cache


def _syn_matches(app, syn, search):
    """Synthetic car numbers (from 4, ascending) whose name or version holds `search`, from the cached
    index in one pass; the last few searches are kept so the next page is quick."""
    cache = warm(app)
    memo = app.cache.setdefault("syn_version_hits", {})
    hit = memo.get(search)
    if hit is not None and hit[0] is syn:
        return hit[1]
    names = syn.index()[2]
    vmatch = [v is not None and search in v.lower() for v, _src in SYN_VERSIONS]
    ks = cache[1]
    hit = [i for i, (nm, k) in enumerate(zip(names, ks), 4) if vmatch[k] or search in nm]
    if len(memo) > 8:
        memo.clear()
    memo[search] = (syn, hit)
    return hit


def fleet_versions(req):
    search = (req.q("search") or "").strip().lower()
    limit = req.limit()
    fp = req.fingerprint()
    after = req.q("after")
    ck = decode_cursor(after, fp) if after else None
    if ck is not None and not (isinstance(ck, tuple) and len(ck) == 1 and type(ck[0]) is int):
        raise bad_cursor()
    start = ck[0] + 1 if ck else 1
    st, ds = req.store, req.ds
    acc = req.account
    out = []
    more = False
    ids = None if acc["cars"] == "all" else sorted((c for c in acc["cars"] if ds.exists(c)), key=lambda c: int(c[1:]))
    if req.csv:
        limit = 10 ** 9

    def featured_row(cid):
        h = ds.history(cid)
        meta = st.car_meta(cid)
        if h is None:
            return _syn_version(ds.synthetic, int(cid[1:])) | {"name": meta["name"]}
        sw = software_at(req.app, h, req.now)
        ups = _visible_updates(h, req.now)
        return {"car": cid, "name": meta["name"], "version": sw["version"], "version_source": sw["version_source"],
                "build_changed_since": sw["changed_since_ms"], "last_update_ms": ups[-1].end if ups else None}

    def ok(row):
        return not search or search in (row["version"] or "").lower() or search in row["name"].lower()

    if ids is not None:
        cands = [int(c[1:]) for c in ids if int(c[1:]) >= start]
        for n in cands:
            row = featured_row(f"c{n}")
            if ok(row):
                if len(out) == limit:
                    more = True
                    break
                out.append((n, row))
    else:
        total = ds.total_cars()
        special = sorted(int(c[1:]) for c in set(ds.featured) | set(st.car_over)
                         if c[1:].isdigit() and start <= int(c[1:]) <= total and ds.exists(c))
        if search:
            # synthetic cars that match, from one pass over the cached index (kept for the next page)
            syn_hits = _syn_matches(req.app, ds.synthetic, search)
            k0 = bisect.bisect_left(syn_hits, start)
            cands = sorted(set(special) | set(n for n in syn_hits[k0:k0 + limit + 1 + len(special)]))
        else:
            cands = None
        if cands is not None:
            for n in cands:
                cid = f"c{n}"
                row = featured_row(cid) if (cid in ds.featured or cid in st.car_over) else _syn_version(ds.synthetic, n)
                if ok(row):
                    if len(out) == limit:
                        more = True
                        break
                    out.append((n, row))
        else:
            n = start
            while n <= total:
                cid = f"c{n}"
                row = featured_row(cid) if (cid in ds.featured or cid in st.car_over) else _syn_version(ds.synthetic, n)
                if len(out) == limit:
                    more = True
                    break
                out.append((n, row))
                n += 1
    nxt = encode_cursor([out[-1][0]], fp) if more and out else None
    # each car's own time zone, so a list of many cars can show every row's times where that car is
    return {"items": [row | {"time_zone": st.car_meta(row["car"])["time_zone"]} for _, row in out], "next": nxt}


# ------------------------------------------------------------------ car events (as MQTT publishes them)

EVENT_TYPES = ("drive_started", "drive_ended", "charge_started", "charge_ended", "software_changed", "user_present")


def _iso(ms):
    return iso_utc(ms).replace("Z", "+00:00")


def _car_events(req, h, types):
    app = req.app
    mq = req.store.mqtt
    loc = bool(mq.get("publish_location"))
    units = {"distance": "km", "speed": "km/h", "temperature": "°C", "efficiency": "Wh/km", "elevation": "m"}
    out = []

    def name(pid):
        p = place_obj(app, pid)
        return p["name"] if p else None

    for d in h.drives:
        if d.start > req.now or not h.shown(d):
            continue
        x = cached_drive_row(req, h, d)
        a, b = drive_pids(app, d)
        base = {"event_type": "drive_started", "car": h.car.id, "drive": d.id, "start": _iso(d.start),
                "start_place": name(a), "start_soc": x["start_soc_pct"], "start_range": x["start_range_km"],
                "start_odometer": r(x["start_odometer_km"], 1)}
        if loc:
            base.update({"start_latitude": x["start"]["lat"], "start_longitude": x["start"]["lon"],
                         "start_address": x["start"]["address"]["short"]})
        if "drive_started" in types:
            out.append((d.start, "drive_started", d.id, dict(base, units=units)))
        if "drive_ended" in types and x["end_ms"] is not None:
            e = dict(base, event_type="drive_ended", end=_iso(x["end_ms"]),
                     end_place=name(b) if d.cut_ms is None else None,
                     duration_min=r(x["duration_ms"] / 60_000, 1), distance=r(x["distance_km"], 1),
                     energy_used_kwh=r(x["energy_kwh"], 2), efficiency=r(x["wh_per_km"], 0),
                     speed_max=x["speed_max_kph"], power_max_kw=x["power_max_kw"], power_min_kw=x["power_min_kw"],
                     end_soc=x["end_soc_pct"], end_range=x["end_range_km"], end_odometer=r(x["end_odometer_km"], 1),
                     outside_temp_avg=x["outside_avg_c"], inside_temp_avg=x["cabin_avg_c"], ascent=x["climb_m"],
                     descent=x["descent_m"])
            if loc:
                e.update({"end_latitude": x["end"]["lat"], "end_longitude": x["end"]["lon"],
                          "end_address": x["end"]["address"]["short"]})
            e["units"] = units
            out.append((x["end_ms"], "drive_ended", d.id, e))
    for c in h.charges:
        if c.start > req.now or not h.shown(c):
            continue
        x = cached_charge_row(req, h, c)
        base = {"event_type": "charge_started", "car": h.car.id, "charge": c.id, "start": _iso(c.start),
                "place": name(piece_pid(app, c)), "kind": c.ckind, "start_soc": x["start_soc_pct"],
                "start_range": x["start_range_km"]}
        if loc:
            base.update({"latitude": x["position"]["lat"], "longitude": x["position"]["lon"],
                         "address": x["address"]["short"]})
        if "charge_started" in types:
            out.append((c.start, "charge_started", c.id, dict(base, units=units)))
        if "charge_ended" in types and x["end_ms"] is not None:
            e = dict(base, event_type="charge_ended", end=_iso(x["end_ms"]),
                     duration_min=r(x["charging_ms"] / 60_000, 1), energy_added_kwh=r(x["energy_added_kwh"], 2),
                     energy_used_kwh=r(x["energy_grid_kwh"], 2), power_max_kw=x["power_max_kw"],
                     end_soc=x["end_soc_pct"], end_range=x["end_range_km"], battery_temp_avg=x["battery_avg_c"],
                     outside_temp_avg=x["outside_avg_c"], cost=(x["cost"] or {}).get("amount"), units=units)
            out.append((x["end_ms"], "charge_ended", c.id, e))
    if "software_changed" in types:
        for u in _visible_updates(h, req.now):
            if u.end > req.now:
                continue
            v, vs = update_version(app, u)
            out.append((u.end, "software_changed", u.id,
                        {"event_type": "software_changed", "car": h.car.id, "update": u.id, "at": _iso(u.end),
                         "version": v, "version_source": vs if v else None}))
    if "user_present" in types:
        for s in h.segs:
            if s.start > req.now:
                break
            if s.kind != "parked":
                continue
            for s0, _e0, st, rs, _kw in s.subs:
                if "user_present" in rs and s.start <= s0 < s.end and s0 <= req.now:
                    out.append((s0, "user_present", str(s0),
                                {"event_type": "user_present", "car": h.car.id, "at": _iso(s0),
                                 "place": name(piece_pid(app, s))}))
    return out


def car_events(req):
    types = req.list_param("types", EVENT_TYPES) or list(EVENT_TYPES)
    a, b = req.period()
    h = req.history()
    rows = []
    if h is not None:
        key = (tuple(sorted(types)), req.store.rev, derive.started(h, req.now), req.now // 10_000)
        evs = derive.memo(req.app, f"events:{h.car.id}", key, lambda: _car_events(req, h, set(types)))
        for at, typ, rid, payload in evs:
            if a <= at < b:
                rows.append({"id": f"{typ}:{rid}", "type": typ, "at_ms": at,
                             "topic": f"{req.store.mqtt['namespace']}/cars/{h.car.id}/event", "retained": False,
                             "payload": payload})
    items, nxt = page(req, rows, key=lambda x: (x["at_ms"], x["id"]), reverse=True)
    return {"car": req.car, "time_zone": req.tz(), "items": items, "next": nxt}


def register(router):
    router.add("/api/v1/cars/{car}/updates", GET=list_updates)
    router.add("/api/v1/cars/{car}/updates/{update}", PATCH=patch_update, admin=("PATCH",))
    router.add("/api/v1/fleet/versions", GET=fleet_versions)
    router.add("/api/v1/cars/{car}/events", GET=car_events)
