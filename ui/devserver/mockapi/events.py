"""Server-sent events (/api/v1/events): live `now` deltas, `rows`, `fleet`, `prefs`, `notice`, and the live figures
`job`, `health`, `connections` and `storage` (as the real server's crates/web/src/live.rs).

A ticker computes each featured car's "now" object every --sse-interval-ms and sends what changed
to every stream whose account may see that car. A stream's first `now` event for a car carries
the whole object; later ones only the fields that changed (merge them into what you have).
With --rows-every-s N every featured car also gets a `rows` event every N seconds with every row kind, as if
its rows had changed: the pages load their lists again, so their refreshes can be watched (ui/tools/check.js
--stable) without waiting for a drive to end. With --rebuild-running that is every second unless --rows-every-s
says otherwise, as while a long rebuild rewrites the car's rows window by window (ui/tools/scroll.js).

The live figures are worked out with the same handlers as their GET routes, every LIVE_TICK_S while a stream is
open, and each topic (a job's kind and id, a car's health, the connections, the storage) is sent only when its value
changed (leaving out a top-level `read_ms`) and its gap since the last one passed: 0.5 s for a job, 1 s for health
and connections, 2 s for storage. `health` goes to the streams that see the car; the others to admins' streams only.
A topic that takes long to work out is looked at less often (at most a twentieth of the time goes to it).
"""

from __future__ import annotations

import queue
import threading
import time

from .conventions import json_bytes
from .live import now_object

LIVE_TICK_S = 0.25
JOB_GAP_S, HEALTH_GAP_S, CONNECTIONS_GAP_S, STORAGE_GAP_S = 0.5, 1.0, 1.0, 2.0
SHARE = 20
# a compaction pass started from a page moves one notch on this often while a stream is open
COMPACTION_NOTCH_S = 2.0
# the live account the figures are read as (an admin that sees every car)
_LIVE_ACCOUNT = {"id": "_live", "name": "server", "role": "admin", "cars": "all"}

ALL_ROW_KINDS = ["charges", "drives", "states", "updates", "visits"]
ROW_KINDS = {"drive": ["drives", "states"], "charge": ["charges", "states"], "parked": ["visits", "states"],
             "gap": ["states"], "update": ["updates", "states"]}


_MISSING = object()

# the look for missing CAN logs (the `job` of kind `missing_scan`) when none ran since the start
MISSING_SCAN_IDLE = {"state": "idle", "keys_done": 0, "keys_total": 0, "found": 0, "started_ms": None,
                     "finished_ms": None}


def _leaf(v):
    return isinstance(v, dict) and set(v) == {"value", "at_ms"}


def diff(old, new):
    """The parts of `new` that differ from `old` (nested objects compared field by field; a
    { value, at_ms } pair is sent whole)."""
    out = {}
    for k, v in new.items():
        o = old.get(k, _MISSING)
        if isinstance(v, dict) and isinstance(o, dict) and not _leaf(v):
            d = diff(o, v)
            if d:
                out[k] = d
        elif o != v:
            out[k] = v
    return out


class Stream:
    def __init__(self, session):
        self.session = session
        self.q = queue.Queue(maxsize=1000)
        self.sent_full = set()
        self.closed = False

    def push(self, event, data):
        """Queue one event. A reader that fell 1000 events behind would miss a `now` delta and merge the
        later ones into stale values, so its stream is ended instead: the page reconnects and loads again."""
        if self.closed and event != "_close":
            return
        try:
            self.q.put_nowait((event, data))
        except queue.Full:
            self.closed = True  # the reader is busy with the queue, so it sees `closed` at its next event


def _comparable(v):
    if isinstance(v, dict) and "read_ms" in v:
        return {k: x for k, x in v.items() if k != "read_ms"}
    return v


class Throttle:
    """What was sent last per topic: a new value goes out only when it differs and the topic's gap passed. A change
    that comes too soon is not kept: the topic is worked out again at its next look, and sent then."""

    def __init__(self):
        self.sent = {}

    def offer(self, topic, value, gap, now):
        key = _comparable(value)
        last = self.sent.get(topic)
        if last is not None and (last[0] == key or now - last[1] < gap):
            return False
        self.sent[topic] = (key, now)
        return True

    def clear(self):
        self.sent.clear()


