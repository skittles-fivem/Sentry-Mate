"""Places (docs/ui-api.md, "Places"): the list, create, edit, delete, a preview of what a change
would do to history, repricing past charges, suggestions and the place at a point.

Names change history at once (every drive, charge, stop and timeline row shows a place as it is
now). Costs do not: before a change, charges that ended keep the price they had (placing.freeze_costs),
and POST /places/{place}/reprice applies the place's current price to past charges on request.
"""

from __future__ import annotations

import copy
import math

from .. import derive, placing
from ..conventions import page
from ..dataset import places as P
from ..errors import bad_input, conflict, invalid, not_found
from ..rows import charge_cost
from ..util import haversine_m, r, valid_zone
from .charges import CURRENCIES

KINDS = ("home", "work", "charger", "service", "other")
FIELDS = ("name", "kind", "lat", "lon", "radius_m", "time_zone", "price")
DAYS_RX = ("mon", "tue", "wed", "thu", "fri", "sat", "sun")


# ------------------------------------------------------------------ counts per place


def _cars(req):
    return [c for c in req.ds.car_ids_featured() if req.store.sees(req.account, c)]


def place_counts(req):
    """{place id: {drives, charges, visits, last_visit_ms}} over the cars this account sees."""
    cars = tuple(_cars(req))
    key = (cars, req.store.places_rev, tuple(derive.started(req.ds.history(c), req.now) for c in cars))
    return derive.memo(req.app, "place_counts", key, lambda: _counts(req, cars))


def _counts(req, cars):
    out = {}

    def at(pid):
        return out.setdefault(pid, {"drives": 0, "charges": 0, "visits": 0, "last_visit_ms": None})

    for cid in cars:
        h = req.ds.history(cid)
        for d in h.drives:
            if d.start > req.now or not h.shown(d):
                continue
            a, b = placing.drive_pids(req.app, d)
            if d.cut_ms is not None:
                b = None
            for pid in {a, b} - {None}:
                at(pid)["drives"] += 1
        for c in h.charges:
            if c.start <= req.now and h.shown(c):
                pid = placing.piece_pid(req.app, c)
                if pid:
                    at(pid)["charges"] += 1
        for v in derive.visits(req.app, h, req.now):
            if v["pid"]:
                e = at(v["pid"])
                e["visits"] += 1
                last = v["left_ms"] if v["left_ms"] is not None else req.now
                e["last_visit_ms"] = last if e["last_visit_ms"] is None else max(e["last_visit_ms"], last)
    return out


def view(req, p, counts=None):
    counts = place_counts(req) if counts is None else counts
    c = counts.get(p["id"], {"drives": 0, "charges": 0, "visits": 0, "last_visit_ms": None})
    return {"id": p["id"], "name": p["name"], "kind": p["kind"], "lat": p["lat"], "lon": p["lon"],
            "radius_m": p["radius_m"], "time_zone": p["time_zone"], "address": p["address"],
            "price": copy.deepcopy(p.get("price")),
            "counts": {"drives": c["drives"], "charges": c["charges"], "visits": c["visits"]},
            "last_visit_ms": c["last_visit_ms"], "created_ms": p.get("created_ms"), "version": p["version"]}


def _visible(req, p, counts):
    """Admins see every place; a watch account only the places its cars have used."""
    if req.is_admin:
        return True
    c = counts.get(p["id"])
    return bool(c and (c["drives"] or c["charges"] or c["visits"]))


def _car_counts(req, cid):
    key = (cid, req.store.places_rev, derive.started(req.ds.history(cid), req.now))
    return derive.memo(req.app, "place_counts_car", key, lambda: _counts(req, (cid,)))


