"""The made-up dataset: three featured cars with full history, plus light synthetic cars.

Build once per server (`Dataset(config)`); everything is derived from the seed and "now", so the
same flags always give the same data. Histories run to two days past "now": pieces that start
after the virtual clock are not shown yet, and the piece the clock is in is "in progress".
"""

from __future__ import annotations

import bisect

from .. import sources as S
from ..util import DAY, local_date
from . import places as P
from .cars import featured_specs, sold_spec
from .sim import Sim
from .synthetic import SyntheticFleet


class CarHistory:
    """One featured car: its pieces in time order, and quick lookups."""

    def __init__(self, car, segs, shadow, overlaps, import_overlaps=None):
        self.car = car
        self.segs = segs
        self.starts = [s.start for s in segs]
        self.shadow = shadow
        self.overlaps = overlaps  # [{start_ms, end_ms, shown}]
        # spans both imports cover: [{start_ms, end_ms, shown: "teslamate" | "teslafi", default, reason}]
        self.import_overlaps = import_overlaps or []
        self.overlap_changes = 0  # the overlaps' `version` is one more
        self.drives = [s for s in segs if s.kind == "drive"] + [s for s in shadow if s.kind == "drive"]
        self.drives.sort(key=lambda s: (s.start, s.id))
        self.charges = [s for s in segs if s.kind == "charge"] + [s for s in shadow if s.kind == "charge"]
        self.charges.sort(key=lambda s: (s.start, s.id))
        self.updates = [s for s in segs if s.kind == "update"]
        self.by_id = {s.id: s for s in self.drives + self.charges + self.updates}
        self.first_ms = segs[0].start if segs else None

    def seg_index_at(self, t):
        i = bisect.bisect_right(self.starts, t) - 1
        return i if 0 <= i < len(self.segs) and t < self.segs[i].end else None

    def seg_at(self, t):
        i = self.seg_index_at(t)
        return None if i is None else self.segs[i]

    def segs_between(self, a, b, now):
        """Pieces overlapping [a, b) that have started by `now`, in time order."""
        b = min(b, now + 1)
        i = max(0, bisect.bisect_right(self.starts, a) - 1)
        out = []
        while i < len(self.segs) and self.segs[i].start < b:
            s = self.segs[i]
            if s.end > a:
                out.append(s)
            i += 1
        return out

    def site_shown(self, row, source="all"):
        """Where both imports cover the same time, whether an imported row is of the chosen site (with `source`
        all or import; a site's own filter shows that site alone)."""
        site = S.site_of(row)
        if site is None or S.canon(source or "all") not in ("all", "import"):
            return True
        for o in self.import_overlaps:
            if o["start_ms"] <= row.start < o["end_ms"]:
                return site == o["shown"]
        return True

    def shown(self, row, source="all"):
        """Whether a drive or charge row is listed for a source filter (overlaps show one side, and one import)."""
        if source not in (None, "all"):
            return S.keeps(source, row.source, getattr(row, "inputs", None), S.site_of(row)) \
                and self.site_shown(row, source)
        for o in self.overlaps:
            if o["start_ms"] <= row.start < o["end_ms"]:
                return row.source == o["shown"] and self.site_shown(row)
        return self.site_shown(row)


class Dataset:
    def __init__(self, cfg):
        self.seed = cfg.seed
        self.now0 = cfg.now_ms
        self.n_cars = cfg.cars
        self.plan_end = cfg.now_ms + 2 * DAY
        self.featured = {}
        specs = featured_specs()[: max(0, min(3, cfg.cars))]
        if cfg.cars >= 3:  # and the car the owner sold (its history only)
            specs.append(sold_spec())
        for spec in specs:
            spec.now_ms = cfg.now_ms
            spec.now_day_index = cfg.now_ms // DAY
            spec.live_drive = None
            spec.live_charge = None
            sim = Sim(spec, cfg.seed, cfg.now_ms, self.plan_end).run()
            overlaps = []
            if sim.can_from is not None and sim.tm_until is not None:
                overlaps.append({"start_ms": sim.can_from, "end_ms": sim.tm_until, "shown": "can"})
            self.featured[spec.id] = CarHistory(spec, sim.segs, sim.shadow, overlaps, _import_overlaps(sim))
        # a fleet of N cars counts the sold car among them (c4 to c<N-1> are the synthetic ones)
        self.synthetic = SyntheticFleet(cfg.cars - 1 if cfg.cars > 3 else cfg.cars, cfg.now_ms, cfg.seed)
        self.places = P.PLACES

    # --- cars
    def car_ids_featured(self):
        return list(self.featured)

    def exists(self, car_id):
        return isinstance(car_id, str) and (car_id in self.featured or self.synthetic.has(car_id))

    def history(self, car_id):
        return self.featured.get(car_id)

    def total_cars(self):
        return len(self.featured) + self.synthetic.count

    def car_tz(self, car_id):
        h = self.featured.get(car_id)
        if h:
            return h.car.time_zone
        return self.synthetic.summary(int(car_id[1:]))["time_zone"]

    def local_day(self, car_id, ms):
        return local_date(ms, self.car_tz(car_id))


def _import_overlaps(sim):
    """The span both imports cover (TeslaFi's copies of TeslaMate's rows), widened over the rows reaching into it.
    The same drives and charges on both sides: TeslaMate's positions every few seconds beat TeslaFi's once a minute,
    so TeslaMate's is shown at first."""
    if sim.tf_span is None:
        return []
    a, b = sim.tf_span
    rows = [s for s in sim.segs + sim.shadow
            if s.kind in ("drive", "charge") and s.source == "teslamate" and s.end > a and s.start < b]
    if not rows:
        return []
    a, b = min([a] + [s.start for s in rows]), max([b] + [s.end for s in rows])
    return [{"start_ms": a, "end_ms": b, "shown": "teslamate", "default": "teslamate", "reason": "more_samples"}]
