"""Fleet and cars (docs/ui-api.md, "Fleet and cars").

/api/v1/cars pages through any number of cars without building the whole list: the featured
cars (and any synthetic car the owner renamed, pinned or ordered) are computed one by one, and the
synthetic cars come from an index that is already in order (activity) or cached (name, battery).
"""

from __future__ import annotations

import bisect
import datetime as dt
import secrets

from ..conventions import bad_cursor, decode_cursor, encode_cursor, nk, page
from ..errors import bad_input, invalid, not_found
from ..live import attention, list_item
from ..util import valid_zone
from .. import gapfill
from .. import sources as SRC

STATES = ("driving", "charging", "parked", "asleep", "unknown")
SORTS = ("last_activity", "name", "soc", "custom")
CONFIG_KEYS = ("trim", "drivetrain", "performance_package", "pack_kwh_code", "wheel", "paint", "roof", "spoiler",
               "right_hand_drive", "map_region", "supercharging_access")


def _num(cid):
    return int(cid[1:])


def _key(sort, item, prefs, reverse):
    n = _num(item["id"])
    if sort == "name":
        return (item["name"].lower(), n)
    if sort == "soc":
        return (nk(item["now"]["soc_pct"], reverse), n)
    if sort == "custom":
        pins = prefs["pinned_cars"]
        order = prefs["car_order"]
        if item["id"] in pins:
            return (0, pins.index(item["id"]), 0, n)
        if item["id"] in order:
            return (1, order.index(item["id"]), 0, n)
        return (2, 0, -item["last_activity_ms"], n)
    return (-item["last_activity_ms"], n)


def _matches(item, search, states):
    if states and item["now"]["state"] not in states:
        return False
    if search:
        s = search.lower()
        if s not in item["name"].lower() and s not in item["model"].lower() and s not in item["vin_prefix"].lower():
            return False
    return True


def list_cars(req):
    st = req.store
    ds = req.ds
    search = (req.q("search") or "").strip()
    states = req.list_param("state", STATES)
    sort = req.enum("sort", SORTS, "last_activity")
    dflt_dir = "asc" if sort in ("name", "custom") else "desc"
    d = req.enum("dir", ("asc", "desc"), dflt_dir)
    # every key is built ascending = the default direction; the other direction walks it backwards
    reverse = d != dflt_dir
    if sort == "custom" and d == "desc":
        reverse = True
    limit = req.limit()
    if req.csv:  # every matching car, in the same order as the pages (from `after` on)
        limit = 10 ** 9
    fp = req.fingerprint()
    after = req.q("after")
    ck = decode_cursor(after, fp) if after else None
    prefs = st.get_prefs(req.account)
    acc = req.account
    t = req.now
    syn = ds.synthetic
    special_ids = []
    if acc["cars"] == "all":
        special_ids = list(ds.featured)
        extra = set(st.car_over) | set(prefs["pinned_cars"]) | set(prefs["car_order"])
        special_ids += sorted((c for c in extra if c not in ds.featured and syn.has(c)), key=_num)
        use_syn = syn.count > 0
    else:
        special_ids = [c for c in acc["cars"] if ds.exists(c)]
        use_syn = False
    special_set = set(special_ids)
    # the explicit cars
    cands = []
    for cid in special_ids:
        it = typed_over(list_item(req.app, cid, t), st.car_meta(cid))
        if _matches(it, search, states):
            cands.append((_key(sort, it, prefs, False if sort != "soc" else d == "asc"), it))
    soc_desc = sort == "soc" and d == "desc"
    if sort == "soc":
        # soc: ascending keys; desc walks backwards. Nulls last both ways via nk().
        cands = [((nk(it["now"]["soc_pct"], soc_desc), _num(it["id"])), it) for _, it in cands]
        reverse = soc_desc
    cands.sort(key=lambda x: x[0])
    keys = [k for k, _ in cands]
    try:
        if ck is None:
            pick = cands[::-1] if reverse else cands
        elif not reverse:
            pick = cands[bisect.bisect_right(keys, ck):]
        else:
            pick = cands[:bisect.bisect_left(keys, ck)][::-1]
        # the synthetic cars, already in order
        out = []
        syn_iter = _syn_iter(req, sort, reverse, ck, search, states, special_set, soc_desc) if use_syn else iter(())
        nxt_syn = next(syn_iter, None)
        i = 0
        while len(out) < limit + 1:
            a = pick[i] if i < len(pick) else None
            if a is None and nxt_syn is None:
                break
            if nxt_syn is None or (a is not None and ((a[0] < nxt_syn[0]) != reverse)):
                out.append(a)
                i += 1
            else:
                out.append(nxt_syn)
                nxt_syn = next(syn_iter, None)
    except TypeError:
        if ck is None:
            raise
        raise bad_cursor()  # a cursor made by hand whose key does not compare with this sort's keys
    more = len(out) > limit
    out = out[:limit]
    nxt = encode_cursor(out[-1][0], fp) if more and out else None
    res = {"items": [it for _, it in out], "next": nxt}
    if not search and not states:
        res["total"] = st.visible_car_count(acc)
    return res


def _syn_key(syn, sort, i, soc_desc):
    if sort == "name":
        return (syn.name(i).lower(), i)
    if sort == "soc":
        return (nk(syn.soc(i), soc_desc), i)
    if sort == "custom":
        return (2, 0, -(syn.now_ms - syn.age_ms(i)), i)
    return (-(syn.now_ms - syn.age_ms(i)), i)


