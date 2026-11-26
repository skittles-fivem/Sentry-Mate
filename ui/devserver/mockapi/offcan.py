"""Values no CAN message carries (docs/ui-api.md, "Now", `off_can`): the software version and a waiting update, the
locks, the windows, the sunroof, the charge schedule and the navigation route. The server fills them from the car's
other live inputs, the Tesla API first, then TeslaFi's polls; each value names the input it came from and goes stale
after an age limit (body 1 h, update 6 h, schedule 12 h, route 10 min, version 30 days).

The made-up inputs: Comet (`c1`) and Maple (`c2`) have the Tesla API, Atlas (`c3`) has TeslaFi while its TeslaFi
account is matched, running and not paused; every other car has SMO alone, so all its values are null. Comet waits
on an update that is downloading; Maple has a sunroof and a departure set the evening before (stale by now); Atlas
reads its windows from TeslaFi like the rest. The software version: Comet's from the Tesla API, Atlas's from TeslaFi
(live.live_version), Maple's from the owner's local TeslaMate once its car there is matched to Maple (its Tesla API
reads carry none). A car that drives follows a route. Nothing is ever guessed: a lock
state comes only from what an input read, never from a lock request.
"""

from __future__ import annotations

from . import sources as S
from .util import DAY, HOUR, MIN, h01, local_dt, local_ms

AGE = {"body": HOUR, "update": 6 * HOUR, "schedule": 12 * HOUR, "route": 10 * MIN, "version": 30 * DAY}
DESTINATIONS = ["Harbour Market", "Northgate Library", "Willow Park", "Station Road Garage", "Riverside Gym"]
WINDOWS = ("driver_front", "passenger_front", "driver_rear", "passenger_rear")
# the update a car waits on: (version, status word, download %)
UPDATES = {"c1": ("2026.38.4", "downloading", 64)}


def api_input(app, car_id):
    """The car's API input (`tesla_api`, `teslafi_api`) or None."""
    if car_id in ("c1", "c2"):
        return "tesla_api"
    if car_id == "c3":
        for a in ((getattr(app.store, "teslafi", None) or {}).get("accounts") or {}).values():
            if a.get("paused") or a.get("state") != "ok":
                continue
            if any(c.get("match") == car_id for c in a.get("cars") or []):
                return "teslafi_api"
    return None


def teslafi_read_ms(t):
    """When TeslaFi last polled the car: it polls on its own, whether or not SMO has heard from the car (50 s ago)."""
    return t - 50_000


def teslafi_times(app, car_id, t):
    """(last_ms, received_ms) of TeslaFi's newest data for the car: TeslaFi's own time of it, and when the server read
    it. A page check sets `data_age_ms` on the car's TeslaFi account (TeslaFi has had nothing new for that long: the
    data came then) or `time_offset_ms` (polls still come, stamped that long before they are read: the account's
    time zone is wrong); see `resources.teslafi.data_health`."""
    a = next((a for a in app.store.teslafi["accounts"].values()
              if any(c.get("match") == car_id for c in a["cars"])), None) or {}
    if a.get("data_age_ms"):
        last = t - a["data_age_ms"]
        return last, last + 30_000
    if a.get("time_offset_ms"):
        return t - 50_000 - a["time_offset_ms"], t - 45_000
    last = teslafi_read_ms(t)
    return last, last + 5_000


def _v(value, at, kind, t, age):
    if value is None:
        return None
    return {"value": value, "at_ms": at, "sources": [S.ref(kind)], "stale": t - at > age}


def empty():
    return {
        "version": None, "update_available": None, "update_version": None, "update_download_pct": None,
        "update_installing": None, "locked": None, "windows_open": None,
        "windows": {k: None for k in WINDOWS},
        "sunroof_state": None, "sunroof_pct": None,
        "scheduled_charge_start_ms": None, "scheduled_departure_ms": None,
        "route": {"destination": None, "km": None, "minutes": None, "traffic_min": None},
    }


