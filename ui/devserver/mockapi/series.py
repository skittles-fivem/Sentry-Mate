"""Time series from a car's history, generated on demand and bucketed.

Each piece is sampled at its own rate (1 Hz while driving or charging, every 30 s while parked
and awake, nothing while asleep), never finer than a quarter bucket, plus its first and last
moment and its peak moments, so minima and maxima survive any bucket size. Buckets with no
sample hold null; a parallel `state` column says why (asleep or unknown).
"""

from __future__ import annotations

import math

from .dataset.segments import SERIES_FIELDS
from .util import percentile

NICE = [1000, 2000, 5000, 10_000, 15_000, 30_000, 60_000, 120_000, 300_000, 600_000, 900_000, 1_800_000,
        3_600_000, 7_200_000, 10_800_000, 21_600_000, 43_200_000, 86_400_000, 172_800_000, 604_800_000]
AGGS = ("last", "min", "max", "mean", "p15", "p85")
LAST_BY_DEFAULT = {"lat", "lon", "accuracy_m", "odometer_km", "heading_deg"}
STATE_OF = {"driving": "driving", "charging": "charging", "online": "parked", "online_vehicle_bus_only": "parked",
            "updating": "parked", "asleep": "asleep", "unknown": "unknown"}
STATE_RANK = ("driving", "charging", "parked", "asleep", "unknown")


def nice_bucket(span_ms, max_points):
    need = span_ms / max(1, max_points)
    for b in NICE:
        if b >= need:
            return b
    return int(math.ceil(need / 86_400_000)) * 86_400_000


def pieces(h, a, b, now, source):
    out = []
    for s in h.segs_between(a, b, now):
        if source in ("can", "teslamate") and s.source != source:
            if source == "teslamate":
                continue
            continue
        out.append(s)
    if source == "teslamate":
        out += [s for s in h.shadow if s.end > a and s.start < min(b, now)]
    return out


def build(h, fields, a, b, max_points, aggs, now, source="all", value_fn=None, explicit_aggs=True, bucket=None,
          align=None):
    """Columns { t_ms, values, bucket_ms, coverage_pct, state } for [a, b).

    `bucket` fixes the bucket size (else the smallest nice one for max_points); `align` is a moment
    buckets start from (for example a local midnight), else buckets are aligned to UTC."""
    b = min(b, now)
    if b <= a:
        return {"t_ms": [], "values": {f: [] for f in fields}, "bucket_ms": bucket or 1000, "coverage_pct": [],
                "state": []}
    bucket = bucket or nice_bucket(b - a, max_points)
    a0 = a - a % bucket if align is None else align + ((a - align) // bucket) * bucket
    n = int(math.ceil((b - a0) / bucket))
    samples = [[] for _ in range(n)]  # (t, values)
    state_ms = [dict() for _ in range(n)]
    for s in pieces(h, a0, b, now, source):
        lo, hi = max(s.start, a0), min(s.end, b, getattr(s, "data_end", s.end) if s.kind != "parked" else s.end)
        if hi <= lo:
            continue
        # state time per bucket
        for kind, x, y in s.activity():
            x, y = max(x, lo), min(y, hi)
            if y <= x:
                continue
            st = STATE_OF[kind]
            i = (x - a0) // bucket
            while x < y and i < n:
                e = min(y, a0 + (i + 1) * bucket)
                state_ms[i][st] = state_ms[i].get(st, 0) + (e - x)
                x = e
                i += 1
        # sample times
        times = set()
        t = lo
        while t < hi:
            native = s.native_step_ms(t)
            if native is None:  # asleep: jump to the end of this sleep
                if s.kind == "parked":
                    _, sub = s.sub_at(t)
                    t = max(t + 1000, sub[1])
                    continue
                break
            step = max(native, bucket // 4) if bucket > native else native
            times.add(t - t % 1000)
            t = (t // step + 1) * step
        times.add(lo - lo % 1000 + (1000 if lo % 1000 else 0))
        times.add(hi - 1000 - (hi - 1000) % 1000)
        for p in s.peak_times():
            if p is not None and lo <= p < hi:
                times.add(p)
        for t in sorted(times):
            if t < lo or t >= hi:
                continue
            v = value_fn(s, t) if value_fn else s.values(t)
            if v is None:
                continue
            i = (t - a0) // bucket
            if 0 <= i < n:
                samples[i].append((t, v))
    t_ms = [a0 + i * bucket for i in range(n)]
    want = list(aggs) if aggs else []
    default = not explicit_aggs
    values = {}
    for f in fields:
        first_agg = want[0] if want else ("last" if f in LAST_BY_DEFAULT else "mean")
        cols = {first_agg: []}
        extra = list(dict.fromkeys(want[1:] if want else ([] if bucket <= 1000 or f in LAST_BY_DEFAULT else ["min", "max"])))
        for ag in extra:
            cols[ag] = []
        for i in range(n):
            xs = [(t, v.get(f)) for t, v in samples[i] if v.get(f) is not None]
            for ag in cols:
                cols[ag].append(_agg(ag, xs))
        values[f] = cols[first_agg]
        if want and len(want) > 1:
            for ag in want:
                values[f"{f}.{ag}"] = cols[ag]
        elif not want:
            for ag in extra:
                values[f"{f}.{ag}"] = cols[ag]
    coverage = []
    states = []
    for i in range(n):
        span = min(bucket, b - t_ms[i])
        known = sum(v for k, v in state_ms[i].items() if k != "unknown")
        coverage.append(round(100 * known / span, 1) if span > 0 else 0.0)
        if state_ms[i]:
            best = max(state_ms[i].items(), key=lambda kv: (kv[1], -STATE_RANK.index(kv[0])))
            states.append(best[0])
        else:
            states.append("unknown")
    _ = default
    return {"t_ms": t_ms, "values": values, "bucket_ms": bucket, "coverage_pct": coverage, "state": states}


def _agg(ag, xs):
    if not xs:
        return None
    if isinstance(xs[0][1], str):
        return xs[-1][1] if ag == "last" else None
    vs = [v for _, v in xs]
    if ag == "last":
        v = xs[-1][1]
    elif ag == "min":
        v = min(vs)
    elif ag == "max":
        v = max(vs)
    elif ag == "mean":
        v = sum(vs) / len(vs)
    elif ag == "p15":
        v = percentile(vs, 0.15)
    else:
        v = percentile(vs, 0.85)
    return round(v, 4) if isinstance(v, float) else v


__all__ = ["build", "nice_bucket", "AGGS", "SERIES_FIELDS"]
