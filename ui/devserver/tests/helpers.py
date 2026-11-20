"""Start the dev server in-process on a free port and talk to it like a browser (cookies, CSRF)."""

from __future__ import annotations

import gc
import http.client
import json
import os
import sys
import threading
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
if ROOT not in sys.path:
    sys.path.insert(0, ROOT)

from mockapi.config import Config  # noqa: E402
from mockapi.server import App  # noqa: E402

PACK = os.path.normpath(os.path.join(ROOT, "..", "packs", "instrument"))
NOW = 1_794_850_200_000  # 2026-11-16T17:30:00Z, the default "now"
DAY = 86_400_000


class Running:
    """A dev server running in a thread."""

    def __init__(self, **kw):
        kw.setdefault("freeze_clock", True)
        kw.setdefault("sse_interval_ms", 300)
        self.cfg = Config(pack=PACK, port=0, **kw)
        self.app = App(self.cfg)
        self.srv = self.app.make_server()
        self.port = self.srv.server_address[1]
        self.thread = threading.Thread(target=self.srv.serve_forever, kwargs={"poll_interval": 0.1}, daemon=True)
        self.thread.start()

    def client(self, **kw):
        return Client(self.port, **kw)

    def close(self):
        self.app.events.shutdown()
        self.srv.shutdown()
        self.srv.server_close()


class Client:
    def __init__(self, port, csrf=True):
        self.port = port
        self.cookie = None
        self.csrf = csrf

    def request(self, method, path, body=None, headers=None, csrf=None):
        c = http.client.HTTPConnection("127.0.0.1", self.port, timeout=30)
        h = dict(headers or {})
        if self.cookie:
            h["Cookie"] = self.cookie
        if (self.csrf if csrf is None else csrf) and method != "GET":
            h["X-Sentry-Mate"] = "1"
        data = None
        if body is not None:
            data = body if isinstance(body, bytes) else json.dumps(body).encode()
            h["Content-Type"] = "application/json"
        c.request(method, path, body=data, headers=h)
        r = c.getresponse()
        raw = r.read()
        sc = r.getheader("Set-Cookie")
        if sc:
            self.cookie = sc.split(";", 1)[0]
        c.close()
        return r.status, {k.lower(): v for k, v in r.getheaders()}, raw

    def json(self, method, path, body=None, expect=200, **kw):
        st, hd, raw = self.request(method, path, body, **kw)
        if expect is not None and st != expect:
            raise AssertionError(f"{method} {path}: {st} {raw[:400]!r}")
        return json.loads(raw) if raw else None

    def get(self, path, expect=200, **kw):
        return self.json("GET", path, expect=expect, **kw)

    def error(self, method, path, status, code, body=None, **kw):
        st, hd, raw = self.request(method, path, body, **kw)
        if st != status:
            raise AssertionError(f"{method} {path}: wanted {status}, got {st} {raw[:300]!r}")
        e = json.loads(raw)["error"]
        if e["code"] != code:
            raise AssertionError(f"{method} {path}: wanted code {code}, got {e}")
        return e, hd


class ServerCase(unittest.TestCase):
    """One server per test class (building the dataset takes about a second)."""

    server_args = {}

    @classmethod
    def setUpClass(cls):
        cls.server = Running(**cls.server_args)
        cls.c = cls.server.client()

    @classmethod
    def tearDownClass(cls):
        cls.server.close()
        # drop this class's dataset and caches, so later classes do not pay for them in garbage collection
        cls.server = cls.c = None
        gc.collect()

    def walk(self, path, limit=50, max_pages=1000):
        """Every item of a paged list, following `next`."""
        sep = "&" if "?" in path else "?"
        items, nxt, pages = [], None, 0
        while True:
            url = f"{path}{sep}limit={limit}" + (f"&after={nxt}" if nxt else "")
            d = self.c.get(url)
            items += d["items"]
            nxt = d["next"]
            pages += 1
            if not nxt or pages >= max_pages:
                return items

    def assertWrapped(self, v, msg=""):
        """A live value: null, or { value, at_ms, sources } with the one input that sent it."""
        if v is None:
            return
        self.assertIsInstance(v, dict, msg)
        self.assertEqual(set(v), {"value", "at_ms", "sources"}, msg)
        self.assertIsInstance(v["at_ms"], int, msg)
        self.assertEqual(len(v["sources"]), 1, msg)
        self.assertIn(v["sources"][0]["kind"], ("smo", "tesla_api"), msg)
        self.assertTrue(v["sources"][0]["label"].startswith("Data from"), msg)
