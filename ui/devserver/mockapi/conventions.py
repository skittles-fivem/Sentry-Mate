"""What every endpoint shares: the request object, query parsing with field errors, cursor paging,
period and car filters, and CSV output.

Use these from a resource module instead of re-implementing them, so every endpoint answers the
same way (docs/ui-api.md, "Conventions" and "Decisions").
"""

from __future__ import annotations

import base64
import bisect
import csv
import hashlib
import io
import json
import math

from .errors import ApiError, bad_input, invalid, no_such_car

MAX_LIMIT = 500
DEFAULT_LIMIT = 50
MAX_MS = 4_102_444_800_000  # 2100-01-01


class Response:
    def __init__(self, status=200, body=b"", content_type="application/json; charset=utf-8", headers=None):
        self.status = status
        self.body = body if isinstance(body, bytes) else body.encode("utf-8")
        self.content_type = content_type
        self.headers = headers or {}


def json_bytes(obj) -> bytes:
    return json.dumps(obj, separators=(",", ":"), ensure_ascii=False, allow_nan=False).encode("utf-8")


class Req:
    """One API request. `app` holds the dataset, the store, the clock and the event hub."""

    def __init__(self, app, method, path, query, headers, body, params, session):
        self.app = app
        self.method = method
        self.path = path
        self.query = query  # {name: [values]}
        self.headers = headers
        self.body = body
        self.params = params  # path parameters
        self.session = session
        self.account = session["account"] if session else None
        self.now = app.clock.now()
        self.set_cookie = None
        fmt = self.q("format")
        accept = headers.get("Accept") or ""
        self.csv = fmt == "csv" or (fmt is None and "text/csv" in accept and "json" not in accept)
        self._json = None

    # --- shortcuts
    @property
    def ds(self):
        return self.app.ds

    @property
    def store(self):
        return self.app.store

    @property
    def car(self):
        return self.params.get("car")

    @property
    def is_admin(self):
        return bool(self.account) and self.account["role"] == "admin"

    def history(self):
        """The featured car's history, or None for a light synthetic car."""
        return self.ds.history(self.car)

    def tz(self, car=None):
        return self.store.car_meta(car or self.car)["time_zone"]

    # --- query values
    def q(self, name, default=None):
        v = self.query.get(name)
        if not v:
            return default
        return v[-1]

    def int_param(self, name, default=None, lo=None, hi=None):
        v = self.q(name)
        if v is None or v == "":
            return default
        try:
            n = int(v)
        except ValueError:
            raise bad_input({name: "Must be a whole number."})
        if lo is not None and n < lo or hi is not None and n > hi:
            raise bad_input({name: f"Must be between {lo} and {hi}."})
        return n

    def float_param(self, name, default=None, lo=None, hi=None):
        v = self.q(name)
        if v is None or v == "":
            return default
        try:
            n = float(v)
        except ValueError:
            raise bad_input({name: "Must be a number."})
        if not math.isfinite(n) or (lo is not None and n < lo) or (hi is not None and n > hi):
            raise bad_input({name: f"Must be a number between {lo} and {hi}."})
        return n

    def bool_param(self, name, default=None):
        v = self.q(name)
        if v is None or v == "":
            return default
        if v in ("true", "1"):
            return True
        if v in ("false", "0"):
            return False
        raise bad_input({name: "Must be true or false."})

    def enum(self, name, allowed, default=None):
        v = self.q(name)
        if v is None or v == "":
            return default
        if v not in allowed:
            raise bad_input({name: "Must be one of: " + ", ".join(allowed) + "."})
        return v

    def list_param(self, name, allowed=None, max_items=500):
        v = self.q(name)
        if v is None or v == "":
            return None
        items = [x.strip() for x in v.split(",") if x.strip()]
        if len(items) > max_items:
            raise bad_input({name: f"At most {max_items} values."})
        if allowed is not None:
            badv = [x for x in items if x not in allowed]
            if badv:
                raise bad_input({name: "Unknown: " + ", ".join(badv[:5]) + ". Allowed: " + ", ".join(allowed) + "."})
        return items

    def period(self, window_ms=None):
        """(from_ms, to_ms): the period filter. Missing edges mean "from the start" and "until now".
        With `window_ms`, a missing from_ms means that long before to_ms (or before now)."""
        a = self.int_param("from_ms", None, 0, MAX_MS)
        b = self.int_param("to_ms", None, 0, MAX_MS)
        if a is not None and b is not None and a >= b:
            raise bad_input({"to_ms": "Must be after from_ms."})
        b = self.now + 1 if b is None else b
        if a is None:
            a = max(0, min(b, self.now) - window_ms) if window_ms else 0
        return (a, b)

    def limit(self):
        return self.int_param("limit", DEFAULT_LIMIT, 1, MAX_LIMIT)

    def cars(self):
        """Car ids for a report resource: cars=all (or absent) for every car the account sees, else a list."""
        v = self.q("cars")
        st = self.store
        if v is None or v == "" or v == "all":
            return [c for c in self.ds.car_ids_featured() if st.sees(self.account, c)]
        ids = self.list_param("cars")
        for c in ids:
            if not self.ds.exists(c) or not st.sees(self.account, c):
                raise no_such_car()
        return [c for c in dict.fromkeys(ids)]

    # --- the JSON body
    def json(self, required=True):
        if self._json is None:
            if not self.body:
                if required:
                    raise bad_input({"body": "Send a JSON object."})
                self._json = {}
            else:
                try:
                    self._json = json.loads(self.body.decode("utf-8"))
                except (UnicodeDecodeError, json.JSONDecodeError):
                    raise bad_input({"body": "Not valid JSON."})
                if not isinstance(self._json, dict):
                    raise bad_input({"body": "Send a JSON object."})
        return self._json

    def version_check(self, current):
        """Changes carry the `version` they were based on; a stale one is a 409."""
        from .errors import stale_version
        body = self.json()
        v = body.get("version")
        if not isinstance(v, int) or isinstance(v, bool):
            raise bad_input({"version": "Send the version you loaded."})
        if v != current:
            raise stale_version(current)

    def fingerprint(self):
        """A short hash of the filters, so a cursor is only used with the filters it came from."""
        parts = []
        for k in sorted(self.query):
            if k in ("after", "limit", "format"):
                continue
            parts.append(k + "=" + ",".join(self.query[k]))
        parts.append(self.path)
        return hashlib.blake2b("&".join(parts).encode(), digest_size=4).hexdigest()


