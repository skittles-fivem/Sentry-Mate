"""Live values: what the car is doing at a moment, as the "now" object and the car list summary.

Values arrive about 60 to 90 s late (the CAN Bridge uploads a log a minute or so). Each value
carries its own `at_ms`: while the car sleeps, values keep the time they were last seen.
"""

from __future__ import annotations

from . import offcan, placing
from . import sources as S
from .util import DAY, HOUR, MIN, r

FLEET_STATE = {"drive": "driving", "charge": "charging", "update": "parked", "gap": "unknown"}


def W(v, at):
    return None if v is None else {"value": v, "at_ms": at}


def delay_s(t):
    return 60 + (t // 1000) % 30


def last_sample(h, tv):
    """(piece, sample time, values) for the newest moment at or before tv that has data."""
    t = tv
    for _ in range(12):
        s = h.seg_at(t)
        if s is None:
            return None, None, None
        v = s.values(t)
        if v is not None:
            return s, t, v
        if s.kind == "parked":
            _, sub = s.sub_at(t)
            t = max(s.start, sub[0]) - 1000
        elif s.kind == "drive":
            for a, b in s.drops:
                if a <= t < b:
                    t = a - 1000
                    break
            else:
                t = min(t, s.data_end) - 1000
        elif s.kind == "charge":
            for a, b, _k in s.gaps:
                if a <= t < b:
                    t = a - 1000
                    break
            else:
                t = min(t, s.data_end) - 1000
        else:
            t = s.start - 1000
    return None, None, None


# How long after a fingerprint update a live input's new version still names it.
INPUT_NAME_WINDOW_MS = 24 * HOUR


def _in_order(app, car_id, inputs):
    """`inputs` in the car's input order (`source_order`), highest priority first."""
    order = S.order_of(app.store, car_id)
    return sorted(set(inputs), key=lambda i: order.index(i) if i in order else len(order))


def input_versions(h):
    """[(input, version, first_ms)] in time order: each version a live input started reporting (made up, from the
    car spec's `input_versions`; a record stays whatever the input's state is now)."""
    got = getattr(h, "_input_versions", None)
    if got is None:
        from .dataset.segments import update_id
        car = h.car
        got = []
        for inp, ver, seq, after_h in getattr(car, "input_versions", None) or ():
            u = h.by_id.get(update_id(car.n, seq))
            if u is not None:
                got.append((inp, ver, u.end + int(after_h * HOUR)))
        got.sort(key=lambda x: x[2])
        h._input_versions = got
    return got


def input_named(app, h, from_ms, to_ms):
    """(version, input) a live input started reporting from `from_ms` until a day after `to_ms` that is not the one
    it said before `from_ms`, from the inputs in the car's order (`source_order`); else None."""
    seen = input_versions(h)
    until = to_ms + INPUT_NAME_WINDOW_MS
    for inp in _in_order(app, h.car.id, (s[0] for s in seen)):
        mine = [s for s in seen if s[0] == inp]
        before = next((s[1] for s in reversed(mine) if s[2] < from_ms), None)
        hit = next((s for s in mine if from_ms <= s[2] <= until), None)
        if hit is not None and hit[1] != before:
            return hit[1], inp
    return None


def teslamate_local_on(app, car_id):
    """Whether the owner's local TeslaMate is read for this car now: on, not paused, no trouble, the car matched."""
    t = getattr(app.store, "teslamate", None) or {}
    if not t.get("enabled") or t.get("paused") or t.get("mqtt_error") or t.get("mqtt_quiet"):
        return False
    return any(c.get("match") == car_id for c in t.get("cars") or [])


def live_version(app, h, t):
    """(version, read at, input) the car's live input reads now, or None: the newest version that input reported
    by t, while the input is read."""
    seen = [s for s in input_versions(h) if s[2] <= t]
    for inp in _in_order(app, h.car.id, (s[0] for s in seen)):
        mine = [s for s in seen if s[0] == inp]
        if not mine:
            continue
        if inp == "teslafi_api" and offcan.api_input(app, h.car.id) == "teslafi_api":
            return mine[-1][1], offcan.teslafi_read_ms(t), inp
        if inp == "teslamate_local" and teslamate_local_on(app, h.car.id):
            return mine[-1][1], t - 30_000, inp
    return None


def software_at(app, h, t):
    """The car's software at t (crates/engine query/software.rs `current`): the version typed on the newest update;
    else the live input's when it read it after the newest update started; else the newest update's; else the car
    settings' typed version."""
    ups = [u for u in h.updates if u.start <= t]
    newest = ups[-1] if ups else None
    nv, ns = update_version(app, newest) if newest else (None, None)
    end = newest.end if newest else None
    changed = end if newest and not nv else None
    if newest and ns == "owner":
        return {"version": nv, "version_source": "owner", "version_since_ms": end, "changed_since_ms": None}
    live = live_version(app, h, t)
    if live and (newest is None or live[1] >= newest.start):
        v, _at, inp = live
        if newest and (nv is None or nv == v):
            since = end
        else:
            since = next((s[2] for s in reversed(input_versions(h)) if s[0] == inp and s[1] == v), None)
        return {"version": v, "version_source": inp, "version_since_ms": since, "changed_since_ms": None}
    if nv:
        return {"version": nv, "version_source": ns, "version_since_ms": end, "changed_since_ms": None}
    if newest is None:
        ver, src = h.car.first_version
    else:
        ver, src = None, None
    typed = (app.store.car_settings.get(h.car.id) or {}).get("software_version_typed")
    if typed:
        ver, src = typed, "owner"
    return {"version": ver, "version_source": src, "version_since_ms": end if newest else h.first_ms,
            "changed_since_ms": changed}


def is_fingerprint(u):
    return u.source == "can" and u.detected_by == "fingerprint"


def update_version(app, u):
    """(version, version_source) of an update (crates/engine query/software.rs `version_of`): the owner's typed one
    (a name an owner cleared stays cleared); for one found from fingerprints, else the version a live input started
    reporting around it, else one learned from the same build; else the dataset's."""
    st = app.store
    if u.id in st.update_versions:
        own = st.update_versions[u.id]
        return (own, "owner") if own is not None else (None, None)
    if u.version_source == "owner":
        return u.version, "owner"
    if is_fingerprint(u):
        h = app.ds.history(u.car.id)
        named = input_named(app, h, u.start, u.end) if h is not None else None
        if named:
            return named
    learned = st.builds.get(u.build_id)
    if learned and learned["from_update"] != u.id:
        return learned["version"], "build_match"
    return u.version, u.version_source


def avg_wh_per_km(app, h, t):
    key = (h.car.id, t // DAY)
    cache = app.cache.setdefault("whkm", {})
    if key not in cache:
        e = d = 0.0
        for dr in h.drives:
            if t - 30 * DAY <= dr.start < t and dr.data_end <= t and dr.source == "can":
                f = dr.figures()
                e += f["energy"]
                d += f["dist_km"]
        cache[key] = e / d * 1000 if d > 5 else h.car.base_wh_per_km
    return cache[key]


def now_object(app, car_id, t):
    """The live "now" object for a car at virtual time t: every value names its input (`sources`), and `inputs`
    lists the car's live inputs with each one's newest value."""
    obj = _now_object(app, car_id, t)

    def mark(x, key=None):
        if isinstance(x, dict):
            if set(x) == {"value", "at_ms"}:
                x["sources"] = [S.value_ref(car_id, key)]
                return
            for k, v in x.items():
                mark(v, k)
        elif isinstance(x, list):
            for v in x:
                mark(v, key)

    for k, v in obj.items():
        mark(v, k)
    at = obj.get("at_ms")
    inputs = []
    if at is not None:
        inputs.append({**S.ref("smo"), "last_ms": at, "received_ms": None})
        if car_id in ("c1", "c2"):
            inputs.append({**S.ref("tesla_api"), "last_ms": at - 45_000, "received_ms": at - 40_000})
        elif offcan.api_input(app, car_id) == "teslafi_api":
            last, came = offcan.teslafi_times(app, car_id, t)
            inputs.append({**S.ref("teslafi_api"), "last_ms": last, "received_ms": came})
        if teslamate_local_on(app, car_id):
            inputs.append({**S.ref("teslamate_local"), "last_ms": t - 30_000, "received_ms": t - 29_000})
    # in the car's input order (`source_order`), highest priority first
    inputs.sort(key=S.rank(S.order_of(app.store, car_id)))
    obj["inputs"] = inputs
    obj["off_can"] = offcan.off_can(app, car_id, t, obj)
    return obj


def _now_object(app, car_id, t):
    h = app.ds.history(car_id)
    meta = app.store.car_meta(car_id)
    if h is None:
        return synthetic_now(app, car_id, t, meta)
    car = h.car
    legacy = car.profile == "tesla-sx-legacy"
    dl = delay_s(t)
    tv = t - dl * 1000
    cur = h.seg_at(tv)
    seg, ts, vals = last_sample(h, tv)
    silent = cur is not None and cur.kind == "gap" and cur.open_end
    received = (cur.start if silent else t - (t // 1000 % 12) * 1000) if cur is not None else None
    if vals is None:
        return {"car": car_id, "time_zone": meta["time_zone"], "state": "unknown", "since_ms": None, "at_ms": None,
                "received_ms": received, "delay_s": None, "stale": True, "buses": [], "driving": False}
    # state and since
    if silent and seg is not None and seg.kind == "parked":
        # nothing since the newest log, which ended parked: the car stays parked (or asleep, if that log closed quiet)
        _, sub = seg.sub_at(ts)
        state, since = ("asleep" if sub[2] == "asleep" else "parked"), max(seg.start, sub[0])
    elif cur is None or cur.kind == "gap":
        state, since = "unknown", cur.start if cur else None
    elif cur.kind == "parked":
        i, sub = cur.sub_at(tv)
        state = "asleep" if sub[2] == "asleep" else "parked"
        since = cur.start if state == "parked" else max(cur.start, sub[0])
        if state == "parked":  # since the last time it fell asleep or arrived
            for j in range(i, -1, -1):
                if cur.subs[j][2] == "asleep":
                    since = cur.subs[j][1]
                    break
            since = max(since, cur.start)
    else:
        state, since = FLEET_STATE[cur.kind], cur.start
    awake = state in ("driving", "charging", "parked")
    vbus_only = False
    reasons = []
    if cur is not None and cur.kind == "parked":
        _, sub = cur.sub_at(tv)
        vbus_only = sub[2] == "vbus"
        reasons = list(sub[3])
    elif cur is not None and cur.kind == "charge":
        reasons = ["charging"]
    bus_at = ts if awake else (ts if seg is not None else None)
    buses = [{"bus": "vehicle", "awake": awake, "at_ms": bus_at},
             {"bus": "chassis", "awake": awake and not vbus_only, "at_ms": bus_at}]
    odo = vals.get("odometer_km")
    if odo is None:
        odo = getattr(seg, "odo", None)
    lat, lon = vals.get("lat"), vals.get("lon")
    pl = None
    if lat is not None and seg.kind != "drive":
        pl = placing.eff_place(app, lat, lon, getattr(seg, "pid", None))
    addr = placing.full_address(app, lat, lon, getattr(seg, "pid", None) if seg.kind != "drive" else None) \
        if lat is not None else None
    ty_keys = ("tyre_fl_kpa", "tyre_fr_kpa", "tyre_rl_kpa", "tyre_rr_kpa")
    has_ty = vals.get("tyre_fl_kpa") is not None
    soc = vals["soc_pct"]
    usable = car.usable_kwh(ts)
    avg = avg_wh_per_km(app, h, ts)
    est = usable * soc / 100 * 1000 / avg if avg else None
    driving = seg.kind == "drive" and cur is seg
    speed = vals.get("speed_kph")
    flags_at = ts
    st_reasons = set(reasons)
    climate = "off"
    keep = "off"
    if "preconditioning_cabin" in st_reasons:
        climate = "precondition"
    elif driving or "keep_climate_dog" in st_reasons or "keep_climate_on" in st_reasons:
        climate = "on"
    if "keep_climate_dog" in st_reasons:
        keep = "dog"
    elif "keep_climate_camp" in st_reasons:
        keep = "camp"
    elif "keep_climate_on" in st_reasons:
        keep = "on"
    doors = {k: W(False, flags_at) for k in ("fl", "fr", "rl", "rr")}
    if "user_present" in st_reasons:
        doors["fl"] = W(True, flags_at)
    sentry = "sentry" in st_reasons
    reduced = bool(getattr(seg, "reduced_range", False)) if seg.kind == "drive" else (
        vals.get("battery_c") is not None and vals["battery_c"] < 0)
    flags = {
        "sentry": None if legacy else W(sentry, flags_at),
        "sentry_event_guess": None if legacy else W(sentry and (ts // MIN) % 37 == 0, flags_at),
        "service_mode": None if legacy else W(seg.kind == "parked" and seg.pid == "p18", flags_at),
        "user_present": W(driving or "user_present" in st_reasons, flags_at),
        "keep_climate": None if legacy else W(keep, flags_at),
        "climate": None if legacy else W(climate, flags_at),
        "battery_preconditioning": None if legacy else W(False, flags_at),
        "keep_warm": W("keep_warm" in st_reasons, flags_at),
        "accessory_power": None if legacy else W("accessory_power" in st_reasons, flags_at),
        "parking_brake": None if legacy else W(not driving, flags_at),
        "reduced_range": W(bool(reduced), flags_at),
        "doors": doors,
        "trunk": W(False, flags_at), "frunk": W(False, flags_at),
        "liftgate_deg": None if car.model != "Model Y" else W(0, flags_at),
        "charge_port_open": W(False, flags_at),
        "display_on": None if legacy else W(awake and not vbus_only, flags_at),
    }
    charge = charge_now(app, h, cur, seg, ts, tv, flags)
    drive = None
    if driving:
        f = seg.figures(tv)
        drive = {"drive_id": seg.id, "start_ms": seg.start, "distance_km": r(f["dist_km"], 3),
                 "energy_kwh": r(f["energy"], 3)}
    obj = {
        "car": car_id, "time_zone": meta["time_zone"],
        "state": state, "since_ms": since, "at_ms": ts, "received_ms": received, "delay_s": dl if not silent else None,
        "stale": (t - received) > HOUR if received else True,
        "buses": buses, "driving": driving,
        "gear": None if legacy and seg.source != "can" else W("D" if driving and speed else "P", ts),
        "speed_kph": W(speed if driving else 0.0, ts),
        "power_kw": W(vals.get("power_kw"), ts),
        "soc_pct": W(soc, ts), "soc_bms_pct": W(vals.get("soc_bms_pct"), ts),
        "usable_soc_pct": W(vals.get("usable_soc_pct"), ts),
        "range_km": W(vals.get("range_km"), ts), "ideal_range_km": W(vals.get("ideal_range_km"), ts),
        "est_range_km": W(r(est, 1), ts), "est_basis": "wh_per_km_30d",
        # the car's own estimate from an API input (the Tesla API's, else local TeslaMate's): none in the dev data
        "car_est_range_km": None,
        "energy_remaining_kwh": W(vals.get("energy_remaining_kwh"), ts),
        "odometer_km": W(odo if odo is None else round(odo, 3), ts),
        "outside_c": W(vals.get("outside_c"), ts), "cabin_c": W(vals.get("cabin_c"), ts),
        "battery_c": W(vals.get("battery_c"), ts),
        "setpoint_left_c": W(vals.get("setpoint_left_c"), ts), "setpoint_right_c": W(vals.get("setpoint_right_c"), ts),
        "lv_v": W(vals.get("lv_v"), ts),
        "location": {
            "lat": W(lat, ts), "lon": W(lon, ts), "accuracy_m": W(vals.get("accuracy_m"), ts),
            "elevation_m": W(vals.get("elevation_m"), ts), "heading_deg": W(vals.get("heading_deg"), ts),
            "place": {"id": pl["id"], "name": pl["name"]} if pl else None,
            "address": addr,
        },
        "tyres_kpa": [W(vals.get(k), ts - 20_000) for k in ty_keys] if has_ty else None,
        **tyres_now(car, meta, legacy, has_ty, [vals.get(k) for k in ty_keys], ts - 20_000, h.first_ms),
        "flags": flags,
        "charge": charge,
        "drive": drive,
        "awake_reasons": reasons if awake else [],
        "software": software_at(app, h, ts),
    }
    return obj


# made-up variety: c2's sensors are not mapped yet and the car sends no recommended pressure (the door placard's for
# its 18 in wheels is used, a size taken from its factory tyre until an admin types one); the other cars send theirs
# on CAN
PLACARD_CARS = {"c2": 18}


def tyres_now(car, meta, legacy, has_ty, kpa, tyres_at, rec_at):
    """The Now's tyre fields: whether the slots are mapped, the recommended pressures (typed, else CAN, else the
    door placard's), and the low flag (each tyre against its axle's, or the lower one while not mapped)."""
    cfg = meta.get("config", {})
    typed = [(cfg.get(k) or {}).get("value") if (cfg.get(k) or {}).get("source") == "owner" else None
             for k in ("tyre_pressure_front_kpa", "tyre_pressure_rear_kpa")]
    mapped = has_ty and car.id not in PLACARD_CARS
    src = wheel = wheel_from = wheel_guess = None
    front = rear = None
    if typed[0] is not None or typed[1] is not None:
        front = typed[0] if typed[0] is not None else typed[1]
        rear = typed[1] if typed[1] is not None else front
        src = "owner"
    elif car.id in PLACARD_CARS:
        front = rear = 290.0
        typed_wheel = (cfg.get("wheel_in") or {}).get("value") if (cfg.get("wheel_in") or {}).get("source") == "owner" else None
        src, wheel = "placard", int(typed_wheel) if typed_wheel is not None else PLACARD_CARS[car.id]
        wheel_from, wheel_guess = ("owner", False) if typed_wheel is not None else ("can", True)
    elif not legacy:
        front = rear = float(car.tyre_rec_kpa)
        src = "can"

    def rec(x):
        if x is None:
            return None
        return W(x, rec_at) if src == "can" else {"value": x, "at_ms": None, "sources": []}

    low = None
    if has_ty and front is not None and None not in kpa:
        against = [front, front, rear, rear] if mapped else [min(front, rear)] * 4
        low = W(any(p < a * 0.9 for p, a in zip(kpa, against)), tyres_at)
    return {"tyres_slots_mapped": bool(mapped), "tyre_recommended_kpa": rec(front), "tyre_recommended_rear_kpa": rec(rear),
            "tyre_recommended_source": src, "tyre_recommended_wheel_in": wheel, "tyre_recommended_wheel_from": wheel_from,
            "tyre_recommended_wheel_guess": wheel_guess, "tyre_low": low}


def charge_now(app, h, cur, seg, ts, tv, flags):
    car = h.car
    keys = ("status", "plugged", "kind", "power_kw", "volts_v", "amps_a", "phases", "pilot_a", "limit_source",
            "added_kwh", "limit_pct", "to_full_kwh", "to_full_min")
    out = {k: None for k in keys}
    out["finish_ms"] = None
    out["charge_id"] = None
    ch = None
    status = "disconnected"
    plugged = False
    if cur is not None and cur.kind == "charge":
        ch = cur
        status = "charging"
        plugged = True
    elif cur is not None and cur.kind == "parked" and cur.plugged:
        ch = cur.charge
        plugged = True
        status = "about_to_charge" if cur.plugged == "before" else "complete"
    out["status"] = W(status, ts)
    out["plugged"] = W(plugged, ts)
    if plugged:
        flags["charge_port_open"] = W(True, ts)
    if ch is None:
        return out
    c = ch.curve(min(tv, ch.data_end)) if status == "charging" else None
    limit = car.charge_limit if ch.ckind == "ac" else round(ch.target)
    out["kind"] = W(ch.ckind, ts)
    out["limit_pct"] = W(limit, ts)
    out["charge_id"] = ch.id
    if c:
        out["power_kw"] = W(c["power_kw"], ts)
        out["volts_v"] = W(c["volts_v"], ts)
        out["amps_a"] = W(c["amps_a"], ts)
        out["phases"] = W(c["phases"], ts)
        out["pilot_a"] = W(c["pilot_a"], ts)
        out["limit_source"] = W("pilot" if ch.ckind == "ac" else "station", ts)
        soc = ch.soc_at(tv)
        added = (soc - ch.soc0) / 100 * ch.usable
        to_full = max(0.0, (limit - soc) / 100 * ch.usable)
        p = c["power_kw"] or 0.0
        mins = to_full / p * 60 if p > 0.5 else None
        out["added_kwh"] = W(r(added, 3), ts)
        out["to_full_kwh"] = W(r(to_full, 3), ts)
        out["to_full_min"] = W(r(mins, 0), ts)
        out["finish_ms"] = int(tv + mins * MIN) if mins is not None else None
    else:
        f = ch.figures()
        out["power_kw"] = W(0.0, ts)
        out["added_kwh"] = W(r(f["added"], 3), ts)
        out["to_full_kwh"] = W(0.0 if status == "complete" else r((limit - ch.soc0) / 100 * ch.usable, 3), ts)
    return out


def synthetic_now(app, car_id, t, meta):
    sm = app.ds.synthetic.summary(int(car_id[1:]), t)
    n = sm["now"]
    at = n["at_ms"]
    depot = sm["_depot"]
    awake = n["state"] in ("driving", "charging", "parked")
    return {
        "car": car_id, "time_zone": meta["time_zone"], "state": n["state"], "since_ms": n["since_ms"], "at_ms": at,
        "received_ms": n["received_ms"], "delay_s": 75, "stale": n["stale"],
        "buses": [{"bus": "vehicle", "awake": awake, "at_ms": at}, {"bus": "chassis", "awake": awake, "at_ms": at}],
        "driving": n["state"] == "driving",
        "soc_pct": W(n["soc_pct"], at), "range_km": W(n["range_km"], at),
        "ideal_range_km": W(n.get("ideal_range_km"), at),
        "location": {"lat": W(depot[1], at), "lon": W(depot[2], at), "accuracy_m": W(8.0, at),
                     "elevation_m": None, "heading_deg": None, "place": None, "address": None},
        "charge": {"status": W("charging" if n["state"] == "charging" else "disconnected", at),
                   "plugged": W(n["plugged"], at), "power_kw": W(n["charge_power_kw"], at)},
        "awake_reasons": [], "drive": None,
        "software": {"version": None, "version_source": None, "version_since_ms": None, "changed_since_ms": None},
    }


def list_item(app, car_id, t):
    """The /api/v1/cars item for a car at virtual time t."""
    meta = app.store.car_meta(car_id)
    h = app.ds.history(car_id)
    if h is None:
        sm = app.ds.synthetic.summary(int(car_id[1:]), t)
        sm = {k: v for k, v in sm.items() if not k.startswith("_")}
        sm["name"] = meta["name"]
        sm["time_zone"] = meta["time_zone"]
        sm["time_zone_source"] = meta["time_zone_source"]
        sm["sources"] = [S.ref("smo")]
        sm["source_label"] = S.label_of(sm["sources"])
        return sm
    n = now_object(app, car_id, t)
    pl = n.get("location", {}).get("place") if n.get("location") else None
    ch = n.get("charge") or {}
    last = last_activity(h, t)
    return {
        "id": car_id, "name": meta["name"], "model": meta["model"], "year": meta["year"],
        "vin_prefix": meta["vin_prefix"], "trim": meta["config"].get("trim", {}).get("value") or meta["trim"],
        "profile": meta["profile"], "time_zone": meta["time_zone"], "time_zone_source": meta["time_zone_source"],
        "last_activity_ms": last,
        "now": {
            "state": n["state"], "since_ms": n["since_ms"], "at_ms": n["at_ms"], "received_ms": n["received_ms"],
            "stale": n["stale"], "soc_pct": _v(n.get("soc_pct")), "range_km": _v(n.get("range_km")),
            "ideal_range_km": _v(n.get("ideal_range_km")),
            "place": pl, "plugged": _v(ch.get("plugged")),
            "charge_power_kw": _v(ch.get("power_kw")) if _v(ch.get("status")) == "charging" else None,
        },
        "attention": [a["code"] for a in attention(app, car_id, t, n)],
        **_car_sources(h, car_id),
    }


def _car_sources(h, car_id):
    shadow = getattr(h, "shadow", None) or []
    has_import = any(s.source == "teslamate" for s in h.segs) or any(S.site_of(s) == "teslamate" for s in shadow)
    refs = S.car_refs(car_id, True, has_import, any(S.site_of(s) == "teslafi" for s in shadow))
    return {"sources": refs, "source_label": S.label_of(refs)}


def _v(w):
    return None if w is None else w["value"]


def last_activity(h, t):
    """The last time the car drove, charged or woke up (by t)."""
    tv = t - delay_s(t) * 1000
    i = h.seg_index_at(tv)
    if i is None:
        return h.first_ms
    for j in range(i, -1, -1):
        s = h.segs[j]
        if s.kind in ("drive", "charge", "update"):
            return min(tv, s.data_end if hasattr(s, "data_end") else s.end)
        if s.kind == "parked":
            for sub in reversed(s.subs):
                if sub[0] < min(tv, s.end) and sub[2] != "asleep":
                    return min(tv, sub[1], s.end)
    return h.first_ms


def attention(app, car_id, t, n=None):
    """[{car, code, since_ms, detail}] for a featured car at t."""
    h = app.ds.history(car_id)
    if h is None:
        sm = app.ds.synthetic.summary(int(car_id[1:]), t)
        out = []
        for code in sm["attention"]:
            out.append({"car": car_id, "code": code, "since_ms": sm["last_activity_ms"],
                        "detail": _synthetic_detail(code, sm)})
        return out
    n = n or now_object(app, car_id, t)
    out = []
    car = h.car
    cur = h.seg_at(t - delay_s(t) * 1000)
    if n.get("stale") and n.get("received_ms"):
        out.append({"car": car_id, "code": "upload_silence", "since_ms": n["received_ms"],
                    "detail": {"last_upload_ms": n["received_ms"]}})
    if cur is not None and cur.kind == "gap" and cur.open_end:
        idx = h.segs.index(cur)
        prev = h.segs[idx - 1] if idx > 0 else None
        if prev is not None and prev.kind == "gap" and prev.reason == "failed_check":
            out.append({"car": car_id, "code": "trust_failed", "since_ms": prev.start,
                        "detail": {"reason": "signature_mismatch", "log_start_ms": prev.start}})
    soc = _v(n.get("soc_pct"))
    if soc is not None and soc < 15:
        out.append({"car": car_id, "code": "low_battery", "since_ms": n["at_ms"], "detail": {"soc_pct": soc}})
    ty = n.get("tyres_kpa")
    if ty:
        rec = car.tyre_rec_kpa
        for name, w in zip(("fl", "fr", "rl", "rr"), ty):
            if w and w["value"] < rec * 0.9:
                since = t
                day = 0
                while day < 60:
                    probe = t - day * DAY
                    from .dataset.segments import tyres as tyre_fn
                    v = tyre_fn(car, probe, 15.0, 0.0)
                    if v is None or v[("fl", "fr", "rl", "rr").index(name)] >= rec * 0.9:
                        break
                    since = probe
                    day += 1
                out.append({"car": car_id, "code": "tyre_low", "since_ms": since,
                            "detail": {"tyre": name, "pressure_kpa": w["value"], "recommended_kpa": rec, "source": "can"}})
    return out


def _synthetic_detail(code, sm):
    if code == "low_battery":
        return {"soc_pct": sm["now"]["soc_pct"]}
    if code == "upload_silence":
        return {"last_upload_ms": sm["now"]["received_ms"]}
    if code == "tyre_low":
        return {"tyre": "rr", "pressure_kpa": 241.0, "recommended_kpa": 290, "source": "can"}
    if code == "door_open_asleep":
        return {"door": "rl"}
    if code == "trust_failed":
        return {"reason": "signature_mismatch", "log_start_ms": sm["last_activity_ms"]}
    if code in ("stopped_mid_drive", "stopped_mid_charge"):
        return {"last_data_ms": sm["last_activity_ms"]}
    return {}

