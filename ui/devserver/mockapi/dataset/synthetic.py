"""Light synthetic cars c4..cN for --cars N: summary-level data computed from the car's index.

Nothing is stored per car. Activity ages grow with the index, so "last activity, newest first" is
simply index order and paging needs no sort. Other sorts (name, battery), the search and state
filters, the counts and the attention items come from cached indexes built once, when the server
starts (warm()), so the first page of a large fleet is as fast as the next.
"""

from __future__ import annotations

import threading

from ..util import DAY, HOUR, MIN, u01

WORDS_A = ["Amber", "Birch", "Cedar", "Delta", "Ember", "Fjord", "Garnet", "Harbor", "Indigo", "Juniper",
           "Kestrel", "Lumen", "Meadow", "Nimbus", "Onyx", "Pine", "Quartz", "Raven", "Sierra", "Tundra",
           "Umber", "Vale", "Willow", "Xenon", "Yarrow", "Zephyr"]
WORDS_B = ["Arrow", "Badger", "Comet", "Drift", "Echo", "Falcon", "Glider", "Heron", "Iris", "Jet", "Kite",
           "Lynx", "Marlin", "Nova", "Otter", "Pulse", "Quill", "Rover", "Swift", "Tide", "Unity", "Vector",
           "Wren", "Yeti", "Zest"]
MODELS = [  # model, trims, vin prefix stem (9 chars, made up), full range km, weight
    ("Model Y", ("Long Range AWD", "RWD", "Performance"), "7SAYGDEE0", 520, 34),
    ("Model 3", ("Long Range AWD", "RWD", "Performance"), "5YJ3E1EA0", 500, 30),
    ("Model S", ("Long Range", "Plaid"), "5YJSA1E20", 600, 10),
    ("Model X", ("Long Range", "Plaid"), "7SAXCBE20", 540, 9),
    ("Cybertruck", ("AWD", "Cyberbeast"), "7G2CEHED0", 510, 7),
]
MODEL_WEIGHT = sum(m[4] for m in MODELS)
YEAR_LETTER = {2019: "K", 2020: "L", 2021: "M", 2022: "N", 2023: "P", 2024: "R", 2025: "S", 2026: "T"}
ZONES = ["America/Los_Angeles", "America/Chicago", "America/New_York", "America/Denver", "America/Phoenix"]
DEPOTS = [("Depot 1", 37.7706, -122.3915), ("Depot 2", 37.6550, -122.1180), ("Depot 3", 37.7980, -122.3060),
          ("Depot 4", 37.6545, -122.3990), ("Customer site", 37.4430, -122.1610), ("Long-term lot", 37.6280, -122.4010)]


