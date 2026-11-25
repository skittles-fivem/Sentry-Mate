"""The contract: every resource and method in the tables of docs/ui-api.md is answered by the dev
server, with no 5xx, and every field the document names is in the answer.

The resources, methods and fields are read from the document itself (tests/apidoc.py). Ids come
from the API (a drive, a charge, an update of the car), so the calls are made-up but valid. Writes
use the recipes below; a documented write without a recipe fails the test, so the document and
the server cannot drift apart unnoticed. The reverse holds too: a route the server answers must be
in the document.
"""

from __future__ import annotations

import base64
import hashlib
import http.client
import json
import re

from . import apidoc
from .helpers import NOW, Running, ServerCase

DAY = 86_400_000
# calls that work only on a fresh server (before set-up): SetupContract below covers them on a server of its own
SETUP_ONLY = {("POST", "/api/v1/setup"), ("POST", "/api/v1/setup/claim"), ("POST", "/api/v1/setup/fresh"),
              ("POST", "/api/v1/setup/move-here"), ("POST", "/api/v1/setup/move-here/finish"),
              ("POST", "/api/v1/setup/restore"), ("PATCH", "/api/v1/setup/restore"), ("GET", "/api/v1/setup/job"),
              ("DELETE", "/api/v1/setup/job")}
