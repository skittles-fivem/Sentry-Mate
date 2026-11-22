"""Seeds for what the owner can change outside the car data: notices, server and MQTT settings,
import jobs and storage figures. The Store copies these in (store.notices, store.server_settings,
store.mqtt, store.imports, store.storage).
"""

from __future__ import annotations

from ..util import DAY, HOUR, MIN


def notices(now):
    return [
        {"code": "certificate_expiring", "level": "warning",
         "message": "The HTTPS certificate expires in 12 days.", "since_ms": now - 18 * DAY,
         "fix_link": "/settings/server", "expires_ms": now + 12 * DAY},
        {"code": "recovery_key_unsaved", "level": "info",
         "message": "Save the vault recovery key file somewhere safe.", "since_ms": now - 30 * DAY,
         "fix_link": "/settings/server"},
    ]


def server_settings(now):
    return {
        "version": 1,
        "units": {"distance": "km", "temperature": "c", "pressure": "bar"},
        "range": "rated",
        "car_order": [],
        "geocode": {"mode": "offline", "dataset": "made-up offline set", "url": None, "language": "en",
                    "rounding_m": 10},
        "tiles": {"mode": "online", "online_url": "https://tile.openstreetmap.org/{z}/{x}/{y}.png",
                  "attribution": "© OpenStreetMap contributors", "max_zoom": 19},
        "release_check": {"enabled": False},
        "outside_links": {"maps": False, "release_notes": False},
        "outbound": {"proxy": None},
        "host_names": ["sentry-mate.local"],
        # the zone new cars start in (set from the first admin's browser at set-up)
        "default_time_zone": "America/Chicago",
    }


def mqtt(now):
    return {
        "version": 1, "enabled": True, "host": "broker.local", "port": 8883, "username": "sentry-mate",
        "password_set": True, "tls": True, "pin_sha256": None, "namespace": "sentrymate", "publish_location": False,
        "ha_discovery": {"enabled": True, "prefix": "homeassistant", "url": None}, "no_place_text": "away",
        "status": {"connected": True, "delivered": 18_442, "waiting": 0, "dropped": 3, "last_error": None,
                   "last_error_ms": None},
    }


