"""The contract: every resource and method in the tables of docs/ui-api.md is answered by the dev
server, with no 5xx, and every field the document names is in the answer.

The resources, methods and fields are read from the document itself (tests/apidoc.py). Ids come
from the API (a drive, a charge, an update of the car), so the calls are made-up but valid. Writes
use the recipes below; a documented write without a recipe fails the test, so the document and
the server cannot drift apart unnoticed. The reverse holds too: a route the server answers must be
in the document.
"""

from __future__ import annotations

import hashlib
import http.client
import json
import re

from . import apidoc
from .helpers import NOW, Running, ServerCase

DAY = 86_400_000
VIN_RX = re.compile(r"[A-HJ-NPR-Z0-9]{17}")


def _all_strings(obj, out):
    if isinstance(obj, dict):
        for k, v in obj.items():
            out.append(("key", k))
            _all_strings(v, out)
    elif isinstance(obj, list):
        for v in obj:
            _all_strings(v, out)
    elif isinstance(obj, str):
        out.append(("value", obj))


class Contract(ServerCase):
    @classmethod
    def setUpClass(cls):
        super().setUpClass()
        cls.rows, cls.types = apidoc.read()
        c = cls.c
        cls.ids = {
            "car": "c1",
            "drive": c.get("/api/v1/cars/c1/drives?limit=3")["items"][1]["id"],
            "charge": c.get("/api/v1/cars/c1/charges?limit=3")["items"][1]["id"],
            "update": c.get("/api/v1/cars/c1/updates")["items"][0]["id"],
            "place": "p1", "session": "s-older-1", "account": "a2", "token": "t12", "key": "cbk_5b2e9f13",
            "trip": "tr1", "rule": "r1", "import": "im1",
        }
        cls.bodies = []

    # ------------------------------------------------------------ helpers
    def fill(self, pattern, **over):
        ids = dict(self.ids, **over)
        return re.sub(r"{(\w+)}", lambda m: ids[m.group(1)], pattern)

    def documented(self):
        """{(method, pattern): row} for every documented call."""
        out = {}
        for row in self.rows:
            paths = row.paths
            for m in row.methods:
                if len(paths) == 2 and paths[1].startswith(paths[0] + "/"):  # `/things[/{thing}]`
                    coll, item = paths
                    if m == "GET":
                        out[(m, coll)] = row
                        out[(m, item)] = row
                    elif m == "POST":
                        out[(m, coll)] = row
                    else:
                        out[(m, item)] = row
                        if m == "DELETE" and "one or all" in row.notes:
                            out[(m, coll)] = row
                else:
                    for p in paths:
                        out[(m, p)] = row
        return out

    def call(self, method, path, body=None, headers=None, client=None):
        c = client or self.c
        st, hd, raw = c.request(method, path, body, headers=headers)
        data = None
        if raw and "json" in (hd.get("content-type") or ""):
            data = json.loads(raw)
        return st, hd, raw, data

    # ------------------------------------------------------------ the document and the router agree
    def test_document_and_router_agree(self):
        doc = self.documented()
        self.assertGreater(len(doc), 120, "the document parser found too few resources")
        router = self.server.app.router
        missing = []
        for (m, pattern) in sorted(doc):
            if pattern == "/health":
                continue
            route, _ = router.match(self.fill(pattern))
            if route is None or m not in route.handlers:
                missing.append(f"{m} {pattern}")
        self.assertEqual(missing, [], "documented but not served")
        known = {(m, p) for (m, p) in doc}
        extra = []
        for r in router.routes:
            for m in r.handlers:
                if (m, r.pattern) not in known:
                    extra.append(f"{m} {r.pattern}")
        self.assertEqual(extra, [], "served but not in docs/ui-api.md")

    # ------------------------------------------------------------ every GET answers with its fields
    GET_QUERY = {
        "/api/v1/cars/{car}/signals": "?series=soc_display,pack_power",
        "/api/v1/places/at": "?lat=37.7508&lon=-122.433",
        "/api/v1/cars/{car}/battery-level": "?from_ms=%d" % (NOW - 10 * DAY),
        "/api/v1/cars/{car}/drives/{drive}/track": "?extensions=speed,soc",
    }
    GET_IDS = {"/api/v1/cars/{car}/overlaps": {"car": "c3"}, "/api/v1/cars/{car}/health": {"car": "c3"}}

    def test_every_get(self):
        doc = self.documented()
        problems = []
        for (m, pattern), row in sorted(doc.items()):
            if m != "GET":
                continue
            path = self.fill(pattern, **self.GET_IDS.get(pattern, {})) + self.GET_QUERY.get(pattern, "")
            if pattern == "/api/v1/events":
                self.check_events(problems)
                continue
            st, hd, raw, data = self.call("GET", path)
            if pattern == "/health":
                if st != 200 or raw:
                    problems.append(f"GET /health: {st} {raw[:80]!r}")
                continue
            if st >= 500 or not 200 <= st < 300:
                problems.append(f"GET {path}: {st} {raw[:200]!r}")
                continue
            if data is None:
                if "geo+json" not in (hd.get("content-type") or ""):
                    problems.append(f"GET {path}: not JSON ({hd.get('content-type')})")
                continue
            self.bodies.append((path, data))
            miss = apidoc.missing_fields(row.shape, data, columns=row.columns)
            if miss:
                problems.append(f"GET {path}: lacks {', '.join(miss[:12])}")
        self.assertEqual(problems, [], "\n".join(problems))
        # nothing a page can read carries a VIN serial or a field named vin
        found = []
        for path, data in self.bodies:
            strings = []
            _all_strings(data, strings)
            for kind, s in strings:
                if kind == "key" and s.lower() == "vin":
                    found.append(f"{path}: a field named vin")
                if kind == "value" and len(s) == 17 and VIN_RX.fullmatch(s) and re.search(r"[A-Z]", s) \
                        and re.search(r"\d", s):
                    found.append(f"{path}: {s}")
        self.assertEqual(found, [])

    def check_events(self, problems):
        conn = http.client.HTTPConnection("127.0.0.1", self.server.port, timeout=10)
        conn.request("GET", "/api/v1/events", headers={"Cookie": self.c.cookie})
        r = conn.getresponse()
        head = r.read1(64) if hasattr(r, "read1") else r.fp.read(16)
        conn.close()
        if r.status != 200 or "text/event-stream" not in (r.getheader("Content-Type") or "") \
                or not head.startswith(b"retry:"):
            problems.append(f"GET /api/v1/events: {r.status} {r.getheader('Content-Type')} {head[:40]!r}")

    # ------------------------------------------------------------ every write, with a recipe
    def recipes(self):
        """(method, pattern) -> function returning (path, body, headers) for a valid call. In run order."""
        ctx = self.ctx = {}
        now = NOW
        c = self.c

        def new(key, value):
            ctx[key] = value
            return value

        def chunk_headers():
            return {"Upload-Offset": "0", "Upload-Length": "10",
                    "Upload-SHA256": hashlib.sha256(b"0123456789").hexdigest(),
                    "Content-Type": "application/offset+octet-stream"}

        return [
            ("PATCH", "/api/v1/me/preferences", lambda: (None, {"theme": "dark"}, None)),
            ("POST", "/api/v1/accounts", lambda: (None, {"name": "contract-watch", "password": "contract-pass-1",
                                                         "role": "watch", "cars": ["c1"]}, None)),
            ("PATCH", "/api/v1/accounts/{account}",
             lambda: (self.fill("/api/v1/accounts/{account}", account=ctx["account"]),
                      {"version": 1, "name": "contract-watch-2"}, None)),
            ("DELETE", "/api/v1/accounts/{account}",
             lambda: (self.fill("/api/v1/accounts/{account}", account=ctx["account"]), None, None)),
            ("PATCH", "/api/v1/cars/{car}", lambda: (None, {"version": c.get("/api/v1/cars/c1")["version"],
                                                            "name": "Comet"}, None)),
            ("PATCH", "/api/v1/cars/{car}/settings",
             lambda: (None, {"version": c.get("/api/v1/cars/c1/settings")["version"], "warranty_threshold_pct": 70},
                      None)),
            ("POST", "/api/v1/cars/{car}/upload-tokens", lambda: (None, None, None)),
            ("DELETE", "/api/v1/cars/{car}/upload-tokens/{token}",
             lambda: (self.fill("/api/v1/cars/{car}/upload-tokens/{token}", token=ctx["token"]), None, None)),
            ("POST", "/api/v1/upload-tokens", lambda: (None, None, None)),
            ("DELETE", "/api/v1/upload-tokens/{token}",
             lambda: (self.fill("/api/v1/upload-tokens/{token}", token=ctx["new_token"]), None, None)),
            ("PATCH", "/api/v1/can-keys/{key}", lambda: (None, {"preferred": False}, None)),
            ("PUT", "/api/v1/cars/{car}/drives/{drive}/purpose",
             lambda: (None, {"purpose": "business", "relation": "Contract test", "note": None}, None)),
            ("DELETE", "/api/v1/cars/{car}/drives/{drive}/purpose", lambda: (None, None, None)),
            ("POST", "/api/v1/cars/{car}/purpose-rules",
             lambda: (None, {"from_place": "p1", "to_place": "p11", "days": "sat,sun", "purpose": "private"}, None)),
            ("DELETE", "/api/v1/cars/{car}/purpose-rules/{rule}",
             lambda: (self.fill("/api/v1/cars/{car}/purpose-rules/{rule}", rule=ctx["rule"]), None, None)),
            ("POST", "/api/v1/trips", lambda: (None, {"car": "c1", "from_ms": now - 3 * DAY, "to_ms": now - DAY,
                                                      "name": "Contract trip"}, None)),
            ("PATCH", "/api/v1/trips/{trip}",
             lambda: (self.fill("/api/v1/trips/{trip}", trip=ctx["trip"]), {"name": "Contract trip 2"}, None)),
            ("DELETE", "/api/v1/trips/{trip}",
             lambda: (self.fill("/api/v1/trips/{trip}", trip=ctx["trip"]), None, None)),
            ("PUT", "/api/v1/cars/{car}/charges/{charge}/cost",
             lambda: (None, {"mode": "total", "amount": 5.5, "currency": "USD"}, None)),
            ("DELETE", "/api/v1/cars/{car}/charges/{charge}/cost", lambda: (None, None, None)),
            ("PATCH", "/api/v1/cars/{car}/updates/{update}", lambda: (None, {"version": "2026.2.6"}, None)),
            ("POST", "/api/v1/places/preview", lambda: (None, {"name": "Downtown LA", "lat": 34.0522, "lon": -118.2437,
                                                              "radius_m": 150}, None)),
            ("POST", "/api/v1/places", lambda: (None, {"name": "Contract corner", "kind": "other", "lat": 34.0522,
                                                       "lon": -118.2437, "radius_m": 150}, None)),
            ("PATCH", "/api/v1/places/{place}",
             lambda: (self.fill("/api/v1/places/{place}", place=ctx["place"]), {"version": 1, "name": "Corner 2"},
                      None)),
            ("POST", "/api/v1/places/{place}/reprice", lambda: (None, {"dry_run": True}, None)),
            ("DELETE", "/api/v1/places/{place}",
             lambda: (self.fill("/api/v1/places/{place}", place=ctx["place"]), None, None)),
            ("PUT", "/api/v1/settings/mqtt", lambda: (None, {k: v for k, v in c.get("/api/v1/settings/mqtt").items()
                                                             if k not in ("password", "password_set")}, None)),
            ("POST", "/api/v1/settings/mqtt/test", lambda: (None, None, None)),
            ("PATCH", "/api/v1/settings", lambda: (None, {"version": c.get("/api/v1/settings")["version"],
                                                          "range": "rated"}, None)),
            ("POST", "/api/v1/cars/{car}/rebuild", lambda: (None, {"from_ms": now - 2 * DAY}, None)),
            ("PATCH", "/api/v1/cars/{car}/overlaps",
             lambda: (self.fill("/api/v1/cars/{car}/overlaps", car="c3"),
                      {"id": c.get("/api/v1/cars/c3/overlaps")["items"][0]["id"], "shown": "can"}, None)),
            ("POST", "/api/v1/imports", lambda: (None, {"name": "contract.sql", "bytes_total": 10}, None)),
            ("PATCH", "/api/v1/imports/{import}",
             lambda: (self.fill("/api/v1/imports/{import}", **{"import": ctx["import"]}), b"0123456789",
                      chunk_headers())),
            ("PUT", "/api/v1/imports/{import}/cars",
             lambda: (self.fill("/api/v1/imports/{import}/cars", **{"import": ctx["import"]}),
                      {"items": [{"tm_car": 1, "match": "c3"}, {"tm_car": 2, "match": "leave_out"}]}, None)),
            ("POST", "/api/v1/imports/{import}/start",
             lambda: (self.advance(7000) or self.fill("/api/v1/imports/{import}/start", **{"import": ctx["import"]}),
                      None, None)),
            ("DELETE", "/api/v1/imports/{import}",
             lambda: (self.fill("/api/v1/imports/{import}", **{"import": ctx["import"]}), None, None)),
            ("DELETE", "/api/v1/me/sessions/{session}", lambda: (None, None, None)),
            # sign-in on a client of its own, so this test's session stays
            ("POST", "/api/v1/session", lambda: (None, {"name": "admin", "password": "sentry-mate-dev"}, "own")),
            ("DELETE", "/api/v1/session", lambda: (None, None, "own")),
            ("POST", "/api/v1/session/two-step", lambda: (None, self.two_step_body(), "own2")),
            ("DELETE", "/api/v1/me/sessions", lambda: (None, None, None)),
        ], new

    def advance(self, ms):
        self.server.app.clock.advance(ms)

    def two_step_body(self):
        c = self.clients["own2"]
        d = c.json("POST", "/api/v1/session", {"name": "alex", "password": "sentry-mate-dev"})
        return {"pending": d["pending"], "code": "000000"}

    def test_every_write(self):
        doc = self.documented()
        writes = {(m, p) for (m, p) in doc if m != "GET"}
        recipes, new = self.recipes()
        have = {(m, p) for m, p, _ in recipes}
        # POST /api/v1/setup works only before set-up: test_setup below covers it on a server of its own
        lacking = sorted(writes - have - {("POST", "/api/v1/setup")})
        self.assertEqual(lacking, [], "documented writes without a recipe in test_contract.py")
        self.clients = {"own": self.server.client(), "own2": self.server.client()}
        problems = []
        for m, pattern, fn in recipes:
            path, body, who = fn()
            headers = None
            client = None
            if isinstance(who, dict):
                headers = who
            elif isinstance(who, str):
                client = self.clients[who]
            path = path or self.fill(pattern)
            st, hd, raw, data = self.call(m, path, body, headers=headers, client=client)
            if not 200 <= st < 300:
                problems.append(f"{m} {path}: {st} {raw[:300]!r}")
                continue
            if data is None:
                problems.append(f"{m} {path}: answered without a JSON body")
                continue
            # remember what was made, for the calls that change or remove it
            if m == "POST" and st == 201:
                key = {"/api/v1/accounts": "account", "/api/v1/cars/{car}/upload-tokens": "token",
                       "/api/v1/upload-tokens": "new_token",
                       "/api/v1/cars/{car}/purpose-rules": "rule", "/api/v1/trips": "trip",
                       "/api/v1/places": "place", "/api/v1/imports": "import"}.get(pattern)
                if key:
                    new(key, data["id"])
            row = doc[(m, pattern)]
            want = None
            if m in row.writes:  # the document spells out this write's answer
                want = row.writes[m]
            elif m in ("PATCH", "PUT") and row.shape and "GET" in row.methods and pattern == row.paths[-1]:
                want = row.shape  # a change answers the changed object
            elif m == "POST" and st == 201 and row.shape and "GET" in row.methods and len(row.paths) == 2:
                want = row.shape  # creating answers the new object
            elif m == "POST" and st == 201 and pattern == "/api/v1/imports":
                want = doc[("GET", "/api/v1/imports/{import}")].shape
            if want is not None:
                miss = apidoc.missing_fields(want, data, columns=row.columns)
                if miss:
                    problems.append(f"{m} {path}: answer lacks {', '.join(miss[:8])}")
        self.assertEqual(problems, [], "\n".join(problems))
        # every change is in the audit log
        audit = self.c.get("/api/v1/audit?limit=500")["items"]
        actions = {a["action"] for a in audit}
        for want in ("account.create", "account.update", "account.delete", "token.create", "token.revoke",
                     "drive.purpose", "purpose_rule.create", "trip.create", "charge.cost", "update.version",
                     "place.create", "place.update", "place.delete", "mqtt.update", "settings.update",
                     "car.rebuild", "car.overlap", "import.create", "import.start", "import.discard"):
            self.assertIn(want, actions)

    # ------------------------------------------------------------ deliberately absent
    def test_deliberately_absent(self):
        for m, path in (("POST", "/api/v1/cars/c1/wake"), ("POST", "/api/v1/cars/c1/command"),
                        ("POST", "/api/v1/cars/c1/commands/honk"), ("POST", "/api/v1/cars/c1/logging/pause"),
                        ("POST", "/api/v1/cars/c1/charge/start"), ("GET", "/api/v1/tesla/vehicles"),
                        ("GET", "/api/v1/tesla/token"), ("POST", "/api/v1/tesla/sign-in"), ("GET", "/api/v1/vehicles"),
                        ("GET", "/api/v1/sql"), ("POST", "/api/v1/sql"), ("GET", "/api/v1/db/indexes"),
                        ("GET", "/api/v1/cars/c1/vin")):
            self.c.error(m, path, 404, "not_found")
        words = re.compile(r"/(wake|commands?|tesla|vehicles|sql|db|vin|tokens/tesla)(/|$)")
        for r in self.server.app.router.routes:
            self.assertIsNone(words.search(r.pattern), r.pattern)


class Setup(ServerCase):
    server_args = {"needs_setup": True}

    def test_setup(self):
        self.assertTrue(self.c.get("/api/v1/setup")["needs_setup"])
        d = self.c.json("POST", "/api/v1/setup", {"code": "SETUP-DEV-0000", "name": "owner",
                                                  "password": "a-long-password"})
        self.assertEqual(d["status"], "done")
        self.assertFalse(self.c.get("/api/v1/setup")["needs_setup"])


__all__ = ["Contract", "Setup", "Running"]