class EventHub:
    def __init__(self, app):
        self.app = app
        self.lock = threading.Lock()
        self.streams = []
        self.last = {}
        self.last_seg = {}
        self.seq = 0
        self.forced_rows_at = None  # time.monotonic() of the last --rows-every-s event
        self.throttle = Throttle()
        self.due = {}  # group -> time.monotonic() it is looked at next
        self.notch_at = None  # time.monotonic() a running compaction last moved on
        self.stop = threading.Event()
        self.thread = None

    def start(self):
        if self.thread is None:
            self.thread = threading.Thread(target=self._run, name="sse-ticker", daemon=True)
            self.thread.start()

    def shutdown(self):
        self.stop.set()
        with self.lock:
            for s in self.streams:
                s.closed = True
                s.push("_close", None)

    def subscribe(self, session):
        s = Stream(session)
        with self.lock:
            self.streams.append(s)
        return s

    def unsubscribe(self, s):
        with self.lock:
            if s in self.streams:
                self.streams.remove(s)

    def next_id(self):
        with self.lock:
            self.seq += 1
            return self.seq

    def publish(self, event, data, car=None, account_id=None, admins_only=False):
        st = self.app.store
        with self.lock:
            targets = list(self.streams)
        for s in targets:
            acc = s.session["account"]
            if account_id is not None and acc["id"] != account_id:
                continue
            if admins_only and acc.get("role") != "admin":
                continue
            if car is not None and not st.sees(acc, car):
                continue
            s.push(event, data)

    def account_cars_changed(self, account, old, new, prefs):
        """An account's car list changed: its streams get a `fleet` event per car added or removed (one
        `{ car: null, change: "list_changed" }` when more than 100 changed) and its preferences again,
        whose order and pins now list other cars. A car taken away starts from a whole `now` if it comes back."""
        ds = self.app.ds

        def ids(v):
            if v != "all":
                return set(v)
            n = ds.total_cars()
            return None if n > 1000 else {f"c{i}" for i in range(0, n + 1) if ds.exists(f"c{i}")}

        a, b = ids(old), ids(new)
        if a is not None and b is not None:
            added, removed = sorted(b - a, key=lambda c: int(c[1:])), sorted(a - b, key=lambda c: int(c[1:]))
            events = [{"car": c, "change": "added", "name": self.app.store.car_meta(c)["name"]} for c in added] + \
                     [{"car": c, "change": "removed", "name": None} for c in removed]
            if len(events) > 100:
                events = [{"car": None, "change": "list_changed", "name": None}]
        else:
            events = [] if old == new else [{"car": None, "change": "list_changed", "name": None}]
        with self.lock:
            targets = [s for s in self.streams if s.session["account"]["id"] == account["id"]]
        for s in targets:
            s.sent_full = {c for c in s.sent_full if self.app.store.sees(account, c)}
            for e in events:
                s.push("fleet", e)
            if events:
                s.push("prefs", prefs)

    def end_session(self, token):
        with self.lock:
            targets = [s for s in self.streams if s.session.get("token") == token]
        for s in targets:
            s.closed = True
            s.push("_close", None)

    def _run(self):
        interval = self.app.cfg.sse_interval_ms / 1000
        next_tick = time.monotonic() + 0.3
        while not self.stop.wait(max(0.0, min(LIVE_TICK_S, next_tick - time.monotonic()))):
            if time.monotonic() >= next_tick:
                next_tick = time.monotonic() + interval
                self._safely(self.tick)
            self._safely(self.live_round)

    def _safely(self, fn):
        try:
            fn()
        except Exception as e:  # keep ticking; a dev server should not die from one bad tick
            if self.app.cfg.verbose:
                import traceback
                traceback.print_exception(e)

    # ------------------------------------------------------------------ live figures
    def _get(self, path):
        """The body GET `path` answers now, read as the server (an admin that sees every car)."""
        from .conventions import Req
        from .server import _strip
        route, params = self.app.router.match(path)
        req = Req(self.app, "GET", path, {}, {}, b"", params or {}, {"account": _LIVE_ACCOUNT})
        req.client_ip = None
        return _strip(route.handlers["GET"](req)), req

    def _group_due(self, name, gap, fn):
        """Runs group `name` when due; one that took long is looked at less often."""
        t0 = time.monotonic()
        if t0 < self.due.get(name, 0):
            return []
        try:
            out = fn()
        except Exception as e:  # a state some handler refuses (a fresh server, a move): nothing this round
            if self.app.cfg.verbose:
                import traceback
                traceback.print_exception(e)
            out = []
        t1 = time.monotonic()
        self.due[name] = t1 + max(gap, (t1 - t0) * SHARE)
        return out

    def _compaction(self, advance):
        from .resources import system
        from .conventions import Req
        req = Req(self.app, "GET", "/api/v1/system/storage/detail", {}, {}, b"", {}, {"account": _LIVE_ACCOUNT})
        with self.app.store.lock:
            return system._compaction(req, advance=advance)

    def _jobs(self):
        st = self.app.store
        out = []
        fr, _ = self._get("/api/v1/system/full-rebuild")
        out.append(("full_rebuild", "", fr))
        out.append(("compaction", "", self._compaction(False)))
        for eid in list(st.exports):
            try:
                v, _ = self._get(f"/api/v1/system/exports/{eid}")
            except Exception:
                continue  # removed meanwhile
            out.append(("export", eid, v))
        out.append(("missing_scan", "", dict(getattr(st, "missing_scan", None) or MISSING_SCAN_IDLE)))
        for iid in list(st.imports):
            try:
                v, _ = self._get(f"/api/v1/imports/{iid}")
            except Exception:
                continue
            out.append(("import", iid, v))
        return [(("job", kind, jid), "job", None, True, {"kind": kind, "id": jid, "status": v}, JOB_GAP_S)
                for kind, jid, v in out]

    def _invoices(self):
        out = []
        for src in ("tesla", "teslafi"):
            v, _ = self._get(f"/api/v1/integrations/{src}")
            out.append((("job", "invoices", src), "job", None, True,
                        {"kind": "invoices", "id": src, "status": v["invoices"]}, JOB_GAP_S))
        return out

    def _health(self):
        out = []
        for car in self.app.ds.featured:
            v, _ = self._get(f"/api/v1/cars/{car}/health")
            out.append((("health", car), "health", car, False, {"car": car, "health": v}, HEALTH_GAP_S))
        return out

    def _connections(self):
        v, _ = self._get("/api/v1/integrations/overview")
        return [(("connections",), "connections", None, True, v, CONNECTIONS_GAP_S)]

    def _storage(self):
        v, _ = self._get("/api/v1/system/storage")
        v["compaction"] = self._compaction(False)
        return [(("storage",), "storage", None, True, v, STORAGE_GAP_S)]

    def live_round(self):
        """One look at the live figures: what changed (and whose gap passed) is published."""
        app = self.app
        with self.lock:
            streams = list(self.streams)
        if not streams:
            self.throttle.clear()
            self.due.clear()
            self.notch_at = None
            return
        from . import uploads
        uploads.advance(app, app.clock.now())
        mono = time.monotonic()
        # a compaction pass started from a page moves on by itself while a page watches
        c = app.store.compaction
        if c is not None and c.get("status") == "running":
            if self.notch_at is None:
                self.notch_at = mono
            elif mono - self.notch_at >= COMPACTION_NOTCH_S:
                self.notch_at = mono
                self._compaction(True)
        else:
            self.notch_at = None
        admins = any(s.session["account"].get("role") == "admin" for s in streams)
        out = []
        if admins:
            out += self._group_due("jobs", JOB_GAP_S, self._jobs)
            out += self._group_due("invoices", JOB_GAP_S * 2, self._invoices)
        out += self._group_due("health", HEALTH_GAP_S, self._health)
        if admins:
            out += self._group_due("connections", CONNECTIONS_GAP_S, self._connections)
            out += self._group_due("storage", STORAGE_GAP_S, self._storage)
        now = time.monotonic()
        for topic, name, car, admins_only, data, gap in out:
            if self.throttle.offer(topic, data, gap, now):
                self.publish(name, data, car=car, admins_only=admins_only)

    def tick(self):
        app = self.app
        t = app.clock.now()
        with self.lock:
            streams = list(self.streams)
        every = app.cfg.rows_every_s or (1.0 if app.cfg.rebuild_running else 0)
        mono = time.monotonic()
        forced = bool(every) and (self.forced_rows_at is None or mono - self.forced_rows_at >= every - 0.05)
        if forced:
            self.forced_rows_at = mono
        for car_id, h in app.ds.featured.items():
            if forced:
                self.publish("rows", {"car": car_id, "kinds": ALL_ROW_KINDS, "from_ms": t - int(every * 1000), "to_ms": t},
                             car=car_id)
            # rows: a piece started or finished since the last tick
            i = h.seg_index_at(t)
            prev = self.last_seg.get(car_id)
            if prev is not None and i is not None and i != prev:
                a = h.segs[min(prev, i)].start
                kinds = sorted({k for j in range(min(prev, i), max(prev, i) + 1) for k in ROW_KINDS[h.segs[j].kind]})
                self.publish("rows", {"car": car_id, "kinds": kinds, "from_ms": a, "to_ms": t}, car=car_id)
            self.last_seg[car_id] = i
            if not streams:
                continue
            obj = now_object(app, car_id, t)
            old = self.last.get(car_id)
            delta = diff(old, obj) if old is not None else obj
            self.last[car_id] = obj
            for s in streams:
                if not app.store.sees(s.session["account"], car_id):
                    continue
                if car_id not in s.sent_full:
                    s.sent_full.add(car_id)
                    s.push("now", {"car": car_id, "now": obj})
                elif delta:
                    s.push("now", {"car": car_id, "now": delta})


def frame(event, data, eid):
    return b"event: " + event.encode() + b"\nid: " + str(eid).encode() + b"\ndata: " + json_bytes(data) + b"\n\n"
