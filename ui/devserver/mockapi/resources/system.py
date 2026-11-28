"""Server, settings and health (docs/ui-api.md, "Server, settings and health"): server defaults,
map and address settings, notices, about, per-car data health, rebuilds, CAN and TeslaMate
overlaps, and storage.

Health follows one rule (the same one MQTT `healthy` uses): a car is healthy when its newest log
arrived within the hour and passed its check, no CAN Bridge key waits to be trusted, and no
rebuild is pending.
"""

from __future__ import annotations

import copy
from zoneinfo import ZoneInfo, ZoneInfoNotFoundError
import ipaddress
import re
import time

from .. import derive
from ..conventions import Response, json_bytes, page
from ..errors import ApiError, bad_input, conflict, invalid, not_found, stale_version
from ..live import now_object
from .fleet import device_label
from ..util import DAY, HOUR, MIN, date_str, h01, local_date, r
from .. import sources as SRC
from ..uploads import free_bytes
from .reports import gap_rows

DEV_VERSION = "0.1.0-dev"
API_VERSION = 1


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
             "outbound", "host_names", "default_time_zone", "import_gap_fill", "full_rebuild")
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
    tiles_body = body.get("tiles") if isinstance(body.get("tiles"), dict) else {}
    sub("tiles", {"mode": ("offline", "online"), "online_url": "str?", "attribution": "str", "max_zoom": int,
                  "offline_file": "str?", "preset": "str?", "labels": bool, "styles": "any"})
    if "online_url" in tiles_body and "preset" not in tiles_body:
        # an address without a preset: the preset it is, else the admin's own
        p = next((x for x in TILE_PRESETS if x["url"] == new["tiles"].get("online_url")), None)
        new["tiles"]["preset"] = p["id"] if p else "custom"
    if "preset" in tiles_body and isinstance(tiles_body["preset"], str) and tiles_body["preset"] != "custom":
        p = next((x for x in TILE_PRESETS if x["id"] == tiles_body["preset"]), None)
        if p is None:
            rule["tiles.preset"] = "One of: " + ", ".join(x["id"] for x in TILE_PRESETS) + ", custom, or null."
        else:
            new["tiles"].update(online_url=p["url"], attribution=p["attribution"], max_zoom=p["max_zoom"])
    if "styles" in tiles_body:
        v = tiles_body["styles"]
        if not isinstance(v, list) or not all(isinstance(x, str) for x in v):
            bad["tiles.styles"] = "Must be a list of preset ids."
        elif any(x not in {p["id"] for p in TILE_PRESETS} for x in v):
            rule["tiles.styles"] = "Preset ids only: " + ", ".join(x["id"] for x in TILE_PRESETS) + "."
        else:
            new["tiles"]["styles"] = list(dict.fromkeys(v))
    sub("release_check", {"enabled": bool})
    sub("outside_links", {"maps": bool, "release_notes": bool})
    sub("outbound", {"proxy": "str?"})
    sub("import_gap_fill", {"enabled": bool})
    sub("full_rebuild", {"auto": bool})
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
            rule["car_order"] = "No such car."
        else:
            new["car_order"] = list(dict.fromkeys(v))
    if "host_names" in body:
        v = body["host_names"]
        if not isinstance(v, list) or not all(isinstance(x, str) for x in v):
            bad["host_names"] = "Must be a list of host names."
        elif len(v) > 10:
            rule["host_names"] = "At most 10 host names."
        else:
            # an empty list: the running certificate's names; IPv6 addresses are kept without brackets
            out = []
            for i, x in enumerate(v):
                x = x.strip().lower()
                bare = x[1:-1] if x.startswith("[") and x.endswith("]") else x
                try:
                    out.append(str(ipaddress.IPv6Address(bare)))
                    continue
                except ValueError:
                    pass
                if not re.fullmatch(r"[A-Za-z0-9]([A-Za-z0-9-]*[A-Za-z0-9])?(\.[A-Za-z0-9]([A-Za-z0-9-]*[A-Za-z0-9])?)*",
                                    x) or len(x) > 253:
                    rule[f"host_names.{i}"] = "A host name such as sentry-mate.local, or an IP address."
                else:
                    out.append(x)
            out = list(dict.fromkeys(out))
            new["host_names"] = out or [n for n in st.cert_names if n != "localhost"]
    if "default_time_zone" in body:
        z = body["default_time_zone"]
        if not isinstance(z, str):
            bad["default_time_zone"] = "Must be text."
        elif not valid_zone(z.strip()):
            rule["default_time_zone"] = "An IANA time zone such as America/Chicago."
        else:
            new["default_time_zone"] = z.strip()
    if bad:
        raise bad_input(bad)
    g, t = new["geocode"], new["tiles"]
    # (trimmed, as the real server keeps them)
    g["language"] = g["language"].strip()
    if isinstance(t.get("attribution"), str):
        t["attribution"] = t["attribution"].strip()
    if "car_order" in new and len(new["car_order"]) > 10_000:
        rule["car_order"] = "At most 10000 cars."
    if g["mode"] == "online" and not _https_url(g.get("url")):
        rule["geocode.url"] = "Online address lookup needs an https:// address of the service."
    if g.get("url") is not None and not _https_url(g["url"]):
        rule["geocode.url"] = "An https:// address, or null."
    if not 0 <= g["rounding_m"] <= 1000:
        rule["geocode.rounding_m"] = "Must be between 0 and 1000 m."
    if not 2 <= len(g["language"]) <= 10 or not re.fullmatch(r"[A-Za-z0-9_-]+", g["language"]):
        rule["geocode.language"] = "A language code such as en or de."
    if len(t.get("attribution") or "") > 300:
        rule["tiles.attribution"] = "At most 300 characters."
    if t["mode"] == "online" and not _https_url(t.get("online_url"), template=True):
        rule["tiles.online_url"] = "An https:// tile address with {z}, {x} and {y}."
    px = new["outbound"].get("proxy")
    if px is not None and not re.fullmatch(r"(https?|socks5)://[A-Za-z0-9.-]+(:\d{1,5})?/?", px):
        rule["outbound.proxy"] = "A proxy address such as http://proxy.lan:3128 (no name or password in it), or null."
    if not 1 <= t["max_zoom"] <= 22:
        rule["tiles.max_zoom"] = "Must be between 1 and 22."
    of = t.get("offline_file")
    if of is not None and not re.fullmatch(r"(?!\.)[A-Za-z0-9._-]{1,112}\.pmtiles", of):
        rule["tiles.offline_file"] = "A .pmtiles file name in the data folder's maps folder (no folders), or null."
    if rule:
        raise invalid(rule)
    with st.lock:
        new["version"] = cur["version"] + 1
        st.server_settings = new
        accounts = list(st.accounts.values())
    st.record(req.account, "settings-changed", "settings:server",
              "Changed the server settings: " + ", ".join(sorted(k for k in body if k != "version")) + ".")
    if any(cur[k] != new[k] for k in ("units", "range", "car_order")):
        # preferences default from these: everyone's (merged) preferences may have changed
        for a in accounts:
            req.app.events.publish("prefs", st.get_prefs(a), account_id=a["id"])
    if new["release_check"]["enabled"] and not cur["release_check"]["enabled"]:
        # turning it on checks at once (the made-up check answers at once)
        st.release_checked_ms = req.now
        n = release_notice(req)
        if n:
            req.app.events.publish("notice", n)
    elif cur["release_check"]["enabled"] and not new["release_check"]["enabled"]:
        st.release_checked_ms = None  # off drops the result
    return copy.deepcopy(new)


def valid_zone(z):
    try:
        ZoneInfo(z)
        return True
    except (ZoneInfoNotFoundError, ValueError, TypeError):
        return False


