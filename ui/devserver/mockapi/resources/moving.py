"""First-run set-up and moving to another server (docs/ui-api.md, "Moving and first-run setup").

A fresh server (`--fresh`): the set-up page claims it with the one-time set-up code, then starts fresh (the
first admin and the recovery key, shown once), moves the data here from another server (its address and a
move code), or restores an export file (uploaded in chunks, opened with the recovery key).

A set-up server: Settings -> Server, "Move to another server" makes a move code, shows the move as a made-up
new server copies it, and undoes it; "Download export file" prepares an export and serves its bytes.

Everything runs on the virtual clock, in a few seconds, so a page can poll it from start to end. Nothing is
copied: the progress is worked out from the time. The dev server cannot reach another one, so the address
given to "Move here" picks what happens (see SCENARIOS and ui/devserver/README.md).
"""

from __future__ import annotations

import base64
import binascii
import hashlib
import json
import re
import secrets
import threading
import time

from ..conventions import Response
from ..errors import ApiError, bad_input, conflict, invalid, not_found
from ..util import DAY, HOUR, MIN

DEV_RECOVERY_KEY = "SMRK-DEVK-EY00-0000-0000-0000-0000-0000-0000-0000-0000-0000-0000-0000"
EXPORT_MAGIC = b"SENTRY-MATE-EXPORT dev\n"  # the dev server's own made-up export format
CLAIM_MS = HOUR  # a claim lasts an hour from its last use
CODE_VALID_MS = 24 * HOUR
THROTTLE_KEY_TRIES = 10  # wrong recovery keys from one address within 60 s
DEFAULT_PORT = 10100
APP_VERSION = "0.1.0-dev"

# what the made-up old and new servers hold
MOVE_FILES = 52_318
MOVE_BYTES = 18_431_260_672
CATCH_FILES = 37
CATCH_BYTES = 41_943_040
MOVE_CARS = 3

# the old server's side of a move, after the new server claimed the code (ms)
OLD_COPY_MS = 10_000
OLD_CATCH_MS = 3_000
OLD_PAUSE_MS = 2_000
OLD_SWITCH_MS = 2_000
PEER_ADDRESS = "192.0.2.30"

# the new server's side ("Move here"), from the start (ms)
NEW_CONNECT_MS = 1_000
NEW_COPY_MS = 6_000
NEW_CATCH_MS = 2_000
NEW_FINISH_MS = 1_500
NEW_SWITCH_MS = 1_500
FLAKY_DROP_MS = 5_000

# a restore, after the last chunk (ms)
RESTORE_CHECK_MS = 3_000
RESTORE_SWITCH_MS = 1_500
CHUNK_MAX = 1024 * 1024 - 1024
MAX_EXPORT = 64 * 1024 ** 4

# an export file: prepared in a few seconds, kept a day after its last download (a week at most)
EXPORT_PREPARE_MS = 3_000
EXPORT_SIZE = 4 * 1024 * 1024 + 123_457
EXPORT_FILES = 412
EXPORT_KEEP_MS = DAY
EXPORT_KEEP_MOST_MS = 7 * DAY
EXPORT_STALL_MS = MIN  # no progress for this long: `stalled`
EXPORT_SHA_MS = 2_000  # the checksum, after it is ready (quick)
# How a new export runs (--export): its counting, waiting and collecting steps (ms), what it carries, and where it
# stops (a fraction of the bytes collected: `stall` stops making progress there, `fail` fails there).
EXPORT_PROFILES = {
    "quick": {"count_ms": 500, "wait_ms": 300, "collect_ms": 2_200, "files": EXPORT_FILES,
              "bytes": EXPORT_SIZE - 4096, "sha_ms": EXPORT_SHA_MS},
    "slow": {"count_ms": 4_000, "wait_ms": 2_000, "collect_ms": 120_000, "files": MOVE_FILES,
             "bytes": MOVE_BYTES, "sha_ms": 30_000},
    "stall": {"count_ms": 4_000, "wait_ms": 2_000, "collect_ms": 120_000, "files": MOVE_FILES,
              "bytes": MOVE_BYTES, "sha_ms": 30_000, "stall_at": 0.4},
    "fail": {"count_ms": 4_000, "wait_ms": 2_000, "collect_ms": 40_000, "files": MOVE_FILES,
             "bytes": MOVE_BYTES, "sha_ms": 30_000, "fail_at": 0.55},
}
EXPORT_FAILURE = ("no_space", "not enough space: 18.4 GB are needed (with 2 GB to spare), 6.1 GB are free",
                  "Free some space on the data disk, then make the export again.")

# The first label of the address given to "Move here" picks what the made-up old server does.
SCENARIOS = {
    "unreachable": "nothing answers there: 502 old_server_unreachable, nothing starts",
    "wrong": "the old server does not take the code: 422 move_code_wrong",
    "expired": "the code expired: 422 move_code_expired",
    "used": "the code was used already: 409 move_code_used",
    "newer": "the old server runs a newer Sentry Mate: 422 move_too_old",
    "away": "the code works from the old server's home network only: 422 move_not_home_network",
    "nomove": "no move code waits there: 422 move_not_offered",
    "flaky": "the connection drops at 40 % for 5 s (waiting), then the copy carries on",
    "gone": "the connection drops at 50 % and never comes back (waiting, for good)",
    "cancel": "the old server cancels the move at 60 % (stopped: the copy so far is kept)",
}

B32 = "0123456789ABCDEFGHJKMNPQRSTVWXYZ"


# ------------------------------------------------------------------ codes and keys


def _symbols(text, prefix):
    """Crockford base32 symbols of a typed code: case, spaces and dashes do not matter; O reads as 0, I and L as 1."""
    t = text.strip().upper()
    if t.startswith(prefix):
        t = t[len(prefix):]
    out = []
    for ch in t:
        if ch in "- \t":
            continue
        ch = {"O": "0", "I": "1", "L": "1"}.get(ch, ch)
        i = B32.find(ch)
        if i < 0:
            return None
        out.append(i)
    return out


def _crc20(symbols):
    """The move code's check (crates/moving/src/code.rs): x^20 + x^3 + 1 over 5-bit symbols."""
    reg = 0
    for v in symbols:
        for i in range(4, -1, -1):
            fb = ((reg >> 19) ^ (v >> i)) & 1
            reg = (reg << 1) & 0xFFFFF
            if fb:
                reg ^= 0x9
    return [(reg >> (5 * (3 - i))) & 31 for i in range(4)]


def make_move_code():
    """SMMV- and six groups of five: the format (1), 25 random symbols and a 4-symbol check."""
    s = [1] + [secrets.randbelow(32) for _ in range(25)]
    s += _crc20(s)
    text = "".join(B32[v] for v in s)
    return "SMMV-" + "-".join(text[i:i + 5] for i in range(0, 30, 5)), hashlib.sha256(text.encode()).hexdigest()[:8]


def read_move_code(text):
    """None when the code reads, else ("typo" | "newer")."""
    s = _symbols(text, "SMMV")
    if s is None or len(s) != 30:
        return "typo"
    if _crc20(s[:26]) != s[26:]:
        return "typo"
    if s[0] != 1:
        return "newer" if s[0] > 1 else "typo"
    return None


def make_recovery_key():
    s = "".join(B32[secrets.randbelow(32)] for _ in range(52))
    return "SMRK-" + "-".join(s[i:i + 4] for i in range(0, 52, 4))


def _recovery_symbols(text):
    s = _symbols(text, "SMRK")
    return s if s is not None and len(s) == 52 else None


def _same_key(a, b):
    sa, sb = _recovery_symbols(a), _recovery_symbols(b)
    return sa is not None and sb is not None and secrets.compare_digest(bytes(sa), bytes(sb))


def _norm_setup_code(text):
    """Case, spaces, dashes and the SETUP- in front do not matter, and O, I and L read as 0, 1 and 1 (docs/ui-api.md;
    accounts::normal_setup_code)."""
    t = re.sub(r"[^0-9A-Za-z]", "", text).upper()
    t = t[len("SETUP"):] if t.startswith("SETUP") else t
    return t.translate(str.maketrans("OIL", "011"))