DEV_KEY = "SMRK-DEVK-EY00-0000-0000-0000-0000-0000-0000-0000-0000-0000-0000-0000"
VIN_RX = re.compile(r"[A-HJ-NPR-Z0-9]{17}")
# answers that are files, not JSON
PDF_OR_ZIP = {"/api/v1/cars/{car}/charges/{charge}/invoices/{invoice}/pdf": "application/pdf",
              "/api/v1/integrations/invoices/{invoice}/pdf": "application/pdf",
              "/api/v1/integrations/invoices/zip": "application/zip",
              "/api/v1/ui-packs/{pack}/download": "application/zip"}


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
            "trip": "tr1", "rule": "r1", "import": "im1", "hold": "h1", "index": "0", "tesla_account": "ta1", "sign_in": "si0",
            "teslafi_account": "fa1", "pack": "instrument", "z": "13", "x": "1310", "y": "3166",
            "export": c.json("POST", "/api/v1/system/exports", {"password": "sentry-mate-dev"}, expect=201)["id"],
        }
        # a Supercharger session of c1 matched to a charge, with its invoice (PDF) kept
        inv = next(i for i in c.get("/api/v1/integrations/invoices?matched=true&limit=200")["items"]
                   if i["car"] == "c1" and i["pdf"])
        cls.ids["invoice"] = inv["id"]
        cls.invoice_charge = inv["charge"]
        cls.server.app.store.exports[cls.ids["export"]]["made_ms"] -= 10_000  # prepared
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
            if m != "GET" or (m, pattern) in SETUP_ONLY:
                continue
            path = self.fill(pattern, **self.GET_IDS.get(pattern, {})) + self.GET_QUERY.get(pattern, "")
            if pattern == "/api/v1/events":
                self.check_events(problems)
                continue
            if pattern == "/api/v1/cars/{car}/charges/{charge}/invoices/{invoice}/pdf":
                path = self.fill(pattern, charge=self.invoice_charge)
            st, hd, raw, data = self.call("GET", path)
            if pattern in PDF_OR_ZIP:
                if st != 200 or hd.get("content-type") != PDF_OR_ZIP[pattern] or not raw.startswith(
                        b"%PDF-1.4" if "pdf" in PDF_OR_ZIP[pattern] else b"PK") \
                        or "attachment" not in (hd.get("content-disposition") or ""):
                    problems.append(f"GET {path}: {st} {hd.get('content-type')}")
                continue
            if pattern == "/api/v1/system/exports/{export}/file":
                if st != 200 or hd.get("content-type") != "application/octet-stream" or not raw:
                    problems.append(f"GET {path}: {st} {hd.get('content-type')}")
                continue
            if pattern == "/api/v1/maps/tiles/{z}/{x}/{y}":
                # online tiles here (the dev default): no offline map file is served (tests/test_maps.py has one)
                if st != 404 or (data or {}).get("error", {}).get("code") != "no_offline_map":
                    problems.append(f"GET {path}: {st} {raw[:120]!r}")
                continue
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

        def chunk_headers(data=b"0123456789", offset=0):
            return {"Upload-Offset": str(offset), "Upload-Length": "10",
                    "Upload-SHA256": hashlib.sha256(data).hexdigest(),
                    "Content-Type": "application/offset+octet-stream"}

        return [
            ("PATCH", "/api/v1/me/preferences", lambda: (None, {"theme": "dark"}, None)),
            ("POST", "/api/v1/accounts", lambda: (None, {"name": "contract-watch", "password": "contract-pass-1",
                                                         "role": "watch", "cars": ["c1"],
                                                         "confirm_password": "sentry-mate-dev"}, None)),
            ("PATCH", "/api/v1/accounts/{account}",
             lambda: (self.fill("/api/v1/accounts/{account}", account=ctx["account"]),
                      {"version": 1, "name": "contract-watch-2"}, None)),
            ("DELETE", "/api/v1/accounts/{account}",
             lambda: (self.fill("/api/v1/accounts/{account}", account=ctx["account"]),
                      {"confirm_password": "sentry-mate-dev"}, None)),
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
            ("POST", "/api/v1/release/check", lambda: (None, {}, None) if c.json("PATCH", "/api/v1/settings", {
                "version": c.get("/api/v1/settings")["version"], "release_check": {"enabled": True}}) else None),
            ("POST", "/api/v1/cars/{car}/rebuild", lambda: (None, {"from_ms": now - 2 * DAY}, None)),
            ("POST", "/api/v1/cars/{car}/logs/use-unsigned",
             lambda: (None, {"all": True, "confirm_password": "sentry-mate-dev"}, None)),
            ("DELETE", "/api/v1/cars/{car}/logs/use-unsigned",
             lambda: (None, {"all": True, "confirm_password": "sentry-mate-dev"}, None)),
            ("PUT", "/api/v1/cars/{car}/gps-layout",
             lambda: (None, {"layout": "lat 0|28@1- (1e-6,0); lon 28|28@1- (1e-6,16.777216)"}, None)),
            ("POST", "/api/v1/cars/{car}/gps-layout/fit", lambda: (None, {"days": 30}, None)),
            ("POST", "/api/v1/system/storage/compact", lambda: (None, None, None)),
            ("POST", "/api/v1/cars/{car}/full-rebuild", lambda: (None, {"confirm_password": "sentry-mate-dev"}, None)),
            ("PATCH", "/api/v1/system/full-rebuild", lambda: (None, {"when": "pause"}, None)),
            ("DELETE", "/api/v1/system/full-rebuild", lambda: (None, {}, None)),
            ("POST", "/api/v1/system/full-rebuild", lambda: (None, {"confirm_password": "sentry-mate-dev"}, None)),
            ("PATCH", "/api/v1/cars/{car}/overlaps",
             lambda: (self.fill("/api/v1/cars/{car}/overlaps", car="c3"),
                      {"id": c.get("/api/v1/cars/c3/overlaps")["items"][0]["id"], "shown": "can"}, None)),
            ("POST", "/api/v1/imports", lambda: (None, {"name": "contract.sql", "bytes_total": 10}, None)),
            ("PATCH", "/api/v1/imports/{import}/files/{index}",
             lambda: (self.fill("/api/v1/imports/{import}/files/{index}", **{"import": ctx["import"]}), b"01234",
                      chunk_headers(b"01234"))),
            ("PATCH", "/api/v1/imports/{import}",
             lambda: (self.fill("/api/v1/imports/{import}", **{"import": ctx["import"]}), b"56789",
                      chunk_headers(b"56789", 5))),
            ("PUT", "/api/v1/imports/{import}/cars",
             lambda: (self.fill("/api/v1/imports/{import}/cars", **{"import": ctx["import"]}),
                      {"items": [{"tm_car": 1, "match": "c3"}, {"tm_car": 2, "match": "leave_out"}]}, None)),
            ("POST", "/api/v1/imports/{import}/start",
             lambda: (self.advance(7000) or self.fill("/api/v1/imports/{import}/start", **{"import": ctx["import"]}),
                      None, None)),
            ("DELETE", "/api/v1/imports/{import}",
             lambda: (self.fill("/api/v1/imports/{import}", **{"import": ctx["import"]}), None, None)),
            ("POST", "/api/v1/import-holds/{hold}", lambda: (None, {"match": "c2"}, None)),
            ("PATCH", "/api/v1/integrations/tesla",
             lambda: (None, {"version": c.get("/api/v1/integrations/tesla")["version"], "poll": {"idle_ms": 300_000},
                             "confirm_password": "sentry-mate-dev"}, None)),
            ("POST", "/api/v1/integrations/tesla/sign-in", lambda: (None, {"region": "global"}, None)),
            ("POST", "/api/v1/integrations/tesla/sign-in/{sign_in}", lambda: self.tesla_finish()),
            ("POST", "/api/v1/integrations/tesla/accounts",
             lambda: (None, {"label": "Contract account", "refresh_token": "made-up-refresh-token",
                             "confirm_password": "sentry-mate-dev"}, None)),
            ("PATCH", "/api/v1/integrations/tesla/accounts/{tesla_account}",
             lambda: (None, {"version": c.get("/api/v1/integrations/tesla/accounts/ta1")["version"],
                             "label": "Family", "confirm_password": "sentry-mate-dev"}, None)),
            ("PUT", "/api/v1/integrations/tesla/accounts/{tesla_account}/cars",
             lambda: (None, {"version": c.get("/api/v1/integrations/tesla/accounts/ta1")["version"],
                             "items": [{"vehicle": "tv3", "match": "leave_out"}]}, None)),
            ("DELETE", "/api/v1/integrations/tesla/accounts/{tesla_account}",
             lambda: (self.fill("/api/v1/integrations/tesla/accounts/{tesla_account}",
                                tesla_account=ctx["tesla_account"]), {"confirm_password": "sentry-mate-dev"}, None)),
            ("PATCH", "/api/v1/integrations/teslafi",
             lambda: (None, {"version": c.get("/api/v1/integrations/teslafi")["version"], "poll": "gentle",
                             "confirm_password": "sentry-mate-dev"}, None)),
            ("POST", "/api/v1/integrations/teslafi/accounts",
             lambda: (None, {"label": "Contract TeslaFi", "token": "0000madeup0000", "time_zone": "Europe/Oslo",
                             "confirm_password": "sentry-mate-dev"}, None)),
            ("PATCH", "/api/v1/integrations/teslafi/accounts/{teslafi_account}",
             lambda: (None, {"version": c.get("/api/v1/integrations/teslafi/accounts/fa1")["version"],
                             "label": "TeslaFi (paid)", "confirm_password": "sentry-mate-dev"}, None)),
            ("PUT", "/api/v1/integrations/teslafi/accounts/{teslafi_account}/cars",
             lambda: (None, {"version": c.get("/api/v1/integrations/teslafi/accounts/fa1")["version"],
                             "items": [{"vehicle": "fv1", "match": "c3"}]}, None)),
            ("DELETE", "/api/v1/integrations/teslafi/accounts/{teslafi_account}",
             lambda: (self.fill("/api/v1/integrations/teslafi/accounts/{teslafi_account}",
                                teslafi_account=ctx["teslafi_account"]), {"confirm_password": "sentry-mate-dev"},
                      None)),
            ("PUT", "/api/v1/integrations/teslamate",
             lambda: (None, {"version": c.get("/api/v1/integrations/teslamate")["version"],
                             "mqtt": {"host": "192.0.2.11", "port": 1883, "tls": False, "prefix": "teslamate"},
                             "database": {"enabled": True, "host": "192.0.2.11", "port": 5432, "tls": False},
                             "confirm_password": "sentry-mate-dev"}, None)),
            ("POST", "/api/v1/integrations/teslamate/test",
             lambda: (None, {"target": "mqtt"}, None)),
            ("PUT", "/api/v1/integrations/teslamate/cars",
             lambda: (None, {"version": c.get("/api/v1/integrations/teslamate/cars")["version"],
                             "items": [{"teslamate_car": "1", "match": "c2"}]}, None)),
            ("PATCH", "/api/v1/integrations/teslamate",
             lambda: (None, {"version": c.get("/api/v1/integrations/teslamate")["version"], "paused": True,
                             "confirm_password": "sentry-mate-dev"}, None)),
            ("DELETE", "/api/v1/integrations/teslamate",
             lambda: (None, {"confirm_password": "sentry-mate-dev"}, None)),
            ("POST", "/api/v1/system/recovery-key/new", lambda: (None, {"password": "sentry-mate-dev"}, None)),
            ("POST", "/api/v1/system/recovery-key/show", lambda: (None, {"password": "sentry-mate-dev"}, None)),
            ("POST", "/api/v1/system/recovery-key/saved", lambda: (None, None, None)),
            ("POST", "/api/v1/system/exports", lambda: (None, {"password": "sentry-mate-dev"}, None)),
            ("POST", "/api/v1/network/certificate", lambda: (None, {"password": "sentry-mate-dev"}, None)),
            ("DELETE", "/api/v1/system/exports/{export}",
             lambda: (self.fill("/api/v1/system/exports/{export}", export=ctx["export"]), None, None)),
            # last of the changes: a moved server takes no other changes until the undo
            ("POST", "/api/v1/system/move", lambda: (None, {"password": "sentry-mate-dev"}, None)),
            ("POST", "/api/v1/system/move/undo",
             lambda: (self.advance(40_000) or None, {"password": "sentry-mate-dev", "confirm": True}, None)),
            ("DELETE", "/api/v1/system/move", lambda: (None, None, None)),
            ("DELETE", "/api/v1/me/sessions/{session}", lambda: (None, None, None)),
            # sign-in on a client of its own, so this test's session stays
            ("POST", "/api/v1/session", lambda: (None, {"name": "admin", "password": "sentry-mate-dev"}, "own")),
            ("DELETE", "/api/v1/session", lambda: (None, None, "own")),
            ("POST", "/api/v1/session/two-step", lambda: (None, self.two_step_body(), "own2")),
            ("POST", "/api/v1/me/two-step", lambda: (None, None, None)),
            ("POST", "/api/v1/me/two-step/confirm",
             lambda: (None, {"password": "sentry-mate-dev", "code": "123456"}, None)),
            ("POST", "/api/v1/me/two-step/recovery-codes",
             lambda: (None, {"password": "sentry-mate-dev", "code": "123456"}, None)),
            ("DELETE", "/api/v1/me/two-step", lambda: (None, {"password": "sentry-mate-dev", "code": "123456"}, None)),
            ("PUT", "/api/v1/two-step-policy",
             lambda: (None, {"outside": True, "confirm_password": "sentry-mate-dev"}, None)),
            ("PUT", "/api/v1/me/password",
             lambda: (None, {"password": "sentry-mate-dev", "new_password": "sentry-mate-dev"}, None)),
            ("DELETE", "/api/v1/me/sessions", lambda: (None, None, None)),
        ], new

    def tesla_finish(self):
        """A sign-in of its own, finished with the address Tesla's page would end on."""
        from urllib.parse import parse_qs, urlsplit
        s = self.c.json("POST", "/api/v1/integrations/tesla/sign-in", {}, expect=201)
        state = parse_qs(urlsplit(s["url"]).query)["state"][0]
        path = self.fill("/api/v1/integrations/tesla/sign-in/{sign_in}", sign_in=s["sign_in"])
        return path, {"pasted_url": f"tesla://auth/callback?code=made-up-code&state={state}",
                      "label": "Signed in", "confirm_password": "sentry-mate-dev"}, None

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
        # the set-up calls work only before set-up: SetupContract below covers them on a server of its own
        lacking = sorted(writes - have - SETUP_ONLY)
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
                       "/api/v1/places": "place", "/api/v1/imports": "import",
                       "/api/v1/system/exports": "export",
                       "/api/v1/integrations/tesla/accounts": "tesla_account",
                       "/api/v1/integrations/teslafi/accounts": "teslafi_account"}.get(pattern)
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
        for want in ("account-created", "account-removed", "token-created", "token-revoked",
                     "drive-purpose-set", "purpose-rule-created", "trip-saved", "charge-cost-entered", "update-version",
                     "place-created", "place-changed", "place-deleted", "mqtt-changed", "settings-changed",
                     "car-rebuild", "car-overlap", "import-created", "import-started", "import-discarded",
                     "export-made", "export-removed", "move-code-made", "move-undone", "recovery-key-new",
                     "recovery-key-shown", "recovery-key-saved",
                     "tesla-poll-changed", "tesla-account-added", "tesla-account-changed", "tesla-car-matched",
                     "tesla-account-removed"):
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
        words = re.compile(r"/(wake|wake_up|commands?|vehicles|sql|db|vin|tokens?)(/|$)")
        for r in self.server.app.router.routes:
            self.assertIsNone(words.search(r.pattern), r.pattern)
            # the Tesla API input lives under /integrations/tesla only (its accounts, the sign-in, status)
            if "tesla" in r.pattern:
                self.assertTrue(r.pattern.startswith("/api/v1/integrations/tesla"), r.pattern)