def imported_history(h):
    """What c3's imported TeslaMate history holds (the made-up backup's first car is exactly that)."""
    if h is None:
        return None
    tm = [x for x in h.segs + h.shadow if x.source == "teslamate" and getattr(x, "site", None) != "teslafi"]
    if not tm:
        return None
    drives = sum(1 for x in tm if x.kind == "drive")
    return {"drives": drives, "charges": sum(1 for x in tm if x.kind == "charge"),
            "updates": sum(1 for x in tm if x.kind == "update"),
            "positions": sum(max(1, (x.end - x.start) // 1000) for x in tm if x.kind == "drive"),
            "first_ms": min(x.start for x in tm), "last_ms": max(x.end for x in tm)}


def backup_cars(now, h):
    """The cars in the made-up TeslaMate backup: Atlas (c3 here) and an old car that is not here."""
    tm = imported_history(h) or {"drives": 1188, "charges": 224, "updates": 4, "positions": 2_410_000,
                                 "first_ms": now - 900 * DAY, "last_ms": now - 393 * DAY}
    return [
        {"tm_car": 1, "name": "Atlas", "vin_prefix": "5YJSA1E20HF", "drives": tm["drives"], "charges": tm["charges"],
         "updates": tm["updates"], "positions": tm["positions"], "first_ms": tm["first_ms"], "last_ms": tm["last_ms"]},
        {"tm_car": 2, "name": "Old Roadster", "vin_prefix": "5YJRE1A10BF", "drives": 312, "charges": 61, "updates": 3,
         "positions": 640_000, "first_ms": now - 1500 * DAY, "last_ms": now - 1100 * DAY},
    ]


def _car_tables(c):
    """Rows one backup car brings, per table: (read, skipped by reason, cleared values by column)."""
    d, ch = c["drives"], c["charges"]
    return {
        "cars": (1, {}, {}),
        "drives": (d + d // 150, {"duplicate": d // 150}, {}),
        "charging_processes": (ch + ch // 110, {"no_energy": ch // 110}, {"cost": 1 if ch else 0}),
        "positions": (c["positions"] + d // 2, {"no_position": d // 2}, {"elevation": d // 30}),
        "updates": (c["updates"], {}, {}),
    }


def import_report(cars, imported):
    """The report of an import of `cars` (all of them when only checked; the matched ones once imported).
    Each table's figures are the sum of its cars' (geofences belong to no car); each car carries its own
    read, imported and skipped counts (None for a car left out of a finished import: it was not counted)."""
    use = [c for c in cars if c["match"] != "leave_out"] if imported else cars
    tables = {name: {"read": 0, "imported": 0, "skipped": {}, "bad_values": {}}
              for name in ("cars", "drives", "charging_processes", "positions", "updates", "geofences")}
    per_car = {}
    for c in use:
        own = {"read": 0, "imported": 0, "skipped": {}}
        for name, (read, skipped, bad) in _car_tables(c).items():
            t, sk = tables[name], sum(skipped.values())
            t["read"] += read
            t["imported"] += (read - sk) if imported else 0
            own["read"] += read
            own["imported"] += (read - sk) if imported else 0
            for k, v in skipped.items():
                if v:
                    t["skipped"][k] = t["skipped"].get(k, 0) + v
                    own["skipped"][k] = own["skipped"].get(k, 0) + v
            for k, v in bad.items():
                if v:
                    t["bad_values"][k] = t["bad_values"].get(k, 0) + v
        per_car[c["tm_car"]] = own
    tables["geofences"].update(read=6, imported=6 if imported else 0)
    return {"tables": tables,
            "span": {"from_ms": min(c["first_ms"] for c in cars), "to_ms": max(c["last_ms"] for c in cars)},
            "cars": [dict({"tm_car": c["tm_car"], "name": c["name"], "match": c["match"], "drives": c["drives"], "charges": c["charges"]},
                          **(per_car.get(c["tm_car"]) or {"read": None, "imported": None, "skipped": None}))
                     for c in cars], "imported": imported}


# The made-up TeslaFi file (mockapi/data/TeslaFi32023.csv): one day of a car no car here is, "Blue Whale" (a
# made-up VIN, positions in the open ocean), polled once a minute awake and every 30 minutes asleep.
TESLAFI_DAY = {"rows": 259, "drives": 2, "charges": 2, "states": 3, "updates": 0, "positions": 259,
               "charge_points": 120, "first_ms": 1_677_888_000_000, "last_ms": 1_677_972_600_000}


def teslafi_cars():
    d = TESLAFI_DAY
    return [{"tm_car": 1, "name": "Blue Whale", "vin_prefix": "5YJ3E1EA4PF", "drives": d["drives"],
             "charges": d["charges"], "first_ms": d["first_ms"], "last_ms": d["last_ms"]}]


def teslafi_report(cars, imported, settings, again=False, read_again=False, read_in=None):
    """A TeslaFi file's report as the server writes it (`engine::query::imports::teslafi_report_json`): files, rows
    read, used and skipped with why, what was found (stored once `imported`), the cars, the span, the units and
    the time zone the file was read in. `again`: the same file a second time (every row already imported);
    `read_again`: the same file in another time zone or other units than `read_in` (zone, distance, temperature),
    its earlier rows replaced."""
    d = TESLAFI_DAY
    left_out = imported and all(c["match"] == "leave_out" for c in cars)
    used = 0 if (again or left_out) else d["rows"]
    skipped = {"already_imported": d["rows"]} if again else ({"car_left_out": d["rows"]} if left_out else {})
    found = {k: (0 if used == 0 else d[k]) for k in ("drives", "charges", "states", "updates", "positions",
                                                      "charge_points")}
    found["replaced"] = 0
    found["read_again"] = 1 if read_again and used else 0
    warnings = []
    if found["read_again"]:
        how = f"in {read_in[0]}" if read_in and read_in[0] != (settings.get("time_zone") or "UTC") else "in other units"
        warnings.append(f"months imported before {how} were read again: what they stored was replaced (read the "
                        "months next to them again too, so a drive or charge over their edges is whole)")

    def table(read, n):
        return {"read": read, "imported": n if imported else 0, "found": n, "skipped": skipped if read else {}}

    units = {"distance": settings.get("distance_unit") or "mi",
             "distance_from": "setting" if settings.get("distance_unit") else "detected",
             "temperature": settings.get("temperature_unit") or "c",
             "temperature_from": "setting" if settings.get("temperature_unit") else "detected"}
    tz = settings.get("time_zone")
    return {
        "kind": "teslafi", "imported": imported, "files": 1, "files_skipped": {},
        "rows": {"read": d["rows"], "used": used, "skipped": skipped, "bad_values": {}, "windows_1252": 0},
        "found": found,
        "tables": {"positions": table(d["rows"], found["positions"]), "drives": table(0, found["drives"]),
                   "charging_processes": table(0, found["charges"]), "states": table(0, found["states"]),
                   "updates": table(0, found["updates"])},
        "span": {"from_ms": d["first_ms"] if used else None, "to_ms": d["last_ms"] if used else None},
        "cars": [{"tm_car": str(c["tm_car"]), "name": c["name"], "vin_prefix": c["vin_prefix"], "match": c["match"],
                  "drives": found["drives"], "charges": found["charges"], "first_ms": c["first_ms"],
                  "last_ms": c["last_ms"],
                  "rows": None if imported and c["match"] == "leave_out" else d["rows"]} for c in cars],
        "cars_count": len(cars), "cars_left_out": sum(1 for c in cars if c["match"] == "leave_out") if imported else 0,
        "units": units,
        "time_zone": {"name": tz or "UTC", "from": "setting" if tz else "default", "asked": True,
                      "dst_repeated_rows": 0},
        "compression": "plain", "packed_bytes": 73_610, "unpacked_bytes": 73_610, "missing_columns": [],
        "unknown_columns": 3, "warnings": warnings,
    }


def imports(now, h=None):
    """The import that brought c3's history in (done), with a report that matches what c3 holds."""
    if imported_history(h) is None:  # no c3 (a smaller fleet): nothing was imported
        return {}
    cars = [dict(backup_cars(now, h)[0], match="c3")]
    t0 = cars[0]["last_ms"] + 2 * DAY
    return {
        "im1": {"id": "im1", "name": "teslamate-backup-atlas.sql.xz", "compression": "xz", "kind": "teslamate",
                "status": "done", "pass": 2,
                "bytes_read": 412_000_000, "bytes_total": 412_000_000, "uploaded_bytes": 412_000_000,
                "started_ms": t0, "checked_ms": t0 + 9 * MIN, "finished_ms": t0 + 40 * MIN, "error": None,
                "cars": cars, "report": import_report(cars, imported=True)},
    }


def storage(now):
    """Server figures that no car gives: offline tiles, free space and uptime (cars' bytes come from
    their own history, see resources/system.py)."""
    gb = 1024 ** 3
    return {"tiles_bytes": int(2.3 * gb), "free_bytes": int(412 * gb), "uptime_ms": 9 * DAY + 4 * HOUR}