def parse_address(text):
    """(host, port) or raises 400 on `from`: the rules of moving::client::Address::parse."""
    t = text.strip()
    if t.lower().startswith("http://"):
        raise bad_input({"from": "The old server's address is https:// (its upload port), not http://."})
    if t.lower().startswith("https://"):
        t = t[8:]
    t = t.split("/")[0]
    bad = bad_input({"from": "This is not a server address. Write it like 192.0.2.20 or nas.local:10100 "
                             "(the address the CAN Bridge sends to)."})
    if t.startswith("["):
        h, sep, after = t[1:].partition("]")
        if not sep:
            raise bad
        if after.startswith(":") and after[1:].isdigit():
            port = int(after[1:])
        elif after == "":
            port = DEFAULT_PORT
        else:
            raise bad
        host = h
    elif t.count(":") > 1:
        host, port = t, DEFAULT_PORT
    elif ":" in t:
        host, p = t.split(":", 1)
        if not p.isdigit():
            raise bad
        port = int(p)
    else:
        host, port = t, DEFAULT_PORT
    if not host or not 0 < port < 65536 or not re.fullmatch(r"[A-Za-z0-9.\-:_%]+", host):
        raise bad
    return host, port


def _show_address(host, port):
    return f"[{host}]:{port}" if ":" in host else f"{host}:{port}"


# ------------------------------------------------------------------ state


def seed(st, cfg, now):
    st.setup = {"claim": None, "job": None}
    st.move = {"code": None, "moved": None, "cancelled": None, "undone_ms": None}
    st.exports = {}
    st.export_profile = getattr(cfg, "export_profile", "quick")
    _seed_export(st, getattr(cfg, "export_state", "none"), now)
    st.recovery_key_saved = False
    st.recovery_key = DEV_RECOVERY_KEY  # what the made-up RECOVERY-KEY.txt holds until it is saved
    preset = getattr(cfg, "move_state", "none")
    if preset == "none" or cfg.needs_setup:
        return
    code_id = "4f1c9a2e"
    made = now - MIN
    claim_at = None
    if preset == "expired":
        made = now - 25 * HOUR
    elif preset == "copying":
        claim_at = now - int(OLD_COPY_MS * 0.4)
    elif preset == "paused":
        claim_at = now - (OLD_COPY_MS + OLD_CATCH_MS + 500)
    elif preset == "moved":
        claim_at = now - (OLD_COPY_MS + OLD_CATCH_MS + OLD_PAUSE_MS + 500)
    elif preset == "done":
        claim_at = now - (OLD_COPY_MS + OLD_CATCH_MS + OLD_PAUSE_MS + OLD_SWITCH_MS + 2 * HOUR)
        made = claim_at - 20 * MIN
    st.move["code"] = {"id": code_id, "made_ms": made, "expires_ms": made + CODE_VALID_MS, "anywhere": False,
                       "wrong_tries": 0, "claim_at_ms": claim_at}


# --export-state: an export file already there when the server starts, and the profile it runs with
EXPORT_STATES = {
    "none": None,
    "preparing": ("slow", 50_000),  # collecting, about a third done
    "stalled": ("stall", 6_000 + 48_000 + 90_000),  # stopped at 40 % a minute and a half ago
    "failed": ("fail", 60_000),
    "ready": ("quick", 10_000),  # ready, its SHA-256 worked out
}


def _seed_export(st, state, now):
    preset = EXPORT_STATES.get(state)
    if not preset:
        return
    profile, ago = preset
    made = now - ago
    day = time.strftime("%Y-%m-%d", time.gmtime(made / 1000))
    eid = "ex7d3e9a41c2"
    st.exports[eid] = {"id": eid, "made_ms": made, "file_name": f"sentry-mate-{day}.smexport", "sent_bytes": 0,
                       "last_download_ms": None, "_bytes": None, "profile": profile}


def _peer_after_ms(st):
    s = getattr(st.cfg, "move_peer_after_s", 15)
    return s * 1000 if s else None


# ------------------------------------------------------------------ re-auth (dangerous actions)


def _reauth(req, body):
    """The account's own password again, and its two-step code when two-step is on: 400 bad_input without one,
    403 wrong_password or wrong_two_step (throttled like sign-in) when it is not right."""
    from .auth import _check_throttle, _fail
    from ..store import take_code
    st = req.store
    a = req.account
    pw = body.get("password")
    # missing (or not text): 400 before anything else, as the real server reads the body first
    if not isinstance(pw, str):
        raise bad_input({"password": "Required." if pw is None else "Must be text."})
    if body.get("two_step") is not None and not isinstance(body.get("two_step"), str):
        raise bad_input({"two_step": "Must be text."})
    key = "name:" + a["name"].strip().lower()
    _check_throttle(st, key)
    if not pw:
        _fail(st, key)
        raise ApiError(403, "wrong_password", "That password is not right. Type your own password again.",
                       fields={"password": "Not your password."})
    if not secrets.compare_digest(pw.encode(), a["password"].encode()):
        _fail(st, key)
        raise ApiError(403, "wrong_password", "That password is not right. Type your own password again.",
                       fields={"password": "Not your password."})
    if a.get("two_step"):
        code = body.get("two_step")
        if not take_code(a, code):
            _fail(st, key)
            raise ApiError(403, "wrong_two_step", "Type the code from your authenticator app as well.",
                           fields={"two_step": "Required: the 6-digit code." if not code else "Not the right code."})


def _flag(body, name, default=False):
    v = body.get(name, default)
    if not isinstance(v, bool):
        raise bad_input({name: "true or false."})
    return v


def _only(body, allowed):
    extra = {k: "Unknown field." for k in body if k not in allowed}
    if extra:
        raise bad_input(extra)


# ------------------------------------------------------------------ the old server's side


def _old_phase(st, now):
    """(state, code, claimed_ms) of this server's side of a move."""
    m = st.move
    if m["moved"] is not None:
        return ("done" if m["moved"]["switched_ms"] and now >= m["moved"]["switched_ms"] else "moved"), m["code"], None
    c = m["code"]
    if c is None:
        return "none", None, None
    claim = c.get("claim_at_ms")
    if claim is None or now < claim or claim >= c["expires_ms"]:
        return ("expired" if now >= c["expires_ms"] else "offered"), c, None
    t = now - claim
    if t < OLD_COPY_MS + OLD_CATCH_MS:
        return "copying", c, claim
    if t < OLD_COPY_MS + OLD_CATCH_MS + OLD_PAUSE_MS:
        return "paused", c, claim
    return ("moved" if t < OLD_COPY_MS + OLD_CATCH_MS + OLD_PAUSE_MS + OLD_SWITCH_MS else "done"), c, claim


def _progress(round_, files_done, files_total, bytes_done, bytes_total, per_s):
    left = max(0, bytes_total - bytes_done) if bytes_total is not None else None
    # the cars are copied one after the other: a car counts once all its files are over
    cars_total = MOVE_CARS if files_total else None
    cars_done = min(MOVE_CARS, int(MOVE_CARS * files_done / files_total)) if files_total else 0
    return {"round": round_, "files_done": int(files_done), "files_total": files_total, "bytes_done": int(bytes_done),
            "bytes_total": bytes_total, "cars_done": cars_done, "cars_total": cars_total, "bytes_per_s": per_s,
            "eta_s": int(left / per_s) if per_s and left is not None else None}


def _old_progress(t):
    per_s = int(MOVE_BYTES / (OLD_COPY_MS / 1000))
    if t < OLD_COPY_MS:
        f = t / OLD_COPY_MS
        return _progress(1, MOVE_FILES * f, MOVE_FILES, MOVE_BYTES * f, MOVE_BYTES, per_s)
    total_f, total_b = MOVE_FILES + CATCH_FILES, MOVE_BYTES + CATCH_BYTES
    if t < OLD_COPY_MS + OLD_CATCH_MS:
        f = (t - OLD_COPY_MS) / OLD_CATCH_MS
        return _progress(2, MOVE_FILES + CATCH_FILES * f, total_f, MOVE_BYTES + CATCH_BYTES * f, total_b,
                         int(CATCH_BYTES / (OLD_CATCH_MS / 1000)))
    return _progress(3, total_f, total_f, total_b, total_b, None)


# ------------------------------------------------------------------ the move's log (as the real server words it)

LOG_MAX = 500  # the lines kept
# what each file copied is, in words (the real server names a file by its kind, never its path)
FILE_KINDS = ("a CAN log", "a CAN log's sidecar", "a part of the history database", "a trust record", "an upload token",
              "a web setting", "the accounts", "the audit log")
FILE_LINE_MS = 2_000  # a "Copying file" line at most this often


def _size(n):
    for unit, k in (("GB", 1e9), ("MB", 1e6), ("KB", 1e3)):
        if n >= k:
            return f"{n / k:.1f} {unit}"
    return f"{int(n)} bytes"


def _when(ms):
    return time.strftime("%Y-%m-%d at %H:%M UTC", time.gmtime(ms / 1000))


