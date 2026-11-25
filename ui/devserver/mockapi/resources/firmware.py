"""Car firmware (docs/ui-api.md, "Reports"): the modules' firmware fingerprints, made up.

The real server decodes them from the info frames the front body controller (0x301), the battery
(0x300) and the drive units (0x335 rear, 0x656 front) send on the vehicle bus, and finds a
"Tesla update installed" where they changed between two drives. Here every car's CAN updates are
such fingerprint updates: each changes the app CRC of the modules it lists (and sometimes the
headlights' or the oil pump's CRC), and the values are made up from the car, the field and how
many updates changed it. The app git hashes read zero on these cars, so they are not listed.
"""
from __future__ import annotations

from .. import sources as S
from ..conventions import page
from ..errors import bad_input
from ..live import update_version
from ..util import h01, iso_utc

# (id, name, can_id, [(field, bits)]): the fields the made-up cars send, in the DBC's order
MODULES = [
    ("vcfront", "Front body controller", 0x301,
     [("VCFRONT_infoAppCrc", 32), ("VCFRONT_infoBootCrc", 32), ("VCFRONT_infoBootGitHash", 56),
      ("VCFRONT_infoBuildType", 3), ("VCFRONT_infoBuildConfigId", 16), ("VCFRONT_hcmlAppCRC", 32),
      ("VCFRONT_hcmrAppCRC", 32)]),
    ("bms", "Battery management", 0x300,
     [("BMS_appCrc", 32), ("BMS_bootCrc", 32), ("BMS_buildType", 3), ("BMS_buildConfigId", 16)]),
    ("dir", "Rear drive unit", 0x335,
     [("DIR_applicationCrc", 32), ("DIR_bootloaderCrc", 32), ("DIR_subcomponentGitHash", 56),
      ("DIR_buildType", 3), ("DIR_buildConfigurationId", 16), ("DIR_oilPumpAppCrc", 32),
      ("DIR_oilPumpBuildType", 3)]),
    ("dif", "Front drive unit", 0x656,
     [("DIF_applicationCrc", 32), ("DIF_bootloaderCrc", 32), ("DIF_subcomponentGitHash", 56),
      ("DIF_buildType", 3), ("DIF_buildConfigurationId", 16), ("DIF_oilPumpAppCrc", 32),
      ("DIF_oilPumpBuildType", 3)]),
]
BITS = {f: b for _, _, _, fs in MODULES for f, b in fs}
MODULE_OF = {f: (mid, name) for mid, name, _, fs in MODULES for f, _ in fs}
MAIN = {"vcfront": "VCFRONT_infoAppCrc", "bms": "BMS_appCrc", "dir": "DIR_applicationCrc",
        "dif": "DIF_applicationCrc"}
EXTRA = {"vcfront": "VCFRONT_hcmlAppCRC", "dir": "DIR_oilPumpAppCrc", "dif": "DIF_oilPumpAppCrc"}


def hexof(field, v):
    return format(v, f"0{(BITS[field] + 3) // 4}x")


def has_front(car):
    return car.config.get("drivetrain", ("AWD",))[0] != "RWD"


def value(car, field, epoch):
    """A made-up value, never zero or SNA (every bit set)."""
    bits = BITS[field]
    if bits <= 3:
        return 1 + int(h01("fw", car.id, field, epoch) * 5)
    top = (1 << bits) - 2
    return 1 + int(h01("fw", car.id, field, epoch) * top)


def fingerprint_updates(h):
    return [u for u in h.updates if u.source == "can" and u.detected_by == "fingerprint"]


def changed_fields(h, u):
    """The fields an update changed, in the pages' order."""
    out = []
    for m in u.changed_modules:
        out.append(MAIN[m])
        if m in EXTRA and h01("fw-extra", h.car.id, u.seq, m) < 0.5:
            out.append(EXTRA[m])
    order = [f for _, _, _, fs in MODULES for f, _ in fs]
    return sorted(out, key=order.index)


def _fields(car):
    return [(mid, f) for mid, _, _, fs in MODULES for f, _ in fs if mid != "dif" or has_front(car)]


def _span(h, now):
    can = [d for d in h.drives if d.source == "can" and d.start <= now]
    first = min((d.start for d in can), default=h.first_ms)
    last = max((min(d.end, now) for d in can), default=now)
    return first, last


def runs(h, now):
    """field -> [(value, first_seen_ms, last_seen_ms)], oldest first."""
    car = h.car
    first, last = _span(h, now)
    ups = [u for u in fingerprint_updates(h) if u.end <= now and u.start >= first]
    out = {}
    for _, f in _fields(car):
        rs, epoch, start = [], 0, first
        for u in ups:
            if f in changed_fields(h, u):
                rs.append((value(car, f, epoch), start, u.start))
                epoch += 1
                start = u.end
        rs.append((value(car, f, epoch), start, last))
        out[f] = rs
    return out