class Setup(ServerCase):
    server_args = {"needs_setup": True}

    def test_setup(self):
        self.assertTrue(self.c.get("/api/v1/setup")["needs_setup"])
        d = self.c.json("POST", "/api/v1/setup", {"code": "SETUP-DEV-0000", "name": "owner",
                                                  "password": "a-long-password"})
        self.assertEqual(d["status"], "done")
        self.assertFalse(self.c.get("/api/v1/setup")["needs_setup"])


class SetupContract(ServerCase):
    """The set-up calls on a fresh server: each answers, with the fields docs/ui-api.md names."""

    server_args = {"needs_setup": True}

    def test_setup_calls(self):
        self.rows, types = apidoc.read()
        doc = Contract.documented(self)  # the same reading of the tables
        job_shape = types["SetupJob"]
        c = self.server.client()
        problems = []

        def check(m, pattern, shape, data):
            miss = apidoc.missing_fields(shape, data)
            if miss:
                problems.append(f"{m} {pattern}: lacks {', '.join(miss[:10])}")

        d = c.get("/api/v1/setup")
        check("GET", "/api/v1/setup", doc[("GET", "/api/v1/setup")].shape, d)
        d = c.json("POST", "/api/v1/setup/claim", {"setup_code": "SETUP-DEV-0000"})
        check("GET", "/api/v1/setup", doc[("GET", "/api/v1/setup")].shape, d)
        from mockapi.resources.moving import EXPORT_MAGIC, make_move_code
        j = c.json("POST", "/api/v1/setup/move-here", {"from": "gone.example", "code": make_move_code()[0]},
                   expect=201)
        check("POST", "/api/v1/setup/move-here", job_shape, j)
        check("POST", "/api/v1/setup/move-here", doc[("POST", "/api/v1/setup/move-here")].writes.get("POST"), j)
        self.server.app.clock.advance(60_000)
        check("GET", "/api/v1/setup/job", job_shape, c.get("/api/v1/setup/job"))
        j = c.json("POST", "/api/v1/setup/move-here/finish", {"recovery_key": DEV_KEY, "confirm": True})
        check("POST", "/api/v1/setup/move-here/finish", job_shape, j)
        # give that one up before it switches? it switches at once: start again on a new server below
        fresh = Running(needs_setup=True)
        try:
            f = fresh.client()
            f.json("POST", "/api/v1/setup/claim", {"setup_code": "SETUP-DEV-0000"})
            data = EXPORT_MAGIC + b"x" * 5000
            j = f.json("POST", "/api/v1/setup/restore", {"name": "a.smexport", "size": len(data),
                                                         "recovery_key": DEV_KEY,
                                                         "header": base64.b64encode(data).decode()}, expect=201)
            check("POST", "/api/v1/setup/restore", doc[("POST", "/api/v1/setup/restore")].writes.get("POST"), j)
            check("POST", "/api/v1/setup/restore", job_shape, j)
            st, _hd, raw = f.request("PATCH", "/api/v1/setup/restore", data[:100],
                                     headers={"Upload-Offset": "0", "Upload-Length": str(len(data)),
                                              "Content-Type": "application/offset+octet-stream"})
            self.assertEqual(st, 200, raw)
            check("PATCH", "/api/v1/setup/restore", job_shape, json.loads(raw))
            self.assertTrue(f.json("DELETE", "/api/v1/setup/job")["deleted"])
            d = f.json("POST", "/api/v1/setup/fresh", {"name": "owner", "password": "a long password"}, expect=201)
            check("POST", "/api/v1/setup/fresh", doc[("POST", "/api/v1/setup/fresh")].writes.get("POST"), d)
        finally:
            fresh.close()
        self.assertEqual(problems, [], "\n".join(problems))


__all__ = ["Contract", "Setup", "SetupContract", "Running"]
