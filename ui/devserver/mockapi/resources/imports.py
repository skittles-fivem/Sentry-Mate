"""Import (docs/ui-api.md, "Import"): a resumable upload of a TeslaMate backup, of TeslaFi's data
download (its monthly CSVs) or of Sentry Six Manager's CAN export (.zip), a check of the whole file,
matching a file's cars to cars here, the import itself and its report.

The kind comes from the file's first bytes, as on the server: a CSV whose header is TeslaFi's raw
data (`data_id,Date,...`: plain, gzipped, or the first .csv of a zip) is TeslaFi's (`kind:
"teslafi"`: one made-up car, "Blue Whale", that no car here has, so it must be matched; its report
counts rows, drives, charges, the units and the time zone); TeslaFi's drives or charges list fails
with `teslafi_summary`. Any other zip ("PK\x03\x04") is a CAN export (`kind: "sentry_manager_can"`:
no cars to match, its report counts logs), anything else a TeslaMate backup (`kind: "teslamate"`).
A zip named with "photos" is not a CAN export; a file the upload calls `compression: "zip"` that
does not start like one fails too. A made-up TeslaFi file to upload: `mockapi/data/TeslaFi32023.csv`.

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
import struct
import zlib

from ..conventions import page
from ..dataset import extras
from ..errors import ApiError, bad_input, conflict, invalid, not_found

CHECK_MS = 6_000
IMPORT_MS = 12_000
MAX_FILE = 64 * 1024 ** 3
COMPRESSIONS = ("plain", "gzip", "xz", "zip")
KIND_TM, KIND_CAN, KIND_TF = "teslamate", "sentry_manager_can", "teslafi"
# columns of TeslaFi's raw data; a header with "Date" and four of these is TeslaFi's
TF_MARKS = ("data_id", "shift_state", "charging_state", "battery_level", "odometer", "latitude", "longitude",
            "state", "carstate", "battery_range", "car_version")
TIME_ZONES = ("UTC", "Europe/Berlin", "Europe/London", "Europe/Amsterdam", "America/New_York", "America/Chicago",
              "America/Denver", "America/Los_Angeles", "Asia/Tokyo", "Australia/Sydney", "Pacific/Auckland")
ZIP_HEADS = (b"PK\x03\x04", b"PK\x05\x06")
# the CAN export's made-up logs: c3's key, and a key no car has yet
CAN_KEYS = (("cbk_c90a7716", 1440, 3), ("smo-new-0001", 60, 0))

def _job(req):
    j = req.store.imports.get(req.params["import"])
    if j is None:
        raise not_found("no_such_import", "There is no such import.")
    _advance(req, j)
    return j


def teslafi_sniff(head):
    """What the start of an upload is: "raw" (TeslaFi's data), "summary" (its drives or charges list) or None."""
    text = None
    try:
        if head[:2] == b"\x1f\x8b":
            text = zlib.decompressobj(16 + zlib.MAX_WBITS).decompress(head, 1 << 16)
        elif head[:4] == b"PK\x03\x04" and len(head) >= 30:
            method, csize = struct.unpack("<H", head[8:10])[0], struct.unpack("<I", head[18:22])[0]
            nlen, xlen = struct.unpack("<HH", head[26:30])
            name = head[30:30 + nlen].decode("utf-8", "replace").lower()
            if not name.endswith(".csv"):
                return None
            body = head[30 + nlen + xlen:]
            text = zlib.decompressobj(-15).decompress(body, 1 << 16) if method == 8 else body[:csize or None]
        else:
            text = head
    except (zlib.error, struct.error):
        return None
    line = text.decode("utf-8", "replace").lstrip("\ufeff").lstrip('"').splitlines()
    names = [x.strip().strip('"').lower() for x in (line[0].split(",") if line else [])]
    if "date" in names and sum(1 for m in TF_MARKS if m in names) >= 4:
        return "raw"
    if "start time" in names and "end time" in names and "data_id" not in names:
        return "summary"
    return None


def _advance(req, j):
    """Move a job along the virtual clock."""
    with req.store.lock:
        now = req.now
        if j["status"] == "checking" and now >= j["_check_done_ms"] and j.get("_tf"):
            _advance_tf(req, j)
        elif j["status"] == "checking" and now >= j["_check_done_ms"] and j.get("_zip") is not None:
            _advance_can(req, j)
        elif j["status"] == "checking" and now >= j["_check_done_ms"]:
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
            if j.get("kind") == KIND_CAN:
                j["report"] = can_report(req, imported=True, again=j.get("_again", False))
            elif j.get("kind") == KIND_TF:
                j["report"] = extras.teslafi_report(j["cars"], True, j.get("teslafi") or {}, j.get("_again", False))
            else:
                j["report"] = _report(req, j, imported=True)
        if j["status"] == "checking":
            f = (now - j["_check_start_ms"]) / CHECK_MS
            j["bytes_read"] = int(j["bytes_total"] * max(0.0, min(1.0, f)))
        if j["status"] == "importing":
            f = (now - j["_import_start_ms"]) / IMPORT_MS
            j["bytes_read"] = int(j["bytes_total"] * max(0.0, min(1.0, f)))


def _report(req, j, imported):
    return extras.import_report(j["cars"], imported)


def _advance_tf(req, j):
    """The end of a TeslaFi file's check."""
    done = j["_check_done_ms"]
    j["kind"] = KIND_TF
    if j["_tf"] == "summary":
        j.update(status="failed", finished_ms=done, error={
            "code": "teslafi_summary",
            "message": "this is TeslaFi's list of drives or charges, which has no positions: upload the monthly "
                       "files of Settings > Account > Advanced > Download TeslaFi Data instead"})
        return
    again = any(x.get("kind") == KIND_TF and x["status"] == "done" for x in req.store.imports.values())
    j["_again"] = again
    j.update(status="ready", bytes_read=j["bytes_total"], checked_ms=done,
             report=extras.teslafi_report(j["cars"], False, {}, again))
    j["pass"] = 1


def _advance_can(req, j):
    """The end of a zip's check: a CAN export, or a zip that is not one, or no zip at all."""
    done = j["_check_done_ms"]
    if not j["_zip"]:
        j["kind"] = KIND_CAN if j["compression"] == "zip" else KIND_TM
        j.update(status="failed", finished_ms=done, error={
            "code": "unreadable",
            "message": "This file is not a zip. For a TeslaMate backup upload the .sql (or .sql.gz, .sql.xz) itself; "
                       "from Sentry Six Manager, its CAN export bundle (.zip) as it was downloaded."})
        return
    j["kind"] = KIND_CAN
    if "photos" in (j.get("name") or ""):
        j.update(status="failed", finished_ms=done, error={
            "code": "not_a_can_export",
            "message": "this zip is not a Sentry Six Manager CAN export: it has no manifest.json naming a car key and "
                       "no CAN Bridge logs. In Sentry Six Manager use the CAN export bundle; for a TeslaMate backup, "
                       "upload the .sql (or .sql.gz, .sql.xz) itself"})
        return
    again = any(x.get("kind") == KIND_CAN and x["status"] == "done" for x in req.store.imports.values())
    j["_again"] = again
    j.update(status="ready", bytes_read=j["bytes_total"], checked_ms=done,
             report=can_report(req, imported=False, again=again))
    j["pass"] = 1


def can_report(req, imported, again=False):
    """A CAN export's report (the server's `engine::query::can_import` report): its logs per car key, what came
    in, what was here already and what was refused, and why. `again`: the same export a second time."""
    now = req.app.cfg.now_ms
    rows, refused_list = [], []
    tot = {"read": 0, "imported": 0, "to_import": 0, "already_here": 0, "refused": 0}
    for n, (key, logs, bad) in enumerate(CAN_KEYS):
        cid = "c3" if n == 0 and req.ds.exists("c3") else None
        good = logs - bad
        took, had = (0, good) if again else (good, 0)
        first, last = now - 40 * 86_400_000 - n * 3_600_000, now - 39 * 86_400_000
        status = "car" if cid else ("working" if imported else "new_key")
        rows.append({
            "key": key, "logs": logs, "imported": took if imported else 0, "already_here": had, "refused": bad,
            "first_ms": first, "last_ms": last, "car": cid, "car_name": req.store.car_meta(cid)["name"] if cid else None,
            "status": status, "other_signing_keys": [],
            "chain": {"signing_key": f"SHA256:made-up-{n}", "count": good, "first": 0, "last": good - 1,
                      "issues": [{"kind": "gap", "after": 811, "missing": 3}] if bad else [], "restarts": 0},
            "chain_gaps": 1 if bad else 0, "chain_forks": 0})
        for i in range(bad):
            refused_list.append({"name": f"{key}_20260101T0{i}0000Z_{812 + i}.log.xz", "reason": "failed_check",
                                 "message": "its SHA-256 is not the one it was signed with"})
        tot["read"] += logs
        tot["imported" if imported else "to_import"] += took
        tot["already_here"] += had
        tot["refused"] += bad
    rows_bytes = sum(r["logs"] for r in rows) * 410_000
    return {"kind": KIND_CAN, "imported": imported,
            "logs": dict(tot, sidecars_added=0, bytes=rows_bytes),
            "refused": {"failed_check": tot["refused"]} if tot["refused"] else {},
            "refused_logs": refused_list, "cars": rows,
            "span": {"from_ms": min(r["first_ms"] for r in rows), "to_ms": max(r["last_ms"] for r in rows)},
            "manifest": {"vehicle_key": CAN_KEYS[0][0], "from": None, "to": None, "made_at": None,
                         "logs": CAN_KEYS[0][1]},
            "files": {"entries": tot["read"] * 2 + 2, "candump_text": 0, "ids_csv": True,
                      "sidecars_without_log": 0, "other": 0},
            "warnings": []}


def view(j):
    out = {k: copy.deepcopy(v) for k, v in j.items() if not k.startswith("_") and k != "cars"}
    out.setdefault("kind", None)
    if j.get("kind") == KIND_CAN:
        out["cars_to_match"] = 0 if j["status"] not in ("uploading", "checking") else None
    else:
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
        comp = ("zip" if name.lower().endswith(".zip") else "xz" if name.endswith(".xz")
                else "gzip" if name.endswith(".gz") else "plain")
    if comp not in COMPRESSIONS:
        rule["compression"] = "Must be plain, gzip, xz or zip."
    if rule:
        raise invalid(rule)
    if running_import(req):
        raise conflict("import_running", "Another import is running. Wait for it or discard it first.")
    jid = st.new_id("im")
    j = {"id": jid, "name": name, "compression": comp, "kind": None, "status": "uploading", "pass": 0, "bytes_read": 0,
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
        if j["uploaded_bytes"] == 0:
            zip_ = req.body[:4] in ZIP_HEADS
            j["_tf"] = teslafi_sniff(req.body)
            if (zip_ or comp_says_zip(j)) and not j["_tf"]:
                j["_zip"] = zip_
        j["uploaded_bytes"] += len(req.body)
        j["bytes_read"] = j["uploaded_bytes"]
        if j["uploaded_bytes"] == j["bytes_total"]:
            j["status"] = "checking"
            j["pass"] = 1
            j["bytes_read"] = 0
            j["_check_start_ms"] = req.now
            j["_check_done_ms"] = req.now + CHECK_MS
            if j.get("_tf") == "raw":
                j["cars"] = [dict(c, match=_suggest(req, c)) for c in extras.teslafi_cars()]
            elif j.get("_zip") is None and not j.get("_tf"):
                j["cars"] = [dict(c, match=_suggest(req, c))
                             for c in extras.backup_cars(req.app.cfg.now_ms, req.ds.history("c3"))]
            st.rev += 1
    if j["status"] == "checking":
        st.record(req.account, "import.uploaded", f"import:{j['id']}", "Uploaded the whole file; checking it.")
    out = view(j)
    out["offset"] = j["uploaded_bytes"]
    return out


def comp_says_zip(j):
    return j["compression"] == "zip"


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
    if j.get("kind") == KIND_CAN:
        return {"import": j["id"], "items": [], "next": None}
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


def _start_body(req, j):
    """`{ currency?, time_zone?, distance_unit?, temperature_unit? }` (the last three for TeslaFi only)."""
    body = req.json(required=False) or {}
    bad = {k: "Unknown field." for k in body if k not in ("currency", "time_zone", "distance_unit", "temperature_unit")}
    if bad:
        raise bad_input(bad)
    rule = {}
    cur = body.get("currency")
    if cur is not None and not (isinstance(cur, str) and re.fullmatch(r"[A-Za-z]{3}", cur.strip())):
        rule["currency"] = "A currency code such as EUR."
    tz = body.get("time_zone")
    if tz is not None and tz not in TIME_ZONES:
        rule["time_zone"] = "An IANA time zone such as Europe/Berlin."
    du = body.get("distance_unit")
    if du is not None and du not in ("mi", "km"):
        rule["distance_unit"] = "mi or km."
    tu = body.get("temperature_unit")
    if tu is not None and tu not in ("c", "f"):
        rule["temperature_unit"] = "c or f."
    if j.get("kind") != KIND_TF:
        for k in ("time_zone", "distance_unit", "temperature_unit"):
            if body.get(k) is not None:
                rule[k] = "Only for a TeslaFi import."
    if rule:
        raise invalid(rule)
    return {"time_zone": tz, "distance_unit": du, "temperature_unit": tu}


def start(req):
    st = req.store
    j = _job(req)
    if j["status"] != "ready":
        raise conflict("not_ready", f"This import is {j['status']}; it can start once the whole file is checked.")
    settings = _start_body(req, j)
    if j.get("kind") == KIND_TF:
        j["teslafi"] = settings
    if j.get("kind") != KIND_CAN and any(c["match"] is None for c in j["cars"]):
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
