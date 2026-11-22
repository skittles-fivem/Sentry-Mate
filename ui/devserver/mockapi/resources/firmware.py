"""Car firmware (docs/ui-api.md, "Reports"): the modules' firmware fingerprints, made up.

The real server decodes them from the info frames the front body controller (0x301), the battery
(0x300) and the drive units (0x335 rear, 0x656 front) send on the vehicle bus, and finds a
"Tesla update installed" where they changed between two drives. Here every car's CAN updates are
such fingerprint updates: each changes the app CRC of the modules it lists (and sometimes the
headlights' or the oil pump's CRC), and the values are made up from the car, the field and how
many updates changed it. The app git hashes read zero on these cars, so they are not listed.
"""
from __future__ import annotations

from ..util import h01

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


def register(router):
    router.add("/api/v1/cars/{car}/firmware", GET=get_firmware)
