"""Import (docs/ui-api.md, "Import"): a resumable upload of a TeslaMate backup, a check of the whole
file, matching its cars to cars here, the import itself and its report.

The dev server keeps no bytes: each chunk is checked (offset, length, SHA-256) and counted. The
check and the import then run on the virtual clock (about 6 s and 12 s), so a page can poll a job
from "checking" through "ready" and "importing" to "done". A file named with "broken" fails its
check. Imported history is not added to the made-up dataset; the report says what would be.
"""

from __future__ import annotations

import base64
import binascii
import copy
import hashlib
import re

from ..conventions import page
from ..dataset import extras
from ..errors import ApiError, bad_input, conflict, invalid, not_found

CHECK_MS = 6_000
IMPORT_MS = 12_000
MAX_FILE = 64 * 1024 ** 3
COMPRESSIONS = ("plain", "gzip", "xz")

def _job(req):
    j = req.store.imports.get(req.params["import"])
    if j is None:
        raise not_found("no_such_import", "There is no such import.")
    _advance(req, j)
    return j


def _advance(req, j):
    """Move a job along the virtual clock."""
    with req.store.lock:
        now = req.now
        if j["status"] == "checking" and now >= j["_check_done_ms"]:
            if "broken" in (j.get("name") or ""):
                j["status"] = "failed"
                j["error"] = {"code": "not_a_backup",
                              "message": "This file is not a TeslaMate backup (no drives table)."}
                j["finished_ms"] = j["_check_done_ms"]
            else:
                j["status"] = "ready"
                j["pass"] = 1
                j["bytes_read"] = j["bytes_total"]
                j["checked_ms"] = j["_check_done_ms"]
                j["report"] = _report(req, j, imported=False)
        if j["status"] == "importing" and now >= j["_import_done_ms"]:
            j["status"] = "done"
            j["bytes_read"] = j["bytes_total"]
            j["finished_ms"] = j["_import_done_ms"]
            j["report"] = _report(req, j, imported=True)
        if j["status"] == "checking":
            f = (now - j["_check_start_ms"]) / CHECK_MS
            j["bytes_read"] = int(j["bytes_total"] * max(0.0, min(1.0, f)))
        if j["status"] == "importing":
            f = (now - j["_import_start_ms"]) / IMPORT_MS
            j["bytes_read"] = int(j["bytes_total"] * max(0.0, min(1.0, f)))


def _report(req, j, imported):
    return extras.import_report(j["cars"], imported)


def view(j):
    out = {k: copy.deepcopy(v) for k, v in j.items() if not k.startswith("_") and k != "cars"}
    out["cars_to_match"] = sum(1 for c in j["cars"] if c["match"] is None) if j["cars"] else None
    return out


RUNNING = ("uploading", "checking", "ready", "importing")


def running_import(req):
    """The import that is not finished yet (uploading, checked or importing), or None."""
    for j in req.store.imports.values():
        _advance(req, j)
        if j["status"] in RUNNING:
            return j
    return None


def list_imports(req):
    rows = []
    for j in req.store.imports.values():
        _advance(req, j)
        rows.append(view(j))
    items, nxt = page(req, rows, key=lambda x: (x.get("started_ms") or 0, x["id"]), reverse=True)
    return {"items": items, "next": nxt}