def update_changes(h, u, now):
    """[{module, module_name, field, old, new}] of a fingerprint update."""
    rs = runs(h, now)
    out = []
    for f in changed_fields(h, u):
        seq = rs.get(f, [])
        for a, b in zip(seq, seq[1:]):
            if a[2] == u.start and b[1] == u.end:
                mid, name = MODULE_OF[f]
                out.append({"module": mid, "module_name": name, "field": f, "old": hexof(f, a[0]),
                            "new": hexof(f, b[0])})
    return out


def get_firmware(req):
    h = req.history()
    mods, updated, count = [], None, 0
    rs = runs(h, req.now) if h is not None else {}
    if h is not None:
        count = len([u for u in fingerprint_updates(h) if u.end <= req.now])
    for mid, name, can_id, fs in MODULES:
        fields = []
        for f, bits in fs:
            seq = rs.get(f)
            if not seq:
                continue
            v, a, b = seq[-1]
            updated = b if updated is None else max(updated, b)
            fields.append({"field": f, "hex": hexof(f, v), "first_seen_ms": a, "last_seen_ms": b, "bits": bits,
                           "history": [{"hex": hexof(f, x), "first_seen_ms": p, "last_seen_ms": q}
                                       for x, p, q in seq]})
        mods.append({"id": mid, "name": name, "can_id": can_id, "fields": fields})
    return {"car": req.car, "time_zone": req.tz(), "modules": mods, "updated_ms": updated, "updates": count,
            "bridge_hints": {"count": 0, "last_ms": None}}


# ------------------------------------------------------------------ history

def _module_rows(req, h, rs):
    """One row per fingerprint each module had (its main field's runs), oldest first per module."""
    car = h.car
    ups = {u.end: u for u in fingerprint_updates(h)}
    drives = sorted((d for d in h.drives if d.source == "can" and d.start <= req.now), key=lambda d: d.start)
    out = []
    for mid, name, _, fs in MODULES:
        seq = rs.get(MAIN[mid])
        if not seq:
            continue
        for i, (v, a, b) in enumerate(seq):
            fields = []
            for f, _ in fs:
                run = next((x for x in rs.get(f, []) if x[1] <= a <= x[2]), None)
                if run is not None:
                    fields.append({"field": f, "hex": hexof(f, run[0])})
            u = ups.get(a) if i else None
            upd = None
            changed = None
            if u is not None:
                ver, vs = update_version(req.app, u)
                upd = {"id": u.id, "version": ver, "version_source": vs if ver else None}
                d = next((d for d in drives if d.start >= a), None)
                changed = {"drive_id": d.id if d else None,
                           "log": f"{iso_utc(a - a % 60_000).replace(':', '-')}_{car.id}.log"}
            nxt = seq[i + 1][1] if i + 1 < len(seq) else None
            out.append({
                "id": f"{mid}:{a}", "kind": "module", "module": mid, "module_name": name,
                "fingerprint": hexof(MAIN[mid], v), "previous": hexof(MAIN[mid], seq[i - 1][0]) if i else None,
                "fields": fields, "first_seen_ms": a, "last_seen_ms": b, "until_ms": nxt, "duration_ms": b - a,
                "current": nxt is None, "update": upd, "version": upd["version"] if upd else None,
                "version_source": upd["version_source"] if upd else None, "name_inferred": None,
                "changed_in": changed, "source": "can", "source_label": S.LABEL["smo"],
            })
            if i == 0:
                _name_first(req, h, out[-1])
    return out


def _name_first(req, h, row):
    """A module's first segment (no update came with it): the name an owner typed; else the import's version when
    the import still covers it; else, as an inference, the last version known before it with no update between."""
    typed = req.store.first_versions.get((req.car, row["module"]))
    if typed:
        row["version"], row["version_source"] = typed, "owner"
        return
    a = row["first_seen_ms"]
    imp = sorted((u for u in h.updates if u.source == "teslamate" and u.start <= req.now), key=lambda u: u.start)
    known = [(u, update_version(req.app, u)) for u in imp]
    known = [(u, v) for u, v in known if v[0]]
    if not known:
        return
    u, (ver, _) = known[-1]
    # an update (of any kind) after it and up to the segment's start: the version may have changed
    if any(x.start > u.start and x.start <= a for x in h.updates if x.id != u.id):
        return
    if min(u.end, req.now) >= a:
        row["version"], row["version_source"] = ver, S.site_of(u) or "teslamate"
    else:
        row["name_inferred"] = {"version": ver, "from": "last known before"}