# ------------------------------------------------------------------ paging


def nk(v, reverse=False):
    """A sort key part that puts null last in either direction."""
    if v is None:
        return (0, 0) if reverse else (1, 0)
    return (1, v) if reverse else (0, v)


def _tuplify(x):
    if isinstance(x, list):
        return tuple(_tuplify(i) for i in x)
    return x


def encode_cursor(key, fp):
    raw = json.dumps({"k": key, "f": fp}, separators=(",", ":")).encode()
    return base64.urlsafe_b64encode(raw).decode().rstrip("=")


def decode_cursor(s, fp):
    try:
        raw = base64.urlsafe_b64decode(s + "=" * (-len(s) % 4))
        obj = json.loads(raw)
        key = _tuplify(obj["k"])
        f = obj["f"]
    except Exception:
        raise bad_input({"after": "Not a cursor this server gave out."})
    if f != fp:
        raise bad_input({"after": "This cursor belongs to other filters. Start again without it."})
    return key


def bad_cursor():
    return bad_input({"after": "Not a cursor this server gave out."})


def page(req, rows, key, reverse=False, limit=None):
    """One page of `rows` sorted by `key(row)` (a tuple). Returns (items, next).

    CSV answers carry every matching row (no paging), from `after` on when it is given.
    """
    fp = req.fingerprint()
    after = req.q("after")
    ck = decode_cursor(after, fp) if after else None
    keyed = sorted(((key(r), r) for r in rows), key=lambda x: x[0])
    keys = [k for k, _ in keyed]
    n = len(keyed)
    lim = n if req.csv else (limit or req.limit())
    try:
        if not reverse:
            i = bisect.bisect_right(keys, ck) if ck is not None else 0
            chunk = keyed[i:i + lim]
            more = i + lim < n
        else:
            j = bisect.bisect_left(keys, ck) if ck is not None else n
            chunk = keyed[max(0, j - lim):j][::-1]
            more = j - lim > 0
    except TypeError:  # a cursor whose key does not compare with this list's keys (made by hand)
        raise bad_cursor()
    nxt = encode_cursor(chunk[-1][0], fp) if more and chunk else None
    return [r for _, r in chunk], nxt