class SyntheticFleet:
    def __init__(self, n_cars, now_ms, seed):
        self.n = max(0, n_cars)  # total fleet size; synthetic cars are 4..n
        self.now_ms = now_ms
        self.salt = sum(ord(c) * (i + 1) for i, c in enumerate(str(seed)))
        self._lock = threading.Lock()
        self._sorted = {}
        self._counts = None
        self._attention = None
        self._index = None
        self._filtered = {}  # (order, search, states) -> car indexes that pass, in that order (a few kept)

    @property
    def count(self):
        return max(0, self.n - 3)

    def has(self, car_id):
        if not isinstance(car_id, str) or not car_id.startswith("c") or not car_id[1:].isascii() \
                or not car_id[1:].isdigit() or car_id[1:].startswith("0"):
            return False
        i = int(car_id[1:])
        return 4 <= i <= self.n

    def q(self, i):
        return (i - 4) / max(1, self.count)

    def age_ms(self, i):
        q = self.q(i)
        if q < 0.1:
            return int(20_000 + q / 0.1 * 10 * MIN)
        if q < 0.85:
            return int(10 * MIN + (q - 0.1) / 0.75 * 2 * DAY)
        return int(2 * DAY + (q - 0.85) / 0.15 * 58 * DAY)

    def name(self, i):
        j = i - 4
        a = WORDS_A[j % len(WORDS_A)]
        b = WORDS_B[(j // len(WORDS_A) + 7 * (j % len(WORDS_A))) % len(WORDS_B)]
        rnd = j // (len(WORDS_A) * len(WORDS_B))
        return f"{a} {b}" + (f" {rnd + 1}" if rnd else "")

    def soc(self, i):
        return round(12 + 84 * u01(i, self.salt + 1), 1)

    def state(self, i):
        q = self.q(i)
        x = u01(i, self.salt + 2)
        if q < 0.1:
            return "driving" if x < 0.4 else "charging" if x < 0.7 else "parked"
        if q < 0.85:
            return "parked" if x < 0.15 else "asleep"
        return "unknown" if self.age_ms(i) > 7 * DAY else "asleep"

    def ident(self, i):
        """(model row, year, VIN prefix) of car c<i>."""
        k = int(u01(i, self.salt + 3) * MODEL_WEIGHT)
        for m in MODELS:
            k -= m[4]
            if k < 0:
                break
        year = 2019 + int(u01(i, self.salt + 4) * 8)
        if m[0] == "Cybertruck":
            year = max(year, 2024)
        return m, year, f"{m[2]}{YEAR_LETTER.get(year, 'S')}F"

    def summary(self, i, now_ms=None):
        """The car list item for car c<i>."""
        now_ms = self.now_ms if now_ms is None else now_ms
        m, year, vin_prefix = self.ident(i)
        model, trims, stem, full_km, _w = m
        trim = trims[int(u01(i, self.salt + 5) * len(trims))]
        tz = ZONES[int(u01(i, self.salt + 6) * len(ZONES))]
        st = self.state(i)
        age = self.age_ms(i)
        last = self.now_ms - age
        soc = self.soc(i)
        depot = DEPOTS[int(u01(i, self.salt + 7) * len(DEPOTS))]
        plugged = st == "charging" or (st in ("parked", "asleep") and u01(i, self.salt + 8) < 0.3)
        silent = st == "unknown" or self.q(i) > 0.92
        received = last if silent else now_ms - int(u01(i, self.salt + 9) * 90_000)
        since = last - int(u01(i, self.salt + 10) * 3 * HOUR) if st in ("parked", "asleep") else last - int(
            u01(i, self.salt + 11) * 40 * MIN)
        at_ms = last
        return {
            "id": f"c{i}", "name": self.name(i), "model": model, "year": year,
            "vin_prefix": vin_prefix, "trim": trim,
            "profile": "tesla-sx-legacy" if model in ("Model S", "Model X") and year < 2021 else "tesla-3y",
            "time_zone": tz, "time_zone_source": "position" if i % 7 else "default", "last_activity_ms": last,
            "now": {
                "state": st, "since_ms": since, "at_ms": at_ms, "received_ms": received,
                "stale": now_ms - received > HOUR,
                "soc_pct": soc, "range_km": round(soc / 100 * full_km, 1),
                "ideal_range_km": round(soc / 100 * full_km * 1.08, 1),
                "place": None if st == "driving" else {"id": None, "name": depot[0]},
                "plugged": plugged,
                "charge_power_kw": round(7 + 240 * u01(i, self.salt + 12) ** 3, 1) if st == "charging" else None,
            },
            "attention": self.attention_codes(i, st, soc, now_ms - received > HOUR),
            "_depot": depot,
        }

    def attention_codes(self, i, st, soc, stale):
        codes = []
        x = u01(i, self.salt + 13)
        if st == "unknown" and x < 0.15:
            codes.append("stopped_mid_drive")
        elif st == "unknown" and x < 0.22:
            codes.append("stopped_mid_charge")
        if stale:
            codes.append("upload_silence")
        if x > 0.995:
            codes.append("trust_failed")
        if soc < 18:
            codes.append("low_battery")
        if 0.40 < x < 0.42:
            codes.append("tyre_low")
        if st == "asleep" and 0.60 < x < 0.61:
            codes.append("door_open_asleep")
        return codes

    # --- cached indexes for a large fleet (built once, at start: see warm())
    def index(self):
        """Per car from c4 (list position i - 4): (states, battery %, lower-case names, search texts), cached.
        A search text is the name, model and VIN prefix, lower case, joined by NUL."""
        with self._lock:
            if self._index is None:
                states, socs, names, texts = [], [], [], []
                for i in range(4, self.n + 1):
                    m, _year, vin = self.ident(i)
                    name = self.name(i).lower()
                    states.append(self.state(i))
                    socs.append(self.soc(i))
                    names.append(name)
                    texts.append(f"{name}\0{m[0].lower()}\0{vin.lower()}")
                self._index = (states, socs, names, texts)
            return self._index

    def filtered(self, key, search, states):
        """Car indexes in `key` order ("activity", "name" or "soc") that pass the car list's `search`
        (lower case) and `state` filters. The last few answers are kept, so the next page is quick too."""
        k = (key, search or "", tuple(sorted(states or ())))
        with self._lock:
            hit = self._filtered.get(k)
        if hit is not None:
            return hit
        st, _socs, _names, texts = self.index()
        sts = set(states or ())
        ok = [i for i, (a, t) in enumerate(zip(st, texts), 4)
              if (not sts or a in sts) and (not search or search in t)]
        if key != "activity":
            if len(ok) > 2000:
                keep = set(ok)
                ok = [i for i in self.order(key) if i in keep]
            else:
                col = self.index()[2 if key == "name" else 1]
                ok.sort(key=lambda i: (col[i - 4], i))
        with self._lock:
            if len(self._filtered) >= 16:
                self._filtered.pop(next(iter(self._filtered)))
            self._filtered[k] = ok
        return ok

    def order(self, key):
        """Car indexes sorted by `name` or `soc` (ascending), cached."""
        _st, socs, names, _texts = self.index()
        with self._lock:
            if key not in self._sorted:
                idx = range(4, self.n + 1)
                col = names if key == "name" else socs
                self._sorted[key] = sorted(idx, key=lambda i: (col[i - 4], i))
            return self._sorted[key]

    def _stale(self, i, st):
        return st == "unknown" or self.q(i) > 0.92

    def counts(self):
        states, _socs, _names, _texts = self.index()
        with self._lock:
            if self._counts is None:
                by = {"driving": 0, "charging": 0, "parked": 0, "asleep": 0, "unknown": 0}
                stale = 0
                for i, st in enumerate(states, 4):
                    by[st] += 1
                    if self._stale(i, st):
                        stale += 1
                self._counts = (by, stale)
            return self._counts

    def attention(self):
        """[(since_ms, car index, code)] for every synthetic car, cached."""
        states, socs, _names, _texts = self.index()
        with self._lock:
            if self._attention is None:
                out = []
                for i, st in enumerate(states, 4):
                    codes = self.attention_codes(i, st, socs[i - 4], self._stale(i, st))
                    if codes:
                        last = self.now_ms - self.age_ms(i)
                        for c in codes:
                            out.append((last, i, c))
                self._attention = out
            return self._attention

    def warm(self):
        """Build every cached index now, so the first fleet page is as fast as the next."""
        if self.count:
            self.order("name")
            self.order("soc")
            self.counts()
            self.attention()
