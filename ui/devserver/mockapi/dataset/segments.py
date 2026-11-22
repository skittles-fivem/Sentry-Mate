"""The pieces a car's history is made of: drives, charges, parked spells, gaps and software updates.

Each piece is generated from its own seed and knows how to give its values at any moment
(`values(t)`), so series at 1 Hz are computed on demand and never stored. A car's history is one
list of pieces that touch end to end.

Signs: `power_kw` is the pack's power, positive out of the pack (driving, heating) and negative
into it (charging, regenerating). Charge figures (`power_max_kw`, the curve's `power_kw`) are the
charging power, positive.
"""

from __future__ import annotations

import bisect
import hashlib
import math

from ..sources import inputs_for
from ..util import HOUR, MIN, clamp, h01, haversine_m
from . import places as P
from . import routes as R
from .cars import outside_c

SERIES_FIELDS = (
    "soc_pct", "usable_soc_pct", "soc_bms_pct", "range_km", "ideal_range_km", "energy_remaining_kwh",
    "speed_kph", "power_kw", "odometer_km", "outside_c", "cabin_c", "battery_c", "setpoint_left_c",
    "setpoint_right_c", "elevation_m", "tyre_fl_kpa", "tyre_fr_kpa", "tyre_rl_kpa", "tyre_rr_kpa",
    "lat", "lon", "accuracy_m", "heading_deg", "lv_v", "battery_heat_kw", "climate", "fan_level",
)
CURVE_FIELDS = (
    "power_kw", "volts_v", "amps_a", "soc_pct", "usable_soc_pct", "battery_c", "range_km", "outside_c",
    "phases", "pilot_a", "ac_line_kw", "heater_kw", "dc_volts_v", "dc_amps_a",
    "ac_volts_v", "ac_amps_a", "limit_source",
)

ID_BASE = 2 ** 53 + 1


def drive_id(car_n, seq):
    return str(ID_BASE + car_n * 10 ** 9 + seq)


def charge_id(car_n, seq):
    return str(ID_BASE + 10 ** 12 + car_n * 10 ** 9 + seq)


def update_id(car_n, seq):
    return str(ID_BASE + 2 * 10 ** 12 + car_n * 10 ** 9 + seq)


# ------------------------------------------------------------------ shared value helpers


def battery_values(car, t, soc, battery_c):
    usable = car.usable_kwh(t)
    full = usable * 1000 / car.rated_wh_per_km
    rng_km = soc / 100 * full
    cold = 0.0 if battery_c is None else clamp((6 - battery_c) * 0.35, 0, 6)
    return {
        "soc_pct": round(soc, 2),
        "usable_soc_pct": round(max(0.0, soc - cold), 2),
        "soc_bms_pct": round(min(100.0, soc + 0.8), 2),
        "range_km": round(rng_km, 2),
        "ideal_range_km": round(rng_km * car.ideal_factor, 2),
        "energy_remaining_kwh": round(usable * soc / 100, 3),
    }


def tyres(car, t, outside, warm):
    """[fl, fr, rl, rr] in kPa, or None when the car has no mapped tyre sensors."""
    if car.tyre_base is None:
        return None
    days = (t - car.hist_start_ms) / 86_400_000
    since_topup = days % 60
    out = []
    for i, base in enumerate(car.tyre_base):
        p = base + 3 - 0.12 * since_topup
        leak = car.special.get("leak_start")
        if leak is not None and i == 2:
            t_leak = car.now_ms + leak * 86_400_000
            if t > t_leak:
                p -= 2.0 * (t - t_leak) / 86_400_000
        p *= (273.15 + outside) / 293.15
        p *= 1 + 0.07 * warm
        out.append(round(p, 1))
    return out


def lv_volts(car, awake):
    if car.profile == "tesla-sx-legacy":
        return 13.9 if awake else 12.6
    return 15.6 if awake else 15.1


def nearest_place(lat, lon):
    best, bd = None, 1e18
    for p in P.PLACES:
        d = haversine_m(lat, lon, p["lat"], p["lon"])
        if d < bd:
            best, bd = p, d
    return best, bd


def match_place(lat, lon):
    """The place a point is in, by the nearest-centre rule among the circles that contain it."""
    best, bd = None, 1e18
    for p in P.PLACES:
        d = haversine_m(lat, lon, p["lat"], p["lon"])
        if d <= p["radius_m"] and d < bd:
            best, bd = p, d
    return best


def address_at(lat, lon):
    """A made-up offline reverse geocode: the place's address, else 'near' the closest one."""
    pl = match_place(lat, lon)
    if pl is not None:
        return dict(pl["address"])
    near, d = nearest_place(lat, lon)
    a = dict(near["address"])
    if d <= 1_200:
        a.update(short=f"near {a['street']}, {a['neighbourhood'] or a['locality']}")
    elif d <= 8_000:  # same town, but too far to name the street
        a.update(short=a["locality"] or a["region"], street=None, neighbourhood=None)
    elif d < 80_000:
        a.update(short=f"{a['locality']} area" if a["locality"] else a["region"], street=None, neighbourhood=None)
    else:
        a.update(short=a["region"], street=None, neighbourhood=None, locality=None)
    return a


def point(lat, lon, pid=None, label=None):
    pl = P.BY_ID.get(pid) if pid else match_place(lat, lon)
    addr = dict(pl["address"]) if pl else address_at(lat, lon)
    return {
        "lat": round(lat, 6), "lon": round(lon, 6),
        "place": {"id": pl["id"], "name": pl["name"]} if pl else None,
        "address": {k: addr[k] for k in ("short", "locality", "region", "country_code")},
        "label": label or (pl["name"] if pl else addr["short"]),
    }


