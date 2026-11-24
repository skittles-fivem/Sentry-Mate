"""The HTTP side: static pack files, the SPA fallback, security headers, the session cookie,
dispatch to /api/v1 routes, and the server-sent event stream.
"""

from __future__ import annotations

import hashlib
import http.server
import mimetypes
import os
import re
import socketserver
import sys
import threading
import time
import traceback
from http.cookies import SimpleCookie
from urllib.parse import unquote, unquote_to_bytes, urlsplit

from . import resources, uploads
from .config import Clock
from .conventions import Req, Response, json_bytes, to_csv
from .dataset import Dataset
from .errors import ApiError, bad_input, csrf, no_such_car, not_allowed, not_found, not_signed_in
from .events import EventHub, frame
from .packs import Packs
from .router import Router
from .store import COOKIE, COOKIE_SIGNED_OUT, SETUP_COOKIE, Store

def tile_origins(store):
    """As the real server's tile_origins(): the https origins of the chosen tile style and the styles viewers may
    switch to (overlays too), none while tiles are offline."""
    from .resources.system import tile_viewer_styles
    t = store.server_settings["tiles"]
    if t["mode"] != "online":
        return []
    out = []
    for s in tile_viewer_styles(t):
        for url in [s["template"], *s["overlays"]]:
            if not url.startswith("https://"):
                continue
            auth = re.split(r"[/?#]", url[len("https://"):], maxsplit=1)[0]
            ok = auth and "@" not in auth and re.fullmatch(r"[A-Za-z0-9.-]+(:[0-9]{1,5})?", auth) \
                and not auth.startswith((".", "-"))
            if ok and f"https://{auth.lower()}" not in out:
                out.append(f"https://{auth.lower()}")
    return out


def page_csp(store=None):
    """As the real server's csp(): the pages may load tiles only from the online tile server, named while it is on."""
    extra = "".join(f" {o}" for o in tile_origins(store)) if store is not None else ""
    return (f"default-src 'self'; img-src 'self' data: blob:{extra}; worker-src 'self' blob:; "
            f"style-src 'self'; script-src 'self'; connect-src 'self'{extra}; font-src 'self'; "
            "frame-ancestors 'none'; base-uri 'none'; form-action 'self'")


CSRF_HEADER = "X-Sentry-Mate"


def page_referrer_policy(store):
    """As the real server: pages send their origin (never a path) to the tile server while online tiles are on, since
    OpenStreetMap's tile policy blocks tile requests without a Referer; otherwise no Referer at all."""
    return "strict-origin" if tile_origins(store) else "no-referrer"
MAX_BODY = 1 << 20
# changes a moved server still takes: the move itself, export files, signing in and out
# (and the recovery key file: shown again, or removed once saved; a new key is refused, as on the real server)
MOVED_STILL_OPEN = ("/api/v1/system/move", "/api/v1/system/exports", "/api/v1/system/recovery-key/show",
                    "/api/v1/system/recovery-key/saved", "/api/v1/session", "/api/v1/me/sessions",
                    "/api/v1/me/preferences")
TYPES = {".js": "text/javascript; charset=utf-8", ".mjs": "text/javascript; charset=utf-8",
         ".css": "text/css; charset=utf-8", ".html": "text/html; charset=utf-8", ".svg": "image/svg+xml",
         ".json": "application/json; charset=utf-8", ".woff2": "font/woff2", ".woff": "font/woff",
         ".txt": "text/plain; charset=utf-8", ".png": "image/png", ".webp": "image/webp", ".ico": "image/x-icon",
         ".map": "application/json; charset=utf-8", ".pmtiles": "application/octet-stream"}


