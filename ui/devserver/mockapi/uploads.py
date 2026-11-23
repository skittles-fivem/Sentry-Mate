"""A made-up upload cadence: the CAN Bridges of the featured cars that are awake send a log every UPLOAD_EVERY_MS of
the virtual clock, so a car's health (its keys' `last_upload_ms`) and the server's free space move while pages are
open, and the live stream's `health` and `storage` events can be watched.

Everything is worked out from the virtual "now": with --freeze-clock nothing moves (stable screenshots), and the
values at the start are the seeded ones.
"""

from __future__ import annotations

import threading

UPLOAD_EVERY_MS = 30_000
LOG_BYTES = 34_400  # about one log a minute's worth, sealed (as resources/system.py counts them)
LIVE_WITHIN_MS = 10 * 60_000  # a key that uploaded this recently at the start is a live one
AWAKE_KINDS = ("drive", "charge", "update")

_lock = threading.Lock()


def advance(app, now):
    """Brings the live keys' (and their tokens') last upload up to `now`."""
    st = app.store
    base = getattr(st, "_upload_base", None)
    with _lock:
        if base is None:
            now0 = app.cfg.now_ms
            base = st._upload_base = {k["key"]: k["last_upload_ms"] for k in st.keys
                                      if k["car"] in app.ds.featured and k["preferred"] and k["last_upload_ms"]
                                      and now0 - k["last_upload_ms"] <= LIVE_WITHIN_MS}
            st._uploaded_logs = 0
        steps = (now - app.cfg.now_ms) // UPLOAD_EVERY_MS
        if steps <= 0:
            return
        for k in st.keys:
            b = base.get(k["key"])
            if b is None or k["car"] is None:
                continue
            h = app.ds.featured.get(k["car"])
            seg = h.seg_at(now) if h else None
            if seg is None or seg.kind not in AWAKE_KINDS:
                continue
            t = b + steps * UPLOAD_EVERY_MS
            if t > (k["last_upload_ms"] or 0):
                st._uploaded_logs += max(1, (t - (k["last_upload_ms"] or b)) // UPLOAD_EVERY_MS)
                k["last_upload_ms"] = t
                for tok in st.tokens.get(k["car"], []):
                    if tok.get("key") == k["key"]:
                        tok["last_upload_ms"] = t


def free_bytes(app, now):
    """The disk's free space, less what the made-up uploads since the start took."""
    advance(app, now)
    return app.store.storage["free_bytes"] - getattr(app.store, "_uploaded_logs", 0) * LOG_BYTES