def _syn_iter(req, sort, reverse, ck, search, states, special, soc_desc):
    """(key, item) for synthetic cars after the cursor, in order, filtered."""
    syn = req.ds.synthetic
    key = "name" if sort == "name" else "soc" if sort == "soc" else "activity"
    s = search.lower() if search else None
    filtered = bool(s or states)
    # the cars in order (only those that pass the filters, from a small cache: no scan of the fleet per page)
    order = syn.filtered(key, s, states) if filtered else syn.order(key) if key != "activity" else None
    if order is not None:
        at = order.__getitem__
        m = len(order)
    else:
        at = lambda p: 4 + p  # noqa: E731
        m = syn.count
    # binary search the first position after the cursor
    if ck is None:
        pos = m - 1 if reverse else 0
    else:
        lo, hi = 0, m
        while lo < hi:
            mid = (lo + hi) // 2
            if _syn_key(syn, sort, at(mid), soc_desc) <= ck:
                lo = mid + 1
            else:
                hi = mid
        pos = lo - 1 if reverse else lo
        if reverse:
            # step back over keys equal to the cursor
            while pos >= 0 and _syn_key(syn, sort, at(pos), soc_desc) >= ck:
                pos -= 1
    step = -1 if reverse else 1
    while 0 <= pos < m:
        i = at(pos)
        pos += step
        cid = f"c{i}"
        if cid in special:
            continue
        it = syn.summary(i, req.now)
        it = {k: v for k, v in it.items() if not k.startswith("_")}
        yield _syn_key(syn, sort, i, soc_desc), it


def fleet_summary(req):
    st = req.store
    ds = req.ds
    acc = req.account
    by = dict.fromkeys(STATES, 0)
    stale = 0
    ids = list(ds.featured) if acc["cars"] == "all" else [c for c in acc["cars"] if ds.exists(c)]
    sold = 0
    for cid in ids:
        if st.car_meta(cid)["sold"]:
            sold += 1
            continue
        it = list_item(req.app, cid, req.now)
        by[it["now"]["state"]] += 1
        stale += 1 if it["now"]["stale"] else 0
    if acc["cars"] == "all" and ds.synthetic.count:
        sby, sst = ds.synthetic.counts()
        for k, v in sby.items():
            by[k] += v
        stale += sst
    return {"cars": st.visible_car_count(acc) - sold, "by_state": by, "stale": stale, "sold": sold}


def _att_key(a):
    """Newest first; one car can have the same code twice (two low tyres), so the tyre is in the key."""
    return (a["since_ms"], -_num(a["car"]), a["code"], (a.get("detail") or {}).get("tyre") or "")


def warm(app):
    """The synthetic cars' attention items as sort keys, in order (built once: the fleet never changes)."""
    syn = app.ds.synthetic
    cache = app.cache.get("syn_attention")
    if cache is None or cache[0] is not syn:
        keys = sorted((since, -i, code, "") for since, i, code in syn.attention())
        cache = app.cache["syn_attention"] = (syn, keys)
    return cache[1]


def _syn_attention_item(req, k):
    from ..live import _synthetic_detail
    i = -k[1]
    sm = req.ds.synthetic.summary(i, req.now)
    return {"car": f"c{i}", "code": k[2], "since_ms": k[0], "detail": _synthetic_detail(k[2], sm)}


def fleet_attention(req):
    ds = req.ds
    acc = req.account
    rows = []
    ids = list(ds.featured) if acc["cars"] == "all" else [c for c in acc["cars"] if ds.exists(c)]
    for cid in ids:
        if not req.store.car_meta(cid)["sold"]:
            rows += attention(req.app, cid, req.now)
    if not (acc["cars"] == "all" and ds.synthetic.count):
        items, nxt = page(req, rows, key=_att_key, reverse=True)
        return {"items": items, "next": nxt}
    # a large fleet: the synthetic items are a cached sorted list, so a page is two slices and a merge
    skeys = warm(req.app)
    fp = req.fingerprint()
    ck = decode_cursor(req.q("after"), fp) if req.q("after") else None
    lim = len(rows) + len(skeys) if req.csv else req.limit()  # CSV: every row (from `after` on)
    try:
        feat = sorted(((_att_key(a), a) for a in rows), key=lambda kv: kv[0])
        if ck is not None:
            feat = [kv for kv in feat if kv[0] < ck]
        j = bisect.bisect_left(skeys, ck) if ck is not None else len(skeys)
    except TypeError:
        raise bad_cursor()
    picked = feat[-(lim + 1):] + [(k, None) for k in skeys[max(0, j - lim - 1):j]]
    picked.sort(key=lambda kv: kv[0], reverse=True)
    more = len(picked) > lim
    picked = picked[:lim]
    items = [a if a is not None else _syn_attention_item(req, k) for k, a in picked]
    nxt = encode_cursor(picked[-1][0], fp) if more and picked else None
    return {"items": items, "next": nxt}


def config_value(c):
    """A config value with the input it was read from (`sources`; empty when typed by an admin or unknown)."""
    src = c.get("source")
    refs = ([SRC.ref("smo")] if src in ("can", "vin") else [SRC.ref("tesla_api")] if src == "tesla_api"
            else [SRC.ref("import", "teslamate")] if src == "teslamate" else [])
    return {"value": c.get("value"), "source": src, "sources": refs}


# ------------------------------------------------------------------ vehicle info (made up, in the server's shape)

PLANTS = {"F": ("Fremont; California", "US", "model_year"), "A": ("Austin; Texas (Giga Texas)", "US", "model_year"),
          "C": ("Shanghai (Giga Shanghai)", "CN", "build_year"), "B": ("Berlin-Brandenburg (Giga Berlin)", "DE", "build_year")}