def _odo(s):
    """The odometer while standing still (the car sends it while awake); None when not known."""
    o = getattr(s, "odo", None)
    return None if o is None else round(o, 3)


# ------------------------------------------------------------------ base


class Seg:
    kind = "seg"
    open_end = False  # the end is only planned (an open gap)

    def __init__(self, car, start, end, source="can"):
        self.car = car
        self.start = int(start)
        self.end = int(end)
        self.source = source
        # the live inputs that saw it (empty for an imported piece), see mockapi/sources.py
        self.inputs = inputs_for(car, self.start, getattr(car, "now_day_index", None)) if source == "can" else []

    def state_at(self, t):
        return "unknown"

    def values(self, t):
        return None

    def native_step_ms(self, t):
        return None

    def peak_times(self):
        return ()

    def activity(self):
        return [("unknown", self.start, self.end)]

    def awake_spans(self):
        return []


# ------------------------------------------------------------------ drives


class Drive(Seg):
    kind = "drive"

    def __init__(self, car, seq, start, from_key, to_key, soc0, odo0, batt0, rnd, source="can",
                 road=False, purpose=None, to_pid=None, from_pid=None):
        self.seq = seq
        self.id = drive_id(car.n, seq)
        self.route = R.between(from_key, to_key, road)
        self.from_pid = from_pid
        self.to_pid = to_pid
        self.road = road
        L = self.route.length
        self.kt, self.kv, self.ks = _knots(L, road, rnd)
        dur_s = self.kt[-1]
        super().__init__(car, start, start + dur_s * 1000, source)
        self.planned_end = self.end
        self.length_m = L
        self.cut_ms = None  # data stopped here (a gap starts)
        self.drops = []  # (t0, t1) dropped frames inside the drive
        self.purpose = purpose
        mid = start + dur_s * 500
        la, lo, _, _ = self.route.at(L / 2)
        self.outside = outside_c(car, mid, la, lo)
        v_avg = (L / 1000) / max(dur_s / 3600, 1e-6)
        out = self.outside
        whkm = car.base_wh_per_km * (0.90 + 0.00003 * v_avg * v_avg)
        whkm *= 1 + 0.018 * max(0.0, 15 - out) + 0.01 * max(0.0, out - 28)
        if out < 10:
            whkm *= 1 + 0.25 * math.exp(-dur_s / 600)
        whkm *= 1 + (rnd.random() - 0.5) * 0.12
        climb_kwh = car.mass_kg * 9.81 * (self.route.climb_m - 0.65 * self.route.descent_m) / 3.6e6
        self.climb_kwh = climb_kwh
        energy = max(0.06 * L / 1000, L / 1000 * whkm / 1000 + climb_kwh)
        self.energy_full = energy
        self.aux_kw = 0.35 + 0.12 * max(0.0, 18 - out) + 0.06 * max(0.0, out - 24)
        self.hvac_full = self.aux_kw * dur_s / 3600
        self.regen_share = (0.04 if v_avg > 70 else 0.13) * (1 + self.route.descent_m / 300)
        usable = car.usable_kwh(start)
        self.soc0 = soc0
        self.soc1_full = soc0 - energy / usable * 100
        self.odo0 = odo0
        self.batt0 = batt0
        self.batt1_full = batt0 + (min(32.0, batt0 + 2 + dur_s / 400) - batt0) * 0.8
        self.cabin0 = out + 4
        self.reduced_range = out < -5 and batt0 < 2
        self.setpoint = 21.0 if h01("sp", car.id, start // 86_400_000) > 0.1 else 22.5
        # climate off on some mild drives; the pack heats itself on cold starts
        self.climate_on = not (12 <= out <= 24 and h01("ac", car.id, seq) < 0.3)
        # on some warm or chilly starts the driver sets full cooling (LO) or full heat (HI) for the first
        # minutes, and the cabin was still preconditioning as the car moved off
        self.max_mode = (("LO" if out >= 17 else "HI" if out <= 13 else None)
                         if self.climate_on and h01("mx", car.id, seq) < 0.3 else None)
        self._pw = None
        self._ps = None

    # --- kinematics at time t
    def kin(self, t):
        tau = (t - self.start) / 1000
        kt = self.kt
        if tau <= 0:
            return 0.0, 0.0, 0.0
        if tau >= kt[-1]:
            return self.length_m, 0.0, 0.0
        k = bisect.bisect_right(kt, tau) - 1
        dt = kt[k + 1] - kt[k]
        a = (self.kv[k + 1] - self.kv[k]) / dt if dt > 0 else 0.0
        u = tau - kt[k]
        v = self.kv[k] + a * u
        s = self.ks[k] + self.kv[k] * u + a * u * u / 2
        return s, max(0.0, v), a

    def frac_at(self, t):
        return self.kin(t)[0] / self.length_m

    def soc_at(self, t):
        return self.soc0 - (self.soc0 - self.soc1_full) * self.frac_at(t)

    def power(self, v, a, s):
        car = self.car
        if s < self.length_m - 25:
            e1 = self.route.elevation(s)
            e2 = self.route.elevation(s + 20)
            grade = (e2 - e1) / 20
        else:
            grade = 0.0
        f = car.mass_kg * a + car.mass_kg * 9.81 * car.crr * (1 if v > 0.1 else 0) + 0.6 * car.cda * v * v
        f += car.mass_kg * 9.81 * grade
        p = f * v
        p = p / 0.88 if p >= 0 else max(-75000.0, p * 0.72)
        return p / 1000 + self.aux_kw

    def _power_scale(self):
        """(k_use, k_regen): factors on the modelled power so that its 1 Hz readings add up to the drive's own
        figures (net `energy_full`, regen `energy_full * regen_share`). Worked out once, when first needed, from
        midpoints about 3 s apart in each stretch of steady acceleration (within a few % of the 1 Hz sum)."""
        if self._ps is None:
            use = back = 0.0
            kt, kv, ks = self.kt, self.kv, self.ks
            for k in range(len(kt) - 1):
                dt = kt[k + 1] - kt[k]
                if dt <= 0:
                    continue
                a = (kv[k + 1] - kv[k]) / dt
                n = max(1, math.ceil(dt / 3))
                step = dt / n
                for j in range(n):
                    u = (j + 0.5) * step
                    e = self.power(max(0.0, kv[k] + a * u), a, ks[k] + kv[k] * u + a * u * u / 2) * step / 3600
                    if e >= 0:
                        use += e
                    else:
                        back -= e
            regen = self.energy_full * self.regen_share if back > 0 else 0.0
            self._ps = ((self.energy_full + regen) / use if use > 0 else 1.0, regen / back if back > 0 else 1.0)
        return self._ps

    def scaled_power(self, v, a, s):
        """The power reading (kW) at speed v, acceleration a and distance s, in step with the drive's figures."""
        p = self.power(v, a, s)
        ku, kr = self._power_scale()
        return p * ku if p >= 0 else p * kr

    def _power_peaks(self):
        if self._pw is None:
            best = (-1e9, None)
            worst = (1e9, None)
            kt, kv, ks = self.kt, self.kv, self.ks
            for k in range(len(kt) - 1):
                dt = kt[k + 1] - kt[k]
                if dt <= 0:
                    continue
                a = (kv[k + 1] - kv[k]) / dt
                if a > 0:
                    p = self.power(kv[k + 1], a, ks[k + 1])
                    if p > best[0]:
                        best = (p, self.start + int((kt[k + 1] - 0.001) * 1000))
                elif a < 0:
                    p = self.power(kv[k], a, ks[k])
                    if p < worst[0]:
                        worst = (p, self.start + int((kt[k] + 0.001) * 1000))
            if best[1] is None:
                best = (self.aux_kw, self.start)
            if worst[1] is None:
                worst = (self.aux_kw, self.start)
            self._pw = (best, worst)
        return self._pw

    @property
    def data_end(self):
        return self.cut_ms if self.cut_ms is not None else self.end

    def in_drop(self, t):
        for a, b in self.drops:
            if a <= t < b:
                return True
        return False

    def state_at(self, t):
        if self.in_drop(t):
            return "unknown"
        return "driving"

    def native_step_ms(self, t):
        return 1000 if self.source == "can" else 5000

    def peak_times(self):
        (pmax, tmax), (pmin, tmin) = self._power_peaks()
        k = max(range(len(self.kv)), key=lambda i: self.kv[i])
        return (self.start + int(self.kt[k] * 1000), tmax, tmin)

    def position(self, t):
        """(lat, lon, accuracy_m) at t, exactly as values(t) has them, without the rest (for long tracks)."""
        if t < self.start or t > self.data_end or self.in_drop(t):
            return None
        lat, lon = self.route.latlon(self.kin(t)[0])
        tau = (t - self.start) / 1000
        return round(lat, 6), round(lon, 6), round(4 + 2.5 * (1 + math.sin(tau / 37)), 1)

    def values(self, t):
        if t < self.start or t > self.data_end or self.in_drop(t):
            return None
        car = self.car
        s, v, a = self.kin(t)
        lat, lon, ele, heading = self.route.at(s)
        frac = s / self.length_m
        tau = (t - self.start) / 1000
        batt = self.batt0 + (self.batt1_full - self.batt0) * frac
        soc = self.soc0 - (self.soc0 - self.soc1_full) * frac
        out = self.outside + 0.8 * math.sin(tau / 900)
        vals = battery_values(car, t, soc, batt)
        cabin = self.cabin0 + (self.setpoint - self.cabin0) * (1 - math.exp(-tau / 300))
        legacy = car.profile == "tesla-sx-legacy"
        tm = self.source == "teslamate"
        ty = None if tm else tyres(car, t, out, min(1.0, tau / 900))
        vals.update({
            "speed_kph": round(v * 3.6, 2),
            "power_kw": round(self.scaled_power(v, a, s), 2),
            "odometer_km": round(self.odo0 + s / 1000, 3),
            "outside_c": round(out, 1),
            "cabin_c": None if legacy and not tm else round(cabin, 1),
            "battery_c": None if tm else round(batt, 1),
            "setpoint_left_c": None if legacy or tm else (self.max_mode if self.max_mode and tau < 300 else self.setpoint),
            "setpoint_right_c": None if legacy or tm else self.setpoint,
            "elevation_m": round(ele, 1),
            "tyre_fl_kpa": ty[0] if ty else None, "tyre_fr_kpa": ty[1] if ty else None,
            "tyre_rl_kpa": ty[2] if ty else None, "tyre_rr_kpa": ty[3] if ty else None,
            "lat": round(lat, 6), "lon": round(lon, 6),
            "accuracy_m": round(4 + 2.5 * (1 + math.sin(tau / 37)), 1),
            "heading_deg": round(heading, 1) if not legacy else None,
            "lv_v": None if tm else lv_volts(car, True),
            "battery_heat_kw": None if tm or legacy else round(
                4.5 * clamp((15 - batt) / 5, 0.3, 1.0) if batt < 15 and self.outside < 10 else 0.0, 2),
            "climate": None if tm or legacy else (
                "precondition" if self.max_mode and tau < 90 else "on" if self.climate_on else "off"),
            "fan_level": None if tm or legacy else (
                int(clamp(round(1 + abs(cabin - self.setpoint) * 0.6), 1, 10)) if self.climate_on else 0),
        })
        return vals

    def activity(self):
        out = []
        t = self.start
        for a, b in sorted(self.drops):
            if a > t:
                out.append(("driving", t, a))
            out.append(("unknown", a, b))
            t = b
        out.append(("driving", t, self.data_end))
        return out

    def awake_spans(self):
        return []

    # --- summary figures at a moment (`upto` for a drive still going)
    def figures(self, upto=None):
        end = self.data_end if upto is None else min(upto, self.data_end)
        s, _, _ = self.kin(end)
        frac = s / self.length_m
        dur_s = (end - self.start) / 1000
        car = self.car
        energy = self.energy_full * frac
        dist = s / 1000
        lat0, lon0, _, _ = self.route.at(0)
        lat1, lon1, _, _ = self.route.at(s)
        soc1 = self.soc0 - (self.soc0 - self.soc1_full) * frac
        full0 = car.full_range_km(self.start)
        moving = dur_s - self._stopped_s(end)
        gap_ms = sum(min(b, end) - a for a, b in self.drops if a < end)
        if self.cut_ms is not None:
            gap_ms += self.planned_end - self.cut_ms
        (pmax, tmax), (pmin, tmin) = self._power_peaks()
        ku, kr = self._power_scale()
        pmax = pmax * ku if pmax >= 0 else pmax * kr
        pmin = pmin * ku if pmin >= 0 else pmin * kr
        vmax = 0.0
        for k in range(len(self.kt)):
            if self.start + self.kt[k] * 1000 <= end:
                vmax = max(vmax, self.kv[k])
        _, vnow, _ = self.kin(end)
        vmax = max(vmax, vnow)
        return {
            "dist_km": dist, "dur_s": dur_s, "moving_s": moving, "energy": energy, "soc1": soc1,
            "range0": self.soc0 / 100 * full0, "range1": soc1 / 100 * car.full_range_km(end),
            "end_pt": (lat1, lon1), "start_pt": (lat0, lon0), "pmax": pmax, "pmin": pmin, "vmax": vmax * 3.6,
            "climb": self.route.climb_m * frac, "descent": self.route.descent_m * frac,
            "regen": energy * self.regen_share, "hvac": self.hvac_full * frac, "gap_ms": gap_ms,
            "batt1": self.batt0 + (self.batt1_full - self.batt0) * frac, "frac": frac,
        }

    def _stopped_s(self, end):
        tot = 0.0
        kt, kv = self.kt, self.kv
        for k in range(len(kt) - 1):
            if kv[k] == 0 and kv[k + 1] == 0:
                a = self.start + kt[k] * 1000
                b = min(end, self.start + kt[k + 1] * 1000)
                if b > a:
                    tot += (b - a) / 1000
        return tot

    def speed_bands(self, upto=None, width=10):
        end = self.data_end if upto is None else min(upto, self.data_end)
        lim = (end - self.start) / 1000
        bands = {}
        kt, kv = self.kt, self.kv
        for k in range(len(kt) - 1):
            t0, t1 = kt[k], min(kt[k + 1], lim)
            if t0 >= lim:
                break
            if t1 <= t0:
                continue
            v0 = kv[k] * 3.6
            v1full = kv[k + 1] * 3.6
            v1 = v0 + (v1full - v0) * (t1 - t0) / (kt[k + 1] - t0) if kt[k + 1] > t0 else v0
            dt = t1 - t0
            if abs(v1 - v0) < 1e-9:
                b = int(v0 // width)
                bands[b] = bands.get(b, 0.0) + dt
                continue
            lo, hi = min(v0, v1), max(v0, v1)
            for b in range(int(lo // width), int(hi // width) + 1):
                a_, b_ = max(lo, b * width), min(hi, (b + 1) * width)
                if b_ > a_:
                    bands[b] = bands.get(b, 0.0) + dt * (b_ - a_) / (hi - lo)
        return bands


def _knots(L, road, rnd):
    """A piecewise-linear speed profile (time s, speed m/s, distance m) that covers exactly L metres."""
    kt, kv, ks = [0.0], [0.0], [0.0]
    st = {"t": 0.0, "s": 0.0, "v": 0.0}

    def add(dt, v1):
        st["s"] += (st["v"] + v1) / 2 * dt
        st["t"] += dt
        st["v"] = v1
        kt.append(st["t"])
        kv.append(v1)
        ks.append(st["s"])

    B = 1.6  # a comfortable braking rate, m/s²
    long_trip = road or L > 12_000
    guard = 0
    while True:
        guard += 1
        s, v = st["s"], st["v"]
        rem = L - s
        hwy = long_trip and 2500 < s < L - 2500
        target = rnd.uniform(27.5, 32.5) if hwy and road else rnd.uniform(24.0, 29.5) if hwy else rnd.choice(
            (8.3, 10.0, 11.1, 12.5, 13.9, 15.3))
        a = rnd.uniform(1.1, 2.2)
        if target > v:
            d_acc = (target * target - v * v) / (2 * a)
            dt_acc = (target - v) / a
        else:
            d_acc = (v * v - target * target) / (2 * B)
            dt_acc = (v - target) / B
        if guard > 4000 or rem < 40 or s + d_acc + target * target / (2 * B) > L:
            # wind down: brake to a stop exactly at L
            if v < 0.5:
                vv = min(target, math.sqrt(max(1.0, rem) * a * B / (a + B)))
                d1 = vv * vv / (2 * a)
                if d1 >= rem:
                    vv = math.sqrt(max(0.5, rem) * a)
                    d1 = rem / 2
                add(vv / a if a > 0 else 1.0, vv)
                add(2 * (L - st["s"]) / vv, 0.0)
            else:
                add(2 * rem / v, 0.0)
            break
        add(dt_acc, target)
        cruise = rnd.uniform(3000, 18000) if hwy else rnd.uniform(120, 650)
        pieces = max(1, int(cruise / (2500 if hwy else 200)))
        for _ in range(pieces):
            vw = target * (1 + rnd.uniform(-0.08, 0.08))
            seg = cruise / pieces
            if st["s"] + seg + vw * vw / (2 * B) > L:
                break
            add(seg / ((st["v"] + vw) / 2), vw)
        if not hwy and rnd.random() < 0.45:
            v = st["v"]
            if st["s"] + v * v / (2 * B) + 40 < L:
                add(v / B, 0.0)
                add(rnd.uniform(4, 45), 0.0)
    return kt, kv, ks


# ------------------------------------------------------------------ charges


class Charge(Seg):
    kind = "charge"

    def __init__(self, car, seq, start, pid, kind, soc0, target, max_end, batt0, rnd, source="can",
                 ac=None, dc_kw=None, lat=None, lon=None):
        self.seq = seq
        self.id = charge_id(car.n, seq)
        self.pid = pid
        pl = P.BY_ID.get(pid)
        self.lat = pl["lat"] if pl else lat
        self.lon = pl["lon"] if pl else lon
        self.ckind = kind
        self.soc0 = soc0
        self.batt0 = batt0
        self.usable = car.usable_kwh(start)
        self.ac = ac
        self.outside = outside_c(car, start + 30 * MIN, self.lat, self.lon)
        target = max(target, soc0 + 1)
        if kind == "ac":
            volts, amps, phases = ac
            self.volts_ac, self.amps_ac, self.phases = volts, amps, phases
            self.line_kw = volts * amps * phases / 1000
            self.eff = 0.92 if phases == 3 else 0.90 if amps >= 24 else 0.86
            self.pack_kw = self.line_kw * self.eff
            need = (target - soc0) / 100 * self.usable
            dur_ms = int((need / self.pack_kw) * HOUR + 40_000)
            self.table = None
        else:
            self.phases = None
            self.eff = 0.975
            peak = min(dc_kw or car.dc_peak_kw, car.dc_peak_kw)
            if batt0 < 18:
                peak *= 0.6
            self.peak_kw = peak
            self.station_kw = dc_kw or car.dc_peak_kw
            knee_lo, knee_hi = car.dc_knee
            tbl_soc, tbl_t, tbl_p = [soc0], [0.0], []
            soc = soc0
            tt = 0.0
            while soc < target - 1e-9:
                step = min(1.0, target - soc)
                p = _dc_power(car, peak, soc / 100, knee_lo, knee_hi)
                tbl_p.append(p)
                tt += self.usable * step / 100 / p * 3600
                soc += step
                tbl_soc.append(soc)
                tbl_t.append(tt)
            tbl_p.append(_dc_power(car, peak, soc / 100, knee_lo, knee_hi))
            self.table = (tbl_soc, tbl_t, tbl_p)
            dur_ms = int(tt * 1000 + 60_000)
        end = start + dur_ms
        self.target = target
        self.planned_end = end
        if max_end is not None and end > max_end:
            end = max(start + 5 * MIN, max_end)
        super().__init__(car, start, end, source)
        self.soc1 = self.soc_at(end)
        self.cut_ms = None
        self.gaps = []  # (t0, t1, kind) inside the charge
        self.plug_start = start
        self.plug_end = end
        self.batt1 = self._batt(end)
        r_ = rnd.random()
        self.tm_cost = None if r_ < 0.35 else round(0.2 + r_ * 0.25, 2)  # for teslamate copies

    def _soc_raw(self, t):
        tau = (t - self.start) / 1000
        if self.ckind == "ac":
            gain = max(0.0, tau - 40) / 3600 * self.pack_kw / self.usable * 100
            return min(self.target, self.soc0 + gain)
        soc_t, t_t, _ = self.table
        tau -= 30
        if tau <= 0:
            return self.soc0
        if tau >= t_t[-1]:
            return soc_t[-1]
        k = bisect.bisect_right(t_t, tau) - 1
        f = (tau - t_t[k]) / (t_t[k + 1] - t_t[k])
        return soc_t[k] + (soc_t[k + 1] - soc_t[k]) * f

    def soc_at(self, t):
        return self._soc_raw(min(t, self.end))

    def _batt(self, t):
        tau = (t - self.start) / 1000
        if self.ckind == "dc":
            top = 44.0 if self.car.chemistry != "lfp" else 40.0
            return self.batt0 + (top - self.batt0) * (1 - math.exp(-tau / 900))
        target = max(self.outside + 6, 18.0)
        return target + (self.batt0 - target) * math.exp(-tau / 7200)

    def charge_power(self, t):
        """Charging power into the pack (kW, positive) at t."""
        if t < self.start or t >= self.end:
            return 0.0
        tau = (t - self.start) / 1000
        if self.ckind == "ac":
            if tau < 40:
                return self.pack_kw * tau / 40
            if self._soc_raw(t) >= self.target - 1e-6:
                return 0.0
            wob = 1 + 0.004 * math.sin(tau / 7) + 0.003 * math.sin(tau / 13)
            return self.pack_kw * wob
        _, t_t, p_t = self.table
        # the start ramp tops out at the first table power, and nothing flows once the target is reached, so
        # the 1 Hz curve adds up to the session's own power_max_kw, power_avg_kw and energy_added_kwh
        if tau < 30:
            return min(self.peak_kw * 0.5, p_t[0]) * tau / 30
        if tau - 30 >= t_t[-1]:
            return 0.0
        soc = self._soc_raw(t)
        k = int(clamp(soc - self.soc0, 0, len(p_t) - 1))
        return p_t[k] * (1 + 0.006 * math.sin(tau / 5))

    def heater_kw(self, t):
        tau = (t - self.start) / 1000
        if self.batt0 < 15 and tau < 1200:
            return 5.0 if self.ckind == "dc" else 2.5
        return 0.0

    @property
    def data_end(self):
        return self.cut_ms if self.cut_ms is not None else self.end

    def in_gap(self, t):
        for a, b, _ in self.gaps:
            if a <= t < b:
                return True
        return False

    def state_at(self, t):
        return "unknown" if self.in_gap(t) else "charging"

    def native_step_ms(self, t):
        return 1000 if self.source == "can" else 30_000

    def peak_times(self):
        return (self.start + 120_000,)

    def pack_volts(self, soc):
        if self.car.chemistry == "lfp":
            return 325 + 0.6 * soc + (30 if soc > 97 else 0)
        if self.car.profile == "tesla-sx-legacy":
            return 330 + 0.75 * soc
        return 340 + 0.72 * soc

    def curve(self, t):
        """Every charge-curve field at t (None outside the data)."""
        if t < self.start or t > self.data_end or self.in_gap(t):
            return None
        car = self.car
        soc = self.soc_at(t)
        p = self.charge_power(t)
        batt = self._batt(t)
        v = self.pack_volts(soc) + (4 if p > 0 else 0)
        amps = p * 1000 / v
        bv = battery_values(car, t, soc, batt)
        ac = self.ckind == "ac"
        tm = self.source == "teslamate"
        heater = self.heater_kw(t)
        return {
            "power_kw": round(p, 2), "volts_v": round(v, 1), "amps_a": round(amps, 1),
            "soc_pct": bv["soc_pct"], "usable_soc_pct": bv["usable_soc_pct"],
            "battery_c": None if tm else round(batt, 1), "range_km": bv["range_km"],
            "outside_c": round(self.outside, 1),
            "phases": self.phases if ac else None,
            "pilot_a": self.amps_ac if ac and not tm else None,
            "ac_line_kw": round(p / self.eff + heater * 0, 2) if ac else None,
            "heater_kw": None if tm else round(heater, 2),
            "ac_volts_v": round(self.volts_ac + 2.5 * math.sin(t / 1_400_000) + 0.8 * math.sin(t / 211_000), 1) if ac else None,
            "ac_amps_a": round(min(self.amps_ac, p / self.eff * 1000 / (self.volts_ac * self.phases)), 1) if ac else None,
            "limit_source": None if tm else "station",
            "dc_volts_v": None if ac else round(v + 2, 1),
            "dc_amps_a": None if ac else round(amps, 1),
            "_bv": bv, "_batt": batt,
        }

    def values(self, t):
        c = self.curve(t)
        if c is None:
            return None
        car = self.car
        vals = dict(c["_bv"])
        ty = None if self.source == "teslamate" else tyres(car, t, self.outside, 0.0)
        legacy = car.profile == "tesla-sx-legacy"
        vals.update({
            "speed_kph": 0.0, "power_kw": round(-c["power_kw"] + c["heater_kw"] if c["heater_kw"] else -c["power_kw"], 2),
            "odometer_km": _odo(self), "outside_c": c["outside_c"],
            "cabin_c": None if legacy else round(self.outside + 3, 1),
            "battery_c": c["battery_c"], "setpoint_left_c": None, "setpoint_right_c": None,
            "elevation_m": round(R.terrain_m(self.lat, self.lon), 1),
            "tyre_fl_kpa": ty[0] if ty else None, "tyre_fr_kpa": ty[1] if ty else None,
            "tyre_rl_kpa": ty[2] if ty else None, "tyre_rr_kpa": ty[3] if ty else None,
            "lat": round(self.lat, 6), "lon": round(self.lon, 6), "accuracy_m": 6.0, "heading_deg": None,
            "lv_v": lv_volts(car, True) if self.source == "can" else None,
        })
        return vals

    def activity(self):
        out = []
        t = self.start
        for a, b, _ in sorted(self.gaps):
            if a > t:
                out.append(("charging", t, a))
            out.append(("unknown", a, b))
            t = b
        out.append(("charging", t, self.data_end))
        return out

    def awake_spans(self):
        return [(self.start, self.data_end, ("vehicle", "chassis"), ("charging",), None)]

    def figures(self, upto=None):
        end = self.data_end if upto is None else min(upto, self.data_end)
        soc1 = self.soc_at(end)
        added = (soc1 - self.soc0) / 100 * self.usable
        ms = end - self.start
        car = self.car
        if self.ckind == "ac":
            grid = added / self.eff
            inlet = None
            pmax = self.pack_kw * 1.007 if ms > 40_000 else self.pack_kw * ms / 40_000
        else:
            grid = None
            inlet = added / self.eff
            pmax = max(self.table[2]) if self.table[2] else 0.0
        gap_ms = sum(min(b, end) - a for a, b, _ in self.gaps if a < end)
        if self.cut_ms is not None:
            gap_ms += self.planned_end - self.cut_ms
        return {
            "soc1": soc1, "added": added, "grid": grid, "inlet": inlet, "ms": ms, "pmax": pmax,
            "pavg": added / (ms / HOUR) if ms > 0 else None,
            "range_added": (soc1 - self.soc0) / 100 * car.full_range_km(end),
            "range0": self.soc0 / 100 * car.full_range_km(self.start),
            "range1": soc1 / 100 * car.full_range_km(end),
            "batt_avg": (self.batt0 + self._batt(end)) / 2, "gap_ms": gap_ms,
        }


def _dc_power(car, peak, soc, knee_lo, knee_hi):
    if soc < knee_lo:
        p = peak * (0.75 + 0.25 * soc / knee_lo)
    elif soc < knee_hi:
        p = peak
    else:
        f = (soc - knee_hi) / (1 - knee_hi)
        p = peak * (1 - f) ** (1.1 if car.chemistry == "lfp" else 1.5) + 8
    return max(6.0, p)


# ------------------------------------------------------------------ parked spells


class Parked(Seg):
    kind = "parked"

    def __init__(self, car, start, end, pid, lat, lon, soc0, batt0, ctx, rnd, source="can"):
        super().__init__(car, start, end, source)
        self.pid = pid
        self.lat = lat
        self.lon = lon
        self.soc0 = soc0
        self.batt0 = batt0
        self.plugged = ctx.get("plugged")  # "before" | "after" | None (plugged to a charge before / after it)
        self.charge = ctx.get("charge")
        self.outside0 = outside_c(car, start, lat, lon)
        self.subs = _parked_subs(car, start, end, ctx, rnd)
        cum = [0.0]
        drain = 0.0 if self.plugged else 1.0
        for s, e, _st, _r, kw in self.subs:
            cum.append(cum[-1] + kw * drain * (e - s) / HOUR)
        self.cum_kwh = cum
        self.usable = car.usable_kwh(start)
        self.base_start = start

    def clip(self, start, end):
        """A view of part of this spell (the history trajectory is kept)."""
        import copy
        c = copy.copy(self)
        c.start = int(start)
        c.end = int(end)
        return c

    def sub_at(self, t):
        for i, sub in enumerate(self.subs):
            if sub[0] <= t < sub[1]:
                return i, sub
        return len(self.subs) - 1, self.subs[-1]

    def kwh_at(self, t):
        i, (s, e, _st, _r, kw) = self.sub_at(t)
        drain = 0.0 if self.plugged else 1.0
        return self.cum_kwh[i] + kw * drain * (max(s, min(t, e)) - s) / HOUR

    def soc_at(self, t):
        return self.soc0 - self.kwh_at(t) / self.usable * 100

    def batt_at(self, t):
        out = outside_c(self.car, t, self.lat, self.lon)
        return out + (self.batt0 - out) * math.exp(-(t - self.base_start) / (6 * HOUR))

    def state_at(self, t):
        _, sub = self.sub_at(t)
        return "asleep" if sub[2] == "asleep" else "parked"

    def native_step_ms(self, t):
        _, sub = self.sub_at(t)
        if sub[2] == "asleep":
            return None
        return 30_000 if self.source == "can" else 60_000

    def values(self, t):
        if t < self.start or t >= self.end:
            return None
        _, sub = self.sub_at(t)
        if sub[2] == "asleep":
            return None
        car = self.car
        out = outside_c(car, t, self.lat, self.lon)
        batt = self.batt_at(t)
        soc = self.soc_at(t)
        vals = battery_values(car, t, soc, batt)
        climate = any(r in ("preconditioning_cabin", "keep_climate_dog", "keep_climate_on", "keep_climate_camp")
                      for r in sub[3])
        warm = math.exp(-(t - self.base_start) / (30 * MIN))
        tm = self.source == "teslamate"
        ty = None if tm else tyres(car, t, out, warm)
        legacy = car.profile == "tesla-sx-legacy"
        vals.update({
            "speed_kph": 0.0, "power_kw": round(sub[4], 3), "odometer_km": _odo(self),
            "outside_c": round(out, 1),
            "cabin_c": None if legacy and not tm else round(21.0 if climate else out + 2.5, 1),
            "battery_c": None if tm else round(batt, 1),
            "setpoint_left_c": 21.0 if climate and not legacy else None,
            "setpoint_right_c": 21.0 if climate and not legacy else None,
            "elevation_m": round(R.terrain_m(self.lat, self.lon), 1),
            "tyre_fl_kpa": ty[0] if ty else None, "tyre_fr_kpa": ty[1] if ty else None,
            "tyre_rl_kpa": ty[2] if ty else None, "tyre_rr_kpa": ty[3] if ty else None,
            "lat": round(self.lat, 6), "lon": round(self.lon, 6), "accuracy_m": 5.0, "heading_deg": None,
            "lv_v": None if tm else lv_volts(car, True),
        })
        return vals

    def activity(self):
        names = {"awake": "online", "vbus": "online_vehicle_bus_only", "asleep": "asleep"}
        out = []
        for s, e, st, _r, _kw in self.subs:
            a, b = max(s, self.start), min(e, self.end)
            if b > a:
                out.append((names[st], a, b))
        return out

    def awake_spans(self):
        out = []
        for s, e, st, reasons, kw in self.subs:
            a, b = max(s, self.start), min(e, self.end)
            if b > a and st != "asleep":
                buses = ("vehicle", "chassis") if st == "awake" else ("vehicle",)
                out.append((a, b, buses, reasons, kw))
        return out

    def split_ms(self):
        """Time asleep, awake, awake with only the vehicle bus, unknown (ms)."""
        res = {"asleep": 0, "awake": 0, "awake_vehicle_bus_only": 0, "unknown": 0}
        key = {"asleep": "asleep", "awake": "awake", "vbus": "awake_vehicle_bus_only"}
        for s, e, st, _r, _kw in self.subs:
            a, b = max(s, self.start), min(e, self.end)
            if b > a:
                res[key[st]] += b - a
        return res

    def reasons(self):
        seen = []
        for s, e, _st, rs, _kw in self.subs:
            if min(e, self.end) > max(s, self.start):
                for r_ in rs:
                    if r_ not in seen:
                        seen.append(r_)
        return seen


def _parked_subs(car, start, end, ctx, rnd):
    """Sub-spans of a parked spell: [start, end, state ('awake' | 'vbus' | 'asleep'), reasons, kW]."""
    subs = []
    dur = end - start
    legacy = car.profile == "tesla-sx-legacy"
    if dur <= 8 * MIN or ctx.get("stay_awake"):
        reasons = ctx.get("stay_awake") or ()
        kw = {"sentry": 0.30, "keep_climate_dog": 1.6, "keep_climate_camp": 1.4, "keep_climate_on": 1.8}.get(
            reasons[0] if reasons else "", 0.25)
        first = min(dur, 2 * MIN)
        subs.append([start, start + first, "awake", ("user_present",) + tuple(reasons), 0.45])
        if dur > first:
            subs.append([start + first, end, "awake", tuple(reasons), kw])
        return subs
    awake = min(dur, int(rnd.uniform(6, 15) * MIN))
    subs.append([start, start + 2 * MIN, "awake", ("user_present",), 0.45])
    subs.append([start + 2 * MIN, start + awake, "awake", (), 0.25])
    t = start + awake
    pre = end - 15 * MIN if ctx.get("precondition") and dur > 45 * MIN else end
    cold = ctx.get("cold")
    while t < pre:
        nap = (rnd.uniform(1.0, 2.5) if legacy else rnd.uniform(4.0, 9.0)) * HOUR
        e = min(pre, t + int(nap))
        subs.append([t, e, "asleep", (), car.asleep_kw])
        t = e
        if t < pre:
            blip = min(pre - t, int(rnd.uniform(8, 18 if legacy else 16) * MIN))
            if legacy:
                subs.append([t, t + blip, "awake", (), 0.35])
            elif cold:
                subs.append([t, t + blip, "vbus", ("keep_warm",), 1.8])
            else:
                subs.append([t, t + blip, "vbus", (), 0.09])
            t += blip
    if pre < end:
        subs.append([pre, end, "awake", ("preconditioning_cabin",), 4.5])
    return subs


# ------------------------------------------------------------------ gaps and updates


class Gap(Seg):
    kind = "gap"

    def __init__(self, car, start, end, reason, before=None, after=None, odo_jump=0.0, open_end=False):
        super().__init__(car, start, end, "can")
        self.reason = reason
        self.before = before  # (lat, lon) last known
        self.after = after  # (lat, lon) first known after
        self.odo_jump = odo_jump
        self.open_end = open_end

    def activity(self):
        return [("unknown", self.start, self.end)]


class Update(Seg):
    kind = "update"

    def __init__(self, car, seq, start, end, version, version_source, detected_by, lat, lon, pid, soc, source="can"):
        super().__init__(car, start, end, source)
        self.seq = seq
        self.id = update_id(car.n, seq)
        self.version = version
        self.version_source = version_source
        self.detected_by = detected_by
        self.lat, self.lon, self.pid = lat, lon, pid
        self.soc = soc
        # the fingerprint set the car's modules reported after the update (made up; one set per version)
        self.build_id = hashlib.blake2b((version or f"{car.id}:{seq}").encode(), digest_size=8).hexdigest()
        mods = ["vcfront", "bms", "dir", "dif"]
        if car.config.get("drivetrain", ("AWD",))[0] == "RWD":
            mods.remove("dif")
        k = int(h01("mods", car.id, seq) * 3)
        self.changed_modules = [m for i, m in enumerate(mods) if (i + k) % 3 != 2 or m == "bms"]

    def state_at(self, t):
        return "parked"

    def native_step_ms(self, t):
        return 60_000

    def values(self, t):
        if t < self.start or t >= self.end:
            return None
        car = self.car
        out = outside_c(car, t, self.lat, self.lon)
        vals = battery_values(car, t, self.soc, out + 4)
        vals.update({k: None for k in SERIES_FIELDS if k not in vals})
        vals.update({"speed_kph": 0.0, "power_kw": 0.6, "outside_c": round(out, 1), "odometer_km": _odo(self),
                     "lat": round(self.lat, 6), "lon": round(self.lon, 6), "lv_v": lv_volts(car, True)})
        return vals

    def activity(self):
        return [("updating", self.start, self.end)]

    def awake_spans(self):
        return [(self.start, self.end, ("vehicle", "chassis"), (), 0.6)]