def list_places(req):
    search = (req.q("search") or "").strip().lower()
    kind = req.enum("kind", KINDS)
    sort = req.enum("sort", ("name", "visits", "drives", "last_visit", "created"), "name")
    counts = place_counts(req)
    car = req.q("car")
    shown = counts
    if car:  # counts, last visit and the order are that car's; every visible place is still listed
        if not req.ds.exists(car) or not req.store.sees(req.account, car):
            raise not_found("no_such_car", "There is no such car.")
        shown = _car_counts(req, car)
    rows = []
    for p in placing.all_places(req.app):
        if not _visible(req, p, counts):
            continue
        if kind and p["kind"] != kind:
            continue
        if search and search not in " ".join(filter(None, [p["name"], p["address"].get("short"),
                                                            p["address"].get("locality")])).lower():
            continue
        rows.append(view(req, p, shown))
    n = lambda x: int(x["id"][1:])  # noqa: E731
    keyf = {"name": lambda x: (x["name"].lower(), n(x)),
            "visits": lambda x: (-x["counts"]["visits"], x["name"].lower(), n(x)),
            "drives": lambda x: (-x["counts"]["drives"], -x["counts"]["visits"], x["name"].lower(), n(x)),
            "last_visit": lambda x: (-(x["last_visit_ms"] or 0), n(x)),
            "created": lambda x: (-(x["created_ms"] or 0), n(x))}[sort]
    items, nxt = page(req, rows, key=keyf)
    out = {"items": items, "next": nxt, "total": len(rows)}
    if car:
        out["car"] = car
    return out


def _place(req):
    p = placing.place_obj(req.app, req.params["place"])
    if p is None:
        raise not_found("no_such_place", "There is no such place.")
    if not req.is_admin and not _visible(req, p, place_counts(req)):
        raise not_found("no_such_place", "There is no such place.")
    return p


def get_place(req):
    return view(req, _place(req))


# ------------------------------------------------------------------ checking a place


def _hm_ok(s):
    if not isinstance(s, str) or len(s) != 5 or s[2] != ":":
        return False
    try:
        h, m = int(s[:2]), int(s[3:])
    except ValueError:
        return False
    return 0 <= h <= 24 and 0 <= m < 60 and (h < 24 or m == 0)


def _days_ok(s):
    if s == "all":
        return True
    if not isinstance(s, str) or not s:
        return False
    for part in s.split(","):
        ws = part.strip().split("-")
        if not 1 <= len(ws) <= 2 or any(w not in DAYS_RX for w in ws):
            return False
    return True


def _num(v):
    return isinstance(v, (int, float)) and not isinstance(v, bool) and math.isfinite(v)


def check_price(price, bad, rule):
    if price is None:
        return None
    if not isinstance(price, dict):
        bad["price"] = "Must be an object or null."
        return None
    known = ("currency", "per_kwh", "per_minute", "rates", "session_fee")
    for k in price:
        if k not in known:
            bad[f"price.{k}"] = "Unknown field."
    cur = price.get("currency")
    if not isinstance(cur, str):
        bad["price.currency"] = "Required: a currency code such as USD."
    elif cur not in CURRENCIES:
        rule["price.currency"] = "A currency code such as USD, CAD or EUR."
    for k in ("per_kwh", "per_minute"):  # negative prices are allowed (some tariffs pay you)
        v = price.get(k)
        if v is not None and not _num(v):
            bad[f"price.{k}"] = "Must be a number or null."
        elif v is not None and not -10 <= v <= 100:
            rule[f"price.{k}"] = "Must be between -10 and 100."
    fee = price.get("session_fee")
    if fee is not None and not _num(fee):
        bad["price.session_fee"] = "Must be a number or null."
    elif fee is not None and not 0 <= fee <= 1000:
        rule["price.session_fee"] = "Must be between 0 and 1000."
    rates = price.get("rates", [])
    if rates is None:
        rates = []
    if not isinstance(rates, list) or not all(isinstance(x, dict) for x in rates):
        bad["price.rates"] = "Must be a list of rates."
        rates = []
    elif len(rates) > 24:
        rule["price.rates"] = "At most 24 rates."
    clean = []
    for i, rt in enumerate(rates):
        p = f"price.rates.{i}"
        for k in rt:
            if k not in ("name", "per_kwh", "per_minute", "days", "from", "to"):
                bad[f"{p}.{k}"] = "Unknown field."
        if not isinstance(rt.get("name"), str) or not 1 <= len(rt.get("name", "").strip()) <= 40:
            rule[f"{p}.name"] = "1 to 40 characters."
        for k in ("per_kwh", "per_minute"):
            v = rt.get(k)
            if v is not None and not _num(v):
                bad[f"{p}.{k}"] = "Must be a number or null."
        if not _days_ok(rt.get("days", "all")):
            rule[f"{p}.days"] = "Days such as mon-fri, sat,sun or all."
        for k in ("from", "to"):
            if not _hm_ok(rt.get(k, "00:00" if k == "from" else "24:00")):
                rule[f"{p}.{k}"] = "A time as HH:MM (24:00 for the end of the day)."
        clean.append({"name": str(rt.get("name", "")).strip(), "per_kwh": rt.get("per_kwh"),
                      "per_minute": rt.get("per_minute"), "days": rt.get("days", "all"),
                      "from": rt.get("from", "00:00"), "to": rt.get("to", "24:00")})
    if price.get("per_kwh") is None and price.get("per_minute") is None and not clean and fee is None:
        rule["price"] = "Give a price per kWh, per minute, a rate or a session fee, or send null for no price."
    return {"currency": cur, "per_kwh": price.get("per_kwh"), "per_minute": price.get("per_minute"), "rates": clean,
            "session_fee": fee}


