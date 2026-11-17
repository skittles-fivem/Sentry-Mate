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
        "host_names": ["sentry-mate.local"],
    }


def mqtt(now):
    return {
        "version": 1, "enabled": True, "host": "broker.local", "port": 8883, "username": "sentry-mate",
        "password_set": True, "tls": True, "pin_sha256": None, "namespace": "sentrymate", "publish_location": False,
        "ha_discovery": {"enabled": True, "prefix": "homeassistant", "url": None}, "no_place_text": "away",
        "status": {"connected": True, "delivered": 18_442, "waiting": 0, "dropped": 3, "last_error": None,
                   "last_error_ms": None},
    }


def imports(now):
    return {
        "im1": {"id": "im1", "name": "teslamate-backup-atlas.sql.xz", "compression": "xz", "status": "done", "pass": 2,
                "bytes_read": 412_000_000, "bytes_total": 412_000_000, "uploaded_bytes": 412_000_000,
                "started_ms": now - 398 * DAY, "checked_ms": now - 398 * DAY + 9 * MIN,
                "finished_ms": now - 398 * DAY + 40 * MIN, "error": None,
                "cars": [{"tm_car": 1, "name": "Atlas", "vin_prefix": "5YJSA1E20HF", "drives": 1188, "charges": 224,
                          "first_days": -900, "last_days": -393, "match": "c3"}],
                "report": {"tables": {
                    "drives": {"read": 1188, "imported": 1180, "skipped": {"duplicate": 8}, "bad_values": {}},
                    "charging_processes": {"read": 224, "imported": 222, "skipped": {"no_energy": 2},
                                           "bad_values": {"cost": 1}},
                    "positions": {"read": 2_410_000, "imported": 2_409_500, "skipped": {"no_position": 500},
                                  "bad_values": {"elevation": 40}},
                    "updates": {"read": 4, "imported": 4, "skipped": {}, "bad_values": {}},
                }, "span": {"from_ms": now - 900 * DAY, "to_ms": now - 393 * DAY},
                    "cars": [{"tm_car": 1, "match": "c3", "drives": 1188, "charges": 224}], "imported": True}},
    }


def storage(now):
    gb = 1024 ** 3
    return {
        "kinds": [{"kind": "logs", "label": "CAN logs (sealed)", "bytes": int(38.4 * gb)},
                  {"kind": "series", "label": "Time series", "bytes": int(6.1 * gb)},
                  {"kind": "rows", "label": "Drives, charges, states", "bytes": int(0.4 * gb)},
                  {"kind": "imported", "label": "Imported from TeslaMate", "bytes": int(1.2 * gb)},
                  {"kind": "tiles", "label": "Offline map tiles", "bytes": int(2.3 * gb)}],
        "free_bytes": int(412 * gb),
        "growth_per_day_bytes": int(0.11 * gb),
        "uptime_ms": 9 * DAY + 4 * HOUR,
    }
