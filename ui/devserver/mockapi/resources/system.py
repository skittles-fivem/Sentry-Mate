"""Server, settings and health (docs/ui-api.md, "Server, settings and health"): server defaults,
map and address settings, notices, about, per-car data health, rebuilds, CAN and TeslaMate
overlaps, and storage.

Health follows one rule (the same one MQTT `healthy` uses): a car is healthy when its newest log
arrived within the hour and passed its check, no CAN Bridge key waits to be trusted, and no
rebuild is pending.
"""

from __future__ import annotations

import copy
import json
import os
import re

from .. import derive
from ..conventions import page
from ..errors import bad_input, invalid, not_found
from ..live import now_object
from ..util import DAY, HOUR, MIN, date_str, h01, local_date, r
from .reports import gap_rows

DEV_VERSION = "0.1.0-dev"
API_VERSION = "1"


# ------------------------------------------------------------------ server settings


def get_settings(req):
    out = copy.deepcopy(req.store.server_settings)
    if req.account["cars"] != "all":  # the server's car order lists every car: show only the ones this account sees
        out["car_order"] = [c for c in out["car_order"] if req.store.sees(req.account, c)]
    return out


UNITS = {"distance": ("km", "mi"), "temperature": ("c", "f"), "pressure": ("bar", "psi")}


def _https_url(v, template=False):
    if not isinstance(v, str) or not re.fullmatch(r"https://[A-Za-z0-9.-]+(:\d+)?(/\S*)?", v):
        return False
    return not template or all(x in v for x in ("{z}", "{x}", "{y}"))


def patch_settings(req):
    st = req.store
    cur = st.server_settings
    body = req.json()
    req.version_check(cur["version"])
    bad, rule = {}, {}
    known = ("version", "units", "range", "car_order", "geocode", "tiles", "release_check", "outside_links",
             "outbound", "host_names")
    for k in body:
        if k not in known:
            bad[k] = "Unknown field."
    new = copy.deepcopy(cur)

    def sub(name, spec):
        if name not in body:
            return
        v = body[name]
        if not isinstance(v, dict):
            bad[name] = "Must be an object."
            return
        for k, x in v.items():
            if k not in spec:
                bad[f"{name}.{k}"] = "Unknown field."
                continue
            want = spec[k]
            if isinstance(want, tuple) and all(isinstance(w, str) for w in want):
                if x not in want:
                    rule[f"{name}.{k}"] = "Must be one of: " + ", ".join(want) + "."
                    continue
            elif want is bool and not isinstance(x, bool):
                bad[f"{name}.{k}"] = "Must be true or false."
                continue
            elif want is int and (not isinstance(x, int) or isinstance(x, bool)):
                bad[f"{name}.{k}"] = "Must be a whole number."
                continue
            elif want == "str?" and x is not None and not isinstance(x, str):
                bad[f"{name}.{k}"] = "Must be text or null."
                continue
            elif want == "str" and not isinstance(x, str):
                bad[f"{name}.{k}"] = "Must be text."
                continue
            new[name][k] = x

    sub("units", UNITS)
    sub("geocode", {"mode": ("offline", "online"), "dataset": "str", "url": "str?", "language": "str",
                    "rounding_m": int})
    sub("tiles", {"mode": ("offline", "online"), "online_url": "str?", "attribution": "str", "max_zoom": int})
    sub("release_check", {"enabled": bool})
    sub("outside_links", {"maps": bool, "release_notes": bool})
    sub("outbound", {"proxy": "str?"})
    if "range" in body:
        if body["range"] not in ("rated", "ideal"):
            rule["range"] = "Must be rated or ideal."
        else:
            new["range"] = body["range"]
    if "car_order" in body:
        v = body["car_order"]
        if not isinstance(v, list) or not all(isinstance(x, str) for x in v):
            bad["car_order"] = "Must be a list of car ids."
        elif any(not req.ds.exists(c) for c in v):
            rule["car_order"] = "Unknown car in the list."
        else:
            new["car_order"] = list(dict.fromkeys(v))
    if "host_names" in body:
        v = body["host_names"]
        if not isinstance(v, list) or not all(isinstance(x, str) for x in v):
            bad["host_names"] = "Must be a list of host names."
        elif not 1 <= len(v) <= 10:
            rule["host_names"] = "1 to 10 host names."
        else:
            for i, x in enumerate(v):
                if not re.fullmatch(r"[A-Za-z0-9]([A-Za-z0-9-]*[A-Za-z0-9])?(\.[A-Za-z0-9]([A-Za-z0-9-]*[A-Za-z0-9])?)*",
                                    x) or len(x) > 253:
                    rule[f"host_names.{i}"] = "A host name such as sentry-mate.local."
            new["host_names"] = list(dict.fromkeys(v))
    if bad:
        raise bad_input(bad)
    g, t = new["geocode"], new["tiles"]
    if g["mode"] == "online" and not _https_url(g.get("url")):
        rule["geocode.url"] = "Online address lookup needs an https:// address of the service."
    if g.get("url") is not None and not _https_url(g["url"]):
        rule["geocode.url"] = "An https:// address, or null."
    if not 0 <= g["rounding_m"] <= 1000:
        rule["geocode.rounding_m"] = "Must be between 0 and 1000 m."
    if not 2 <= len(g["language"]) <= 10:
        rule["geocode.language"] = "A language code such as en or de."
    if t["mode"] == "online" and not _https_url(t.get("online_url"), template=True):
        rule["tiles.online_url"] = "An https:// tile address with {z}, {x} and {y}."
    px = new["outbound"].get("proxy")
    if px is not None and not re.fullmatch(r"(https?|socks5)://[A-Za-z0-9.-]+(:\d{1,5})?/?", px):
        rule["outbound.proxy"] = "A proxy address such as http://proxy.lan:3128 (no name or password in it), or null."
    if not 1 <= t["max_zoom"] <= 22:
        rule["tiles.max_zoom"] = "Must be between 1 and 22."
    if rule:
        raise invalid(rule)
    with st.lock:
        new["version"] = cur["version"] + 1
        st.server_settings = new
        accounts = list(st.accounts.values())
    st.record(req.account, "settings.update", "settings:server",
              "Changed the server settings: " + ", ".join(sorted(k for k in body if k != "version")) + ".")
    if any(cur[k] != new[k] for k in ("units", "range", "car_order")):
        # preferences default from these: everyone's (merged) preferences may have changed
        for a in accounts:
            req.app.events.publish("prefs", st.get_prefs(a), account_id=a["id"])
    if new["release_check"]["enabled"] and not cur["release_check"]["enabled"]:
        req.app.events.publish("notice", release_notice(req))
    return copy.deepcopy(new)


