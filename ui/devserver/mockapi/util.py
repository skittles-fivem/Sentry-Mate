"""Small shared helpers: deterministic randomness, geometry and car time zones.

Everything here is pure and deterministic: the same inputs always give the same answer, so the
made-up dataset is the same on every run with the same seed.
"""

from __future__ import annotations

import datetime as _dt
import functools
import hashlib
import math
import random
import zoneinfo

MIN = 60_000
HOUR = 3_600_000
DAY = 86_400_000


# ------------------------------------------------------------------ randomness


def rng(*parts) -> random.Random:
    """A random generator seeded from any parts (strings, numbers); stable across runs."""
    return random.Random(":".join(str(p) for p in parts))


def h01(*parts) -> float:
    """A stable number in [0, 1) from any parts (cheap: one blake2b)."""
    d = hashlib.blake2b(":".join(str(p) for p in parts).encode(), digest_size=8).digest()
    return int.from_bytes(d, "big") / 2.0**64


def mix64(x: int) -> int:
    """splitmix64 finaliser: a fast stable hash of an integer (for the light synthetic cars)."""
    x = (x + 0x9E3779B97F4A7C15) & 0xFFFFFFFFFFFFFFFF
    x = ((x ^ (x >> 30)) * 0xBF58476D1CE4E5B9) & 0xFFFFFFFFFFFFFFFF
    x = ((x ^ (x >> 27)) * 0x94D049BB133111EB) & 0xFFFFFFFFFFFFFFFF
    return x ^ (x >> 31)


def u01(x: int, salt: int = 0) -> float:
    return mix64(x * 1_000_003 + salt) / 2.0**64


def r(v, d=1):
    """Round for the API; None stays None."""
    if v is None:
        return None
    return round(float(v), d)


def clamp(v, lo, hi):
    return lo if v < lo else hi if v > hi else v


# ------------------------------------------------------------------ geometry

EARTH_M = 6_371_000.0


def haversine_m(lat1, lon1, lat2, lon2) -> float:
    p = math.pi / 180
    a = (math.sin((lat2 - lat1) * p / 2) ** 2
         + math.cos(lat1 * p) * math.cos(lat2 * p) * math.sin((lon2 - lon1) * p / 2) ** 2)
    return 2 * EARTH_M * math.asin(min(1.0, math.sqrt(a)))


def bearing_deg(lat1, lon1, lat2, lon2) -> float:
    p = math.pi / 180
    y = math.sin((lon2 - lon1) * p) * math.cos(lat2 * p)
    x = (math.cos(lat1 * p) * math.sin(lat2 * p)
         - math.sin(lat1 * p) * math.cos(lat2 * p) * math.cos((lon2 - lon1) * p))
    return (math.degrees(math.atan2(y, x)) + 360) % 360


# ------------------------------------------------------------------ time zones


@functools.lru_cache(maxsize=64)
def zone(name: str) -> zoneinfo.ZoneInfo:
    return zoneinfo.ZoneInfo(name)


def valid_zone(name) -> bool:
    if not isinstance(name, str) or not name or len(name) > 64:
        return False
    try:
        zoneinfo.ZoneInfo(name)
        return True
    except Exception:
        return False


def local_dt(ms: int, tz: str) -> _dt.datetime:
    return _dt.datetime.fromtimestamp(ms / 1000, tz=zone(tz))


def local_ms(day: _dt.date, hour: float, tz: str) -> int:
    """Epoch ms of a local wall-clock time (fractional hours allowed) on a day in a zone."""
    h = int(hour)
    m = int(round((hour - h) * 60))
    if m == 60:
        h, m = h + 1, 0
    extra_days, h = divmod(h, 24)
    d = day + _dt.timedelta(days=extra_days)
    dt = _dt.datetime(d.year, d.month, d.day, h, m, tzinfo=zone(tz))
    return int(dt.timestamp() * 1000)


def local_date(ms: int, tz: str) -> _dt.date:
    return local_dt(ms, tz).date()


def date_str(ms: int, tz: str) -> str:
    return local_date(ms, tz).isoformat()


def local_hour(ms: int, tz: str) -> float:
    d = local_dt(ms, tz)
    return d.hour + d.minute / 60 + d.second / 3600


def day_start_ms(day: _dt.date, tz: str) -> int:
    return local_ms(day, 0, tz)


def iso_utc(ms: int) -> str:
    return _dt.datetime.fromtimestamp(ms / 1000, tz=_dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def parse_date(s) -> _dt.date | None:
    if not isinstance(s, str):
        return None
    try:
        return _dt.date.fromisoformat(s)
    except ValueError:
        return None


def median(xs):
    xs = sorted(x for x in xs if x is not None)
    if not xs:
        return None
    n = len(xs)
    return xs[n // 2] if n % 2 else (xs[n // 2 - 1] + xs[n // 2]) / 2


def percentile(xs, p):
    xs = sorted(x for x in xs if x is not None)
    if not xs:
        return None
    k = (len(xs) - 1) * p
    f = math.floor(k)
    c = min(len(xs) - 1, f + 1)
    return xs[f] + (xs[c] - xs[f]) * (k - f)