class App:
    def __init__(self, cfg):
        self.cfg = cfg
        self.clock = Clock(cfg.now_ms, frozen=cfg.freeze_clock)
        t0 = time.monotonic()
        self.ds = Dataset(cfg)
        self.ds.synthetic.warm()  # sorts, counts and search of a large fleet: built before the first request
        for h in self.ds.featured.values():  # each drive's power peaks (the slow part of a drive row), once
            for d in h.drives:
                d._power_peaks()
        self.build_s = time.monotonic() - t0
        self.store = Store(cfg, self.ds, self.clock)
        self.cache = {}
        resources.invoices.apply_prices(self)  # the places' prices from the made-up Supercharger invoices
        # One change at a time: a handler checks `version` and then changes the data, and two requests
        # with the same version must not both pass the check.
        self.write_lock = threading.Lock()
        if self.ds.synthetic.count:  # a large fleet: its search and attention indexes, before the first request
            resources.software.warm(self)
            resources.fleet.warm(self)
        self.events = EventHub(self)
        self.router = Router()
        resources.register_all(self.router)
        self.packs = Packs(cfg.pack)
        self.web_root = os.path.realpath(os.path.join(cfg.pack, "web"))
        # Which build of the interface the server runs (X-Sentry-Mate-Build on every answer, as the real server).
        # POST /__dev/new-build stands for an update: a tab open across it sees another build.
        self.build = "devbuild00000001"
        self.builds = 1

    def new_build(self):
        self.builds += 1
        self.build = f"devbuild{self.builds:08d}"
        return self.build

    def root_for(self, session):
        """The web folder this browser is served: the person's own `ui_pack`, else the --pack one."""
        if session is None:
            return self.web_root
        own = self.store.get_prefs(session["account"]).get("ui_pack")
        pid = self.packs.pick(own)
        return self.web_root if pid == self.packs.served else self.packs.web_root(pid)

    def make_server(self):
        srv = Server((self.cfg.host, self.cfg.port), Handler)
        srv.app = self
        self.events.start()
        return srv


class Server(socketserver.ThreadingMixIn, http.server.HTTPServer):
    daemon_threads = True
    allow_reuse_address = True
    request_queue_size = 128


