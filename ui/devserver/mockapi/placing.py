"""Places as they are now: the dataset's places with the owner's changes (new, edited, deleted),
and which place a point of history belongs to.

Names are live: renaming, moving or deleting a place changes the name every drive, charge, stop
and timeline row shows at once. Costs are not: a charge keeps the cost it had when it ended
(see `freeze_costs`), until the owner reprices it (POST /places/{place}/reprice) or types a cost.

The match rule: of the places whose circle holds the point, the one with the nearest centre wins.
A piece of history that the dataset put at a place is matched from where the car really stopped
(the dataset place's own centre), so moving a circle a little never loses its drives by accident.
"""

from __future__ import annotations

import copy

from .dataset import places as P
from .dataset.segments import address_at
from .util import haversine_m


def seed_places():
    """The store's place table at start: every dataset place, version 1, priced from the start."""
    out = {}
    for p in P.PLACES:
        q = copy.deepcopy(p)
        q["version"] = 1
        q["created_ms"] = None
        out[p["id"]] = q
    return out


def all_places(app):
    return list(app.store.places.values())


def place_obj(app, pid):
    if not isinstance(pid, str):
        return None
    return app.store.places.get(pid)


def _num(pid):
    try:
        return int(pid[1:])
    except ValueError:
        return 0


def match_in(places, lat, lon):
    """The place a point is in among `places` (nearest centre among the circles that hold it)."""
    best, bd = None, None
    for p in places:
        d = haversine_m(lat, lon, p["lat"], p["lon"])
        if d <= p["radius_m"] and (bd is None or d < bd or (d == bd and _num(p["id"]) < _num(best["id"]))):
            best, bd = p, d
    return best


def stop_point(lat, lon, base_pid):
    """Where the car really was: the dataset place's centre when the dataset put the piece there."""
    if base_pid and base_pid in P.BY_ID:
        b = P.BY_ID[base_pid]
        return b["lat"], b["lon"]
    return lat, lon


def eff_pid(app, lat, lon, base_pid):
    """The id of the place a history point belongs to now (None: no place)."""
    st = app.store
    if base_pid is not None and st.places_rev == 0:
        return base_pid
    if lat is None or lon is None:
        return base_pid if st.places_rev == 0 else None
    la, lo = stop_point(lat, lon, base_pid)
    memo = app.cache.setdefault("place_match", {})
    if memo.get("_rev") != st.places_rev:
        memo.clear()
        memo["_rev"] = st.places_rev
    key = (round(la, 6), round(lo, 6))
    if key not in memo:
        p = match_in(st.places.values(), la, lo)
        memo[key] = p["id"] if p else None
    return memo[key]


def eff_place(app, lat, lon, base_pid):
    return place_obj(app, eff_pid(app, lat, lon, base_pid))


def drive_pids(app, d):
    """(start place id, end place id) of a drive now. A drive whose data stopped has no end place."""
    lat0, lon0, _, _ = d.route.at(0)
    lat1, lon1, _, _ = d.route.at(d.length_m)
    return eff_pid(app, lat0, lon0, d.from_pid), eff_pid(app, lat1, lon1, d.to_pid)


def piece_pid(app, s):
    """The place id of a charge, parked spell or update now."""
    return eff_pid(app, getattr(s, "lat", None), getattr(s, "lon", None), getattr(s, "pid", None))


def ref(p):
    return {"id": p["id"], "name": p["name"]} if p else None


def point(app, lat, lon, base_pid=None, label=None, at_place=True):
    """{ lat, lon, place, address, label } for a point of history, with places as they are now."""
    pl = eff_place(app, lat, lon, base_pid) if at_place else None
    addr = dict(pl["address"]) if pl else address_at(lat, lon)
    return {
        "lat": round(lat, 6), "lon": round(lon, 6),
        "place": ref(pl),
        "address": {k: addr.get(k) for k in ("short", "locality", "region", "country_code")},
        "label": label or (pl["name"] if pl else addr["short"]),
        "_address": addr,
    }


def full_address(app, lat, lon, base_pid=None):
    pl = eff_place(app, lat, lon, base_pid)
    return dict(pl["address"]) if pl else address_at(lat, lon)


# ------------------------------------------------------------------ costs that stay put


def pricing(app, c):
    """(place used for pricing or None, price or None) for a charge: its frozen price if it has one,
    else the place it is at now with that place's current price."""
    snap = app.store.charge_price.get(c.id)
    if snap is not None:
        return snap["place"], snap["price"]
    pl = place_obj(app, piece_pid(app, c))
    if pl is None:
        return None, None
    return {k: pl[k] for k in ("id", "name", "kind", "time_zone")}, pl.get("price")


def freeze_costs(app, affected):
    """Before a place change: charges that ended keep the price they were costed with."""
    st = app.store
    now = app.clock.now()
    for c in affected:
        if c.id in st.charge_price or c.source != "can" or c.data_end > now:
            continue
        place, price = pricing(app, c)
        st.charge_price[c.id] = {"place": place, "price": copy.deepcopy(price), "at_ms": now}


def new_place_address(lat, lon):
    a = address_at(lat, lon)
    a["source"] = "offline"
    return a