def _line(at, text):
    return {"at_ms": int(at), "text": text}


def _keep(lines):
    lines = sorted(lines, key=lambda x: x["at_ms"])
    return lines[-LOG_MAX:]


def _old_timeline(c, claim, now):
    """The lines a move away from here has written by `now`."""
    out = []
    if c is None:
        return out
    reach = "from anywhere" if c["anywhere"] else "from the home network only"
    out.append(_line(c["made_ms"], f"A move code was made: it works once, {reach}, until {_when(c['expires_ms'])}."))
    if claim is None or now < claim:
        return out
    out.append(_line(claim, f"The new server at {PEER_ADDRESS} connected (Sentry Mate {APP_VERSION}): "
                            f"{MOVE_FILES + CATCH_FILES} files, {_size(MOVE_BYTES)} to copy."))
    sent_end = OLD_COPY_MS + OLD_CATCH_MS
    for k in range(1, 11):
        at = claim + sent_end * k // 10
        if at <= now:
            out.append(_line(at, f"Sent {k * 10} % to the new server ({_size(MOVE_BYTES * k / 10)} of {_size(MOVE_BYTES)})."))
    steps = [
        (sent_end, "Last-round pause: uploads wait here while the new server copies the last changes."),
        (sent_end + OLD_PAUSE_MS, f"The new server's copy matches: the data moved to {PEER_ADDRESS}. This server takes "
                                  "no more uploads and stops its inputs; the new server switches."),
        (sent_end + OLD_PAUSE_MS + 300, "Inputs stopped on this server: the Tesla API (1 account), TeslaFi (1 account), "
                                        "MQTT publishing (its retained topics cleared), the history's own work "
                                        "(decoding, rebuilds, the GPS fit, compaction, the upkeep)."),
        (sent_end + OLD_PAUSE_MS + OLD_SWITCH_MS, f"Done: {PEER_ADDRESS} serves the data now. This server takes no "
                                                  "uploads; you can turn it off (keep its folders as a backup for a "
                                                  "while)."),
    ]
    out += [_line(claim + t, text) for t, text in steps if claim + t <= now]
    return out


def old_log(st, now):
    m = st.move
    c = m["code"]
    claim = c.get("claim_at_ms") if c else None
    if claim is not None and claim >= c["expires_ms"]:
        claim = None
    return _keep(list(m.get("log") or []) + _old_timeline(c, claim, now))


def _freeze_old_log(st, now, text):
    """An action that ends the move's code here (a cancel, an undo): what the log said so far stays, with a line."""
    m = st.move
    m["log"] = _keep(old_log(st, now) + [_line(now, text)])


def old_view(st, now):
    state, c, claim = _old_phase(st, now)
    m = st.move
    out = {"state": state, "code": None, "peer": None, "progress": None, "moved": None,
           "last_cancelled": m["cancelled"], "last_undone_ms": m["undone_ms"], "skipped": [], "problem": None,
           "log": old_log(st, now)}
    if c is not None:
        out["code"] = {"id": c["id"], "made_ms": c["made_ms"], "expires_ms": c["expires_ms"],
                       "anywhere": c["anywhere"], "wrong_tries": c["wrong_tries"]}
    if claim is not None:
        t = now - claim
        moved_at = claim + OLD_COPY_MS + OLD_CATCH_MS + OLD_PAUSE_MS
        out["peer"] = {"address": PEER_ADDRESS, "app": APP_VERSION, "claimed_ms": claim,
                       "last_seen_ms": min(now, moved_at + OLD_SWITCH_MS)}
        out["progress"] = _old_progress(t)
        if state in ("moved", "done"):
            switched = moved_at + OLD_SWITCH_MS
            out["moved"] = {"at_ms": moved_at, "to": PEER_ADDRESS, "switched_ms": switched if now >= switched else None}
        out["skipped"] = [{"path": "lost+found", "why": "this server can't read it (not carried)"}] if t > 2000 else []
    elif m["moved"] is not None:
        out["moved"] = dict(m["moved"])
    return out


def get_move(req):
    return old_view(req.store, req.now)


def post_move(req):
    st = req.store
    body = req.json(required=False)
    _only(body, ("password", "two_step", "anywhere", "replace"))
    anywhere = _flag(body, "anywhere")
    replace = _flag(body, "replace")
    _reauth(req, body)
    state, c, _claim = _old_phase(st, req.now)
    if state in ("moved", "done"):
        raise conflict("moved", "This server's data moved already. To take uploads here again: Undo the move.")
    if state in ("copying", "paused") and not replace:
        raise ApiError(409, "move_under_way",
                       f"A move is under way: {PEER_ADDRESS} is copying this server. To end it and make a new code, "
                       "send replace: true (the copy there stops).")
    text, cid = make_move_code()
    after = _peer_after_ms(st)
    with st.lock:
        if state in ("copying", "paused"):
            st.move["cancelled"] = {"at_ms": req.now, "why": "replaced by a new move code"}
        st.move["code"] = {"id": cid, "made_ms": req.now, "expires_ms": req.now + CODE_VALID_MS, "anywhere": anywhere,
                           "wrong_tries": 0, "claim_at_ms": req.now + after if after else None}
        st.move["moved"] = None
        st.move["log"] = []
    st.record(req.account, "move-code-made", f"move:{cid}",
              f"Made a move code, valid 24 hours, usable {'from anywhere' if anywhere else 'on the home network'}.")
    out = old_view(st, req.now)
    out["move_code"] = text
    out["_created"] = True
    return out


def delete_move(req):
    st = req.store
    state, c, _claim = _old_phase(st, req.now)
    if state in ("moved", "done"):
        raise conflict("finished", "The move finished: the new server holds the data. To take uploads here again: "
                                   "Undo the move.")
    what = "nothing"
    with st.lock:
        if state in ("copying", "paused"):
            what = "ended"
            st.move["cancelled"] = {"at_ms": req.now, "why": "cancelled on this server"}
            _freeze_old_log(st, req.now, "The move was cancelled here: uploads go on as before.")
        elif c is not None:
            what = "revoked"
            _freeze_old_log(st, req.now, "The move code was cancelled.")
        st.move["code"] = None
    if what != "nothing":
        st.record(req.account, "move-cancelled", "move:" + ((c or {}).get("id") or "current"),
                  "Ended the move under way." if what == "ended" else "Cancelled the move code.")
    out = old_view(st, req.now)
    out["cancelled"] = what
    return out


def post_undo(req):
    st = req.store
    body = req.json(required=False)
    _only(body, ("password", "two_step", "confirm"))
    if body.get("confirm") is not True:
        raise bad_input({"confirm": "Send confirm: true. Stop the new server first: if both take uploads, they "
                                    "drift apart."})
    _reauth(req, body)
    state, _c, _claim = _old_phase(st, req.now)
    if state not in ("moved", "done"):
        raise conflict("not_moved", "This server has not moved; there is nothing to undo.")
    with st.lock:
        _freeze_old_log(st, req.now, "The move was undone: this server takes uploads again.")
        st.move = {"code": None, "moved": None, "cancelled": st.move["cancelled"], "undone_ms": req.now,
                   "log": st.move["log"]}
    st.record(req.account, "move-undone", "move:current", "Undid the move: this server takes uploads again.")
    return old_view(st, req.now)


def move_notices(req):
    """The notices the old server's side of a move adds (/api/v1/notices)."""
    v = old_view(req.store, req.now)
    s = v["state"]
    if s == "offered":
        return [{"code": "move_code_waiting", "level": "info",
                 "message": "A move code is waiting for the new server.", "since_ms": v["code"]["made_ms"],
                 "fix_link": "/settings/server#move", "detail": {"expires_ms": v["code"]["expires_ms"]}}]
    if s in ("copying", "paused"):
        return [{"code": "moving", "level": "info",
                 "message": f"{v['peer']['address']} is copying this server. Keep it on.",
                 "since_ms": v["peer"]["claimed_ms"], "fix_link": "/settings/server#move", "detail": None}]
    if s in ("moved", "done"):
        mv = v["moved"]
        words = "You can turn it off." if s == "done" else "Keep it on until the new server says it took over."
        return [{"code": "moved", "level": "warning",
                 "message": f"This server's data moved to {mv['to']}. It takes no uploads. {words}",
                 "since_ms": mv["at_ms"], "fix_link": "/settings/server#move",
                 "detail": {"to": mv["to"], "at_ms": mv["at_ms"], "switched_ms": mv["switched_ms"]}}]
    return []


