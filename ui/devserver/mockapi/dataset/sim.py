"""Day-by-day simulation of a featured car: where it went, when it charged, how it slept.

The simulator walks the car's local days from the start of its history to two days past "now",
asks the car's planner for the day's trips, and writes one unbroken list of pieces (drives,
charges, parked spells). Gaps and software updates are then laid over that list.
"""

from __future__ import annotations

import datetime as dt

from ..util import DAY, HOUR, MIN, local_date, local_hour, local_ms, rng
from . import places as P
from . import routes as R
from .cars import outside_c
from .segments import Charge, Drive, Gap, Parked, Update


def trip(to, hour=None, at_ms=None, dwell=(4, 9), dc=None, ac=None, ac_target=None, ac_at_ms=None,
         stay=None, road=False, purpose=None):
    return {"to": to, "hour": hour, "at_ms": at_ms, "dwell": dwell, "dc": dc, "ac": ac, "ac_target": ac_target,
            "ac_at_ms": ac_at_ms, "stay": stay, "road": road, "purpose": purpose}


# ------------------------------------------------------------------ planners


def plan_c1(car, day, rel, rnd):
    sp = car.special
    rt0, rt1 = sp["road_trip"]
    h0, h1 = sp["holiday"]
    wd = day.weekday()
    if rel == 0:
        return [trip("p11", at_ms=car.now_ms - 12 * MIN, ac=(208, 30, 1), ac_target=90),
                trip("p1", hour=15.6)]
    if h0 <= rel <= h1:
        return []
    hotel_ac = (208, 40, 1)
    if rel == rt0:
        return [trip("p19", 7.5, road=True, dc=85), trip("p20", road=True, dc=85),
                trip("p21", road=True, dc=72), trip("p22", road=True, ac=hotel_ac, ac_target=90)]
    if rt0 < rel < rt1:
        k = rel - rt0
        if k == 1:
            return [trip("p23", 10.5), trip("p22", 15.0, ac=hotel_ac, ac_target=90)]
        if k == 2:
            return [trip("34.05220,-118.24370", 11.0), trip("p22", 16.5, ac=hotel_ac, ac_target=90)]
        return [trip("p23", 18.0), trip("p22", 21.5, ac=hotel_ac, ac_target=92)]
    if rel == rt1:
        return [trip("p21", 8.0, road=True, dc=80), trip("p20", road=True, dc=85),
                trip("p19", road=True, dc=70), trip("p1", road=True)]
    if rel == -200:
        return [trip("p18", 8.5), trip("p1", 15.2)]
    T = []
    if wd < 5:
        wfh = wd == 4 and rnd.random() < 0.4
        if not wfh:
            sentry = ("sentry",) if rnd.random() < car.sentry_at_work else None
            T.append(trip("p2", 8.05 + rnd.uniform(-0.25, 0.3), stay=sentry, purpose="commute"))
            if rnd.random() < 0.3:
                T += [trip("p4", 12.3 + rnd.uniform(0, 0.2)), trip("p2", 13.1, stay=sentry)]
            x = rnd.random()
            if x < 0.05:
                T += [trip("p3", 17.5, dc=80), trip("p1")]
            elif x < 0.28:
                dog = ("keep_climate_dog",) if rnd.random() < 0.3 else None
                T += [trip("p5", 17.5 + rnd.uniform(0, 0.3), stay=dog), trip("p1", dwell=(25, 50))]
            else:
                T.append(trip("p1", 17.7 + rnd.uniform(-0.3, 0.5), purpose="commute"))
            if rnd.random() < 0.1:
                T += [trip("p10", 19.6), trip("p1", 20.3)]
        elif rnd.random() < 0.5:
            T += [trip("p6", 11.5), trip("p1", 13.2)]
        return T
    outings = [("p6", 2.0), ("p7", 2.5), ("p8", 2.0), ("p9", 3.0), ("p11", 3.5), ("p12", 3.0), ("p13", 3.5),
               ("p14", 2.5), ("p10", 0.7), ("p15", 0.5)]
    x = rnd.random()
    n = 0 if x < 0.12 else 1 if x < 0.75 else 2
    h = 10.0 + rnd.uniform(0, 1.5)
    prev_dwell = None
    for i in range(n):
        pid, hours = rnd.choice(outings)
        kw = {}
        if pid == "p11":
            kw = {"ac": (208, 30, 1), "ac_target": 90}
        if pid == "p15":
            kw = {"dc": 85}
        if i == 0:
            T.append(trip(pid, h, **kw))
        else:
            T.append(trip(pid, dwell=prev_dwell, **kw))
        prev_dwell = (hours * 60 * 0.7, hours * 60 * 1.3)
    if n:
        T.append(trip("p1", dwell=prev_dwell))
    return T


