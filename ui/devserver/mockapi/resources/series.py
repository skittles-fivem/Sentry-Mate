"""Time series and decoded signals (docs/ui-api.md, "Time series")."""

from __future__ import annotations

from .. import series as S
from ..conventions import page
from ..dataset.segments import SERIES_FIELDS
from ..errors import bad_input
from ..live import last_sample, now_object
from ..util import HOUR

# Decoded signals: our series name, message id, bus, unit, what it is, and the series field it feeds.
SIGNALS = [
    ("vehicle_speed", "0x257", "chassis", "km/h", "Speed the car shows", "speed_kph"),
    ("pack_power", "0x132", "vehicle", "kW", "Pack power, positive out of the pack", "power_kw"),
    ("soc_display", "0x33A", "vehicle", "%", "Battery % on the screen", "soc_pct"),
    ("soc_usable", "0x33A", "vehicle", "%", "Usable battery % (cold-locked part removed)", "usable_soc_pct"),
    ("soc_bms", "0x292", "vehicle", "%", "Battery % from the battery controller", "soc_bms_pct"),
    ("range_rated", "0x33A", "vehicle", "km", "Rated range", "range_km"),
    ("range_ideal", "0x33A", "vehicle", "km", "Ideal range", "ideal_range_km"),
    ("energy_remaining", "0x352", "vehicle", "kWh", "Energy left in the pack", "energy_remaining_kwh"),
    ("odometer", "0x3B6", "vehicle", "km", "Odometer", "odometer_km"),
    ("temp_outside", "0x321", "vehicle", "°C", "Outside temperature", "outside_c"),
    ("temp_cabin", "0x243", "vehicle", "°C", "Cabin temperature", "cabin_c"),
    ("temp_battery", "0x312", "vehicle", "°C", "Battery temperature (lowest module)", "battery_c"),
    ("hvac_setpoint_left", "0x2F3", "vehicle", "°C", "Climate set point, driver side", "setpoint_left_c"),
    ("hvac_setpoint_right", "0x2F3", "vehicle", "°C", "Climate set point, passenger side", "setpoint_right_c"),
    ("gps_latitude", "0x04F", "chassis", "°", "Latitude", "lat"),
    ("gps_longitude", "0x04F", "chassis", "°", "Longitude", "lon"),
    ("gps_accuracy", "0x04F", "chassis", "m", "GPS accuracy", "accuracy_m"),
    ("gps_elevation", "0x3D8", "chassis", "m", "Elevation", "elevation_m"),
    ("gps_heading", "0x3D9", "chassis", "°", "Heading", "heading_deg"),
    ("tpms_front_left", "0x219", "vehicle", "kPa", "Tyre pressure, front left", "tyre_fl_kpa"),
    ("tpms_front_right", "0x219", "vehicle", "kPa", "Tyre pressure, front right", "tyre_fr_kpa"),
    ("tpms_rear_left", "0x219", "vehicle", "kPa", "Tyre pressure, rear left", "tyre_rl_kpa"),
    ("tpms_rear_right", "0x219", "vehicle", "kPa", "Tyre pressure, rear right", "tyre_rr_kpa"),
    ("lv_battery_voltage", "0x261", "vehicle", "V", "Low-voltage battery", "lv_v"),
]
ENUM_SIGNALS = [
    ("gear", "0x118", "vehicle", None, "Gear", {0: "P", 1: "R", 2: "N", 3: "D"}),
    ("sentry_mode", "0x284", "vehicle", None, "Sentry mode", {0: "Off", 1: "On"}),
    ("charge_status", "0x212", "vehicle", None, "Charging status",
     {0: "Disconnected", 1: "No power", 2: "About to charge", 3: "Charging", 4: "Complete", 5: "Stopped"}),
]
BY_SERIES = {s[0]: s for s in SIGNALS}


def _fields(req):
    fields = req.list_param("fields", SERIES_FIELDS) or ["soc_pct", "range_km"]
    return list(dict.fromkeys(fields))


def get_series(req):
    fields = _fields(req)
    a, b = req.period(24 * HOUR)
    max_points = req.int_param("max_points", 1000, 2, 20000)
    aggs = req.list_param("agg", S.AGGS, 6)
    source = req.enum("source", ("can", "teslamate", "all"), "all")
    h = req.history()
    out = {"car": req.car, "time_zone": req.tz(), "fields": fields}
    if h is None:
        out.update({"t_ms": [], "values": {f: [] for f in fields}, "bucket_ms": 1000, "coverage_pct": [], "state": []})
        return out
    out.update(S.build(h, fields, a, b, max_points, aggs, req.now, source, explicit_aggs=aggs is not None))
    return out


def _enum_value(name, n, cur):
    if name == "gear":
        return 3 if n.get("driving") else 0
    if name == "sentry_mode":
        f = (n.get("flags") or {}).get("sentry")
        return None if f is None else int(bool(f["value"]))
    st = ((n.get("charge") or {}).get("status") or {}).get("value")
    return {"disconnected": 0, "no_power": 1, "about_to_charge": 2, "charging": 3, "complete": 4,
            "stopped": 5}.get(st)


def signals_latest(req):
    search = (req.q("search") or "").strip().lower()
    h = req.history()
    rows = []
    if h is not None:
        n = now_object(req.app, req.car, req.now)
        seg, ts, vals = last_sample(h, n["at_ms"] or req.now)
        vals = vals or {}
        for name, mid, bus, unit, comment, field in SIGNALS:
            v = vals.get(field)
            if field == "odometer_km" and v is None and n.get("odometer_km"):
                v = n["odometer_km"]["value"]
            rows.append({"series": name, "message_id": mid, "bus": bus, "value": v, "unit": unit, "value_name": None,
                         "comment": comment, "at_ms": ts if v is not None else None})
        for name, mid, bus, unit, comment, names in ENUM_SIGNALS:
            v = _enum_value(name, n, seg)
            rows.append({"series": name, "message_id": mid, "bus": bus, "value": v, "unit": unit,
                         "value_name": names.get(v) if v is not None else None, "comment": comment,
                         "at_ms": ts if v is not None else None})
        if search:
            rows = [x for x in rows if search in x["series"] or search in x["comment"].lower()
                    or search in x["message_id"].lower()]
    items, nxt = page(req, rows, key=lambda x: (x["series"],))
    return {"car": req.car, "time_zone": req.tz(), "items": items, "next": nxt}


def get_signals(req):
    names = req.list_param("series", [s[0] for s in SIGNALS])
    if not names:
        raise bad_input({"series": "Name at least one series (see /signals/latest)."})
    a, b = req.period(HOUR)
    max_points = req.int_param("max_points", 2000, 2, 20000)
    h = req.history()
    fields = [BY_SERIES[n][5] for n in names]
    out = {"car": req.car, "time_zone": req.tz(), "series": names,
           "units": {n: BY_SERIES[n][3] for n in names}}
    if h is None:
        out.update({"t_ms": [], "values": {n: [] for n in names}, "bucket_ms": 1000, "coverage_pct": [], "state": []})
        return out
    cols = S.build(h, fields, a, b, max_points, ["last"], req.now, "can")
    out.update({"t_ms": cols["t_ms"], "values": {n: cols["values"][f] for n, f in zip(names, fields)},
                "bucket_ms": cols["bucket_ms"], "coverage_pct": cols["coverage_pct"], "state": cols["state"]})
    return out


def register(router):
    router.add("/api/v1/cars/{car}/series", GET=get_series)
    router.add("/api/v1/cars/{car}/signals/latest", GET=signals_latest)
    router.add("/api/v1/cars/{car}/signals", GET=get_signals)