class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    server_version = "SentryMateDev"
    sys_version = ""

    @property
    def app(self) -> App:
        return self.server.app

    def log_message(self, fmt, *args):
        if self.app.cfg.verbose:
            sys.stderr.write("%s %s\n" % (self.address_string(), fmt % args))

    def do_GET(self):
        self.handle_any("GET")

    def do_HEAD(self):
        self.handle_any("HEAD")

    def do_POST(self):
        self.handle_any("POST")

    def do_PUT(self):
        self.handle_any("PUT")

    def do_PATCH(self):
        self.handle_any("PATCH")

    def do_DELETE(self):
        self.handle_any("DELETE")

    def do_OPTIONS(self):
        self.handle_any("OPTIONS")

    # ------------------------------------------------------------ responses
    def send(self, status, body=b"", content_type=None, headers=None, api=False):
        self.send_response(status)
        self.send_header("Content-Security-Policy", page_csp(self.app.store))
        self.send_header("X-Content-Type-Options", "nosniff")
        self.send_header("Referrer-Policy", "no-referrer" if api else page_referrer_policy(self.app.store))
        self.send_header("X-Sentry-Mate-Build", self.app.build)
        if api:
            # (a tagged JSON answer of a GET says "private, no-cache" itself, see tagged())
            self.send_header("Cache-Control", (headers or {}).pop("Cache-Control", None) or "no-store")
            # The virtual clock, so pages count "5 min ago" and the period presets from the dataset's "now".
            self.send_header("X-Sentry-Mate-Now-Ms", str(self.app.clock.now()))
        else:
            self.send_header("Cache-Control", "no-cache")
        if content_type:
            self.send_header("Content-Type", content_type)
        for k, v in (headers or {}).items():
            if isinstance(v, list):
                for x in v:
                    self.send_header(k, x)
            else:
                self.send_header(k, v)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        if self.command != "HEAD" and body:
            try:
                self.wfile.write(body)
            except (BrokenPipeError, ConnectionResetError):
                pass

    def send_error_obj(self, e: ApiError, cookie=None):
        headers = dict(e.headers)
        if cookie:
            headers["Set-Cookie"] = cookie
        self.send(e.status, json_bytes(e.body()), "application/json; charset=utf-8", headers, api=True)

    # ------------------------------------------------------------ sessions
    def cookie_value(self, name=COOKIE):
        raw = self.headers.get("Cookie")
        if not raw:
            return None
        try:
            c = SimpleCookie()
            c.load(raw)
        except Exception:
            return None
        m = c.get(name)
        return m.value if m else None

    def session(self):
        """(session or None, Set-Cookie value or None). Signs in the default account on first contact."""
        st = self.app.store
        token = self.cookie_value()
        if token and token != COOKIE_SIGNED_OUT:
            s = st.session_for(token)
            if s:
                return s, None
        # first contact, or a cookie from an earlier run of the dev server: sign in the default account
        # (not after a sign-out, and not for a session this server ended)
        if token != COOKIE_SIGNED_OUT and token not in st.ended and not self.app.cfg.signed_out:
            acc = st.default_account()
            if acc is not None:
                s = st.start_session(acc, device=_device(self.headers.get("User-Agent", "")))
                return s, set_cookie(s["token"])
        return None, None

    # ------------------------------------------------------------ dispatch
    def handle_any(self, method):
        try:
            parts = urlsplit(self.path)
            path = unquote(parts.path)
            if path == "/health":
                if method in ("GET", "HEAD"):
                    return self.send(200, b"", None, api=True)
                return self.send(405, b"", None, {"Allow": "GET, HEAD"}, api=True)
            if path == "/__dev/new-build":  # the dev server only: stands for a server update (README.md)
                if method != "POST":
                    return self.send(405, b"", None, {"Allow": "POST"}, api=True)
                return self.send(200, json_bytes({"build": self.app.new_build()}), "application/json; charset=utf-8",
                                 api=True)
            if path.startswith("/api/"):
                return self.api(method, path, parts.query)
            # a file under a build's address (/_v/<build>/core/app.js): the file of that path now, as the real
            # server answers a tab loaded before an update
            try:  # a page address that is not UTF-8 once decoded is no file (the real server's 404)
                path = unquote_to_bytes(parts.path).decode("utf-8")
            except UnicodeDecodeError:
                return self.send(404, b"Not found", "text/plain; charset=utf-8")
            m = re.match(r"^/_v/[^/]+(/.*)$", path)
            if m:
                path = m.group(1)
            return self.static(method, path)
        except Exception as e:  # last line of defence: answer 500 in the one error shape
            traceback.print_exception(e)
            try:
                self.send_error_obj(ApiError(500, "server_error", "The dev server failed on this request."))
            except Exception:
                pass

    def read_body(self):
        raw = (self.headers.get("Content-Length") or "0").strip()
        if not raw.isascii() or not raw.isdigit():
            self.close_connection = True  # the body cannot be skipped: do not read another request after it
            raise bad_input({"Content-Length": "Must be a whole number of bytes."})
        n = int(raw)
        if n > MAX_BODY:
            self.close_connection = True
            raise ApiError(413, "too_large", "The body is larger than 1 MB.")
        return self.rfile.read(n) if n > 0 else b""

    def api(self, method, path, query_string):
        app = self.app
        cookie = None
        try:
            body = self.read_body() if method in ("POST", "PUT", "PATCH", "DELETE") else b""
            route, params = app.router.match(path)
            if route is None:
                raise not_found("not_found", f"No API route {path}.")
            m = "GET" if method == "HEAD" else method
            if m not in route.handlers:
                allow = ", ".join(sorted(route.handlers))
                raise ApiError(405, "method_not_allowed", f"{path} answers {allow}.", headers={"Allow": allow})
            resources.moving.advance(app.store, app.clock.now())  # a move or restore here that finished
            uploads.advance(app, app.clock.now())  # the made-up upload cadence (mockapi/uploads.py)
            session, cookie = self.session()
            if m != "GET" and self.headers.get(CSRF_HEADER) != "1":
                raise csrf()
            if m != "GET" and not path.startswith(MOVED_STILL_OPEN) and \
                    resources.moving.refuses_changes(app.store, app.clock.now()):
                raise ApiError(409, "moved", "This server's data moved to another server: it takes no changes. "
                                             "Reading still works. To use it again: Settings, Server, Undo the move.")
            if m not in route.public and session is None:
                raise not_signed_in()
            account = session["account"] if session else None
            if m in route.admin and (account is None or account["role"] != "admin"):
                raise not_allowed("Only an admin may do this.")
            if route.car_scoped:
                car = params["car"]
                if not app.ds.exists(car) or not app.store.sees(account, car):
                    raise no_such_car()
            query = parse_query(query_string)
            if account is not None:
                # every car the query names (`cars=` or `car=`, on any route) must be one the account sees
                for key in ("cars", "car"):
                    text = query.get(key, [None])[-1]
                    if text is None or (key == "cars" and text == "all"):
                        continue
                    for c in (x.strip() for x in text.split(",")):
                        if c and (not app.ds.exists(c) or not app.store.sees(account, c)):
                            raise no_such_car()
            req = Req(app, m, path, query, self.headers, body, params, session)
            req.raw_cookie_token = self.cookie_value()
            req.setup_token = self.cookie_value(SETUP_COOKIE)  # the first-run claim (resources/moving.py)
            req.client_ip = self.client_address[0] if self.client_address else None
            if route.handlers[m] is resources.SSE:
                if method == "HEAD":  # the headers only: a stream has no length to tell
                    return self.send(200, b"", "text/event-stream; charset=utf-8",
                                     {"Set-Cookie": cookie} if cookie else {}, api=True)
                return self.sse(req, cookie)
            if app.cfg.latency_ms:
                time.sleep(app.cfg.latency_ms / 1000)
            if m == "GET" or getattr(route.handlers[m], "own_locks", False):
                # (a handler that takes its own locks: one that waits, like a new recovery key being made)
                res = route.handlers[m](req)
            else:
                with app.write_lock:
                    res = route.handlers[m](req)
            if req.set_cookie:
                cookie = req.set_cookie
            if isinstance(res, Response):
                headers = dict(res.headers)
                if cookie:
                    headers["Set-Cookie"] = cookie
                return self.send(res.status, res.body, res.content_type, headers, api=True)
            if req.csv:
                name = path.rstrip("/").split("/")[-1] or "data"
                csv_res = to_csv(res, name)
                if csv_res is None:
                    raise bad_input({"format": "CSV is offered for lists and columns only."})
                headers = dict(csv_res.headers)
                if cookie:
                    headers["Set-Cookie"] = cookie
                return self.send(200, csv_res.body, csv_res.content_type, headers, api=True)
            status = 201 if m in ("POST", "PUT") and isinstance(res, dict) and res.pop("_created", False) else 200
            headers = {"Set-Cookie": cookie} if cookie else {}
            body = json_bytes(_strip(res))
            if m == "GET" and status == 200 and not sensitive(path):
                tag = tagged(body)
                headers.update({"ETag": tag, "Cache-Control": "private, no-cache"})
                if tag in [t.strip() for t in (self.headers.get("If-None-Match") or "").split(",")]:
                    return self.send(304, b"", None, headers, api=True)
            return self.send(status, body, "application/json; charset=utf-8", headers, api=True)
        except ApiError as e:
            return self.send_error_obj(e, cookie)

    def sse(self, req, cookie):
        app = self.app
        stream = app.events.subscribe(req.session)
        self.send_response(200)
        self.send_header("Content-Security-Policy", page_csp())
        self.send_header("X-Content-Type-Options", "nosniff")
        self.send_header("Referrer-Policy", "no-referrer")
        self.send_header("Cache-Control", "no-store")
        self.send_header("Content-Type", "text/event-stream; charset=utf-8")
        self.send_header("Connection", "close")
        if cookie:
            self.send_header("Set-Cookie", cookie)
        self.end_headers()
        self.close_connection = True
        try:
            self.wfile.write(b"retry: 3000\n: connected\n\n")
            self.wfile.flush()
            while not stream.closed and not app.events.stop.is_set():
                try:
                    event, data = stream.q.get(timeout=15)
                except Exception:
                    self.wfile.write(b": keep-alive\n\n")
                    self.wfile.flush()
                    continue
                if event == "_close":
                    break
                self.wfile.write(frame(event, data, app.events.next_id()))
                self.wfile.flush()
        except (BrokenPipeError, ConnectionResetError, OSError):
            pass
        finally:
            app.events.unsubscribe(stream)

    # ------------------------------------------------------------ static files
    def static(self, method, path):
        if method not in ("GET", "HEAD"):
            return self.send(405, b"", None, {"Allow": "GET, HEAD"})
        rel = path.lstrip("/") or "index.html"
        # as the real server (crates/web/src/packs/mod.rs, serve_file): these are never files, and a name with a
        # dot that is no file is a missing file, not a page
        if ".." in rel or "\\" in rel or "//" in rel or "\0" in rel:
            return self.send(404, b"Not found", "text/plain; charset=utf-8")
        name = rel.rsplit("/", 1)[-1]
        if name in NEVER_SERVED:
            return self.send(404, b"Not found", "text/plain; charset=utf-8")
        token = self.cookie_value()
        s = self.app.store.session_for(token) if token and token != COOKIE_SIGNED_OUT else None
        root = self.app.root_for(s)
        full = os.path.realpath(os.path.join(root, rel))
        is_file = (full.startswith(root + os.sep)) and os.path.isfile(full)
        cookie = None
        if rel == "index.html" or (not is_file and "." not in name and not rel.endswith("/")):
            full = os.path.join(root, "index.html")
        elif not is_file:
            return self.send(404, b"Not found", "text/plain; charset=utf-8")
        if full.endswith("index.html"):
            s, cookie = self.session()  # a session starts on a page load
            root = self.app.root_for(s)
            full = os.path.join(root, "index.html")
        try:
            with open(full, "rb") as f:
                data = f.read()
        except OSError:
            return self.send(404, b"Not found", "text/plain; charset=utf-8")
        ext = os.path.splitext(full)[1].lower()
        ctype = TYPES.get(ext) or mimetypes.guess_type(full)[0] or "application/octet-stream"
        headers = {"Set-Cookie": cookie} if cookie else {}
        return self.send(200, data, ctype, headers)