def check_limit_after(req):
    """Validate limit/after on endpoints that page by their own means."""
    return req.limit(), (decode_cursor(req.q("after"), req.fingerprint()) if req.q("after") else None)


# ------------------------------------------------------------------ CSV


def _flatten(obj, prefix="", out=None):
    if out is None:
        out = {}
    if not isinstance(obj, dict):
        out[prefix.rstrip(".") or "value"] = _cell(obj)
        return out
    for k, v in obj.items():
        if k.startswith("_"):
            continue
        if isinstance(v, dict):
            _flatten(v, f"{prefix}{k}.", out)
        else:
            out[f"{prefix}{k}"] = _cell(v)
    return out


def _cell(v):
    if v is None:
        return ""
    if isinstance(v, bool):
        return "true" if v else "false"
    if isinstance(v, (int, float, str)):
        return v
    if isinstance(v, list) and all(not isinstance(x, (dict, list)) for x in v):
        return ";".join("" if x is None else str(x) for x in v)
    return json.dumps(v, separators=(",", ":"), ensure_ascii=False)


def rows_csv(rows) -> str:
    flat = [_flatten(r) for r in rows]
    cols = []
    seen = set()
    for f in flat:
        for k in f:
            if k not in seen:
                seen.add(k)
                cols.append(k)
    buf = io.StringIO()
    w = csv.writer(buf, lineterminator="\r\n")
    w.writerow(cols)
    for f in flat:
        w.writerow([f.get(c, "") for c in cols])
    return buf.getvalue()


def columns_rows(obj):
    """Columns ({t_ms: [], values: {field: []}, ...}) as one row per time."""
    t = obj["t_ms"]
    vals = obj.get("values", {})
    extra = {k: v for k, v in obj.items() if isinstance(v, list) and k not in ("t_ms",) and len(v) == len(t)}
    out = []
    for i, tm in enumerate(t):
        row = {"t_ms": tm}
        for k, col in vals.items():
            row[k] = col[i]
        for k, col in extra.items():
            row[k] = col[i]
        out.append(row)
    return out


def to_csv(obj, name):
    """A JSON answer as CSV: its items, its rows, or its columns. None when it is not a list."""
    if isinstance(obj, dict):
        if isinstance(obj.get("items"), list):
            rows = obj["items"]
        elif isinstance(obj.get("rows"), list):
            rows = obj["rows"]
        elif isinstance(obj.get("t_ms"), list):
            rows = columns_rows(obj)
        else:
            return None
    elif isinstance(obj, list):
        rows = obj
    else:
        return None
    body = rows_csv(rows)
    return Response(200, "﻿" + body, "text/csv; charset=utf-8",
                    {"Content-Disposition": f'attachment; filename="{name}.csv"'})


def require_fields(body, spec):
    """Check a JSON body against {field: (type or tuple, required)}; 400 with fields on a wrong type."""
    errs = {}
    for k, (types, required) in spec.items():
        if k not in body:
            if required:
                errs[k] = "Required."
            continue
        v = body[k]
        if v is None and type(None) in (types if isinstance(types, tuple) else (types,)):
            continue
        tt = types if isinstance(types, tuple) else (types,)
        if isinstance(v, bool) and bool not in tt:
            errs[k] = "Wrong type."
        elif not isinstance(v, tt):
            errs[k] = "Wrong type."
    if errs:
        raise bad_input(errs)


__all__ = ["Req", "Response", "page", "nk", "to_csv", "rows_csv", "json_bytes", "require_fields", "invalid",
           "ApiError"]
