"""Routes: path patterns like /api/v1/cars/{car}/drives/{drive}, each with handlers per method.

Register with `router.add(pattern, GET=fn, POST=fn, admin=("POST",), public=("GET",))`:

- `admin`: methods only an admin account may use (others get 403 not_allowed).
- `public`: methods that need no sign-in (setup, sign-in).
- a `{car}` in the pattern makes the route car-scoped: the car is checked against the account's
  car list before the handler runs (unknown and hidden cars both give 404 no_such_car).

A handler takes the request (conventions.Req) and returns a dict (sent as JSON), a
conventions.Response, or raises errors.ApiError.
"""

from __future__ import annotations

import re

METHODS = ("GET", "POST", "PUT", "PATCH", "DELETE")


class Route:
    def __init__(self, pattern, handlers, admin=(), public=()):
        self.pattern = pattern
        names = re.findall(r"{(\w+)}", pattern)
        rx = re.escape(pattern)
        for n in names:
            rx = rx.replace(re.escape("{" + n + "}"), f"(?P<{n}>[^/]+)")
        self.regex = re.compile("^" + rx + "$")
        self.names = names
        self.handlers = handlers
        self.admin = set(admin)
        self.public = set(public)
        self.static_len = len(pattern) - sum(len(n) + 2 for n in names)

    @property
    def car_scoped(self):
        return "car" in self.names


class Router:
    def __init__(self):
        self.routes = []

    def add(self, pattern, admin=(), public=(), **handlers):
        for m in handlers:
            if m not in METHODS:
                raise ValueError(m)
        for r in self.routes:
            if r.pattern == pattern:
                r.handlers.update(handlers)
                r.admin |= set(admin)
                r.public |= set(public)
                return r
        r = Route(pattern, dict(handlers), admin, public)
        self.routes.append(r)
        # the most specific pattern (most fixed characters) wins
        self.routes.sort(key=lambda x: -x.static_len)
        return r

    def match(self, path):
        for r in self.routes:
            m = r.regex.match(path)
            if m:
                return r, m.groupdict()
        return None, None