def create_import(req):
    st = req.store
    body = req.json(required=False)
    bad = {k: "Unknown field." for k in body if k not in ("name", "bytes_total", "compression")}
    length = req.headers.get("Upload-Length")
    total = body.get("bytes_total", int(length) if length and length.isdigit() else None)
    if total is None:
        bad["bytes_total"] = "Required: the size of the whole file in bytes (or the Upload-Length header)."
    elif not isinstance(total, int) or isinstance(total, bool):
        bad["bytes_total"] = "Must be a whole number."
    if "name" in body and not isinstance(body["name"], str):
        bad["name"] = "Must be text."
    if bad:
        raise bad_input(bad)
    rule = {}
    if not 1 <= total <= MAX_FILE:
        rule["bytes_total"] = "Between 1 byte and 64 GB."
    comp = body.get("compression")
    name = (body.get("name") or "teslamate-backup.sql").strip()[:200]
    if comp is None:
        comp = "xz" if name.endswith(".xz") else "gzip" if name.endswith(".gz") else "plain"
    if comp not in COMPRESSIONS:
        rule["compression"] = "Must be plain, gzip or xz."
    if rule:
        raise invalid(rule)
    if running_import(req):
        raise conflict("import_running", "Another import is running. Wait for it or discard it first.")
    jid = st.new_id("im")
    j = {"id": jid, "name": name, "compression": comp, "status": "uploading", "pass": 0, "bytes_read": 0,
         "bytes_total": total, "uploaded_bytes": 0, "started_ms": req.now, "finished_ms": None, "error": None,
         "cars": [], "report": None}
    with st.lock:
        st.imports[jid] = j
    st.record(req.account, "import.create", f"import:{jid}", f"Started uploading {name} ({total} bytes).")
    out = view(j)
    out["upload_url"] = f"/api/v1/imports/{jid}"
    out["chunk_max_bytes"] = 1024 * 1024 - 1024
    out["_created"] = True
    return out


def get_import(req):
    return view(_job(req))


def _sha_header(v):
    v = (v or "").strip()
    if re.fullmatch(r"[0-9a-fA-F]{64}", v):
        return bytes.fromhex(v)
    try:
        raw = base64.b64decode(v, validate=True)
        return raw if len(raw) == 32 else None
    except (binascii.Error, ValueError):
        return None


def patch_import(req):
    st = req.store
    j = _job(req)
    if j["status"] != "uploading":
        raise conflict("not_uploading", f"This import is {j['status']}; it takes no more chunks.")
    hd = req.headers
    off, ln = hd.get("Upload-Offset"), hd.get("Upload-Length")
    bad = {}
    if off is None or not off.isdigit():
        bad["Upload-Offset"] = "Required: where this chunk starts, in bytes."
    if ln is not None and not ln.isdigit():
        bad["Upload-Length"] = "The whole file size in bytes."
    want = _sha_header(hd.get("Upload-SHA256"))
    if want is None:
        bad["Upload-SHA256"] = "Required: the chunk's SHA-256 (hex or base64)."
    if not req.body:
        bad["body"] = "Send the chunk's bytes."
    if bad:
        raise bad_input(bad)
    if ln is not None and int(ln) != j["bytes_total"]:
        raise invalid({"Upload-Length": f"This upload is {j['bytes_total']} bytes."})
    if int(off) != j["uploaded_bytes"]:
        raise ApiError(409, "wrong_offset", f"The upload is at byte {j['uploaded_bytes']}; send from there.",
                       headers={"Upload-Offset": str(j["uploaded_bytes"])}, extra={"offset": j["uploaded_bytes"]})
    if hashlib.sha256(req.body).digest() != want:
        raise ApiError(422, "checksum_mismatch", "The chunk does not match its SHA-256. Send it again.",
                       fields={"Upload-SHA256": "Does not match the bytes sent."})
    if j["uploaded_bytes"] + len(req.body) > j["bytes_total"]:
        raise invalid({"body": "More bytes than the upload's length."})
    with st.lock:
        j["uploaded_bytes"] += len(req.body)
        j["bytes_read"] = j["uploaded_bytes"]
        if j["uploaded_bytes"] == j["bytes_total"]:
            j["status"] = "checking"
            j["pass"] = 1
            j["bytes_read"] = 0
            j["_check_start_ms"] = req.now
            j["_check_done_ms"] = req.now + CHECK_MS
            j["cars"] = [dict(c, match=_suggest(req, c))
                         for c in extras.backup_cars(req.app.cfg.now_ms, req.ds.history("c3"))]
            st.rev += 1
    if j["status"] == "checking":
        st.record(req.account, "import.uploaded", f"import:{j['id']}", "Uploaded the whole file; checking it.")
    out = view(j)
    out["offset"] = j["uploaded_bytes"]
    return out


def _suggest(req, c):
    for cid in req.ds.car_ids_featured():
        if req.store.car_meta(cid)["vin_prefix"] == c["vin_prefix"]:
            return cid
    return None