BODIES = {"Model Y": "Class D MPV 5-door", "Model 3": "sedan 4-door", "Model S": "hatchback 5-door",
          "Model X": "Class E MPV 5-door", "Cybertruck": "truck"}
# config key -> vehicle field
CONFIG_FIELDS = {"trim": "trim", "drivetrain": "drive", "performance_package": "performance", "pack_kwh_code": "pack_code",
                 "wheel": "wheel", "paint": "paint", "roof": "roof", "spoiler": "spoiler",
                 "right_hand_drive": "steering", "map_region": "region", "supercharging_access": "supercharging_access"}
# the keys an admin may type beside CONFIG_KEYS (they show in `vehicle`, not in `config`)
EXTRA_KEYS = ("model", "model_year", "plant", "body", "restraint", "seats", "battery_type", "chemistry", "nominal_kwh",
              "usable_kwh", "motors", "epa_range_km", "wheel_in")
# each extra key's kind, as the server's OWNER_EXTRA_KEYS: ("text",), ("word", words), ("whole"|"number", lo, hi)
EXTRA_KINDS = {"model": ("text",), "model_year": ("whole", 2008, 2045), "plant": ("text",), "body": ("text",),
               "restraint": ("text",), "seats": ("whole", 1, 9), "battery_type": ("text",),
               "chemistry": ("word", ("lfp", "nca_nmc")), "nominal_kwh": ("number", 5, 300),
               "usable_kwh": ("number", 5, 300), "motors": ("whole", 1, 4), "epa_range_km": ("number", 10, 2000),
               "wheel_in": ("whole", 14, 24),
               # the recommended tyre pressures, typed (kPa): not build facts, so not in `vehicle` (the Now reads them)
               "tyre_pressure_front_kpa": ("number", 100, 600), "tyre_pressure_rear_kpa": ("number", 100, 600)}
TYRE_KEYS = ("tyre_pressure_front_kpa", "tyre_pressure_rear_kpa")
VEHICLE_FACTS = ("maker", "built_in", "model", "model_year", "plant", "platform", "body", "restraint", "seats",
                 "battery_type", "chemistry", "pack", "pack_code", "nominal_kwh", "usable_kwh", "motors", "drive",
                 "performance", "trim", "epa_range_km", "wheel", "wheel_in", "paint", "roof", "spoiler", "steering",
                 "region", "supercharging_access")


def _fact(value=None, source=None, confidence=None, note=None):
    if value is None:
        return {"value": None, "source": None, "confidence": None}
    f = {"value": value, "source": source, "confidence": confidence or "high"}
    if note:
        f["note"] = note
    return f


def _extra(path, kind, v, bad, rule):
    """One extra key as typed (the server's owner_extra): null to forget it, or a value of its kind; else an error."""
    if v is None:
        return None
    k = kind[0]
    if k == "text":
        if not isinstance(v, str):
            bad[path] = "Must be text or null."
            return None
        t = v.strip()
        if not t or len(t) > 100 or any(ord(c) < 32 or ord(c) == 127 for c in t):
            rule[path] = "1 to 100 characters, or null to forget it."
        return t
    if k == "word":
        if not isinstance(v, str) or v not in kind[1]:
            bad[path] = f"One of {', '.join(kind[1])}, or null."
        return v
    if isinstance(v, bool) or not isinstance(v, (int, float)):
        bad[path] = "Must be a number or null."
        return None
    lo, hi = kind[1], kind[2]
    if not lo <= v <= hi or (k == "whole" and float(v) != int(v)):
        rule[path] = f"{'A whole number' if k == 'whole' else 'A number'} from {float(lo)} to {float(hi)}, or null."
    return v


def _typed(meta, key):
    c = meta.get("config", {}).get(key) or {}
    return c["value"] if c.get("source") == "owner" and c.get("value") is not None else None


def typed_over(item, meta):
    """A typed model and model year are also the list item's `model` and `year` (the server's typed_over)."""
    if _typed(meta, "model") is not None:
        item["model"] = _typed(meta, "model")
    if _typed(meta, "model_year") is not None:
        item["year"] = int(_typed(meta, "model_year"))
    item["sold"] = meta.get("sold", False)
    item["sold_date"] = meta.get("sold_date")
    if item["sold"]:  # a sold car is not watched any more
        item["attention"] = []
    return item


