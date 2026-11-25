"""A car's CAN logs with their check results, and the owner's word on unsigned ones (crates/web/src/resources/logs.rs,
docs/ui-api.md "Server, settings and health", docs/security.md "Pairing and the owner's word").

The dev data keeps no logs of its own, so the logs here are made up, for one car only (PAIRED_CAR, the others list
none): its CAN Bridge key was paired by its first signed log, then sent older logs without a signature (an app from
before it signed logs) and a few after; one has a bad signature and one is signed by another key: those two are
never used. Times are whole days back from today (UTC), so the names stay the same all day.
"""

from __future__ import annotations

from ..conventions import page
from ..errors import ApiError, bad_input, invalid
from ..util import DAY, MIN

PAIRED_CAR = "c1"
NAMES_MAX = 100_000
REFUSED_SHOWN = 10
NOT_SIGNED_BUT_PAIRED = "not signed, but this car is paired"
KIND_WORDS = {"no_sidecar": "with no sidecar", "no_integrity": "from before the app signed logs",
              "signed_false": "the app said were not signed", "could_not_sign": "the app could not sign",
              "unknown": "of a kind not known"}


def _stamp(ms):
    import datetime
    return datetime.datetime.fromtimestamp(ms / 1000, datetime.timezone.utc).strftime("%Y%m%dT%H%M%SZ")


def _key(req, cid):
    keys = [k for k in req.store.keys if k["car"] == cid]
    keys.sort(key=lambda k: (not k.get("preferred"), k["key"]))
    return keys[0]["key"] if keys else "cbk_00000000"


def made_up(req, cid):
    """The car's made-up logs, oldest first, as rows of `/cars/{car}/logs` (with the owner's word as it is now)."""
    if cid != PAIRED_CAR:
        return []
    key = _key(req, cid)
    day0 = req.now - req.now % DAY
    first_signed = day0 - 20 * DAY + 9 * 60 * MIN
    plan = []
    # 21 days back: logs from before the app signed them, sent after the first signed one
    t = day0 - 21 * DAY + 8 * 60 * MIN
    kinds = ["no_integrity"] * 6 + ["no_sidecar", "signed_false", "could_not_sign"]
    for i, k in enumerate(kinds):
        plan.append((t + i * 10 * MIN, "unsigned", k))
    plan.append((t + len(kinds) * 10 * MIN, "bad_signature", None))
    for i in range(6):
        plan.append((first_signed + i * 10 * MIN, "verified", None))
    # 5 days back: two more without a signature, and one signed by another key
    t5 = day0 - 5 * DAY + 17 * 60 * MIN
    plan.append((t5, "unsigned", "no_integrity"))
    plan.append((t5 + 10 * MIN, "unsigned", "signed_false"))
    plan.append((t5 + 20 * MIN, "other_key", None))
    for i in range(3):
        plan.append((t5 + (3 + i) * 10 * MIN, "verified", None))
    used = req.store.__dict__.setdefault("unsigned_used", {}).setdefault(cid, set())
    rows = []
    for seq, (start, what, kind) in enumerate(plan, start=1):
        name = f"{key}_{_stamp(start)}_{seq}.log.xz"
        if what == "verified":
            result, reason = "verified", None
        elif what == "bad_signature":
            result, reason = "failed", "the signature does not match"
        elif what == "other_key":
            result, reason = "failed", "signed by another key"
        elif kind == "could_not_sign":
            result, reason = "could_not_sign", "the CAN Bridge could not sign it (keystore was busy)"
        else:
            result, reason = "failed", NOT_SIGNED_BUT_PAIRED
        override = name in used and what == "unsigned"
        ok = result == "verified" or override
        rows.append({
            "name": name, "key": key, "start_ms": start, "result": result, "reason": reason, "used": ok,
            "override_use": override, "unsigned_kind": kind if what == "unsigned" else None,
            "before_first_signed": start < first_signed, "arrived_ms": first_signed + 2 * MIN + seq * 1000,
            "decoded": "decoded" if ok else "left_out", "samples": 600 if ok else None, "dropped": 0 if ok else None,
            "buses": ["CH", "VEH"] if ok else [], "data_from_ms": start, "data_to_ms": start + 10 * MIN,
            "decoded_ms": first_signed + 3 * MIN + seq * 1000,
        })
    return rows


def can_use(r):
    return r["unsigned_kind"] is not None and r["result"] in ("failed", "could_not_sign") and not r["override_use"]


def car_logs(req):
    a, b = req.int_param("from_ms", None, 0), req.int_param("to_ms", None, 0)
    if a is not None and b is not None and a >= b:
        raise bad_input({"to_ms": "Must be after from_ms."})
    rows = [r for r in made_up(req, req.car)
            if (a is None or r["start_ms"] >= a) and (b is None or r["start_ms"] < b)]
    items, nxt = page(req, rows, lambda r: (-r["start_ms"], r["name"]))
    return {"items": items, "next": nxt}