def check_place(req, body, base=None):
    """The place `body` describes (merged into `base` for an edit), or 400/422 with fields."""
    bad, rule = {}, {}
    for k in body:
        if k not in FIELDS + ("version", "id"):
            bad[k] = "Unknown field."
    if base is None:
        for k in ("name", "lat", "lon"):
            if k not in body:
                bad[k] = "Required."
    p = copy.deepcopy(base) if base else {"kind": "other", "radius_m": 100, "price": None}
    if "name" in body:
        if not isinstance(body["name"], str):
            bad["name"] = "Must be text."
        elif not 1 <= len(body["name"].strip()) <= 64:
            rule["name"] = "1 to 64 characters."
        elif any(ord(c) < 32 for c in body["name"]):
            rule["name"] = "No control characters."
        else:
            p["name"] = body["name"].strip()
    if "kind" in body:
        if body["kind"] not in KINDS:
            rule["kind"] = "Must be home, work, charger, service or other."
        else:
            p["kind"] = body["kind"]
    for k, lo, hi in (("lat", -90, 90), ("lon", -180, 180), ("radius_m", 10, 5000)):
        if k in body:
            v = body[k]
            if not _num(v):
                bad[k] = "Must be a number."
            elif not lo <= v <= hi:
                rule[k] = f"Must be between {lo} and {hi}" + (" m." if k == "radius_m" else ".")
            else:
                p[k] = round(float(v), 6) if k != "radius_m" else int(round(v))
    if "time_zone" in body:
        if not isinstance(body["time_zone"], str):
            bad["time_zone"] = "Must be text."
        elif not valid_zone(body["time_zone"]):
            rule["time_zone"] = "Not a known time zone (for example America/Chicago)."
        else:
            p["time_zone"] = body["time_zone"]
    if "price" in body:
        p["price"] = check_price(body["price"], bad, rule)
    if bad:
        raise bad_input(bad)
    if rule:
        raise invalid(rule)
    if "time_zone" not in p or (base is None and "time_zone" not in body):
        near = min(P.PLACES, key=lambda q: haversine_m(p["lat"], p["lon"], q["lat"], q["lon"]))
        p["time_zone"] = near["time_zone"]
    if base is None or any(k in body for k in ("lat", "lon")):
        p["address"] = placing.new_place_address(p["lat"], p["lon"])
    return p


# ------------------------------------------------------------------ what a change does


def _points(req):
    """Every drive end, charge and stop of the visible cars: (kind, id, lat, lon, base pid, piece)."""
    out = []
    for cid in _cars(req):
        h = req.ds.history(cid)
        for d in h.drives:
            if d.start > req.now or not h.shown(d):
                continue
            la, lo, _, _ = d.route.at(0)
            out.append(("drive", d.id, la, lo, d.from_pid, d, h))
            if d.cut_ms is None and not (d.start <= req.now < d.end):
                la, lo, _, _ = d.route.at(d.length_m)
                out.append(("drive", d.id, la, lo, d.to_pid, d, h))
        for c in h.charges:
            if c.start <= req.now and h.shown(c):
                out.append(("charge", c.id, c.lat, c.lon, c.pid, c, h))
    return out


def _pinfo(p):
    return {k: p[k] for k in ("id", "name", "kind", "time_zone")} if p else None