def vehicle_info(meta, battery_setting="auto"):
    """`vehicle` of /cars/{car}: what the server works out from the VIN, CAN, the Tesla API and the owner.

    `battery_setting` is the car's battery setting: lfp or nca_nmc is the owner's word on the chemistry, unless
    config names one."""
    vp = meta.get("vin_prefix") or ""
    cfg = meta.get("config", {})
    build = meta.get("build", {})
    plant = PLANTS.get(vp[10:11])
    build_year = bool(plant) and plant[2] == "build_year"
    v = {k: _fact() for k in VEHICLE_FACTS}
    model = meta.get("model")
    v["maker"] = _fact("Tesla Inc.", "vin")
    v["model"] = _fact(model, "vin")
    v["model_year"] = _fact(meta.get("year"), "vin",
                            note="VIN position 10: the calendar year built (Tesla's service manual)" if build_year
                            else "VIN position 10 (49 CFR 565.15)")
    if plant:
        v["plant"] = _fact(plant[0], "vin")
        v["built_in"] = _fact(plant[1], "vin")
    v["body"] = _fact(BODIES.get(model), "vin")
    for k, field in CONFIG_FIELDS.items():
        c = cfg.get(k) or {}
        val = c.get("value")
        if val is None:
            continue
        if field == "steering":
            val = "right" if val else "left"
        v[field] = _fact(val, c.get("source"))
    for k in EXTRA_KEYS:
        c = cfg.get(k) or {}
        if c.get("value") is not None and c.get("source") == "owner":
            val = c["value"]
            v[k] = _fact({"min": val, "max": val} if k in ("nominal_kwh", "usable_kwh") else val, "owner")
    if v["chemistry"]["source"] != "owner" and battery_setting in ("lfp", "nca_nmc"):
        v["chemistry"] = _fact(battery_setting, "owner")
    drive = v["drive"]["value"]
    if drive and v["motors"]["source"] != "owner":
        v["motors"] = _fact(3 if "Plaid" in str(meta.get("trim")) or "Cyberbeast" in str(meta.get("trim"))
                            else 2 if drive == "AWD" else 1, "vin", note="VIN position 8")
    if model in ("Model S", "Model X"):
        v["platform"] = _fact("legacy" if meta.get("profile") == "tesla-sx-legacy" else "refresh", "vin",
                              note="VIN position 8")
    # the usual pack for the build (the brick count is not read for it); a US VIN never tells the chemistry
    if build.get("chemistry") and v["chemistry"]["source"] != "owner":
        us = plant is None or plant[1] == "US"
        v["chemistry"] = _fact(build["chemistry"], "vin", "low" if us else "medium",
                               note="the usual pack for this build; a US VIN does not tell the chemistry" if us
                               else "the usual pack for this build")
    if build.get("full_now_kwh") and v["nominal_kwh"]["source"] != "owner":
        now = round(build["full_now_kwh"], 1)
        v["nominal_kwh"] = _fact({"min": now, "max": now}, "can", note="BMS_nominalFullPackEnergy: the pack's own reading now")
        if build.get("buffer_kwh") is not None:
            u = round(now - build["buffer_kwh"], 1)
            v["usable_kwh"] = _fact({"min": u, "max": u}, "can", note="BMS_nominalFullPackEnergy less BMS_energyBuffer, now")
    if build.get("full_range_new_km") and (plant is None or plant[1] == "US") and v["epa_range_km"]["source"] != "owner":
        v["epa_range_km"] = _fact(build["full_range_new_km"], "vin", "medium", note="EPA rating, US test cycle")
    v["battery_type"] = _fact("electric (no battery detail)", "vin") if vp[6:7] == "E" else _fact()
    yk = None if v["model_year"]["value"] is None else (
        "build_year" if build_year and v["model_year"]["source"] == "vin" else "model_year")
    return dict(v, vin_prefix=vp or None, year_kind=yk,
                trim_candidates=[], epa_name=None, epa_id=None, wheel_options_in=[], disagreements=[],
                unknown_codes=[], notes=[])


def car_detail(req, cid=None):
    cid = cid or req.car
    st = req.store
    meta = st.car_meta(cid)
    it = typed_over(list_item(req.app, cid, req.now), meta)
    it["config"] = {k: config_value(meta["config"].get(k, {"value": None, "source": None})) for k in CONFIG_KEYS}
    for k in TYRE_KEYS:
        it["config"][k] = config_value(meta["config"].get(k, {"value": None, "source": None}))
    s = st.car_settings.get(cid) or {}
    it["vehicle"] = vehicle_info(meta, (s.get("battery_chemistry") or {}).get("setting", "auto"))
    it["version"] = meta["version"]
    return it


def patch_car(req):
    st = req.store
    cid = req.car
    meta = st.car_meta(cid)
    body = req.json()
    req.version_check(meta["version"])
    bad, rule = {}, {}
    for k in body:
        if k not in ("version", "name", "time_zone", "config", "sold", "sold_date"):
            bad[k] = "Unknown field."
    if "name" in body:
        if not isinstance(body["name"], str):
            bad["name"] = "Must be text."
        elif not 1 <= len(body["name"].strip()) <= 64:
            rule["name"] = "1 to 64 characters."
    if "time_zone" in body:
        z = body["time_zone"]
        if z is None or (isinstance(z, str) and z.strip().lower() == "auto"):
            body["time_zone"] = None  # automatic again
        elif not isinstance(z, str):
            bad["time_zone"] = "Must be text or null."
        elif not valid_zone(z.strip()):
            rule["time_zone"] = "Not a known time zone (for example Europe/Amsterdam), or null for automatic."
        else:
            body["time_zone"] = z.strip()
    if "sold" in body and not isinstance(body["sold"], bool):
        bad["sold"] = "Must be true or false."
    if "sold_date" in body and body["sold_date"] is not None:
        try:
            dt.date.fromisoformat(body["sold_date"])
            if len(body["sold_date"]) != 10:
                raise ValueError
        except (TypeError, ValueError):
            rule["sold_date"] = "A date (YYYY-MM-DD), or null."
    cfg = body.get("config")
    if cfg is not None:
        if not isinstance(cfg, dict):
            bad["config"] = "Must be an object."
        else:
            for k, v in list(cfg.items()):
                if k in EXTRA_KINDS:
                    cfg[k] = _extra(f"config.{k}", EXTRA_KINDS[k], v, bad, rule)
                elif k not in CONFIG_KEYS:
                    bad[f"config.{k}"] = "Unknown setting."
                elif v is not None and not isinstance(v, (str, bool, int, float)):
                    bad[f"config.{k}"] = "Must be text, a number, true/false or null."
    if bad:
        raise bad_input(bad)
    if rule:
        raise invalid(rule)
    with st.lock:
        over = st.car_over.setdefault(cid, {})
        if "name" in body:
            over["name"] = body["name"].strip()
        if "time_zone" in body:
            over["time_zone"] = body["time_zone"]
        if "sold" in body:
            over["sold"] = body["sold"]
            if not body["sold"]:
                over.pop("sold_date", None)
        if "sold_date" in body and over.get("sold"):
            over["sold_date"] = body["sold_date"]
        for k, v in (cfg or {}).items():
            if v is None:
                over.setdefault("config", {}).pop(k, None)
            else:
                over.setdefault("config", {})[k] = v
        over["version"] = meta["version"] + 1
    st.record(req.account, "car-changed", f"car:{cid}", _car_change_words(meta["name"], body))
    if "name" in body:
        req.app.events.publish("fleet", {"car": cid, "change": "renamed", "name": over["name"]}, car=cid)
    if "sold" in body:
        req.app.events.publish("fleet", {"car": cid, "change": "sold", "name": None}, car=cid)
    return car_detail(req)


