"""Server-sent events (/api/v1/events): live `now` deltas, `rows`, `fleet`, `prefs` and `notice`.

A ticker computes each featured car's "now" object every --sse-interval-ms and sends what changed
to every stream whose account may see that car. A stream's first `now` event for a car carries
the whole object; later ones only the fields that changed (merge them into what you have).
"""

from __future__ import annotations

import queue
import threading

from .conventions import json_bytes
from .live import now_object

ROW_KINDS = {"drive": ["drives", "states"], "charge": ["charges", "states"], "parked": ["visits", "states"],
             "gap": ["states"], "update": ["updates", "states"]}


_MISSING = object()


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


class EventHub:
    def __init__(self, app):
        self.app = app
        self.lock = threading.Lock()
        self.streams = []
        self.last = {}
        self.last_seg = {}
        self.seq = 0
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

    def publish(self, event, data, car=None, account_id=None):
        st = self.app.store
        with self.lock:
            targets = list(self.streams)
        for s in targets:
            acc = s.session["account"]
            if account_id is not None and acc["id"] != account_id:
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
            return None if n > 1000 else {f"c{i}" for i in range(1, n + 1) if ds.exists(f"c{i}")}

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
        first = True
        while not self.stop.wait(0.3 if first else interval):
            first = False
            try:
                self.tick()
            except Exception as e:  # keep ticking; a dev server should not die from one bad tick
                if self.app.cfg.verbose:
                    import traceback
                    traceback.print_exception(e)

    def tick(self):
        app = self.app
        t = app.clock.now()
        with self.lock:
            streams = list(self.streams)
        for car_id, h in app.ds.featured.items():
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