# the ready-made online tile sources, as the real server's places::tiles::PRESETS
ESRI_LABELS = [
    "https://server.arcgisonline.com/ArcGIS/rest/services/Reference/World_Boundaries_and_Places/MapServer/tile/{z}/{y}/{x}",
    "https://server.arcgisonline.com/ArcGIS/rest/services/Reference/World_Transportation/MapServer/tile/{z}/{y}/{x}",
]
TILE_PRESETS = [
    {"id": "satellite", "name": "Satellite (Esri World Imagery)", "short": "Satellite", "kind": "satellite",
     "url": "https://server.arcgisonline.com/ArcGIS/rest/services/World_Imagery/MapServer/tile/{z}/{y}/{x}",
     "attribution": "Imagery © Esri, Maxar, Earthstar Geographics, and the GIS User Community", "max_zoom": 19,
     "terms": "https://www.esri.com/en-us/legal/terms/full-master-agreement", "overlays": ESRI_LABELS,
     "region": None},
    {"id": "satellite_us", "name": "Satellite, US only (USGS National Map)", "short": "Satellite (US)",
     "kind": "satellite",
     "url": "https://basemap.nationalmap.gov/arcgis/rest/services/USGSImageryOnly/MapServer/tile/{z}/{y}/{x}",
     "attribution": "Imagery: USGS The National Map (public domain)", "max_zoom": 16,
     "terms": "https://www.usgs.gov/information-policies-and-instructions/copyrights-and-credits", "overlays": [],
     "region": "US"},
    {"id": "osm", "name": "OpenStreetMap standard", "short": "Map", "kind": "map",
     "url": "https://tile.openstreetmap.org/{z}/{x}/{y}.png",
     "attribution": "© OpenStreetMap contributors", "max_zoom": 19,
     "terms": "https://operations.osmfoundation.org/policies/tiles/", "overlays": [], "region": None},
    {"id": "opentopomap", "name": "OpenTopoMap (terrain)", "short": "Terrain", "kind": "map",
     "url": "https://tile.opentopomap.org/{z}/{x}/{y}.png",
     "attribution": "Map data: © OpenStreetMap contributors, SRTM | Map style: © OpenTopoMap (CC-BY-SA)",
     "max_zoom": 17, "terms": "https://opentopomap.org/about", "overlays": [], "region": None},
]
DEFAULT_PRESET = "osm"


def tile_chosen_id(t):
    """As the real server's Tiles::chosen_id: the preset an admin picked, else the OpenStreetMap street map (also over the old
    implicit OpenStreetMap / OpenTopoMap default); an address of their own is `custom`."""
    p = t.get("preset")
    if p == "custom" or any(x["id"] == p for x in TILE_PRESETS):
        return p
    url = t.get("online_url")
    m = next((x for x in TILE_PRESETS if x["url"] == url), None)
    if url is None or (m and m["id"] in ("osm", "opentopomap")):
        return DEFAULT_PRESET
    return m["id"] if m else "custom"


def tile_style(t, sid):
    if sid == "custom":
        if not t.get("online_url"):
            return None
        return {"id": "custom", "name": "Custom tile server", "short": "Map", "kind": "map",
                "template": t["online_url"], "attribution": t["attribution"], "max_zoom": t["max_zoom"],
                "overlays": [], "imagery": False, "region": None}
    p = next((x for x in TILE_PRESETS if x["id"] == sid), None)
    if p is None:
        return None
    return {"id": p["id"], "name": p["name"], "short": p["short"], "kind": p["kind"], "template": p["url"],
            "attribution": p["attribution"], "max_zoom": p["max_zoom"],
            "overlays": list(p["overlays"]) if t.get("labels", True) else [], "imagery": p["kind"] == "satellite",
            "region": p["region"]}


def tile_viewer_styles(t):
    first = tile_chosen_id(t)
    out = [s for s in [tile_style(t, first)] if s]
    for sid in t.get("styles", ["osm", "satellite"]):
        if sid != first and (s := tile_style(t, sid)):
            out.append(s)
    return out


def tile_hosts(t):
    out = []
    for s in tile_viewer_styles(t):
        for u in [s["template"], *s["overlays"]]:
            h = re.sub(r"^https?://([^/:]+).*$", r"\1", u).lower()
            if h not in out:
                out.append(h)
    return out


# the made-up offline map file (--map-tiles offline): one raster file, as the real server reports a .pmtiles in data/maps
MAP_FILES = [{"name": "bay-area.pmtiles", "bytes": 48_200_000}]
OFFLINE_TILE_URL = "/api/v1/maps/tiles/{z}/{x}/{y}"


def maps_config(req):
    ss = req.store.server_settings
    t, g = ss["tiles"], ss["geocode"]
    present = req.store.map_file
    online = t["mode"] == "online"
    chosen = tile_style(t, tile_chosen_id(t))
    files = copy.deepcopy(MAP_FILES) if present else []
    return {"tiles": {"mode": t["mode"],
                      "offline_url": OFFLINE_TILE_URL if present and t["mode"] != "online" else None,
                      "offline_present": present, "offline_data_present": present,
                      "offline_file": files[0]["name"] if files else None, "offline_choice": t.get("offline_file"),
                      "offline_files": files, "offline_kind": "raster" if present else None,
                      "offline_min_zoom": 0 if present else None, "offline_max_zoom": 16 if present else None,
                      "offline_bounds": [-122.6, 37.6, -122.3, 37.9] if present else None, "offline_error": None,
                      "online_template": chosen["template"] if online and chosen else None,
                      "attribution": chosen["attribution"] if chosen else t["attribution"],
                      "max_zoom": chosen["max_zoom"] if chosen else t["max_zoom"],
                      "preset": tile_chosen_id(t), "labels": t.get("labels", True),
                      "allowed_styles": list(t.get("styles", ["osm", "satellite"])),
                      "overlays": chosen["overlays"] if online and chosen else [],
                      "imagery": bool(online and chosen and chosen["imagery"]),
                      "styles": tile_viewer_styles(t) if online else [],
                      "hosts": tile_hosts(t) if online else [],
                      "presets": copy.deepcopy(TILE_PRESETS)},
            "geocoding": {"mode": g["mode"], "credit": "GeoNames (CC BY 4.0)" if g["mode"] == "offline" else
                          "The address service set in Settings", "offline_data_present": True,
                          "language": g["language"]}}


def _tile_png(z, x, y):
    """A 256 x 256 made-up map tile: pale land with a darker grid line on its top and left edge."""
    import zlib
    import struct
    land, line = (0xE9, 0xE6, 0xDF), (0xB8, 0xC4, 0xCC)
    row_line = b"\x00" + bytes(line) * 256
    row_land = b"\x00" + bytes(line) * 2 + bytes(land) * 254
    raw = row_line * 2 + row_land * 254

    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data) & 0xFFFFFFFF)
    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", 256, 256, 8, 2, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(raw, 6)) + chunk(b"IEND", b""))