def effect(req, old_places, new_places, focus):
    """What changes between two place sets, for the history near the edited place.

    Returns ({drives_named, drives_unnamed, charges_named, charges_unnamed, charges_repriced, examples},
    the charge pieces whose place or price changes, to freeze their costs before saving).
    `charges_repriced` counts the past charges a reprice after saving would change."""
    circles = [(p["lat"], p["lon"], p["radius_m"]) for p in focus if p]
    focus_ids = {p["id"] for p in focus if p}
    named, unnamed, c_named, c_unnamed, repriced = set(), set(), set(), set(), set()
    affected, examples, example_rows = [], [], []
    st = req.store

    def note(kind, rid, piece, h, change):
        if len(example_rows) < 8 and not any(x["id"] == rid for x in example_rows):
            example_rows.append({"kind": kind, "id": rid, "car": h.car.id, "start_ms": piece.start, "change": change})

    for kind, rid, lat, lon, base, piece, h in _points(req):
        la, lo = placing.stop_point(lat, lon, base)
        if not any(haversine_m(la, lo, a, b) <= rr + 1 for a, b, rr in circles):
            continue
        before = placing.match_in(old_places, la, lo)
        after = placing.match_in(new_places, la, lo)
        bid, aid = (before or {}).get("id"), (after or {}).get("id")
        moved = bid != aid
        if kind == "drive":
            if moved:
                if aid in focus_ids:
                    named.add(rid)
                if bid in focus_ids:
                    unnamed.add(rid)
                if len(examples) < 5 and rid not in examples:
                    examples.append(rid)
                note("drive", rid, piece, h, "named" if aid in focus_ids else "unnamed")
            continue
        if moved:
            if aid in focus_ids:
                c_named.add(rid)
            if bid in focus_ids:
                c_unnamed.add(rid)
            if len(examples) < 5:
                examples.append(rid)
            note("charge", rid, piece, h, "named" if aid in focus_ids else "unnamed")
        if moved or (bid in focus_ids and (before or {}).get("price") != (after or {}).get("price")):
            affected.append(piece)
        if aid in focus_ids and st.costs.get(rid) is None and piece.source == "can" and piece.data_end <= req.now:
            f = piece.figures()
            cur, _, _ = charge_cost(req.app, h.car.id, piece, f, req.now)
            new, _, _ = charge_cost(req.app, h.car.id, piece, f, req.now, pricing=(_pinfo(after), after.get("price")))
            if (cur or {}).get("amount") != (new or {}).get("amount"):
                repriced.add(rid)
                if not moved:
                    note("charge", rid, piece, h, "repriced")
    return {"drives_named": len(named), "drives_unnamed": len(unnamed), "charges_named": len(c_named),
            "charges_unnamed": len(c_unnamed), "charges_repriced": len(repriced), "examples": examples,
            "example_rows": example_rows}, affected


def _apply(req, action, pid, new):
    """Change the place table: freeze the costs the change would touch, then save it."""
    st = req.store
    with st.lock:
        old_list = list(st.places.values())
        new_tab = dict(st.places)
        if new is None:
            new_tab.pop(pid, None)
        else:
            new_tab[pid] = new
        focus = [st.places.get(pid), new]
        summary, affected = effect(req, old_list, list(new_tab.values()), focus)
        placing.freeze_costs(req.app, affected)
        st.places = new_tab
        st.places_rev += 1
    return summary


def create_place(req):
    st = req.store
    body = req.json()
    p = check_place(req, body)
    p["id"] = st.new_id("p")
    p["version"] = 1
    p["created_ms"] = req.now
    summary = _apply(req, "create", p["id"], p)
    st.record(req.account, "place.create", f"place:{p['id']}",
              f"Added the place {p['name']} ({summary['drives_named']} drives and {summary['charges_named']} "
              "charges now carry its name).")
    _rows_event(req)
    out = view(req, p)
    out["effect"] = summary
    out["_created"] = True
    return out


def patch_place(req):
    st = req.store
    cur = _place(req)
    body = req.json()
    req.version_check(cur["version"])
    p = check_place(req, body, base=cur)
    p["version"] = cur["version"] + 1
    summary = _apply(req, "update", cur["id"], p)
    what = ", ".join(sorted(k for k in body if k != "version")) or "nothing"
    st.record(req.account, "place.update", f"place:{cur['id']}", f"Changed {what} of the place {p['name']}.")
    _rows_event(req)
    out = view(req, p)
    out["effect"] = summary
    return out