NEVER_SERVED = ("mock-api.js", "mock-api.mjs")  # the packs' made-up stand-in for the API, as the real server's list


def parse_query(text):
    """A query string as the real server reads it (crates/web/src/query.rs, Params::parse): a dict of lists; a part
    that is not UTF-8 once decoded, or a 201st parameter, is 400."""
    out, n = {}, 0
    for part in text.split("&"):
        if not part:
            continue
        k, _, v = part.partition("=")
        try:
            k, v = (unquote_to_bytes(x.replace("+", " ")).decode("utf-8") for x in (k, v))
        except UnicodeDecodeError:
            raise bad_input({"query": "The query is not valid UTF-8."})
        if n >= 200:
            raise bad_input({"query": "Too many parameters."})
        n += 1
        out.setdefault(k, []).append(v)
    return out


def set_cookie(token):
    """The session cookie: HttpOnly and SameSite=Strict. Not Secure: the dev server is plain HTTP on 127.0.0.1."""
    return f"{COOKIE}={token}; Path=/; Max-Age=2592000; HttpOnly; SameSite=Strict"


def set_setup_cookie(token, max_age_s=3600):
    """The first-run claim: HttpOnly, SameSite=Strict, an hour from its last use. The real server names it
    __Host-sentry-mate-setup and adds Secure (HTTPS only)."""
    return f"{SETUP_COOKIE}={token}; Path=/; Max-Age={max_age_s}; HttpOnly; SameSite=Strict"