def _next_local(t, tz, hour):
    """The next local `hour` (a float) after t."""
    d = local_dt(t, tz).date()
    at = local_ms(d, hour, tz)
    return at if at > t else local_ms(d.fromordinal(d.toordinal() + 1), hour, tz)


def off_can(app, car_id, t, n):
    """The `off_can` object for a car at t, given its now object `n` (without `off_can`)."""
    out = empty()
    if n.get("at_ms") is None:
        return out
    from .live import live_version
    h = app.ds.history(car_id)
    lv = live_version(app, h, t) if h is not None else None
    if lv is not None:  # a version a live input reads (TeslaFi, local TeslaMate), with that input
        out["version"] = _v(lv[0], lv[1], lv[2], t, AGE["version"])
    kind = api_input(app, car_id)
    if kind is None:
        return out
    at = next((i["last_ms"] for i in n.get("inputs") or [] if i["kind"] == kind), None)
    if at is None:
        return out
    tz = n.get("time_zone") or "UTC"
    if car_id == "c1":  # Comet's Tesla API reads say the version it runs; Maple's carry none (made up)
        ver = (n.get("software") or {}).get("version")
        out["version"] = _v(ver, at, kind, t, AGE["version"])
    upd = UPDATES.get(car_id)
    out["update_available"] = _v(upd is not None, at, kind, t, AGE["update"])
    out["update_installing"] = _v(bool(upd and upd[1] == "installing"), at, kind, t, AGE["update"])
    if upd:
        out["update_version"] = _v(upd[0], at, kind, t, AGE["update"])
        out["update_download_pct"] = _v(upd[2], at, kind, t, AGE["update"])
    driving = bool(n.get("driving"))
    present = bool(((n.get("flags") or {}).get("user_present") or {}).get("value"))
    out["locked"] = _v(not (driving or present), at, kind, t, AGE["body"])
    # Maple's driver rear window is left open a crack while it is parked at home in the day
    win = {k: False for k in WINDOWS}
    if car_id == "c2" and not driving and 9 <= local_dt(t, tz).hour < 18:
        win["driver_rear"] = True
    for k in WINDOWS:
        out["windows"][k] = _v(win[k], at, kind, t, AGE["body"])
    out["windows_open"] = _v(any(win.values()), at, kind, t, AGE["body"])
    if car_id == "c2":  # the one car with a sunroof
        vent = not driving and 9 <= local_dt(t, tz).hour < 18
        out["sunroof_state"] = _v("vent" if vent else "closed", at, kind, t, AGE["body"])
        out["sunroof_pct"] = _v(15 if vent else 0, at, kind, t, AGE["body"])
    if kind == "tesla_api":
        # an off-peak start set in the car; Maple's departure was read the evening before and is stale by now
        out["scheduled_charge_start_ms"] = _v(_next_local(t, tz, 1.5), at, kind, t, AGE["schedule"])
        if car_id == "c2":
            dep_at = at - 14 * HOUR
            out["scheduled_departure_ms"] = _v(_next_local(t, tz, 7.25), dep_at, kind, t, AGE["schedule"])
        else:
            out["scheduled_departure_ms"] = _v(_next_local(t, tz, 7.75), at, kind, t, AGE["schedule"])
    if driving and n.get("drive"):
        d = n["drive"]
        x = h01("route", car_id, d.get("drive_id") or d.get("start_ms"))
        total_km = 6 + x * 30
        left = max(0.4, round(total_km - (d.get("distance_km") or 0.0), 1))
        r = out["route"]
        r["destination"] = _v(DESTINATIONS[int(x * len(DESTINATIONS))], at, kind, t, AGE["route"])
        r["km"] = _v(left, at, kind, t, AGE["route"])
        r["minutes"] = _v(max(1, round(left * 1.4)), at, kind, t, AGE["route"])
        r["traffic_min"] = _v(int(x * 7) % 4, at, kind, t, AGE["route"])
    return out
