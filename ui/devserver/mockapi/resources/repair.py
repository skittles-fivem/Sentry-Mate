"""A car's drives table repair (docs/ui-api.md, "Data health"; crates/web/src/resources/system.rs).

POST /cars/{car}/repair {table: "drives"} starts a made-up repair that runs on the virtual clock (about 6 s) and
answers 202; GET gives the report of the last one, or 404 no_repair. The seeded car c1 already has one finished
report with a made-up loss, so the admins see the notice `data_repaired`. The audit log gets the admin's ask when
it is made and, in the server's name, the end of the repair. The dataset's rows don't change.
"""

from __future__ import annotations

from ..conventions import Response, json_bytes
from ..errors import conflict, invalid, not_found
from ..util import DAY

REPAIR_MS = 6_000
LOST = {"what": "imported drives", "source": "TeslaFi", "from_ms": None, "to_ms": None, "expected": 1,
        "found": 0, "why": "the import's upload was discarded"}


def _said(l):
    n = f"{l['expected'] - l['found']} of {l['expected']}" if l["expected"] is not None and l["found"] is not None \
        else "some"
    span = "a time that is not known" if l["from_ms"] is None else "the span it covered"
    return f"{n} {l['what']} ({l['source']}, {span}): {l['why']}"


def _report(car, started, finished, lost):
    done = finished is not None
    return {"car": car, "table": "drives", "state": "done" if done else "running", "started_ms": started,
            "finished_ms": finished, "aside": f"db/cars/{car}/aside/drives-{started}" if done else None,
            "damage": ["drives.snap: a chunk does not read"] if done else [],
            "salvaged": 212 if done else 0, "reimported": 3 if done else 0, "rebuilt": 41 if done else None,
            "relinked": 17 if done else 0, "unmatched": 0, "rebuild_from_ms": started - 90 * DAY if done else None,
            "lost": [dict(l, said=_said(l)) for l in lost] if done else []}


def _summary(name, r):
    n = len(r["lost"])
    lost = "nothing was lost" if n == 0 else ("1 thing could not be recovered" if n == 1
                                              else f"{n} things could not be recovered")
    return (f"Repaired {name}'s drives table: {r['salvaged']} rows kept, {r['reimported']} imported rows put back "
            f"from the kept uploads, {r['rebuilt'] or 0} made again from the logs, {r['relinked']} purposes put "
            f"back on their drives; {lost}.")


def seed(st, now):
    """c1 had its drives table repaired a few days ago."""
    st.repairs = {"c1": _report("c1", now - 5 * DAY, now - 5 * DAY + 40_000, [LOST])}


def status(req, car):
    """The car's last repair report (a running one that is due is finished and audited now), or None."""
    st = req.store
    with st.lock:
        r = st.repairs.get(car)
        if r and r["state"] == "running" and req.now - r["started_ms"] >= REPAIR_MS:
            r = st.repairs[car] = _report(car, r["started_ms"], r["started_ms"] + REPAIR_MS, [])
            st.record(None, "table-repaired", f"car:{car}", _summary(st.car_meta(car)["name"], r), car=car)
    return r


def get_repair(req):
    r = status(req, req.car)
    if not r:
        raise not_found("no_repair", "This car's drives table has not been repaired.")
    return r


def post_repair(req):
    body = req.json()
    bad = {k: "Unknown field." for k in body if k != "table"}
    if bad:
        raise invalid(bad)
    if body.get("table") != "drives":
        raise invalid({"table": "Only the drives table can be repaired."})
    st = req.store
    from .fullrebuild import status as full_status
    from .imports import running_import
    r = status(req, req.car)
    if r and r["state"] == "running":
        raise conflict("repair_running", "A repair of this car's drives table is under way.")
    if running_import(req):
        raise conflict("import_running", "An import is running. Wait for it or discard it first.")
    if full_status(req)["state"] == "running":
        raise conflict("full_rebuild_running", "A full rebuild is under way: it makes the whole history again.")
    with st.lock:
        st.repairs[req.car] = _report(req.car, req.now, None, [])
    st.record(req.account, "table-repaired", f"car:{req.car}",
              f"Asked for a repair of {st.car_meta(req.car)['name']}'s drives table.", car=req.car)
    return Response(202, json_bytes({"car": req.car, "table": "drives", "state": "running"}))


def notices(req):
    """`data_repaired` for each car whose last repair is done and lost something (the admins')."""
    out = []
    for car in sorted(req.store.repairs):
        r = status(req, car)
        if r["state"] != "done" or not r["lost"]:
            continue
        said = "; ".join(l["said"] for l in r["lost"])
        out.append({"code": "data_repaired", "level": "warning", "since_ms": r["finished_ms"],
                    "fix_link": "/settings/server", "detail": r,
                    "message": f"The drives table of {req.store.car_meta(car)['name']} was damaged and has been "
                               f"repaired, but some of it could not be recovered: {said}. The damaged file was kept "
                               f"aside. Restore from a backup only for what is listed here."})
    return out


def register(router):
    router.add("/api/v1/cars/{car}/repair", GET=get_repair, POST=post_repair, admin=("GET", "POST"))