# ------------------------------------------------------------------ per-car settings

CHEM = ("auto", "lfp", "nca_nmc")


def get_settings(req):
    st = req.store
    s = st.car_settings.get(req.car)
    if s is None:
        s = st.car_settings.setdefault(req.car, {
            "version": 1, "battery_chemistry": {"setting": "auto", "detected": None, "evidence": None},
            "free_supercharging": [], "capacity_new_kwh": None, "full_range_new_km": None,
            "warranty_threshold_pct": 70, "efficiency_wh_per_km_override": None, "software_version_typed": None,
            "import_gap_fill": None, "pack_replaced_date": None, "pack_new_kwh": None,
            "default_price_per_kwh": None})
    out = dict(s)
    out.setdefault("import_gap_fill", None)
    out.setdefault("default_price_per_kwh", None)
    out.setdefault("pack_replaced_date", None)
    out.setdefault("pack_new_kwh", None)
    out["import_gap_fill_on"] = gapfill.on(st, req.car)
    out["import_gap_fill_default"] = bool((st.server_settings.get("import_gap_fill") or {}).get("enabled", True))
    # the live inputs, highest priority first (the car's `source_order`, else the default), and the imports
    out["source_order"] = SRC.order_of(st, req.car)
    out["source_order_default"] = list(SRC.DEFAULT_ORDER)
    out["inputs"] = _inputs(req, out["source_order"])
    out["imports"] = _imports(req)
    # what the car's build gives when the owner has typed nothing (the inputs' placeholders)
    h = req.history()
    car = h.car if h is not None else None
    out["capacity_new_kwh_default"] = round(car.full_new_kwh - car.buffer_kwh, 1) if car else None
    out["full_range_new_km_default"] = car.full_range_new_km if car else None
    out["efficiency_wh_per_km_default"] = car.rated_wh_per_km if car else None
    out["defaults_source"] = "vehicle_info" if car else None
    out["car"] = req.car
    out["time_zone"] = req.tz()
    return out


def _inputs(req, order):
    """One entry per live input word in the car's order: whether it feeds this car in the made-up data (as
    /cars/{car}/now `inputs` says) and its newest data."""
    from ..live import now_object
    last = {}
    try:
        for x in now_object(req.app, req.car, req.now).get("inputs") or []:
            last[x["kind"]] = x.get("last_ms")
    except Exception:  # a car with no live values yet: every input off
        last = {}
    return [{"kind": k, "label": SRC.INPUT_LABEL[k], "on": k in last, "last_ms": last.get(k)} for k in order]


def _imports(req):
    """The car's imported history per site, oldest first: { site, label, from_ms, to_ms }."""
    h = req.history()
    if h is None:
        return []
    span = {}
    for x in list(h.segs) + list(h.shadow):
        site = SRC.site_of(x)
        if site is None or x.start > req.now:
            continue
        a, b = span.get(site, (x.start, x.end))
        span[site] = (min(a, x.start), max(b, min(x.end, req.now)))
    return [{"site": k, "label": SRC.ref("import", k)["label"], "from_ms": a, "to_ms": b}
            for k, (a, b) in sorted(span.items(), key=lambda kv: kv[1][0])]


def _num_rule(body, k, lo, hi, rule, bad, allow_null=True):
    if k not in body:
        return
    v = body[k]
    if v is None and allow_null:
        return
    if isinstance(v, bool) or not isinstance(v, (int, float)):
        bad[k] = "Must be a number" + (" or null." if allow_null else ".")
    elif not lo <= v <= hi:
        rule[k] = f"Must be between {lo} and {hi}."


def _default_price_rule(v, rule, bad):
    """`default_price_per_kwh`: { value 0 to 10, currency three letters } (null forgets it)."""
    import re
    k = "default_price_per_kwh"
    if not isinstance(v, dict):
        bad[k] = "Must be { value, currency } or null."
        return
    val, cur = v.get("value"), v.get("currency")
    if isinstance(val, bool) or not isinstance(val, (int, float)):
        bad[k + ".value"] = "Must be a number."
    elif not 0 <= val <= 10:
        rule[k + ".value"] = "Must be between 0 and 10."
    if not isinstance(cur, str):
        bad[k + ".currency"] = "Must be a currency code such as USD."
    elif not re.fullmatch(r"[A-Za-z]{3}", cur):
        rule[k + ".currency"] = "Three letters, such as USD or EUR."