def clear_setup_cookie():
    return f"{SETUP_COOKIE}=; Path=/; Max-Age=0; HttpOnly; SameSite=Strict"


def clear_cookie():
    return f"{COOKIE}={COOKIE_SIGNED_OUT}; Path=/; Max-Age=2592000; HttpOnly; SameSite=Strict"


def _device(ua):
    for name in ("Firefox", "Edg", "Chrome", "Safari"):
        if name in ua:
            return {"Edg": "Edge"}.get(name, name)
    return "Browser"



# As the real server (crates/web/src/delivery.rs): a successful GET with a JSON body is tagged (a weak tag, a hash
# of the body), "private, no-cache", and answered 304 with no body when If-None-Match names the tag. Paths that may
# carry a secret stay no-store, untagged.
SECRET_PARTS = ("token", "key", "setup", "/me", "two-step", "session", "auth", "sign-in", "sign-out", "password", "exports",
                "move", "recover")


def sensitive(path: str) -> bool:
    p = path.lower()
    return any(w in p for w in SECRET_PARTS)


def tagged(body: bytes) -> str:
    return 'W/"' + hashlib.sha256(body).hexdigest()[:20] + '"'


def _strip(obj):
    """Drop private keys (starting with _) before sending."""
    if isinstance(obj, dict):
        return {k: _strip(v) for k, v in obj.items() if not (isinstance(k, str) and k.startswith("_"))}
    if isinstance(obj, list):
        return [_strip(v) for v in obj]
    return obj