def _version_rows(req, h):
    """One row per imported (TeslaMate, TeslaFi) update: they carry a version, no fingerprints."""
    ups = sorted((u for u in h.updates if u.source == "teslamate" and u.start <= req.now), key=lambda u: u.start)
    out = []
    for i, u in enumerate(ups):
        ver, vs = update_version(req.app, u)
        nxt = ups[i + 1].start if i + 1 < len(ups) else None
        src = S.of_seg(u)
        site = S.site_of(u) or "teslamate"
        end = min(u.end, req.now)
        out.append({
            "id": f"version:{u.id}", "kind": "version", "module": None, "module_name": None, "fingerprint": None,
            "previous": None, "fields": [], "first_seen_ms": u.start, "last_seen_ms": end, "until_ms": nxt,
            "duration_ms": (nxt if nxt is not None else req.now) - u.start, "current": nxt is None,
            "update": {"id": u.id, "version": ver, "version_source": vs if ver else None}, "version": ver,
            "version_source": vs if ver else None, "name_inferred": None, "changed_in": None, "source": site, "source_label": src["source_label"],
        })
    return out


def get_firmware_history(req):
    mod = req.q("module")
    if mod is not None and mod not in MAIN:
        raise bad_input({"module": "One of vcfront, bms, dir, dif."})
    req.limit()
    h = req.history()
    rows = []
    if h is not None:
        rows = _module_rows(req, h, runs(h, req.now)) + _version_rows(req, h)
    mrows = [x for x in rows if x["kind"] == "module"]
    modules = []
    for mid, name, can_id, _ in MODULES:
        mine = [x for x in mrows if x["module"] == mid]
        modules.append({"id": mid, "name": name, "can_id": can_id, "fingerprints": len(mine),
                        "current": mine[-1]["fingerprint"] if mine else None})
    spans = sorted(({"module": x["module"], "fingerprint": x["fingerprint"], "from_ms": x["first_seen_ms"],
                     "to_ms": x["last_seen_ms"], "version": x["version"], "name_inferred": x["name_inferred"],
                     "current": x["current"]} for x in mrows),
                   key=lambda s: (s["from_ms"], [m[0] for m in MODULES].index(s["module"])))
    sel = [x for x in rows if x["module"] == mod] if mod else rows
    items, nxt = page(req, sel, key=lambda x: (x["first_seen_ms"], x["id"]), reverse=True)
    return {"car": req.car, "time_zone": req.tz(), "items": items, "next": nxt, "modules": modules,
            "spans": spans,
            "range": {"from_ms": min((x["first_seen_ms"] for x in mrows), default=None),
                      "to_ms": max((x["last_seen_ms"] for x in mrows), default=None)}}


def _history_rows(req):
    h = req.history()
    return h, ([] if h is None else _module_rows(req, h, runs(h, req.now)))


def patch_segment(req):
    """PATCH /cars/{car}/firmware/history/{row} { version }: an admin names a module's fingerprint segment: the
    update it came with (as typing on that update does), or, for a module's first one, the segment itself."""
    from .software import VERSION_RX, patch_update
    from ..errors import invalid, not_found
    h, rows = _history_rows(req)
    row = next((x for x in rows if x["id"] == req.params["row"]), None)
    if row is None or (row["update"] is None and row["previous"] is not None):
        raise not_found("no_such_update", "There is no such software update for this car.")
    if row["update"] is not None:
        req.params = {**req.params, "update": row["update"]["id"]}
        patch_update(req)
    else:
        body = req.json()
        bad = {k: "Unknown field." for k in body if k != "version"}
        if "version" not in body:
            bad["version"] = "Required (a version such as 2026.38.4, or null to forget it)."
        elif body["version"] is not None and not isinstance(body["version"], str):
            bad["version"] = "Must be text or null."
        if bad:
            raise bad_input(bad)
        v = body["version"].strip() if isinstance(body["version"], str) else None
        if v is not None and not VERSION_RX.fullmatch(v):
            raise invalid({"version": "A version such as 2026.38.4."})
        st = req.store
        with st.lock:
            if v is None:
                st.first_versions.pop((req.car, row["module"]), None)
            else:
                st.first_versions[(req.car, row["module"])] = v
        st.record(req.account, "update-version", f"update:{req.car}:{row['id']}",
                  f"Named a firmware version: {v}." if v else "Forgot the typed firmware version.", car=req.car)
        req.app.events.publish("rows", {"car": req.car, "kinds": ["updates"], "from_ms": row["first_seen_ms"],
                                        "to_ms": row["until_ms"] or row["first_seen_ms"]}, car=req.car)
    _, rows = _history_rows(req)
    return next(x for x in rows if x["id"] == row["id"]) | {"time_zone": req.tz()}


def register(router):
    router.add("/api/v1/cars/{car}/firmware", GET=get_firmware)
    router.add("/api/v1/cars/{car}/firmware/history", GET=get_firmware_history)
    router.add("/api/v1/cars/{car}/firmware/history/{row}", PATCH=patch_segment, admin=("PATCH",))