def patch_settings(req):
    from ..util import parse_date
    st = req.store
    cur = get_settings(req)
    body = req.json()
    req.version_check(cur["version"])
    bad, rule = {}, {}
    known = ("version", "battery_chemistry", "free_supercharging", "capacity_new_kwh", "full_range_new_km",
             "warranty_threshold_pct", "efficiency_wh_per_km_override", "software_version_typed", "import_gap_fill",
             "pack_replaced_date", "pack_new_kwh", "source_order", "default_price_per_kwh")
    for k in body:
        if k not in known:
            bad[k] = "Unknown field."
    if "battery_chemistry" in body:
        bc = body["battery_chemistry"]
        if not isinstance(bc, dict) or "setting" not in bc:
            bad["battery_chemistry"] = 'Send { "setting": "auto" | "lfp" | "nca_nmc" }.'
        elif bc["setting"] not in CHEM:
            rule["battery_chemistry.setting"] = "Must be auto, lfp or nca_nmc."
    if "free_supercharging" in body:
        fs = body["free_supercharging"]
        if not isinstance(fs, list) or not all(isinstance(p, dict) for p in fs):
            bad["free_supercharging"] = "Must be a list of { from_date, to_date }."
        else:
            for i, p in enumerate(fs):
                a = parse_date(p.get("from_date"))
                b = parse_date(p.get("to_date")) if p.get("to_date") is not None else None
                if a is None:
                    rule[f"free_supercharging.{i}.from_date"] = "A date as YYYY-MM-DD."
                elif p.get("to_date") is not None and b is None:
                    rule[f"free_supercharging.{i}.to_date"] = "A date as YYYY-MM-DD, or null for no end."
                elif b is not None and b < a:
                    rule[f"free_supercharging.{i}.to_date"] = "Must not be before from_date."
    if body.get("default_price_per_kwh") is not None:
        _default_price_rule(body["default_price_per_kwh"], rule, bad)
    _num_rule(body, "capacity_new_kwh", 10, 250, rule, bad)
    _num_rule(body, "pack_new_kwh", 10, 250, rule, bad)
    if body.get("pack_replaced_date") is not None and parse_date(body["pack_replaced_date"]) is None:
        rule["pack_replaced_date"] = "A date as YYYY-MM-DD, or null for not replaced."
    _num_rule(body, "full_range_new_km", 100, 1500, rule, bad)
    _num_rule(body, "warranty_threshold_pct", 50, 95, rule, bad, allow_null=False)
    _num_rule(body, "efficiency_wh_per_km_override", 60, 500, rule, bad)
    _num_rule(body, "pack_new_kwh", 10, 250, rule, bad)
    if body.get("pack_replaced_date") is not None:
        v = body["pack_replaced_date"]
        if not isinstance(v, str):
            bad["pack_replaced_date"] = "Must be a date as YYYY-MM-DD or null."
        elif parse_date(v) is None:
            rule["pack_replaced_date"] = "A date as YYYY-MM-DD."
    if "software_version_typed" in body:
        v = body["software_version_typed"]
        import re
        if v is not None and not isinstance(v, str):
            bad["software_version_typed"] = "Must be text or null."
        elif v is not None and not re.fullmatch(r"\d{4}\.\d{1,2}(\.\d{1,3}){0,2}", v.strip()):
            rule["software_version_typed"] = "A version such as 2026.38.4."
    if "import_gap_fill" in body and body["import_gap_fill"] is not None and not isinstance(body["import_gap_fill"], bool):
        bad["import_gap_fill"] = "Must be true, false or null (the server's setting)."
    if "source_order" in body and body["source_order"] is not None and not SRC.order_valid(body["source_order"]):
        rule["source_order"] = SRC.ORDER_RULE
    if bad:
        raise bad_input(bad)
    if rule:
        raise invalid(rule)
    reorder = "source_order" in body and SRC.order_of(st, req.car) != (body["source_order"] or SRC.DEFAULT_ORDER)
    h = req.history()
    a = h.first_ms if h is not None and h.first_ms is not None else req.now - 30 * 86_400_000
    with st.lock:
        s = st.car_settings[req.car]
        for k, v in body.items():
            if k == "version":
                continue
            if k == "battery_chemistry":
                s["battery_chemistry"] = dict(s["battery_chemistry"], setting=v["setting"])
            elif k == "software_version_typed":
                s[k] = v.strip() if isinstance(v, str) else None
            elif k == "default_price_per_kwh":
                s[k] = {"value": float(v["value"]), "currency": v["currency"].upper()} if v is not None else None
            elif k == "source_order":
                # the default is kept as null, so a car follows a later change of the default
                s[k] = list(v) if v is not None and v != SRC.DEFAULT_ORDER else None
            else:
                s[k] = v
        s["version"] += 1
        if reorder:
            # the server fuses the car's rows again in the new order: its health says rebuild_pending for a moment
            st.rebuilds.setdefault(req.car, []).append({
                "id": st.new_id("rb"), "from_ms": a, "to_ms": req.now, "at_ms": req.now, "took_ms": 3000,
                "pending_until": req.now + 3000, "kinds": ["drives", "charges", "states", "visits"],
                "cause": "source_order"})
    st.record(req.account, "car-settings-changed", f"car:{req.car}",
              "Changed " + ", ".join(sorted(k for k in body if k != "version")) + ".")
    if reorder:
        req.app.events.publish("rows", {"car": req.car, "kinds": ["drives", "charges", "states", "visits"],
                                        "from_ms": a, "to_ms": req.now}, car=req.car)
    return get_settings(req)


# ------------------------------------------------------------------ upload tokens and CAN keys


