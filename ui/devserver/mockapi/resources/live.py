"""Live values (docs/ui-api.md, "Live values"). /api/v1/events is streamed by server.py."""

from __future__ import annotations

from ..conventions import page
from ..live import now_object
from ..util import r


def get_now(req):
    return now_object(req.app, req.car, req.now)


def awake_reasons(req):
    a, b = req.period()
    h = req.history()
    rows = []
    if h is not None:
        spans = []
        # from a little earlier: a span that touches one before the period is that span, not a new one
        for s in h.segs_between(max(0, a - 2 * 86_400_000), b, req.now):
            for x, y, buses, reasons, kw in s.awake_spans():
                y = min(y, req.now)
                if y > x:
                    spans.append([x, y, set(buses), list(reasons), kw if kw is not None else 0.0])
        spans.sort(key=lambda z: z[0])
        merged = []
        for sp in spans:
            if merged and merged[-1][1] == sp[0]:
                m = merged[-1]
                dur_m, dur_s = m[1] - m[0], sp[1] - sp[0]
                m[4] = (m[4] * dur_m + sp[4] * dur_s) / max(1, dur_m + dur_s)
                m[1] = sp[1]
                m[2] |= sp[2]
                for rr in sp[3]:
                    if rr not in m[3]:
                        m[3].append(rr)
            else:
                merged.append(sp)
        for x, y, buses, reasons, kw in merged:
            if a <= x < b:
                rows.append({"start_ms": x, "end_ms": y, "buses": sorted(buses, key=("vehicle", "chassis").index),
                             "reasons": reasons, "avg_power_kw": r(kw, 3)})
    items, nxt = page(req, rows, key=lambda s: (s["start_ms"],), reverse=True)
    return {"car": req.car, "time_zone": req.tz(), "items": items, "next": nxt}


def register(router):
    router.add("/api/v1/cars/{car}/now", GET=get_now)
    router.add("/api/v1/cars/{car}/awake-reasons", GET=awake_reasons)