def delete_place(req):
    st = req.store
    cur = _place(req)
    v = req.int_param("version", None, 0)
    if v is not None and v != cur["version"]:
        from ..errors import stale_version
        raise stale_version(cur["version"])
    summary = _apply(req, "delete", cur["id"], None)
    st.record(req.account, "place.delete", f"place:{cur['id']}",
              f"Removed the place {cur['name']} ({summary['drives_unnamed']} drives lost the name).")
    _rows_event(req)
    return {"deleted": True, "id": cur["id"], "effect": summary}


def _rows_event(req):
    for cid in _cars(req):
        req.app.events.publish("rows", {"car": cid, "kinds": ["drives", "charges", "visits"], "from_ms": 0,
                                        "to_ms": req.now}, car=cid)


def preview(req):
    st = req.store
    body = req.json()
    for k in ("place", "id"):
        if body.get(k) is not None and not isinstance(body[k], str):
            raise bad_input({k: "Must be a place id."})
    counts = place_counts(req)

    def seen(p):  # a watch account previews only places its cars have used, like the list
        return p if p is not None and _visible(req, p, counts) else None

    if body.get("delete") is True:
        bad = {k: "Unknown field." for k in body if k not in ("place", "delete")}
        if bad:
            raise bad_input(bad)
        cur = seen(placing.place_obj(req.app, body.get("place")))
        if cur is None:
            raise invalid({"place": "No such place."})
        tab = {k: v for k, v in st.places.items() if k != cur["id"]}
        summary, _ = effect(req, list(st.places.values()), list(tab.values()), [cur])
        return dict(summary, place=cur["id"], delete=True)
    base = seen(placing.place_obj(req.app, body.get("id"))) if body.get("id") else None
    if body.get("id") and base is None:
        raise invalid({"id": "No such place."})
    draft = check_place(req, body, base=base)
    draft["id"] = base["id"] if base else "draft"
    tab = dict(st.places)
    tab[draft["id"]] = draft
    summary, _ = effect(req, list(st.places.values()), list(tab.values()), [base, draft])
    return dict(summary, place=base["id"] if base else None, delete=False)


def reprice(req):
    st = req.store
    pl = _place(req)
    body = req.json(required=False)
    bad = {k: "Unknown field." for k in body if k not in ("from_ms", "to_ms", "only_without_cost", "dry_run")}
    for k in ("from_ms", "to_ms"):
        if k in body and body[k] is not None and (not isinstance(body[k], int) or isinstance(body[k], bool)):
            bad[k] = "Must be epoch milliseconds."
    for k in ("only_without_cost", "dry_run"):
        if k in body and not isinstance(body[k], bool):
            bad[k] = "Must be true or false."
    if bad:
        raise bad_input(bad)
    a = body.get("from_ms") or 0
    b = body.get("to_ms") or req.now + 1
    if a >= b:
        raise invalid({"to_ms": "Must be after from_ms."})
    only = body.get("only_without_cost", False)
    dry = body.get("dry_run", False)
    considered, changed, examples, example_rows = 0, [], [], []
    before, after = {}, {}
    snap = {"place": {k: pl[k] for k in ("id", "name", "kind", "time_zone")}, "price": copy.deepcopy(pl.get("price")),
            "at_ms": req.now}
    with st.lock:
        for cid in _cars(req):
            h = req.ds.history(cid)
            for c in h.charges:
                if not (a <= c.start < b) or c.start > req.now or not h.shown(c) or c.source != "can":
                    continue
                if c.data_end > req.now or placing.piece_pid(req.app, c) != pl["id"] or st.costs.get(c.id):
                    continue
                f = c.figures()
                old, _, _ = charge_cost(req.app, cid, c, f, req.now)
                if only and old is not None:
                    continue
                considered += 1
                new, _, _ = charge_cost(req.app, cid, c, f, req.now, pricing=(snap["place"], snap["price"]))
                if not dry:
                    st.charge_price[c.id] = snap
                for box, cost in ((before, old), (after, new)):
                    if cost:
                        box[cost["currency"]] = box.get(cost["currency"], 0.0) + cost["amount"]
                if (old or {}).get("amount") != (new or {}).get("amount"):
                    changed.append(c.id)
                    if len(examples) < 5:
                        examples.append(c.id)
                    if len(example_rows) < 8:
                        example_rows.append({"kind": "charge", "id": c.id, "car": cid, "start_ms": c.start,
                                             "change": "repriced"})
        if not dry:
            st.rev += 1
    if not dry:
        st.record(req.account, "place.reprice", f"place:{pl['id']}",
                  f"Repriced {len(changed)} past charges at {pl['name']}.")
        _rows_event(req)
    money = lambda d: [{"currency": k, "amount": round(v, 2)} for k, v in sorted(d.items())]  # noqa: E731
    return {"place": pl["id"], "from_ms": a, "to_ms": b, "only_without_cost": only, "dry_run": dry,
            "charges": considered, "repriced": len(changed), "unchanged": considered - len(changed),
            "examples": examples, "example_rows": example_rows, "cost_before": money(before),
            "cost_after": money(after)}