def refuses_changes(st, now):
    """A moved server takes no changes (reading still works)."""
    return _old_phase(st, now)[0] in ("moved", "done")


# ------------------------------------------------------------------ export files


def _export_bytes(e):
    if e.get("_bytes") is None:
        head = json.dumps({"export": e["id"], "app": APP_VERSION, "made_ms": e["made_ms"], "files": EXPORT_FILES,
                           "made_up": True}).encode() + b"\n"
        body = hashlib.shake_256(("sentry-mate dev export " + e["id"]).encode()).digest(EXPORT_SIZE - len(head)
                                                                                         - len(EXPORT_MAGIC))
        e["_bytes"] = EXPORT_MAGIC + head + body
    return e["_bytes"]


def _export_progress(done, total, files_done, files_total, phase, step, started, phase_ms, progress_ms, per_s, now):
    known = total is not None
    pct = None if not known else round(1000 * done / total) / 10 if total else 100.0
    eta = -(-(total - done) // per_s) if known and per_s else None
    quiet = max(0, now - progress_ms)
    return {"phase": phase, "step": step, "steps": 3, "files_done": files_done, "files_total": files_total,
            "bytes_done": done, "bytes_total": total, "percent": pct, "bytes_per_s": per_s, "eta_s": eta,
            "started_ms": started, "phase_started_ms": phase_ms, "progress_ms": progress_ms, "quiet_ms": quiet,
            "stalled": quiet >= EXPORT_STALL_MS}


def _export_timeline(e, now):
    """Where an export is at `now`: (phase, progress or None, ready_ms or None, failed_ms or None)."""
    p = EXPORT_PROFILES[e.get("profile", "quick")]
    t0 = e["made_ms"]
    t = now - t0
    c_end = p["count_ms"]
    w_end = c_end + p["wait_ms"]
    k_end = w_end + p["collect_ms"]
    if t < c_end:  # counting: what it will carry, no total yet
        f = t / c_end
        files = int(p["files"] * f)
        return "counting", _export_progress(int(p["bytes"] * f), None, files, None, "counting", 1, t0, t0,
                                     now, None, now), None, None
    if t < w_end:  # waiting for the writes under way: nothing moves
        return "waiting", _export_progress(0, 0, 0, 0, "waiting", 2, t0, t0 + c_end, t0 + c_end, None, now), None, None
    rate = p["bytes"] * 1000 // p["collect_ms"]
    stop = p.get("stall_at") or p.get("fail_at")
    stop_ms = w_end + int(p["collect_ms"] * stop) if stop else None
    if p.get("fail_at") and t >= stop_ms:
        return "failed", None, None, t0 + stop_ms
    if t < k_end or p.get("stall_at"):  # a stalled one never gets further
        at = min(t, stop_ms) if stop_ms else t
        f = (at - w_end) / p["collect_ms"]
        moving = not (stop_ms and t >= stop_ms)
        return "collecting", _export_progress(int(p["bytes"] * f), p["bytes"], int(p["files"] * f), p["files"],
                                       "collecting", 3, t0, t0 + w_end, t0 + at,
                                       rate if moving else 0, now), None, None
    return "ready", None, t0 + k_end, None


def export_view(e, now):
    phase, progress, ready_ms, failed_ms = _export_timeline(e, now)
    out = {"id": e["id"], "state": "preparing", "phase": phase, "progress": progress, "checksum": None,
           "sha256": None, "problem": None, "now_ms": now, "file_name": e["file_name"], "files": None,
           "bytes": None, "size": None, "sent_bytes": e["sent_bytes"], "made_ms": e["made_ms"], "expires_ms": None,
           "url": None, "skipped": [], "error": None}
    if failed_ms is not None:
        code, message, what = EXPORT_FAILURE
        out.update(state="failed", phase="failed", error=message,
                   problem={"code": code, "message": message, "what_to_do": what})
    elif ready_ms is not None:
        p = EXPORT_PROFILES[e.get("profile", "quick")]
        expires = min(e["made_ms"] + EXPORT_KEEP_MOST_MS, max(e["made_ms"], e["last_download_ms"] or 0)
                      + (ready_ms - e["made_ms"]) + EXPORT_KEEP_MS)
        state = "expired" if now >= expires else "ready"
        out.update(state=state, phase=state)
        if state == "ready":
            sha_t = now - ready_ms
            if sha_t >= p["sha_ms"]:
                checksum = {"state": "done", "bytes_done": None, "bytes_total": None, "percent": None,
                            "bytes_per_s": None, "eta_s": None, "quiet_ms": None, "stalled": False, "error": None}
                out["sha256"] = hashlib.sha256(_export_bytes(e)).hexdigest()
            else:
                checksum = _export_progress(EXPORT_SIZE * sha_t // p["sha_ms"], EXPORT_SIZE, 0, 0, "checking", 3,
                                     e["made_ms"], ready_ms, now, EXPORT_SIZE * 1000 // p["sha_ms"], now)
                checksum.update(state="working", error=None)
            out.update(checksum=checksum, files=p["files"],
                       bytes=EXPORT_SIZE - 4096 if p["bytes"] == EXPORT_SIZE - 4096 else p["bytes"], size=EXPORT_SIZE, expires_ms=expires,
                       url=f"/api/v1/system/exports/{e['id']}/file",
                       skipped=[{"path": "lost+found", "why": "this server can't read it (not carried)"}])
    return out


def _export(req):
    e = req.store.exports.get(req.params["export"])
    if e is None:
        raise not_found("no_such_export", "There is no such export. Make a new one.")
    return e


def list_exports(req):
    items = [export_view(e, req.now) for e in sorted(req.store.exports.values(), key=lambda x: -x["made_ms"])]
    return {"items": items, "next": None}


def post_export(req):
    st = req.store
    body = req.json(required=False)
    _only(body, ("password", "two_step"))
    _reauth(req, body)
    for e in st.exports.values():
        if export_view(e, req.now)["state"] == "preparing":
            raise conflict("export_preparing", "An export is being prepared. Wait a few seconds for it.")
    eid = st.new_id("ex")
    day = time.strftime("%Y-%m-%d", time.gmtime(req.now / 1000))
    e = {"id": eid, "made_ms": req.now, "file_name": f"sentry-mate-{day}.smexport", "sent_bytes": 0,
         "last_download_ms": None, "_bytes": None, "profile": st.export_profile}
    with st.lock:
        st.exports.clear()  # one export at a time: a new one replaces the one there was
        st.exports[eid] = e
    st.record(req.account, "export-made", f"export:{eid}", "Made an export file of the whole server.")
    out = export_view(e, req.now)
    out["_created"] = True
    return out


def get_export(req):
    return export_view(_export(req), req.now)


def delete_export(req):
    """Removes the export file; one being made is cancelled."""
    e = _export(req)
    cancelled = export_view(e, req.now)["state"] == "preparing"
    with req.store.lock:
        req.store.exports.pop(e["id"], None)
    req.store.record(req.account, "export-removed", f"export:{e['id']}",
                     "Cancelled the export file being made." if cancelled else "Removed the export file.")
    return {"deleted": True, "id": e["id"], "cancelled": cancelled}


def export_file(req):
    e = _export(req)
    v = export_view(e, req.now)
    if v["state"] == "preparing":
        raise conflict("not_ready", "The export is being prepared. Try again in a few seconds.")
    if v["state"] in ("expired", "failed"):
        raise not_found("export_gone", "This export was removed. Make a new one.")
    data = _export_bytes(e)
    size = len(data)
    headers = {"Content-Disposition": f'attachment; filename="{e["file_name"]}"', "ETag": f'"{e["id"]}"',
               "Accept-Ranges": "bytes"}
    rng = req.headers.get("Range")
    if_range = req.headers.get("If-Range")
    status, part = 200, data
    if rng and (if_range is None or if_range.strip() == f'"{e["id"]}"'):
        m = re.fullmatch(r"bytes=(\d*)-(\d*)", rng.strip())
        a = b = None
        if m and (m.group(1) or m.group(2)):
            if m.group(1):
                a = int(m.group(1))
                b = int(m.group(2)) if m.group(2) else size - 1
            else:
                a = max(0, size - int(m.group(2)))
                b = size - 1
        if a is None or a >= size or b < a:
            return Response(416, b"", "application/octet-stream", dict(headers, **{"Content-Range": f"bytes */{size}"}))
        b = min(b, size - 1)
        status, part = 206, data[a:b + 1]
        headers["Content-Range"] = f"bytes {a}-{b}/{size}"
    with req.store.lock:
        e["sent_bytes"] = min(size, e["sent_bytes"] + len(part))
        if e["last_download_ms"] is None:
            req.store.record(req.account, "export-downloaded", f"export:{e['id']}", "Downloaded the export file.")
        e["last_download_ms"] = req.now
    return Response(status, part, "application/octet-stream", headers)


def _key_file_there(st):
    return any(n["code"] == "recovery_key_unsaved" for n in st.notices)


def post_recovery_show(req):
    """The recovery key file's key once more, behind the password: for a key page closed before "I have saved it"
    was ticked. Only while the file is there; nothing shows the key after that."""
    st = req.store
    body = req.json(required=False)
    _only(body, ("password", "two_step"))
    _reauth(req, body)
    if not _key_file_there(st):
        raise conflict("recovery_key_saved", "The recovery key file is gone: it was deleted once the key was saved, "
                                             "so nothing can show the key again. Use the copy you saved.")
    st.record(req.account, "recovery-key-shown", "server:recovery-key", "Showed the recovery key from its file again.")
    return {"recovery_key": st.recovery_key}


# one new key at a time, as on the real server (each takes 1 GiB for a few seconds there): a second one meanwhile
# answers 503 busy
_MAKING_KEY = threading.Lock()
MAKE_KEY_S = 0.3


def post_recovery_new(req):
    """A new recovery key, shown this once (the old one stops working; exports and data backups made before still
    need the old one). Like the first one, it waits in RECOVERY-KEY.txt until "I have saved it". A moved server
    refuses it in the server's own check (409 moved)."""
    st = req.store
    body = req.json(required=False)
    _only(body, ("password", "two_step"))
    _reauth(req, body)
    if _busy(st, req.now) is not None:
        raise conflict("setup_busy", "A move or an import runs here: wait until it is done.")
    if not _MAKING_KEY.acquire(blocking=False):
        raise ApiError(503, "busy", "A new recovery key is being made; try again in a minute.",
                       headers={"Retry-After": "30"})
    try:
        time.sleep(MAKE_KEY_S)
        key = make_recovery_key()
    finally:
        _MAKING_KEY.release()
    with st.lock:
        st.recovery_key = key
        st.recovery_key_saved = False
        if not _key_file_there(st):
            st.notices.append({"code": "recovery_key_unsaved", "level": "info",
                               "message": "Save the vault recovery key file somewhere safe.", "since_ms": req.now,
                               "fix_link": "/settings/server"})
    st.record(req.account, "recovery-key-new", "server:recovery-key", "Made a new recovery key; the old one stopped working.")
    return {"recovery_key": key, "made_ms": req.now, "_created": True}


# not under the server's one-change-at-a-time lock: a second new key meanwhile must see the first one being made
post_recovery_new.own_locks = True


def post_recovery_saved(req):
    """"I have saved it": the file is removed (none is fine). No body, or {}."""
    st = req.store
    _only(req.json(required=False), ())
    with st.lock:
        st.notices = [n for n in st.notices if n["code"] != "recovery_key_unsaved"]
        st.recovery_key_saved = True
        st.recovery_key = None
    st.record(req.account, "recovery-key-saved", "server:recovery-key", "Said the recovery key is saved; its file was removed.")
    return {"saved": True}


# ------------------------------------------------------------------ first-run set-up


def _job_problem(code, message, retry_ms):
    return {"code": code, "message": message, "retry_ms": retry_ms}


def _scenario(host):
    first = host.lower().split(".")[0]
    if host.lower().endswith(".invalid"):
        return "unreachable"
    return first if first in SCENARIOS else None


def job_view(job, now):
    """A move here or a restore as it is at `now` (the job's own times decide it)."""
    if job["kind"] == "move_here":
        out = _move_here_view(job, now)
        out["log"] = _move_here_log(job, out, now)
        return out
    return _restore_view(job, now)


def _base(job):
    return {"id": job["id"], "kind": job["kind"], "state": None, "started_ms": job["started_ms"],
            "from": job.get("from"), "file_name": job.get("file_name"), "size": job.get("size"),
            "received": job.get("received"), "progress": None, "problem": None, "result": None, "skipped": [],
            "log": []}


def _copy_lines(t0, t1, files):
    """The round's "Copying file N of M" lines from `t0` to `t1` (one every FILE_LINE_MS)."""
    out = []
    span = max(1, NEW_COPY_MS)
    at = t0
    k = 0
    while at <= t1:
        n = 1 + int((at - t0) * (files - 1) / span)
        out.append(_line(at, f"Copying file {n} of {files} ({FILE_KINDS[k % len(FILE_KINDS)]})."))
        at += FILE_LINE_MS
        k += 1
    return out


def _move_here_log(job, view, now):
    """The lines a move here has written by `now`, from its own times (the dev server keeps no log of its own)."""
    start = job["started_ms"]
    clock0 = job.get("clock0_ms", start)
    sc = job.get("scenario")
    drop = FLAKY_DROP_MS if sc == "flaky" else 0
    out = list(job.get("log_before") or [])
    out.append(_line(start, f"Moving here from {job['from']}: proving the move code to the old server."))
    c0 = clock0 + NEW_CONNECT_MS
    total_f = MOVE_FILES + CATCH_FILES
    if now >= c0:
        out.append(_line(c0, f"Connected: the old server proved the move code (Sentry Mate {APP_VERSION}, this one "
                             f"{APP_VERSION}): {total_f} files, {_size(MOVE_BYTES)} to copy."))
        out.append(_line(c0 + 1, "Round 1: copying the old server's files."))
    state = view["state"]
    pr = view.get("problem")
    copy_end = c0 + NEW_COPY_MS + drop
    if sc == "flaky":
        s0 = c0 + int(NEW_COPY_MS * 0.4)
        out += _copy_lines(c0 + 2, min(now, s0 - 1), MOVE_FILES)
        if now >= s0:
            out.append(_line(s0, f"Waiting: lost the connection to the old server at {job['from']}; trying again "
                                 "by itself."))
        if now >= s0 + drop:
            out.append(_line(s0 + drop, "Connected to the old server again: carrying on where it stopped."))
            later = _copy_lines(c0 + 2, min(now, copy_end - 1) - drop, MOVE_FILES)
            out += [_line(x["at_ms"] + drop, x["text"]) for x in later if x["at_ms"] >= s0]
    elif sc in ("gone", "cancel") and pr is not None:
        stop_at = c0 + int(NEW_COPY_MS * (0.5 if sc == "gone" else 0.6))
        out += _copy_lines(c0 + 2, min(now, stop_at - 1), MOVE_FILES)
        word = "Waiting" if pr.get("retry_ms") else "Stopped"
        tail = "; trying again by itself." if word == "Waiting" else "."
        out.append(_line(stop_at, f"{word}: {pr['message']}{tail}"))
        return _keep(out)
    else:
        out += _copy_lines(c0 + 2, min(now, copy_end - 1), MOVE_FILES)
    if job.get("forced_ms") is not None:
        fz = job["forced_ms"]
        out = [x for x in out if x["at_ms"] <= fz]
        out.append(_line(fz, "Switching to what was copied, opened with the recovery key: the last changes may be "
                             "missing."))
        if state == "done":
            out.append(_line(fz + NEW_SWITCH_MS, "Switched: this server holds the data now."))
        return _keep(out)
    steps = [
        (copy_end, f"Round 1 done: {MOVE_FILES} files, {_size(MOVE_BYTES)} copied."),
        (copy_end + NEW_CATCH_MS, f"Round 2 caught up {CATCH_FILES} files ({_size(CATCH_BYTES)})."),
        (copy_end + NEW_CATCH_MS + 1, "Caught up: asking the old server for the last-round pause."),
        (copy_end + NEW_CATCH_MS + 2, "Last-round pause: uploads wait on the old server while the last changes are "
                                      "copied."),
        (copy_end + NEW_CATCH_MS + NEW_FINISH_MS, "Last round: 12 files (6.4 KB) in 1 s. The old server confirmed the "
                                                  "copy and stopped its inputs."),
        (copy_end + NEW_CATCH_MS + NEW_FINISH_MS + 1, "Switching to the copied data."),
        (copy_end + NEW_CATCH_MS + NEW_FINISH_MS + NEW_SWITCH_MS, "Switched: this server holds the data now."),
        (copy_end + NEW_CATCH_MS + NEW_FINISH_MS + NEW_SWITCH_MS + 800,
         "Inputs started here: the Tesla API (1 account), TeslaFi (1 account), MQTT publishing, the history's own "
         "work, each from where the old server left it (its waits, pacing and catch-up marks came with the data)."),
    ]
    out += [_line(at, text) for at, text in steps if at <= now]
    return _keep(out)


def _move_here_view(job, now):
    out = _base(job)
    clock0 = job.get("clock0_ms", job["started_ms"])  # moved on when a stopped move carries on
    e = now - clock0
    sc = job.get("scenario")
    copy0 = NEW_CONNECT_MS
    per_s = int(MOVE_BYTES / (NEW_COPY_MS / 1000))
    stop_at = None
    if sc == "flaky":
        s0 = copy0 + int(NEW_COPY_MS * 0.4)
        if s0 <= e < s0 + FLAKY_DROP_MS:
            out["state"] = "waiting"
            out["problem"] = _job_problem("unreachable", f"lost the connection to the old server at {job['from']}; "
                                                         "trying again (it carries on where it stopped)",
                                          max(1000, s0 + FLAKY_DROP_MS - e))
            f = 0.4
            out["progress"] = _progress(1, MOVE_FILES * f, MOVE_FILES, MOVE_BYTES * f, MOVE_BYTES, None)
            return out
        if e >= s0 + FLAKY_DROP_MS:
            e -= FLAKY_DROP_MS
    elif sc in ("gone", "cancel"):
        stop_at = copy0 + int(NEW_COPY_MS * (0.5 if sc == "gone" else 0.6))
    forced = job.get("forced_ms")
    if stop_at is not None and e >= stop_at and forced is None:
        f = (stop_at - copy0) / NEW_COPY_MS
        out["progress"] = _progress(1, MOVE_FILES * f, MOVE_FILES, MOVE_BYTES * f, MOVE_BYTES, None)
        if sc == "gone":
            out["state"] = "waiting"
            out["problem"] = _job_problem("unreachable", f"nothing answers at {job['from']}; trying again "
                                                         "(it carries on where it stopped)", 30_000)
        else:
            out["state"] = "stopped"
            out["problem"] = _job_problem("cancelled", "the move was cancelled on the old server. What was copied "
                                                       "here is kept: with a new move code from the same server it "
                                                       "carries on from there.", None)
        return out
    if forced is not None:
        f = (stop_at - copy0) / NEW_COPY_MS if stop_at else 1.0
        out["progress"] = _progress(1, MOVE_FILES * f, MOVE_FILES, MOVE_BYTES * f, MOVE_BYTES, None)
        if now < forced + NEW_SWITCH_MS:
            out["state"] = "switching"
        else:
            out["state"] = "done"
            out["result"] = _result(job, forced + NEW_SWITCH_MS, forced=True)
        return out
    if e < copy0:
        out["state"] = "connecting"
        out["progress"] = _progress(0, 0, None, 0, None, None)
        return out
    out["skipped"] = [{"path": "lost+found", "why": "the old server can't read it (not carried)"}]
    t = e - copy0
    total_f, total_b = MOVE_FILES + CATCH_FILES, MOVE_BYTES + CATCH_BYTES
    if t < NEW_COPY_MS:
        f = t / NEW_COPY_MS
        out["state"] = "copying"
        out["progress"] = _progress(1, MOVE_FILES * f, MOVE_FILES, MOVE_BYTES * f, MOVE_BYTES, per_s)
        return out
    t -= NEW_COPY_MS
    if t < NEW_CATCH_MS:
        f = t / NEW_CATCH_MS
        out["state"] = "catching_up"
        out["progress"] = _progress(2, MOVE_FILES + CATCH_FILES * f, total_f, MOVE_BYTES + CATCH_BYTES * f, total_b,
                                    int(CATCH_BYTES / (NEW_CATCH_MS / 1000)))
        return out
    t -= NEW_CATCH_MS
    out["progress"] = _progress(3, total_f, total_f, total_b, total_b, None)
    if t < NEW_FINISH_MS:
        out["state"] = "finishing"
    elif t < NEW_FINISH_MS + NEW_SWITCH_MS:
        out["state"] = "switching"
    else:
        out["state"] = "done"
        at = clock0 + NEW_CONNECT_MS + NEW_COPY_MS + NEW_CATCH_MS + NEW_FINISH_MS + NEW_SWITCH_MS
        out["result"] = _result(job, at + (FLAKY_DROP_MS if sc == "flaky" else 0), forced=False)
    return out


def _result(job, at, forced):
    host = (job.get("host") or "").lower()
    same = host in ("127.0.0.1", "localhost", "::1")
    return {"at_ms": at, "from": job.get("from"), "file_name": job.get("file_name"), "forced": forced,
            "same_machine": same, "accounts": 3, "cars": MOVE_CARS,
            "cert_sha256": DEV_CERT_SHA256}


def _restore_view(job, now):
    out = _base(job)
    size = job["size"]
    rec = job["received"]
    f_files = rec / size if size else 0
    if job.get("failed"):
        out["state"] = "failed"
        out["problem"] = job["failed"]
        out["progress"] = _progress(1, EXPORT_FILES * f_files, EXPORT_FILES, rec, size, None)
        return out
    done_up = job.get("uploaded_ms")
    if done_up is None:
        out["state"] = "uploading"
        out["progress"] = _progress(1, EXPORT_FILES * f_files, EXPORT_FILES, rec, size, None)
        return out
    t = now - done_up
    out["progress"] = _progress(1, EXPORT_FILES, EXPORT_FILES, size, size, None)
    if t < RESTORE_CHECK_MS:
        out["state"] = "checking"
    elif t < RESTORE_CHECK_MS + RESTORE_SWITCH_MS:
        out["state"] = "switching"
    else:
        out["state"] = "done"
        out["result"] = _result(job, done_up + RESTORE_CHECK_MS + RESTORE_SWITCH_MS, forced=False)
    return out


DEV_CERT_SHA256 = ":".join(hashlib.sha256(b"sentry-mate dev certificate").hexdigest().upper()[i:i + 2]
                           for i in range(0, 64, 2))


def advance(st, now):
    """A move or restore that finished brings the old server's accounts: the server is set up."""
    job = st.setup.get("job") if hasattr(st, "setup") else None
    if job is None or not st.needs_setup:
        return
    if job_view(job, now)["state"] == "done":
        with st.lock:
            if st.needs_setup:
                st.seed_accounts(now)
                st.needs_setup = False


def _claim_ok(req):
    st = req.store
    c = st.setup["claim"]
    tok = getattr(req, "setup_token", None)
    return bool(c and tok and secrets.compare_digest(c["token"].encode(), tok.encode()) and req.now < c["expires_ms"])


def _need_claim(req, allow_done=False):
    """The claim this browser holds (the set-up cookie), renewed for another hour; 409 setup_done once set up."""
    from ..server import set_setup_cookie
    st = req.store
    if not st.needs_setup and not (allow_done and _claim_ok(req)):
        raise conflict("setup_done", "This server is set up. Sign in instead.")
    if not _claim_ok(req):
        raise ApiError(401, "not_claimed", "Type the set-up code from the server's log first.")
    with st.lock:
        st.setup["claim"]["expires_ms"] = req.now + CLAIM_MS
    req.set_cookie = set_setup_cookie(st.setup["claim"]["token"], CLAIM_MS // 1000)


def _busy(st, now):
    j = st.setup["job"]
    if j is None:
        return None
    v = job_view(j, now)
    return None if v["state"] == "failed" else v


def get_setup(req):
    st = req.store
    advance(st, req.now)
    claimed = _claim_ok(req)
    busy = _busy(st, req.now)
    if not st.needs_setup:
        state = "set_up"
    elif busy is not None:
        state = "moving_here" if busy["kind"] == "move_here" else "restoring"
    else:
        state = "fresh"
    job = None
    if claimed and st.setup["job"] is not None:
        job = job_view(st.setup["job"], req.now)
    return {"needs_setup": st.needs_setup, "state": state, "claimed": claimed,
            "claim_expires_ms": st.setup["claim"]["expires_ms"] if claimed else None, "job": job}


def _check_setup_code(req, code, one_call=False):
    """`one_call`: POST /setup, whose wrong codes the accounts throttle counts (5 from one address), apart from
    /setup/claim's (10)."""
    from .auth import THROTTLE_SETUP_TRIES, THROTTLE_TRIES, _check_throttle, _fail
    from ..store import DEV_SETUP_CODE
    st = req.store
    key = ("setup-one:" if one_call else "setup:") + (getattr(req, "client_ip", None) or "")
    _check_throttle(st, key, THROTTLE_TRIES if one_call else THROTTLE_SETUP_TRIES)
    if not secrets.compare_digest(_norm_setup_code(code).encode(), _norm_setup_code(DEV_SETUP_CODE).encode()):
        _fail(st, key)
        raise ApiError(403, "wrong_setup_code", "That set-up code is not right. Copy it again from the server's log.")


def post_claim(req):
    from ..server import set_setup_cookie
    st = req.store
    body = req.json()
    from .auth import _types
    _types(body, {"setup_code": str})
    _only(body, ("setup_code",))
    if not st.needs_setup:
        raise conflict("setup_done", "This server is set up. Sign in instead.")
    _check_setup_code(req, body["setup_code"])
    token = "sms_" + secrets.token_urlsafe(24)
    with st.lock:
        st.setup["claim"] = {"token": token, "expires_ms": req.now + CLAIM_MS}  # an earlier claim ends
    req.set_cookie = set_setup_cookie(token, CLAIM_MS // 1000)
    req.setup_token = token
    return get_setup(req)


def _password_rules(req, name, password):
    from .auth import _name_password_rules
    errs = _name_password_rules(req.store, name, password)
    if errs:
        raise invalid(errs)


def _first_admin(req, name, password, end_claim=True):
    """`end_claim`: POST /setup/fresh ends the claim; POST /setup leaves it (it may be another browser's)."""
    from .auth import _signed_in, me_view
    st = req.store
    _password_rules(req, name, password)
    with st.lock:
        a = {"id": "a1", "name": name.strip(), "role": "admin", "cars": "all",
             "two_step": False, "password": password, "created_ms": req.now, "version": 1}
        st.accounts[a["id"]] = a
        st.needs_setup = False
        if end_claim:
            st.setup["claim"] = None
        st.setup["job"] = None
        st.recovery_key_saved = False
        if not any(n["code"] == "recovery_key_unsaved" for n in st.notices):
            st.notices.append({"code": "recovery_key_unsaved", "level": "info",
                               "message": "Save the vault recovery key file somewhere safe.", "since_ms": req.now,
                               "fix_link": "/settings/server"})
    st.record(a, "setup", f"account:{a['id']}", f"Made the first admin account {a['name']}.")
    _signed_in(req, a)
    with st.lock:
        st.recovery_key = make_recovery_key()
    return {"status": "done", "me": me_view(a, st), "recovery_key": st.recovery_key}


def post_fresh(req):
    from ..server import clear_setup_cookie
    from .auth import _types
    st = req.store
    body = req.json()
    _types(body, {"name": str, "password": str})
    _only(body, ("name", "password", "time_zone"))
    _need_claim(req)
    if _busy(st, req.now) is not None:
        raise conflict("setup_busy", "A move or a restore into this server is under way. Cancel it first.")
    out = _first_admin(req, body["name"], body["password"])
    # the browser's zone: the one new cars start in (a fresh server has none saved yet); a wrong one is left out
    from .system import valid_zone
    z = body.get("time_zone")
    if isinstance(z, str) and valid_zone(z.strip()):
        with st.lock:
            st.server_settings["default_time_zone"] = z.strip()
            st.server_settings["version"] += 1
    req.set_cookie = [req.set_cookie, clear_setup_cookie()]
    out["_created"] = True
    return out


def post_setup_legacy(req):
    """POST /api/v1/setup { code, name, password }: the claim and "Start fresh" in one call."""
    from .auth import _types
    st = req.store
    body = req.json()
    _only(body, ("code", "name", "password"))
    _types(body, {"code": str, "name": str, "password": str})
    if not st.needs_setup:
        raise conflict("setup_done", "Set-up is already done. Sign in instead.")
    # the real server's order: the name and password, then the code, then a move or a restore that runs
    _password_rules(req, body["name"], body["password"])
    _check_setup_code(req, body["code"], one_call=True)
    if _busy(st, req.now) is not None:
        raise conflict("setup_busy", "A move or a restore into this server is under way. Cancel it first.")
    return _first_admin(req, body["name"], body["password"], end_claim=False)


def post_move_here(req):
    from .auth import _types
    st = req.store
    body = req.json()
    _types(body, {"from": str, "code": str})
    _only(body, ("from", "code"))
    _need_claim(req)
    host, port = parse_address(body["from"])
    bad = read_move_code(body["code"])
    if bad == "typo":
        raise bad_input({"code": "The move code has a typo: check it against the one the old server showed "
                                 "(SMMV- and six groups of five)."})
    if bad == "newer":
        raise invalid({"code": "This move code was made by a newer Sentry Mate: update this server first."})
    addr = _show_address(host, port)
    resumed = _resume_move(req, addr)
    if resumed is not None:
        return resumed
    if _busy(st, req.now) is not None:
        raise conflict("setup_busy", "A move or a restore into this server is under way. Cancel it first.")
    sc = _scenario(host)
    errors = {
        "unreachable": (502, "old_server_unreachable",
                        f"Nothing answers at {addr}. Check the address (the one the CAN Bridge sends to) and that "
                        "the old server is on. Nothing was started."),
        "wrong": (422, "move_code_wrong", "The old server didn't take this code. Check it, or make a new one there."),
        "expired": (422, "move_code_expired", "This move code expired. Make a new one on the old server."),
        "used": (409, "move_code_used", f"This move code was used already, by {PEER_ADDRESS}. If that wasn't this "
                                        "server, cancel the code on the old server now, then make a new one."),
        "newer": (422, "move_too_old", f"The old server runs a newer Sentry Mate than this one ({APP_VERSION}). "
                                       "Update this server first, then try again: the code still works."),
        "away": (422, "move_not_home_network", "The old server's move code works from its home network only. Make one "
                                               "there with \"from anywhere\" ticked."),
        "nomove": (422, "move_not_offered", f"No move code waits at {addr}. Make one there first (Settings, Server, "
                                            "Move to another server)."),
    }
    if sc in errors:
        status, code, message = errors[sc]
        raise ApiError(status, code, message)
    jid = st.new_id("job")
    job = {"id": jid, "kind": "move_here", "started_ms": req.now, "from": addr, "host": host, "scenario": sc,
           "forced_ms": None}
    with st.lock:
        st.setup["job"] = job
    st.record(None, "setup-move-started", None, f"Started moving the data here from {addr}.")
    out = job_view(job, req.now)
    out["_created"] = True
    return out


RESUMABLE = ("cancelled", "bad_session", "not_proven")


def _resume_move(req, addr):
    """A move that stopped (cancelled on the old server, or its session ended) carries on with a new move code from
    the same server: the same job, the copy so far kept. None when there is no such move."""
    st = req.store
    job = st.setup["job"]
    if job is None or job["kind"] != "move_here":
        return None
    v = job_view(job, req.now)
    if v["state"] != "stopped" or (v["problem"] or {}).get("code") not in RESUMABLE:
        return None
    if job["from"] != addr:
        raise conflict("setup_busy", f"The move from {job['from']} stopped here and carries on with a new code from "
                                     "that server. To move from another server, give that move up first.")
    done = (v["progress"] or {}).get("bytes_done") or 0
    with st.lock:  # the made-up copy carries on from where it stopped
        job["scenario"] = None
        job["clock0_ms"] = req.now - NEW_CONNECT_MS - -(-NEW_COPY_MS * done // MOVE_BYTES)
        job["resumed_ms"] = req.now
    st.record(None, "setup-move-resumed", None, f"Carried on moving the data here from {addr} with a new move code.")
    return job_view(job, req.now)


def post_move_finish(req):
    from .auth import _check_throttle, _fail
    st = req.store
    body = req.json()
    _only(body, ("recovery_key", "confirm"))
    if not isinstance(body.get("recovery_key"), str):
        raise bad_input({"recovery_key": "Required: the old server's recovery key."})
    if body.get("confirm") is not True:
        raise bad_input({"confirm": "Send confirm: true. The last changes on the old server may be missing, and the "
                                    "old server must never take uploads again."})
    _need_claim(req)
    job = st.setup["job"]
    if job is None or job["kind"] != "move_here":
        raise not_found("no_such_job", "No move into this server is under way.")
    v = job_view(job, req.now)
    if v["state"] not in ("waiting", "stopped") or not (v["progress"] or {}).get("bytes_done"):
        raise conflict("not_ready", "Finish with the recovery key only when the old server is gone for good and the "
                                    "copy has stopped part way.")
    if _recovery_symbols(body["recovery_key"]) is None:
        raise bad_input({"recovery_key": "This doesn't read as a recovery key: SMRK- and 13 groups of 4."})
    key = "recovery:" + (getattr(req, "client_ip", None) or "")
    _check_throttle(st, key, THROTTLE_KEY_TRIES)
    if not _same_key(body["recovery_key"], DEV_RECOVERY_KEY):
        _fail(st, key)
        raise ApiError(422, "wrong_recovery_key", "That recovery key doesn't open the old server's data. Use the one "
                                                  "the old server showed when it was set up.",
                       fields={"recovery_key": "Doesn't open this data."})
    with st.lock:
        job["forced_ms"] = req.now
    st.record(None, "setup-move-finished-forced", None, "Finished the move with the recovery key (old server gone).")
    return job_view(job, req.now)


def get_job(req):
    _need_claim(req, allow_done=True)
    advance(req.store, req.now)
    job = req.store.setup["job"]
    if job is None:
        raise not_found("no_such_job", "Nothing is under way. Choose how to set up this server.")
    return job_view(job, req.now)


def delete_job(req):
    st = req.store
    _need_claim(req)
    job = st.setup["job"]
    if job is None:
        raise not_found("no_such_job", "Nothing is under way.")
    v = job_view(job, req.now)
    if v["state"] in ("switching", "done"):
        raise conflict("finished", "It is switching to the data already; it finishes by itself.")
    with st.lock:
        st.setup["job"] = None
    if job["kind"] == "restore":
        st.record(None, "setup-restore-cancelled", None, f"Gave up restoring {job['file_name']}.")
    else:
        st.record(None, "setup-move-cancelled", None, f"Gave up moving the data here from {job['from']}.")
    return {"deleted": True, "id": job["id"]}


def post_restore(req):
    from .auth import _check_throttle, _fail
    st = req.store
    body = req.json()
    _only(body, ("name", "size", "recovery_key", "header"))
    bad = {}
    if not isinstance(body.get("name"), str) or not body["name"].strip():
        bad["name"] = "Required: the file's name."
    size = body.get("size")
    if not isinstance(size, int) or isinstance(size, bool):
        bad["size"] = "Required: the file's size in bytes."
    if not isinstance(body.get("recovery_key"), str):
        bad["recovery_key"] = "Required: the old server's recovery key."
    if not isinstance(body.get("header"), str):
        bad["header"] = "Required: the file's first 64 KiB (or all of it when smaller), base64."
    if bad:
        raise bad_input(bad)
    _need_claim(req)
    if not 1 <= size <= MAX_EXPORT:
        raise invalid({"size": "Between 1 byte and 64 TB."})
    try:
        header = base64.b64decode(body["header"], validate=True)
    except (binascii.Error, ValueError):
        raise bad_input({"header": "Not base64."})
    if len(header) != min(65536, size):
        raise bad_input({"header": f"Send the first {min(65536, size)} bytes of the file."})
    if _recovery_symbols(body["recovery_key"]) is None:
        raise bad_input({"recovery_key": "This doesn't read as a recovery key: SMRK- and 13 groups of 4. Check for a "
                                         "missing group."})
    name = body["name"].strip()[:200]
    busy = _busy(st, req.now)
    if busy is not None and busy["kind"] == "restore" and busy["state"] == "uploading" and \
            st.setup["job"]["file_name"] == name and st.setup["job"]["size"] == size:
        return job_view(st.setup["job"], req.now) | {"upload_url": "/api/v1/setup/restore",
                                                     "chunk_max_bytes": CHUNK_MAX}  # carries on: send from received
    if busy is not None:
        raise conflict("setup_busy", "A move or a restore into this server is under way. Cancel it first.")
    if not header.startswith(EXPORT_MAGIC):
        raise ApiError(422, "not_an_export", "This is not a Sentry Mate export file. Choose the .smexport file the "
                                             "old server made.", fields={"header": "Not an export file."})
    if "newer" in name.lower():
        raise ApiError(422, "export_too_new", "This export was made by a newer Sentry Mate. Update this server first.")
    key = "recovery:" + (getattr(req, "client_ip", None) or "")
    _check_throttle(st, key, THROTTLE_KEY_TRIES)
    if not _same_key(body["recovery_key"], DEV_RECOVERY_KEY):
        _fail(st, key)
        raise ApiError(422, "wrong_recovery_key", "That recovery key doesn't open this export. An export opens with "
                                                  "the recovery key the old server had when it was made.",
                       fields={"recovery_key": "Doesn't open this file."})
    jid = st.new_id("job")
    job = {"id": jid, "kind": "restore", "started_ms": req.now, "from": None, "file_name": name, "size": size,
           "received": 0, "uploaded_ms": None, "failed": None}
    with st.lock:
        st.setup["job"] = job
    st.record(None, "setup-restore-started", None, f"Started restoring {name} ({size} bytes).")
    out = job_view(job, req.now)
    out["upload_url"] = "/api/v1/setup/restore"
    out["chunk_max_bytes"] = CHUNK_MAX
    out["_created"] = True
    return out


def patch_restore(req):
    from .imports import _sha_header
    st = req.store
    _need_claim(req)
    job = st.setup["job"]
    if job is None or job["kind"] != "restore":
        raise not_found("no_such_job", "No restore is under way. Choose the file again.")
    v = job_view(job, req.now)
    if v["state"] != "uploading":
        raise conflict("not_uploading", f"This restore is {v['state']}; it takes no more bytes.")
    hd = req.headers
    off, ln = hd.get("Upload-Offset"), hd.get("Upload-Length")
    bad = {}
    if off is None or not off.isdigit():
        bad["Upload-Offset"] = "Required: where this chunk starts, in bytes."
    if ln is not None and not ln.isdigit():
        bad["Upload-Length"] = "The whole file size in bytes."
    sha = hd.get("Upload-SHA256")
    want = _sha_header(sha) if sha else None
    if sha and want is None:
        bad["Upload-SHA256"] = "The chunk's SHA-256, hex or base64 (optional)."
    if not req.body:
        bad["body"] = "Send the chunk's bytes."
    if bad:
        raise bad_input(bad)
    if ln is not None and int(ln) != job["size"]:
        raise invalid({"Upload-Length": f"This file is {job['size']} bytes."})
    if int(off) != job["received"]:
        raise ApiError(409, "wrong_offset", f"The server holds the file up to byte {job['received']}; send from there.",
                       headers={"Upload-Offset": str(job["received"])}, extra={"offset": job["received"]})
    if want is not None and hashlib.sha256(req.body).digest() != want:
        raise ApiError(422, "checksum_mismatch", "The chunk does not match its SHA-256. Send it again.",
                       fields={"Upload-SHA256": "Does not match the bytes sent."})
    if job["received"] + len(req.body) > job["size"]:
        raise invalid({"body": "More bytes than the file's size."})
    with st.lock:
        job["received"] += len(req.body)
        if "broken" in job["file_name"].lower() and job["received"] >= job["size"] * 0.6:
            job["failed"] = _job_problem("export_damaged", f"the export file is damaged or was changed at byte "
                                                           f"{job['received']}: copy or download it again, or make "
                                                           "the export again", None)
        elif job["received"] == job["size"]:
            job["uploaded_ms"] = req.now
    return job_view(job, req.now)


def register(router):
    router.add("/api/v1/setup", GET=get_setup, POST=post_setup_legacy, public=("GET", "POST"))
    router.add("/api/v1/setup/claim", POST=post_claim, public=("POST",))
    router.add("/api/v1/setup/fresh", POST=post_fresh, public=("POST",))
    router.add("/api/v1/setup/move-here", POST=post_move_here, public=("POST",))
    router.add("/api/v1/setup/move-here/finish", POST=post_move_finish, public=("POST",))
    router.add("/api/v1/setup/restore", POST=post_restore, PATCH=patch_restore, public=("POST", "PATCH"))
    router.add("/api/v1/setup/job", GET=get_job, DELETE=delete_job, public=("GET", "DELETE"))
    router.add("/api/v1/system/move", GET=get_move, POST=post_move, DELETE=delete_move,
               admin=("GET", "POST", "DELETE"))
    router.add("/api/v1/system/move/undo", POST=post_undo, admin=("POST",))
    router.add("/api/v1/system/exports", GET=list_exports, POST=post_export, admin=("GET", "POST"))
    router.add("/api/v1/system/exports/{export}", GET=get_export, DELETE=delete_export, admin=("GET", "DELETE"))
    router.add("/api/v1/system/exports/{export}/file", GET=export_file, admin=("GET",))
    router.add("/api/v1/system/recovery-key/show", POST=post_recovery_show, admin=("POST",))
    router.add("/api/v1/system/recovery-key/new", POST=post_recovery_new, admin=("POST",))
    router.add("/api/v1/system/recovery-key/saved", POST=post_recovery_saved, admin=("POST",))