def plan_c2(car, day, rel, rnd):
    sp = car.special
    rt0, rt1 = sp["road_trip"]
    wd = day.weekday()
    work_ac = (208, 32, 1)
    if rel == 0:
        return [trip("p31", 7.75, ac=work_ac, ac_target=100, ac_at_ms=car.now_ms - 45 * MIN, purpose="commute"),
                trip("p30", 17.25, purpose="commute")]
    hotel_ac = (208, 32, 1)
    if rel == rt0:
        return [trip("p37", 7.0, road=True, dc=80), trip("p38", road=True, dc=85), trip("p39", road=True, dc=95),
                trip("p40", road=True, dc=90), trip("p41", road=True, ac=hotel_ac, ac_target=90)]
    if rt0 < rel < rt1:
        k = rel - rt0
        if k % 2:
            return [trip("43.64260,-79.38710", 10.0), trip("p41", 16.0, ac=hotel_ac, ac_target=92)]
        return [trip("43.66770,-79.39480", 11.0), trip("p41", 15.0, ac=hotel_ac, ac_target=95)]
    if rel == rt1:
        return [trip("p40", 8.0, road=True, dc=90), trip("p39", road=True, dc=85), trip("p38", road=True, dc=80),
                trip("p37", road=True, dc=75), trip("p30", road=True)]
    T = []
    if wd < 5:
        ac = {"ac": work_ac, "ac_target": 100} if wd == 0 and rnd.random() < 0.7 else {}
        T.append(trip("p31", 7.75 + rnd.uniform(-0.2, 0.3), purpose="commute", **ac))
        if wd == 1:
            T += [trip("p33", 17.25 + rnd.uniform(0, 0.2)), trip("p30", 18.75)]
        elif wd == 2:
            T += [trip("p32", 17.25 + rnd.uniform(0, 0.2)), trip("p30", dwell=(20, 40))]
        else:
            T.append(trip("p30", 17.25 + rnd.uniform(-0.2, 0.6), purpose="commute"))
        return T
    if wd == 5:
        if rnd.random() < 0.8:
            T += [trip("p34", 10.5 + rnd.uniform(0, 1)), trip("p30", 15.0 + rnd.uniform(0, 1.5))]
        return T
    if rnd.random() < 0.55:
        dc = {"dc": 90} if rnd.random() < 0.3 else {}
        T += [trip("p36", 11.0 + rnd.uniform(0, 1), **dc), trip("p30", dwell=(60, 150))]
    return T


def plan_c3(car, day, rel, rnd):
    wd = day.weekday()
    T = []
    if wd in (0, 1, 3):
        T += [trip("p51", 8.5 + rnd.uniform(-0.3, 0.4), purpose="commute"),
              trip("p50", 17.5 + rnd.uniform(-0.3, 0.8), purpose="commute")]
    elif wd == 2:
        if rnd.random() < 0.5:
            T += [trip("p6", 10.0 + rnd.uniform(0, 1)), trip("p50", 14.0 + rnd.uniform(0, 1.5))]
    elif wd == 4:
        if rnd.random() < 0.6:
            T += [trip("p52", 18.0 + rnd.uniform(0, 0.5)), trip("p50", 21.0)]
    elif rnd.random() < 0.6:
        T += [trip("p17", 10.5 + rnd.uniform(0, 1), dc=85), trip("p14", dwell=(5, 10)), trip("p50", dwell=(60, 150))]
    return T


PLANNERS = {"c1": plan_c1, "c2": plan_c2, "c3": plan_c3}


# ------------------------------------------------------------------ the simulator