# ------------------------------------------------------------------ suggestions and the place at a point


def suggestions(req):
    limit = req.int_param("limit", 10, 1, 100)
    groups = {}
    for cid in _cars(req):
        h = req.ds.history(cid)
        for v in derive.visits(req.app, h, req.now):
            if v["pid"] is not None:
                continue
            k = 1 / 0.0014
            key = (round(v["lat"] * k), round(v["lon"] * k * math.cos(math.radians(v["lat"]))))
            g = groups.setdefault(key, {"lat": 0.0, "lon": 0.0, "visits": 0, "dwell_ms": 0, "cars": set(),
                                        "address": v["address"], "last_visit_ms": 0})
            g["lat"] += v["lat"]
            g["lon"] += v["lon"]
            g["visits"] += 1
            left = v["left_ms"] if v["left_ms"] is not None else req.now
            g["dwell_ms"] += left - v["arrived_ms"]
            g["cars"].add(cid)
            g["last_visit_ms"] = max(g["last_visit_ms"], left)
    rows = []
    for g in groups.values():
        lat, lon = g["lat"] / g["visits"], g["lon"] / g["visits"]
        rows.append({"lat": round(lat, 6), "lon": round(lon, 6), "radius_m": 100, "visits": g["visits"],
                     "dwell_ms": g["dwell_ms"], "cars": sorted(g["cars"], key=lambda c: int(c[1:])),
                     "label": g["address"].get("short"), "address": g["address"], "last_visit_ms": g["last_visit_ms"],
                     "kind": "other"})
    rows.sort(key=lambda x: (-x["visits"], -x["dwell_ms"], x["lat"]))
    return {"items": rows[:limit], "next": None}


def place_at(req):
    lat = req.float_param("lat", None, -90, 90)
    lon = req.float_param("lon", None, -180, 180)
    if lat is None or lon is None:
        raise bad_input({k: "Required." for k, v in (("lat", lat), ("lon", lon)) if v is None})
    counts = place_counts(req)
    cands = []
    for p in placing.all_places(req.app):
        d = haversine_m(lat, lon, p["lat"], p["lon"])
        if d <= p["radius_m"] and _visible(req, p, counts):
            cands.append({"id": p["id"], "name": p["name"], "kind": p["kind"], "distance_m": r(d, 1),
                          "radius_m": p["radius_m"]})
    cands.sort(key=lambda x: (x["distance_m"], int(x["id"][1:])))
    best = placing.place_obj(req.app, cands[0]["id"]) if cands else None
    return {"lat": lat, "lon": lon, "place": view(req, best, counts) if best else None, "candidates": cands,
            "rule": "nearest_centre", "address": placing.full_address(req.app, lat, lon, None)}


def register(router):
    router.add("/api/v1/places", GET=list_places, POST=create_place, admin=("POST",))
    router.add("/api/v1/places/{place}", GET=get_place, PATCH=patch_place, DELETE=delete_place,
               admin=("PATCH", "DELETE"))
    router.add("/api/v1/places/preview", POST=preview)
    router.add("/api/v1/places/{place}/reprice", POST=reprice, admin=("POST",))
    router.add("/api/v1/places/suggestions", GET=suggestions)
    router.add("/api/v1/places/at", GET=place_at)


__all__ = ["register", "conflict"]