def _car_change_words(old_name, body):
    """The server's words for a car change: "Renamed Comet to Comet II", "Changed Comet's time zone to
    America/Chicago", "Changed Comet's: sold, paint"."""
    parts = []
    name = old_name
    if "name" in body and body["name"].strip() != old_name:
        name = body["name"].strip()
        parts.append(f"Renamed {old_name} to {name}")
    if "time_zone" in body:
        parts.append(f"Changed {name}'s time zone to {body['time_zone'] or 'automatic'}")
    rest = [k for k in sorted(body) if k not in ("version", "name", "time_zone", "config")]
    rest += sorted(k for k in (body.get("config") or {}))
    if rest:
        parts.append(f"Changed {name}'s " + ", ".join(k.replace("_", " ") for k in rest))
    return ("; ".join(parts) or f"Saved {name} unchanged") + "."


def list_tokens(req):
    rows = [_tok_out(req.store, t) for t in req.store.tokens.get(req.car, [])]
    items, nxt = page(req, rows, key=lambda t: (t["created_ms"], t["id"]), reverse=True)
    return {"car": req.car, "time_zone": req.tz(), "items": items, "next": nxt}


def create_token(req):
    st = req.store
    tok = {"id": st.new_id("t"), "created_ms": req.now, "revoked": False, "key": None, "last_upload_ms": None}
    with st.lock:
        st.tokens.setdefault(req.car, []).append(tok)
    st.record(req.account, "token-created", f"token:{req.car}:{tok['id']}",
              f"Made the upload token {tok['id']} for {st.car_meta(req.car)['name']}.")
    out = _tok_out(st, tok)
    out["secret"] = "smu_" + secrets.token_urlsafe(24)
    out["app"] = bridge_fields(req, out["secret"])
    out["car"] = req.car
    out["_created"] = True
    return out


def _token(req):
    for t in req.store.tokens.get(req.car, []):
        if t["id"] == req.params["token"]:
            return t
    raise not_found("no_such_token", "There is no such upload token for this car.")


def get_token(req):
    return _tok_out(req.store, _token(req), car=req.car)


def revoke_token(req):
    t = _token(req)
    with req.store.lock:
        t["revoked"] = True
    req.store.record(req.account, "token-revoked", f"token:{req.car}:{t['id']}",
                     f"Revoked the upload token {t['id']} of {req.store.car_meta(req.car)['name']}.")
    return _tok_out(req.store, t, car=req.car)


def list_new_tokens(req):
    rows = [_tok_out(req.store, t, car=None) for t in req.store.new_car_tokens]
    items, nxt = page(req, rows, key=lambda t: (t["created_ms"], t["id"]), reverse=True)
    return {"items": items, "next": nxt}


def create_new_token(req):
    st = req.store
    tok = {"id": st.new_id("t"), "created_ms": req.now, "revoked": False, "key": None, "last_upload_ms": None}
    with st.lock:
        st.new_car_tokens.append(tok)
    st.record(req.account, "token-created", f"token:{tok['id']}", f"Made the upload token {tok['id']} for a new car.")
    out = _tok_out(st, tok, car=None)
    out["secret"] = "smu_" + secrets.token_urlsafe(24)
    out["app"] = bridge_fields(req, out["secret"])
    out["_created"] = True
    return out


def revoke_new_token(req):
    t = next((x for x in req.store.new_car_tokens if x["id"] == req.params["token"]), None)
    if t is None:
        raise not_found("no_such_token", "There is no such upload token waiting for a new car.")
    with req.store.lock:
        t["revoked"] = True
    req.store.record(req.account, "token-revoked", f"token:{t['id']}", f"Revoked the upload token {t['id']} of a new car.")
    return _tok_out(req.store, t, car=None)


DEVICE_NAME_MAX = 60


def device_label(k):
    """The key's chosen name, else the server's automatic label: "CAN Bridge key" and the key's short id."""
    if k.get("device"):
        return k["device"]
    tail = k["key"].rsplit("_", 1)[-1] or k["key"]
    return f"CAN Bridge key {tail[:6]}"


def key_out(k):
    return dict(k, device=k.get("device"), device_label=device_label(k))


def _tok_out(st, t, **extra):
    """A token row; one tied to a key also names its device, as the key rows do."""
    out = dict(t, **extra)
    k = next((x for x in st.keys if x["key"] == t.get("key")), None) if t.get("key") else None
    out["device"] = k.get("device") if k else None
    out["device_label"] = device_label(k) if k else None
    return out


def list_keys(req):
    assigned = req.bool_param("assigned")
    st = req.store
    rows = [k for k in st.keys if (k["car"] is None or st.sees(req.account, k["car"]))]
    if assigned is not None:
        rows = [k for k in rows if (k["car"] is not None) == assigned]
    car = req.q("car")
    if car:
        if not req.ds.exists(car) or not st.sees(req.account, car):
            raise not_found("no_such_car", "There is no such car.")
        rows = [k for k in rows if k["car"] == car]
    items, nxt = page(req, [key_out(k) for k in rows], key=lambda k: (k["first_seen_ms"], k["key"]), reverse=True)
    return {"items": items, "next": nxt}