def unsigned(req):
    rows = made_up(req, req.car)
    kinds = {}
    for r in rows:
        if can_use(r):
            kinds[r["unsigned_kind"]] = kinds.get(r["unsigned_kind"], 0) + 1
    shown = [r for r in rows if can_use(r) or r["override_use"]]
    shown.sort(key=lambda r: (-r["start_ms"], r["name"]))
    firsts = {}
    for r in rows:
        if r["result"] == "verified":
            firsts.setdefault(r["key"], r["start_ms"])
    keys = sorted({r["key"] for r in rows})
    return {
        "can_use": sum(1 for r in rows if can_use(r)),
        "can_use_before_first_signed": sum(1 for r in rows if can_use(r) and r["before_first_signed"]),
        "kinds": kinds,
        "in_use": sum(1 for r in rows if r["override_use"]),
        "never": sum(1 for r in rows if r["result"] == "failed" and r["unsigned_kind"] is None),
        "keys": [{"key": k, "first_signed_ms": firsts.get(k)} for k in keys],
        "items": shown[:req.app.cfg.health_list_max], "items_total": len(shown),
    }


def _pick(body):
    for k in body:
        if k not in ("names", "all", "before_first_signed", "confirm_password", "code"):
            raise bad_input({k: "Unknown field."})
    names, every, before = body.get("names"), body.get("all", False), body.get("before_first_signed")
    if not isinstance(every, bool):
        raise bad_input({"all": "true or false."})
    if before is not None and not isinstance(before, bool):
        raise bad_input({"before_first_signed": "true or false."})
    if names is not None:
        if not isinstance(names, list) or not all(isinstance(n, str) for n in names):
            raise bad_input({"names": "A list of log names."})
        if len(names) > NAMES_MAX:
            raise bad_input({"names": f"At most {NAMES_MAX} names at a time."})
    if names is not None and every:
        raise bad_input({"names": "Either names or all, not both."})
    if names is None and not every:
        raise bad_input({"names": "Name the logs, or say all: true."})
    if names is not None and not names:
        raise bad_input({"names": "Name at least one log."})
    if names is not None and before is not None:
        raise bad_input({"before_first_signed": "Only with all: true."})
    return names, bool(before)


def _why_not(r):
    if r is None:
        return "not a log of this car"
    if r["result"] == "verified":
        return "verified: its data is used already"
    return r["reason"] or "failed its check"


def _change(req, on):
    from .tesla import reconfirm
    st, cid = req.store, req.car
    body = req.json(required=False)
    names, before = _pick(body)
    reconfirm(req, body)
    rows = {r["name"]: r for r in made_up(req, cid)}
    refused, changed = [], []
    if names is not None:
        for n in sorted(set(names)):
            r = rows.get(n)
            if r is None or (on and r["unsigned_kind"] is None) or (on and r["result"] == "verified"):
                refused.append((n, _why_not(r)))
            elif r["override_use"] != on:
                changed.append(r)
    else:
        for r in rows.values():
            if on and can_use(r) and (not before or r["before_first_signed"]):
                changed.append(r)
            elif not on and r["override_use"]:
                changed.append(r)
    if refused:
        n = len(refused)
        listed = ", ".join(f"{a} ({b})" for a, b in refused[:REFUSED_SHOWN])
        more = f" and {n - REFUSED_SHOWN} more" if n > REFUSED_SHOWN else ""
        what = "only a log that failed its check for being unsigned can be used" if on else "not a log of this car"
        logs = "1 of these logs" if n == 1 else f"{n} of these logs"
        raise invalid({"names": f"{logs}: {what}, and nothing was changed: {listed}{more}."})
    used = st.__dict__.setdefault("unsigned_used", {}).setdefault(cid, set())
    kinds = {}
    for r in changed:
        (used.add if on else used.discard)(r["name"])
        kinds[r["unsigned_kind"]] = kinds.get(r["unsigned_kind"], 0) + 1
    if changed:
        n = len(changed)
        what = "1 unsigned CAN log" if n == 1 else f"{n} unsigned CAN logs"
        which = ", ".join(f"{v} {KIND_WORDS.get(k, k)}" for k, v in sorted(kinds.items()))
        car = st.car_meta(cid)["name"]
        if on:
            st.record(req.account, "can-logs-unsigned-used", f"car:{cid}",
                      f"Used {what} of {car} that had failed their check only for being unsigned ({which}); "
                      "bad and other-key logs are never used")
        else:
            st.record(req.account, "can-logs-unsigned-stopped", f"car:{cid}", f"Stopped using {what} of {car} ({which})")
        st.rev += 1
    return {"changed": len(changed), "kinds": kinds, "names": sorted(r["name"] for r in changed)}


def use_unsigned(req):
    return _change(req, True)


def stop_using(req):
    return _change(req, False)


def register(router):
    router.add("/api/v1/cars/{car}/logs", GET=car_logs, admin=("GET",))
    router.add("/api/v1/cars/{car}/logs/unsigned", GET=unsigned, admin=("GET",))
    router.add("/api/v1/cars/{car}/logs/use-unsigned", POST=use_unsigned, DELETE=stop_using,
               admin=("POST", "DELETE"))