class Sim:
    def __init__(self, car, seed, now_ms, plan_end_ms):
        self.car = car
        self.seed = seed
        self.now_ms = now_ms
        self.plan_end = plan_end_ms
        self.tz = car.time_zone
        self.segs = []
        self.shadow = []  # teslamate copies of CAN rows where both exist
        self.dseq = 0
        self.cseq = 0
        self.useq = 0
        now_day = local_date(now_ms, self.tz)
        self.now_day = now_day
        self.start_day = now_day - dt.timedelta(days=car.history_days)
        self.t = local_ms(self.start_day, 0, self.tz)
        car.hist_start_ms = self.t
        self.loc = car.home
        p = P.BY_ID[car.home]
        self.lat, self.lon = p["lat"], p["lon"]
        self.soc = car.soc_start
        self.odo = car.odo_start_km
        self.batt = outside_c(car, self.t)
        self.ready = self.t
        self.intent = None
        self.stay_ctx = None
        sp = car.special
        self.can_from = local_ms(now_day + dt.timedelta(days=sp["can_from"]), 0, self.tz) if "can_from" in sp else None
        self.tm_until = (local_ms(now_day + dt.timedelta(days=sp["teslamate_until"]), 0, self.tz)
                         if "teslamate_until" in sp else None)

    def source(self, t):
        if self.can_from is not None and t < self.can_from:
            return "teslamate"
        return "can"

    def rnd(self, *parts):
        return rng(self.seed, self.car.id, *parts)

    # --- running the plan
    def run(self):
        planner = PLANNERS[self.car.id]
        day = self.start_day
        stop = False
        while not stop and self.t < self.plan_end:
            rel = (day - self.now_day).days
            rnd = self.rnd("day", day.isoformat())
            for tr in planner(self.car, day, rel, rnd):
                if tr["at_ms"] is not None:
                    dep = tr["at_ms"]
                elif tr["hour"] is None:
                    lo, hi = tr["dwell"]
                    dep = self.ready + int(rnd.uniform(lo, hi) * MIN)
                else:
                    dep = local_ms(day, tr["hour"], self.tz)
                dep = max(dep, self.ready + 2 * MIN, self.t + 2 * MIN)
                if dep >= self.plan_end:
                    stop = True
                    break
                self.stay(dep, rel)
                self.maybe_detour(tr, rnd, rel)
                self.drive(tr, rnd)
            day += dt.timedelta(days=1)
            if local_ms(day, 0, self.tz) >= self.plan_end:
                break
        self.stay(self.plan_end, (self.now_day - self.now_day).days + 2)
        self.overlay()
        return self

    def loc_key(self):
        return self.loc

    def maybe_detour(self, tr, rnd, rel):
        """Go to a fast charger first when the battery would get too low."""
        route = R.between(self.loc_key(), tr["to"], tr["road"])
        need = route.length / 1000 * self.car.base_wh_per_km * 1.35 / 1000 / self.car.usable_kwh(self.t) * 100
        if self.soc - need >= 12 or tr["dc"]:
            return
        here = (self.lat, self.lon)
        chargers = [p for p in P.PLACES if p["kind"] == "charger" and p["time_zone"] == self.car.time_zone]
        best = min(chargers, key=lambda p: (p["lat"] - here[0]) ** 2 + (p["lon"] - here[1]) ** 2)
        if best["id"] == self.loc:
            return
        self.drive(trip(best["id"], dc=85), rnd)
        self.stay(self.ready + int(rnd.uniform(3, 7) * MIN), rel)

    def drive(self, tr, rnd):
        car = self.car
        start = self.t
        src = self.source(start)
        to = tr["to"]
        to_pid = to if to in P.BY_ID else None
        self.dseq += 1
        d = Drive(car, self.dseq, start, self.loc_key(), to, self.soc, self.odo, self.batt,
                  self.rnd("drive", self.dseq), source=src, road=tr["road"], purpose=tr["purpose"],
                  to_pid=to_pid, from_pid=self.loc if self.loc in P.BY_ID else None)
        if self.car.id == "c1" and abs(d.start - (car.now_ms - 12 * MIN)) < 1000:
            car.live_drive = d
        self.segs.append(d)
        self.t = d.end
        self.soc = d.soc1_full
        self.odo += d.length_m / 1000
        self.batt = d.batt1_full
        self.loc = to
        lat, lon, _, _ = d.route.at(d.length_m)
        self.lat, self.lon = lat, lon
        self.ready = d.end
        self.intent = None
        self.stay_ctx = tr["stay"]
        if tr["dc"]:
            self.cseq += 1
            pl = P.BY_ID.get(to_pid) if to_pid else None
            dc_kw = 250 if pl and "Supercharger" in pl["name"] else 150
            ch = Charge(car, self.cseq, d.end, to_pid, "dc", self.soc, tr["dc"], None, self.batt,
                        self.rnd("charge", self.cseq), source=src, dc_kw=dc_kw, lat=lat, lon=lon)
            ch.odo = self.odo
            self.intent = ("dc", ch)
            self.ready = ch.end
        elif tr["ac"]:
            self.intent = ("ac", tr)

    def stay(self, until, rel):
        t0, t1 = self.t, until
        if t1 <= t0:
            return
        car = self.car
        src = self.source(t0)
        charge = None
        intent = self.intent
        if intent and intent[0] == "dc":
            charge = intent[1]
        elif intent and intent[0] == "ac":
            tr = intent[1]
            cs = tr["ac_at_ms"] if tr["ac_at_ms"] else t0 + 4 * MIN
            cs = max(cs, t0 + 2 * MIN)
            if cs < t1 - 15 * MIN:
                self.cseq += 1
                charge = Charge(car, self.cseq, cs, self.loc if self.loc in P.BY_ID else None, "ac", self.soc,
                                tr["ac_target"] or car.charge_limit, t1 - 4 * MIN, self.batt,
                                self.rnd("charge", self.cseq), source=src, ac=tr["ac"], lat=self.lat, lon=self.lon)
        elif self.loc == car.home and t1 - t0 > 90 * MIN:
            charge = self.home_charge(t0, t1, rel, src)
        if charge is not None:
            charge.odo = self.odo
            pre_plugged = charge.ckind == "ac"
            if charge.start - t0 >= MIN:
                self.parked(t0, charge.start, rel, {"plugged": "before" if pre_plugged else None, "charge": charge})
                charge.plug_start = t0 + 2 * MIN if pre_plugged else charge.start
            if self.car.id == "c2" and charge.start <= car.now_ms < charge.end and charge.ckind == "ac":
                car.live_charge = charge
            self.segs.append(charge)
            self.soc = charge.soc1
            self.batt = charge.batt1
            self.t = charge.end
            if t1 > charge.end:
                charge.plug_end = t1 - 2 * MIN if pre_plugged else charge.end + 60_000
                self.parked(charge.end, t1, rel, {"plugged": "after" if pre_plugged else None, "charge": charge})
        else:
            self.parked(t0, t1, rel, {})
        self.t = t1
        self.intent = None
        self.stay_ctx = None

    def home_charge(self, t0, t1, rel, src):
        car = self.car
        rel = (local_date(t0, self.tz) - self.now_day).days
        nh = car.special.get("no_home_charge", (-3, -1) if car.id == "c2" else None)
        if nh and nh[0] <= rel <= nh[1]:
            return None
        rnd = self.rnd("home", t0)
        rt = car.special.get("road_trip")
        target = car.charge_limit
        before_trip = bool(rt and rel == rt[0] - 1)
        if before_trip:
            target = 100
        thresholds = {"c1": 58, "c2": 55, "c3": 50}
        if not (self.soc < thresholds[car.id] or rnd.random() < 0.06 or before_trip):
            return None
        if self.soc >= target - 2:
            return None
        cs = t0 + 5 * MIN
        if car.scheduled_charging:
            d0 = local_date(t0, self.tz)
            for k in (0, 1):
                sched = local_ms(d0 + dt.timedelta(days=k), 23.5, self.tz)
                if t0 + 5 * MIN <= sched < t1 - 60 * MIN:
                    cs = sched
                    break
        if cs >= t1 - 30 * MIN:
            return None
        self.cseq += 1
        return Charge(car, self.cseq, cs, car.home, "ac", self.soc, target, t1 - 4 * MIN, self.batt,
                      self.rnd("charge", self.cseq), source=src, ac=car.home_ac)

    def parked(self, t0, t1, rel, ctx):
        car = self.car
        ctx = dict(ctx)
        if self.stay_ctx:
            ctx["stay_awake"] = self.stay_ctx
        out1 = outside_c(car, t1, self.lat, self.lon)
        hr = local_hour(t1, self.tz)
        if self.loc == car.home and 5.5 <= hr <= 10.5 and out1 < 8 and car.profile != "tesla-sx-legacy":
            ctx["precondition"] = True
        ctx["cold"] = outside_c(car, (t0 + t1) // 2, self.lat, self.lon) < 0
        p = Parked(car, t0, t1, self.loc if self.loc in P.BY_ID else None, self.lat, self.lon, self.soc,
                   self.batt, ctx, self.rnd("parked", t0), source=self.source(t0))
        p.odo = self.odo
        self.segs.append(p)
        self.soc = p.soc_at(t1)
        self.batt = p.batt_at(t1)

    # --- laying gaps and updates over the history
    def overlay(self):
        car = self.car
        segs = self.segs
        for rel, version, vsrc, detected in car.updates:
            day = self.now_day + dt.timedelta(days=rel)
            t = local_ms(day, 2.5, self.tz)
            self.useq += 1
            segs = _insert_update(car, segs, t, self.useq, version, vsrc, detected,
                                  self.source(t), self.rnd("update", rel))
        for rel, hour, minutes, reason, mode in car.gaps:
            day = self.now_day + dt.timedelta(days=rel)
            g0 = local_ms(day, hour, self.tz)
            segs = _apply_gap(car, segs, g0, minutes * MIN, reason, mode, self.rnd("gap", rel))
        sp = car.special
        if "silent_from_h" in sp:
            g0 = car.now_ms - int(sp["silent_from_h"] * HOUR)
            segs = _apply_tail(car, segs, g0, self.plan_end)
        # drops inside a few drives (dropped frames) and gaps inside a few charges
        r = self.rnd("drops")
        for s in segs:
            if s.kind == "drive" and s.source == "can" and s.data_end - s.start > 10 * MIN and r.random() < 0.035:
                a = s.start + int((s.data_end - s.start) * r.uniform(0.2, 0.8))
                s.drops.append((a, a + int(r.uniform(4, 25) * 1000)))
            if s.kind == "charge" and s.source == "can" and s.data_end - s.start > 30 * MIN and r.random() < 0.03:
                a = s.start + int((s.data_end - s.start) * r.uniform(0.2, 0.8))
                kind = "dropped" if r.random() < 0.6 else "chain"
                s.gaps.append((a, a + int(r.uniform(40, 300 if kind == "chain" else 60) * 1000), kind))
        if self.can_from is not None and self.tm_until is not None:
            for s in segs:
                if s.kind in ("drive", "charge") and s.source == "can" and self.can_from <= s.start < self.tm_until:
                    self.shadow.append(_teslamate_copy(s, self.rnd("tm", s.id)))
        self.segs = segs


def _containing(segs, t):
    for i, s in enumerate(segs):
        if s.start <= t < s.end:
            return i
    return None


def _parked_room(segs, t, need_ms, margin=10 * MIN):
    """Index of a parked spell holding [t, t+need]; else the next spell after t that is long enough."""
    i = _containing(segs, t)
    if i is not None and segs[i].kind == "parked" and t >= segs[i].start + 2 * MIN and t + need_ms <= segs[i].end - 2 * MIN:
        return i, t
    for j in range(i or 0, len(segs)):
        s = segs[j]
        if s.kind == "parked" and s.start >= t - DAY and s.end - s.start >= need_ms + 2 * margin and s.start + margin >= t - 1:
            return j, s.start + margin
    return None, None


def _split_parked(segs, i, a, b, middle):
    p = segs[i]
    out = []
    if a > p.start:
        out.append(p.clip(p.start, a))
    out.extend(middle)
    if b < p.end:
        out.append(p.clip(b, p.end))
    return segs[:i] + out + segs[i + 1:]


def _insert_update(car, segs, t, seq, version, vsrc, detected, source, rnd):
    dur = int(rnd.uniform(24, 41) * MIN)
    i, t = _parked_room(segs, t, dur)
    if i is None:
        return segs
    p = segs[i]
    u = Update(car, seq, t, t + dur, version, vsrc, detected, p.lat, p.lon, p.pid, p.soc_at(t), source=source)
    return _split_parked(segs, i, t, t + dur, [u])


def _pos_odo(s, t):
    """(lat, lon), odometer at t inside a piece."""
    if s.kind == "drive":
        dist, _, _ = s.kin(t)
        la, lo, _, _ = s.route.at(dist)
        return (la, lo), s.odo0 + dist / 1000
    return (s.lat, s.lon), getattr(s, "odo", None)


def _apply_gap(car, segs, g0, dur, reason, mode, rnd):
    if mode == "parked":
        i, g0 = _parked_room(segs, g0, dur)
        if i is None:
            return segs
        p = segs[i]
        gap = Gap(car, g0, g0 + dur, reason, before=(p.lat, p.lon), after=(p.lat, p.lon), odo_jump=0.0)
        return _split_parked(segs, i, g0, g0 + dur, [gap])
    # the gap starts in a parked spell ("moved"), mid-drive or mid-charge
    if mode == "moved":
        i, g0 = _parked_room(segs, g0, 0, margin=5 * MIN)
        if i is None:
            return segs
    else:
        want = "drive" if mode == "drive_cut" else "charge"
        i = None
        for j, s in enumerate(segs):
            if s.kind == want and s.start >= g0 and s.end - s.start > (8 * MIN if want == "drive" else 60 * MIN):
                i = j
                break
        if i is None:
            return segs
        s = segs[i]
        g0 = s.start + int((s.end - s.start) * (0.6 if want == "drive" else 0.55))
    g1 = g0 + dur
    j, g1 = _parked_room(segs, g1, 0, margin=5 * MIN)
    if j is None:
        return segs
    first = segs[i]
    last = segs[j]
    before, odo_a = _pos_odo(first, g0)
    after, odo_b = (last.lat, last.lon), getattr(last, "odo", None)
    jump = (odo_b - odo_a) if odo_a is not None and odo_b is not None else 0.0
    head = segs[:i]
    if first.kind == "parked":
        if g0 > first.start:
            head.append(first.clip(first.start, g0))
    else:
        first.cut_ms = g0
        first.end = g0
        if first.kind == "charge":
            first.soc1 = first.soc_at(g0)
        head.append(first)
    gap = Gap(car, g0, g1, reason, before=before, after=after, odo_jump=max(0.0, jump))
    tail = [last.clip(g1, last.end)] if g1 < last.end else []
    return head + [gap] + tail + segs[j + 1:]


def _apply_tail(car, segs, g0, plan_end):
    """The car stopped uploading at g0 (the last log failed its check, then nothing more)."""
    i, g0 = _parked_room(segs, g0, 0, margin=5 * MIN)
    if i is None:
        return segs
    p = segs[i]
    head = segs[:i] + ([p.clip(p.start, g0)] if g0 > p.start else [])
    g1 = g0 + HOUR
    return head + [Gap(car, g0, g1, "failed_check", before=(p.lat, p.lon), after=(p.lat, p.lon)),
                   Gap(car, g1, plan_end, "no_log", before=(p.lat, p.lon), after=None, open_end=True)]


def _teslamate_copy(s, rnd):
    import copy
    c = copy.copy(s)
    c.source = "teslamate"
    c.drops = [] if s.kind == "drive" else None
    shift = int(rnd.uniform(-40, 40) * 1000)
    c.start = s.start + shift
    c.end = s.end + shift + int(rnd.uniform(-20, 60) * 1000)
    if s.kind == "drive":
        c.id = str(int(s.id) + 500_000)
        c.cut_ms = None if s.cut_ms is None else s.cut_ms + shift
        c.planned_end = c.end
        c.energy_full = s.energy_full * rnd.uniform(0.93, 1.07)
        c._pw = None
    else:
        c.id = str(int(s.id) + 500_000)
        c.gaps = []
        c.cut_ms = None
    c.tm_copy_of = s.id
    return c