def map_tile(req):
    """GET /maps/tiles/{z}/{x}/{y}: a tile of the made-up offline map file (as the real server's, 204 outside it)."""
    t = req.store.server_settings["tiles"]
    if not req.store.map_file or t["mode"] == "online":
        raise ApiError(404, "no_offline_map", "No offline map file is in use.")
    try:
        z, x, y = (int(req.params[k]) for k in ("z", "x", "y"))
    except ValueError:
        raise ApiError(404, "no_such_tile", "No such tile.")
    if not 0 <= z <= 30 or not 0 <= x < 2 ** z or not 0 <= y < 2 ** z:
        raise ApiError(404, "no_such_tile", "No such tile.")
    if z > 16:
        return Response(204, b"", "application/octet-stream", {"Cache-Control": "private, max-age=86400"})
    return Response(200, _tile_png(z, x, y), "image/png", {"Cache-Control": "private, max-age=86400"})


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
            if not getattr(req.store, "cert_own", False):
                continue  # a certificate the server makes is valid until 4096: only the owner's own expires
            days = max(0, (n["expires_ms"] - req.now) // DAY)
            n["message"] = f"The HTTPS certificate expires in {days} days. Renew it in Settings > Server."
            n["detail"] = {"expires_ms": n.pop("expires_ms"), "days": days}
        items.append(n)
    rn = release_notice(req)
    if rn:
        items.append(rn)
    # the move is an admin's to read, like /system/move
    if req.is_admin:
        from .moving import move_notices
        items += move_notices(req)
        from .tesla import notices as tesla_notices
        items += tesla_notices(req)
        from .teslafi import notices as teslafi_notices
        items += teslafi_notices(req)
        from .teslamate import notices as teslamate_notices
        items += teslamate_notices(req)
        from .fullrebuild import notices as full_notices
        items += full_notices(req)
        from .repair import notices as repair_notices
        items += repair_notices(req)
    return {"items": items, "next": None}


# ------------------------------------------------------------------ build and release check

# this build (made up): what the real server bakes in at image build (crates/release)
BUILD = {"version": "0.1.0", "number": 41, "commit": "2f44a52c0e9b7d1a6f3e8c5b4a2d9e7f1c0b3a68", "short": "2f44a52",
         "built_days_ago": 2, "dev": False}
# the newest build on main the made-up check finds
NEWEST = {"commit": "9c1e7d3b5a2f4e8c6d0b1a3f5e7c9d2b4a6f8e01", "short": "9c1e7d3", "build": 42, "days_ago": 1}
NEWEST_TITLES = [
    "Battery: one counter per stream, so imported days no longer stop the battery pages",
    "Odometer: read the CAN odometer's \"not available\" as such, drop impossible readings",
    "Charges: the place price follows a renamed place",
    "Settings: the release check asks GitHub once a day when it is on",
]
PULL = "docker compose pull && docker compose up -d"
IMAGE = "ghcr.io/skittles-fivem/sentry-mate:latest"
REPO_URL = "https://github.com/skittles-fivem/Sentry-Mate"


def build_info(req):
    b = dict(BUILD)
    b["built_ms"] = req.app.cfg.now_ms - b.pop("built_days_ago") * DAY
    return b


def release_state(req):
    """GET /release: the release check's last result (docs/ui-api.md). Made up: the result is --release-check's."""
    st = req.store
    b = build_info(req)
    current = {k: b[k] for k in ("commit", "short", "built_ms", "number")}
    on = bool(st.server_settings["release_check"]["enabled"])
    out = {"enabled": on, "status": "off", "checked_ms": None, "next_ms": None, "current": current, "newest": None,
           "pull": PULL, "image": IMAGE, "error": None}
    if not on:
        return out
    res = st.release_result
    checked = st.release_checked_ms
    out["checked_ms"] = checked
    out["next_ms"] = (checked if checked is not None else req.now) + DAY
    if res == "never" or (checked is None and res != "checking"):
        out["status"] = "never"
        out["checked_ms"] = None
        return out
    if res == "checking":
        out["status"] = "checking"
        return out
    if res == "error":
        out["status"] = "error"
        out["error"] = {"code": "rate_limited",
                        "message": "GitHub's rate limit for this address is used up; trying again in 40 minutes."}
        out["next_ms"] = req.now + 40 * MIN
        return out
    newest_ms = req.app.cfg.now_ms - NEWEST["days_ago"] * DAY
    if res == "up_to_date":
        out["status"] = "up_to_date"
        out["newest"] = {"commit": b["commit"], "short": b["short"], "date_ms": b["built_ms"], "build": b["number"],
                         "changes": 0, "titles": [], "more": 0,
                         "compare_url": f"{REPO_URL}/compare/{b['commit']}...{b['commit']}"}
        return out
    out["status"] = "newer"
    out["newest"] = {"commit": NEWEST["commit"], "short": NEWEST["short"], "date_ms": newest_ms,
                     "build": NEWEST["build"], "changes": len(NEWEST_TITLES), "titles": list(NEWEST_TITLES),
                     "more": 0, "compare_url": f"{REPO_URL}/compare/{b['commit']}...{NEWEST['commit']}"}
    return out


def release_check_now(req):
    """POST /release/check (admin): checks at once; 409 release_check_off while the check is off."""
    if not req.store.server_settings["release_check"]["enabled"]:
        raise conflict("release_check_off", "The release check is off. Turn it on in Settings: Display and units.")
    if req.store.release_result != "checking":
        req.store.release_checked_ms = req.now
    req.store.record(req.account, "release-checked", "server:release", "Checked GitHub for a newer build.")
    out = release_state(req)
    n = release_notice(req)
    if n:
        req.app.events.publish("notice", n)
    return out


def release_notice(req):
    """The release notice, for admins only, while the check is on and found a newer build."""
    if not req.is_admin:
        return None
    s = release_state(req)
    if s["status"] != "newer":
        return None
    nw = s["newest"]
    import datetime
    d = datetime.datetime.fromtimestamp(nw["date_ms"] / 1000, datetime.timezone.utc)
    # named by its number, as the real server does (crates/release Newest::summary); the commit is in `detail`
    when = f"{d.strftime('%b')} {d.day}" + ("" if d.year == datetime.datetime.fromtimestamp(
        req.app.cfg.now_ms / 1000, datetime.timezone.utc).year else f", {d.year}")
    changes = "1 change" if nw["changes"] == 1 else f"{nw['changes']} changes"
    number = f"build {nw['build']}" if nw.get("build") is not None else ""
    msg = f"A newer build is ready: {', '.join(x for x in (number, when, changes) if x)}."
    return {"code": "release_available", "level": "info", "message": msg,
            "since_ms": nw["date_ms"], "fix_link": "/help#about",
            "detail": {"version": nw["short"], "commit": nw["commit"], "build": nw["build"],
                       "changes": nw["changes"], "notes_url": nw["compare_url"]}}


def about(req):
    packs = req.app.packs
    own = req.store.get_prefs(req.account).get("ui_pack") if req.account else None
    m = packs.get(packs.pick(own))
    pack = {"id": m["id"], "name": m["name"], "version": m["version"], "api": m["api"], "original": False}
    return {
        "version": DEV_VERSION, "api_version": API_VERSION, "ui_pack": pack, "dev_server": True,
        "build": build_info(req),
        "links": {"source": None, "docs": "/help"},
        "licence": {"name": None, "url": None},
        "licences": [{"name": "model3dbc (Tesla Model 3 and Y signal list)", "licence": "MIT"},
                     {"name": "opendbc (Tesla signal lists)", "licence": "MIT"}],
        "data_credits": [{"name": "OpenStreetMap", "text": "Map data © OpenStreetMap contributors", "licence": "ODbL"},
                         {"name": "GeoNames", "text": "Place names for offline address lookup", "licence": "CC BY 4.0"}],
    }


def ui_packs(req):
    """Every pack on the server, for anyone signed in (crates/web/src/packs list())."""
    return req.app.packs.listing()


def ui_pack_download(req):
    """A pack as a .zip, for anyone signed in (crates/web/src/packs download())."""
    import io
    import os
    import zipfile
    pid = req.params["pack"]
    p = req.app.packs.get(pid)
    if p is None:
        raise not_found("no_such_pack", "No such UI pack.")
    buf = io.BytesIO()
    with zipfile.ZipFile(buf, "w", zipfile.ZIP_DEFLATED) as z:
        for root, _dirs, files in os.walk(p["folder"]):
            for f in sorted(files):
                full = os.path.join(root, f)
                z.write(full, os.path.relpath(full, p["folder"]))
    return Response(200, buf.getvalue(), "application/zip",
                    {"Content-Disposition": f'attachment; filename="{pid}-{p.get("version", "0")}.zip"'})


# ------------------------------------------------------------------ per-car health


# the car with logs missing on the server (ingest::missing): three still asked of the car, one it no longer has
MISSING_CAR = "c2"


def missing_logs(req, cid):
    """The "Missing on the server" figures of `/cars/{car}/health` (zeros for every car but MISSING_CAR)."""
    now = req.now
    # the daily look, at 04:00 UTC
    scanned = now - now % DAY + 4 * HOUR - (DAY if now % DAY < 4 * HOUR else 0)
    if cid != MISSING_CAR:
        return {"count": 0, "oldest_ms": None, "found_ms": None, "asked": False, "asked_ms": None, "gone": 0,
                "gone_oldest_ms": None, "scanned_ms": scanned}
    return {"count": 3, "oldest_ms": now - 9 * DAY - 5 * HOUR, "found_ms": scanned - 2 * DAY, "asked": True,
            "asked_ms": scanned - 2 * DAY + 7 * MIN, "gone": 1, "gone_oldest_ms": now - 23 * DAY - 2 * HOUR,
            "scanned_ms": scanned}


def missing_count(req, cid):
    return missing_logs(req, cid)["count"]


LOG_MS = 10 * MIN  # a made-up CAN Bridge log's length


def log_checks(req, h, waiting):
    """`log_checks` of `/cars/{car}/health`: every log as the CAN Bridge app is told it now, counted (one log per
    ten minutes of history; a failed_check stretch is a run of failed logs, unsigned; waiting logs are pending)."""
    out = {"verified": 0, "pending": waiting, "failed": 0, "mismatch": 0, "bad_signature": 0, "unsigned": 0,
           "chain_break": 0, "failed_runs": [], "runs_total": 0}
    if h is None or not h.segs:
        return out
    first = h.segs[0].start
    for s in h.segs:
        if s.start > req.now:
            break
        if s.kind == "gap" and s.reason == "failed_check":
            end = min(s.end, req.now)
            n = max(1, (end - s.start) // LOG_MS)
            out["failed"] += n
            out["unsigned"] += n
            out["failed_runs"].append({"first_start_ms": s.start, "last_start_ms": s.start + (n - 1) * LOG_MS,
                                       "logs": n, "mismatch": 0, "bad_signature": 0, "unsigned": n,
                                       "chain_break": 0})
    out["runs_total"] = len(out["failed_runs"])
    out["failed_runs"] = out["failed_runs"][-200:]
    out["verified"] = max(0, (req.now - first) // LOG_MS - out["failed"] - waiting)
    return out


def car_health(req):
    st = req.store
    cid = req.car
    h = req.history()
    a, b = req.period(30 * DAY)
    keys = [k for k in st.keys if k["car"] == cid]
    toks = [t for t in st.tokens.get(cid, []) if not t["revoked"]]
    waiting = sum(k["logs_waiting"] for k in keys)
    pending = any(rb.get("pending_until", 0) > req.now for rb in st.rebuilds.get(cid, []))
    if req.app.cfg.rebuild_running and cid in req.app.ds.featured:
        pending = True  # --rebuild-running: a rebuild that never ends
    if h is None:
        from ..live import list_item
        it = list_item(req.app, cid, req.now)
        reasons = [c for c in it["attention"] if c in ("upload_silence", "trust_failed")]
        return {"car": cid, "time_zone": req.tz(), "healthy": not reasons, "reasons": reasons,
                "last_upload_ms": it["now"]["received_ms"], "newest_log_ms": it["now"]["received_ms"],
                "last_log_trust": {"result": "failed" if "trust_failed" in reasons else "verified", "reason": None},
                "keys": [], "chain_gaps": [], "chain_gaps_total": 0, "dropped_frames": [],
                "dropped_frames_total": 0, "unknown_spans": [], "unknown_spans_total": 0, "unknown_ms": 0,
                "logs_waiting": 0, "missing_logs": missing_logs(req, cid), "log_checks": log_checks(req, None, 0),
                "rebuild_pending": pending, "rebuild_speed": rebuild_speed(pending),
                "gps_disagreement": gps_disagreement(req, cid), "uploads_enabled": True}
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
    missing = missing_logs(req, cid)
    if missing["count"]:
        reasons.append("logs_missing")
    disagree = gps_disagreement(req, cid)
    if disagree:
        reasons.append("gps_inputs_disagree")
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
    cap = req.app.cfg.health_list_max
    return {
        "car": cid, "time_zone": req.tz(), "healthy": not reasons, "reasons": reasons, "last_upload_ms": last_upload,
        # the bridge's last request and the newest log's arrival: one and the same here (a log a minute while awake)
        "newest_log_ms": last_upload, "last_log_trust": trust,
        "keys": [{"key": k["key"], "last_upload_ms": k["last_upload_ms"], "newest_log_ms": k["last_upload_ms"],
                  "preferred": k["preferred"],
                  "device": k["device"], "device_label": device_label(k), "logs_waiting": k["logs_waiting"]}
                 for k in keys],
        # each list holds the newest few (the real server: 500); its *_total counts the whole period
        "chain_gaps": chain[-cap:], "chain_gaps_total": len(chain),
        "dropped_frames": dropped[-cap:], "dropped_frames_total": len(dropped),
        "unknown_spans": unknown[-cap:], "unknown_spans_total": len(unknown),
        "unknown_ms": sum(((u["end_ms"] or req.now) - u["start_ms"]) for u in unknown),
        "logs_waiting": waiting, "missing_logs": missing, "log_checks": log_checks(req, h, waiting),
        "rebuild_pending": pending, "rebuild_speed": rebuild_speed(pending), "gps_disagreement": disagree,
        "uploads_enabled": bool(toks),
    }


def rebuild_speed(pending):
    """While a rebuild is pending: how fast its logs go now (the full rebuild's `speed`, docs/ui-api.md)."""
    if not pending:
        return None
    return {"logs_per_s": 1.1, "stage": "windows", "stage_share_pct": 72, "over_ms": 30000}


def rebuild(req):
    st = req.store
    from .fullrebuild import status as full_status
    if full_status(req)["state"] == "running":
        raise ApiError(409, "full_rebuild_running", "A full rebuild is under way: it makes the whole history again.")
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
    # the period as it was asked for, as the server's audit entry says it (an open end is said as one)
    def day(ms):
        return time.strftime("%Y-%m-%d UTC", time.gmtime(ms / 1000))
    if "from_ms" in body and "to_ms" in body:
        n = max(1, -(-(b - a) // DAY))
        span = f"{n} day{'' if n == 1 else 's'} from {day(a)}"
    elif "from_ms" in body:
        span = f"everything from {day(a)} on"
    elif "to_ms" in body:
        span = f"everything up to {day(b)}"
    else:
        span = "the whole history"
    st.record(req.account, "car-rebuild", f"car:{req.car}",
              f"Rebuilt {st.car_meta(req.car)['name']}'s drives, charges, states and visits "
              f"from the stored logs ({span}).")
    req.app.events.publish("rows", {"car": req.car, "kinds": job["kinds"], "from_ms": a, "to_ms": b}, car=req.car)
    logs = int(days * 1440 * 0.6)
    return {"car": req.car, "job": job["id"], "status": "done", "from_ms": a, "to_ms": b, "kinds": job["kinds"],
            "logs_read": logs, "logs_total": logs, "took_ms": took}


# ------------------------------------------------------------------ the car's own GPS layout

# a layout as `sentry-mate gps-layout` prints it: "lat 0|28@1- (1e-6,0); lon 28|28@1- (1e-6,16.777216)", with an
# optional "; accuracy ..." field (docs/ui-api.md: /cars/{car}/gps-layout)
_NUM = r"-?\d+(?:\.\d+)?(?:e-?\d+)?"
_FIELD = rf"\d+\|\d+@[01][+-] \({_NUM},{_NUM}\)"
_GPS_LAYOUT = re.compile(rf"lat {_FIELD}; lon {_FIELD}(?:; accuracy {_FIELD})?")


def _gps_layouts(st):
    if not hasattr(st, "gps_layouts"):
        st.gps_layouts = {}
    return st.gps_layouts


def _gps_layout_set(st):
    if not hasattr(st, "gps_layout_set"):
        st.gps_layout_set = {}
    return st.gps_layout_set


def get_gps_layout(req):
    st = req.store
    return {"car": req.car, "layout": _gps_layouts(st).get(req.car), "set_ms": _gps_layout_set(st).get(req.car)}


def put_gps_layout(req):
    st = req.store
    body = req.json()
    bad = {k: "Unknown field." for k in body if k != "layout"}
    if bad:
        raise bad_input(bad)
    if "layout" not in body:
        raise invalid({"layout": "Required: the layout, or null for the DBC's."})
    layout = body["layout"]
    if layout is not None:
        if not isinstance(layout, str):
            raise invalid({"layout": "Must be text or null."})
        layout = layout.strip()
        if not _GPS_LAYOUT.fullmatch(layout):
            raise invalid({"layout": "Write it as sentry-mate gps-layout prints it: lat start|width@order sign "
                                     "(scale,offset); lon ..."})
    layouts = _gps_layouts(st)
    set_at = _gps_layout_set(st)
    if layouts.get(req.car) == layout:
        return {"car": req.car, "layout": layout, "set_ms": set_at.get(req.car), "job": None}
    before = layouts.get(req.car)
    with st.lock:
        if layout is None:
            layouts.pop(req.car, None)
        else:
            layouts[req.car] = layout
        set_at[req.car] = req.now
        _gps_runs(req)
        st.gps_changes.setdefault(req.car, []).append(
            {"at_ms": req.now, "by": req.account["name"], "layout": layout, "before": before, "fit": None})
    name = st.car_meta(req.car)["name"]
    st.record(req.account, "car-gps-layout", f"car:{req.car}",
              f"Set how {name}'s Screenmate GPS frames are read ({layout}); its history is rebuilt." if layout else
              f"Set {name}'s Screenmate GPS frames to be read as the DBC says; its history is rebuilt.")
    return {"car": req.car, "layout": layout, "set_ms": req.now, "job": st.new_id("rb")}


# ------------------------------------------------------------------ finding the GPS layout (Data health)

# --gps-fit: the featured cars' Screenmate GPS out of step with their API input, and the fit not run, under way or
# done. A fit started here takes GPS_FIT_MS of the virtual clock (a running one from --gps-fit running never ends).
# Its result is made up: layouts, counts and metres only, as the server answers (docs/ui-api.md).
GPS_FIT_MS = 8_000
_FIT_LAYOUT = "lat 0|28@1- (1e-6,0); lon 28|28@1- (1e-6,16.777216)"


def gps_disagreement(req, cid):
    if req.app.cfg.gps_fit == "none" or cid not in req.app.ds.featured:
        return None
    return {"disagree": ["smo", "tesla_api"], "not_used": ["smo"], "from_ms": req.now - 3 * DAY,
            "to_ms": req.now - 2 * DAY}


def _fit_source(name, positions, pairs=0, trimmed=0, errors=(None, None, None, None), left_out=None):
    lat_median, lat_p95, lon_median, lon_p95 = errors
    return {"input": name, "positions": positions, "pairs": pairs, "trimmed": trimmed, "lat_median_m": lat_median,
            "lat_p95_m": lat_p95, "lon_median_m": lon_median, "lon_p95_m": lon_p95, "left_out": left_out}


def _fit_line(bits, slope, intercept, constant):
    # the straight line from the raw values of the best's bits to degrees (docs/ui-api.md: gps-layout/fit)
    return {"bits": bits, "slope": slope, "intercept": intercept, "nominal": 1e-6, "scale_error": 0.0,
            "median_m": 3, "p95_m": 9, "mad_m": 2,
            "drift": {"scale": 1e-6, "quarters_m": [0, 0, 1, 0], "per_km_north_m": 0, "per_km_value_m": 0,
                      "constant": constant != 0}}


def _fit_result(cid, days, variant="good", logs=412):
    r = {
        "car": cid, "days": days, "logs": logs, "logs_skipped": 0, "days_read": 58, "days_skipped": 2,
        "pairs": 18_240, "paired": 19_020, "trimmed": 37,
        "sources": [_fit_source("teslamate-local", 0), _fit_source("tesla-api", 21_904, 18_240, 37, (3, 9, 4, 11)),
                    _fit_source("teslafi-api", 0), _fit_source("teslamate", 0),
                    _fit_source("teslafi", 0, left_out="its positions disagree with the live inputs' at the same moments")],
        "enough": True, "spread_ns_m": 8_400, "spread_ew_m": None, "weak": False,
        "lat": {"dbc": {"layout": "0|28@1- (1e-6,0)", "median_m": 3, "p95_m": 9},
                "best": {"layout": "0|28@1- (1e-6,0)", "median_m": 3, "p95_m": 9},
                "runner_up": {"layout": "0|27@1- (1e-6,0)", "median_m": 3, "p95_m": 9},
                "line": _fit_line("0|28@1-", 1e-6, 0.0, 0.0)},
        "lon": {"dbc": {"layout": "28|28@1- (1e-6,0)", "median_m": None, "p95_m": None},
                "best": {"layout": "28|28@1- (1e-6,16.777216)", "median_m": 4, "p95_m": 11},
                "runner_up": None, "line": _fit_line("28|28@1-", 1e-6, 16.777216, 16.777216)},
        "layout": _FIT_LAYOUT, "median_m": 5, "p95_m": 14, "is_dbc": False, "good": True,
    }
    if variant == "weak":
        r.update(weak=True, spread_ns_m=310, pairs=2_104, paired=2_240, trimmed=4, median_m=6, p95_m=18)
    elif variant == "far":
        r.update(good=False, median_m=410, p95_m=650, pairs=5_880, paired=6_100, trimmed=12)
        r["lon"]["best"] = {"layout": "28|27@1- (2e-6,0)", "median_m": 380, "p95_m": 620}
        r["layout"] = "lat 0|28@1- (1e-6,0); lon 28|27@1- (2e-6,0)"
    return r


def _gps_runs(req):
    """The fits of every car (docs/ui-api.md: every run is kept), oldest first, seeded by --gps-fit."""
    st = req.store
    if not hasattr(st, "gps_runs"):
        st.gps_runs = {}
        st.gps_changes = {}
        st.gps_fit_seq = 0
        mode = req.app.cfg.gps_fit
        for cid in req.app.ds.featured:
            if mode in ("running", "done"):
                run = _new_run(st, req.now - 60_000, 30, None if mode == "running" else req.now - 30_000)
                if mode == "running":
                    # the owner's report: 272 of 22 402 logs read, started a minute ago
                    run.update(logs=22_402, positions=155, stuck_done=272)
                st.gps_runs[cid] = [run]
            elif mode == "history":
                runs = []
                for ago, days, variant in ((9 * DAY, 30, "far"), (6 * DAY, 7, "weak"), (3 * DAY, 30, "good"), (45_000, 14, "good")):
                    run = _new_run(st, req.now - ago - 4 * MIN, days, req.now - ago, variant)
                    runs.append(run)
                third = runs[2]
                third["applied_ms"], third["applied_by"] = req.now - 3 * DAY + 2 * MIN, "admin"
                st.gps_runs[cid] = runs
                st.gps_changes[cid] = [
                    {"at_ms": req.now - 40 * DAY, "by": "admin", "layout": "lat 0|27@1- (2e-6,0); lon 28|28@1- (1e-6,0)",
                     "before": None, "fit": None},
                    {"at_ms": third["applied_ms"], "by": "admin", "layout": _FIT_LAYOUT,
                     "before": "lat 0|27@1- (2e-6,0); lon 28|28@1- (1e-6,0)", "fit": third["id"]},
                ]
                _gps_layouts(st)[cid] = _FIT_LAYOUT
                _gps_layout_set(st)[cid] = third["applied_ms"]
    return st.gps_runs


def _new_run(st, started, days, ends, variant="good"):
    st.gps_fit_seq += 1
    return {"id": str(st.gps_fit_seq), "n": st.gps_fit_seq, "days": days, "started_ms": started, "ends_ms": ends,
            "variant": variant, "logs": 412, "positions": 155, "stuck_done": None, "applied_ms": None,
            "applied_by": None}


def _fit_status(req, cid, run=None):
    runs = _gps_runs(req).get(cid) or []
    f = run or (runs[-1] if runs else None)
    current = _gps_layouts(req.store).get(cid)
    busy = "rebuild 34 %" if req.app.cfg.full_rebuild == "running" else None
    if f is None:
        return {"car": cid, "state": "idle", "id": None, "days": None, "phase": None, "done": 0, "total": 0,
                "positions_total": 0, "logs_total": 0, "percent": 0, "eta_ms": None, "started_ms": None,
                "finished_ms": None, "error": None, "result": None, "current": current, "applied": False,
                "applied_ms": None, "applied_by": None, "server_busy": busy, "waiting": None}
    ends = f["ends_ms"]
    p_total, l_total = f["positions"], f["logs"]
    fit_units = max(1, (p_total + l_total) // 50)
    all_units = p_total + l_total + fit_units
    if ends is None or req.now < ends:
        # positions are the first 2 % of the time, logs to 97 %, then the fit (a never-ending one stays in the logs)
        dur = None if ends is None else ends - f["started_ms"]
        part = 0.0 if dur is None else max(0.0, min(1.0, (req.now - f["started_ms"]) / dur))
        eta = None
        if dur is None:
            phase, total, done = "logs", l_total, f["stuck_done"] or l_total // 3
            units = p_total + done
            # the rate measured since the logs began (5 s after the start)
            spent = max(1, req.now - f["started_ms"] - 5_000)
            eta = int((l_total - done) * spent / done)
        elif part < 0.02:
            phase, total = "positions", p_total
            done = int(p_total * part / 0.02)
            units = done
        elif part < 0.97:
            phase, total = "logs", l_total
            frac = (part - 0.02) / 0.95
            done = int(l_total * frac)
            units = p_total + done
            if done:
                spent = int(dur * 0.95 * frac)
                eta = int((l_total - done) * spent / done)
        else:
            phase, total, done, units = "fitting", 0, 0, p_total + l_total
        pct = int(100 * units / all_units)
        return {"car": cid, "state": "running", "id": f["id"], "days": f["days"], "phase": phase, "done": done,
                "total": total, "positions_total": p_total, "logs_total": l_total, "percent": pct, "eta_ms": eta,
                "started_ms": f["started_ms"], "finished_ms": None, "error": None, "result": None,
                "current": current, "applied": False, "applied_ms": None, "applied_by": None, "server_busy": busy, "waiting": None}
    result = _fit_result(cid, f["days"], f["variant"], f["logs"])
    return {"car": cid, "state": "done", "id": f["id"], "days": f["days"], "phase": None, "done": 0, "total": 0,
            "positions_total": p_total, "logs_total": l_total, "percent": 100, "eta_ms": None,
            "started_ms": f["started_ms"], "finished_ms": ends, "error": None,
            "result": result, "current": current,
            "applied": f["applied_ms"] is not None or current == result["layout"],
            "applied_ms": f["applied_ms"], "applied_by": f["applied_by"], "server_busy": busy, "waiting": None}


def get_gps_fit(req):
    return _fit_status(req, req.car)


def post_gps_fit(req):
    body = req.json()
    bad = {k: "Unknown field." for k in body if k != "days"}
    if bad:
        raise bad_input(bad)
    days = body.get("days")
    if days is None:
        days = 30
    if not isinstance(days, int) or isinstance(days, bool) or not 1 <= days <= 366:
        raise invalid({"days": "A whole number of days, 1 to 366."})
    st = req.store
    runs = _gps_runs(req)
    cur = _fit_status(req, req.car)
    if cur["state"] != "running":
        with st.lock:
            runs.setdefault(req.car, []).append(_new_run(st, req.now, days, req.now + GPS_FIT_MS))
    return Response(202, json_bytes(_fit_status(req, req.car)))


def _fit_row(req, cid, run):
    s = _fit_status(req, cid, run)
    r = s["result"] or {}
    return {"id": s["id"], "car": cid, "state": s["state"], "days": s["days"], "percent": s["percent"],
            "started_ms": s["started_ms"], "finished_ms": s["finished_ms"], "error": s["error"],
            "logs": r.get("logs"), "pairs": r.get("pairs"), "layout": r.get("layout"), "median_m": r.get("median_m"),
            "p95_m": r.get("p95_m"), "is_dbc": r.get("is_dbc"), "enough": r.get("enough"), "weak": r.get("weak"),
            "good": r.get("good"), "applied": s["applied"], "applied_ms": s["applied_ms"],
            "applied_by": s["applied_by"]}


def _find_run(req, fit):
    for run in _gps_runs(req).get(req.car, []):
        if run["id"] == fit:
            return run
    raise not_found("no_such_fit")


def get_gps_fits(req):
    runs = list(_gps_runs(req).get(req.car, []))
    items, nxt = page(req, runs, lambda r: (r["n"],), reverse=True)
    return {"items": [_fit_row(req, req.car, r) for r in items], "next": nxt, "total": len(runs)}


def get_gps_fit_one(req):
    return _fit_status(req, req.car, _find_run(req, req.params["fit"]))


def delete_gps_fit(req):
    body = req.json(required=False)
    if body:
        raise bad_input({k: "Unknown field." for k in body})
    run = _find_run(req, req.params["fit"])
    if _fit_status(req, req.car, run)["state"] == "running":
        raise conflict("fit_running", "This fit is still under way: wait for it to end.")
    with req.store.lock:
        _gps_runs(req)[req.car].remove(run)
    name = req.store.car_meta(req.car)["name"]
    req.store.record(req.account, "car-gps-fit-removed", f"car:{req.car}", f"Removed a past GPS layout fit of {name}.")
    return {"car": req.car, "id": run["id"], "removed": True}


def apply_gps_fit(req):
    from .tesla import reconfirm
    st = req.store
    body = req.json(required=False)
    bad = {k: "Unknown field." for k in body if k not in ("confirm_password", "code")}
    if bad:
        raise bad_input(bad)
    run = _find_run(req, req.params["fit"])
    s = _fit_status(req, req.car, run)
    if s["state"] != "done":
        raise conflict("fit_not_done", "This fit has no result to apply.")
    reconfirm(req, body)
    layout = s["result"]["layout"]
    layouts = _gps_layouts(st)
    set_at = _gps_layout_set(st)
    job = None
    if layouts.get(req.car) != layout:
        before = layouts.get(req.car)
        with st.lock:
            layouts[req.car] = layout
            set_at[req.car] = req.now
            _gps_runs(req)
            st.gps_changes.setdefault(req.car, []).append(
                {"at_ms": req.now, "by": req.account["name"], "layout": layout, "before": before, "fit": run["id"]})
        name = st.car_meta(req.car)["name"]
        st.record(req.account, "car-gps-layout", f"car:{req.car}",
                  f"Set how {name}'s Screenmate GPS frames are read ({layout}), from a past fit; its history is rebuilt.")
        job = st.new_id("rb")
    run["applied_ms"], run["applied_by"] = req.now, req.account["name"]
    return {"car": req.car, "layout": layout, "set_ms": set_at.get(req.car), "job": job, "fit": run["id"]}


def get_gps_changes(req):
    _gps_runs(req)
    rows = [dict(c, n=i) for i, c in enumerate(req.store.gps_changes.get(req.car, []))]
    items, nxt = page(req, rows, lambda r: (r["n"],), reverse=True)
    return {"items": [{k: v for k, v in c.items() if k != "n"} for c in items], "next": nxt, "total": len(rows)}


# ------------------------------------------------------------------ overlaps


def _counts(rows):
    return {"drives": sum(1 for s in rows if s.kind == "drive"), "charges": sum(1 for s in rows if s.kind == "charge")}


def _site_counts(rows):
    return {site: _counts([s for s in rows if SRC.site_of(s) == site]) for site in ("teslamate", "teslafi")}


def _overlap_view(h, i, o, now):
    """A span of live and imported history (`kind: "live"`)."""
    can = [s for s in h.drives + h.charges if s.source == "can" and o["start_ms"] <= s.start < o["end_ms"]]
    tm = [s for s in h.shadow if o["start_ms"] <= s.start < o["end_ms"]]
    # the imported side as the lists show it: where both imports cover a span, only its chosen site's rows
    live, imported = _counts(can), _counts([s for s in tm if h.site_shown(s)])
    sites = _site_counts(tm)
    return {"id": f"overlap:{o['start_ms']}", "kind": "live", "start_ms": o["start_ms"], "end_ms": o["end_ms"],
            "shown": o["shown"], "shown_source": "live" if o["shown"] == "can" else "import:teslamate",
            "default_source": "live", "default_reason": None,
            "live": live, "imported": imported, "sites": sites, "can": dict(live), "teslamate": dict(imported),
            "sources": [SRC.ref("smo")] + [SRC.ref("import", k) for k, n in sites.items() if n["drives"] + n["charges"]],
            "version": h.overlap_changes + 1}


def _pair_view(h, o):
    """A span both imports cover (`kind: "imports"`): which site it shows, and why at first."""
    rows = [s for s in h.drives + h.charges if s.source == "teslamate" and o["start_ms"] <= s.start < o["end_ms"]]
    live = _counts([s for s in h.drives + h.charges if s.source == "can" and o["start_ms"] <= s.start < o["end_ms"]])
    # the shown site's rows (each real drive once, as the lists show it); `sites` has both
    imported = _counts([s for s in rows if SRC.site_of(s) == o["shown"]])
    return {"id": f"imports:{o['start_ms']}", "kind": "imports", "start_ms": o["start_ms"], "end_ms": o["end_ms"],
            "shown": "teslamate", "shown_source": f"import:{o['shown']}", "default_source": f"import:{o['default']}",
            "default_reason": o["reason"], "live": live, "imported": imported, "sites": _site_counts(rows),
            "can": dict(live), "teslamate": dict(imported),
            "sources": [SRC.ref("import", "teslamate"), SRC.ref("import", "teslafi")],
            "version": h.overlap_changes + 1}


def get_overlaps(req):
    h = req.history()
    rows = [_overlap_view(h, i, o, req.now) for i, o in enumerate(h.overlaps)] if h else []
    rows += [_pair_view(h, o) for o in h.import_overlaps] if h else []
    # newest first, as the server lists them
    items, nxt = page(req, rows, key=lambda x: (x["start_ms"], x["id"]), reverse=True)
    return {"car": req.car, "time_zone": req.tz(), "items": items, "next": nxt}


def patch_overlap(req):
    h = req.history()
    body = req.json()
    bad = {k: "Unknown field." for k in body if k not in ("id", "start_ms", "shown", "version")}
    if "shown" not in body:
        bad["shown"] = "Required: live or import:teslamate."
    if "id" not in body and "start_ms" not in body:
        bad["id"] = "Name the overlap by its id (or start_ms)."
    pair = str(body.get("id", "")).startswith("imports:")
    version = body.get("version")
    if version is not None and (not isinstance(version, int) or isinstance(version, bool) or version < 0):
        bad["version"] = "Must be a whole number."
    elif pair and version is None:
        bad["version"] = "Required: the version you loaded."
    if bad:
        raise bad_input(bad)
    if pair:
        shown = {"import:teslamate": "teslamate", "teslamate": "teslamate", "import:teslafi": "teslafi",
                 "teslafi": "teslafi"}.get(body["shown"])
        if shown is None:
            raise invalid({"shown": "Must be import:teslamate or import:teslafi: the import this span shows."})
        spans = [o for o in (h.import_overlaps if h else []) if body["id"] == f"imports:{o['start_ms']}"]
    else:
        shown = {"live": "can", "can": "can", "import": "teslamate", "import:teslamate": "teslamate",
                 "import:teslafi": "teslamate", "teslamate": "teslamate"}.get(body["shown"])
        if shown is None:
            raise invalid({"shown": "Must be live or import:teslamate (or the old words can, teslamate)."})
        spans = [o for o in (h.overlaps if h else [])
                 if body.get("id") == f"overlap:{o['start_ms']}" or body.get("start_ms") == o["start_ms"]]
    if not spans:
        raise not_found("no_such_overlap", "There is no such overlap for this car.")
    o = spans[-1]
    st = req.store
    with st.lock:
        if version is not None and version != h.overlap_changes + 1:
            raise stale_version(h.overlap_changes + 1)
        o["shown"] = shown
        h.overlap_changes += 1
    if pair:
        what = f"the {'TeslaFi' if shown == 'teslafi' else 'TeslaMate'} import"
    else:
        what = f"{'live (SMO)' if shown == 'can' else 'imported TeslaMate'} history"
    st.record(req.account, "car-overlap", f"car:{req.car}",
              f"Showing {what} from {date_str(o['start_ms'], req.tz())} to {date_str(o['end_ms'], req.tz())}.")
    req.app.events.publish("rows", {"car": req.car, "kinds": ["drives", "charges", "states", "visits"],
                                    "from_ms": o["start_ms"], "to_ms": o["end_ms"]}, car=req.car)
    out = get_overlaps(req)
    out["changed"] = _pair_view(h, o) if pair else _overlap_view(h, h.overlaps.index(o), o, req.now)
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
                        rows.append({"series": kind, "source": src, "date": d, "bytes": b_,
                                     "sources": SRC.refs_of(src, None)})
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
    # a mean over the days from the first one listed to today (days without new data too), at most 30
    span = 0
    if rows:
        from datetime import date as _date
        span = min(30, max(1, (_date.fromisoformat(today) - _date.fromisoformat(rows[0]["date"])).days))
    per_day = sum(g["bytes"] for g in rows) / span if rows else 0
    free = free_bytes(req.app, req.now)
    return {"kinds": kinds, "total_bytes": sum(k["bytes"] for k in kinds), "free_bytes": free,
            "growth": rows, "growth_per_day_bytes": int(per_day),
            "full_in_days": int(free / per_day) if per_day else None}


# ------------------------------------------------------------------ storage in detail, and compaction

BLOCK = 4096
# what a compaction pass does to the made-up figures: this share of the logs and sidecars as uploaded goes
# into the day packs (which take less room), and these categories shrink by these shares
PACK_SHARE, PACK_RATIO = 0.8, 0.82
SHRINK = {"series.can.snapshots": 0.18, "series.teslamate.snapshots": 0.12, "curves.can.snapshots": 0.1,
          "rows.snapshots": 0.06, "kv.engine": 0.22, "kv.day-parts": 0.3}
COMPACT_STEPS = ("logs", "series", "records")
COMPACT_UNIT = {"logs": "days", "series": "days", "records": "records"}
STALL_MS = 120_000
# the progress fields of a pass that is not running
_IDLE_PROGRESS = {"phase": None, "step_number": None, "steps": 3, "unit": None, "pass_saved_bytes": None,
                  "rate_per_s": None, "eta_ms": None, "progress_ms": None, "stalled": False, "unit_errors": 0,
                  "last_unit_error": None}


def _detail_base(req):
    """{key: [group, label, files, bytes]} before any compaction pass asked for here: the cars' figures of
    /system/storage split into what lies on disk, plus the server's own files."""
    kinds = {k["kind"]: k["bytes"] for k in system_storage(req)["kinds"]}
    logs, series, rows, imported = kinds["logs"], kinds["series"], kinds["rows"], kinds["imported"]
    n_logs = max(1, logs // 34_400)  # a log a minute while a car is awake, about 34 kB each
    days = max(1, series // 9_000_000)
    packed = int(logs * 0.31)  # older days are packed already (see `saved_bytes`)
    out = {
        "logs.raw": ["logs", "CAN logs as uploaded (signed originals)", n_logs - int(n_logs * 0.31), logs - packed],
        "logs.sidecars": ["logs", "Their sidecars as uploaded", n_logs - int(n_logs * 0.31), (n_logs - int(n_logs * 0.31)) * 412],
        "logs.packs": ["logs", "CAN logs and sidecars as uploaded, packed by day", max(1, days // 3), int(packed * PACK_RATIO)],
        "logs.ledger": ["logs", "Check results and counts of the logs", 3, n_logs * 96],
        "series.can.snapshots": ["series", "Time series from CAN logs: days", days, int(series * 0.93)],
        "curves.can.snapshots": ["series", "Charge curves from CAN logs: days", max(1, days // 2), series - int(series * 0.93)],
        "series.teslamate.snapshots": ["series", "Time series from the TeslaMate import: days", max(1, imported // 1_500_000), int(imported * 0.9)],
        "series.teslamate-local.snapshots": ["series", "Time series from local TeslaMate: days", 17, 1_640_512],
        "rows.snapshots": ["rows", "Drives, charges, states", 12, rows + imported - int(imported * 0.9)],
        "kv.engine": ["kv", "The engine's records (decoded logs, costs, firmware)", 9, 184_320_000 + n_logs * 310],
        "kv.day-parts": ["kv", "Kept day parts of the long periods (can be worked out again)", 6, 41_200_000],
        "imports.uploads": ["imports", "Backups uploaded to import", 1, 312_475_648],
        "server.accounts": ["server", "Accounts, sessions and settings", 4, 48_211],
        "server.audit": ["server", "The audit log", 2, 2_184_300],
        "server.db": ["server", "The list of cars and places", 3, 18_000 + 120 * req.ds.total_cars()],
        "server.web": ["server", "The built-in interfaces", 412, 6_912_000],
        "server.tiles": ["server", "Offline map tiles", 1, req.store.storage["tiles_bytes"]],
    }
    return out


def _detail_categories(req, passes):
    """The categories after `passes` compaction passes asked for here (the first packs, later ones find
    nothing left)."""
    base = _detail_base(req)
    if passes > 0:
        moved = 0
        for key in ("logs.raw", "logs.sidecars"):
            c = base[key]
            m_files, m_bytes = int(c[2] * PACK_SHARE), int(c[3] * PACK_SHARE)
            c[2] -= m_files
            c[3] -= m_bytes
            moved += m_bytes
        base["logs.packs"][2] += max(1, base["series.can.snapshots"][2] // 2)
        base["logs.packs"][3] += int(moved * PACK_RATIO)
        for key, share in SHRINK.items():
            base[key][3] -= int(base[key][3] * share)
            base[key][2] = max(1, base[key][2] - int(base[key][2] * share))
    out = []
    for key, (group, label, files, b_) in base.items():
        # on disk a file takes whole blocks: half a block lost per file on average
        disk = -(-(b_ + files * (BLOCK // 2)) // BLOCK) * BLOCK
        out.append({"key": key, "group": group, "label": label, "files": files, "bytes": b_, "disk_bytes": disk})
    out.sort(key=lambda c: (-c["disk_bytes"], c["key"]))
    return out


def _disk_total(req, passes):
    return sum(c["disk_bytes"] for c in _detail_categories(req, passes))


def _compaction(req, advance=False):
    """The compaction's state (under the store's lock). A pass asked for here "runs" for a few reads of
    the detail: each read while it runs moves it one notch on, whatever the clock does (so it also ends
    with --freeze-clock)."""
    st = req.store
    c = st.compaction
    if c is None:
        # the last pass the server ran by itself: three hours before the start, 42 s long
        t0 = req.app.cfg.now_ms - 3 * HOUR - 17 * MIN
        c = st.compaction = {"status": "idle", "automatic": True, "started_ms": t0, "finished_ms": t0 + 42_000,
                             "step": None, "done": 0, "total": 0, **_IDLE_PROGRESS, "last_saved_bytes": 9_512_345,
                             "saved_bytes": int(system_storage(req)["kinds"][0]["bytes"] * 0.31 * (1 - PACK_RATIO)),
                             "error": None, "_passes": 0, "_tick": 0}
    if advance and c["status"] == "running":
        c["_tick"] += 1
        cats = {x["key"]: x for x in _detail_categories(req, c["_passes"])}
        # logs and series count days (a car's day of logs: its uploads of that day, about 1440 a day awake)
        totals = {"logs": max(1, cats["logs.raw"]["files"] // 1440), "series": cats["series.can.snapshots"]["files"],
                  "records": cats["kv.engine"]["files"] + cats["kv.day-parts"]["files"]}
        # the notches: logs counted and half done, logs done, series, records, finished; each notch is told as
        # 40 s of work (whatever the clock does), so rate and estimate read the same on every run
        plan = [("logs", 0.45), ("logs", 1.0), ("series", 0.6), ("records", 0.5)]
        if c["_tick"] <= len(plan):
            step, share = plan[c["_tick"] - 1]
            total = totals[step]
            done = int(total * share)
            before = _disk_total(req, c["_passes"])
            freed = before - _disk_total(req, c["_passes"] + 1)
            part = (c["_tick"] - 1 + share) / len(plan)
            rate = done / 40.0 if done else None
            c.update(step=step, total=total, done=done, phase="packing", step_number=COMPACT_STEPS.index(step) + 1,
                     unit=COMPACT_UNIT[step], pass_saved_bytes=int(freed * part) if c["_passes"] == 0 else 0,
                     rate_per_s=rate, eta_ms=int((total - done) / rate * 1000) if rate else None,
                     progress_ms=req.now - 5_000, stalled=False)
        else:
            before = _disk_total(req, c["_passes"])
            c["_passes"] += 1
            saved = before - _disk_total(req, c["_passes"])
            c.update(status="idle", step=None, done=0, total=0, finished_ms=req.now, last_saved_bytes=saved,
                     saved_bytes=c["saved_bytes"] + saved, **_IDLE_PROGRESS)
    if c["status"] == "running" and c.get("progress_ms") is not None:
        c["stalled"] = req.now - c["progress_ms"] >= STALL_MS
    return {k: v for k, v in c.items() if not k.startswith("_")}


def storage_detail(req):
    """GET /system/storage/detail (admin): the space on disk by category, largest first, and the compaction."""
    with req.store.lock:
        comp = _compaction(req, advance=True)
        cats = _detail_categories(req, req.store.compaction["_passes"])
    return {"categories": cats, "total_bytes": sum(c["bytes"] for c in cats),
            "total_disk_bytes": sum(c["disk_bytes"] for c in cats), "free_bytes": free_bytes(req.app, req.now),
            "compaction": comp}


def storage_compact(req):
    """POST /system/storage/compact (admin): starts a pass, 202 with its state; a running pass is not
    started twice."""
    with req.store.lock:
        _compaction(req)
        c = req.store.compaction
        started = c["status"] != "running"
        if started:
            c.update(status="running", started_ms=req.now, step="logs", done=0, total=0, error=None, _tick=0,
                     **{**_IDLE_PROGRESS, "phase": "counting", "step_number": 1, "unit": "days",
                        "pass_saved_bytes": 0, "progress_ms": req.now})
        comp = _compaction(req)
    if started:
        req.store.record(req.account, "storage-compacted", "server:storage", "Started packing the stored data (Compact now).")
    return Response(202, json_bytes({"compaction": comp}))


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
        "tesla": _tesla_health(req),
        "teslafi": _teslafi_health(req),
        "teslamate_local": _teslamate_health(req),
        "housekeeping_last_ms": req.now - 47 * MIN,
        "timings_ms": {"decode_per_log": 38.0, "rebuild_per_window": 410.0, "api_p50": 4.0, "api_p95": 31.0},
        "dev_server": {"dataset_build_ms": int(app.build_s * 1000), "now_ms": req.now, "clock_frozen": app.cfg.freeze_clock},
    }


def _tesla_health(req):
    from .tesla import health
    return health(req)


def _teslafi_health(req):
    from .teslafi import health
    return health(req)


def _teslamate_health(req):
    from .teslamate import health
    return health(req)


def register(router):
    router.add("/api/v1/settings", GET=get_settings, PATCH=patch_settings, admin=("PATCH",))
    router.add("/api/v1/maps/config", GET=maps_config)
    router.add("/api/v1/maps/tiles/{z}/{x}/{y}", GET=map_tile)
    router.add("/api/v1/geocode/status", GET=geocode_status)
    router.add("/api/v1/notices", GET=notices)
    router.add("/api/v1/about", GET=about)
    router.add("/api/v1/release", GET=release_state)
    router.add("/api/v1/release/check", POST=release_check_now, admin=("POST",))
    router.add("/api/v1/ui-packs", GET=ui_packs)
    router.add("/api/v1/ui-packs/{pack}/download", GET=ui_pack_download)
    router.add("/api/v1/cars/{car}/health", GET=car_health)
    router.add("/api/v1/cars/{car}/rebuild", POST=rebuild, admin=("POST",))
    router.add("/api/v1/cars/{car}/gps-layout", GET=get_gps_layout, PUT=put_gps_layout, admin=("PUT",))
    router.add("/api/v1/cars/{car}/gps-layout/fit", GET=get_gps_fit, POST=post_gps_fit, admin=("GET", "POST"))
    router.add("/api/v1/cars/{car}/gps-layout/fits", GET=get_gps_fits, admin=("GET",))
    router.add("/api/v1/cars/{car}/gps-layout/fits/{fit}", GET=get_gps_fit_one, DELETE=delete_gps_fit,
               admin=("GET", "DELETE"))
    router.add("/api/v1/cars/{car}/gps-layout/fits/{fit}/apply", POST=apply_gps_fit, admin=("POST",))
    router.add("/api/v1/cars/{car}/gps-layout/changes", GET=get_gps_changes, admin=("GET",))
    router.add("/api/v1/cars/{car}/overlaps", GET=get_overlaps, PATCH=patch_overlap, admin=("PATCH",))
    router.add("/api/v1/cars/{car}/storage", GET=car_storage)
    router.add("/api/v1/system/storage", GET=system_storage, admin=("GET",))
    router.add("/api/v1/system/storage/detail", GET=storage_detail, admin=("GET",))
    router.add("/api/v1/system/storage/compact", POST=storage_compact, admin=("POST",))
    router.add("/api/v1/system/health", GET=system_health, admin=("GET",))


__all__ = ["register", "HOUR", "r"]