def maps_config(req):
    ss = req.store.server_settings
    t, g = ss["tiles"], ss["geocode"]
    return {"tiles": {"mode": t["mode"], "offline_url": None, "offline_data_present": False,
                      "online_template": t["online_url"] if t["mode"] == "online" else None,
                      "attribution": t["attribution"], "max_zoom": t["max_zoom"]},
            "geocoding": {"mode": g["mode"], "credit": "GeoNames (CC BY 4.0)" if g["mode"] == "offline" else
                          "The address service set in Settings", "offline_data_present": True,
                          "language": g["language"]}}


def geocode_status(req):
    g = req.store.server_settings["geocode"]
    return {"mode": g["mode"],
            "offline_data": {"present": True, "name": g["dataset"], "places": 48_213, "built_ms": req.app.cfg.now_ms
                             - 40 * DAY},
            "cache_size": 1_842, "rounding_m": g["rounding_m"],
            "online": {"enabled": g["mode"] == "online", "url": g.get("url"), "last_error": None, "retry_ms": None}}


def notices(req):
    items = []
    for n in req.store.notices:
        n = dict(n)
        if n["code"] == "certificate_expiring":
            days = max(0, (n["expires_ms"] - req.now) // DAY)
            n["message"] = f"The HTTPS certificate expires in {days} days. Renew it in Settings > Server."
            n["detail"] = {"expires_ms": n.pop("expires_ms"), "days": days}
        items.append(n)
    if req.store.server_settings["release_check"]["enabled"]:
        items.append(release_notice(req))
    return {"items": items, "next": None}


def release_notice(req):
    """The opt-in release notice: shown only while the owner has the release check on."""
    return {"code": "release_available", "level": "info", "message": "Sentry Mate 0.2.0 is out.",
            "since_ms": req.app.cfg.now_ms - 2 * DAY, "fix_link": "/help#about",
            "detail": {"version": "0.2.0", "notes_url": "https://example.org/sentry-mate/releases/0.2.0"}}


def about(req):
    pack = {"name": None, "version": None, "api": None}
    try:
        with open(os.path.join(req.app.cfg.pack, "ui.json"), encoding="utf-8") as f:
            m = json.load(f)
        pack = {"name": m.get("name"), "version": m.get("version"), "api": m.get("api")}
    except (OSError, ValueError):
        pass
    return {
        "version": DEV_VERSION, "api_version": API_VERSION, "ui_pack": pack, "dev_server": True,
        "links": {"source": None, "docs": "/help"},
        "licence": {"name": None, "url": None},
        "licences": [{"name": "model3dbc (Tesla Model 3 and Y signal list)", "licence": "MIT"},
                     {"name": "opendbc (Tesla signal lists)", "licence": "MIT"}],
        "data_credits": [{"name": "OpenStreetMap", "text": "Map data © OpenStreetMap contributors", "licence": "ODbL"},
                         {"name": "GeoNames", "text": "Place names for offline address lookup", "licence": "CC BY 4.0"}],
    }


# ------------------------------------------------------------------ per-car health


def car_health(req):
    st = req.store
    cid = req.car
    h = req.history()
    a, b = req.period(30 * DAY)
    keys = [k for k in st.keys if k["car"] == cid]
    toks = [t for t in st.tokens.get(cid, []) if not t["revoked"]]
    waiting = sum(k["logs_waiting"] for k in keys)
    pending = any(rb.get("pending_until", 0) > req.now for rb in st.rebuilds.get(cid, []))
    if h is None:
        from ..live import list_item
        it = list_item(req.app, cid, req.now)
        reasons = [c for c in it["attention"] if c in ("upload_silence", "trust_failed")]
        return {"car": cid, "time_zone": req.tz(), "healthy": not reasons, "reasons": reasons,
                "last_upload_ms": it["now"]["received_ms"],
                "last_log_trust": {"result": "failed" if "trust_failed" in reasons else "verified", "reason": None},
                "keys": [], "chain_gaps": [], "dropped_frames": [], "unknown_spans": [], "unknown_ms": 0,
                "logs_waiting": 0, "rebuild_pending": pending, "uploads_enabled": True}
    n = now_object(req.app, cid, req.now)
    last_upload = n.get("received_ms")
    reasons = []
    if n.get("stale"):
        reasons.append("upload_silence")
    gs = gap_rows(req, h)
    last_failed = None
    for s in h.segs:
        if s.start > req.now:
            break
        if s.kind == "gap" and s.reason == "failed_check":
            last_failed = s
    cur = h.seg_at(req.now - 75_000)
    trust = {"result": "verified", "reason": None, "log_start_ms": None}
    if cur is not None and cur.kind == "gap" and cur.open_end and last_failed and last_failed.end == cur.start:
        trust = {"result": "failed", "reason": "signature_mismatch", "log_start_ms": last_failed.start}
        reasons.append("trust_failed")
    if waiting:
        reasons.append("logs_waiting_trust")
    if pending:
        reasons.append("rebuild_pending")
    chain = [{"from_ms": x["start_ms"], "to_ms": x["end_ms"]} for x in gs
             if x["cause"] == "chain_gap" and a <= x["start_ms"] < b]
    dropped, unknown = [], []
    for s in h.segs_between(a, b, req.now):
        if s.kind == "drive":
            for p, q in s.drops:
                if a <= p < b:
                    dropped.append({"start_ms": p, "end_ms": q, "drive": s.id})
        elif s.kind == "charge":
            for p, q, k in s.gaps or []:
                if k == "dropped" and a <= p < b:
                    dropped.append({"start_ms": p, "end_ms": q, "charge": s.id})
        elif s.kind == "gap" and a <= s.start < b:
            if s.reason == "dropped_frames":
                dropped.append({"start_ms": s.start, "end_ms": s.end})
            live = s.open_end and s.end > req.now
            unknown.append({"start_ms": s.start, "end_ms": None if live else s.end, "reason": s.reason})
    return {
        "car": cid, "time_zone": req.tz(), "healthy": not reasons, "reasons": reasons, "last_upload_ms": last_upload,
        "last_log_trust": trust,
        "keys": [{"key": k["key"], "last_upload_ms": k["last_upload_ms"], "preferred": k["preferred"],
                  "device": k["device"], "logs_waiting": k["logs_waiting"]} for k in keys],
        "chain_gaps": chain, "dropped_frames": dropped, "unknown_spans": unknown,
        "unknown_ms": sum(((u["end_ms"] or req.now) - u["start_ms"]) for u in unknown),
        "logs_waiting": waiting, "rebuild_pending": pending, "uploads_enabled": bool(toks),
        "from_ms": a, "to_ms": min(b, req.now),
    }


def rebuild(req):
    st = req.store
    body = req.json(required=False)
    bad = {k: "Unknown field." for k in body if k not in ("from_ms", "to_ms")}
    for k in ("from_ms", "to_ms"):
        if k in body and (not isinstance(body[k], int) or isinstance(body[k], bool)):
            bad[k] = "Must be epoch milliseconds."
    if bad:
        raise bad_input(bad)
    h = req.history()
    a = body.get("from_ms", h.first_ms if h else req.now - DAY)
    b = body.get("to_ms", req.now)
    if a >= b:
        raise invalid({"to_ms": "Must be after from_ms."})
    if b - a > 3 * 366 * DAY:
        raise invalid({"from_ms": "At most three years at a time."})
    days = max(1, (b - a) // DAY)
    took = int(40 + days * 9)
    job = {"id": st.new_id("rb"), "from_ms": a, "to_ms": b, "at_ms": req.now, "took_ms": took,
           "pending_until": req.now + min(took, 5000), "kinds": ["drives", "charges", "states", "visits"]}
    with st.lock:
        st.rebuilds.setdefault(req.car, []).append(job)
    st.record(req.account, "car.rebuild", f"car:{req.car}",
              f"Rebuilt drives, charges, states and visits of {st.car_meta(req.car)['name']} "
              f"from the stored logs ({days} days).")
    req.app.events.publish("rows", {"car": req.car, "kinds": job["kinds"], "from_ms": a, "to_ms": b}, car=req.car)
    return {"car": req.car, "job": job["id"], "status": "done", "from_ms": a, "to_ms": b, "kinds": job["kinds"],
            "logs_read": int(days * 1440 * 0.6), "took_ms": took}


# ------------------------------------------------------------------ overlaps


def _overlap_view(h, i, o, now):
    can = [s for s in h.drives + h.charges if s.source == "can" and o["start_ms"] <= s.start < o["end_ms"]]
    tm = [s for s in h.shadow if o["start_ms"] <= s.start < o["end_ms"]]
    return {"id": f"overlap:{o['start_ms']}", "start_ms": o["start_ms"], "end_ms": o["end_ms"], "shown": o["shown"],
            "can": {"drives": sum(1 for s in can if s.kind == "drive"),
                    "charges": sum(1 for s in can if s.kind == "charge")},
            "teslamate": {"drives": sum(1 for s in tm if s.kind == "drive"),
                          "charges": sum(1 for s in tm if s.kind == "charge")}}


def get_overlaps(req):
    h = req.history()
    rows = [_overlap_view(h, i, o, req.now) for i, o in enumerate(h.overlaps)] if h else []
    items, nxt = page(req, rows, key=lambda x: (x["start_ms"],))
    return {"car": req.car, "time_zone": req.tz(), "items": items, "next": nxt}


def patch_overlap(req):
    h = req.history()
    body = req.json()
    bad = {k: "Unknown field." for k in body if k not in ("id", "start_ms", "shown")}
    if "shown" not in body:
        bad["shown"] = "Required: can or teslamate."
    if "id" not in body and "start_ms" not in body:
        bad["id"] = "Name the overlap by its id (or start_ms)."
    if bad:
        raise bad_input(bad)
    if body["shown"] not in ("can", "teslamate"):
        raise invalid({"shown": "Must be can or teslamate."})
    target = None
    for i, o in enumerate(h.overlaps if h else []):
        if body.get("id") == f"overlap:{o['start_ms']}" or body.get("start_ms") == o["start_ms"]:
            target = (i, o)
    if target is None:
        raise not_found("no_such_overlap", "There is no such overlap for this car.")
    i, o = target
    st = req.store
    with st.lock:
        o["shown"] = body["shown"]
    st.record(req.account, "car.overlap", f"car:{req.car}",
              f"Showing {'CAN' if body['shown'] == 'can' else 'imported TeslaMate'} history from "
              f"{date_str(o['start_ms'], req.tz())} to {date_str(o['end_ms'], req.tz())}.")
    req.app.events.publish("rows", {"car": req.car, "kinds": ["drives", "charges", "states", "visits"],
                                    "from_ms": o["start_ms"], "to_ms": o["end_ms"]}, car=req.car)
    out = get_overlaps(req)
    out["changed"] = _overlap_view(h, i, o, req.now)
    return out


# ------------------------------------------------------------------ storage

GB = 1024 ** 3
CAR_KINDS = [("logs", "CAN logs (sealed)", 52_000_000), ("series", "Time series", 8_300_000),
             ("curves", "Charge curves", 600_000), ("rows", "Drives, charges, states", 140_000)]
# a light synthetic car stores this much per kind (they have only summary data)
SYNTHETIC_BYTES = {"logs": 2_400_000_000, "series": 380_000_000, "curves": 20_000_000, "rows": 6_000_000}
SYNTHETIC_PER_DAY = 3_600_000


def _day_bytes(h, d, tz, kind, base, known, awake):
    """Made-up bytes one car stores for one day of one kind: more on busy days, none without data."""
    f = 0.08 * known / DAY + awake / DAY
    return int(base * f * (0.9 + 0.2 * h01("bytes", h.car.id, d.isoformat(), kind)))


def car_storage_days(req, h):
    """[(date, source, {kind: bytes})] for every local day of a car's history that has data."""
    tz = req.tz(h.car.id)
    key = (derive.started(h, req.now), tz)
    return derive.memo(req.app, f"storage:{h.car.id}", key, lambda: _storage_days(h, tz, req.now))


def car_storage(req):
    h = req.history()
    tz = req.tz()
    a, b = req.period()
    kinds_q = req.list_param("kinds", [k for k, _, _ in CAR_KINDS])
    rows, tot = [], {}
    if h is not None:
        days = car_storage_days(req, h)
        for d, src, per in days:
            for kind, b_ in per.items():
                tot[kind] = tot.get(kind, 0) + b_
        lo = local_date(max(a, h.first_ms), tz).isoformat()
        hi = local_date(min(b, req.now) - 1, tz).isoformat()
        for d, src, per in days:
            if lo <= d <= hi:
                for kind, b_ in per.items():
                    if not kinds_q or kind in kinds_q:
                        rows.append({"series": kind, "source": src, "date": d, "bytes": b_})
    elif req.ds.exists(req.car):
        tot = dict(SYNTHETIC_BYTES)
    items, nxt = page(req, rows, key=lambda x: (x["date"], x["series"]), reverse=True)
    kinds = [{"kind": k, "label": lab, "bytes": tot.get(k, 0)} for k, lab, _ in CAR_KINDS]
    return {"car": req.car, "time_zone": tz, "kinds": kinds, "total_bytes": sum(tot.values()), "items": items,
            "next": nxt}


def _storage_days(h, tz, now):
    from ..util import local_ms
    out = []
    d = local_date(h.first_ms, tz)
    last = local_date(now, tz)
    while d <= last:
        a = local_ms(d, 0, tz)
        e = min(a + DAY, now)
        known = awake = 0
        tm = can = 0
        for s in h.segs_between(a, e, now):
            for k, x, y in s.activity():
                span = max(0, min(y, e) - max(x, a))
                if k != "unknown":
                    known += span
                    if s.source == "teslamate":
                        tm += span
                    else:
                        can += span
                if k not in ("asleep", "unknown"):
                    awake += span
        if known > 0:
            src = "teslamate" if tm > can else "can"
            per = {}
            for kind, _lab, base in CAR_KINDS:
                if src == "teslamate" and kind in ("logs", "curves"):
                    continue  # an import brings rows and positions, no CAN logs
                per[kind] = _day_bytes(h, d, tz, kind, base if src == "can" else base // 6, known, awake)
            out.append((d.isoformat(), src, per))
        d = d.fromordinal(d.toordinal() + 1)
    return out


def system_storage(req):
    """The server's storage: the sum of every car's figures, imported history, and offline map tiles."""
    s = req.store.storage
    ds = req.ds
    sums = {"logs": 0, "series": 0, "rows": 0, "imported": 0}
    growth = {}
    first_growth = date_str(req.now - 32 * DAY, "UTC")
    for cid, h in ds.featured.items():
        for d, src, per in car_storage_days(req, h):
            for kind, b_ in per.items():
                if src == "teslamate":
                    sums["imported"] += b_
                else:
                    sums["series" if kind == "curves" else kind] += b_
            if d > first_growth and src == "can":
                growth[d] = growth.get(d, 0) + sum(per.values())
    n_syn = ds.synthetic.count
    for k, v in SYNTHETIC_BYTES.items():
        sums["series" if k == "curves" else k] += v * n_syn
    labels = {"logs": "CAN logs (sealed)", "series": "Time series and charge curves",
              "rows": "Drives, charges, states", "imported": "Imported from TeslaMate"}
    kinds = [{"kind": k, "label": labels[k], "bytes": v} for k, v in sums.items()]
    kinds.append({"kind": "tiles", "label": "Offline map tiles", "bytes": s["tiles_bytes"]})
    today = date_str(req.now, "UTC")
    days = sorted(d for d in growth if d < today)[-30:]
    rows = [{"date": d, "bytes": growth[d] + SYNTHETIC_PER_DAY * n_syn} for d in days]
    per_day = sum(g["bytes"] for g in rows) / len(rows) if rows else 0
    return {"kinds": kinds, "total_bytes": sum(k["bytes"] for k in kinds), "free_bytes": s["free_bytes"],
            "growth": rows, "growth_per_day_bytes": int(per_day),
            "full_in_days": int(s["free_bytes"] / per_day) if per_day else None}


def system_health(req):
    app = req.app
    s = req.store.storage
    up = s["uptime_ms"] + (req.now - app.cfg.now_ms)
    ds = app.ds
    return {
        "read_ms": round(1.2 + h01("read", req.now // 10_000), 2), "uptime_ms": up,
        "versions": {"server": DEV_VERSION, "api": API_VERSION, "db_format": 3, "timeline_rules": 7,
                     "import_mapping": 2},
        "db": {"cars": ds.total_cars(), "fleet_bytes": 18_000 + 120 * ds.total_cars(),
               "open_cars": min(3, len(ds.featured)), "open_bytes": int(1.9 * GB) * min(3, len(ds.featured)) // 3,
               "cars_opened": 41, "tables_read": 9},
        "ingest": {"memory": {"used_bytes": 212 * 1024 ** 2, "limit_bytes": 1024 ** 3}, "cached_cars": len(ds.featured)},
        "geocoder": {"mode": req.store.server_settings["geocode"]["mode"], "cache_size": 1_842, "hits_pct": 97.4},
        "housekeeping_last_ms": req.now - 47 * MIN,
        "timings_ms": {"decode_per_log": 38.0, "rebuild_per_window": 410.0, "api_p50": 4.0, "api_p95": 31.0},
        "dev_server": {"dataset_build_ms": int(app.build_s * 1000), "now_ms": req.now, "clock_frozen": app.cfg.freeze_clock},
    }


def register(router):
    router.add("/api/v1/settings", GET=get_settings, PATCH=patch_settings, admin=("PATCH",))
    router.add("/api/v1/maps/config", GET=maps_config)
    router.add("/api/v1/geocode/status", GET=geocode_status)
    router.add("/api/v1/notices", GET=notices)
    router.add("/api/v1/about", GET=about)
    router.add("/api/v1/cars/{car}/health", GET=car_health)
    router.add("/api/v1/cars/{car}/rebuild", POST=rebuild, admin=("POST",))
    router.add("/api/v1/cars/{car}/overlaps", GET=get_overlaps, PATCH=patch_overlap, admin=("PATCH",))
    router.add("/api/v1/cars/{car}/storage", GET=car_storage)
    router.add("/api/v1/system/storage", GET=system_storage, admin=("GET",))
    router.add("/api/v1/system/health", GET=system_health, admin=("GET",))


__all__ = ["register", "HOUR", "r"]