def patch_key(req):
    st = req.store
    k = next((x for x in st.keys if x["key"] == req.params["key"]), None)
    if k is None:
        raise not_found("no_such_key", "There is no such CAN Bridge key.")
    body = req.json()
    bad, rule = {}, {}
    for f in body:
        if f not in ("car", "preferred", "name"):
            bad[f] = "Unknown field."
    if "name" in body and body["name"] is not None and not isinstance(body["name"], str):
        bad["name"] = "Must be text or null."
    if "car" in body and body["car"] is not None and not isinstance(body["car"], str):
        bad["car"] = "Must be a car id or null."
    if "preferred" in body and not isinstance(body["preferred"], bool):
        bad["preferred"] = "Must be true or false."
    if bad:
        raise bad_input(bad)
    if body.get("car") is not None and (not req.ds.exists(body["car"]) or not st.sees(req.account, body["car"])):
        rule["car"] = "No such car."
    new_name = body.get("name").strip() if isinstance(body.get("name"), str) else None
    if "name" in body and body["name"] is not None and not 1 <= len(new_name) <= DEVICE_NAME_MAX:
        rule["name"] = f"A name has 1 to {DEVICE_NAME_MAX} characters."
    elif new_name and any(ord(ch) < 32 or 127 <= ord(ch) < 160 for ch in new_name):
        rule["name"] = "A name is one line of text."
    if rule:
        raise invalid(rule)
    was = k["car"]
    old_name = k.get("device")
    with st.lock:
        if "car" in body:
            k["car"] = body["car"]
            if body["car"] is None:
                k["preferred"] = False
            else:
                k["logs_waiting"] = 0
        if body.get("preferred") and k["car"]:
            for other in st.keys:
                if other["car"] == k["car"]:
                    other["preferred"] = other is k
        elif body.get("preferred") is False:
            k["preferred"] = False
        if "name" in body:
            k["device"] = new_name
    name = lambda c: st.car_meta(c)["name"] if c else "no car"  # noqa: E731
    now_car = k["car"]
    if "car" in body and was != now_car:
        if was:
            st.record(req.account, "key-unassigned", f"key:{was}:{k['key']}",
                      f"Moved the CAN Bridge key {k['key']} from {name(was)} to {name(now_car)}.", car=was)
        if now_car:
            st.record(req.account, "key-assigned", f"key:{now_car}:{k['key']}",
                      f"Moved the CAN Bridge key {k['key']} from {name(was)} to {name(now_car)}.", car=now_car)
    elif "car" in body and now_car:
        st.record(req.account, "key-assigned", f"key:{now_car}:{k['key']}",
                  f"Kept the CAN Bridge key {k['key']} with {name(now_car)}.", car=now_car)
    if "preferred" in body and now_car:
        st.record(req.account, "key-preferred", f"key:{now_car}:{k['key']}",
                  f"Made the CAN Bridge key {k['key']} {name(now_car)}'s preferred one." if k["preferred"] else
                  f"The CAN Bridge key {k['key']} is no longer {name(now_car)}'s preferred one.", car=now_car)
    if "name" in body and new_name != old_name:
        st.record(req.account, "can-key-renamed", f"key:{now_car}:{k['key']}" if now_car else f"key:{k['key']}",
                  f"Named the CAN Bridge key {k['key']} \u201c{new_name}\u201d." if new_name else
                  f"Cleared the name of the CAN Bridge key {k['key']}.", car=now_car)
    return key_out(k)


def bridge_fields(req, secret):
    from .integrations import bridge_fields as f
    return f(req, secret)


def setup_status(req):
    st = req.store
    acc = req.account
    cars = st.visible_car_count(acc)
    toks = [t for c, ts in st.tokens.items() if st.sees(acc, c) for t in ts if not t["revoked"]]
    # a watch account hears only about its own cars: no new-car tokens, no keys waiting for a car
    admin = req.is_admin
    if admin:
        toks += [t for t in st.new_car_tokens if not t["revoked"]]
    used = [t["last_upload_ms"] for t in toks if t["last_upload_ms"]]
    first = min((t["created_ms"] for t in toks if t["last_upload_ms"]), default=None)
    waiting = [k for k in st.keys if k["car"] is None] if admin else []
    trust = sum(k["logs_waiting"] for k in st.keys if k["car"] is not None and st.sees(acc, k["car"]))
    fixes = []
    if not toks:
        fixes.append({"cause": "no_token", "link": "/settings/cars"})
    elif not used:
        fixes.append({"cause": "token_unused", "link": "/settings/cars"})
    if waiting:
        fixes.append({"cause": "key_waiting", "link": "/settings/cars", "key": waiting[0]["key"]})
    if trust:
        fixes.append({"cause": "logs_waiting_trust", "link": "/settings/cars"})
    from .imports import running_import
    running = running_import(req)
    if running:
        fixes.append({"cause": "import_running", "link": "/settings/import", "import": running["id"]})
    return {"cars": cars, "tokens": {"count": len(toks), "first_used_ms": first}, "keys_waiting": len(waiting),
            "logs_waiting_trust": trust, "import_running": running is not None,
            # the Tesla accounts are an admin's to read
            "tesla_accounts": len(st.tesla["accounts"]) if admin else None, "fixes": fixes}


def register(router):
    router.add("/api/v1/cars", GET=list_cars)
    router.add("/api/v1/fleet/summary", GET=fleet_summary)
    router.add("/api/v1/fleet/attention", GET=fleet_attention)
    router.add("/api/v1/cars/{car}", GET=car_detail, PATCH=patch_car, admin=("PATCH",))
    router.add("/api/v1/cars/{car}/settings", GET=get_settings, PATCH=patch_settings, admin=("PATCH",))
    router.add("/api/v1/cars/{car}/upload-tokens", GET=list_tokens, POST=create_token, admin=("GET", "POST"))
    router.add("/api/v1/cars/{car}/upload-tokens/{token}", GET=get_token, DELETE=revoke_token,
               admin=("GET", "DELETE"))
    router.add("/api/v1/upload-tokens", GET=list_new_tokens, POST=create_new_token, admin=("GET", "POST"))
    router.add("/api/v1/upload-tokens/{token}", DELETE=revoke_new_token, admin=("DELETE",))
    router.add("/api/v1/can-keys", GET=list_keys, admin=("GET",))
    router.add("/api/v1/can-keys/{key}", PATCH=patch_key, admin=("PATCH",))
    router.add("/api/v1/setup/status", GET=setup_status)

