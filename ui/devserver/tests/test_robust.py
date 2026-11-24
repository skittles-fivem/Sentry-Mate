"""Odd input never breaks the server: every documented GET, called with bad, empty or extreme query
values, answers below 500, and every 4xx carries the one error shape. Also for a light synthetic car
(summary data only) and for an empty fleet."""

from __future__ import annotations

import json
import re

from . import apidoc
from .helpers import DAY, NOW, ServerCase

ODD = ["?format=csv", "?limit=0", "?limit=501", "?after=not-a-cursor", "?from_ms=abc",
       f"?from_ms={NOW + 30 * DAY}", f"?from_ms={NOW}&to_ms={NOW - DAY}",
       f"?from_ms={NOW - 400 * DAY}&to_ms={NOW - 399 * DAY}", "?cars=c1,c99&car=c99&places=p999",
       "?group=decade&period=decade&bucket=0&max_points=1&metric=x&sort=x&kind=x&types=x&kinds=x"]
LIGHT = ["", "?format=csv", f"?from_ms={NOW - 3 * DAY}", "?limit=1"]


class Robust(ServerCase):
    car = "c1"
    variants = ODD

    def paths(self):
        rows, _ = apidoc.read()
        c = self.c
        drives = c.get(f"/api/v1/cars/{self.car}/drives?limit=1")["items"] if self.ds_has_car() else []
        charges = c.get(f"/api/v1/cars/{self.car}/charges?limit=1")["items"] if self.ds_has_car() else []
        ids = {"car": self.car, "drive": drives[0]["id"] if drives else "9007199254740993",
               "charge": charges[0]["id"] if charges else "9007199254740993", "place": "p1",
               "session": "s-older-1", "account": "a2", "token": "t12", "key": "cbk_5b2e9f13", "trip": "tr1",
               "rule": "r1", "import": "im1", "update": "1", "export": "ex1", "tesla_account": "ta1",
               "sign_in": "si0", "teslafi_account": "fa1", "pack": "instrument", "z": "13", "x": "1310", "y": "3166",
               "invoice": "tesla-1000101"}
        out = set()
        for r in rows:
            if "GET" in r.methods:
                for p in r.paths:
                    if p not in ("/api/v1/events", "/health"):
                        out.add(re.sub(r"{(\w+)}", lambda m: ids[m.group(1)], p))
        return sorted(out)

    def ds_has_car(self):
        return self.server.app.ds.exists(self.car)

    def test_odd_queries(self):
        bad = []
        for path in self.paths():
            for v in self.variants:
                st, hd, raw = self.c.request("GET", path + v)
                if st >= 500:
                    bad.append(f"{st} {path}{v}: {raw[:200]!r}")
                elif st >= 400:
                    try:
                        e = json.loads(raw)["error"]
                        assert isinstance(e["code"], str) and isinstance(e["message"], str)
                    except (ValueError, KeyError, AssertionError):
                        bad.append(f"{st} {path}{v}: not the error shape: {raw[:120]!r}")
        self.assertEqual(bad, [], "\n".join(bad[:30]))


class RobustSynthetic(Robust):
    server_args = {"cars": 40}
    car = "c17"
    variants = LIGHT


class RobustEmptyFleet(Robust):
    server_args = {"cars": 0}
    variants = LIGHT