def delete_import(req):
    st = req.store
    j = _job(req)
    with st.lock:
        st.imports.pop(j["id"], None)
    st.record(req.account, "import.discard", f"import:{j['id']}",
              f"Discarded the import {j['name']} ({j['status']}). History already imported stays.")
    return {"deleted": True, "id": j["id"], "status": j["status"]}


def _car_row(req, j, c):
    return {"tm_car": c["tm_car"], "name": c["name"], "vin_prefix": c["vin_prefix"], "drives": c["drives"],
            "charges": c["charges"], "first_ms": c["first_ms"], "last_ms": c["last_ms"], "match": c["match"],
            "suggested": _suggest(req, c)}


def get_cars(req):
    j = _job(req)
    if not j["cars"]:
        raise conflict("not_checked", "The cars are known once the whole file is uploaded.")
    return {"import": j["id"], "items": [_car_row(req, j, c) for c in j["cars"]], "next": None}


def put_cars(req):
    st = req.store
    j = _job(req)
    if j["status"] not in ("checking", "ready"):
        raise conflict("not_matchable", f"This import is {j['status']}; cars can be matched while it is checked "
                                        "or ready.")
    body = req.json()
    rows = body.get("items")
    if not isinstance(rows, list) or not all(isinstance(x, dict) for x in rows) or set(body) - {"items"}:
        raise bad_input({"items": "Send { items: [{ tm_car, match }] }."})
    known = {c["tm_car"]: c for c in j["cars"]}
    bad, rule = {}, {}
    named = {x.get("tm_car") for x in rows}
    # one car here takes at most one imported car (the matches this body leaves alone count too)
    seen = {c["match"]: c["tm_car"] for c in j["cars"]
            if c["tm_car"] not in named and c["match"] not in (None, "new", "leave_out")}
    for i, x in enumerate(rows):
        if set(x) - {"tm_car", "match"}:
            bad[f"items.{i}"] = "Only tm_car and match."
            continue
        if x.get("tm_car") not in known:
            rule[f"items.{i}.tm_car"] = "Not a car in this file."
            continue
        m = x.get("match")
        if m not in ("new", "leave_out") and not (isinstance(m, str) and req.ds.exists(m)
                                                  and st.sees(req.account, m)):
            rule[f"items.{i}.match"] = 'A car id, "new" or "leave_out".'
        elif m not in ("new", "leave_out"):
            if m in seen:
                rule[f"items.{i}.match"] = f"Car {m} is already matched to imported car {seen[m]}."
            seen[m] = x["tm_car"]
    if bad:
        raise bad_input(bad)
    if rule:
        raise invalid(rule)
    with st.lock:
        for x in rows:
            known[x["tm_car"]]["match"] = x["match"]
    st.record(req.account, "import.cars", f"import:{j['id']}",
              "Matched imported cars: " + ", ".join(f"{x['tm_car']} -> {x['match']}" for x in rows) + ".")
    return get_cars(req)


def start(req):
    st = req.store
    j = _job(req)
    if j["status"] != "ready":
        raise conflict("not_ready", f"This import is {j['status']}; it can start once the whole file is checked.")
    if any(c["match"] is None for c in j["cars"]):
        raise conflict("cars_unmatched", "Match every imported car first (a car, new, or leave out).")
    with st.lock:
        j["status"] = "importing"
        j["pass"] = 2
        j["bytes_read"] = 0
        j["_import_start_ms"] = req.now
        j["_import_done_ms"] = req.now + IMPORT_MS
    st.record(req.account, "import.start", f"import:{j['id']}", f"Started importing {j['name']}.")
    return view(j)


def register(router):
    router.add("/api/v1/imports", GET=list_imports, POST=create_import, admin=("GET", "POST"))
    router.add("/api/v1/imports/{import}", GET=get_import, PATCH=patch_import, DELETE=delete_import,
               admin=("GET", "PATCH", "DELETE"))
    router.add("/api/v1/imports/{import}/cars", GET=get_cars, PUT=put_cars, admin=("GET", "PUT"))
    router.add("/api/v1/imports/{import}/start", POST=start, admin=("POST",))
